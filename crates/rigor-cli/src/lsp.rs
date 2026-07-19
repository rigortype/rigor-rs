//! `rigor lsp [--transport=stdio] [--log=PATH]` (§12, ADR-0029) — the in-process
//! Language Server.
//!
//! v1 scope: stdio JSON-RPC (via the sync `lsp-server` scaffold — no async
//! runtime), `TextDocumentSyncKind::FULL` open buffers, live **diagnostics**
//! (`textDocument/publishDiagnostics`) and **hover** (`textDocument/hover`, a
//! type-of probe at the cursor). These two reuse the EXACT `check` / `type-of`
//! analysis path, so an editor sees byte-for-byte the same findings and types the
//! CLI does. Completion is the next slice (it needs a method-enumeration index API
//! plus receiver-before-trigger parsing; deferred, and not advertised as a
//! capability, so no editor calls it).
//!
//! Two-tier essence (ADR-0029): the RBS environment (`CoreIndex`) + config are
//! built ONCE at startup and reused across every request — the per-keystroke cost
//! is a single-file parse+lower+analyze, never the RBS-load floor. `didChange`
//! diagnostics are debounced 200 ms per URI (S2) and computed on a **pre-warmed
//! rayon worker pool** (S3): the loop thread stays responsive to hover/completion
//! while diagnostics compute off-thread, and a result is published only if the
//! buffer's `version` still matches (stale-drop), with at most one worker in
//! flight per URI and a guaranteed re-dispatch of the latest content so the final
//! buffer state is always eventually published. The remaining two-tier machinery
//! (generation counter, watched-files invalidation, temp-file `BufferBinding`,
//! cross-file project context for open buffers) is deferred to S4; a single open
//! buffer is analysed in-memory at file scope.

use std::collections::{HashMap, HashSet};
use std::panic::{self, AssertUnwindSafe};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lsp_server::{Connection, Message, Response};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, Hover,
    HoverContents, HoverParams, HoverProviderCapability, MarkupContent, MarkupKind, MessageType,
    NumberOrString, OneOf, Position, PublishDiagnosticsParams, Range, ServerCapabilities,
    ShowMessageParams, SymbolKind, TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};

use rigor_index::CoreIndex;
use rigor_infer::{SourceIndex, Typer};
use rigor_parse::{comment_lines, lower, parse, Node};
use rigor_rules::{analyze_with_source_and_folder, filter_suppressed, Severity, SuppressSet};
use rigor_types::{Interner, Type, TypeId};

use crate::config::Config;
use crate::ruby_mode;
use crate::sidecar;

/// `rigor lsp [--transport=stdio] [--log=PATH]`. Only `stdio` transport is
/// supported in v1 (ADR-0029); `--log` is accepted and reserved (server logs go
/// to stderr until wired). Returns exit 0 on a clean shutdown, 64 on a usage
/// error (unknown transport), 1 on a protocol/IO error.
pub fn cmd_lsp(args: &[String]) -> ExitCode {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            // `--transport=stdio` or `--transport stdio`.
            "--transport=stdio" => {}
            "--transport" => match it.next().map(String::as_str) {
                Some("stdio") => {}
                other => {
                    eprintln!("rigor lsp: only --transport=stdio is supported, got {other:?}");
                    return ExitCode::from(64);
                }
            },
            a if a.starts_with("--transport=") => {
                eprintln!("rigor lsp: only --transport=stdio is supported, got {a:?}");
                return ExitCode::from(64);
            }
            // `--log=PATH` / `--log PATH` — accepted + reserved (ADR-0029).
            a if a.starts_with("--log=") => {}
            "--log" => {
                let _ = it.next();
            }
            other => {
                eprintln!("rigor lsp: unexpected argument {other:?}");
                return ExitCode::from(64);
            }
        }
    }

    match run_stdio() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rigor lsp: {e}");
            ExitCode::from(1)
        }
    }
}

/// The static server capabilities advertised at `initialize` (extracted so the
/// integration tests can drive the same handshake the stdio boot does).
fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        // FULL sync: each edit resends the whole buffer (ADR-0029 — local stdio
        // bandwidth is irrelevant; UTF-16 incremental diffing is a later slice).
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        // Member-access method completion, triggered on `.` and `:` (the second
        // `:` of `::`). The server returns the full unfiltered candidate set;
        // client-side fuzzy matching narrows it (ADR-0029).
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_string(), ":".to_string()]),
            ..Default::default()
        }),
        // Outline: classes/modules/methods as a nested symbol tree.
        document_symbol_provider: Some(OneOf::Left(true)),
        ..Default::default()
    }
}

/// Boot the stdio server: handshake, build the shared context once, run the loop.
fn run_stdio() -> Result<(), String> {
    let (connection, io_threads) = Connection::stdio();

    let caps_value = serde_json::to_value(server_capabilities()).map_err(|e| e.to_string())?;
    // `initialize` returns the client's `InitializeParams` (S4): we thread the
    // client's `workspace.didChangeWatchedFiles.dynamicRegistration` capability out
    // of it so the `initialized` handler knows whether to `client/registerCapability`
    // the file watchers (or degrade gracefully when the client won't accept dynamic
    // registration). Pre-S4 this return value was discarded.
    let init_params = connection
        .initialize(caps_value)
        .map_err(|e| format!("initialize handshake failed: {e}"))?;
    let watched_files_dynamic_registration = client_supports_watched_files_registration(&init_params);

    // Two-tier essence: the RBS environment + config are built ONCE and reused
    // for the whole session (the per-keystroke path never pays the RBS-load floor).
    let cfg = Config::load(None);

    // ADR-0036 / ADR-0008: `rigor lsp` defaults to `auto` and NEVER hard-errors
    // (an editor's Ruby env is structurally fragile — GUI apps don't source shell
    // rc), so an unreachable sidecar degrades to the sound subset here even under
    // `require`. The posture is always SURFACED via `window/showMessage`, and a
    // reachable sidecar is wired as the folder so the editor gets full fidelity.
    let ruby = ruby_mode::resolve(None, cfg.ruby_config_value(), ruby_mode::RubyMode::Auto)
        .unwrap_or(ruby_mode::RubyMode::Auto);
    let (folder, posture, typ) = match &ruby {
        ruby_mode::RubyMode::Off => (
            None,
            "sound subset (Ruby-free by request)".to_string(),
            MessageType::INFO,
        ),
        mode => {
            let bin = sidecar::ruby_bin_for(mode).expect("a non-off mode names a ruby binary");
            match sidecar::Sidecar::spawn(&bin) {
                Ok(sc) => {
                    let v = sc.ruby_version().to_string();
                    (
                        // Behind an `Arc` so it is PRESERVED across `ProjectContext`
                        // rebuilds (S4 `invalidate`): a project-context rebuild reuses
                        // the same live sidecar rather than respawning the Ruby VM.
                        Some(Arc::new(sidecar::SidecarFolder::new(sc))),
                        format!("full fidelity — Ruby sidecar (ruby {v})"),
                        MessageType::INFO,
                    )
                }
                Err(e) => (
                    None,
                    format!("sound subset — Ruby sidecar unavailable ({e})"),
                    MessageType::WARNING,
                ),
            }
        }
    };
    send_show_message(&connection, typ, format!("rigor: coverage posture — {posture}"))?;

    // The tier-1 project context: RBS index + suppression set + shared sidecar,
    // stamped with generation 0. Loop-owned (swapped on `invalidate`), so it is
    // built here and MOVED into `main_loop` rather than held in the immutable
    // `ServerContext`.
    let project = Arc::new(ProjectContext {
        generation: 0,
        index: CoreIndex::for_project(
            &cfg.plugins,
            &cfg.all_signature_dirs(std::path::Path::new(".")),
        ),
        disable: cfg.disable_matcher(),
        folder,
    });

    let ctx = ServerContext {
        debounce: DEBOUNCE_DEFAULT,
        worker_gate: production_gate(),
        watched_files_dynamic_registration,
    };

    // Pre-warm the rayon global pool at startup (ADR-0029 "pre-warmed worker
    // pool"): the pool spawns its worker threads lazily on first use, so touch it
    // once here to avoid paying that init on the first keystroke's dispatch. The
    // pool size honours `RAYON_NUM_THREADS` natively (the existing knob); no LSP
    // `--workers` flag is added.
    rayon::spawn(|| {});

    main_loop(&connection, &ctx, project, cfg)?;

    // Drop the connection BEFORE joining: the writer IO thread only terminates
    // when its channel disconnects, i.e. when the `Connection` (which owns the
    // sender) is dropped. Joining while `connection` is still alive would hang.
    drop(connection);
    io_threads.join().map_err(|e| e.to_string())?;
    Ok(())
}

/// Read the client's `initialize` params for
/// `capabilities.workspace.didChangeWatchedFiles.dynamicRegistration` (S4). `true`
/// means the client accepts a runtime `client/registerCapability`, so the server
/// registers its file watchers after `initialized`. Absent/false ⇒ degrade
/// gracefully: no registration is sent, and the server still honours any
/// `didChangeWatchedFiles` the client chooses to send (static registration).
fn client_supports_watched_files_registration(init_params: &serde_json::Value) -> bool {
    init_params
        .get("capabilities")
        .and_then(|c| c.get("workspace"))
        .and_then(|w| w.get("didChangeWatchedFiles"))
        .and_then(|d| d.get("dynamicRegistration"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// The default per-URI `didChange` debounce (ADR-0029 §debounce; matches the
/// reference `DiagnosticPublisher`'s `debounce_seconds: 0.2`). Injectable via
/// [`ServerContext::debounce`] so timing tests can drive a small or large value
/// deterministically rather than sleeping the real 200 ms.
const DEBOUNCE_DEFAULT: Duration = Duration::from_millis(200);

/// The tier-1 project context (ADR-0029 `ProjectContext`): the RBS index + the
/// config-derived suppression set + the optional Ruby folder, stamped with a
/// `generation` counter. Built once at startup and thereafter **loop-owned** —
/// [`invalidate`] swaps in a fresh `Arc` with a bumped generation on a
/// watched-files / configuration change (S4). Held behind an `Arc` so a clone is
/// captured into each rayon worker (S3): a worker computes against whichever
/// context was current at dispatch, and a result computed against a superseded
/// generation is dropped by the generation guard in [`handle_result`]. In-flight
/// workers holding the OLD `Arc` finish against it (their results are
/// generation-dropped); new dispatches read the new `Arc`.
///
/// Must be `Send + Sync`: `CoreIndex` and `SidecarFolder` are already shared as
/// `&(dyn RubyFolder + Sync)` across the `check` pipeline's `par_iter` workers
/// (`main.rs`), so sharing them across the LSP worker pool reuses that exact
/// contract; `Arc<SidecarFolder>` keeps that bound.
struct ProjectContext {
    /// Bumped by [`invalidate`] on every project-context rebuild. A worker stamps
    /// its result with the generation it computed against; a stale (superseded)
    /// generation is dropped at publish time — orthogonal to the buffer version
    /// guard (version guards edits; generation guards project rebuilds).
    generation: u64,
    index: CoreIndex,
    disable: SuppressSet,
    /// The ADR-0008 real-Ruby folder for full-fidelity constant folds, when a
    /// sidecar was reachable at startup. `None` = sound subset. Behind an `Arc` so
    /// it is PRESERVED (not respawned) across `invalidate` rebuilds — a rebuild
    /// clones this `Arc` into the new context. Shared across the concurrent LSP
    /// workers as `&(dyn RubyFolder + Sync)` exactly as the `check` pipeline does
    /// (`sidecar.rs`); the folder's internal `Mutex` serializes folds across the
    /// workers (contention accepted, measure later per ADR-0029).
    folder: Option<Arc<sidecar::SidecarFolder>>,
}

/// The test seam for the worker's compute (S3/S4). Called at the START of each
/// worker's body with the buffer `version` AND the project `generation` it is
/// computing against, so a concurrency test can hold a worker mid-flight (block
/// until released) or force it to panic, deterministically — keyed on either axis
/// — without depending on real rayon timing. Production is a no-op
/// ([`production_gate`]); it lives INSIDE the worker's `catch_unwind`, so a gate
/// panic is caught and the worker still sends its `Computed` (never-stuck).
type WorkerGate = dyn Fn(i32, u64) + Send + Sync;

/// The production [`WorkerGate`]: a no-op (no test is holding workers).
fn production_gate() -> Arc<WorkerGate> {
    Arc::new(|_version: i32, _generation: u64| {})
}

/// The session-stable server context: the injectable debounce interval, the worker
/// gate test seam, and the client's watched-files dynamic-registration capability.
/// The mutable, loop-owned state (buffers, debounce deadlines, in-flight set, open
/// epochs, and the current `Arc<ProjectContext>`) lives in [`Session`], NOT here.
struct ServerContext {
    /// The per-URI `didChange` debounce interval (S2, ADR-0029 §debounce).
    /// Injectable — production uses [`DEBOUNCE_DEFAULT`] (200 ms); tests pass a
    /// small value (assert the deferred publish eventually arrives) or a large
    /// one (assert it does NOT fire within a round-trip), so no test depends on
    /// wall-clock precision. Only the PUBLISH is deferred; the BufferTable is
    /// updated synchronously on each change so hover/completion see latest text.
    debounce: Duration,
    /// The worker-compute test seam (S3). Production = [`production_gate`] (no-op);
    /// concurrency tests inject a gate that blocks/panics a worker deterministically.
    worker_gate: Arc<WorkerGate>,
    /// Whether the client advertised
    /// `workspace.didChangeWatchedFiles.dynamicRegistration` at `initialize` (S4).
    /// When `true`, the `initialized` handler sends a `client/registerCapability`
    /// for the config + project-signature file watchers; when `false`, no
    /// registration is sent and the server degrades to honouring whatever
    /// `didChangeWatchedFiles` the client sends statically.
    watched_files_dynamic_registration: bool,
}

/// The mutable, single-threaded state the dispatch loop owns (ADR-0029
/// single-writer). Bundled into one struct so the lifecycle functions take
/// `&mut Session` instead of threading a growing parameter list (and tripping
/// clippy's `too_many_arguments`). Never captured into a worker — workers get an
/// `Arc<ProjectContext>` clone only.
struct Session {
    /// The open-document store (S1).
    buffers: BufferTable,
    /// Per-URI debounced-publish deadlines (S2).
    debouncer: Debouncer,
    /// URIs with a rayon worker in flight — at most one per URI (S3).
    in_flight: HashSet<String>,
    /// Per-URI **open-epoch** (S4): a monotonic counter bumped on every `didOpen`
    /// AND `didClose` for the URI, persisting across close (unlike the buffer
    /// entry). A worker stamps its result with the epoch at dispatch; a result
    /// whose epoch no longer matches is dropped. This closes the close+reopen
    /// version-reuse nit: a reopen (VS Code resends version 1) that reuses the LSP
    /// version cannot let a stale pre-close worker's result publish, because the
    /// epoch advanced past what that worker captured. Generation does NOT bump on
    /// reopen (it is project-scoped), so the epoch — not the generation — is what
    /// closes this.
    epochs: HashMap<String, u64>,
    /// The current tier-1 [`ProjectContext`], swapped by [`invalidate`] (S4).
    project: Arc<ProjectContext>,
    /// The loaded config, retained so [`invalidate`] can rebuild the index from
    /// its plugin list + signature dirs.
    cfg: Config,
    /// The worker-results sender, cloned into each worker (S3). The matching
    /// receiver stays local to [`main_loop`]'s `select!`.
    results_tx: crossbeam_channel::Sender<Computed>,
}

// ---------------------------------------------------------------------------
// BufferTable (ADR-0029) — the loop's owned open-document store.
// ---------------------------------------------------------------------------

/// One open document: its full text (`bytes`, FULL sync so this is the whole
/// buffer), the LSP `version` from the last open/change, and a `dirty` flag set
/// on every `didChange`. In S1 nothing branches on `dirty` — it is maintained
/// for the S2/S3 debounce + temp-file `BufferBinding` consumers (ADR-0029).
struct BufferEntry {
    bytes: String,
    version: i32,
    #[allow(dead_code)] // maintained now; the dirty-materialize consumer lands in S4.
    dirty: bool,
}

/// The open-buffer store, keyed by URI string (`uri_key` semantics unchanged).
/// Replaces the former raw `HashMap<String, String>`: same lookup, but each
/// entry now carries the LSP `version` and a `dirty` flag per ADR-0029, so the
/// later slices have the metadata without another buffer-store refactor.
#[derive(Default)]
struct BufferTable {
    entries: HashMap<String, BufferEntry>,
}

impl BufferTable {
    fn new() -> Self {
        Self::default()
    }

    /// Record a `didOpen`: fresh entry, `dirty = false` (an opened buffer matches
    /// its on-disk file until edited).
    fn open(&mut self, uri: &Uri, bytes: String, version: i32) {
        self.entries
            .insert(uri_key(uri), BufferEntry { bytes, version, dirty: false });
    }

    /// Record a `didChange`: replace the text, bump the version, mark `dirty`.
    fn change(&mut self, uri: &Uri, bytes: String, version: i32) {
        self.entries
            .insert(uri_key(uri), BufferEntry { bytes, version, dirty: true });
    }

    /// Drop a closed buffer.
    fn close(&mut self, uri: &Uri) {
        self.entries.remove(&uri_key(uri));
    }

    /// The current text for `uri`, or `None` if the buffer is not open. This is
    /// the `&str` accessor the query handlers (hover / completion / symbols) read
    /// through, in place of the former `HashMap::get`.
    fn text(&self, uri: &Uri) -> Option<&str> {
        self.entries.get(&uri_key(uri)).map(|e| e.bytes.as_str())
    }

    /// The current `(text, version)` for `uri`, or `None` if the buffer is not
    /// open. Used when a debounced publish fires (S2): the deferred compute reads
    /// the LATEST buffer content — a burst of edits coalesced into one publish
    /// therefore analyses the final text, never an intermediate snapshot.
    fn snapshot(&self, uri: &Uri) -> Option<(&str, i32)> {
        self.entries.get(&uri_key(uri)).map(|e| (e.bytes.as_str(), e.version))
    }

    /// The current LSP `version` for `uri`, or `None` if the buffer is not open.
    /// The S3 version stale-drop compares a worker result's `version` against this
    /// at publish time: a result is published only if it still matches (else a
    /// newer edit superseded it → drop + re-dispatch).
    fn current_version(&self, uri: &Uri) -> Option<i32> {
        self.entries.get(&uri_key(uri)).map(|e| e.version)
    }

    /// Every currently-open URI (S4). Used to re-analyse ALL open buffers after an
    /// `invalidate` (a project-context rebuild can move any buffer's diagnostics).
    /// Reconstructs the `Uri` from its string key (the key is that URI's `as_str`).
    fn open_uris(&self) -> Vec<Uri> {
        self.entries.keys().filter_map(|k| k.parse().ok()).collect()
    }
}

// ---------------------------------------------------------------------------
// Debouncer (ADR-0029 §debounce) — per-URI deferred-publish deadlines.
// ---------------------------------------------------------------------------

/// One pending debounced publish: the buffer `uri` and the `Instant` its publish
/// is due.
struct Pending {
    uri: Uri,
    deadline: Instant,
}

/// Per-URI publish debounce (ADR-0029 §debounce; the Rust analogue of the
/// reference [`Debouncer`]). Maps a buffer URI to the `Instant` its debounced
/// publish is due. [`schedule`](Self::schedule) (re)sets the deadline — a later
/// `didChange` within the window overwrites the earlier deadline, so a burst of
/// edits **coalesces** into a single publish of the final content.
/// [`cancel`](Self::cancel) drops a pending publish (`didClose`, so no stale
/// diagnostics fire after a close). [`take_due`](Self::take_due) removes and
/// returns every URI whose deadline has passed.
///
/// The struct holds **no clock**: the caller computes deadlines
/// (`Instant::now() + interval`) and passes `now` to `take_due`. So the
/// fire/no-fire decision is a pure function of explicit `Instant`s —
/// deterministically unit-testable without any wall-clock sleep (the timing seam
/// S2's non-flaky tests drive).
#[derive(Default)]
struct Debouncer {
    pending: HashMap<String, Pending>,
}

impl Debouncer {
    fn new() -> Self {
        Self::default()
    }

    /// Schedule (or reschedule) a debounced publish for `uri` at `deadline`.
    /// Replacing the entry is the coalescing rule: the last change in a burst
    /// wins the deadline, and there is at most one pending publish per URI.
    fn schedule(&mut self, uri: &Uri, deadline: Instant) {
        self.pending
            .insert(uri_key(uri), Pending { uri: uri.clone(), deadline });
    }

    /// Cancel any pending publish for `uri` (`didClose`). Idempotent.
    fn cancel(&mut self, uri: &Uri) {
        self.pending.remove(&uri_key(uri));
    }

    /// The earliest pending deadline, or `None` when nothing is pending. The loop
    /// blocks its `select!` until this instant (or indefinitely when `None`).
    fn earliest(&self) -> Option<Instant> {
        self.pending.values().map(|p| p.deadline).min()
    }

    /// Remove and return every URI whose deadline is at or before `now`.
    fn take_due(&mut self, now: Instant) -> Vec<Uri> {
        let due: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(k, _)| k.clone())
            .collect();
        due.iter()
            .filter_map(|k| self.pending.remove(k))
            .map(|p| p.uri)
            .collect()
    }
}

/// A computed-diagnostics result carried over the internal worker-results channel
/// from a rayon worker back to the loop's single-writer publish point (S3). The
/// worker always sends exactly one `Computed` (even an empty-diags result on an
/// internal error/panic — the compute is `catch_unwind`-wrapped), so the loop's
/// in-flight tracking for the URI always clears. `version` is the buffer version
/// the worker analysed; the loop publishes `diags` only if it still matches the
/// current buffer version (stale-drop), else drops and re-dispatches the latest.
struct Computed {
    uri: Uri,
    version: i32,
    /// The project generation this result was computed against (S4). At publish
    /// time it must still equal the current `ProjectContext.generation`, else an
    /// `invalidate` superseded it → drop + re-dispatch under the new context.
    generation: u64,
    /// The URI's open-epoch at dispatch (S4). Must still equal the URI's current
    /// epoch at publish, else a `didClose`/`didOpen` cycle superseded it (the
    /// close+reopen version-reuse nit) → drop + re-dispatch.
    epoch: u64,
    diags: Vec<Diagnostic>,
}

/// The dispatch loop. It is the **sole owner** of the `BufferTable`, the
/// [`Debouncer`], and the `in_flight` set, and the **sole sender** of
/// `textDocument/publishDiagnostics` — the Rust analogue of the reference's
/// `SynchronizedWriter` (ADR-0029). It `select!`s over two receivers:
///
/// - (a) `connection.receiver` — client requests/notifications. A `didOpen`
///   *requests a dispatch* (immediate, fast first paint); a `didChange` updates
///   the buffer and schedules a debounce; requests (hover/completion/symbols) are
///   answered SYNCHRONOUSLY on the loop thread (they never go through the worker
///   pool). None of these publish directly.
/// - (b) `results_rx` — the internal **worker-results** channel. A rayon worker
///   pushes its [`Computed`] here; the loop handles it (`handle_result`) — the
///   single-writer publish point.
///
/// **S3 — rayon worker pool + stale-drop + one-in-flight/no-lost-update.**
/// [`request_dispatch`] spawns AT MOST ONE rayon worker per URI ([`spawn_worker`]
/// inserts the URI into `in_flight` and cancels its pending debounce — the worker
/// now covers the latest content). A worker captures a buffer snapshot `(text,
/// version)` + the `Arc<Analysis>` shared context + a `results_tx` clone, runs the
/// EXACT `check` compute off-thread, and always sends exactly one `Computed`.
/// `handle_result` clears the URI's `in_flight`, then:
/// - buffer closed → drop;
/// - `version` still current → publish;
/// - buffer moved past `version` (a newer edit superseded it) → DROP and
///   [`request_dispatch`] the LATEST content. Because `in_flight` was just cleared,
///   this spawns a fresh worker for the newest snapshot — so the final buffer state
///   is ALWAYS eventually published, and a dropped stale result never leaves the
///   latest content unpublished (no lost update). At most one worker per URI holds
///   throughout: only `spawn_worker` spawns, only under `!in_flight`, all on the
///   single loop thread.
///
/// **Debounce timeout arm (c), S2.** The `select!` blocks until the earliest
/// pending deadline (or indefinitely when nothing is pending); on timeout,
/// `fire_due` requests a dispatch for each now-due URI from the LATEST buffer
/// content, coalescing a burst into ONE dispatch. An edit DURING flight only
/// updates the buffer + resets the deadline; it does NOT spawn a second worker
/// (the debounce fire finds `in_flight` set and skips, and the eventual stale-drop
/// re-dispatch publishes the newest content). `didClose` cancels the pending
/// deadline and clears markers; a worker still in flight for a closed buffer has
/// its result dropped (current version is `None`).
///
/// Only the loop thread sends `publishDiagnostics` (single-writer invariant): the
/// top-of-loop drain, both `results_rx` arms, and `didClose`'s direct clear all
/// run on it; workers only push onto the internal channel, never to the connection.
///
/// **Shutdown.** On `shutdown`/`exit` the loop returns; `results_tx`/`results_rx`
/// drop, so any detached worker's later `send` returns `Err` (ignored) rather than
/// blocking — no hang, no deadlock. `shutdown`/`exit` are handled by the scaffold's
/// `handle_shutdown`.
fn main_loop(
    connection: &Connection,
    ctx: &ServerContext,
    project: Arc<ProjectContext>,
    cfg: Config,
) -> Result<(), String> {
    // The worker-results channel (ADR-0029 single-writer seam). `results_tx` is
    // cloned into each rayon worker closure, which pushes its `Computed` from
    // off-thread. Unbounded so a worker's `send` never blocks its rayon thread.
    // The receiver stays local (the `select!` reads it); the sender lives in the
    // loop-owned `Session`.
    let (results_tx, results_rx) = crossbeam_channel::unbounded::<Computed>();
    let mut st = Session {
        buffers: BufferTable::new(),
        debouncer: Debouncer::new(),
        in_flight: HashSet::new(),
        epochs: HashMap::new(),
        project,
        cfg,
        results_tx,
    };

    // Dynamic registration (S4): the `initialized` notification is CONSUMED by the
    // `Connection::initialize` handshake (`initialize_finish` waits for it), so it
    // never reaches this loop — the registration is sent here, once, at the top of
    // the loop. If the client advertised
    // `didChangeWatchedFiles.dynamicRegistration`, register the config +
    // project-signature file watchers now (fire-and-forget: the client's response is
    // ignored by the `Message::Response(_)` arm). Otherwise degrade gracefully — no
    // registration; the server still honours statically-configured
    // `didChangeWatchedFiles`.
    if ctx.watched_files_dynamic_registration {
        register_watched_files(connection)?;
    }

    loop {
        // Single-writer publish point: flush every ready worker result before
        // servicing the next input. This keeps publish-before-next-message
        // ordering and clears `in_flight` promptly (so a re-dispatch can proceed).
        while let Ok(computed) = results_rx.try_recv() {
            handle_result(connection, ctx, &mut st, computed)?;
        }

        // Timeout = time until the earliest pending debounce deadline (clamped to
        // 0 if already passed). No pending deadline ⇒ block with no timeout. An
        // incoming message wakes `select!` immediately regardless of the timeout,
        // so `didClose`'s cancel is serviced without waiting out the deadline.
        match st.debouncer.earliest() {
            Some(deadline) => {
                let timeout = deadline.saturating_duration_since(Instant::now());
                crossbeam_channel::select! {
                    recv(connection.receiver) -> msg => {
                        let Ok(msg) = msg else { return Ok(()) }; // connection closed
                        if handle_message(connection, ctx, &mut st, msg)? {
                            return Ok(()); // shutdown
                        }
                    }
                    recv(results_rx) -> computed => {
                        if let Ok(computed) = computed {
                            handle_result(connection, ctx, &mut st, computed)?;
                        }
                    }
                    default(timeout) => {
                        fire_due(ctx, &mut st);
                    }
                }
            }
            None => {
                crossbeam_channel::select! {
                    recv(connection.receiver) -> msg => {
                        let Ok(msg) = msg else { return Ok(()) }; // connection closed
                        if handle_message(connection, ctx, &mut st, msg)? {
                            return Ok(()); // shutdown
                        }
                    }
                    recv(results_rx) -> computed => {
                        // A rayon worker result arriving asynchronously while the
                        // loop was blocked (the live S3 path).
                        if let Ok(computed) = computed {
                            handle_result(connection, ctx, &mut st, computed)?;
                        }
                    }
                }
            }
        }
    }
}

/// Rebuild the tier-1 [`ProjectContext`] and bump its generation (S4). Invoked on
/// a relevant `workspace/didChangeWatchedFiles` and on
/// `workspace/didChangeConfiguration` — NEVER on a buffer `didChange`.
///
/// **The rebuild is SYNCHRONOUS on the loop thread** (orchestrator decision,
/// overriding the plan's "lazy rebuild on a worker"): invalidation events are RARE
/// (config / `Gemfile.lock` / signature save), unlike keystrokes, so paying a
/// ~100-300 ms `CoreIndex::for_project` build inline is acceptable UX and avoids a
/// second concurrency hazard (a worker-produced context swap). If profiling ever
/// shows this stall matters, the future optimization is a lazy async rebuild that
/// keeps serving the old context until the stamped replacement lands.
///
/// The sidecar folder is PRESERVED (its `Arc` is cloned into the new context), so
/// the Ruby VM is not respawned. The config file is not re-parsed here (matching
/// the reference `ProjectContext#invalidate!`, which rebuilds from the same
/// `@configuration`); the rebuild re-reads the signature dirs from disk, so
/// changed `sig/**/*.rbs` content is picked up. In-flight workers holding the OLD
/// `Arc` finish against it and are generation-dropped in [`handle_result`].
fn invalidate(st: &mut Session) {
    let generation = st.project.generation + 1;
    let index = CoreIndex::for_project(
        &st.cfg.plugins,
        &st.cfg.all_signature_dirs(std::path::Path::new(".")),
    );
    let disable = st.cfg.disable_matcher();
    let folder = st.project.folder.clone(); // reuse the live sidecar; no respawn.
    st.project = Arc::new(ProjectContext { generation, index, disable, folder });
}

/// After an [`invalidate`], re-analyse EVERY open buffer (S4): a project-context
/// rebuild can move any buffer's diagnostics. Each open URI is routed through
/// [`request_dispatch`]; a URI with a worker still in flight (against the old
/// generation) is a no-op here — that worker is generation-dropped and re-dispatched
/// by [`handle_result`], so the new context is always eventually applied.
fn reanalyze_open_buffers(ctx: &ServerContext, st: &mut Session) {
    for uri in st.buffers.open_uris() {
        request_dispatch(&uri, ctx, st);
    }
}

/// Bump and return the open-epoch for `uri` (S4). Called on `didOpen` AND
/// `didClose`. Persists in `st.epochs` across the buffer's lifetime, so a
/// close+reopen advances the epoch past what any pre-close worker captured.
fn bump_epoch(st: &mut Session, uri: &Uri) -> u64 {
    let e = st.epochs.entry(uri_key(uri)).or_insert(0);
    *e += 1;
    *e
}

/// The URI's current open-epoch (0 if never opened).
fn current_epoch(st: &Session, uri: &Uri) -> u64 {
    st.epochs.get(&uri_key(uri)).copied().unwrap_or(0)
}

/// Request a dispatch for every debounced publish whose deadline has passed (S2).
/// Each due URI is routed through [`request_dispatch`], which reads the LATEST
/// buffer content (so a coalesced burst analyses the final text) and spawns a
/// rayon worker unless one is already in flight for that URI. A URI whose buffer
/// was closed mid-window is skipped inside `request_dispatch` (its snapshot is
/// `None`).
fn fire_due(ctx: &ServerContext, st: &mut Session) {
    for uri in st.debouncer.take_due(Instant::now()) {
        request_dispatch(&uri, ctx, st);
    }
}

/// Request a diagnostics dispatch for `uri` from its LATEST buffer snapshot (S3).
/// The **one-in-flight gate**: if a worker is already running for `uri`, do
/// nothing — that worker's result will either publish (if still current) or, when
/// stale, trigger a re-dispatch in [`handle_result`], so the latest content is
/// always eventually analysed without ever running two concurrent workers for one
/// URI. Otherwise spawn a worker for the current snapshot. A closed/unknown buffer
/// (snapshot `None`) is skipped.
fn request_dispatch(uri: &Uri, ctx: &ServerContext, st: &mut Session) {
    if st.in_flight.contains(&uri_key(uri)) {
        return; // one-in-flight: the running worker's result drives re-dispatch.
    }
    // Copy the snapshot out to end the immutable borrow of `st.buffers` before
    // `spawn_worker` takes `&mut st`.
    let snapshot = st.buffers.snapshot(uri).map(|(t, v)| (t.to_string(), v));
    if let Some((text, version)) = snapshot {
        spawn_worker(uri, text, version, ctx, st);
    }
}

/// Handle one message from the connection. Returns `Ok(true)` when the server
/// should shut down. Requests are answered SYNCHRONOUSLY on the loop thread (they
/// never go through the worker pool); `didOpen` *requests* an immediate diagnostics
/// dispatch (a rayon worker publishes via the loop, not here); `didChange` updates
/// the buffer synchronously and *schedules* a debounced dispatch (S2); `didClose`
/// cancels any pending publish and clears inline markers.
/// `workspace/didChangeWatchedFiles` (on a relevant path) and
/// `workspace/didChangeConfiguration` (S4) invalidate the project context and
/// re-analyse open buffers. A buffer `didChange` NEVER invalidates. (The
/// `initialized` notification is consumed by the handshake, not here — the
/// watched-files `client/registerCapability` is sent at the top of [`main_loop`].)
fn handle_message(
    connection: &Connection,
    ctx: &ServerContext,
    st: &mut Session,
    msg: Message,
) -> Result<bool, String> {
    match msg {
        Message::Request(req) => {
            if connection.handle_shutdown(&req).map_err(|e| e.to_string())? {
                return Ok(true);
            }
            match req.method.as_str() {
                "textDocument/hover" => {
                    match req.extract::<HoverParams>("textDocument/hover") {
                        Ok((id, params)) => {
                            let hover = hover(&st.project, &st.buffers, &params);
                            let resp = Response::new_ok(id, hover);
                            connection
                                .sender
                                .send(Message::Response(resp))
                                .map_err(|e| e.to_string())?;
                        }
                        // Malformed params — no reply (the id is unknown on an
                        // extract error, so this can only happen on a truly bad
                        // message); matches the pre-refactor `continue`.
                        Err(e) => eprintln!("rigor lsp: bad hover params: {e:?}"),
                    }
                }
                "textDocument/completion" => {
                    match req.extract::<CompletionParams>("textDocument/completion") {
                        Ok((id, params)) => {
                            let items = completion(&st.project, &st.buffers, &params);
                            let resp = Response::new_ok(id, items);
                            connection
                                .sender
                                .send(Message::Response(resp))
                                .map_err(|e| e.to_string())?;
                        }
                        Err(e) => eprintln!("rigor lsp: bad completion params: {e:?}"),
                    }
                }
                "textDocument/documentSymbol" => {
                    match req.extract::<DocumentSymbolParams>("textDocument/documentSymbol") {
                        Ok((id, params)) => {
                            let syms = document_symbols(&st.buffers, &params);
                            let resp = Response::new_ok(id, syms);
                            connection
                                .sender
                                .send(Message::Response(resp))
                                .map_err(|e| e.to_string())?;
                        }
                        Err(e) => eprintln!("rigor lsp: bad documentSymbol params: {e:?}"),
                    }
                }
                // Unknown request: reply with a null result so the client doesn't
                // hang (we advertise a small surface).
                _ => {
                    let resp = Response::new_ok(req.id, serde_json::Value::Null);
                    connection
                        .sender
                        .send(Message::Response(resp))
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        Message::Notification(not) => match not.method.as_str() {
            "textDocument/didOpen" => {
                if let Ok(p) = not.extract::<DidOpenTextDocumentParams>("textDocument/didOpen") {
                    let uri = p.text_document.uri;
                    let text = p.text_document.text;
                    let version = p.text_document.version;
                    // Fast first paint: `didOpen` requests an IMMEDIATE dispatch
                    // (ADR-0029 plan §4), NOT debounced. Record the buffer first so
                    // the worker snapshots it; bump the open-epoch (S4) so a worker
                    // spawned now captures the fresh epoch AND any pre-close worker
                    // for a re-opened URI is epoch-dropped; then clear any stale
                    // pending publish. If a worker is still in flight for a re-opened
                    // URI, `request_dispatch` no-ops and the stale-drop re-dispatch
                    // (epoch mismatch) picks up the fresh content.
                    st.buffers.open(&uri, text, version);
                    bump_epoch(st, &uri);
                    st.debouncer.cancel(&uri);
                    request_dispatch(&uri, ctx, st);
                }
            }
            "textDocument/didChange" => {
                if let Ok(p) = not.extract::<DidChangeTextDocumentParams>("textDocument/didChange") {
                    // FULL sync: the last content change IS the whole buffer.
                    let version = p.text_document.version;
                    if let Some(change) = p.content_changes.into_iter().last() {
                        let uri = p.text_document.uri;
                        // A buffer edit NEVER invalidates the project context (S4,
                        // ADR-0029): buffer edits are virtual and single-file scope;
                        // only the config / watched-file surface bumps the generation.
                        // Update the buffer SYNCHRONOUSLY (hover/completion/symbols
                        // must see the latest text at once) but DEFER the publish:
                        // schedule a debounced fire `ctx.debounce` after this (the
                        // last) change. A further didChange within the window
                        // overwrites this deadline, coalescing the burst into one
                        // publish of the final content (S2, ADR-0029 §debounce).
                        st.buffers.change(&uri, change.text, version);
                        st.debouncer.schedule(&uri, Instant::now() + ctx.debounce);
                    }
                }
            }
            "textDocument/didClose" => {
                if let Ok(p) = not.extract::<DidCloseTextDocumentParams>("textDocument/didClose") {
                    let uri = p.text_document.uri;
                    st.buffers.close(&uri);
                    // Bump the open-epoch (S4) so a worker still in flight for this
                    // URI is epoch-dropped when it returns — even if a reopen reuses
                    // the same LSP version. Cancel any pending debounced publish so
                    // no stale diagnostics fire after the close, THEN clear inline
                    // markers with an empty publish (an idle-clear on the loop
                    // thread, not a compute — so it does not go through the worker
                    // channel). A worker still in flight is left to finish;
                    // `handle_result` finds the buffer closed (current version
                    // `None`) and DROPS its result — no stale publish escapes.
                    bump_epoch(st, &uri);
                    st.debouncer.cancel(&uri);
                    send_diagnostics(connection, &uri, Vec::new())?;
                }
            }
            "workspace/didChangeWatchedFiles" => {
                // Tier-1 invalidation trigger (S4). Invalidate + re-analyse ALL open
                // buffers ONLY if a changed URI is on the config + project-signature
                // surface (`.rigor.yml` / `Gemfile.lock` / a project `*.rb` /
                // `sig/**/*.rbs`). An unrelated path (a `.txt`, a build artifact) does
                // NOT invalidate — avoiding a needless ~100-300 ms rebuild.
                let relevant = watched_files_params_are_relevant(&not.params);
                if relevant {
                    invalidate(st);
                    reanalyze_open_buffers(ctx, st);
                }
            }
            "workspace/didChangeConfiguration" => {
                // Configuration refresh (S4): always invalidate + re-analyse open
                // buffers (the payload shape is client-specific; v1 ignores it and
                // rebuilds so the next publish observes any external config change).
                invalidate(st);
                reanalyze_open_buffers(ctx, st);
            }
            _ => {}
        },
        Message::Response(_) => {}
    }
    Ok(false)
}

/// Send the server→client `client/registerCapability` request registering the
/// watched-files globs (S4): the config + project-signature surface that tier-1
/// invalidation cares about. Fire-and-forget — the client's response is ignored.
fn register_watched_files(connection: &Connection) -> Result<(), String> {
    let params = serde_json::json!({
        "registrations": [{
            "id": "rigor-watched-files",
            "method": "workspace/didChangeWatchedFiles",
            "registerOptions": {
                "watchers": [
                    { "globPattern": "**/*.rb" },
                    { "globPattern": "**/.rigor.yml" },
                    { "globPattern": "**/Gemfile.lock" },
                    { "globPattern": "**/sig/**/*.rbs" }
                ]
            }
        }]
    });
    let req = lsp_server::Request::new(
        lsp_server::RequestId::from("rigor-watched-files".to_string()),
        "client/registerCapability".to_string(),
        params,
    );
    connection
        .sender
        .send(Message::Request(req))
        .map_err(|e| e.to_string())
}

/// Whether a `workspace/didChangeWatchedFiles` payload touches the config +
/// project-signature surface (S4). Parses `{ changes: [{ uri, .. }] }` and returns
/// `true` if ANY changed URI is relevant per [`watched_file_is_relevant`]. A
/// malformed / empty payload is not relevant (no invalidation).
fn watched_files_params_are_relevant(params: &serde_json::Value) -> bool {
    params
        .get("changes")
        .and_then(|c| c.as_array())
        .is_some_and(|changes| {
            changes.iter().any(|c| {
                c.get("uri")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(watched_file_is_relevant)
            })
        })
}

/// Whether a single changed-file URI is on the tier-1 invalidation surface (S4):
/// `.rigor.yml`, `Gemfile.lock`, a project `*.rb`, or a `sig/**/*.rbs` signature
/// (project sig dirs matter for rigor-rs per ADR-0033). Matches on the URI string
/// tail — sufficient for the watcher globs the server registers.
fn watched_file_is_relevant(uri: &str) -> bool {
    uri.ends_with(".rigor.yml")
        || uri.ends_with("Gemfile.lock")
        || uri.ends_with(".rb")
        || (uri.ends_with(".rbs") && uri.contains("/sig/"))
}

/// A stable string key for a document URI (the buffer table is keyed by it).
fn uri_key(uri: &Uri) -> String {
    uri.as_str().to_string()
}

/// Spawn a rayon worker to compute diagnostics for `uri` off the loop thread (S3).
/// Records the URI as in-flight and CANCELS its pending debounce (the worker now
/// covers the latest content — no separate deferred publish needed, so no
/// redundant re-analysis). The worker captures the buffer snapshot `(text,
/// version)`, the project `generation` + the URI's open-`epoch` at dispatch (S4),
/// an `Arc<ProjectContext>` clone (the shared analysis context — index / suppress
/// set / sidecar folder, exactly the `check` pipeline's shared-worker contract), a
/// `worker_gate` clone (the test seam), and a `results_tx` clone.
///
/// **Never-stuck.** The worker's body is `catch_unwind`-wrapped, so even a panic
/// (in the gate or the compute) yields an empty-diags result rather than a lost
/// send: the worker ALWAYS sends exactly one `Computed`, so the loop's `in_flight`
/// entry for this URI is always cleared in `handle_result`. `compute_diagnostics`
/// is itself panic-isolated (ADR-0016); this outer catch backstops the gate seam
/// and any unexpected panic so a dying worker never strands a URI in flight.
///
/// The unbounded `send` only fails if the receiver is gone (the loop returned —
/// shutdown); that `Err` is ignored, so a detached worker never blocks or panics.
fn spawn_worker(uri: &Uri, text: String, version: i32, ctx: &ServerContext, st: &mut Session) {
    st.in_flight.insert(uri_key(uri));
    st.debouncer.cancel(uri);
    let generation = st.project.generation;
    let epoch = current_epoch(st, uri);
    let project = Arc::clone(&st.project);
    let gate = Arc::clone(&ctx.worker_gate);
    let tx = st.results_tx.clone();
    let uri = uri.clone();
    rayon::spawn(move || {
        let diags = panic::catch_unwind(AssertUnwindSafe(|| {
            gate(version, generation); // test seam: may block (hold mid-flight) or panic.
            compute_diagnostics(&project, &text)
        }))
        .unwrap_or_default();
        // Always send exactly one result (even empty on a caught panic), so the
        // loop's in-flight tracking for this URI clears. `Err` = loop gone (shutdown).
        let _ = tx.send(Computed { uri, version, generation, epoch, diags });
    });
}

/// Handle one worker result — the loop's single-writer publish point (S3/S4).
/// Clears the URI's `in_flight` entry, then applies the three-axis stale-drop with
/// **no-lost-update re-dispatch**. A result is LIVE only if all three still match:
/// **version** (no edit past what was analysed, S3), **generation** (no `invalidate`
/// since dispatch, S4), and **epoch** (no `didClose`/`didOpen` cycle since dispatch,
/// S4 — the close+reopen version-reuse nit). Otherwise: a closed buffer drops
/// silently; any stale axis DROPS + [`request_dispatch`]es the latest content under
/// the current context (so the final state is always eventually published).
fn handle_result(
    connection: &Connection,
    ctx: &ServerContext,
    st: &mut Session,
    computed: Computed,
) -> Result<(), String> {
    st.in_flight.remove(&uri_key(&computed.uri));
    match st.buffers.current_version(&computed.uri) {
        // Buffer closed while the worker ran — drop the result (no stale publish).
        None => Ok(()),
        Some(cur) => {
            let live = cur == computed.version
                && computed.generation == st.project.generation
                && computed.epoch == current_epoch(st, &computed.uri);
            if live {
                // All three axes (version / generation / epoch) current — publish.
                send_diagnostics(connection, &computed.uri, computed.diags)
            } else {
                // Superseded (edit / invalidate / close+reopen) — drop this result
                // and re-dispatch the latest content so the final state is always
                // eventually published under the current context.
                request_dispatch(&computed.uri, ctx, st);
                Ok(())
            }
        }
    }
}

/// Send a `window/showMessage` notification (ADR-0036 posture disclosure).
fn send_show_message(
    connection: &Connection,
    typ: MessageType,
    message: String,
) -> Result<(), String> {
    let params = ShowMessageParams { typ, message };
    let not = lsp_server::Notification::new("window/showMessage".to_string(), params);
    connection
        .sender
        .send(Message::Notification(not))
        .map_err(|e| e.to_string())
}

/// Send a `textDocument/publishDiagnostics` notification.
fn send_diagnostics(
    connection: &Connection,
    uri: &Uri,
    diagnostics: Vec<Diagnostic>,
) -> Result<(), String> {
    let params = PublishDiagnosticsParams { uri: uri.clone(), diagnostics, version: None };
    let not = lsp_server::Notification::new(
        "textDocument/publishDiagnostics".to_string(),
        params,
    );
    connection
        .sender
        .send(Message::Notification(not))
        .map_err(|e| e.to_string())
}

/// Run the single-file analysis path over `text` and map the findings to LSP
/// diagnostics. Reuses the exact `check` pipeline (parse → lower → build a
/// single-file `SourceIndex` → `analyze_with_source`), plus the inline
/// `# rigor:disable` and config `disable:` suppression, so the editor's inline
/// markers match `rigor check` on the same content. Panic-isolated (ADR-0016): a
/// malformed buffer that trips the parser yields no diagnostics, never a crash.
fn compute_diagnostics(project: &ProjectContext, text: &str) -> Vec<Diagnostic> {
    let bytes = text.as_bytes().to_vec();
    // Skip ERB templates (matches `check` + the reference's ErbTemplateDetector):
    // Prism's error recovery over a `<%= … %>` template yields a garbage AST.
    if rigor_parse::looks_like_erb_template(&bytes) {
        return Vec::new();
    }
    let analysed = panic::catch_unwind(AssertUnwindSafe(|| {
        let result = parse(&bytes);
        let comments = comment_lines(&result, &bytes);
        let ast = lower(&result);
        let source = SourceIndex::build(&ast, &project.index);
        let mut interner = Interner::new();
        let folder = project
            .folder
            .as_deref()
            .map(|f| f as &(dyn rigor_infer::RubyFolder + Sync));
        let mut diags =
            analyze_with_source_and_folder(&ast, &mut interner, &project.index, &source, folder);
        diags.extend(rigor_rules::shadowed_rescue_diagnostics(
            &ast, &project.index, &source, text,
        ));
        (diags, comments)
    }));

    let (mut diags, comments) = match analysed {
        Ok(pair) => pair,
        Err(_) => return Vec::new(),
    };
    // Suppression-marker surveillance, before `filter_suppressed` (self-suppressible).
    diags.extend(rigor_rules::suppression_marker_diagnostics(&comments));

    // Inline `# rigor:disable` suppression (same as `check`): key each diag on its
    // 1-based line, filter, then drop config-`disable:`d rules.
    let with_lines: Vec<(usize, rigor_rules::Diagnostic)> = diags
        .into_iter()
        .map(|d| (offset_to_position(text, d.start_offset).line as usize + 1, d))
        .collect();

    filter_suppressed(with_lines, &comments)
        .into_iter()
        .filter(|(_, d)| !project.disable.suppresses(d.rule_id))
        .map(|(_, d)| to_lsp_diagnostic(text, &d))
        .collect()
}

/// Map one rigor `Diagnostic` to an LSP `Diagnostic`. `source` = `"rigor"`,
/// `code` = the rule id, severity per ADR-0029 (`error`→Error, `warning`→Warning,
/// `info`→Information). The range is the diagnostic's byte span, resolved to
/// 0-based UTF-16 LSP positions.
fn to_lsp_diagnostic(text: &str, d: &rigor_rules::Diagnostic) -> Diagnostic {
    let start = offset_to_position(text, d.start_offset);
    let end = offset_to_position(text, d.end_offset.max(d.start_offset));
    let severity = match d.severity {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Info => DiagnosticSeverity::INFORMATION,
    };
    Diagnostic {
        range: Range { start, end },
        severity: Some(severity),
        code: Some(NumberOrString::String(d.rule_id.to_string())),
        source: Some("rigor".to_string()),
        message: d.message.clone(),
        ..Default::default()
    }
}

/// Answer `textDocument/hover`: locate the deepest node under the cursor, type it,
/// and render a node-aware markdown card. A `Call` shows `receiver#method →
/// return` (plus the RBS arity when the receiver class is core-known); a constant
/// shows `Name : type`; anything else shows the inferred type + node kind. Reuses
/// the `type-of` node-locator + type renderer. Returns `None` when the buffer is
/// unknown, the position is out of range, or no node covers it — a null hover.
fn hover(
    project: &ProjectContext,
    buffers: &BufferTable,
    params: &HoverParams,
) -> Option<Hover> {
    let pos = &params.text_document_position_params;
    let text = buffers.text(&pos.text_document.uri)?;
    let offset = position_to_offset(text, pos.position)?;

    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let ast = lower(&parse(text.as_bytes()));
        let node_id = crate::type_of::locate_node(&ast, offset)?;
        let source = SourceIndex::build(&ast, &project.index);
        let typer = Typer::with_source(&project.index, &source);
        let mut interner = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut interner);
        let ty = typer.type_of(&ast, node_id, &env, &mut interner);
        let (start, end) = ast.get(node_id).span();
        let type_render = crate::type_of::render_type(&interner, &project.index, &source, ty);

        // Extract owned node bits so later `&mut interner` calls don't clash with
        // the `&ast` borrow of `node`.
        let call_bits = match ast.get(node_id) {
            Node::Call { receiver, method, .. } => Some((*receiver, method.clone())),
            _ => None,
        };
        let const_name = match ast.get(node_id) {
            Node::ConstantRead { name, .. } if !name.is_empty() => Some(name.clone()),
            _ => None,
        };
        // Definition-site hover (hovering on a `class`/`module`/`def` name): a
        // signature line built from the node, no typing needed.
        let def_sig = match ast.get(node_id) {
            Node::ClassDef { name, superclass_path, .. } if !name.is_empty() => Some(match superclass_path {
                Some(sup) => format!("class {name} < {sup}"),
                None => format!("class {name}"),
            }),
            Node::ModuleDef { name, .. } if !name.is_empty() => Some(format!("module {name}")),
            Node::Definition { name: Some(n), params, .. } => Some(match params {
                Some(ps) if !ps.is_empty() => format!("def {n}({})", ps.join(", ")),
                _ => format!("def {n}"),
            }),
            _ => None,
        };
        let kind = crate::type_of::node_kind(ast.get(node_id));

        let body = if let Some((receiver, method)) = call_bits {
            let recv_ty = receiver.map(|r| typer.type_of(&ast, r, &env, &mut interner));
            let recv_disp = recv_ty
                .map(|rt| receiver_display(&project.index, &typer, &interner, rt))
                .unwrap_or_else(|| "self".to_string());
            let mut sig = format!("{recv_disp}#{method} → {type_render}");
            if let Some(cls) = recv_ty.and_then(|rt| project.index.class_name_of(&interner, rt)) {
                if let Some((min, max)) = project.index.method_arity(cls, &method) {
                    let max_s = max.map_or_else(|| "∞".to_string(), |m| m.to_string());
                    sig.push_str(&format!("  (arity {min}..{max_s})"));
                }
            }
            format!("```ruby\n{sig}\n```\n\n*rigor: Call*")
        } else if let Some(sig) = def_sig {
            format!("```ruby\n{sig}\n```\n\n*rigor: definition*")
        } else if let Some(name) = const_name {
            format!("```ruby\n{name} : {type_render}\n```\n\n*rigor: Constant*")
        } else {
            format!("```ruby\n{type_render}\n```\n\n*rigor: {kind}*")
        };
        Some((body, start, end))
    }));

    let (value, start, end) = result.ok().flatten()?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(Range {
            start: offset_to_position(text, start),
            end: offset_to_position(text, end),
        }),
    })
}

// ---------------------------------------------------------------------------
// Completion (member-access method completion on `.` / `::`)
// ---------------------------------------------------------------------------

/// A stub method name injected at the cursor so a possibly-incomplete buffer
/// (`x.`, `x.up`) parses cleanly into a `Call` whose receiver we can type. Chosen
/// to be a valid, collision-unlikely lowercase identifier.
const COMPLETION_STUB: &str = "rigorCompletionHole";

/// Answer `textDocument/completion`: if the cursor sits after a `.`/`::` member
/// access, resolve the receiver's type and return its callable methods. Returns
/// `None` (a null completion) when the cursor isn't in a member-access context,
/// the buffer is unknown, or the receiver type is unresolved.
///
/// Robust to incomplete input via **placeholder injection**: a stub method name
/// is spliced in right after the separator (replacing any half-typed name), so
/// the parser yields a `Call { receiver, method: <stub> }` regardless of what the
/// user has typed. The receiver node is typed with the same `Typer` `hover`/`check`
/// use; its class drives instance- vs singleton-method enumeration. The half-typed
/// prefix is intentionally dropped — the client filters the full set by it.
fn completion(
    project: &ProjectContext,
    buffers: &BufferTable,
    params: &CompletionParams,
) -> Option<CompletionResponse> {
    let tdp = &params.text_document_position;
    let text = buffers.text(&tdp.text_document.uri)?;
    let offset = position_to_offset(text, tdp.position)?;
    let bytes = text.as_bytes();

    // Scan back over any half-typed identifier to find where it starts.
    let mut ident_start = offset;
    while ident_start > 0 && is_ident_byte(bytes[ident_start - 1]) {
        ident_start -= 1;
    }
    // The separator must sit immediately before the (possibly empty) identifier:
    // `::` (constant/class scope) or a plain `.` (not part of a `..`/`...` range).
    let is_member_access = (ident_start >= 2 && &text[ident_start - 2..ident_start] == "::")
        || (ident_start >= 1
            && bytes[ident_start - 1] == b'.'
            && !(ident_start >= 2 && bytes[ident_start - 2] == b'.'));
    if !is_member_access {
        return None; // not a member-access completion context.
    }
    let stub_at = ident_start; // where the stub name begins (right after the sep).

    // Splice the stub in after the separator, dropping any half-typed name.
    let synth = format!("{}{}{}", &text[..ident_start], COMPLETION_STUB, &text[offset..]);

    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let ast = lower(&parse(synth.as_bytes()));
        // Our injected call is the unique `Call` whose method-name token starts
        // exactly at `stub_at`.
        let receiver = ast.iter().find_map(|(_, n)| match n {
            Node::Call { receiver, message_span, .. } if message_span.0 == stub_at => Some(*receiver),
            _ => None,
        })??;
        let source = SourceIndex::build(&ast, &project.index);
        let typer = Typer::with_source(&project.index, &source);
        let mut interner = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut interner);
        let ty = typer.type_of(&ast, receiver, &env, &mut interner);
        Some(method_names_for(&project.index, &typer, &interner, ty))
    }));

    let names = result.ok().flatten()?;
    if names.is_empty() {
        return None;
    }
    let items: Vec<CompletionItem> = names
        .into_iter()
        .map(|m| CompletionItem {
            label: m.to_string(),
            kind: Some(CompletionItemKind::METHOD),
            ..Default::default()
        })
        .collect();
    Some(CompletionResponse::Array(items))
}

/// Resolve the receiver type to the set of callable method names: singleton
/// (class-object) methods for a `Type::Singleton` receiver (a bare class
/// constant), else instance methods on the receiver's concrete core class. Empty
/// when the class isn't resolvable (a `Dynamic`/project/unknown receiver ⇒ no
/// completion, never a guess).
fn method_names_for(
    index: &CoreIndex,
    typer: &Typer<'_>,
    interner: &Interner,
    ty: TypeId,
) -> Vec<&'static str> {
    if let Type::Singleton(class) = interner.get(ty) {
        return match typer.source().class_name_for_id(*class) {
            Some(name) => index.singleton_method_names(name),
            None => Vec::new(),
        };
    }
    match index.class_name_of(interner, ty) {
        Some(name) => index.instance_method_names(name),
        None => Vec::new(),
    }
}

/// An ASCII identifier byte (`[A-Za-z0-9_]`) — used to scan a half-typed name.
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Display a receiver's type as a class name for a hover signature: a bare class
/// constant renders `singleton(Name)`, a concrete core instance its class name,
/// and anything else falls back to the general type render (e.g. `Dynamic[top]`).
fn receiver_display(
    index: &CoreIndex,
    typer: &Typer<'_>,
    interner: &Interner,
    ty: TypeId,
) -> String {
    if let Type::Singleton(class) = interner.get(ty) {
        return typer
            .source()
            .class_name_for_id(*class)
            .map_or_else(|| "singleton(?)".to_string(), |n| format!("singleton({n})"));
    }
    index
        .class_name_of(interner, ty)
        .map_or_else(|| crate::type_of::render_type(interner, index, typer.source(), ty), |n| n.to_string())
}

// ---------------------------------------------------------------------------
// Document symbols (outline: classes / modules / methods)
// ---------------------------------------------------------------------------

/// Answer `textDocument/documentSymbol`: a nested outline of the buffer's
/// classes, modules, and methods, built from the lowered AST. Returns `None`
/// (null) for an unknown buffer or a file with no definitions. Panic-isolated.
fn document_symbols(
    buffers: &BufferTable,
    params: &DocumentSymbolParams,
) -> Option<DocumentSymbolResponse> {
    let text = buffers.text(&params.text_document.uri)?;
    let syms = panic::catch_unwind(AssertUnwindSafe(|| {
        let ast = lower(&parse(text.as_bytes()));
        crate::outline::build(&ast).iter().map(|s| to_document_symbol(s, text)).collect::<Vec<_>>()
    }))
    .ok()?;
    if syms.is_empty() {
        return None;
    }
    Some(DocumentSymbolResponse::Nested(syms))
}

/// Adapt a shared [`crate::outline::SymNode`] into an LSP `DocumentSymbol`
/// (byte-offset spans → 0-based UTF-16 ranges; kind → `SymbolKind`).
fn to_document_symbol(s: &crate::outline::SymNode, text: &str) -> DocumentSymbol {
    use crate::outline::SymKind;
    let kids: Vec<DocumentSymbol> = s.children.iter().map(|c| to_document_symbol(c, text)).collect();
    let to_range = |(a, b): (usize, usize)| Range {
        start: offset_to_position(text, a),
        end: offset_to_position(text, b),
    };
    let kind = match s.kind {
        SymKind::Class => SymbolKind::CLASS,
        SymKind::Module => SymbolKind::MODULE,
        SymKind::Method => SymbolKind::METHOD,
    };
    #[allow(deprecated)] // `deprecated` field is required by the struct literal.
    DocumentSymbol {
        name: s.name.clone(),
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range: to_range(s.full),
        selection_range: to_range(s.sel),
        children: if kids.is_empty() { None } else { Some(kids) },
    }
}

// ---------------------------------------------------------------------------
// Position <-> byte-offset (LSP: 0-based line, 0-based UTF-16 `character`)
// ---------------------------------------------------------------------------

/// Byte offset → LSP `Position` (0-based line, 0-based UTF-16 character). The
/// column is counted in UTF-16 code units per the LSP default position encoding.
fn offset_to_position(text: &str, offset: usize) -> Position {
    let offset = offset.min(text.len());
    let mut line = 0u32;
    let mut line_start = 0usize;
    for (i, b) in text.as_bytes().iter().enumerate() {
        if i >= offset {
            break;
        }
        if *b == b'\n' {
            line += 1;
            line_start = i + 1;
        }
    }
    let character: u32 = text[line_start..offset]
        .chars()
        .map(|c| c.len_utf16() as u32)
        .sum();
    Position { line, character }
}

/// LSP `Position` → byte offset. Walks to the 0-based `line`, then advances
/// `character` UTF-16 code units into it; a position past the line's end clamps to
/// the line end (LSP semantics). Returns `None` if the line is past EOF.
fn position_to_offset(text: &str, pos: Position) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut line = 0u32;
    let mut idx = 0usize;
    while line < pos.line {
        match bytes.get(idx) {
            Some(b'\n') => {
                line += 1;
                idx += 1;
            }
            Some(_) => idx += 1,
            None => return None, // line past end of buffer
        }
    }
    let line_start = idx;
    let line_end = text[line_start..]
        .find('\n')
        .map(|n| line_start + n)
        .unwrap_or(text.len());
    let mut u16_count = 0u32;
    for (i, c) in text[line_start..line_end].char_indices() {
        if u16_count >= pos.character {
            return Some(line_start + i);
        }
        u16_count += c.len_utf16() as u32;
    }
    Some(line_end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal tier-1 project context (empty core index, no sidecar, generation
    /// 0) for the pure `compute_diagnostics` / `hover` / `completion` unit tests.
    fn project() -> ProjectContext {
        ProjectContext {
            generation: 0,
            index: CoreIndex::new(),
            disable: Config::default().disable_matcher(),
            folder: None,
        }
    }

    #[test]
    fn position_roundtrip_ascii() {
        let text = "s = \"hi\"\ns.upcase\n";
        // line 1 (0-based), char 2 → the `u` of upcase.
        let off = position_to_offset(text, Position { line: 1, character: 2 }).unwrap();
        assert_eq!(&text[off..off + 6], "upcase");
        let back = offset_to_position(text, off);
        assert_eq!(back, Position { line: 1, character: 2 });
    }

    #[test]
    fn position_utf16_multibyte() {
        // "é" is 1 UTF-16 unit but 2 UTF-8 bytes; "𐐷" is 2 UTF-16 units, 4 bytes.
        let text = "x = 'é𐐷z'\n";
        // Walk to the `z`: chars before it on line 0 are x,space,=,space,',é,𐐷.
        let z = text.find('z').unwrap();
        let pos = offset_to_position(text, z);
        // UTF-16 units before z: x(1) (1)=(1) (1)'(1) é(1) 𐐷(2) = 8.
        assert_eq!(pos, Position { line: 0, character: 8 });
        assert_eq!(position_to_offset(text, pos).unwrap(), z);
    }

    #[test]
    fn diagnostics_flag_a_typo() {
        // `"hi".lenght` — undefined method, one diagnostic.
        let diags = compute_diagnostics(&project(), "x = \"hi\"\nx.lenght\n");
        assert_eq!(diags.len(), 1, "one undefined-method diagnostic");
        let d = &diags[0];
        assert_eq!(d.source.as_deref(), Some("rigor"));
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(d.code, Some(NumberOrString::String("call.undefined-method".to_string())));
        assert_eq!(d.range.start.line, 1); // 0-based: line 2 in the file
    }

    #[test]
    fn diagnostics_respect_inline_suppression() {
        // A `# rigor:disable <rule>` on the line suppresses the finding, like
        // `check` (a bare `# rigor:disable` with no rule token is a no-op — it
        // needs a rule, matching the reference's `\s+(rules)` directive grammar).
        let diags =
            compute_diagnostics(&project(), "x = \"hi\"\nx.lenght # rigor:disable undefined-method\n");
        assert!(diags.is_empty(), "inline disable suppresses the diagnostic");
    }

    #[test]
    fn diagnostics_clean_source_is_empty() {
        let diags = compute_diagnostics(&project(), "x = \"hi\"\nx.upcase\n");
        assert!(diags.is_empty());
    }

    #[test]
    fn hover_reports_a_type() {
        let mut buffers = BufferTable::new();
        let uri: Uri = "file:///t.rb".parse().unwrap();
        buffers.open(&uri, "n = 42\n".to_string(), 1);
        let params = HoverParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                position: Position { line: 0, character: 4 }, // on `42`
            },
            work_done_progress_params: Default::default(),
        };
        let h = hover(&project(), &buffers, &params).expect("a hover");
        match h.contents {
            HoverContents::Markup(m) => assert!(m.value.contains("42"), "{}", m.value),
            _ => panic!("expected markup hover"),
        }
    }

    /// Run completion at a 0-based (line, character) over a single buffer,
    /// returning the candidate labels (empty when None).
    fn complete(text: &str, line: u32, character: u32) -> Vec<String> {
        let mut buffers = BufferTable::new();
        let uri: Uri = "file:///c.rb".parse().unwrap();
        buffers.open(&uri, text.to_string(), 1);
        let params = CompletionParams {
            text_document_position: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };
        match completion(&project(), &buffers, &params) {
            Some(CompletionResponse::Array(items)) => items.into_iter().map(|i| i.label).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn completion_instance_methods_on_a_string() {
        // `s = "hi"\ns.` — cursor right after the dot on line 2 (char 2).
        let labels = complete("s = \"hi\"\ns.\n", 1, 2);
        assert!(labels.contains(&"upcase".to_string()), "has upcase: {labels:?}");
        assert!(labels.contains(&"length".to_string()), "has length: {labels:?}");
    }

    #[test]
    fn completion_with_partial_prefix_still_lists_full_set() {
        // `s = "hi"\ns.up` — cursor after `up`; the half-typed prefix is dropped,
        // the FULL instance-method set is returned (client filters by `up`).
        let labels = complete("s = \"hi\"\ns.up\n", 1, 4);
        assert!(labels.contains(&"upcase".to_string()), "{labels:?}");
    }

    #[test]
    fn completion_integer_methods() {
        let labels = complete("n = 3\nn.\n", 1, 2);
        assert!(labels.contains(&"times".to_string()), "has times: {labels:?}");
    }

    #[test]
    fn completion_singleton_methods_on_a_class_constant() {
        // `Time.` — a bare toplevel RBS class constant types to Singleton(Time),
        // so completion offers class (singleton) methods like `now`.
        let labels = complete("Time.\n", 0, 5);
        assert!(labels.contains(&"now".to_string()), "has Time.now: {labels:?}");
    }

    #[test]
    fn completion_not_in_member_access_is_empty() {
        // A bare local write, cursor after `1` — no `.`/`::` before it.
        assert!(complete("x = 1\n", 0, 5).is_empty());
    }

    #[test]
    fn completion_on_dynamic_receiver_is_empty() {
        // `foo.` where `foo` is unbound ⇒ Dynamic receiver ⇒ no completion (no guess).
        assert!(complete("foo.\n", 0, 4).is_empty());
    }

    #[test]
    fn document_symbols_nest_methods_under_classes() {
        let src = "class Foo\n  def bar\n  end\n  def baz\n  end\nend\nmodule M\nend\n";
        let mut buffers = BufferTable::new();
        let uri: Uri = "file:///s.rb".parse().unwrap();
        buffers.open(&uri, src.to_string(), 1);
        let params = DocumentSymbolParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let resp = document_symbols(&buffers, &params).expect("symbols");
        let roots = match resp {
            DocumentSymbolResponse::Nested(v) => v,
            _ => panic!("expected nested"),
        };
        // Two roots: class Foo, module M.
        assert_eq!(roots.len(), 2);
        let foo = roots.iter().find(|s| s.name == "Foo").expect("Foo");
        assert_eq!(foo.kind, SymbolKind::CLASS);
        // Foo nests two methods.
        let kids = foo.children.as_ref().expect("methods under Foo");
        let mut names: Vec<&str> = kids.iter().map(|k| k.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["bar", "baz"]);
        assert!(kids.iter().all(|k| k.kind == SymbolKind::METHOD));
        let m = roots.iter().find(|s| s.name == "M").expect("M");
        assert_eq!(m.kind, SymbolKind::MODULE);
    }

    #[test]
    fn document_symbols_empty_for_scriptish_file() {
        let mut buffers = BufferTable::new();
        let uri: Uri = "file:///s.rb".parse().unwrap();
        buffers.open(&uri, "x = 1\nputs x\n".to_string(), 1);
        let params = DocumentSymbolParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        assert!(document_symbols(&buffers, &params).is_none());
    }

    #[test]
    fn hover_call_shows_receiver_method_signature() {
        // `s = "hi"\ns.upcase` — hover on `upcase` (line 2, char 3) shows a
        // `String#upcase → …` signature with the RBS arity.
        let mut buffers = BufferTable::new();
        let uri: Uri = "file:///t.rb".parse().unwrap();
        buffers.open(&uri, "s = \"hi\"\ns.upcase\n".to_string(), 1);
        let params = HoverParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                position: Position { line: 1, character: 2 },
            },
            work_done_progress_params: Default::default(),
        };
        let h = hover(&project(), &buffers, &params).expect("a hover");
        let HoverContents::Markup(m) = h.contents else { panic!("markup") };
        assert!(m.value.contains("String#upcase"), "signature: {}", m.value);
        assert!(m.value.contains("arity"), "arity shown: {}", m.value);
        assert!(m.value.contains("*rigor: Call*"), "{}", m.value);
    }

    /// Hover value at a 0-based (line, char) over a single buffer (or empty).
    fn hover_value(text: &str, line: u32, character: u32) -> String {
        let mut buffers = BufferTable::new();
        let uri: Uri = "file:///h.rb".parse().unwrap();
        buffers.open(&uri, text.to_string(), 1);
        let params = HoverParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
        };
        match hover(&project(), &buffers, &params) {
            Some(Hover { contents: HoverContents::Markup(m), .. }) => m.value,
            _ => String::new(),
        }
    }

    #[test]
    fn hover_on_a_def_shows_its_signature() {
        // `def greet(name)` — hover on the method name (line 1, char 4).
        let v = hover_value("def greet(name)\n  name\nend\n", 0, 4);
        assert!(v.contains("def greet(name)"), "{v}");
        assert!(v.contains("*rigor: definition*"), "{v}");
    }

    #[test]
    fn hover_on_a_class_shows_its_header() {
        // `class Foo < Bar` — hover on the class name (line 1, char 6).
        let v = hover_value("class Foo < Bar\nend\n", 0, 6);
        assert!(v.contains("class Foo < Bar"), "{v}");
    }

    #[test]
    fn hover_unknown_buffer_is_none() {
        let params = HoverParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier {
                    uri: "file:///missing.rb".parse().unwrap(),
                },
                position: Position { line: 0, character: 0 },
            },
            work_done_progress_params: Default::default(),
        };
        assert!(hover(&project(), &BufferTable::new(), &params).is_none());
    }

    #[test]
    fn buffer_table_records_version_and_dirty() {
        // The BufferTable metadata (version, dirty) is maintained per ADR-0029
        // even though S1 branches on neither — the S2/S3 consumers arrive later.
        let mut t = BufferTable::new();
        let uri: Uri = "file:///b.rb".parse().unwrap();
        t.open(&uri, "a\n".to_string(), 1);
        let e = t.entries.get(&uri_key(&uri)).unwrap();
        assert_eq!(e.version, 1);
        assert!(!e.dirty, "an opened buffer is clean");
        t.change(&uri, "b\n".to_string(), 2);
        let e = t.entries.get(&uri_key(&uri)).unwrap();
        assert_eq!(e.version, 2);
        assert!(e.dirty, "a changed buffer is dirty");
        assert_eq!(t.text(&uri), Some("b\n"));
        t.close(&uri);
        assert_eq!(t.text(&uri), None);
    }

    // ---------------------------------------------------------------------
    // Debouncer: pure, deterministic unit tests (explicit `Instant`s, no sleep).
    // These prove the coalescing + cancel + earliest/take_due invariants without
    // any wall-clock dependency — the timing seam the integration tests lean on.
    // ---------------------------------------------------------------------

    #[test]
    fn debouncer_coalesces_and_last_deadline_wins() {
        let mut d = Debouncer::new();
        let u: Uri = "file:///a.rb".parse().unwrap();
        let t0 = Instant::now();
        // Two schedules for the same URI within the window: the second wins.
        d.schedule(&u, t0 + Duration::from_millis(200));
        d.schedule(&u, t0 + Duration::from_millis(500));
        assert_eq!(d.pending.len(), 1, "one pending entry per URI (coalesced)");
        assert_eq!(d.earliest(), Some(t0 + Duration::from_millis(500)));
        // Not due at +300 (the deadline moved out to +500).
        assert!(d.take_due(t0 + Duration::from_millis(300)).is_empty());
        // Due at +600: exactly the final entry, then removed.
        let due = d.take_due(t0 + Duration::from_millis(600));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].as_str(), "file:///a.rb");
        assert!(d.earliest().is_none(), "a fired entry is removed");
    }

    #[test]
    fn debouncer_cancel_drops_pending() {
        let mut d = Debouncer::new();
        let u: Uri = "file:///a.rb".parse().unwrap();
        let t0 = Instant::now();
        d.schedule(&u, t0 + Duration::from_millis(100));
        d.cancel(&u); // didClose
        assert!(d.earliest().is_none());
        assert!(
            d.take_due(t0 + Duration::from_millis(200)).is_empty(),
            "a cancelled publish never fires"
        );
        d.cancel(&u); // idempotent
    }

    #[test]
    fn debouncer_earliest_is_the_min_across_uris() {
        let mut d = Debouncer::new();
        let a: Uri = "file:///a.rb".parse().unwrap();
        let b: Uri = "file:///b.rb".parse().unwrap();
        let t0 = Instant::now();
        d.schedule(&a, t0 + Duration::from_millis(300));
        d.schedule(&b, t0 + Duration::from_millis(100));
        assert_eq!(d.earliest(), Some(t0 + Duration::from_millis(100)));
        // Only `b` is due at +150; `a`'s later deadline stays pending.
        let due = d.take_due(t0 + Duration::from_millis(150));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].as_str(), "file:///b.rb");
        assert_eq!(d.earliest(), Some(t0 + Duration::from_millis(300)));
    }

    // ---------------------------------------------------------------------
    // Integration tests: the REAL loop over an in-memory connection.
    //
    // These drive `main_loop` through `lsp_server::Connection::memory()` and
    // assert the EXACT published-message sequence. The expected sequences were
    // captured from the pre-refactor (inline-publish) loop as the golden
    // reference; the S1 `select!`/worker-channel refactor must reproduce them
    // byte-for-byte.
    // ---------------------------------------------------------------------

    use lsp_server::{Notification, Request, RequestId};
    use std::thread;
    use std::time::Duration;

    /// A running server loop over an in-memory connection, plus the client end.
    struct Harness {
        client: Connection,
        server: Option<thread::JoinHandle<()>>,
    }

    impl Harness {
        /// Spawn the server loop with the default (200 ms) debounce.
        fn start() -> Self {
            Self::start_with_debounce(DEBOUNCE_DEFAULT)
        }

        /// Spawn the server loop on a thread (with an injected debounce interval)
        /// and complete the LSP handshake. Timing tests pass a SMALL interval
        /// (assert the deferred publish eventually arrives) or a LARGE one (assert
        /// it does NOT fire within a synchronous round-trip) — never a value the
        /// assertions race against.
        fn start_with_debounce(debounce: Duration) -> Self {
            Self::start_with_gate(debounce, production_gate())
        }

        /// Spawn the server loop with an injected debounce AND a worker gate (S3
        /// concurrency tests). The gate is called at the start of every rayon
        /// worker with the buffer version + project generation, so a test can hold a
        /// worker mid-flight (block until released) or force a panic — driving the
        /// version / generation / epoch stale-drop, one-in-flight, and never-stuck
        /// lifecycle deterministically, without any dependence on real rayon timing.
        /// The client advertises NO capabilities (no dynamic registration).
        fn start_with_gate(debounce: Duration, worker_gate: Arc<WorkerGate>) -> Self {
            Self::start_full(debounce, worker_gate, serde_json::json!({}))
        }

        /// Spawn the server loop, driving the client `initialize` with the given
        /// `client_caps` (S4): the server derives `watched_files_dynamic_registration`
        /// from the InitializeParams it receives, exactly as production does, so a
        /// test can assert the `client/registerCapability` handshake (or its absence).
        fn start_full(
            debounce: Duration,
            worker_gate: Arc<WorkerGate>,
            client_caps: serde_json::Value,
        ) -> Self {
            let (server_conn, client) = Connection::memory();
            let handle = thread::spawn(move || {
                let caps = serde_json::to_value(server_capabilities()).unwrap();
                // The authentic path: read the client's capabilities from the
                // InitializeParams the handshake returns (not discarded).
                let init_params = server_conn.initialize(caps).unwrap();
                let ctx = ServerContext {
                    debounce,
                    worker_gate,
                    watched_files_dynamic_registration:
                        client_supports_watched_files_registration(&init_params),
                };
                let project = Arc::new(ProjectContext {
                    generation: 0,
                    index: CoreIndex::new(),
                    disable: Config::default().disable_matcher(),
                    folder: None,
                });
                main_loop(&server_conn, &ctx, project, Config::default()).unwrap();
            });
            // Client-side handshake: initialize request → response → initialized.
            client
                .sender
                .send(Message::Request(Request::new(
                    RequestId::from(1),
                    "initialize".to_string(),
                    serde_json::json!({ "capabilities": client_caps }),
                )))
                .unwrap();
            client
                .receiver
                .recv_timeout(Duration::from_secs(10))
                .expect("initialize response");
            client
                .sender
                .send(Message::Notification(Notification::new(
                    "initialized".to_string(),
                    serde_json::json!({}),
                )))
                .unwrap();
            Harness { client, server: Some(handle) }
        }

        fn notify(&self, method: &str, params: serde_json::Value) {
            self.client
                .sender
                .send(Message::Notification(Notification::new(method.to_string(), params)))
                .unwrap();
        }

        fn request(&self, id: i32, method: &str, params: serde_json::Value) {
            self.client
                .sender
                .send(Message::Request(Request::new(
                    RequestId::from(id),
                    method.to_string(),
                    params,
                )))
                .unwrap();
        }

        fn recv(&self) -> Message {
            self.client
                .receiver
                .recv_timeout(Duration::from_secs(10))
                .expect("a server message")
        }

        /// Wait up to `dur` for a message; `None` on timeout. Used to assert a
        /// debounced publish does NOT arrive before its interval elapses.
        fn try_recv(&self, dur: Duration) -> Option<Message> {
            self.client.receiver.recv_timeout(dur).ok()
        }

        /// The next message, asserted to be a `publishDiagnostics`, parsed.
        fn recv_diags(&self) -> PublishDiagnosticsParams {
            match self.recv() {
                Message::Notification(n) if n.method == "textDocument/publishDiagnostics" => {
                    serde_json::from_value(n.params).unwrap()
                }
                other => panic!("expected publishDiagnostics, got {other:?}"),
            }
        }

        fn shutdown(&mut self) {
            self.request(999, "shutdown", serde_json::json!(null));
            match self.recv() {
                Message::Response(r) if r.id == RequestId::from(999) => {}
                other => panic!("expected shutdown response, got {other:?}"),
            }
            self.notify("exit", serde_json::json!(null));
            if let Some(h) = self.server.take() {
                h.join().unwrap();
            }
        }
    }

    /// A `didOpen` params JSON for `uri` / `text` / `version`.
    fn open_params(uri: &str, text: &str, version: i32) -> serde_json::Value {
        serde_json::json!({
            "textDocument": { "uri": uri, "languageId": "ruby", "version": version, "text": text }
        })
    }

    #[test]
    fn integration_didopen_publishes_one_diagnostic() {
        let mut h = Harness::start();
        h.notify(
            "textDocument/didOpen",
            open_params("file:///g.rb", "x = \"hi\"\nx.lenght\n", 1),
        );
        let d = h.recv_diags();
        assert_eq!(d.uri.as_str(), "file:///g.rb");
        assert_eq!(d.diagnostics.len(), 1, "exactly one diagnostic");
        let diag = &d.diagnostics[0];
        assert_eq!(
            diag.code,
            Some(NumberOrString::String("call.undefined-method".to_string()))
        );
        assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diag.source.as_deref(), Some("rigor"));
        assert_eq!(diag.range.start, Position { line: 1, character: 2 });
        assert_eq!(diag.range.end, Position { line: 1, character: 8 });
        h.shutdown();
    }

    #[test]
    fn integration_didchange_to_clean_republishes_empty() {
        // S2: didChange is now DEBOUNCED. With a small injected interval the
        // deferred publish still arrives (recv_diags waits up to 10 s); we assert
        // only that it arrives and is empty — no coalescing race here (one change).
        let mut h = Harness::start_with_debounce(Duration::from_millis(10));
        h.notify(
            "textDocument/didOpen",
            open_params("file:///g.rb", "x = \"hi\"\nx.lenght\n", 1),
        );
        // didOpen publishes IMMEDIATELY (not debounced): the one diagnostic.
        assert_eq!(h.recv_diags().diagnostics.len(), 1);
        h.notify(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": "file:///g.rb", "version": 2 },
                "contentChanges": [ { "text": "x = \"hi\"\nx.upcase\n" } ]
            }),
        );
        // The debounced publish fires ~10 ms later, carrying the (clean) content.
        let d = h.recv_diags();
        assert_eq!(d.uri.as_str(), "file:///g.rb");
        assert!(d.diagnostics.is_empty(), "clean content republishes an empty set");
        h.shutdown();
    }

    #[test]
    fn integration_didchange_deferred_until_interval() {
        // A didChange's publish does NOT appear before the debounce interval, but
        // DOES after. Interval 150 ms; we assert nothing arrives in a 20 ms window
        // (comfortably < 150 ms, so no race), then that the publish arrives.
        let mut h = Harness::start_with_debounce(Duration::from_millis(150));
        h.notify("textDocument/didOpen", open_params("file:///g.rb", "n = 42\n", 1));
        assert!(h.recv_diags().diagnostics.is_empty(), "clean didOpen → empty (immediate)");
        h.notify(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": "file:///g.rb", "version": 2 },
                "contentChanges": [ { "text": "x = \"hi\"\nx.lenght\n" } ]
            }),
        );
        // Not yet: the deadline is 150 ms out, this window is only 20 ms.
        assert!(
            h.try_recv(Duration::from_millis(20)).is_none(),
            "no publish before the debounce interval elapses"
        );
        // After the interval: the debounced publish with the typo diagnostic.
        let d = h.recv_diags();
        assert_eq!(d.diagnostics.len(), 1, "debounced publish carries the diagnostic");
        assert_eq!(
            d.diagnostics[0].code,
            Some(NumberOrString::String("call.undefined-method".to_string()))
        );
        h.shutdown();
    }

    #[test]
    fn integration_rapid_didchanges_coalesce_to_one_publish() {
        // Two rapid didChanges → exactly ONE publish carrying the FINAL content.
        // Both notifications are queued to the connection before the 120 ms
        // deadline can elapse, so the loop processes #1 (schedule) then #2
        // (reschedule) microseconds apart and fires once. The strict
        // last-writer-wins invariant is also proven deterministically in
        // `debouncer_coalesces_and_last_deadline_wins`.
        let mut h = Harness::start_with_debounce(Duration::from_millis(120));
        h.notify("textDocument/didOpen", open_params("file:///g.rb", "n = 42\n", 1));
        assert!(h.recv_diags().diagnostics.is_empty());
        // #1: clean. #2 (final): a typo → one diagnostic.
        h.notify(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": "file:///g.rb", "version": 2 },
                "contentChanges": [ { "text": "x = \"hi\"\nx.upcase\n" } ]
            }),
        );
        h.notify(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": "file:///g.rb", "version": 3 },
                "contentChanges": [ { "text": "x = \"hi\"\nx.lenght\n" } ]
            }),
        );
        // Exactly one publish, of the FINAL content.
        let d = h.recv_diags();
        assert_eq!(d.diagnostics.len(), 1, "coalesced: one publish of the final content");
        assert_eq!(
            d.diagnostics[0].code,
            Some(NumberOrString::String("call.undefined-method".to_string()))
        );
        // No second publish: a hover round-trips as the very next message.
        h.request(
            2,
            "textDocument/hover",
            serde_json::json!({
                "textDocument": { "uri": "file:///g.rb" },
                "position": { "line": 0, "character": 0 }
            }),
        );
        match h.recv() {
            Message::Response(r) => assert_eq!(r.id, RequestId::from(2)),
            other => panic!("expected hover response (a publish would mean a leaked debounce), got {other:?}"),
        }
        h.shutdown();
    }

    #[test]
    fn integration_didclose_cancels_pending_no_stale_publish() {
        // A didClose BEFORE the deadline cancels the pending publish and clears
        // markers; NO stale publish fires afterward. A 30 s interval guarantees
        // the debounce cannot fire during this millisecond-scale test.
        let mut h = Harness::start_with_debounce(Duration::from_secs(30));
        h.notify("textDocument/didOpen", open_params("file:///g.rb", "n = 42\n", 1));
        assert!(h.recv_diags().diagnostics.is_empty());
        // A change (schedules a publish 30 s out) then an immediate close.
        h.notify(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": "file:///g.rb", "version": 2 },
                "contentChanges": [ { "text": "x = \"hi\"\nx.lenght\n" } ]
            }),
        );
        h.notify(
            "textDocument/didClose",
            serde_json::json!({ "textDocument": { "uri": "file:///g.rb" } }),
        );
        // The didClose empty clear.
        let d = h.recv_diags();
        assert!(d.diagnostics.is_empty(), "didClose clears diagnostics");
        // No stale debounced publish: a hover round-trips as the next message
        // (the buffer is closed, so the result is null — but it's a Response).
        h.request(
            2,
            "textDocument/hover",
            serde_json::json!({
                "textDocument": { "uri": "file:///g.rb" },
                "position": { "line": 0, "character": 0 }
            }),
        );
        match h.recv() {
            Message::Response(r) => assert_eq!(r.id, RequestId::from(2)),
            other => panic!("expected hover response (a publish would be a stale debounce), got {other:?}"),
        }
        h.shutdown();
    }

    #[test]
    fn integration_hover_during_debounce_window_sees_latest_text_no_publish() {
        // Hover during the debounce window is answered SYNCHRONOUSLY from the
        // latest buffer text, and no publish precedes the response. 30 s interval
        // so the deferred publish cannot fire mid-test.
        let mut h = Harness::start_with_debounce(Duration::from_secs(30));
        h.notify("textDocument/didOpen", open_params("file:///g.rb", "s = \"hi\"\ns.upcase\n", 1));
        assert!(h.recv_diags().diagnostics.is_empty(), "clean didOpen → empty (immediate)");
        // Edit to a new expression; the buffer updates synchronously, publish
        // deferred 30 s.
        h.notify(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": "file:///g.rb", "version": 2 },
                "contentChanges": [ { "text": "n = 42\n" } ]
            }),
        );
        // Hover on the `42` in the LATEST text: the response comes back (not a
        // publish), and it reflects the edited content.
        h.request(
            2,
            "textDocument/hover",
            serde_json::json!({
                "textDocument": { "uri": "file:///g.rb" },
                "position": { "line": 0, "character": 4 }
            }),
        );
        match h.recv() {
            Message::Response(r) => {
                assert_eq!(r.id, RequestId::from(2));
                let hover: Option<Hover> = serde_json::from_value(r.result.unwrap()).unwrap();
                let Some(Hover { contents: HoverContents::Markup(m), .. }) = hover else {
                    panic!("expected a markup hover from the latest buffer text");
                };
                assert!(m.value.contains("42"), "hover sees the edited text: {}", m.value);
            }
            other => panic!("expected hover response (a publish would mean the debounce leaked), got {other:?}"),
        }
        h.shutdown();
    }

    #[test]
    fn integration_didclose_publishes_empty() {
        let mut h = Harness::start();
        h.notify(
            "textDocument/didOpen",
            open_params("file:///g.rb", "x = \"hi\"\nx.lenght\n", 1),
        );
        assert_eq!(h.recv_diags().diagnostics.len(), 1);
        h.notify(
            "textDocument/didClose",
            serde_json::json!({ "textDocument": { "uri": "file:///g.rb" } }),
        );
        let d = h.recv_diags();
        assert_eq!(d.uri.as_str(), "file:///g.rb");
        assert!(d.diagnostics.is_empty(), "didClose clears diagnostics");
        h.shutdown();
    }

    #[test]
    fn integration_hover_request_answers_like_inline() {
        let mut h = Harness::start();
        h.notify("textDocument/didOpen", open_params("file:///h.rb", "n = 42\n", 1));
        // A clean buffer's didOpen publishes an empty set first.
        assert!(h.recv_diags().diagnostics.is_empty());
        h.request(
            2,
            "textDocument/hover",
            serde_json::json!({
                "textDocument": { "uri": "file:///h.rb" },
                "position": { "line": 0, "character": 4 }
            }),
        );
        match h.recv() {
            Message::Response(r) => {
                assert_eq!(r.id, RequestId::from(2));
                let hover: Option<Hover> = serde_json::from_value(r.result.unwrap()).unwrap();
                let Some(Hover { contents: HoverContents::Markup(m), .. }) = hover else {
                    panic!("expected a markup hover");
                };
                assert!(m.value.contains("42"), "hover value: {}", m.value);
            }
            other => panic!("expected hover response, got {other:?}"),
        }
        h.shutdown();
    }

    // ---------------------------------------------------------------------
    // S3 concurrency: real rayon workers, driven DETERMINISTICALLY via the
    // worker-gate seam (hold a worker mid-flight / force a panic) + hover
    // round-trips as synchronization barriers. NONE of these depend on
    // wall-clock races: every ordering is pinned by the gate + FIFO message
    // processing, so the version-guard / one-in-flight / no-lost-update /
    // never-stuck invariants are established without a timing window.
    // ---------------------------------------------------------------------

    /// A worker-gate the test controls: a worker whose `version` is in `hold`
    /// blocks until [`GateHandle::release`] is called for it; a worker whose
    /// `version` is in `panic_on` panics (caught by the worker's `catch_unwind`).
    struct GateHandle {
        releases: HashMap<i32, crossbeam_channel::Sender<()>>,
        gate: Arc<WorkerGate>,
    }

    impl GateHandle {
        /// Release a held worker so it proceeds to compute + send its result.
        fn release(&self, version: i32) {
            if let Some(tx) = self.releases.get(&version) {
                let _ = tx.send(());
            }
        }
    }

    /// Build a controllable [`WorkerGate`]: workers at a `hold` version block on a
    /// per-version rendezvous until released; workers at a `panic_on` version
    /// panic. One held worker per version (the tests hold exactly one).
    fn gate_holding(hold: &[i32], panic_on: &[i32]) -> GateHandle {
        let mut releases = HashMap::new();
        let mut recvs: HashMap<i32, crossbeam_channel::Receiver<()>> = HashMap::new();
        for &v in hold {
            let (tx, rx) = crossbeam_channel::unbounded();
            releases.insert(v, tx);
            recvs.insert(v, rx);
        }
        let panics: HashSet<i32> = panic_on.iter().copied().collect();
        let gate: Arc<WorkerGate> = Arc::new(move |version: i32, _generation: u64| {
            if panics.contains(&version) {
                panic!("test gate: forced panic for version {version}");
            }
            if let Some(rx) = recvs.get(&version) {
                let _ = rx.recv(); // block until the test releases this version.
            }
        });
        GateHandle { releases, gate }
    }

    /// A `didChange` params JSON (FULL sync: the whole buffer as one change).
    fn change_params(uri: &str, text: &str, version: i32) -> serde_json::Value {
        serde_json::json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [ { "text": text } ]
        })
    }

    /// Round-trip a hover request as a SYNCHRONIZATION BARRIER: the loop services
    /// messages in FIFO order, so once this response returns, every earlier message
    /// (e.g. a preceding `didChange`) has been fully processed. The next server
    /// message MUST be the hover `Response`; a `publishDiagnostics` arriving here
    /// would mean a stale/leaked diagnostic escaped.
    fn hover_sync(h: &Harness, id: i32, uri: &str) {
        h.request(
            id,
            "textDocument/hover",
            serde_json::json!({
                "textDocument": { "uri": uri },
                "position": { "line": 0, "character": 0 }
            }),
        );
        match h.recv() {
            Message::Response(r) => assert_eq!(r.id, RequestId::from(id)),
            other => panic!(
                "expected hover response (a publish here would mean a leaked/stale diagnostic), got {other:?}"
            ),
        }
    }

    const TYPO: &str = "x = \"hi\"\nx.lenght\n"; // one `call.undefined-method`.
    const CLEAN: &str = "x = \"hi\"\nx.upcase\n"; // zero diagnostics.

    #[test]
    fn integration_s3_edit_during_flight_drops_stale_and_publishes_final_once() {
        // The core no-lost-update case. Hold the v1 worker mid-flight; edit to v2
        // while it is blocked; release v1. Its v1 result is STALE (buffer is v2) →
        // DROPPED, and a re-dispatch analyses v2 → the FINAL content publishes
        // exactly once. 30 s debounce so ONLY the stale-drop re-dispatch (never the
        // clock) drives the final publish — fully deterministic.
        let g = gate_holding(&[1], &[]);
        let mut h = Harness::start_with_gate(Duration::from_secs(30), g.gate.clone());
        // v1 = a TYPO (1 diag). If v1 leaked, we'd observe a 1-diagnostic publish.
        h.notify("textDocument/didOpen", open_params("file:///g.rb", TYPO, 1));
        // Edit to v2 = CLEAN while the v1 worker is blocked in the gate. The buffer
        // updates synchronously; no second worker spawns (one-in-flight).
        h.notify("textDocument/didChange", change_params("file:///g.rb", CLEAN, 2));
        // Barrier: guarantee the loop has processed the v2 didChange before release.
        hover_sync(&h, 100, "file:///g.rb");
        // Release v1: Computed{v1} arrives, current==v2 ⇒ stale ⇒ dropped +
        // re-dispatched ⇒ v2 worker ⇒ publishes the CLEAN final content.
        g.release(1);
        let d = h.recv_diags();
        assert!(
            d.diagnostics.is_empty(),
            "the FINAL (v2, clean) content is published; the stale v1 was dropped: {:?}",
            d.diagnostics
        );
        // Exactly once: no further publish (the debounce was cancelled when the v2
        // worker was spawned). A hover round-trips as the very next message.
        hover_sync(&h, 101, "file:///g.rb");
        h.shutdown();
    }

    #[test]
    fn integration_s3_burst_edits_coalesce_to_final_no_stale_publish() {
        // Concurrency stress: many rapid edits while ONE worker is in flight. The
        // one-in-flight gate means v2..v5 never spawn a worker; only the LAST
        // version is re-dispatched after the stale v1 drop → exactly one publish of
        // the final content, and NO intermediate/stale version ever publishes.
        let g = gate_holding(&[1], &[]);
        let mut h = Harness::start_with_gate(Duration::from_secs(30), g.gate.clone());
        h.notify("textDocument/didOpen", open_params("file:///g.rb", TYPO, 1)); // v1 held
        // Burst: v2..v5 TYPO (would each be 1 diag), v6 CLEAN (the final content).
        for v in 2..=5 {
            h.notify("textDocument/didChange", change_params("file:///g.rb", TYPO, v));
        }
        h.notify("textDocument/didChange", change_params("file:///g.rb", CLEAN, 6));
        hover_sync(&h, 100, "file:///g.rb"); // all edits processed; buffer == v6.
        g.release(1); // v1 stale ⇒ dropped ⇒ re-dispatch v6 ⇒ publish CLEAN.
        let d = h.recv_diags();
        assert!(
            d.diagnostics.is_empty(),
            "only the final v6 (clean) content publishes; no intermediate/stale version escaped: {:?}",
            d.diagnostics
        );
        hover_sync(&h, 101, "file:///g.rb"); // exactly one publish.
        h.shutdown();
    }

    #[test]
    fn integration_s3_worker_panic_does_not_stick_the_uri() {
        // A panicking worker must not strand its URI in flight. v1's worker panics
        // in the gate → caught by the worker's `catch_unwind` → an empty Computed is
        // still sent → in-flight clears → v1 (current) publishes empty. A LATER edit
        // is then analysed + published normally, proving the URI is not stuck.
        let g = gate_holding(&[], &[1]);
        let mut h = Harness::start_with_gate(Duration::from_millis(10), g.gate.clone());
        h.notify("textDocument/didOpen", open_params("file:///g.rb", TYPO, 1));
        // The panicked v1 worker yields a caught (empty) result — not a hang.
        let d = h.recv_diags();
        assert!(
            d.diagnostics.is_empty(),
            "a panicked worker yields a caught empty result, not a stuck URI: {:?}",
            d.diagnostics
        );
        // Not stuck: a subsequent edit (v2, a typo) is dispatched (debounced 10 ms)
        // and published like normal.
        h.notify("textDocument/didChange", change_params("file:///g.rb", TYPO, 2));
        let d2 = h.recv_diags();
        assert_eq!(
            d2.diagnostics.len(),
            1,
            "a later edit is still analysed and published — the URI was not stuck"
        );
        h.shutdown();
    }

    #[test]
    fn integration_s3_shutdown_with_worker_in_flight_does_not_hang() {
        // Shutdown must not wait on a detached rayon worker. Hold a worker
        // mid-flight, then shut down: the loop returns promptly (the join is on the
        // LOOP thread, not the rayon worker); the results channel drops, so the
        // worker's eventual send is a no-op. Release the worker AFTER shutdown so it
        // is not leaked blocked on a rayon pool thread.
        let g = gate_holding(&[1], &[]);
        let mut h = Harness::start_with_gate(Duration::from_secs(30), g.gate.clone());
        h.notify("textDocument/didOpen", open_params("file:///g.rb", TYPO, 1)); // v1 held
        hover_sync(&h, 100, "file:///g.rb"); // the worker is spawned + in flight.
        h.shutdown(); // must return without waiting for the held worker.
        g.release(1); // detached worker proceeds; its send finds the rx gone (no-op).
    }

    // ---------------------------------------------------------------------
    // S4: tier-1 ProjectContext generation + watched-files/config invalidation
    // + dynamic registration + the close+reopen open-epoch nit. All driven
    // DETERMINISTICALLY via the worker-gate seam + hover FIFO barriers — no
    // wall-clock races (30 s debounce where a stray timer would interfere).
    // ---------------------------------------------------------------------

    /// A `workspace/didChangeWatchedFiles` payload naming one changed `uri`.
    fn watched_change(uri: &str) -> serde_json::Value {
        serde_json::json!({ "changes": [ { "uri": uri, "type": 2 } ] })
    }

    /// A recording gate that holds every GENERATION-0 worker (until released) and
    /// records `(version, generation)` for every worker it gates. Keying the hold on
    /// generation (not version) lets the re-dispatched new-generation worker run
    /// freely, while the recording proves whether a fresh worker ran under the new
    /// generation — the observable signature of a generation stale-drop.
    struct GenGate {
        release_gen0: crossbeam_channel::Sender<()>,
        calls: Arc<std::sync::Mutex<Vec<(i32, u64)>>>,
        gate: Arc<WorkerGate>,
    }

    fn gate_recording_hold_gen0() -> GenGate {
        let (tx, rx) = crossbeam_channel::unbounded::<()>();
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_w = Arc::clone(&calls);
        let gate: Arc<WorkerGate> = Arc::new(move |version: i32, generation: u64| {
            calls_w.lock().unwrap().push((version, generation));
            if generation == 0 {
                let _ = rx.recv(); // block gen-0 workers until released.
            }
        });
        GenGate { release_gen0: tx, calls, gate }
    }

    #[test]
    fn integration_s4_generation_stale_drop_after_invalidate() {
        // A worker in flight when an invalidation bumps the generation has its
        // result DROPPED; a fresh dispatch under the new generation publishes. The
        // gen-0 worker is held; a relevant watched-files change bumps the generation
        // to 1; on release the gen-0 result is generation-stale → dropped +
        // re-dispatched → a gen-1 worker publishes.
        let g = gate_recording_hold_gen0();
        let mut h = Harness::start_with_gate(Duration::from_secs(30), g.gate.clone());
        // didOpen v1 (CLEAN) → worker (v1, gen0) spawns and blocks in the gate.
        h.notify("textDocument/didOpen", open_params("file:///g.rb", CLEAN, 1));
        hover_sync(&h, 100, "file:///g.rb"); // barrier: the gen0 worker is in flight.
        // Invalidate via a relevant watched-files change → generation → 1; re-analyse
        // open buffers (the URI is in flight → no-op; the eventual gen-drop covers it).
        h.notify("workspace/didChangeWatchedFiles", watched_change("file:///proj/.rigor.yml"));
        hover_sync(&h, 101, "file:///g.rb"); // barrier: the invalidation is processed.
        // Release the gen0 worker → its result is generation-stale (gen0 != gen1) →
        // DROPPED + re-dispatched → a fresh (v1, gen1) worker publishes the clean set.
        g.release_gen0.send(()).unwrap();
        let d = h.recv_diags();
        assert!(
            d.diagnostics.is_empty(),
            "the fresh gen-1 result publishes (clean): {:?}",
            d.diagnostics
        );
        hover_sync(&h, 102, "file:///g.rb"); // exactly one publish.
        // Proof of the generation drop: a worker ran under generation 1 (the
        // re-dispatch). Without the generation guard the gen-0 result would have
        // published directly and NO gen-1 worker would ever have run.
        let calls = g.calls.lock().unwrap().clone();
        assert!(
            calls.iter().any(|&(_, genr)| genr == 0),
            "the initial worker ran under generation 0: {calls:?}"
        );
        assert!(
            calls.iter().any(|&(_, genr)| genr == 1),
            "a re-dispatched worker ran under the new generation (proves the stale \
             gen-0 result was dropped, not published): {calls:?}"
        );
        h.shutdown();
    }

    #[test]
    fn integration_s4_watched_files_relevant_reanalyzes_all_open_buffers() {
        // A relevant `didChangeWatchedFiles` (`.rigor.yml`) invalidates + re-analyses
        // ALL open buffers — both `a.rb` and `b.rb` re-publish. 30 s debounce so only
        // the invalidation (never a timer) drives the re-publishes.
        let mut h = Harness::start_with_debounce(Duration::from_secs(30));
        h.notify("textDocument/didOpen", open_params("file:///a.rb", CLEAN, 1));
        assert!(h.recv_diags().diagnostics.is_empty());
        h.notify("textDocument/didOpen", open_params("file:///b.rb", CLEAN, 1));
        assert!(h.recv_diags().diagnostics.is_empty());
        h.notify(
            "workspace/didChangeWatchedFiles",
            watched_change("file:///proj/.rigor.yml"),
        );
        // Both buffers re-publish (worker order is nondeterministic; collect a set).
        let mut seen = std::collections::HashSet::new();
        seen.insert(h.recv_diags().uri.as_str().to_string());
        seen.insert(h.recv_diags().uri.as_str().to_string());
        assert!(
            seen.contains("file:///a.rb") && seen.contains("file:///b.rb"),
            "both open buffers re-analysed after invalidate: {seen:?}"
        );
        h.shutdown();
    }

    #[test]
    fn integration_s4_watched_files_unrelated_does_not_invalidate() {
        // An unrelated watched path (a `.txt`) does NOT invalidate → no re-analysis.
        let mut h = Harness::start_with_debounce(Duration::from_secs(30));
        h.notify("textDocument/didOpen", open_params("file:///a.rb", CLEAN, 1));
        assert!(h.recv_diags().diagnostics.is_empty());
        h.notify(
            "workspace/didChangeWatchedFiles",
            watched_change("file:///proj/notes.txt"),
        );
        // No re-publish: a hover round-trips as the very next message (a publish here
        // would mean the unrelated change wrongly invalidated).
        hover_sync(&h, 100, "file:///a.rb");
        h.shutdown();
    }

    #[test]
    fn integration_s4_did_change_configuration_reanalyzes() {
        // `didChangeConfiguration` always invalidates + re-analyses open buffers.
        let mut h = Harness::start_with_debounce(Duration::from_secs(30));
        h.notify("textDocument/didOpen", open_params("file:///a.rb", CLEAN, 1));
        assert!(h.recv_diags().diagnostics.is_empty());
        h.notify(
            "workspace/didChangeConfiguration",
            serde_json::json!({ "settings": {} }),
        );
        let d = h.recv_diags();
        assert_eq!(d.uri.as_str(), "file:///a.rb", "the open buffer re-analysed");
        assert!(d.diagnostics.is_empty());
        h.shutdown();
    }

    #[test]
    fn integration_s4_buffer_didchange_never_invalidates() {
        // A buffer `didChange` NEVER invalidates: only the EDITED buffer re-publishes;
        // an untouched second open buffer is NOT re-analysed (an invalidate would
        // re-publish BOTH — see `..._reanalyzes_all_open_buffers`).
        let mut h = Harness::start_with_debounce(Duration::from_millis(10));
        h.notify("textDocument/didOpen", open_params("file:///a.rb", CLEAN, 1));
        assert!(h.recv_diags().diagnostics.is_empty());
        h.notify("textDocument/didOpen", open_params("file:///b.rb", CLEAN, 1));
        assert!(h.recv_diags().diagnostics.is_empty());
        // Edit ONLY a.rb → its debounced publish carries the typo; b.rb stays quiet.
        h.notify("textDocument/didChange", change_params("file:///a.rb", TYPO, 2));
        let d = h.recv_diags();
        assert_eq!(d.uri.as_str(), "file:///a.rb", "only the edited buffer republishes");
        assert_eq!(d.diagnostics.len(), 1);
        // Prove b.rb did NOT re-publish (no invalidation): a hover on b.rb round-trips
        // as the next message.
        hover_sync(&h, 100, "file:///b.rb");
        h.shutdown();
    }

    #[test]
    fn integration_s4_dynamic_registration_sent_when_advertised() {
        // Client advertises `didChangeWatchedFiles.dynamicRegistration` → the server
        // sends a `client/registerCapability` request after `initialized`.
        let caps = serde_json::json!({
            "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } }
        });
        let mut h = Harness::start_full(DEBOUNCE_DEFAULT, production_gate(), caps);
        match h.recv() {
            Message::Request(r) => {
                assert_eq!(r.method, "client/registerCapability");
                // Reply so the request isn't left outstanding (the server ignores it).
                h.client
                    .sender
                    .send(Message::Response(Response::new_ok(r.id, serde_json::Value::Null)))
                    .unwrap();
            }
            other => panic!("expected client/registerCapability, got {other:?}"),
        }
        h.shutdown();
    }

    #[test]
    fn integration_s4_no_registration_when_not_advertised_but_watched_files_still_honored() {
        // Client does NOT advertise dynamic registration → NO `client/registerCapability`
        // is sent (the first server message is the didOpen publish, not a request);
        // yet a subsequently-received `didChangeWatchedFiles` is STILL honoured (the
        // static-registration degrade path — no regression).
        let mut h = Harness::start_full(DEBOUNCE_DEFAULT, production_gate(), serde_json::json!({}));
        h.notify("textDocument/didOpen", open_params("file:///a.rb", CLEAN, 1));
        // `recv_diags` panics on a Request, so this asserts no registration preceded it.
        let d = h.recv_diags();
        assert_eq!(d.uri.as_str(), "file:///a.rb");
        h.notify(
            "workspace/didChangeWatchedFiles",
            watched_change("file:///proj/.rigor.yml"),
        );
        let d2 = h.recv_diags();
        assert_eq!(
            d2.uri.as_str(),
            "file:///a.rb",
            "didChangeWatchedFiles is honoured even without dynamic registration"
        );
        h.shutdown();
    }

    #[test]
    fn integration_s4_close_reopen_version_reuse_drops_stale_preclose_worker() {
        // The S3 reopen-identity nit. A pre-close worker is held in flight; then
        // didClose; then didOpen REUSING version 1 (VS Code resends version 1 on
        // reopen) with DIFFERENT (clean) content. Version matches and the generation
        // is unchanged (project-scoped — a reopen never bumps it), so ONLY the
        // open-epoch closes this: the pre-close worker (a TYPO) is epoch-dropped and
        // the reopened CLEAN content is analysed fresh.
        let g = gate_holding(&[1], &[]);
        let mut h = Harness::start_with_gate(Duration::from_secs(30), g.gate.clone());
        // v1 = TYPO, worker held in flight (open-epoch 1).
        h.notify("textDocument/didOpen", open_params("file:///g.rb", TYPO, 1));
        hover_sync(&h, 100, "file:///g.rb"); // the pre-close worker is in flight.
        // Close (open-epoch → 2) — clears markers with an empty publish.
        h.notify(
            "textDocument/didClose",
            serde_json::json!({ "textDocument": { "uri": "file:///g.rb" } }),
        );
        assert!(h.recv_diags().diagnostics.is_empty(), "didClose clears markers");
        // Reopen REUSING version 1 with CLEAN content (open-epoch → 3). The reopen's
        // dispatch no-ops (the pre-close worker is still in flight); its content is
        // picked up by the epoch-drop re-dispatch.
        h.notify("textDocument/didOpen", open_params("file:///g.rb", CLEAN, 1));
        hover_sync(&h, 101, "file:///g.rb"); // the reopen is processed.
        // Two release tokens (same version 1): the first unblocks the pre-close
        // worker; the second is buffered for the epoch-drop re-dispatch (also v1).
        g.release(1);
        g.release(1);
        // The pre-close worker returns: version matches (1) and generation matches,
        // but its EPOCH (1) != current (3) → DROPPED + re-dispatched → the reopened
        // CLEAN content publishes. Under S3 (no epoch guard) the stale TYPO would
        // have published (1 diagnostic) — this empty publish proves the epoch drop.
        let d = h.recv_diags();
        assert!(
            d.diagnostics.is_empty(),
            "the stale pre-close (TYPO) worker was epoch-dropped; the reopened CLEAN \
             content publishes: {:?}",
            d.diagnostics
        );
        hover_sync(&h, 102, "file:///g.rb"); // exactly one publish.
        h.shutdown();
    }

    #[test]
    fn watched_file_relevance_matches_the_config_and_signature_surface() {
        // Relevant: `.rigor.yml`, `Gemfile.lock`, project `*.rb`, `sig/**/*.rbs`.
        assert!(watched_file_is_relevant("file:///p/.rigor.yml"));
        assert!(watched_file_is_relevant("file:///p/Gemfile.lock"));
        assert!(watched_file_is_relevant("file:///p/app/models/user.rb"));
        assert!(watched_file_is_relevant("file:///p/sig/user.rbs"));
        assert!(watched_file_is_relevant("file:///p/sig/models/user.rbs"));
        // Not relevant: an `.rbs` outside a `sig/` dir, and unrelated files.
        assert!(!watched_file_is_relevant("file:///p/vendor/other.rbs"));
        assert!(!watched_file_is_relevant("file:///p/notes.txt"));
        assert!(!watched_file_is_relevant("file:///p/README.md"));
        // A payload with any relevant change is relevant; an all-unrelated one is not.
        assert!(watched_files_params_are_relevant(&watched_change("file:///p/.rigor.yml")));
        assert!(!watched_files_params_are_relevant(&watched_change("file:///p/notes.txt")));
        assert!(!watched_files_params_are_relevant(&serde_json::json!({})));
    }
}
