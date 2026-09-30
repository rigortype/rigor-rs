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
//! buffer state is always eventually published. S4 added the generation counter +
//! watched-files/configuration invalidation, and **S4b the cross-file overlay**:
//! tier 1 holds every project file's `LoweredAst`, and a diagnostics dispatch
//! rebuilds the project `SourceIndex` with the dirty buffer's file REPLACED by the
//! buffer's own AST, so the editor sees the same cross-file facts `check` does —
//! behind a measured scale guard that falls back to the single-file index (with a
//! `window/showMessage` disclosure) on projects too large to rebuild per dispatch.
//! Hover / completion / documentSymbol stay on the single-file index in v1.

use std::collections::{HashMap, HashSet};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rayon::prelude::*;

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
use rigor_infer::{Harvest, SourceIndex, Typer};
use rigor_parse::{comment_lines, lower, lower_with_key, parse, FileKey, LoweredAst, Node};
use rigor_rules::{analyze_with_source_and_folder, filter_suppressed, Severity, SuppressSet};
use rigor_types::{Interner, Type, TypeId};

use crate::config::Config;
use crate::ruby_mode;
use crate::severity;
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

    // S4b's deferred N4, closed: the client's `workspaceFolders` / `rootUri` /
    // `rootPath` now decide the project root. An editor that opens folder X but
    // spawns the server elsewhere used to discover ZERO project files and — since
    // an empty project deliberately does not trip the scale guard — disclose
    // nothing at all.
    //
    // The root is adopted as the process CWD (see [`enter_project_root`] for why
    // that, and not a threaded absolute root): rigor's project root IS a cwd in
    // every consumer below, so this puts the server in exactly the state
    // `cd <root> && rigor check` runs in and leaves `root` — and therefore the
    // `exclude:` spellings, the `paths:` walk and the config path — byte-identical
    // to what they were. This MUST stay above `read_project_config`: everything
    // from here down reads the filesystem relative to the cwd.
    for (typ, msg) in enter_project_root(&init_params) {
        send_show_message(&connection, typ, msg)?;
    }
    let root = PathBuf::from(".");

    // Two-tier essence: the RBS environment is built ONCE and reused for the whole
    // session (the per-keystroke path never pays the RBS-load floor). The CONFIG is
    // read here and re-read by every structural [`invalidate`] — through the SAME
    // [`read_project_config`] call, so startup and reload can never disagree about
    // which file is the project config or how a broken one is handled.
    //
    // `root.join(".rigor.yml")` is `./.rigor.yml` in production — byte-identical to
    // the `Config::load(None)` cwd discovery this replaces; the join is what lets a
    // test drive a real config file under an injected root.
    //
    // A config broken AT STARTUP has no "last good" to fall back on, so it takes
    // the same defaults `check` would and discloses — but it still records
    // `config_broken`, so fixing the file publishes the recovery notice rather
    // than landing silently.
    let config_read = read_project_config(&root);
    let config_broken = config_read.is_err();
    if let Err(reason) = &config_read {
        send_show_message(
            &connection,
            MessageType::WARNING,
            config_broken_at_startup_message(reason),
        )?;
    }
    let cfg = config_read.unwrap_or_default();

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

    // The tier-1 project context: RBS index + suppression set + shared sidecar +
    // the S4b cross-file overlay substrate, stamped with generation 0. Loop-owned
    // (swapped on `invalidate`), so it is built here and MOVED into `main_loop`
    // rather than held in the immutable `ServerContext`.
    let index = Arc::new(build_core_index(&root, &cfg));
    let build = build_overlay(&root, &cfg, &index);
    // The startup build is the guard's FIRST sample. With hysteresis it can never
    // disable the overlay on its own (that needs `OVERLAY_GUARD_STRIKES`
    // consecutive over-budget samples), so a session always starts cross-file and
    // only steps down if the cost is confirmed by the next real dispatch.
    let mut guard = OverlayGuard::new();
    if build.file_count > 0 {
        guard.record(build.merge, OVERLAY_BUILD_BUDGET_DEFAULT);
    }
    report_overlay_timing(&build, guard.enabled);
    let overlay = (guard.enabled && build.file_count > 0).then_some(build.files);
    let project = Arc::new(ProjectContext {
        generation: 0,
        index,
        disable: cfg.disable_matcher(),
        folder,
        stamp: SeverityStamp::from_config(&cfg),
        exclude: ExcludeMatcher::from_config(&root, &cfg),
        overlay,
    });

    let ctx = ServerContext {
        debounce: DEBOUNCE_DEFAULT,
        worker_gate: production_gate(),
        watched_files_dynamic_registration,
        project_root: root,
        overlay_budget: OVERLAY_BUILD_BUDGET_DEFAULT,
    };

    // Pre-warm the rayon global pool at startup (ADR-0029 "pre-warmed worker
    // pool"): the pool spawns its worker threads lazily on first use, so touch it
    // once here to avoid paying that init on the first keystroke's dispatch. The
    // pool size honours `RAYON_NUM_THREADS` natively (the existing knob); no LSP
    // `--workers` flag is added.
    rayon::spawn(|| {});

    main_loop(&connection, &ctx, project, cfg, guard, config_broken)?;

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

/// The workspace root a client named at `initialize`, read from the params.
///
/// PURE — no filesystem, no connection — so the precedence rule is unit-testable
/// on its own. [`enter_project_root`] is the half that touches the world.
#[derive(Default, PartialEq, Eq, Debug)]
struct RootRequest {
    /// The directory to adopt as the project root, or `None` when the client
    /// named nothing usable (then the server's cwd stands, as it always has).
    path: Option<PathBuf>,
    /// The RAW spelling of the highest-priority field that named ANYTHING, used
    /// only for the disclosure when it could not be turned into a path. `Some`
    /// with `path: None` is the "the editor opened a workspace rigor cannot
    /// analyse" case (a `vscode-vfs:` / `untitled:` root); `None` with
    /// `path: None` is "the client named no root at all", which is silent.
    named: Option<String>,
    /// The DISPLAY names of the workspace folders that were NOT chosen. Non-empty
    /// only in a multi-root workspace, and the sole trigger of the multi-root
    /// disclosure — rigor's config model is single-root, so the others are simply
    /// not analysed and the user is told so rather than left to wonder.
    ignored_folders: Vec<String>,
}

/// Read `initialize`'s three root fields in the protocol's own modernity order
/// and return the first that names a local directory.
///
/// LSP offers three, of decreasing currency:
///
/// | field | status | shape |
/// | --- | --- | --- |
/// | `workspaceFolders` | current (3.6+) | `[{uri, name}] \| null` |
/// | `rootUri` | deprecated in favour of `workspaceFolders` | `DocumentUri \| null` |
/// | `rootPath` | deprecated in favour of `rootUri` (1.x legacy) | plain path `\| null` |
///
/// so the precedence is exactly that order, and the rule is "the first source
/// that yields a filesystem path wins" — one sentence, rather than a matrix of
/// per-field validity. A `workspaceFolders` entry with a non-`file:` scheme
/// therefore falls through to `rootUri` instead of aborting, which costs nothing
/// and is the forgiving direction.
///
/// **Multiple folders: the FIRST wins, and the rest are disclosed.** rigor's
/// configuration model is single-root — one `.rigor.yml`, one `paths:`, one
/// `sig/` — so there is no honest way to serve N folders from one session, and
/// the three candidate answers were: pick one silently, refuse to start, or pick
/// one and say so. Refusing makes the server useless in a VS Code multi-root
/// workspace that merely happens to contain a Ruby project; picking silently is
/// the exact failure this whole slice exists to remove (the user sees diagnostics
/// that do not match `rigor check` and cannot tell why). Picking the first and
/// DISCLOSING is the ADR-0036 posture precedent this server already follows for
/// the sidecar and the overlay scale guard: rigor never silently degrades.
/// "First" rather than "the one with a `.rigor.yml`" because a content heuristic
/// would let *adding a config file to another folder* silently move the root.
fn requested_root(init_params: &serde_json::Value) -> RootRequest {
    let mut out = RootRequest::default();

    // `workspaceFolders` (current). Absent or `null` ⇒ fall through; `[]` is
    // treated the same as `null` (VS Code sends `null` for "no folder open", but
    // an empty array means the same thing and must not be read as a root).
    let folders: Vec<&serde_json::Value> = init_params
        .get("workspaceFolders")
        .and_then(serde_json::Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    if let Some(first) = folders.first() {
        let uri = first.get("uri").and_then(serde_json::Value::as_str);
        out.named = uri.map(str::to_string);
        out.path = uri.and_then(file_uri_to_path);
        // Recorded whatever the first folder resolved to: a multi-root workspace
        // is a degradation even when the chosen folder is perfectly usable.
        out.ignored_folders = folders[1..]
            .iter()
            .map(|f| {
                f.get("name")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| f.get("uri").and_then(serde_json::Value::as_str))
                    .unwrap_or("<unnamed>")
                    .to_string()
            })
            .collect();
        if out.path.is_some() {
            return out;
        }
    }

    // `rootUri` (deprecated, still what most clients send).
    if let Some(uri) = init_params.get("rootUri").and_then(serde_json::Value::as_str) {
        out.named.get_or_insert_with(|| uri.to_string());
        out.path = file_uri_to_path(uri);
        if out.path.is_some() {
            return out;
        }
    }

    // `rootPath` (legacy) — a plain filesystem path, NOT a URI, so it is taken
    // verbatim. An empty string is not a path.
    if let Some(p) = init_params
        .get("rootPath")
        .and_then(serde_json::Value::as_str)
        .filter(|p| !p.is_empty())
    {
        out.named.get_or_insert_with(|| p.to_string());
        out.path = Some(PathBuf::from(p));
    }
    out
}

/// Adopt the client's workspace root as the server's CURRENT DIRECTORY, and
/// return every `window/showMessage` the choice owes the user.
///
/// **Why a `chdir` and not a threaded root.** rigor's project root is a process
/// cwd *everywhere* — `Config::load` discovers `.rigor.yml` there, `paths:` and
/// `exclude:` are matched against the path strings `check` builds from it,
/// `signature_paths:` (`sig/`) resolve against it, `Gemfile.lock` and
/// `rbs_collection.lock.yaml` are looked up under it. The LSP already threads a
/// `project_root` for its own discovery, but that seam covers only two of those
/// five: `Config::signature_dirs` is cwd-relative by construction, and — the
/// decisive one — [`join_root`] deliberately spells discovery paths RELATIVELY
/// so the `exclude:` globs match the same strings `check` matches. Handing that
/// seam an absolute root would silently stop every relative `exclude:` pattern
/// from matching, i.e. re-open the presence divergence
/// `docs/notes/20260725-lsp-exclude-parity.md` closed (and which took three
/// review-caught regressions to close correctly).
///
/// So the root is moved where the model already says it lives. The server ends up
/// in exactly the state `cd <root> && rigor check` runs in, which makes the parity
/// bar hold **by construction** rather than by argument, and `project_root` stays
/// `.` — every consumer below is byte-identically the code that ran before.
///
/// Safe here in a way a `chdir` usually is not: `rigor lsp` is a dedicated
/// single-session process, this runs ONCE on the main thread immediately after the
/// handshake, and nothing has touched the filesystem yet (`Connection::stdio`'s IO
/// threads only own stdin/stdout, `--log` is accepted-but-unwired so no path is
/// captured, and the sidecar, the `CoreIndex` and the overlay are all built after
/// this point). Buffer URIs are absolute, so no request path depends on the cwd.
///
/// A root that cannot be entered — deleted, a file, unreadable — does NOT abort:
/// the server keeps its cwd and discloses, because a language server that refuses
/// to start is strictly worse for the user than one that says what it is analysing.
fn enter_project_root(init_params: &serde_json::Value) -> Vec<(MessageType, String)> {
    let req = requested_root(init_params);
    let mut disclosures = Vec::new();
    match &req.path {
        Some(path) => match std::env::set_current_dir(path) {
            Ok(()) => {
                if !req.ignored_folders.is_empty() {
                    disclosures.push((
                        MessageType::WARNING,
                        multi_root_message(path, &req.ignored_folders),
                    ));
                }
            }
            Err(e) => disclosures.push((
                MessageType::WARNING,
                unusable_root_message(&path.display().to_string(), &e.to_string()),
            )),
        },
        // Named something that is not a local directory at all (a virtual
        // workspace). Nothing named ⇒ nothing to say: the cwd root is this
        // server's documented default, not a degradation.
        None => {
            if let Some(named) = &req.named {
                disclosures.push((
                    MessageType::WARNING,
                    unusable_root_message(named, "not a local file: directory"),
                ));
            }
        }
    }
    disclosures
}

/// The multi-root disclosure. Names the folder that WON (so the user can tell at
/// a glance whether it is the one they care about) and the ones dropped, and
/// points at the two real remedies rather than just stating the limitation.
fn multi_root_message(chosen: &Path, ignored: &[String]) -> String {
    format!(
        "rigor: this workspace has {} folders but rigor's project model is single-root \
         (one .rigor.yml, one paths:, one sig/) — analysing {} only, ignoring {}. \
         Open the folder you want in its own window, or widen paths: in its .rigor.yml.",
        ignored.len() + 1,
        chosen.display(),
        ignored.join(", "),
    )
}

/// The unusable-root disclosure: the editor believes it opened a workspace and
/// rigor is analysing something else, which the user must be told or the
/// mismatched diagnostics are inexplicable.
fn unusable_root_message(named: &str, reason: &str) -> String {
    format!(
        "rigor: the workspace root {named} could not be used ({reason}) — analysing the \
         server's working directory instead; diagnostics may not match `rigor check` for \
         your project."
    )
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
    /// Behind an `Arc` so the context can be re-stamped CHEAPLY: the scale guard
    /// (and the per-file re-harvest) swap in a new `ProjectContext` carrying a
    /// different overlay while REUSING this index — a `CoreIndex` rebuild costs
    /// ~100-300 ms and must not be paid to change an unrelated field.
    index: Arc<CoreIndex>,
    disable: SuppressSet,
    /// The ADR-0008 real-Ruby folder for full-fidelity constant folds, when a
    /// sidecar was reachable at startup. `None` = sound subset. Behind an `Arc` so
    /// it is PRESERVED (not respawned) across `invalidate` rebuilds — a rebuild
    /// clones this `Arc` into the new context. Shared across the concurrent LSP
    /// workers as `&(dyn RubyFolder + Sync)` exactly as the `check` pipeline does
    /// (`sidecar.rs`); the folder's internal `Mutex` serializes folds across the
    /// workers (contention accepted, measure later per ADR-0029).
    folder: Option<Arc<sidecar::SidecarFolder>>,
    /// The ADR-8 [`SeverityStamp`] inputs — `check`'s stage-3 tail. Config-derived
    /// exactly like [`Self::disable`], so it sits beside it and is rebuilt for free
    /// by [`invalidate`] / [`swap_project`].
    stamp: SeverityStamp,
    /// The config `exclude:` gate for the OPEN BUFFER — `check`'s STAGE-1 file
    /// filter. Config-derived like [`Self::disable`] and [`Self::stamp`], so it
    /// sits beside them and rides the same `invalidate` / `swap_project` rebuild
    /// and the same S4 generation guard.
    exclude: ExcludeMatcher,
    /// The **cross-file overlay substrate** (S4b): every project `.rb` file as a
    /// [`HeldFile`] — canonical path, `LoweredAst`, and `Harvest`, all produced at
    /// build time. A diagnostics dispatch merges these into the project
    /// `SourceIndex` with the dirty buffer's file's PAIR replaced by the buffer's
    /// freshly-lowered-and-harvested one ([`overlay_source_index`]), so LSP
    /// diagnostics see the same cross-file context `check` does. `None` ⇒ the
    /// overlay is OFF (the scale guard tripped, or no project files were found)
    /// and diagnostics fall back to today's single-file [`SourceIndex::build`].
    overlay: Option<ProjectFiles>,
}

/// The ADR-8 SeverityStamp inputs (reference `severity_stamp.rb`), carried on the
/// tier-1 [`ProjectContext`] so the LSP's post-analysis tail is `check`'s stage-3
/// tail. Every field is config-derived — the same provenance as
/// [`ProjectContext::disable`] — so a config/watched-file `invalidate` rebuilds
/// them for free.
///
/// Grouped into one struct rather than four `ProjectContext` fields because they
/// are one decision with one build site; `check` computes exactly these once per
/// run in `analyze_files` and threads them through stage 3.
///
/// The bleeding-edge SELECTOR itself is deliberately NOT stored: `check` uses it
/// for exactly two things, and both are already reduced here — the merged
/// override map ([`Self::bleeding_overrides`]) and the void-rule activation gate
/// ([`Self::void_rule_active`], which `check` derives with the same
/// `severity::resolve` call, not from the selector directly). A stored selector
/// would be a dead field.
struct SeverityStamp {
    /// `severity_profile:` — the ADR-8 profile table consulted below the overrides.
    profile: severity::Profile,
    /// `severity_overrides:` — the user's per-rule / per-FAMILY overrides.
    user_overrides: Vec<(String, severity::ResolvedSeverity)>,
    /// The merged overrides of the ACTIVE bleeding-edge features
    /// (`bleeding_edge::severity_overrides_for`), composed below the user's.
    bleeding_overrides: Vec<(&'static str, severity::ResolvedSeverity)>,
    /// The reference's memoised rule-activation gate: `static.value-use.void` runs
    /// only when its RESOLVED severity is not `:off` (authored `:warning`, every
    /// shipped profile `:off`, promoted by the `use-of-void-value` feature OR by a
    /// user `severity_overrides:` entry). Byte-identical to `check`'s
    /// `void_rule_active`.
    void_rule_active: bool,
}

impl SeverityStamp {
    /// Compute the stamp inputs from the session config — the LSP's counterpart of
    /// the block at the top of `main.rs`'s `analyze_files`.
    ///
    /// The bleeding-edge selection comes from `bleeding_edge:` in `.rigor.yml`
    /// only: unlike `check`, `rigor lsp` accepts no `--bleeding-edge` flag (an
    /// editor launches the server, so a per-invocation flag has no user).
    fn from_config(cfg: &Config) -> Self {
        let selector = cfg.bleeding_edge_selector();
        let bleeding_overrides = crate::bleeding_edge::severity_overrides_for(&selector);
        let profile = cfg.severity_profile();
        let user_overrides = cfg.severity_overrides();
        let void_rule_active = severity::resolve(
            rigor_rules::STATIC_VALUE_USE_VOID,
            severity::ResolvedSeverity::Warning,
            profile,
            &user_overrides,
            &bleeding_overrides,
        ) != severity::ResolvedSeverity::Off;
        Self { profile, user_overrides, bleeding_overrides, void_rule_active }
    }

    /// Re-stamp `diag`'s severity from the profile + overrides, returning `false`
    /// when the resolution is `:off` — i.e. when `check` would DROP the diagnostic
    /// (`severity::ResolvedSeverity::Off => continue`). That drop is why the
    /// pre-stamp LSP diverged on PRESENCE, not merely on severity.
    ///
    /// The `internal-error` sentinel BYPASSES the stamp entirely (the reference's
    /// `rule.nil?` short-circuit): a per-file panic must never be silenced by
    /// configuration.
    fn apply(&self, diag: &mut rigor_rules::Diagnostic) -> bool {
        if diag.rule_id == "internal-error" {
            return true;
        }
        let current = match diag.severity {
            Severity::Error => severity::ResolvedSeverity::Error,
            Severity::Warning => severity::ResolvedSeverity::Warning,
            Severity::Info => severity::ResolvedSeverity::Info,
        };
        match severity::resolve(
            diag.rule_id,
            current,
            self.profile,
            &self.user_overrides,
            &self.bleeding_overrides,
        ) {
            severity::ResolvedSeverity::Off => return false,
            severity::ResolvedSeverity::Error => diag.severity = Severity::Error,
            severity::ResolvedSeverity::Warning => diag.severity = Severity::Warning,
            severity::ResolvedSeverity::Info => diag.severity = Severity::Info,
        }
        true
    }
}

/// The `exclude:` gate for the OPEN BUFFER — the LSP's counterpart of
/// `check`'s EXPANSION-TIME filter (`main.rs`: `expand_check_paths_excluding`
/// applies `BUILTIN_EXCLUDES + exclude:` with `File.fnmatch?`-no-flags to
/// every directory-expanded path, before the file is even read; an explicit
/// `.rb` root is never filtered).
///
/// Carried on [`ProjectContext`] beside [`ProjectContext::disable`] and
/// [`ProjectContext::stamp`] for the same reason those live there: every field is
/// config-derived, so `invalidate` / `swap_project` rebuild it for free and the S4
/// generation guard covers it with no new concurrency reasoning. Nothing re-reads
/// `.rigor.yml` per dispatch.
///
/// **THE INVARIANT** (PR #45 review): a buffer is excluded **iff EVERY discovery
/// spelling of that file is excluded**. One file can reach discovery under several
/// names — a symlinked `.rb` is walked under the LINK's name while its content
/// lives elsewhere (`collect_rb_files` includes symlinked files on purpose,
/// `main.rs`, matching `Dir.glob`), and overlapping `paths:` roots
/// (`[".", "lib"]`) walk the same file twice — and `check` analyses the file if ANY
/// of those names survives `exclude:`. A gate that re-derives one canonical
/// spelling cannot express that, and the first implementation of this slice
/// silently dropped three such shapes that `check` analyses.
///
/// So the gate is answered in two tiers:
///
/// 1. **Discovery MEMBERSHIP — the primary, exact signal.** [`ProjectFiles`]
///    already holds the canonical path of every file in the POST-`exclude:`
///    discovery set, i.e. exactly the files `check` analyses. Buffer present ⇒ some
///    spelling survived ⇒ **not excluded**. This satisfies the invariant by
///    construction, with no spelling arithmetic at all.
/// 2. **Spelling fallback — only when the overlay cannot answer**: the scale guard
///    tripped (`overlay: None`), the project is empty, the buffer is new/unsaved, or
///    it lives outside `paths:`. Then every candidate spelling of the buffer is
///    enumerated ([`Self::check_spellings`]) and the buffer is excluded only if they
///    are ALL excluded — the invariant again, this time computed — and it knows
///    two names `check` never matches `exclude:` against: a `paths:` entry that
///    names the `.rb` file itself and the explicit `rigor check <file>` spelling
///    of a file under no `paths:` root (verbatim `accept_as_ruby_file?` roots —
///    `reject_excluded` runs inside directory expansion only).
///
/// The spelling half still matters because `exclude:` patterns are matched against
/// the path string as `check` SPELLS it, not an absolute canonical path: bare
/// `check` expands each `paths:` root (`expand_check_paths_excluding` →
/// `collect_rb_files`, building `<root>/<rel>` by `Path::join`), so with the
/// production root `.` the matched string is `lib/sub.rb`, and under
/// `paths: ["."]` it is `./lib/sub.rb`.
///
/// Carried on [`ProjectContext`] beside [`ProjectContext::disable`] and
/// [`ProjectContext::stamp`] for the same reason those live there: every field is
/// config-derived, so `invalidate` / `swap_project` rebuild it for free and the S4
/// generation guard covers it with no new concurrency reasoning. Nothing re-reads
/// `.rigor.yml` per dispatch.
struct ExcludeMatcher {
    /// The project root AS SPELLED to the server: `.` in production, an absolute
    /// temp dir in tests — the same `join_root` convention [`project_files`] and
    /// [`touches_configured_root`] use.
    root: PathBuf,
    /// `paths:` — the discovery roots, needed to reproduce discovery's spelling of
    /// a buffer's path.
    paths: Vec<String>,
    /// `Configuration#exclude_patterns` verbatim — `BUILTIN_EXCLUDES +
    /// exclude:`, the one list `check`'s `reject_excluded` applies. Matched by
    /// [`crate::config::matches_exclude`], which reaches the same `dir.c`
    /// `File.fnmatch?`-no-flags port `check` expands with — never a second
    /// implementation of the glob rule.
    patterns: Vec<String>,
}

/// The three ways one open buffer's file can be NAMED, all of which `check` may
/// legitimately use — which is why the gate reasons over all of them rather than
/// picking one.
///
/// Resolved once per dispatch, off the loop thread, from the document URI.
#[derive(Default, Clone)]
struct BufferPaths {
    /// Fully resolved — the overlay's REPLACE key and the discovery-membership
    /// lookup key. `None` for a buffer with no filesystem identity at all.
    canonical: Option<PathBuf>,
    /// Literally what the editor named, percent-decoded and NOT resolved. This is
    /// the spelling `rigor check <that file>` would receive, and the only one that
    /// survives a symlinked DIRECTORY (which discovery never traverses).
    decoded: Option<PathBuf>,
    /// The decoded path with its DIRECTORY resolved but the file NAME kept. A
    /// symlinked `.rb` FILE is walked by discovery under the link's name, so this
    /// is the name `exclude:` is matched against for it — while `canonical` points
    /// at the target and would be matched against the wrong patterns.
    named: Option<PathBuf>,
}

impl BufferPaths {
    fn for_uri(uri: &Uri) -> Self {
        let decoded = uri_decoded_path(uri);
        let named = decoded.as_deref().and_then(|d| {
            let name = d.file_name()?;
            Some(std::fs::canonicalize(d.parent()?).ok()?.join(name))
        });
        Self { canonical: uri_to_canonical_path(uri), decoded, named }
    }
}

impl ExcludeMatcher {
    fn from_config(root: &Path, cfg: &Config) -> Self {
        Self {
            root: root.to_path_buf(),
            paths: crate::effective_config_paths(cfg),
            patterns: crate::exclude_patterns(cfg),
        }
    }

    /// Whether this buffer is `exclude:`d — i.e. whether `check` would report
    /// nothing for it. See the type docs for the two tiers and the invariant.
    fn excludes(&self, buf: &BufferPaths, overlay: Option<&ProjectFiles>) -> bool {
        // `patterns` is `BUILTIN_EXCLUDES + exclude:` — never empty (the
        // builtins are always present), so there is no cheap early exit;
        // tier 1 is the cheap answer when the overlay exists.
        // TIER 1 — discovery membership. The overlay IS the post-`exclude:` set.
        if let (Some(canonical), Some(overlay)) = (buf.canonical.as_deref(), overlay) {
            if overlay.files.iter().any(|(p, ..)| p == canonical) {
                return false;
            }
        }
        // TIER 2 — every candidate spelling of the buffer must be excluded. An empty
        // candidate set means no `check` invocation from this root names the file
        // (an untitled buffer, or one outside the workspace) ⇒ never excluded.
        let spellings = self.check_spellings(buf);
        if spellings.is_empty()
            || !spellings
                .iter()
                .all(|s| crate::config::matches_exclude(&self.patterns, s))
        {
            return false;
        }
        // TIER 3 — the buffer's OWN names are all excluded, but the file may still
        // reach discovery under a name the buffer does not carry: `lib/link.rb` is a
        // symlink to `lib/real.rb`, `exclude: ["lib/real.rb"]` prunes only the
        // target's spelling, and `check` analyses the content under the link's name.
        // Dropping output is the consequential direction, so it is confirmed against
        // the REAL walk before it happens — and only here, so the common path pays
        // nothing (this runs only for a buffer already judged excluded, in a session
        // whose overlay could not answer).
        !self.survives_discovery(buf.canonical.as_deref())
    }

    /// Whether bare-`check` discovery keeps SOME spelling of this canonical file —
    /// i.e. whether `check` analyses its content under any name. Consults
    /// [`discovery_spellings`], the same walk [`project_files`] uses, so it can
    /// never drift from what discovery actually does.
    ///
    /// **Only SYMLINKED spellings are consulted**, and that is complete rather than
    /// a shortcut: a spelling that is not a symlink names its own file, and every
    /// such spelling of the buffer is already among tier 2's candidates (tier 2
    /// re-spells the buffer under EVERY configured root, so the multi-root alias is
    /// covered there). The only name tier 2 structurally cannot see is one the
    /// buffer does not carry — a symlink elsewhere in the tree pointing at it.
    ///
    /// **Cost, measured** (3 000 files / 60 directories, warm): ~6 ms, versus
    /// ~25-40 ms for the naive form that `canonicalize`s every surviving spelling
    /// (`realpath` walks the whole path per file). An `lstat` per candidate answers
    /// the common case; the full resolve is paid only for entries that really are
    /// symlinks — of which a project has very few. This runs ONLY for a buffer
    /// already judged excluded — a dispatch that publishes nothing either way — so
    /// it never sits on the latency path of a buffer that is getting diagnostics.
    fn survives_discovery(&self, canonical: Option<&Path>) -> bool {
        let Some(canonical) = canonical else { return false };
        // `paths:` `.rb` FILE entries are kept VERBATIM — `accept_as_ruby_file?`
        // never consults `exclude:` — so a file root that resolves to this
        // canonical path is discovered regardless of the patterns.
        let file_root_hit = self.paths.iter().any(|p| {
            let joined = join_root(&self.root, p);
            joined.is_file()
                && p.ends_with(".rb")
                && std::fs::canonicalize(&joined).is_ok_and(|c| c == canonical)
        });
        file_root_hit
            || discovery_spellings(&self.root, &self.paths)
                .iter()
                .filter(|f| !crate::config::matches_exclude(&self.patterns, f))
                .any(|f| {
                    std::fs::symlink_metadata(f).is_ok_and(|m| m.file_type().is_symlink())
                        && std::fs::canonicalize(f).is_ok_and(|c| c == canonical)
                })
    }

    /// Every string `check` could match `exclude:` against for this buffer.
    ///
    /// Each of the buffer's three names ([`BufferPaths`]) is re-spelled under EVERY
    /// configured root that contains it — not the first, because under overlapping
    /// roots (`paths: [".", "lib"]`) discovery genuinely produces two spellings and
    /// only one of them may be excluded. A name under no configured root falls back
    /// to the project-root-relative spelling, which is what an explicit
    /// `rigor check <that file>` from the project root receives.
    ///
    /// Spellings a buffer only reaches through a VERBATIM root — a `paths:`
    /// `.rb` FILE entry, or an explicit `rigor check <file>` for a file under
    /// no `paths:` root — produce NO excludable spelling at all: `check` keeps
    /// them without consulting `exclude:` (`accept_as_ruby_file?` /
    /// `expand_check_paths_excluding`), so matching them against the patterns
    /// would suppress a buffer `check` analyses.
    ///
    /// Duplicates are harmless (the caller only asks whether they are ALL excluded)
    /// and common — the three names coincide whenever no symlink is involved.
    fn check_spellings(&self, buf: &BufferPaths) -> Vec<String> {
        let mut out = Vec::new();
        for candidate in [&buf.decoded, &buf.named, &buf.canonical].into_iter().flatten() {
            self.push_spellings(candidate, &mut out);
        }
        out
    }

    /// Append every spelling of one candidate path.
    fn push_spellings(&self, path: &Path, out: &mut Vec<String>) {
        for p in &self.paths {
            let base = join_root(&self.root, p);
            // A root that does not resolve cannot spell anything. Both the literal
            // and the canonical form are tried: the literal one catches a candidate
            // spelled through the same alias the root is (`project_root` = "." with
            // an absolute candidate never matches literally, but an injected
            // absolute root does), the canonical one everything else.
            let bases = [Some(base.clone()), std::fs::canonicalize(&base).ok()];
            for prefix in bases.into_iter().flatten() {
                let Ok(rel) = path.strip_prefix(&prefix) else { continue };
                // `paths:` may name a FILE (`project_files` pushes the joined path
                // as is); then `rel` is empty and the root IS the file — kept
                // VERBATIM (`accept_as_ruby_file?` never consults `exclude:`), so
                // it contributes no excludable spelling.
                if rel.as_os_str().is_empty() {
                    if !(base.is_file() && p.ends_with(".rb")) {
                        out.push(base.to_string_lossy().into_owned());
                    }
                    continue;
                }
                out.push(base.join(rel).to_string_lossy().into_owned());
            }
        }
        // Outside every `paths:` root there is NO excludable spelling: the only
        // run that reports on the file is an explicit `rigor check <that file>`,
        // which keeps an `.rb` argument VERBATIM — `reject_excluded` runs inside
        // directory expansion only (issue #201). Contributing no spelling leaves
        // the buffer's verdict to its in-roots names; a buffer with none is never
        // excluded, matching `check <file>` reporting it unconditionally.
    }
}

/// One held project file: its canonical path, its lowered AST, and its per-file
/// [`Harvest`] — everything `SourceIndex::merge` needs from that file.
///
/// The harvest is HELD rather than recomputed per dispatch (the held-harvest
/// slice). It is a pure function of `(AST, frozen CoreIndex)`, and the table
/// already pins both, so holding it cannot serve a fact the recomputation would
/// not have produced — it only removes the recomputation. Measured cost of NOT
/// holding it: 42.8 ms of every keystroke's dispatch at 4 675 files, which is
/// what pushed the dispatch over the [`OVERLAY_BUILD_BUDGET_DEFAULT`] and turned
/// the overlay OFF at that scale. Measured cost of holding it: ~4 KB/file, ~9 %
/// of the AST it sits beside.
///
/// Both are behind their own `Arc` for the same reason the AST always was: one
/// entry can be replaced ([`reharvest_sources`]) and the whole table cloned by
/// pointer copy.
///
/// **The `CoreIndex` half of that purity is an invariant, not an assumption.** A
/// harvest filters constant reads against the core it was built with, so a held
/// harvest is only valid while the `CoreIndex` it was built against is still the
/// one the merge uses. Every path that can change the core rebuilds the WHOLE
/// table against the new one ([`invalidate`] → [`apply_full_overlay_build`] →
/// [`build_overlay`]); the only path that does NOT rebuild the core is
/// [`reharvest_sources`], which reuses `st.project.index` verbatim — a project
/// `.rb` file cannot change the plugin set or the signature dirs, which is the
/// documented reason it never rebuilds the core. So no reachable state pairs a
/// harvest with a core it did not see.
type HeldFile = (PathBuf, Arc<LoweredAst>, Arc<Harvest>);

/// The tier-1 held project files backing the S4b cross-file overlay: one
/// [`HeldFile`] per analysable project `.rb` file, in the same order `check`'s
/// stage 1 produces them.
///
/// `Sync` by construction (`LoweredAst` is a plain owned arena and `Harvest` is
/// plain owned collections), so an `Arc<ProjectContext>` carrying it is shared
/// across the rayon workers exactly as the `CoreIndex` already is.
///
/// Each entry's members are behind their own `Arc` so the WHOLE table is cheap to
/// clone (pointer copies, no AST or harvest copies, ~100 µs at 4 675 files vs the
/// ~190 MB a deep clone would move). That is what makes the single-file re-harvest
/// ([`reharvest_sources`]) affordable: replace one entry's pair and swap the table
/// in, instead of re-parsing the project.
#[derive(Clone)]
struct ProjectFiles {
    files: Vec<HeldFile>,
}

/// One tier-1 overlay build's outcome + its MEASURED cost (S4b). The build does
/// NOT itself decide whether the overlay stays on — that is [`OverlayGuard`]'s
/// job, fed by this `merge` sample.
struct OverlayBuild {
    files: ProjectFiles,
    /// How many project files were parsed, lowered and harvested (reported even
    /// when the guard trips, so the disclosure can name the scale).
    file_count: usize,
    /// Stage 1 equivalent: read + parse + lower + harvest the whole project.
    parse_lower_harvest: Duration,
    /// Stage 2 equivalent: the `SourceIndex::merge` call the guard times. This is
    /// the SAME quantity a dispatch pays ([`overlay_source_index`]) — which is the
    /// point of sampling it here: tier 1 and the dispatch must be measuring one
    /// budget, not two different ones.
    merge: Duration,
}

/// The project-index rebuild budget for the S4b cross-file overlay (mini-spec
/// §"Scale guard"). The quantity is `SourceIndex::merge` — since the held-harvest
/// slice that is the whole per-dispatch rebuild (`build_project` before it, which
/// re-harvested every file first). A per-dispatch overlay rebuild pays this cost on every debounced
/// publish, so it must leave headroom under ADR-0029's 250 ms p50
/// `didChange`→publish target. Injectable via [`ServerContext::overlay_budget`] so
/// the guard test can force a trip.
const OVERLAY_BUILD_BUDGET_DEFAULT: Duration = Duration::from_millis(100);

/// How many CONSECUTIVE over-budget samples DISABLE the overlay (review N2). One
/// sample is not a classifier: measured on an idle machine at 3 117 files, six
/// consecutive rebuilds (`build_project`, pre-held-harvest) were 93.9 / 93.7 / 90.9 / **106.2** / 94.7 /
/// 92.9 ms against a 100 ms budget — a 1-in-6 false trip on a single sample, and
/// the same quantity spans 3.3× across machines (145–485 ms). Requiring two
/// consecutive samples squares that error probability.
///
/// **Re-enabling takes a SINGLE under-budget sample** — the hysteresis is
/// deliberately ASYMMETRIC. Disabling must resist a transient stall, but
/// re-enabling is cheap to undo: once the overlay is ON, per-dispatch samples are
/// plentiful, so a wrong re-enable self-corrects within two dispatches. Symmetry
/// would also make recovery near-unreachable, since while OFF the only samples
/// come from structural invalidations — requiring two consecutive ones would have
/// made the "re-evaluated, no restart needed" disclosure effectively false.
const OVERLAY_GUARD_STRIKES: u32 = 2;

/// The S4b scale guard's HYSTERESIS state (review N2, superseding the mini-spec's
/// single-sample sticky trip).
///
/// The guard is fed by builds that happen ANYWAY — the per-dispatch
/// rebuild while the overlay is ON (free: the dispatch builds it to
/// analyse), and the tier-1 build at a structural [`invalidate`] while it is OFF
/// (the only work done specifically to sample, and structural invalidations are
/// rare). It disables only after [`OVERLAY_GUARD_STRIKES`] consecutive
/// over-budget samples and RE-ENABLES after that many consecutive under-budget
/// ones, so the decision tracks the project rather than one noisy measurement,
/// and no session is stuck with a posture it did not earn.
struct OverlayGuard {
    /// Whether the cross-file overlay is currently allowed. Starts `true`.
    enabled: bool,
    /// Consecutive over-budget samples (reset by any under-budget one).
    over: u32,
}

/// What one [`OverlayGuard::record`] did to the posture.
#[derive(PartialEq, Eq, Debug)]
enum GuardVerdict {
    /// No posture change (the common case).
    Unchanged,
    /// The overlay just turned OFF: drop the held files and disclose.
    Disabled,
    /// The overlay just turned back ON: keep the freshly-built files and disclose.
    ReEnabled,
}

impl OverlayGuard {
    fn new() -> Self {
        Self { enabled: true, over: 0 }
    }

    /// Feed one rebuild timing to the guard and report any posture flip.
    ///
    /// Callers must only pass samples that describe the CURRENT posture: a worker
    /// result produced before a disable-swap says nothing about the state it lands
    /// in, and acting on one would be terminal (see [`handle_result`]).
    fn record(&mut self, sample: Duration, budget: Duration) -> GuardVerdict {
        if sample > budget {
            self.over += 1;
            if self.enabled && self.over >= OVERLAY_GUARD_STRIKES {
                self.enabled = false;
                self.over = 0;
                return GuardVerdict::Disabled;
            }
        } else {
            // Any under-budget sample clears the streak, so an isolated spike among
            // healthy samples can never accumulate into a trip…
            self.over = 0;
            // …and, while OFF, a single one is enough to recover (asymmetric by
            // design — see `OVERLAY_GUARD_STRIKES`).
            if !self.enabled {
                self.enabled = true;
                return GuardVerdict::ReEnabled;
            }
        }
        GuardVerdict::Unchanged
    }
}

/// Build the tier-1 overlay substrate (S4b): discover the project's `.rb` files
/// (bare-`check` semantics — the config's `paths:` roots expanded recursively,
/// minus `exclude:` and ERB templates), parse + lower + HARVEST them all, then run
/// the `SourceIndex::merge` the scale guard times.
///
/// The returned index itself is DISCARDED: a dispatch always rebuilds with the
/// buffer's file swapped in, so a stored copy would only cost memory. What tier 1
/// keeps is the per-file `(AST, harvest)` pairs (the rebuild input) and the timing
/// (the guard input).
///
/// **The harvest moved here from the dispatch.** `SourceIndex::build_project` is
/// `merge(asts.map(harvest))` (issue #92), so the old call harvested every project
/// file on the spot — and so did every keystroke's dispatch. Harvesting once per
/// file per BUILD instead is the held-harvest slice: same inputs, same result (a
/// harvest is a pure function of the AST and the frozen `CoreIndex`), one
/// occurrence instead of one per keystroke.
///
/// Runs on the loop thread — at startup, and inside the SYNCHRONOUS [`invalidate`]
/// (S4's decision: STRUCTURAL invalidations are rare, so an inline rebuild is
/// acceptable; a plain `.rb` save no longer comes here at all — see
/// [`reharvest_sources`]). Parse+lower+harvest is rayon-parallel exactly like
/// `check`'s stage 1, which harvests inside the same closure (#92).
fn build_overlay(root: &Path, cfg: &Config, index: &CoreIndex) -> OverlayBuild {
    let paths = project_files(root, cfg);
    let t0 = Instant::now();
    // Stage-1 equivalent: read + parse + lower + harvest, file-parallel, with
    // parse+lower panic-isolated per file (ADR-0016) — an unreadable or
    // parser-tripping file is simply omitted from the project index, never a crash
    // and never a diagnostic here (the LSP only reports on OPEN buffers).
    let files: Vec<HeldFile> = paths
        .par_iter()
        .filter_map(|path| {
            // Canonicalize so a buffer URI's path (also canonicalized) matches this
            // entry exactly — symlinks and `.`/`..` segments would otherwise defeat
            // the REPLACE lookup and silently double-register the file.
            let canonical = std::fs::canonicalize(path).ok()?;
            let (ast, harvest) = held_pair(path, index)?;
            Some((canonical, ast, harvest))
        })
        .collect();
    let parse_lower_harvest = t0.elapsed();

    let t1 = Instant::now();
    let pairs: Vec<(&Harvest, &LoweredAst)> =
        files.iter().map(|(_, a, h)| (h.as_ref(), a.as_ref())).collect();
    let project_index = SourceIndex::merge(&pairs, index);
    let merge = t1.elapsed();
    // Deliberately discarded (AFTER the measurement, so the timing is the BUILD,
    // not the build + teardown): every dispatch rebuilds with the buffer's file
    // swapped in, so keeping this copy would only cost memory. Its purpose here is
    // to be TIMED — it is the quantity the scale guard is defined on.
    drop(project_index);

    let file_count = files.len();
    OverlayBuild { files: ProjectFiles { files }, file_count, parse_lower_harvest, merge }
}

/// One held file's `(AST, harvest)` pair, or `None` when the file contributes
/// nothing to the project index — the single place the two are produced together,
/// so a caller cannot swap one and forget the other (a stale harvest beside a
/// fresh AST would serve cross-file facts the file no longer states).
///
/// The harvest is deliberately OUTSIDE [`lower_one`]'s `catch_unwind`, matching
/// both `check`'s stage 1 (#92 deviation 3) and today's LSP, where the harvest ran
/// inside an un-isolated `SourceIndex::build_project`: a harvest panic propagates
/// exactly as it does now rather than silently dropping the file from the index.
fn held_pair(path: &str, core: &CoreIndex) -> Option<(Arc<LoweredAst>, Arc<Harvest>)> {
    let ast = Arc::new(lower_one(path)?);
    let harvest = Arc::new(SourceIndex::harvest(&ast, core));
    Some((ast, harvest))
}

/// The [`FileKey`] a document's lowering must carry (issue #102): the file's
/// canonical path, which is the identity the per-file constant gate compares.
///
/// It is derived from the SAME path the overlay's REPLACE lookup compares
/// ([`uri_to_canonical_path`] for a buffer, tier 1's `canonicalize` for a held
/// file), and [`FileKey::for_path`] is idempotent on an already-canonical path
/// (and falls back verbatim for a path that no longer resolves, which is exactly
/// the parent-resolved spelling [`uri_to_canonical_path`] hands back for a
/// deleted file). So key equality tracks REPLACE equality exactly: the buffer's
/// lowering, the worker's lowering of the same buffer and the held on-disk
/// lowering of the same file all agree, which is what makes a cache-hit hover
/// fold a same-file literal constant instead of declining it.
///
/// `None` — a non-`file:` URI, an untitled buffer — yields a pathless
/// [`FileKey::anonymous`], distinct from every other lowering, so nothing folds
/// across two identity-less buffers.
fn document_file_key(canonical: Option<&Path>) -> FileKey {
    canonical.map_or_else(FileKey::anonymous, FileKey::for_path)
}

/// Read + lower ONE project file the way [`build_overlay`] does: ERB templates are
/// skipped (Prism's recovery over `<%= … %>` yields a garbage AST — matching
/// `check`), and a read error or a parser panic yields `None` rather than a crash
/// (ADR-0016). `None` therefore means "this file contributes nothing to the
/// project index", which is also the right answer for a file that was deleted.
fn lower_one(path: &str) -> Option<LoweredAst> {
    let source = std::fs::read(path).ok()?;
    if rigor_parse::looks_like_erb_template(&source) {
        return None;
    }
    // Issue #102: the held AST carries the file's canonical-path identity, the
    // same one a buffer's lowering of that file gets — so the REPLACE swap in
    // `overlay_source_index` preserves the per-file constant gate's answer.
    let key = document_file_key(Some(Path::new(path)));
    panic::catch_unwind(AssertUnwindSafe(|| {
        let result = parse(&source);
        // A file with parse errors contributes nothing to the index, the same
        // answer `check` gives it (`main.rs` stage 1) and the reference's
        // dependency walker gives it: Prism's recovery invents bindings, so
        // indexing the wreckage is worse than not indexing the file.
        (result.errors().next().is_none()).then(|| lower_with_key(&result, key))
    }))
    .ok()
    .flatten()
}

/// The tier-1 `CoreIndex`, built EXACTLY as `check`'s `analyze_files` builds it
/// (`main.rs`): the config `plugins:` PLUS the ADR-72 `Gemfile.lock`-gated
/// auto-detected overlays (`bundler.auto_detect`).
///
/// Review N1: the LSP previously passed the bare `cfg.plugins`, so on the most
/// common Ruby project shape — a `Gemfile.lock` with activesupport — the editor
/// did not see activesupport's core-ext reopenings and fired `undefined method
/// 'blank?' for "s"` where `rigor check` was silent. A per-keystroke false
/// positive, and a direct contradiction of S4b's parity headline.
fn build_core_index(root: &Path, cfg: &Config) -> CoreIndex {
    CoreIndex::for_project(&cfg.effective_plugins(root), &cfg.all_signature_dirs(root))
}

/// The project's analysable `.rb` files, in bare-`check` order (ADR-0040) —
/// literally the expansion `check` runs: each configured `paths:` root
/// through [`crate::expand_check_paths_excluding`], so a directory's hits are
/// `BUILTIN_EXCLUDES + exclude:`-filtered (`File.fnmatch?`, no flags) while a
/// `paths:` entry that names a `.rb` file is kept verbatim.
/// `root` is the project root — `.` in production (so the produced path strings
/// are byte-identical to what `check` matches `exclude:` against); a temp dir in
/// tests, which is what makes the overlay testable without mutating the process
/// cwd.
fn project_files(root: &Path, cfg: &Config) -> Vec<String> {
    let excludes = crate::exclude_patterns(cfg);
    // `configuration.paths` as `check` sees them — declared entries
    // absolutized (`resolve_paths_in`), so the `exclude:` match runs on the
    // same spelling (a `.`-spelled config root would otherwise defeat
    // `**`-led patterns under `File.fnmatch?`'s leading-period rule).
    let mut out = Vec::new();
    for p in crate::effective_config_paths(cfg) {
        let joined = join_root(root, &p).to_string_lossy().into_owned();
        // Expansion errors (a non-`.rb` file, a missing entry) produce no file —
        // the same silent skip `discovery_spellings` gives them.
        out.extend(crate::expand_check_paths_excluding(&[&joined], &excludes).0);
    }
    out
}

/// Bare-`check` discovery BEFORE `exclude:` — every path string the expansion
/// would be handed. Split out of [`project_files`] so [`ExcludeMatcher`] can
/// consult the real walk (rather than a second re-derivation of it) when it
/// needs to know whether ANOTHER spelling of one file survives the patterns.
fn discovery_spellings(root: &Path, paths: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for p in paths {
        let joined = join_root(root, p);
        if joined.is_dir() {
            let mut in_dir = Vec::new();
            crate::collect_rb_files(&joined, &mut in_dir);
            in_dir.sort();
            out.extend(in_dir);
        } else if joined.is_file() && p.ends_with(".rb") {
            out.push(joined.to_string_lossy().into_owned());
        }
    }
    out
}

/// Print the tier-1 overlay build's stage breakdown to stderr under `RIGOR_TIMING`
/// (any value) — the same env gate + one-line style `check`'s `analyze_files`
/// uses, so an operator can compare the LSP's tier-1 cost against the CLI's
/// stage 1/2 numbers directly. Invisible by default (no test, harness, or editor
/// sees it).
fn report_overlay_timing(build: &OverlayBuild, enabled: bool) {
    if std::env::var_os("RIGOR_TIMING").is_none() {
        return;
    }
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    eprintln!(
        "rigor lsp timing: overlay files={} parse+lower+harvest={:.1}ms merge={:.1}ms overlay={}",
        build.file_count,
        ms(build.parse_lower_harvest),
        ms(build.merge),
        if enabled { "on" } else { "off" },
    );
}

/// The `window/showMessage` text for a scale-guard posture flip. Mirrors the
/// ADR-0036 posture-disclosure precedent already used for the sidecar at startup:
/// rigor NEVER silently degrades — if the editor is getting narrower (single-file)
/// diagnostics than `check`, it says so, with the measured number that caused it.
///
/// The wording states that the decision is RE-EVALUATED (review N2): the guard has
/// hysteresis in both directions now, so a project that gets faster — or a session
/// that tripped on a slow moment — recovers without a restart.
fn overlay_guard_message(
    verdict: &GuardVerdict,
    files: usize,
    sample: Duration,
    budget: Duration,
) -> Option<String> {
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    match verdict {
        GuardVerdict::Unchanged => None,
        GuardVerdict::Disabled => Some(format!(
            "rigor: cross-file diagnostics disabled — the project index for {files} files took \
             {:.0}ms to build, over the {:.0}ms budget, on {OVERLAY_GUARD_STRIKES} consecutive \
             measurements; diagnostics fall back to single-file scope. Saving a config or \
             signature file re-measures it, and one under-budget rebuild restores cross-file \
             diagnostics — no restart needed.",
            ms(sample),
            ms(budget),
        )),
        GuardVerdict::ReEnabled => Some(format!(
            "rigor: cross-file diagnostics re-enabled — rebuilding the project index for {files} \
             files took {:.0}ms, back inside the {:.0}ms budget.",
            ms(sample),
            ms(budget),
        )),
    }
}

/// Join a configured relative path onto the project root WITHOUT introducing a
/// `./` prefix at the default root, so production path strings stay exactly the
/// ones `check` builds (the config `exclude:` globs are matched against them).
fn join_root(root: &Path, p: &str) -> PathBuf {
    if root == Path::new(".") {
        PathBuf::from(p)
    } else {
        root.join(p)
    }
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
    /// The project root the S4b overlay discovers its files under. Production is
    /// `.` (the server's cwd, matching the root `CoreIndex::for_project` and bare
    /// `check` already use); tests inject a temp dir so a real multi-file project
    /// can be driven without mutating the process-global cwd.
    ///
    /// It stays `.` even now that `rootUri` is honoured, and deliberately: the
    /// client's root is adopted by moving the process cwd to it
    /// ([`enter_project_root`]), so the path STRINGS this seam produces — which
    /// the `exclude:` globs are matched against — remain the relative ones
    /// `check` builds. An absolute root here would silently stop every relative
    /// `exclude:` pattern from matching.
    project_root: PathBuf,
    /// The tier-1 rebuild scale-guard budget (S4b). Production is
    /// [`OVERLAY_BUILD_BUDGET_DEFAULT`]; the guard test forces a value low enough
    /// to trip deterministically.
    overlay_budget: Duration,
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
    /// The session config. Re-read from `<root>/.rigor.yml` by [`reload_config`]
    /// on every structural [`invalidate`], and the source every context rebuild
    /// derives from (index plugins + signature dirs, overlay `paths:`/`exclude:`,
    /// `disable:`, the severity stamp).
    cfg: Config,
    /// Whether the LAST `.rigor.yml` read failed — the hysteresis bit that makes
    /// the disclosure fire on the usable⇄broken TRANSITION rather than on every
    /// save of a file the user is still fixing. Seeded from the startup read, so a
    /// session that booted on a broken config announces the recovery when the file
    /// finally parses.
    config_broken: bool,
    /// The worker-results sender, cloned into each worker (S3). The matching
    /// receiver stays local to [`main_loop`]'s `select!`.
    results_tx: crossbeam_channel::Sender<Computed>,
    /// The S4b overlay scale guard's hysteresis state (review N2). Loop-owned like
    /// everything else here: workers report timings, the loop decides the posture.
    guard: OverlayGuard,
    /// The per-URI last-good project index the synchronous query handlers answer
    /// from ([`CrossFileCache`]). Loop-owned: written by [`handle_result`] after
    /// the liveness gate, read by `hover` / `completion`, cleared on every
    /// [`swap_project`], evicted on `didClose`.
    crossfile: CrossFileCache,
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
    /// How long this dispatch's cross-file overlay `SourceIndex::merge`
    /// took, or `None` when the overlay was off (nothing was built). The scale
    /// guard's sample (review N2) — a measurement of the work the dispatch did
    /// anyway, carried back to the loop thread which owns the guard state.
    overlay_build: Option<Duration>,
    /// The PROJECT `SourceIndex` this dispatch analysed against — the cross-file
    /// cache's payload — or `None` when the overlay was off (a single-file index
    /// is what hover/completion already build per request, so caching one buys
    /// nothing and would only add staleness). Populated in lockstep with
    /// [`Self::overlay_build`]: `Some` exactly when that is `Some`.
    ///
    /// An `Arc` because the index the dispatch built is exactly the index a
    /// same-URI hover/completion wants, and re-deriving it on the loop thread
    /// costs the 20-70 ms merge the <100 ms p95 budget cannot spend. Sending it
    /// over the existing channel adds no concurrency reasoning: `SourceIndex` is
    /// already `Send + Sync` (it is reachable from the `Arc<ProjectContext>`
    /// every worker holds).
    project_index: Option<Arc<SourceIndex>>,
}

/// The maximum number of per-URI cross-file cache entries kept alive. Hygiene,
/// NOT a memory control: the probe measured one merged `SourceIndex` at
/// ~4.1-4.7 KB per project file, so even the largest measured project fits
/// ~17-90 entries inside ADR-0029's 600 MB budget, an order of magnitude above a
/// realistic open-tab count. The cap exists only so a pathological session (a
/// bulk "open every file" action) cannot grow the map without bound.
const CROSSFILE_CACHE_CAP: usize = 8;

/// One cached last-good project index for one URI.
struct CachedIndex {
    /// The `Arc` the dispatch handed back — read, never rebuilt, by a same-URI
    /// hover / completion.
    index: Arc<SourceIndex>,
    /// The project generation the dispatch computed against. A reader re-checks
    /// it (belt-and-braces: [`swap_project`] already clears the whole cache on
    /// every generation bump, so an entry can never legally outlive one).
    generation: u64,
    /// The LRU clock value of this entry's last store/hit.
    used: u64,
}

/// The **per-URI last-good project `SourceIndex`** cache (2026-08-26 cross-file
/// cache slice). Loop-owned like every other [`Session`] field: only the loop
/// thread writes it (in [`handle_result`], AFTER the 3-axis liveness gate) and
/// only the loop thread reads it (the synchronous hover / completion handlers).
///
/// **Why per-URI, and why SAME-URI ONLY.** An entry for URI A is the project
/// index with A's dirty buffer REPLACING A's on-disk file
/// ([`overlay_source_index`]). Serving it for URI B would answer B's questions
/// from A's unsaved edits — the double-registration hazard the REPLACE rule
/// exists to prevent, in the query handlers instead of the diagnostics. So a
/// reader consults its OWN URI's entry or falls back to today's single-file
/// [`SourceIndex::build`]; there is no cross-URI borrowing at any staleness.
///
/// **Staleness is inherited, not invented.** The write happens at exactly the
/// point the diagnostics computed alongside it are published, so a cached entry
/// is never staler than what the editor is currently SHOWING for that file
/// (probe §4: one debounce + one dispatch, ~230-330 ms worst case) — and hover
/// agreeing with the visible markers is the UX property, not an accident.
#[derive(Default)]
struct CrossFileCache {
    entries: HashMap<String, CachedIndex>,
    /// A monotonic counter stamped on every store and every hit — the LRU order.
    /// A counter, not an `Instant`: the eviction order must be deterministic in
    /// tests, and "which entry was touched least recently" needs no wall clock.
    clock: u64,
}

impl CrossFileCache {
    fn new() -> Self {
        Self::default()
    }

    /// Record the project index a LIVE dispatch for `uri` computed under
    /// `generation`. Called ONLY from [`handle_result`]'s live branch, and only
    /// when the overlay was on for that dispatch.
    fn store(&mut self, uri: &Uri, index: Arc<SourceIndex>, generation: u64) {
        self.clock += 1;
        let used = self.clock;
        self.entries.insert(uri_key(uri), CachedIndex { index, generation, used });
        while self.entries.len() > CROSSFILE_CACHE_CAP {
            let victim = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| k.clone());
            let Some(victim) = victim else { break };
            self.entries.remove(&victim);
        }
    }

    /// The cached project index for `uri`, iff it was built under the CURRENT
    /// `generation`. `None` ⇒ the caller uses today's single-file index.
    fn get(&mut self, uri: &Uri, generation: u64) -> Option<Arc<SourceIndex>> {
        self.clock += 1;
        let now = self.clock;
        let entry = self.entries.get_mut(&uri_key(uri))?;
        if entry.generation != generation {
            return None;
        }
        entry.used = now;
        Some(Arc::clone(&entry.index))
    }

    /// Drop `uri`'s entry (`didClose` — a closed document can receive no
    /// hover/completion request, so holding its index is pure retention).
    fn evict(&mut self, uri: &Uri) {
        self.entries.remove(&uri_key(uri));
    }

    /// Drop EVERY entry ([`swap_project`]): a new project generation means every
    /// cached index was built against a context that no longer exists.
    fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    fn contains(&self, uri: &Uri) -> bool {
        self.entries.contains_key(&uri_key(uri))
    }
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
    guard: OverlayGuard,
    config_broken: bool,
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
        config_broken,
        results_tx,
        guard,
        crossfile: CrossFileCache::new(),
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
/// the Ruby VM is not respawned. In-flight workers holding the OLD `Arc` finish
/// against it and are generation-dropped in [`handle_result`].
///
/// **The config file IS re-parsed here** ([`reload_config`], first, so the
/// `CoreIndex` and the overlay are both built from the NEW config) — deliberately
/// beating the reference, whose `ProjectContext#invalidate!` rebuilds from the
/// same retained `@configuration` and so needs an editor restart to see an edited
/// `.rigor.yml`. The LSP has no diagnostic-set parity obligation (it is not the
/// `check` pipeline), and the alternative was strictly worse than doing nothing:
/// the watcher already fired, the rebuild already ran, and it republished the same
/// stale answer — which reads to a user as "rigor ignored my config" rather than
/// as "restart me". See `docs/notes/20260801-lsp-config-reload.md`.
///
/// **S4b**: the rebuild also re-harvests the cross-file overlay substrate and
/// re-times its `merge`, feeding the sample to the hysteresis
/// [`OverlayGuard`]. Returns every `window/showMessage` disclosure the rebuild
/// owes the user (config reload state, then a guard posture flip) for the caller
/// to send — a `Vec` because one invalidation can genuinely owe both.
///
/// **This is now the STRUCTURAL path only** (review N3): `.rigor.yml` /
/// `Gemfile.lock` / `sig/**/*.rbs`. A plain project `.rb` save goes to
/// [`reharvest_sources`] instead, which touches one AST entry rather than
/// re-parsing the project on the loop thread.
fn invalidate(ctx: &ServerContext, st: &mut Session) -> Vec<(MessageType, String)> {
    let mut disclosures = Vec::new();
    // FIRST: everything below reads `st.cfg`. `build_core_index` consumes
    // `plugins:` + `signature_paths:`, `build_overlay` consumes `paths:` +
    // `exclude:`, and `swap_project` consumes `disable:` + the severity axes — so
    // a reload placed anywhere later would ship a context built half from each.
    if let Some(msg) = reload_config(ctx, st) {
        disclosures.push(msg);
    }
    // The sidecar is PRESERVED (its `Arc` is cloned into the new context by
    // `swap_project`), so the Ruby VM is not respawned.
    let index = Arc::new(build_core_index(&ctx.project_root, &st.cfg));
    if let Some(msg) = apply_full_overlay_build(ctx, st, index) {
        disclosures.push((MessageType::WARNING, msg));
    }
    disclosures
}

/// Read the project's `.rigor.yml` and, when it is usable, install it as the
/// session config. Returns a `window/showMessage` disclosure on a STATE CHANGE
/// only (usable ⇄ broken), never per save.
///
/// **A broken config keeps the last good one.** This is the case the feature
/// lives or dies on: an editor writes `.rigor.yml` on every save, so the server
/// sees half-written YAML constantly, and [`Config::load`]'s one-shot answer —
/// silently substitute [`Config::default`] — would drop the user's whole
/// `disable:` list mid-keystroke and flood the buffer with markers that vanish
/// again when the file parses. Defaults are a configuration the user did not
/// write; the last good one is. Deleting `.rigor.yml` is NOT that case and is
/// honoured immediately: absent means the defaults genuinely ARE the config,
/// which is why [`ConfigRead`] separates the two.
///
/// **Disclosure is `window/showMessage`, not a diagnostic on the YAML file.** A
/// diagnostic would need a range this loader does not compute (serde_yaml's error
/// carries a location, but the LSP would then own publishing and CLEARING markers
/// on a non-Ruby URI that may not even be open), and `showMessage` is already
/// this server's disclosure channel for the sidecar posture and the overlay
/// guard. Rejected, not overlooked.
///
/// Only the transition messages: a user fighting a broken file saves it many
/// times, and one modal per save is worse than the staleness this fixes.
fn reload_config(ctx: &ServerContext, st: &mut Session) -> Option<(MessageType, String)> {
    match read_project_config(&ctx.project_root) {
        Ok(cfg) => {
            st.cfg = cfg;
            // Only announce the recovery — a config that was fine and stayed fine
            // is the overwhelmingly common case and says nothing worth a popup.
            std::mem::replace(&mut st.config_broken, false).then(|| {
                (
                    MessageType::INFO,
                    "rigor: config reloaded — the earlier error is resolved".to_string(),
                )
            })
        }
        Err(reason) => {
            // `st.cfg` is deliberately left alone: the last good config keeps
            // serving until the file parses again.
            (!std::mem::replace(&mut st.config_broken, true))
                .then(|| (MessageType::WARNING, config_broken_message(&reason)))
        }
    }
}

/// Read the project's config into the session's answer for "what is the
/// config", following `Configuration::DISCOVERY_ORDER` (`.rigor.yml`, then
/// `.rigor.dist.yml` — the first present wins outright).
/// `Ok` = usable (parsed, or the defaults because there is no file); `Err(reason)`
/// = the file is THERE but unusable, and the caller must decide what to serve
/// instead — [`reload_config`] keeps the last good config, startup falls back to
/// defaults because it has none.
fn read_project_config(root: &Path) -> Result<Config, String> {
    // `Configuration::DISCOVERY_ORDER`: `.rigor.yml` then `.rigor.dist.yml` —
    // the first present wins outright (includes are the only merge upstream).
    for name in [".rigor.yml", ".rigor.dist.yml"] {
        match Config::read(&root.join(name)) {
            crate::config::ConfigRead::Parsed(cfg) => return Ok(*cfg),
            // Absent is a valid answer only when NEITHER candidate exists —
            // try the next before settling on defaults.
            crate::config::ConfigRead::Absent(_) => continue,
            crate::config::ConfigRead::Fatal(f) => return Err(f.message),
        }
    }
    // No file is a valid configuration — the defaults, exactly as a project
    // that never wrote one gets. Reloading to defaults after a DELETE is
    // correct for the same reason.
    Ok(Config::default())
}

/// The user-facing text for a `.rigor.yml` that will not parse. Names WHICH
/// config is now in force, because that is the question a user staring at
/// unexpected markers is really asking — and the answer differs by when it broke:
/// a reload keeps the last good config, but a session that booted on a broken
/// file never had one and is running on defaults.
fn config_broken_message(reason: &str) -> String {
    format!(
        "rigor: the config could not be read ({reason}) — keeping the last good \
         configuration; fix and save the file to reload it"
    )
}

/// …the startup variant, where there is no last good configuration to keep.
fn config_broken_at_startup_message(reason: &str) -> String {
    format!(
        "rigor: the config could not be read ({reason}) — analyzing with DEFAULT \
         settings; fix and save the file to reload it"
    )
}

/// Re-harvest the WHOLE overlay against `index`, feed the `merge` timing
/// to the scale guard, and swap the resulting context in. Shared by the structural
/// [`invalidate`] and by [`reharvest_sources`]'s new-file path.
fn apply_full_overlay_build(
    ctx: &ServerContext,
    st: &mut Session,
    index: Arc<CoreIndex>,
) -> Option<String> {
    let build = build_overlay(&ctx.project_root, &st.cfg, &index);
    // An EMPTY project is not an over-budget one: it must neither feed the guard a
    // meaningless ~0 sample (which would count toward re-enabling) nor disclose.
    let message = if build.file_count > 0 {
        let verdict = st.guard.record(build.merge, ctx.overlay_budget);
        overlay_guard_message(&verdict, build.file_count, build.merge, ctx.overlay_budget)
    } else {
        None
    };
    report_overlay_timing(&build, st.guard.enabled);
    let overlay = (st.guard.enabled && build.file_count > 0).then_some(build.files);
    swap_project(ctx, st, index, overlay);
    message
}

/// Install a new tier-1 [`ProjectContext`] with a BUMPED generation, reusing the
/// live sidecar and (unless replaced) the existing `CoreIndex`.
///
/// The generation bump is the whole point: any worker in flight computed against
/// the previous context, so its result is generation-dropped and re-dispatched by
/// [`handle_result`] — the 3-axis stale-drop covers an overlay swap exactly as it
/// covers an index rebuild, with no new concurrency reasoning.
fn swap_project(
    ctx: &ServerContext,
    st: &mut Session,
    index: Arc<CoreIndex>,
    overlay: Option<ProjectFiles>,
) {
    st.project = Arc::new(ProjectContext {
        generation: st.project.generation + 1,
        index,
        disable: st.cfg.disable_matcher(),
        folder: st.project.folder.clone(), // reuse the live sidecar; no respawn.
        // Config-derived exactly like `disable`, and rebuilt from `st.cfg` on the
        // same schedule. Since `invalidate` re-parses `.rigor.yml` into `st.cfg`
        // first, an edited `severity_profile:` / `severity_overrides:` /
        // `bleeding_edge:` lands on the very next publish — the stamp was written
        // to follow `st.cfg` for exactly this day.
        stamp: SeverityStamp::from_config(&st.cfg),
        // Same provenance, same schedule: rebuilt from `st.cfg` (and the immutable
        // session root) on every context swap, so a newly-added `exclude:` entry
        // takes effect on the next publish.
        exclude: ExcludeMatcher::from_config(&ctx.project_root, &st.cfg),
        overlay,
    });
    // Every cross-file cache entry was built against the context just superseded
    // — including the overlay this swap may have just turned OFF (the guard trip)
    // — so the whole cache dies with it. Clearing HERE, at the single site every
    // generation bump goes through, is what makes the invalidation total: config
    // reloads, watched-file re-harvests, empty-project swaps and guard trips are
    // all covered without any of them having to remember to.
    st.crossfile.clear();
}

/// Re-harvest ONLY the changed project `.rb` files' held entries (review N3).
///
/// S4 approved a synchronous `invalidate` when it was a `CoreIndex` rebuild on a
/// RARE trigger. S4b made that ~20× more expensive (it re-reads, re-parses and
/// re-lowers the whole project — ~200 ms at 3 117 files) while the trigger became
/// EVERY source save, on the thread that owes hover/completion a <100 ms p95. The
/// fix is to touch what actually changed: the held table is a `Vec` of
/// [`HeldFile`]s, so replacing or removing one entry is a cheap clone + one file's
/// parse + harvest — sub-millisecond regardless of project size.
///
/// **The invariant** (adversarial review of PR #43): the incremental state must be
/// byte-identical to what a full rebuild would produce. The first implementation
/// tried to decide membership with a `ProjectScope` predicate re-deriving
/// bare-`check`'s discovery rule, and diverged from it three ways (a deleted
/// DIRECTORY, a symlinked `.rb` stored under its out-of-root canonical path, and
/// `paths: ["."]`). A predicate that must agree with a tree walk is the wrong
/// shape; this is the conservative rule that replaces it:
///
/// - **Replace in place ONLY when the changed path resolves to an entry ALREADY
///   HELD** (looked up by canonical path, keeping its original position — order
///   matters to `merge`'s multi-pass replay). A held entry whose file is
///   confirmed gone is removed in place.
/// - **EVERY other case falls back to [`apply_full_overlay_build`]**: an
///   unresolvable path (the deleted-directory case), a path not currently held (it
///   could be a new in-scope file), a read/parse failure, anything ambiguous.
///   Correct by construction — no predicate to keep in sync with discovery.
/// - An event is IGNORED only when BOTH: its canonical form is not held AND its
///   path is not under any configured root. Both conditions, because a filter that
///   can drop a real event reintroduces the divergence.
///
/// The fast path still covers the overwhelmingly common event — saving the file
/// you are editing is a held entry, so it is a clone of a `Vec<(PathBuf, Arc<_>)>`
/// plus one file's parse (measured 0.21 ms at 3 117 files, vs 121 ms for the full
/// rebuild it replaced).
///
/// **Known latency trade-off.** A file the index never holds — `exclude`d, an ERB
/// template, or one the parser rejects — can never take the fast path, so EVERY
/// save of one pays the full rebuild (121 ms on the loop thread at 3 117 files).
/// That is correct, and harmless for the usual case (excluded trees are usually
/// vendored and rarely edited), but it is a latency cliff for a repo that excludes
/// a large tree it still actively edits. The fix, if it ever bites, is to remember
/// the discovered-but-not-held paths so they can be recognised and skipped —
/// deliberately not done here, because it reintroduces exactly the
/// second-source-of-truth-about-discovery that this rewrite removed.
///
/// Never rebuilds the `CoreIndex`: it depends on the plugin set and the signature
/// dirs, neither of which a project `.rb` file can change.
fn reharvest_sources(ctx: &ServerContext, st: &mut Session, uris: &[String]) -> Option<String> {
    let index = Arc::clone(&st.project.index);
    let Some(mut files) = st.project.overlay.clone() else {
        // Overlay off (guard tripped, or an empty project): `.rb` content feeds
        // nothing. Still bump the generation so open buffers re-publish under a
        // fresh context, matching S4's observable "a watched change re-analyses".
        swap_project(ctx, st, index, None);
        return None;
    };
    for uri_str in uris {
        let Ok(uri) = uri_str.parse::<Uri>() else {
            return apply_full_overlay_build(ctx, st, index); // unparseable ⇒ ambiguous
        };
        // The parent-fallback canonicalization matters here: a DELETED file must
        // still resolve to the path tier 1 recorded, or its stale entry could never
        // be found and removed. `None` means even the parent is gone (a deleted
        // directory) — not resolvable, so not decidable here.
        let canonical = uri_to_canonical_path(&uri);
        // ALL positions holding this canonical path, not just the first. Discovery
        // can legitimately yield the same file twice — under `paths: ["."]` a
        // symlinked `.rb` and its target are both walked, and both canonicalize to
        // the target — and a full rebuild keeps both entries (as does `check`). The
        // invariant is to match the rebuild, so every occurrence is updated or
        // removed together.
        let held: Vec<usize> = canonical
            .as_deref()
            .map(|p| {
                files
                    .files
                    .iter()
                    .enumerate()
                    .filter(|(_, (q, ..))| q == p)
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default();
        if held.is_empty() {
            // Not held. Ignoring requires proving the event is out of project scope,
            // which is a comparison of two path SPELLINGS — and that is only sound
            // when the path RESOLVES, so both sides can be canonicalized.
            //
            // With no canonical form (review R-1: the deleted-DIRECTORY case) the
            // URI's decoded spelling is the only candidate, and a client that
            // reaches the workspace through a symlink spells it differently from the
            // canonicalized root — `/tmp/proj/lib/...` vs `/private/tmp/proj/lib` on
            // macOS, or any symlinked project/home dir. The comparison then says
            // "out of scope" for an event a full rebuild WOULD have acted on, and
            // the stale AST survives: B-1's symptom again.
            //
            // So an unresolvable path is NEVER ignored. The cost is one rebuild for
            // an out-of-workspace delete whose parent is also gone — which a
            // workspace-scoped watcher barely produces.
            if watched_event_is_ignorable(ctx, &st.cfg, canonical.as_deref(), &uri) {
                continue;
            }
            return apply_full_overlay_build(ctx, st, index);
        }
        let path = canonical.expect("a held match implies a resolved path");
        if !path.exists() {
            // Confirmed gone — remove in place (back-to-front, so the earlier
            // indices stay valid).
            for i in held.iter().rev() {
                files.files.remove(*i);
            }
        } else if let Some((ast, harvest)) = held_pair(&path.to_string_lossy(), &index) {
            // BOTH halves, always together ([`held_pair`]): the harvest is what
            // carries this file's cross-file facts into the merge, so swapping only
            // the AST would leave every other file seeing the pre-save content.
            for i in &held {
                files.files[*i].1 = Arc::clone(&ast);
                files.files[*i].2 = Arc::clone(&harvest);
            }
        } else {
            // Present but unharvestable (read error, now an ERB template, a parser
            // panic) — ambiguous, so let a full rebuild decide.
            return apply_full_overlay_build(ctx, st, index);
        }
    }
    swap_project(ctx, st, index, Some(files));
    None
}

/// Whether a watched event for a path that is NOT currently held may be dropped
/// without rebuilding — the complete ignore rule (review R-1), named so the
/// implementation and its tests share one definition.
///
/// **An unresolvable path is never ignorable.** Ignoring requires PROVING the event
/// is out of project scope, which is a comparison of path spellings, and that is
/// only sound when the path resolves so both sides can be canonicalized. When it
/// does not resolve — the deleted-DIRECTORY case — the URI's decoded spelling is
/// the only candidate, and a client reaching the workspace through a symlink
/// spells it differently from the canonicalized root (`/tmp/proj/...` vs
/// `/private/tmp/proj/...` on macOS, or any symlinked project or home dir). The
/// comparison then "proves" out-of-scope for an event a full rebuild WOULD have
/// acted on, and a stale AST survives — the B-1 symptom.
fn watched_event_is_ignorable(
    ctx: &ServerContext,
    cfg: &Config,
    canonical: Option<&Path>,
    uri: &Uri,
) -> bool {
    canonical.is_some_and(|c| !touches_configured_root(ctx, cfg, c, uri))
}

/// **Only ever called with a RESOLVED path** (review R-1), so the configured roots
/// and the candidate are compared canonical-to-canonical and no symlinked spelling
/// can make an in-scope path look out of scope. An unresolvable path never reaches
/// here — it takes the full rebuild unconditionally.
///
/// Deliberately one-sided: it answers "is it safe to IGNORE this?", so every
/// uncertainty (an unresolvable configured root; a path in scope under EITHER its
/// decoded or its canonical spelling) answers `true` and costs at most one full
/// rebuild. Both spellings count because a NEW symlink inside `lib` pointing out of
/// the tree is in scope by its decoded spelling while its canonical form is not —
/// and bare-`check` discovery WOULD harvest it. It is NOT a discovery predicate —
/// it never decides that a file belongs in the index, only that an event is not
/// obviously irrelevant.
///
/// The configured roots are CANONICALIZED rather than compared literally: in
/// production `ctx.project_root` is `.`, so `join_root` yields the relative
/// `"lib"`, which no absolute candidate could ever match.
fn touches_configured_root(
    ctx: &ServerContext,
    cfg: &Config,
    canonical: &Path,
    uri: &Uri,
) -> bool {
    let decoded = uri_decoded_path(uri);
    let candidates: Vec<&Path> = decoded.as_deref().into_iter().chain([canonical]).collect();
    cfg.paths.iter().any(|p| {
        // An unresolvable configured root cannot rule anything out.
        let Ok(canon_root) = std::fs::canonicalize(join_root(&ctx.project_root, p)) else {
            return true;
        };
        candidates.iter().any(|c| c.starts_with(&canon_root))
    })
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
                            // SAME-URI only, and only under the CURRENT project
                            // generation. The `Arc` is cloned out first so the
                            // `&mut st` borrow the LRU touch needs ends before
                            // the handler's immutable borrows begin.
                            let cached = crossfile_for(
                                st,
                                &params.text_document_position_params.text_document.uri,
                            );
                            let hover = hover(&st.project, &st.buffers, &params, cached.as_deref());
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
                            let cached =
                                crossfile_for(st, &params.text_document_position.text_document.uri);
                            let items =
                                completion(&st.project, &st.buffers, &params, cached.as_deref());
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
                    // A closed document can no longer receive a hover/completion
                    // request, so its cached project index is pure retention —
                    // drop it here rather than waiting for the LRU or the next
                    // generation bump. (A reopen re-populates on its first
                    // dispatch, which is also the point its diagnostics return.)
                    st.crossfile.evict(&uri);
                    send_diagnostics(connection, &uri, Vec::new())?;
                }
            }
            "workspace/didChangeWatchedFiles" => {
                // Tier-1 invalidation trigger (S4). Invalidate + re-analyse ALL open
                // buffers ONLY if a changed URI is on the config + project-signature
                // surface (`.rigor.yml` / `Gemfile.lock` / a project `*.rb` /
                // `sig/**/*.rbs`). An unrelated path (a `.txt`, a build artifact) does
                // NOT invalidate — avoiding a needless ~100-300 ms rebuild.
                // Review N3: a project `.rb` save re-harvests ONLY that file's AST
                // entry (sub-millisecond); only the config / signature surface
                // pays the full synchronous tier-1 rebuild.
                match classify_watched_files(&not.params) {
                    WatchedChange::None => {}
                    WatchedChange::Sources(uris) => {
                        if let Some(msg) = reharvest_sources(ctx, st, &uris) {
                            send_show_message(connection, MessageType::WARNING, msg)?;
                        }
                        reanalyze_open_buffers(ctx, st);
                    }
                    WatchedChange::Structural => {
                        for (typ, msg) in invalidate(ctx, st) {
                            send_show_message(connection, typ, msg)?;
                        }
                        reanalyze_open_buffers(ctx, st);
                    }
                }
            }
            "workspace/didChangeConfiguration" => {
                // Configuration refresh (S4): always invalidate + re-analyse open
                // buffers. The payload shape is client-specific and still ignored —
                // but `invalidate` now RE-READS `.rigor.yml`, so this notification
                // finally does what its name promises for the config that actually
                // governs rigor, instead of rebuilding from the startup parse.
                for (typ, msg) in invalidate(ctx, st) {
                    send_show_message(connection, typ, msg)?;
                }
                reanalyze_open_buffers(ctx, st);
            }
            _ => {}
        },
        Message::Response(_) => {}
    }
    Ok(false)
}

/// The cross-file cache entry a synchronous query handler for `uri` may answer
/// from: THAT URI's own last-good project index, and only if it was built under
/// the CURRENT project generation. `None` ⇒ the handler builds today's
/// single-file index, exactly as before this slice.
///
/// A free function rather than an inline call so the two handlers cannot drift
/// apart on the same-URI rule, which is the correctness guard of the whole
/// design (an index cached for A carries A's dirty overlay).
fn crossfile_for(st: &mut Session, uri: &Uri) -> Option<Arc<SourceIndex>> {
    let generation = st.project.generation;
    st.crossfile.get(uri, generation)
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
                    { "globPattern": "**/.rigor.dist.yml" },
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

/// How a `workspace/didChangeWatchedFiles` payload affects tier 1 (review N3).
/// The two relevant kinds cost VERY different amounts, so they are dispatched
/// differently rather than both funnelled into a full rebuild.
#[derive(PartialEq, Eq, Debug)]
enum WatchedChange {
    /// Nothing on the invalidation surface — no work at all.
    None,
    /// Project `*.rb` saves: re-harvest exactly these files' held entries.
    Sources(Vec<String>),
    /// `.rigor.yml` / `Gemfile.lock` / `sig/**/*.rbs`: the plugin set or the
    /// signature environment may have changed, so the `CoreIndex` AND the whole
    /// overlay are rebuilt. Structural changes are genuinely rare, which is what
    /// made S4's synchronous-rebuild decision reasonable in the first place.
    Structural,
}

/// Classify a `workspace/didChangeWatchedFiles` payload. A structural change
/// anywhere in the batch wins (it subsumes any source change in the same batch,
/// since the full rebuild re-harvests everything).
fn classify_watched_files(params: &serde_json::Value) -> WatchedChange {
    let Some(changes) = params.get("changes").and_then(|c| c.as_array()) else {
        return WatchedChange::None;
    };
    let uris: Vec<&str> = changes
        .iter()
        .filter_map(|c| c.get("uri").and_then(serde_json::Value::as_str))
        .collect();
    if uris.iter().any(|u| watched_file_is_structural(u)) {
        return WatchedChange::Structural;
    }
    let sources: Vec<String> = uris
        .iter()
        .filter(|u| u.ends_with(".rb"))
        .map(|u| (*u).to_string())
        .collect();
    if sources.is_empty() {
        WatchedChange::None
    } else {
        WatchedChange::Sources(sources)
    }
}

/// Whether a changed URI is on the STRUCTURAL surface — the one that can move the
/// plugin set or the RBS environment, and so needs a full tier-1 rebuild.
fn watched_file_is_structural(uri: &str) -> bool {
    uri.ends_with(".rigor.yml")
        || uri.ends_with(".rigor.dist.yml")
        || uri.ends_with("Gemfile.lock")
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
        // The buffer's on-disk identity, resolved OFF the loop thread (S4b). The
        // CANONICAL form is what the overlay REPLACES in the held project ASTs
        // (`None` — a non-`file:` or never-saved URI — appends instead, the same
        // index `check` would build for the project files PLUS this one); the other
        // two names are what the `exclude:` gate matches patterns against.
        let paths = BufferPaths::for_uri(&uri);
        let (diags, overlay_build, project_index) = panic::catch_unwind(AssertUnwindSafe(|| {
            gate(version, generation); // test seam: may block (hold mid-flight) or panic.
            compute_diagnostics(&project, &paths, &text)
        }))
        .unwrap_or_default();
        // Always send exactly one result (even empty on a caught panic), so the
        // loop's in-flight tracking for this URI clears. `Err` = loop gone (shutdown).
        let _ = tx.send(Computed {
            uri,
            version,
            generation,
            epoch,
            diags,
            overlay_build,
            project_index,
        });
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
///
/// The live branch is also where the [`CrossFileCache`] entry is written, so the
/// cache inherits all three staleness axes instead of inventing its own.
fn handle_result(
    connection: &Connection,
    ctx: &ServerContext,
    st: &mut Session,
    mut computed: Computed,
) -> Result<(), String> {
    st.in_flight.remove(&uri_key(&computed.uri));
    let sample = computed.overlay_build;
    // Taken out before the publish moves `diags`; consumed only on the live path.
    let project_index = computed.project_index.take();
    let outcome = match st.buffers.current_version(&computed.uri) {
        // Buffer closed while the worker ran — drop the result (no stale publish).
        None => Ok(()),
        Some(cur) => {
            let live = cur == computed.version
                && computed.generation == st.project.generation
                && computed.epoch == current_epoch(st, &computed.uri);
            if live {
                // The cross-file cache write rides the liveness gate rather than
                // repeating it: a result that is fit to PUBLISH is by definition
                // the newest content, under the current project context, for a
                // still-open buffer — precisely the entry a same-URI hover wants,
                // and precisely the guarantee "hover agrees with the markers on
                // screen" needs. A superseded result is dropped here exactly as
                // its diagnostics are, so a stale index can never be cached.
                //
                // `project_index` is `Some` iff the overlay was on for this
                // dispatch, so a guard-off dispatch silently caches nothing.
                if let Some(index) = project_index {
                    st.crossfile.store(&computed.uri, index, computed.generation);
                }
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
    };
    outcome?;

    // Feed the scale guard the overlay rebuild this dispatch already paid for
    // (review N2). Done AFTER the result is handled, so a posture flip never
    // suppresses the (correct, overlay-computed) publish that produced the sample.
    //
    // **Only while the guard is ENABLED** (review B-2). A sample arriving once the
    // overlay is off necessarily comes from a dispatch that PREDATES the
    // disable-swap (a concurrent per-URI dispatch, or a buffer closed mid-flight),
    // so it describes a posture that no longer exists. Acting on one would also be
    // terminal: `ReEnabled` here would flip the guard on while `project.overlay` is
    // still `None`, and with no overlay no further sample is ever produced — the
    // session could never actually recover, while telling the user it had.
    // Recovery belongs where the overlay is being rebuilt anyway, in `invalidate`.
    if let Some(sample) = sample.filter(|_| st.guard.enabled) {
        let verdict = st.guard.record(sample, ctx.overlay_budget);
        // Read the count BEFORE the swap empties the overlay.
        let files = st.project.overlay.as_ref().map_or(0, |o| o.files.len());
        if let Some(msg) = overlay_guard_message(&verdict, files, sample, ctx.overlay_budget) {
            if verdict == GuardVerdict::Disabled {
                // Drop the held files — no overlay, no reason to hold them, so the
                // memory goes back. The generation bump inside `swap_project`
                // re-dispatches anything in flight under the new posture.
                let index = Arc::clone(&st.project.index);
                swap_project(ctx, st, index, None);
            }
            send_show_message(connection, MessageType::WARNING, msg)?;
        }
    }
    Ok(())
}

/// Build the `SourceIndex` one diagnostics dispatch analyses against (S4b).
///
/// With the overlay live: the project index rebuilt from tier 1's held files with
/// the buffer's file **REPLACED** by `ast` — the buffer's freshly-lowered content.
/// **Replacement, not addition, is non-negotiable** (mini-spec §Decision): a
/// project index carrying BOTH the on-disk and the buffer version of one file
/// would hold two competing method / return facts for the same class and could
/// resolve a name the user just renamed away, i.e. a WRONG type — a false
/// positive, which this project never trades for speed. A buffer whose path is
/// not among the project files (unsaved, outside `paths:`, or a non-`file:` URI)
/// is APPENDED instead, which is exactly the index `check` builds when that file
/// is added to the run.
///
/// With the overlay off (guard tripped / no project files): today's single-file
/// [`SourceIndex::build`], unchanged.
/// Returns the index plus, when the overlay was used, the rebuild timing — the
/// scale guard's sample. Measuring here is free: the dispatch builds this index
/// anyway to analyse the buffer, so the guard observes the very quantity it is
/// protecting, on every publish, at zero extra cost (review N2).
///
/// **The held-harvest slice.** This used to call `SourceIndex::build_project`,
/// which is `merge(asts.map(harvest))` — i.e. it re-harvested every project file
/// on every keystroke (42.8 ms of a 114.5 ms dispatch at 4 675 files, which is
/// what tripped the scale guard OFF at that scale). Tier 1 now HOLDS each file's
/// harvest, so a dispatch harvests exactly the one file whose content changed —
/// the dirty buffer — and calls `merge` directly. Nothing else moves: the same
/// harvests, over the same files, in the same order, into the same merge.
///
/// **The `Arc`** (cross-file cache slice). The merged index used to die on the
/// worker thread at the end of the dispatch that built it; it is now handed back
/// so a same-URI hover/completion can READ it instead of paying a rebuild. The
/// wrap is observationally inert — an `Arc<SourceIndex>` derefs to the same
/// `&SourceIndex` every analysis call took before — and the fallback branch is
/// wrapped too, purely so both branches have one type; only the overlay branch's
/// index is ever cached (see [`compute_diagnostics`]).
fn overlay_source_index(
    project: &ProjectContext,
    path: Option<&Path>,
    ast: &LoweredAst,
) -> (Arc<SourceIndex>, Option<Duration>) {
    let Some(overlay) = &project.overlay else {
        return (Arc::new(SourceIndex::build(ast, &project.index)), None);
    };
    // The timer starts HERE, before the buffer's harvest: the guard's budget is
    // "what one dispatch costs", and the buffer harvest is part of that.
    let t0 = Instant::now();
    // The one harvest a dispatch still pays. `SourceIndex::harvest` reads only its
    // AST and the frozen `CoreIndex` (#92), so this is exactly the harvest
    // `build_project` computed for the buffer's AST before.
    let buffer = SourceIndex::harvest(ast, &project.index);
    let mut files: Vec<(&Harvest, &LoweredAst)> = Vec::with_capacity(overlay.files.len() + 1);
    let mut replaced = false;
    for (p, held_ast, held_harvest) in &overlay.files {
        if path == Some(p.as_path()) {
            // REPLACE: the buffer's content supersedes the on-disk file — BOTH
            // halves of it. Swapping the AST while keeping the held harvest would
            // serve cross-file facts (classes, methods, constants) the buffer no
            // longer states, which is the double-registration hazard by another
            // route: a name the user just renamed away, still resolvable.
            files.push((&buffer, ast));
            replaced = true;
        } else {
            files.push((held_harvest.as_ref(), held_ast.as_ref()));
        }
    }
    if !replaced {
        files.push((&buffer, ast));
    }
    let source = SourceIndex::merge(&files, &project.index);
    // The timer stops on the MERGE, before the `Arc` wrap: the guard's budget is
    // the analysis work, and an `Arc::new` is one allocation of a moved value.
    let elapsed = t0.elapsed();
    (Arc::new(source), Some(elapsed))
}

/// The canonical filesystem path a `file:` document URI names, or `None` for a
/// non-`file:` URI or a path whose containing directory does not exist. Canonical
/// form is what makes the overlay's REPLACE lookup exact: tier 1 canonicalizes
/// every project file too, so symlinks and `.`/`..` segments cannot smuggle the
/// same file in twice under two spellings.
///
/// **The file itself need not exist.** `fs::canonicalize` requires the whole path
/// to resolve, so a buffer whose file was just deleted or renamed on disk (a
/// `git checkout`, a `git stash`, an IDE rename) would fail it — and returning
/// `None` there is NOT the same answer as for an untitled buffer: the tier-1
/// overlay still holds that path's stale on-disk AST, so
/// [`overlay_source_index`]'s append fallback would register the file TWICE (the
/// stale disk version alongside the buffer's), which is exactly the double
/// registration the REPLACE rule exists to prevent — a wrong type, i.e. a false
/// positive. So resolve the PARENT directory and re-attach the file name; only a
/// path whose parent is also unresolvable (a genuinely non-filesystem buffer) is
/// `None`, and only that case appends.
fn uri_to_canonical_path(uri: &Uri) -> Option<PathBuf> {
    let decoded = uri_decoded_path(uri)?;
    if let Ok(canonical) = std::fs::canonicalize(&decoded) {
        return Some(canonical);
    }
    // The file is gone (or not yet written) — canonicalize its directory instead,
    // which yields the SAME path tier 1 recorded while the file still existed. When
    // the DIRECTORY is gone too this returns `None`, and callers must treat that as
    // "not decidable here" rather than "no on-disk identity".
    let name = decoded.file_name()?;
    let parent = std::fs::canonicalize(decoded.parent()?).ok()?;
    Some(parent.join(name))
}

/// The literal filesystem path a `file:` URI spells, percent-decoded but NOT
/// resolved — so it survives a path whose directory no longer exists, which
/// [`uri_to_canonical_path`] cannot.
fn uri_decoded_path(uri: &Uri) -> Option<PathBuf> {
    file_uri_to_path(uri.as_str())
}

/// …the same decode, from a RAW URI string. [`requested_root`] reads workspace
/// URIs straight out of the `initialize` params (`serde_json`, not `lsp_types`),
/// and a second decoder there would be exactly the drift the `exclude:` parity
/// slice was written to prevent: the root's decode and the buffer's decode must
/// agree, or a buffer under the root can be spelled outside it. One rule, two
/// entry points.
///
/// `None` for anything that is not a `file:` URI — a virtual workspace
/// (`vscode-vfs:`, `untitled:`) has no local directory for rigor to analyse.
fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Skip the (empty or `localhost`) authority component: `file:///a/b` → `/a/b`.
    let start = rest.find('/')?;
    Some(PathBuf::from(percent_decode(&rest[start..])))
}

/// Percent-decode a URI path component (`%20` → a space). Invalid escapes are
/// passed through verbatim; the result is UTF-8-lossy, which is enough for the
/// canonicalize + compare the overlay does with it.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(b) = hex {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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

/// Run the analysis path over `text` and map the findings to LSP diagnostics.
/// Reuses the exact `check` pipeline (parse → lower → build a `SourceIndex` →
/// `analyze_with_source_and_folder`), plus the inline `# rigor:disable` and config
/// `disable:` suppression and the ADR-8 SeverityStamp, so the editor's inline
/// markers match `rigor check` on the same content. Panic-isolated (ADR-0016): a
/// malformed buffer that trips the parser yields no diagnostics, never a crash.
///
/// **The stage-1 head.** `check` never even reads a directory-expanded file the
/// `exclude:`/`BUILTIN_EXCLUDES` patterns cover (its `reject_excluded`), so it
/// reports no rows for it; the LSP applies the same gate to the open buffer
/// ([`ExcludeMatcher`]) and returns an EMPTY set — a publish that clears, not a
/// silent skip.
///
/// **The stage-3 tail.** `check`'s stage 3 ends by re-stamping each diagnostic's
/// severity from the profile + user + bleeding-edge overrides and DROPPING an
/// `:off` resolution ([`SeverityStamp::apply`]); the bleeding-edge selection also
/// gates the `static.value-use.void` collector. Both run here, in `check`'s order,
/// so a project on a non-default `severity_profile:` sees the same rule SET and
/// the same severities in the editor that its CI run reports.
///
/// **S4b — the cross-file overlay.** When tier 1 holds the project ASTs, the
/// `SourceIndex` is the PROJECT index rebuilt with this buffer's file swapped for
/// its freshly-lowered AST ([`overlay_source_index`]) — so the editor sees the
/// same cross-file facts `check` does, live, without a save. With the overlay off
/// (scale guard tripped, or an empty/absent project) it falls back to today's
/// single-file [`SourceIndex::build`]. `buf` carries the buffer's on-disk names —
/// all `None` for an unsaved/non-`file:` buffer.
///
/// Returns the diagnostics, the overlay rebuild timing (the scale guard's
/// sample, `None` when the overlay is off), and — for the cross-file cache — the
/// PROJECT index this dispatch analysed against, `Some` on exactly the same
/// condition as the timing. Handing the index back changes nothing about what is
/// computed or published: the third slot is dropped by every caller that does
/// not want it, and every early return (`exclude`d buffer, ERB template, parse
/// error, caught panic) yields `None` for it exactly as it already does for the
/// timing.
fn compute_diagnostics(
    project: &ProjectContext,
    buf: &BufferPaths,
    text: &str,
) -> (Vec<Diagnostic>, Option<Duration>, Option<Arc<SourceIndex>>) {
    // STAGE-1 PARITY, in `check`'s order (`main.rs`): `exclude:` FIRST —
    // settled by the expansion before `check` even reads a directory's file,
    // and mirrored here before the buffer is parsed — then the ERB-template
    // skip. An excluded buffer yields an EMPTY set rather than no publish at
    // all, so the caller's publish CLEARS any markers the editor is already
    // showing for it (the same empty-publish `didClose` uses).
    if project.exclude.excludes(buf, project.overlay.as_ref()) {
        return (Vec::new(), None, None);
    }
    let bytes = text.as_bytes().to_vec();
    // Skip ERB templates (matches `check` + the reference's ErbTemplateDetector):
    // Prism's error recovery over a `<%= … %>` template yields a garbage AST.
    // `check` runs the same `rigor_parse::looks_like_erb_template` on the file's
    // bytes; the LSP runs it on the BUFFER's, which is the same predicate over the
    // content the user is actually editing.
    if rigor_parse::looks_like_erb_template(&bytes) {
        return (Vec::new(), None, None);
    }
    let analysed = panic::catch_unwind(AssertUnwindSafe(|| {
        let result = parse(&bytes);
        // A buffer Prism could not parse gets no semantic diagnostics, matching
        // `check` (`main.rs` stage 1) and the reference, which returns its parse
        // diagnostics without ever reaching the typing pass. Mid-keystroke the
        // buffer is routinely unparseable; running the rules over Prism's
        // recovered AST publishes invented findings that vanish on the next
        // keystroke. `None` here clears the file's diagnostics, as `didClose`
        // and the ERB skip above already do.
        if result.errors().next().is_some() {
            return None;
        }
        let comments = comment_lines(&result, &bytes);
        // Issue #102: the buffer is the content of a file at a known path, so its
        // lowering takes that path's key — the same key tier 1 gave the on-disk
        // lowering this one REPLACES, and the same key `hover`/`completion` give
        // their own re-lowering of the buffer.
        let ast = lower_with_key(&result, document_file_key(buf.canonical.as_deref()));
        let (source, overlay_build) =
            overlay_source_index(project, buf.canonical.as_deref(), &ast);
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
        // `static.value-use.void` (ADR-100) — behind the `use-of-void-value`
        // bleeding-edge feature, under the SAME resolved-severity gate `check`
        // uses, and produced BEFORE suppression filtering like every check rule.
        if project.stamp.void_rule_active {
            diags.extend(rigor_rules::void_value_use_diagnostics(
                &ast,
                &mut interner,
                &project.index,
                &source,
            ));
        }
        // ADR-47 WD5 (upstream #627) — the dead arm of a decidable version guard
        // reports nothing. Same position as `check`'s stage 3: after every
        // type/flow rule, before `suppression.*` joins the list.
        let diags = rigor_rules::filter_dead_version_guard_arms(diags, &ast);
        // The cache's payload rides back beside the guard's sample and on the
        // SAME condition: a guard-off dispatch built the single-file index
        // hover/completion already build per request, so caching it would buy
        // nothing and cost staleness (probe §4 — this is the one gate that keeps
        // the fallback posture literally unchanged).
        let project_index = overlay_build.is_some().then(|| Arc::clone(&source));
        Some((diags, comments, overlay_build, project_index))
    }));

    let (mut diags, comments, overlay_build, project_index) = match analysed {
        Ok(Some(quad)) => quad,
        Ok(None) | Err(_) => return (Vec::new(), None, None),
    };
    // Suppression-marker surveillance, before `filter_suppressed` (self-suppressible).
    diags.extend(rigor_rules::suppression_marker_diagnostics(&comments));

    // Inline `# rigor:disable` suppression (same as `check`): key each diag on its
    // 1-based line, filter, then drop config-`disable:`d rules.
    let with_lines: Vec<(usize, rigor_rules::Diagnostic)> = diags
        .into_iter()
        .map(|d| (offset_to_position(text, d.start_offset).line as usize + 1, d))
        .collect();

    // COMPOSITION ORDER, verified against `main.rs`'s stage 3 (do not reorder):
    // rules → `suppression_marker_diagnostics` → `filter_suppressed` (inline
    // `# rigor:disable`) → config `disable:` → the ADR-8 SeverityStamp. The stamp
    // runs LAST because it is the only step that can also REWRITE a diagnostic; a
    // suppression that ran after it would be deciding on a re-stamped severity.
    // (`check` then applies the ADR-22 baseline after the stamp; the LSP has no
    // baseline — see the stage-3-parity note.)
    let out = filter_suppressed(with_lines, &comments)
        .into_iter()
        .filter(|(_, d)| !project.disable.suppresses(d.rule_id))
        .filter_map(|(_, mut d)| {
            project.stamp.apply(&mut d).then(|| to_lsp_diagnostic(text, &d))
        })
        .collect();
    (out, overlay_build, project_index)
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
///
/// **`cached`** is THIS URI's last-good project [`SourceIndex`]
/// ([`CrossFileCache`]), or `None`. On a hit the hover is answered from the
/// project index — a call whose class lives in another file finally types — at
/// the cost of an `Arc` read, never a rebuild. On a miss it is today's
/// single-file [`SourceIndex::build`], unchanged. The caller resolves the entry
/// (same-URI, current generation); this function never chooses which index it is
/// handed.
fn hover(
    project: &ProjectContext,
    buffers: &BufferTable,
    params: &HoverParams,
    cached: Option<&SourceIndex>,
) -> Option<Hover> {
    let pos = &params.text_document_position_params;
    let text = buffers.text(&pos.text_document.uri)?;
    let offset = position_to_offset(text, pos.position)?;
    // Issue #102: the same file key the dispatch's lowering carried, so a
    // constant this file assigns still folds when the answer comes from the
    // CACHED project index (which was merged against the worker's lowering of
    // this same buffer). With the old per-`lower()` counter the two disagreed
    // and `FOO : 5` degraded to `FOO : Dynamic[top]` on every cache hit.
    let key = document_file_key(uri_to_canonical_path(&pos.text_document.uri).as_deref());

    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let ast = lower_with_key(&parse(text.as_bytes()), key);
        let node_id = crate::type_of::locate_node(&ast, offset)?;
        // Deferred init, so the single-file index is not even BUILT on a hit.
        let single_file;
        let source: &SourceIndex = match cached {
            Some(project_index) => project_index,
            None => {
                single_file = SourceIndex::build(&ast, &project.index);
                &single_file
            }
        };
        let typer = Typer::with_source(&project.index, source);
        let mut interner = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut interner);
        let ty = typer.type_of(&ast, node_id, &env, &mut interner);
        let (start, end) = ast.get(node_id).span();
        let type_render = crate::type_of::render_type(&interner, &project.index, source, ty);

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

/// The CONSTANT-shaped twin of [`COMPLETION_STUB`], spliced after `::` when the
/// cursor is in a namespace position. It MUST start with an uppercase letter:
/// `Foo::rigorCompletionHole` parses as a method call, not a constant path.
const COMPLETION_STUB_CONST: &str = "RigorCompletionHole";

/// Answer `textDocument/completion`: if the cursor sits after a `.`/`::` member
/// access, resolve the receiver's type and return its callable methods. Returns
/// `None` (a null completion) when the cursor isn't in a member-access context,
/// the buffer is unknown, or the receiver type is unresolved.
///
/// Robust to incomplete input via **placeholder injection**: a stub name is
/// spliced in right after the separator (replacing any half-typed name), so the
/// parser yields a well-formed node regardless of what the user has typed. The
/// half-typed prefix is intentionally dropped — the client filters by it.
///
/// Two shapes share that machinery, split exactly where Ruby splits them — on
/// the CASE of the name being typed after `::` (LSP v4):
///
/// - `Foo::|` / `Foo::Ba|` — a NAMESPACE position. Yields the nested
///   constants (classes/modules) under `Foo`.
/// - `x.|`, `x.up|`, `Foo::ba|` — a METHOD position. The receiver node is typed
///   with the same `Typer` `hover`/`check` use; its class drives instance- vs
///   singleton-method enumeration.
///
/// The reference reaches the same split differently: it feeds the raw buffer to
/// Prism first and dispatches on the located node's class (`ConstantPathNode` ⇒
/// constants, `CallNode` ⇒ methods), falling back to an uppercase / lowercase
/// sentinel only when the buffer does not parse. Since Ruby's own rule for
/// "constant or method call after `::`" IS the first character's case, deciding
/// on the prefix directly is the same decision without the double parse.
///
/// **`cached`** is THIS URI's last-good project [`SourceIndex`], exactly as for
/// [`hover`]: on a hit the receiver is typed against the project index (so a
/// receiver whose class — or whose class's inferred method return — lives in
/// another file completes), on a miss against today's single-file index. The
/// namespace branch uses it too, to UNION the project's own nested constants
/// into the RBS children.
fn completion(
    project: &ProjectContext,
    buffers: &BufferTable,
    params: &CompletionParams,
    cached: Option<&SourceIndex>,
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
    let scope_sep = ident_start >= 2 && &text[ident_start - 2..ident_start] == "::";
    let dot_sep = ident_start >= 1
        && bytes[ident_start - 1] == b'.'
        && !(ident_start >= 2 && bytes[ident_start - 2] == b'.');
    if !scope_sep && !dot_sep {
        return None; // not a member-access completion context.
    }
    // `Foo::` with nothing typed yet, or an uppercase-initial partial, is a
    // constant the user is writing; a lowercase-initial one is a class-method
    // call (`Foo::parse`), which stays on the method path.
    if scope_sep && !bytes[ident_start..offset].first().is_some_and(u8::is_ascii_lowercase) {
        return namespace_completion(project, text, ident_start, offset, cached);
    }
    let stub_at = ident_start; // where the stub name begins (right after the sep).

    // Splice the stub in after the separator, dropping any half-typed name.
    let synth = format!("{}{}{}", &text[..ident_start], COMPLETION_STUB, &text[offset..]);
    // Issue #102, exactly as in `hover`: the synthesized text is still THIS
    // file's content (one stub name spliced in), so its lowering takes this
    // file's key and a receiver that is a same-file literal constant completes
    // on a cache hit instead of degrading to `Dynamic`.
    let key = document_file_key(uri_to_canonical_path(&tdp.text_document.uri).as_deref());

    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let ast = lower_with_key(&parse(synth.as_bytes()), key);
        // Our injected call is the unique `Call` whose method-name token starts
        // exactly at `stub_at`.
        let receiver = ast.iter().find_map(|(_, n)| match n {
            Node::Call { receiver, message_span, .. } if message_span.0 == stub_at => Some(*receiver),
            _ => None,
        })??;
        // Deferred init, so the single-file index is not even BUILT on a hit.
        let single_file;
        let source: &SourceIndex = match cached {
            Some(project_index) => project_index,
            None => {
                single_file = SourceIndex::build(&ast, &project.index);
                &single_file
            }
        };
        let typer = Typer::with_source(&project.index, source);
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

/// LSP v4 — `Foo::|` namespace completion: the nested constants (classes and
/// modules) declared under the namespace the cursor is qualifying. Returns
/// `None` (a null completion) when the parent isn't a constant path, or names
/// no known namespace with children.
///
/// The parent FQN comes from the AST, not a backwards text scan, so an
/// expression-shaped left side (`foo()::Bar`, `[1, 2]::Bar`) resolves to no
/// constant path and correctly yields nothing.
///
/// The enumeration surface is the RBS one (core / stdlib / plugins / project
/// `sig/`) UNIONED, on a cross-file cache hit, with the classes and modules the
/// PROJECT'S OWN SOURCE declares under the same namespace
/// ([`SourceIndex::namespace_children`]) — the LSP-v4 note's withheld
/// "buffer-local constants after `::`" item, closed now that a project index is
/// available without a per-request rebuild. With no cache entry (no dispatch
/// yet, a stale generation, the overlay off) the answer is RBS-only, exactly as
/// before.
///
/// **The RBS entry wins a kind conflict.** The two surfaces can disagree about
/// whether a name is a class or a module — a project that reopens `Process` and
/// writes `module Status` inside it says MODULE where core RBS says CLASS. RBS
/// is the declaration of record for a name it knows, so it is inserted LAST and
/// overwrites; the union still offers the name exactly once.
fn namespace_completion(
    project: &ProjectContext,
    text: &str,
    ident_start: usize,
    offset: usize,
    cached: Option<&SourceIndex>,
) -> Option<CompletionResponse> {
    let synth = format!("{}{}{}", &text[..ident_start], COMPLETION_STUB_CONST, &text[offset..]);
    let suffix = format!("::{COMPLETION_STUB_CONST}");

    let parent = panic::catch_unwind(AssertUnwindSafe(|| {
        // A constant path lowers to a single `ConstantRead` carrying the dotted
        // name (`Foo::Bar::<stub>`), so the enclosing namespace is the name with
        // the stub segment removed. A stub with no `::` before it is `::Stub`,
        // a TOP-LEVEL reference with no parent — nothing to enumerate, which is
        // also what the reference returns for a parent-less path.
        lower(&parse(synth.as_bytes())).iter().find_map(|(_, n)| match n {
            Node::ConstantRead { name, .. } => {
                name.strip_suffix(suffix.as_str()).map(str::to_string)
            }
            _ => None,
        })
    }))
    .ok()??;

    // Name-keyed so the union is deduplicated and stays in the SAME name-sorted
    // order the RBS-only path already produced (both accessors are `BTreeMap`
    // collects, so an RBS-only namespace yields a byte-identical list).
    let mut children: std::collections::BTreeMap<&str, bool> = std::collections::BTreeMap::new();
    if let Some(source) = cached {
        for (name, is_module) in source.namespace_children(&parent) {
            children.insert(name, is_module);
        }
    }
    // LAST, so an RBS declaration overrides the project's kind on a conflict.
    for (name, is_module) in project.index.namespace_children(&parent) {
        children.insert(name, is_module);
    }
    if children.is_empty() {
        return None;
    }
    let items: Vec<CompletionItem> = children
        .into_iter()
        .map(|(name, is_module)| CompletionItem {
            label: name.to_string(),
            // The reference labels every child `Class` with a "may distinguish
            // Module later" note; the qualified registry already knows which is
            // which, so render it. Same SET, more accurate icon.
            kind: Some(if is_module {
                CompletionItemKind::MODULE
            } else {
                CompletionItemKind::CLASS
            }),
            detail: Some(format!("{parent}::{name}")),
            ..Default::default()
        })
        .collect();
    Some(CompletionResponse::Array(items))
}

/// Resolve the receiver type to the set of callable method names: singleton
/// (class-object) methods for a `Type::Singleton` receiver (a bare class
/// constant), the per-arm INTERSECTION for a union, else instance methods on the
/// receiver's concrete core class. Empty when the class isn't resolvable (a
/// `Dynamic`/project/unknown receiver ⇒ no completion, never a guess).
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
    // LSP v4 item 3 — a union receiver offers only what dispatches on EVERY arm.
    // Conservative in the direction that matters for a popup: `s.upcase` must not
    // be suggested on a `String | Integer`, where half the values raise. Arms the
    // index cannot resolve are SKIPPED rather than treated as the empty set,
    // matching the reference's `filter_map` in `intersect_member_methods` — an
    // unknown arm is no information, not a veto.
    if let Type::Union(members) = interner.get(ty) {
        let sets: Vec<Vec<&'static str>> = members
            .iter()
            .map(|&m| method_names_for(index, typer, interner, m))
            .filter(|s| !s.is_empty())
            .collect();
        let Some((first, rest)) = sets.split_first() else {
            return Vec::new();
        };
        // Each arm's set is already sorted + deduped, so the filtered result is.
        return first.iter().copied().filter(|n| rest.iter().all(|s| s.contains(n))).collect();
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
mod tests;
