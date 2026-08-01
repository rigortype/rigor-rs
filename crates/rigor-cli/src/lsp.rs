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
use rigor_infer::{SourceIndex, Typer};
use rigor_parse::{comment_lines, lower, parse, LoweredAst, Node};
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

    // N4 (deferred, recorded in the S4b note): the root is the server's cwd — the
    // `initialize` params' `rootUri` / `workspaceFolders` are not consulted yet, so
    // a server launched from the wrong directory discovers zero project files.
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
        guard.record(build.build_project, OVERLAY_BUILD_BUDGET_DEFAULT);
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
    /// The **cross-file overlay substrate** (S4b): every project `.rb` file's
    /// canonical path + its held `LoweredAst`, harvested at build time. A
    /// diagnostics dispatch rebuilds the project `SourceIndex` from these ASTs
    /// with the dirty buffer's file's AST REPLACED by the buffer's freshly-lowered
    /// one ([`overlay_source_index`]), so LSP diagnostics see the same cross-file
    /// context `check` does. `None` ⇒ the overlay is OFF (the scale guard tripped,
    /// or no project files were found) and diagnostics fall back to today's
    /// single-file [`SourceIndex::build`].
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

/// The config `exclude:` gate for the OPEN BUFFER — the LSP's counterpart of
/// `check`'s STAGE-1 file filter (`main.rs`: `if cfg.is_excluded(path) { return
/// Stage1::Excluded; }`, the very first thing stage 1 does, before the file is
/// even read).
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
///    are ALL excluded — the invariant again, this time computed.
///
/// The spelling half still matters because `exclude:` patterns are matched against
/// the path string as `check` SPELLS it, not an absolute canonical path: bare
/// `check` expands each `paths:` root (`expand_check_paths` → `collect_rb_files`,
/// building `<root>/<rel>` by `Path::join`), so with the production root `.` the
/// matched string is `lib/sub.rb`, and under `paths: ["."]` it is `./lib/sub.rb`.
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
    /// `exclude:` verbatim. Matched by [`crate::config::matches_exclude`], the same
    /// entry point `Config::is_excluded` (and so `check`) calls — never a second
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
            paths: cfg.paths.clone(),
            patterns: cfg.exclude.clone(),
        }
    }

    /// Whether this buffer is `exclude:`d — i.e. whether `check` would report
    /// nothing for it. See the type docs for the two tiers and the invariant.
    fn excludes(&self, buf: &BufferPaths, overlay: Option<&ProjectFiles>) -> bool {
        if self.patterns.is_empty() {
            return false; // the overwhelmingly common case: no work at all.
        }
        // TIER 1 — discovery membership. The overlay IS the post-`exclude:` set.
        if let (Some(canonical), Some(overlay)) = (buf.canonical.as_deref(), overlay) {
            if overlay.files.iter().any(|(p, _)| p == canonical) {
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
        discovery_spellings(&self.root, &self.paths)
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
        let before = out.len();
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
                // as is); then `rel` is empty and the spelling is the root itself.
                let spelled =
                    if rel.as_os_str().is_empty() { base.clone() } else { base.join(rel) };
                out.push(spelled.to_string_lossy().into_owned());
            }
        }
        if out.len() > before {
            return; // named by at least one configured root.
        }
        // Outside every `paths:` root: the only run that reports on this file is an
        // explicit `rigor check <that file>` from the project root.
        let Ok(canonical_root) = std::fs::canonicalize(&self.root) else { return };
        if let Ok(rel) = path.strip_prefix(&canonical_root) {
            out.push(join_root(&self.root, &rel.to_string_lossy()).to_string_lossy().into_owned());
        }
    }
}

/// The tier-1 held project ASTs backing the S4b cross-file overlay: one entry per
/// analysable project `.rb` file, `(canonical path, lowered AST)`, in the same
/// order `check`'s stage 1 produces them.
///
/// `Sync` by construction (`LoweredAst` is a plain owned arena), so an `Arc<
/// ProjectContext>` carrying it is shared across the rayon workers exactly as the
/// `CoreIndex` already is.
///
/// Each AST is behind its own `Arc` so the WHOLE table is cheap to clone
/// (`Vec<(PathBuf, Arc<_>)>` — pointer copies, no AST copies, ~100 µs at 4 675
/// files vs the ~190 MB a deep clone would move). That is what makes the
/// single-file re-harvest ([`reharvest_sources`]) affordable: replace one entry's
/// `Arc` and swap the table in, instead of re-parsing the project.
#[derive(Clone)]
struct ProjectFiles {
    files: Vec<(PathBuf, Arc<LoweredAst>)>,
}

/// One tier-1 overlay build's outcome + its MEASURED cost (S4b). The build does
/// NOT itself decide whether the overlay stays on — that is [`OverlayGuard`]'s
/// job, fed by this `build_project` sample.
struct OverlayBuild {
    files: ProjectFiles,
    /// How many project files were parsed + lowered (reported even when the guard
    /// trips, so the disclosure can name the scale).
    file_count: usize,
    /// Stage 1 equivalent: read + parse + lower the whole project.
    parse_lower: Duration,
    /// Stage 2 equivalent: the `SourceIndex::build_project` call the guard times.
    build_project: Duration,
}

/// The `build_project` budget for the S4b cross-file overlay (mini-spec §"Scale
/// guard"). A per-dispatch overlay rebuild pays this cost on every debounced
/// publish, so it must leave headroom under ADR-0029's 250 ms p50
/// `didChange`→publish target. Injectable via [`ServerContext::overlay_budget`] so
/// the guard test can force a trip.
const OVERLAY_BUILD_BUDGET_DEFAULT: Duration = Duration::from_millis(100);

/// How many CONSECUTIVE over-budget samples DISABLE the overlay (review N2). One
/// sample is not a classifier: measured on an idle machine at 3 117 files, six
/// consecutive `build_project` runs were 93.9 / 93.7 / 90.9 / **106.2** / 94.7 /
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
/// `build_project` while the overlay is ON (free: the dispatch builds it to
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
    /// The overlay just turned OFF: drop the held ASTs and disclose.
    Disabled,
    /// The overlay just turned back ON: keep the freshly-built ASTs and disclose.
    ReEnabled,
}

impl OverlayGuard {
    fn new() -> Self {
        Self { enabled: true, over: 0 }
    }

    /// Feed one `build_project` timing to the guard and report any posture flip.
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
/// minus `exclude:` and ERB templates), parse + lower them all, then run the
/// `SourceIndex::build_project` the scale guard times.
///
/// The returned index itself is DISCARDED: a dispatch always rebuilds with the
/// buffer's AST swapped in, so a stored copy would only cost memory. What tier 1
/// keeps is the ASTs (the rebuild input) and the timing (the guard input).
///
/// Runs on the loop thread — at startup, and inside the SYNCHRONOUS [`invalidate`]
/// (S4's decision: STRUCTURAL invalidations are rare, so an inline rebuild is
/// acceptable; a plain `.rb` save no longer comes here at all — see
/// [`reharvest_sources`]). Parse+lower is rayon-parallel exactly like `check`'s
/// stage 1.
fn build_overlay(root: &Path, cfg: &Config, index: &CoreIndex) -> OverlayBuild {
    let paths = project_files(root, cfg);
    let t0 = Instant::now();
    // Stage-1 equivalent: read + parse + lower, file-parallel, panic-isolated per
    // file (ADR-0016) — an unreadable or parser-tripping file is simply omitted
    // from the project index, never a crash and never a diagnostic here (the LSP
    // only reports on OPEN buffers).
    let files: Vec<(PathBuf, Arc<LoweredAst>)> = paths
        .par_iter()
        .filter_map(|path| {
            // Canonicalize so a buffer URI's path (also canonicalized) matches this
            // entry exactly — symlinks and `.`/`..` segments would otherwise defeat
            // the REPLACE lookup and silently double-register the file.
            let canonical = std::fs::canonicalize(path).ok()?;
            Some((canonical, Arc::new(harvest_one(path)?)))
        })
        .collect();
    let parse_lower = t0.elapsed();

    let t1 = Instant::now();
    let refs: Vec<&LoweredAst> = files.iter().map(|(_, a)| a.as_ref()).collect();
    let project_index = SourceIndex::build_project(&refs, index);
    let build_project = t1.elapsed();
    // Deliberately discarded (AFTER the measurement, so the timing is the BUILD,
    // not the build + teardown): every dispatch rebuilds with the buffer's AST
    // swapped in, so keeping this copy would only cost memory. Its purpose here is
    // to be TIMED — it is the quantity the scale guard is defined on.
    drop(project_index);

    let file_count = files.len();
    OverlayBuild { files: ProjectFiles { files }, file_count, parse_lower, build_project }
}

/// Read + lower ONE project file the way [`build_overlay`] does: ERB templates are
/// skipped (Prism's recovery over `<%= … %>` yields a garbage AST — matching
/// `check`), and a read error or a parser panic yields `None` rather than a crash
/// (ADR-0016). `None` therefore means "this file contributes nothing to the
/// project index", which is also the right answer for a file that was deleted.
fn harvest_one(path: &str) -> Option<LoweredAst> {
    let source = std::fs::read(path).ok()?;
    if rigor_parse::looks_like_erb_template(&source) {
        return None;
    }
    panic::catch_unwind(AssertUnwindSafe(|| {
        let result = parse(&source);
        // A file with parse errors contributes nothing to the index, the same
        // answer `check` gives it (`main.rs` stage 1) and the reference's
        // dependency walker gives it: Prism's recovery invents bindings, so
        // indexing the wreckage is worse than not indexing the file.
        (result.errors().next().is_none()).then(|| lower(&result))
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

/// The project's analysable `.rb` files, in bare-`check` order (ADR-0040): each
/// configured `paths:` root expanded recursively (a directory's files sorted,
/// roots concatenated in config order), minus the config `exclude:` patterns.
/// `root` is the project root — `.` in production (so the produced path strings
/// are byte-identical to what `check` matches `exclude:` against); a temp dir in
/// tests, which is what makes the overlay testable without mutating the process
/// cwd.
fn project_files(root: &Path, cfg: &Config) -> Vec<String> {
    let mut out = discovery_spellings(root, &cfg.paths);
    out.retain(|p| !cfg.is_excluded(p));
    out
}

/// Bare-`check` discovery BEFORE `exclude:` — every path string stage 1 would be
/// handed. Split out of [`project_files`] so [`ExcludeMatcher`] can consult the
/// real walk (rather than a second re-derivation of it) when it needs to know
/// whether ANOTHER spelling of one file survives the patterns.
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
        "rigor lsp timing: overlay files={} parse+lower={:.1}ms build_project={:.1}ms overlay={}",
        build.file_count,
        ms(build.parse_lower),
        ms(build.build_project),
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
    project_root: PathBuf,
    /// The tier-1 `build_project` scale-guard budget (S4b). Production is
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
    /// How long this dispatch's cross-file overlay `SourceIndex::build_project`
    /// took, or `None` when the overlay was off (nothing was built). The scale
    /// guard's sample (review N2) — a measurement of the work the dispatch did
    /// anyway, carried back to the loop thread which owns the guard state.
    overlay_build: Option<Duration>,
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
/// re-times its `build_project`, feeding the sample to the hysteresis
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
                    "rigor: .rigor.yml reloaded — the earlier error is resolved".to_string(),
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

/// Read `<root>/.rigor.yml` into the session's answer for "what is the config".
/// `Ok` = usable (parsed, or the defaults because there is no file); `Err(reason)`
/// = the file is THERE but unusable, and the caller must decide what to serve
/// instead — [`reload_config`] keeps the last good config, startup falls back to
/// defaults because it has none.
fn read_project_config(root: &Path) -> Result<Config, String> {
    match Config::read(&root.join(".rigor.yml")) {
        crate::config::ConfigRead::Parsed(cfg) => Ok(*cfg),
        // No file is a valid configuration — the defaults, exactly as a project
        // that never wrote one gets. Reloading to defaults after a DELETE is
        // correct for the same reason.
        crate::config::ConfigRead::Absent(_) => Ok(Config::default()),
        crate::config::ConfigRead::Unreadable(e) | crate::config::ConfigRead::Malformed(e) => {
            Err(e)
        }
    }
}

/// The user-facing text for a `.rigor.yml` that will not parse. Names WHICH
/// config is now in force, because that is the question a user staring at
/// unexpected markers is really asking — and the answer differs by when it broke:
/// a reload keeps the last good config, but a session that booted on a broken
/// file never had one and is running on defaults.
fn config_broken_message(reason: &str) -> String {
    format!(
        "rigor: .rigor.yml could not be read ({reason}) — keeping the last good \
         configuration; fix and save the file to reload it"
    )
}

/// …the startup variant, where there is no last good configuration to keep.
fn config_broken_at_startup_message(reason: &str) -> String {
    format!(
        "rigor: .rigor.yml could not be read ({reason}) — analyzing with DEFAULT \
         settings; fix and save the file to reload it"
    )
}

/// Re-harvest the WHOLE overlay against `index`, feed the `build_project` timing
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
        let verdict = st.guard.record(build.build_project, ctx.overlay_budget);
        overlay_guard_message(&verdict, build.file_count, build.build_project, ctx.overlay_budget)
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
}

/// Re-harvest ONLY the changed project `.rb` files' AST entries (review N3).
///
/// S4 approved a synchronous `invalidate` when it was a `CoreIndex` rebuild on a
/// RARE trigger. S4b made that ~20× more expensive (it re-reads, re-parses and
/// re-lowers the whole project — ~200 ms at 3 117 files) while the trigger became
/// EVERY source save, on the thread that owes hover/completion a <100 ms p95. The
/// fix is to touch what actually changed: the held table is
/// `Vec<(PathBuf, Arc<LoweredAst>)>`, so replacing or removing one entry is a
/// cheap clone + one file's parse — sub-millisecond regardless of project size.
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
///   matters to `build_project`'s multi-pass harvest). A held entry whose file is
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
                    .filter(|(_, (q, _))| q == p)
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
        } else if let Some(ast) = harvest_one(&path.to_string_lossy()) {
            let ast = Arc::new(ast);
            for i in &held {
                files.files[*i].1 = Arc::clone(&ast);
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

/// How a `workspace/didChangeWatchedFiles` payload affects tier 1 (review N3).
/// The two relevant kinds cost VERY different amounts, so they are dispatched
/// differently rather than both funnelled into a full rebuild.
#[derive(PartialEq, Eq, Debug)]
enum WatchedChange {
    /// Nothing on the invalidation surface — no work at all.
    None,
    /// Project `*.rb` saves: re-harvest exactly these files' AST entries.
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
        let (diags, overlay_build) = panic::catch_unwind(AssertUnwindSafe(|| {
            gate(version, generation); // test seam: may block (hold mid-flight) or panic.
            compute_diagnostics(&project, &paths, &text)
        }))
        .unwrap_or_default();
        // Always send exactly one result (even empty on a caught panic), so the
        // loop's in-flight tracking for this URI clears. `Err` = loop gone (shutdown).
        let _ = tx.send(Computed { uri, version, generation, epoch, diags, overlay_build });
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
    let sample = computed.overlay_build;
    let outcome = match st.buffers.current_version(&computed.uri) {
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
                // Drop the held ASTs — no overlay, no reason to hold them, so the
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
/// With the overlay live: the project index rebuilt from tier 1's held ASTs with
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
/// Returns the index plus, when the overlay was used, the `build_project` timing —
/// the scale guard's sample. Measuring here is free: the dispatch builds this
/// index anyway to analyse the buffer, so the guard observes the very quantity it
/// is protecting, on every publish, at zero extra cost (review N2).
fn overlay_source_index(
    project: &ProjectContext,
    path: Option<&Path>,
    ast: &LoweredAst,
) -> (SourceIndex, Option<Duration>) {
    let Some(overlay) = &project.overlay else {
        return (SourceIndex::build(ast, &project.index), None);
    };
    let mut refs: Vec<&LoweredAst> = Vec::with_capacity(overlay.files.len() + 1);
    let mut replaced = false;
    for (p, held) in &overlay.files {
        if path == Some(p.as_path()) {
            refs.push(ast); // REPLACE: the buffer's content supersedes the on-disk file.
            replaced = true;
        } else {
            refs.push(held);
        }
    }
    if !replaced {
        refs.push(ast);
    }
    let t0 = Instant::now();
    let source = SourceIndex::build_project(&refs, &project.index);
    (source, Some(t0.elapsed()))
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
    let rest = uri.as_str().strip_prefix("file://")?;
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
/// **The stage-1 head.** `check` never even reads a file the config `exclude:`
/// patterns cover, so it reports no rows for it; the LSP applies the same gate to
/// the open buffer ([`ExcludeMatcher`]) and returns an EMPTY set — a publish that
/// clears, not a silent skip.
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
/// Returns the diagnostics plus the overlay `build_project` timing (the scale
/// guard's sample, `None` when the overlay is off).
fn compute_diagnostics(
    project: &ProjectContext,
    buf: &BufferPaths,
    text: &str,
) -> (Vec<Diagnostic>, Option<Duration>) {
    // STAGE-1 PARITY, in `check`'s order (`main.rs`): config `exclude:` FIRST —
    // before the file is even read there, before the buffer is parsed here — then
    // the ERB-template skip. An excluded buffer yields an EMPTY set rather than no
    // publish at all, so the caller's publish CLEARS any markers the editor is
    // already showing for it (the same empty-publish `didClose` uses).
    if project.exclude.excludes(buf, project.overlay.as_ref()) {
        return (Vec::new(), None);
    }
    let bytes = text.as_bytes().to_vec();
    // Skip ERB templates (matches `check` + the reference's ErbTemplateDetector):
    // Prism's error recovery over a `<%= … %>` template yields a garbage AST.
    // `check` runs the same `rigor_parse::looks_like_erb_template` on the file's
    // bytes; the LSP runs it on the BUFFER's, which is the same predicate over the
    // content the user is actually editing.
    if rigor_parse::looks_like_erb_template(&bytes) {
        return (Vec::new(), None);
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
        let ast = lower(&result);
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
        Some((diags, comments, overlay_build))
    }));

    let (mut diags, comments, overlay_build) = match analysed {
        Ok(Some(triple)) => triple,
        Ok(None) | Err(_) => return (Vec::new(), None),
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
    (out, overlay_build)
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
        return namespace_completion(project, text, ident_start, offset);
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

/// LSP v4 — `Foo::|` namespace completion: the nested constants (classes and
/// modules) declared under the namespace the cursor is qualifying. Returns
/// `None` (a null completion) when the parent isn't a constant path, or names
/// no known namespace with children.
///
/// The parent FQN comes from the AST, not a backwards text scan, so an
/// expression-shaped left side (`foo()::Bar`, `[1, 2]::Bar`) resolves to no
/// constant path and correctly yields nothing.
///
/// Like the reference's `enumerate_constant_children`, this sees only the RBS
/// surface (core / stdlib / plugins / project `sig/`) — a class defined in the
/// edited buffer is NOT offered. That is the S4b "hover / completion keep the
/// single-file index" carve-out and the reference's own scope, not an oversight.
fn namespace_completion(
    project: &ProjectContext,
    text: &str,
    ident_start: usize,
    offset: usize,
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

    let children = project.index.namespace_children(&parent);
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
mod tests {
    use super::*;

    /// A minimal tier-1 project context (empty core index, no sidecar, generation
    /// 0) for the pure `compute_diagnostics` / `hover` / `completion` unit tests.
    fn project() -> ProjectContext {
        project_with_config(&Config::default())
    }

    /// The same minimal context, built from an explicit config — the seam the
    /// stage-3 stamp unit tests drive (`severity_profile:` / `severity_overrides:`
    /// / `bleeding_edge:` all reach `compute_diagnostics` through here).
    fn project_with_config(cfg: &Config) -> ProjectContext {
        project_with_config_rooted(cfg, Path::new("/nonexistent-rigor-lsp-test-root"))
    }

    /// …and rooted at an explicit project root, which is what the `exclude:` gate
    /// needs: the matcher re-spells a buffer path relative to the configured roots,
    /// so a test that drives it must name a real on-disk root.
    fn project_with_config_rooted(cfg: &Config, root: &Path) -> ProjectContext {
        ProjectContext {
            generation: 0,
            index: Arc::new(CoreIndex::new()),
            disable: cfg.disable_matcher(),
            folder: None,
            stamp: SeverityStamp::from_config(cfg),
            exclude: ExcludeMatcher::from_config(root, cfg),
            overlay: None,
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
        let (diags, _) = compute_diagnostics(&project(), &BufferPaths::default(), "x = \"hi\"\nx.lenght\n");
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
            compute_diagnostics(&project(), &BufferPaths::default(), "x = \"hi\"\nx.lenght # rigor:disable undefined-method\n").0;
        assert!(diags.is_empty(), "inline disable suppresses the diagnostic");
    }

    #[test]
    fn diagnostics_clean_source_is_empty() {
        let (diags, _) = compute_diagnostics(&project(), &BufferPaths::default(), "x = \"hi\"\nx.upcase\n");
        assert!(diags.is_empty());
    }

    // ---------------------------------------------------------------------
    // Stage-3 parity tail (ADR-8 SeverityStamp + the bleeding-edge gate).
    // The end-to-end LSP-vs-`check` equalities live in
    // `tests/lsp_check_parity.rs`; these pin the unit-level contract.
    // ---------------------------------------------------------------------

    /// A `severity_overrides:` entry resolving to `off` removes the diagnostic
    /// ENTIRELY — the presence mismatch the pre-stamp LSP had (`check` drops it,
    /// the editor still published a marker).
    #[test]
    fn stage3_stamp_drops_an_off_resolution() {
        let cfg = Config::parse_or_warn(
            "severity_overrides:\n  call.undefined-method: off\n",
            "test",
        );
        let (diags, _) =
            compute_diagnostics(&project_with_config(&cfg), &BufferPaths::default(), "x = \"hi\"\nx.lenght\n");
        assert!(diags.is_empty(), "an `off` resolution is DROPPED, not merely downgraded");
    }

    /// A non-`off` resolution re-stamps the published severity (here authored
    /// `error` → `info`), so the editor shows the project's configured level.
    #[test]
    fn stage3_stamp_restamps_a_resolved_severity() {
        let cfg = Config::parse_or_warn(
            "severity_overrides:\n  call.undefined-method: info\n",
            "test",
        );
        let (diags, _) =
            compute_diagnostics(&project_with_config(&cfg), &BufferPaths::default(), "x = \"hi\"\nx.lenght\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].severity,
            Some(DiagnosticSeverity::INFORMATION),
            "the published severity is the RESOLVED one, not the authored `error`"
        );
    }

    /// A FAMILY override reaches the rule too (`severity::resolve`'s exact-id →
    /// family fallback), so the LSP honours `call: off` exactly as `check` does.
    #[test]
    fn stage3_stamp_honours_a_family_override() {
        let cfg = Config::parse_or_warn("severity_overrides:\n  call: off\n", "test");
        let (diags, _) =
            compute_diagnostics(&project_with_config(&cfg), &BufferPaths::default(), "x = \"hi\"\nx.lenght\n");
        assert!(diags.is_empty(), "the `call` family override covers call.undefined-method");
    }

    /// Acceptance 3: the `internal-error` sentinel BYPASSES the stamp. Even a
    /// config that names it explicitly cannot silence a per-file panic (the
    /// reference's `rule.nil?` short-circuit).
    #[test]
    fn stage3_stamp_never_silences_internal_error() {
        let cfg = Config::parse_or_warn(
            "severity_overrides:\n  internal-error: off\n  call.undefined-method: off\n",
            "test",
        );
        let stamp = SeverityStamp::from_config(&cfg);

        let mut panic_diag = rigor_rules::Diagnostic {
            rule_id: "internal-error",
            start_offset: 0,
            end_offset: 0,
            message: "internal panic: boom".to_string(),
            severity: Severity::Error,
            source_family: "builtin",
            receiver_type: None,
            method_name: None,
        };
        assert!(stamp.apply(&mut panic_diag), "internal-error survives an `off` config");
        assert_eq!(panic_diag.severity, Severity::Error, "and is not re-stamped either");

        // The control: an ordinary rule under the SAME config IS dropped, so the
        // survival above is the bypass and not an inert override.
        let mut ordinary = rigor_rules::Diagnostic {
            rule_id: "call.undefined-method",
            ..panic_diag.clone()
        };
        assert!(!stamp.apply(&mut ordinary), "an ordinary rule under the same config drops");
    }

    /// The `static.value-use.void` activation gate is `check`'s: off by default
    /// (every shipped profile has it `:off`), promoted by the `use-of-void-value`
    /// bleeding-edge feature, and resurrectable by a user override alone.
    #[test]
    fn stage3_void_rule_gate_matches_check() {
        let gate = |yaml: &str| SeverityStamp::from_config(&Config::parse_or_warn(yaml, "test")).void_rule_active;
        assert!(!gate(""), "off by default");
        assert!(!gate("bleeding_edge: false\n"));
        assert!(gate("bleeding_edge: true\n"), "`all` activates the feature");
        assert!(gate("bleeding_edge:\n  - use-of-void-value\n"));
        assert!(!gate("bleeding_edge:\n  - some-other-feature\n"));
        assert!(
            gate("severity_overrides:\n  static.value-use.void: warning\n"),
            "a user override alone resurrects the rule (it outranks the profile table)"
        );
        assert!(
            !gate("bleeding_edge: true\nseverity_overrides:\n  static.value-use.void: off\n"),
            "and a user `off` outranks the bleeding-edge promotion"
        );
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

    /// LSP v4 item 1: `Process::` offers the NESTED CONSTANTS, not the singleton
    /// methods it used to return.
    #[test]
    fn completion_namespace_children_on_scope_operator() {
        let labels = complete("Process::\n", 0, 9);
        assert!(labels.contains(&"Status".to_string()), "nested class: {labels:?}");
        assert!(labels.contains(&"UID".to_string()), "nested module: {labels:?}");
        assert!(!labels.contains(&"wait".to_string()), "not a singleton method: {labels:?}");
    }

    /// An uppercase-initial partial is still a constant position; the prefix is
    /// dropped and the full child set returned (the client filters).
    #[test]
    fn completion_namespace_children_with_uppercase_partial() {
        let labels = complete("Process::St\n", 0, 11);
        assert!(labels.contains(&"Status".to_string()), "{labels:?}");
    }

    /// …but a LOWERCASE partial after `::` is a class-method call, which keeps
    /// the singleton-method behaviour (this is where the reference keeps it too).
    #[test]
    fn completion_lowercase_after_scope_operator_stays_on_methods() {
        let labels = complete("Time::no\n", 0, 8);
        assert!(labels.contains(&"now".to_string()), "singleton method: {labels:?}");
        assert!(!labels.contains(&"Status".to_string()), "{labels:?}");
    }

    /// A namespace with no children (and an unknown one) yields a null
    /// completion rather than an empty list — matching the reference's
    /// `return nil if children.empty?`.
    #[test]
    fn completion_namespace_without_children_is_empty() {
        assert!(complete("Symbol::\n", 0, 8).is_empty());
        assert!(complete("NoSuchThing::\n", 0, 13).is_empty());
    }

    /// The parent comes from the AST, so a non-constant left side offers nothing
    /// (`[1, 2]::Foo` is not a namespace).
    #[test]
    fn completion_namespace_on_non_constant_parent_is_empty() {
        assert!(complete("[1, 2]::\n", 0, 8).is_empty());
    }

    /// LSP v4 item 2: a PRIVATE method is never offered on an explicit receiver.
    #[test]
    fn completion_excludes_private_methods() {
        let labels = complete("s = \"hi\"\ns.\n", 1, 2);
        assert!(!labels.contains(&"respond_to_missing?".to_string()), "{labels:?}");
        assert!(!labels.contains(&"method_missing".to_string()), "{labels:?}");
        assert!(labels.contains(&"upcase".to_string()), "public still offered: {labels:?}");
    }

    /// …including on a class object, whose surface folds in `Module`'s private
    /// reflection methods.
    #[test]
    fn completion_excludes_private_methods_on_a_class_object() {
        let labels = complete("Time.\n", 0, 5);
        assert!(!labels.contains(&"module_function".to_string()), "{labels:?}");
        assert!(!labels.contains(&"refine".to_string()), "{labels:?}");
        assert!(labels.contains(&"now".to_string()), "{labels:?}");
    }

    /// LSP v4 item 3: a union receiver offers only the methods present on EVERY
    /// arm — `upcase` (String-only) and `times` (Integer-only) are both out,
    /// while the shared `Object`/`Kernel` surface remains.
    #[test]
    fn completion_on_a_union_receiver_intersects_the_arms() {
        let src = "x = ARGV.empty? ? \"hi\" : 3\nx.\n";
        let labels = complete(src, 1, 2);
        assert!(!labels.is_empty(), "a union receiver still completes: {labels:?}");
        assert!(!labels.contains(&"upcase".to_string()), "String-only: {labels:?}");
        assert!(!labels.contains(&"times".to_string()), "Integer-only: {labels:?}");
        assert!(labels.contains(&"frozen?".to_string()), "shared surface: {labels:?}");
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
            // No project root ⇒ `paths: ["lib"]` under a nonexistent dir ⇒ zero
            // project files ⇒ overlay OFF: every pre-S4b test keeps the exact
            // single-file `SourceIndex::build` behaviour it was written against.
            Self::start_project(
                debounce,
                worker_gate,
                client_caps,
                PathBuf::from("/nonexistent-rigor-lsp-test-root"),
                OVERLAY_BUILD_BUDGET_DEFAULT,
            )
        }

        /// Spawn the server loop over a real on-disk project `root` with an
        /// injected overlay scale-guard `budget` (S4b). This is the authentic
        /// production boot: the tier-1 overlay is built by the SAME
        /// [`build_overlay`] call `run_stdio` makes, the config comes from
        /// `<root>/.rigor.yml` through the SAME [`read_project_config`], the guard
        /// is evaluated the same way, and both `window/showMessage` disclosures go
        /// out on the same connection — only the root and the budget are injected,
        /// so no test has to mutate the process-global cwd or race a wall clock.
        ///
        /// The pre-reload harness took a PARSED `Config` and injected it, which was
        /// "the same thing minus the file" only while the config was read exactly
        /// once. It is not any more: an injected config has no file behind it, so
        /// the first structural invalidation would reload it away — a divergence
        /// from production that the assertions could not have seen. Tests now write
        /// the YAML a user would write.
        fn start_project(
            debounce: Duration,
            worker_gate: Arc<WorkerGate>,
            client_caps: serde_json::Value,
            root: PathBuf,
            overlay_budget: Duration,
        ) -> Self {
            let (server_conn, client) = Connection::memory();
            let handle = thread::spawn(move || {
                let caps = serde_json::to_value(server_capabilities()).unwrap();
                // The authentic path: read the client's capabilities from the
                // InitializeParams the handshake returns (not discarded).
                let init_params = server_conn.initialize(caps).unwrap();
                let config_read = read_project_config(&root);
                let config_broken = config_read.is_err();
                if let Err(reason) = &config_read {
                    send_show_message(
                        &server_conn,
                        MessageType::WARNING,
                        config_broken_at_startup_message(reason),
                    )
                    .unwrap();
                }
                let cfg = config_read.unwrap_or_default();
                let ctx = ServerContext {
                    debounce,
                    worker_gate,
                    watched_files_dynamic_registration:
                        client_supports_watched_files_registration(&init_params),
                    project_root: root.clone(),
                    overlay_budget,
                };
                // `CoreIndex::new()` (not `build_core_index`) keeps the tests fast
                // and hermetic — no Gemfile.lock probing under a temp root.
                let index = Arc::new(CoreIndex::new());
                let build = build_overlay(&root, &cfg, &index);
                // The authentic startup posture: the first build is the guard's
                // first sample and can never disable on its own (hysteresis).
                let mut guard = OverlayGuard::new();
                if build.file_count > 0 {
                    guard.record(build.build_project, overlay_budget);
                }
                let overlay = (guard.enabled && build.file_count > 0).then_some(build.files);
                let project = Arc::new(ProjectContext {
                    generation: 0,
                    index,
                    disable: cfg.disable_matcher(),
                    folder: None,
                    stamp: SeverityStamp::from_config(&cfg),
                    exclude: ExcludeMatcher::from_config(&root, &cfg),
                    overlay,
                });
                main_loop(&server_conn, &ctx, project, cfg, guard, config_broken).unwrap();
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

        /// The next message, asserted to be a `window/showMessage`, parsed. The
        /// disclosure channel for the sidecar posture, the overlay guard, and the
        /// config-reload state.
        fn recv_show_message(&self) -> ShowMessageParams {
            match self.recv() {
                Message::Notification(n) if n.method == "window/showMessage" => {
                    serde_json::from_value(n.params).unwrap()
                }
                other => panic!("expected window/showMessage, got {other:?}"),
            }
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
        // The invalidation surface is unchanged from S4 — `.rigor.yml`,
        // `Gemfile.lock`, project `*.rb`, `sig/**/*.rbs` — but review N3 SPLIT it
        // by cost: only the config/signature files force a full tier-1 rebuild; a
        // project `.rb` re-harvests just that file's AST entry.
        let src = |u: &str| classify_watched_files(&watched_change(u));
        assert_eq!(src("file:///p/.rigor.yml"), WatchedChange::Structural);
        assert_eq!(src("file:///p/Gemfile.lock"), WatchedChange::Structural);
        assert_eq!(src("file:///p/sig/user.rbs"), WatchedChange::Structural);
        assert_eq!(src("file:///p/sig/models/user.rbs"), WatchedChange::Structural);
        assert_eq!(
            src("file:///p/app/models/user.rb"),
            WatchedChange::Sources(vec!["file:///p/app/models/user.rb".to_string()]),
            "a project source save is the CHEAP path, not a full rebuild"
        );
        // Not on the surface at all: an `.rbs` outside a `sig/` dir, unrelated files,
        // and a malformed/empty payload.
        assert_eq!(src("file:///p/vendor/other.rbs"), WatchedChange::None);
        assert_eq!(src("file:///p/notes.txt"), WatchedChange::None);
        assert_eq!(src("file:///p/README.md"), WatchedChange::None);
        assert_eq!(classify_watched_files(&serde_json::json!({})), WatchedChange::None);

        // A batch mixing both kinds is STRUCTURAL: the full rebuild re-harvests
        // every source anyway, so the cheap path would be redundant work.
        let mixed = serde_json::json!({ "changes": [
            { "uri": "file:///p/lib/a.rb" },
            { "uri": "file:///p/.rigor.yml" }
        ]});
        assert_eq!(classify_watched_files(&mixed), WatchedChange::Structural);
    }

    #[test]
    fn overlay_guard_needs_consecutive_samples_in_both_directions() {
        // Review N2: one sample is not a classifier. Measured on an idle machine at
        // 3 117 files, six consecutive `build_project` runs straddled a 100 ms
        // budget (93.9 / 93.7 / 90.9 / 106.2 / 94.7 / 92.9 ms) — a single-sample
        // sticky guard is a per-session coin flip with no recovery. Hysteresis in
        // BOTH directions is what makes the posture track the project.
        let budget = Duration::from_millis(100);
        let over = Duration::from_millis(150);
        let under = Duration::from_millis(50);
        let mut g = OverlayGuard::new();
        assert!(g.enabled, "a session starts cross-file");

        // One spike does NOT disable…
        assert_eq!(g.record(over, budget), GuardVerdict::Unchanged);
        assert!(g.enabled);
        // …and a good sample RESETS the streak, so an isolated outlier among
        // healthy samples can never accumulate into a trip.
        assert_eq!(g.record(under, budget), GuardVerdict::Unchanged);
        assert_eq!(g.record(over, budget), GuardVerdict::Unchanged);
        assert!(g.enabled, "spike, good, spike must not disable");

        // Two CONSECUTIVE over-budget samples do.
        assert_eq!(g.record(over, budget), GuardVerdict::Disabled);
        assert!(!g.enabled);

        // Recovery is ASYMMETRIC: a SINGLE under-budget sample re-enables. While
        // OFF the only samples come from structural invalidations, so requiring two
        // consecutive ones made recovery near-unreachable — and the "no restart
        // needed" disclosure false. A wrong re-enable is cheap: once ON, dispatch
        // samples are plentiful and it self-corrects within two of them.
        assert_eq!(g.record(under, budget), GuardVerdict::ReEnabled);
        assert!(g.enabled);

        // …and the re-enabled guard still needs two consecutive over-budget samples
        // to trip again (recovery does not leave it hair-trigger).
        assert_eq!(g.record(over, budget), GuardVerdict::Unchanged);
        assert_eq!(g.record(over, budget), GuardVerdict::Disabled);
        assert_eq!(g.record(under, budget), GuardVerdict::ReEnabled);

        // A sample exactly AT the budget is inside it (the test is `>`).
        let mut edge = OverlayGuard::new();
        assert_eq!(edge.record(budget, budget), GuardVerdict::Unchanged);
        assert!(edge.enabled);
    }

    // ---------------------------------------------------------------------
    // S4b — cross-file overlay (mini-spec 20260719-lsp-s4b-overlay-mini-spec.md)
    // ---------------------------------------------------------------------

    /// A throwaway on-disk project for the S4b overlay tests: a uniquely-named
    /// temp dir with a `lib/` the default config's `paths: ["lib"]` discovers.
    /// Removed on drop. Unique per test (pid + a process-global counter), so the
    /// suite stays parallel-safe WITHOUT ever mutating the process cwd — that is
    /// what [`ServerContext::project_root`] is injectable for.
    struct TempProject {
        root: PathBuf,
    }

    impl TempProject {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("rigor_lsp_s4b_{tag}_{}_{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("lib")).unwrap();
            // Canonicalize the ROOT once: macOS's temp dir is a symlink
            // (`/var` → `/private/var`), and the overlay compares CANONICAL paths.
            let root = std::fs::canonicalize(&root).unwrap();
            Self { root }
        }

        /// Write (or overwrite) the project's `.rigor.yml`. The server reads it
        /// through the production loader, so a test drives config exactly as a user
        /// does — including a `yaml` that does not parse.
        fn write_config(&self, yaml: &str) {
            std::fs::write(self.root.join(".rigor.yml"), yaml).unwrap();
        }

        /// Delete the project's `.rigor.yml` (the "user removed their config" case,
        /// which reloads to DEFAULTS rather than keeping the last good one).
        fn remove_config(&self) {
            std::fs::remove_file(self.root.join(".rigor.yml")).unwrap();
        }

        /// The `file:` URI of the project's `.rigor.yml` — what a client names in
        /// the `didChangeWatchedFiles` payload after the editor saves it.
        fn config_uri(&self) -> String {
            format!("file://{}", self.root.join(".rigor.yml").display())
        }

        /// Write (or overwrite) `lib/<name>` and return its canonical path.
        fn write(&self, name: &str, text: &str) -> PathBuf {
            let p = self.root.join("lib").join(name);
            std::fs::write(&p, text).unwrap();
            std::fs::canonicalize(&p).unwrap()
        }

        /// The `file:` URI for `lib/<name>` (must already exist).
        fn uri(&self, name: &str) -> String {
            format!("file://{}", self.root.join("lib").join(name).display())
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// The cross-file fixture: `Base#helper` is PUBLIC in one file, and `Sub`
    /// (another file) overrides it as PRIVATE. `def.override-visibility-reduced`
    /// fires on `sub.rb` — but ONLY with project-wide context: a single-file index
    /// for `sub.rb` cannot see `Base`, so today's LSP misses it entirely. Verified
    /// against the real CLI: `rigor check lib/sub.rb` is silent, `rigor check lib`
    /// reports it.
    const BASE_RB: &str = "class Base\n  def helper\n  end\nend\n";
    const SUB_RB: &str = "class Sub < Base\n  private\n\n  def helper\n  end\nend\n";

    /// Reproduce `check`'s stage 2 + 3 for `target` over the whole project file
    /// set, INDEPENDENTLY of the LSP's overlay code: parse+lower every project
    /// file, `SourceIndex::build_project` over all of them, then
    /// `analyze_with_source_and_folder` on the target's AST. This is the
    /// `rigor check <project>` semantics the S4b overlay must reproduce; the
    /// parity test compares the LSP's published set against it.
    fn check_project_diagnostics(root: &Path, target: &str) -> Vec<(String, u32)> {
        let cfg = Config::default();
        let index = CoreIndex::new();
        let paths = project_files(root, &cfg);
        let asts: Vec<LoweredAst> = paths
            .iter()
            .map(|p| lower(&parse(&std::fs::read(p).unwrap())))
            .collect();
        let refs: Vec<&LoweredAst> = asts.iter().collect();
        let project_source = SourceIndex::build_project(&refs, &index);
        let target_path = std::fs::canonicalize(root.join("lib").join(target)).unwrap();
        let text = std::fs::read_to_string(&target_path).unwrap();
        let target_ast = lower(&parse(text.as_bytes()));
        let mut interner = Interner::new();
        let diags = analyze_with_source_and_folder(
            &target_ast,
            &mut interner,
            &index,
            &project_source,
            None,
        );
        diags
            .iter()
            .map(|d| {
                (d.rule_id.to_string(), offset_to_position(&text, d.start_offset).line)
            })
            .collect()
    }

    /// The `(rule id, 0-based line)` pairs of a published diagnostic set — the
    /// comparable shape for the parity assertion.
    fn diag_keys(params: &PublishDiagnosticsParams) -> Vec<(String, u32)> {
        params
            .diagnostics
            .iter()
            .map(|d| {
                let code = match &d.code {
                    Some(NumberOrString::String(s)) => s.clone(),
                    other => format!("{other:?}"),
                };
                (code, d.range.start.line)
            })
            .collect()
    }

    #[test]
    fn integration_s4b_saved_buffer_matches_project_check_not_single_file() {
        // ACCEPTANCE 1 (the keystone): for a SAVED (non-dirty) buffer, the LSP's
        // diagnostics equal what `check` produces for that file WITH project
        // context — on a case where cross-file context CHANGES the answer.
        let p = TempProject::new("parity");
        p.write("base.rb", BASE_RB);
        p.write("sub.rb", SUB_RB);
        // A long debounce: didOpen publishes immediately, and no stray timer can
        // interfere with the single-publish assertion.
        let mut h = Harness::start_project(
            Duration::from_secs(30),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );
        h.notify("textDocument/didOpen", open_params(&p.uri("sub.rb"), SUB_RB, 1));
        let published = h.recv_diags();
        assert_eq!(published.uri.as_str(), p.uri("sub.rb"));

        // (a) PARITY: identical to check's project-wide answer for this file.
        let expected = check_project_diagnostics(&p.root, "sub.rb");
        assert_eq!(
            diag_keys(&published),
            expected,
            "LSP diagnostics must equal `check`'s project-wide diagnostics for the file"
        );

        // (b) The answer is genuinely CROSS-FILE: exactly the override-visibility
        // finding, which needs `Base` from the OTHER file.
        assert_eq!(
            diag_keys(&published),
            vec![("def.override-visibility-reduced".to_string(), 3)],
            "the cross-file override finding, on the `def helper` line"
        );

        // (c) …and today's single-file index MISSES it — so this test would fail
        // without the overlay. (Directly: the pre-S4b `compute_diagnostics` path.)
        let (single_file, _) = compute_diagnostics(&project(), &BufferPaths::default(), SUB_RB);
        assert!(
            single_file.is_empty(),
            "a single-file index cannot see `Base`, so it finds nothing: {single_file:?}"
        );
        h.shutdown();
    }

    #[test]
    fn integration_s4b_dirty_buffer_overlay_changes_diagnostics_without_a_save() {
        // ACCEPTANCE 2: removing the overriding method in the OPEN BUFFER changes
        // that file's diagnostics with NO save — the on-disk `sub.rb` still holds
        // the override throughout.
        let p = TempProject::new("dirty");
        p.write("base.rb", BASE_RB);
        let on_disk = p.write("sub.rb", SUB_RB);
        let mut h = Harness::start_project(
            Duration::from_millis(10),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );
        h.notify("textDocument/didOpen", open_params(&p.uri("sub.rb"), SUB_RB, 1));
        assert_eq!(h.recv_diags().diagnostics.len(), 1, "saved buffer: the override fires");

        // Rename the override away in the BUFFER only (no write to disk).
        let edited = "class Sub < Base\n  private\n\n  def unrelated\n  end\nend\n";
        h.notify(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": p.uri("sub.rb"), "version": 2 },
                "contentChanges": [ { "text": edited } ]
            }),
        );
        let after = h.recv_diags();
        assert!(
            after.diagnostics.is_empty(),
            "the buffer no longer overrides `Base#helper` → the finding is gone: {:?}",
            after.diagnostics
        );
        assert_eq!(
            std::fs::read_to_string(&on_disk).unwrap(),
            SUB_RB,
            "the on-disk file was never written — the change was buffer-only"
        );

        // Put it back in the buffer: the finding returns (the overlay is live, not
        // a one-shot).
        h.notify(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": p.uri("sub.rb"), "version": 3 },
                "contentChanges": [ { "text": SUB_RB } ]
            }),
        );
        assert_eq!(h.recv_diags().diagnostics.len(), 1, "restoring the override refires it");
        h.shutdown();
    }

    #[test]
    fn integration_s4b_buffer_replaces_rather_than_adds_the_on_disk_ast() {
        // ACCEPTANCE 3 (the FP-safety pin): a method RENAMED in the buffer must not
        // leave the on-disk name resolvable. On disk `Api#fetch` returns a String,
        // so `Api.new.fetch.lenght` is a hard error. The buffer renames the DEF to
        // `grab` while leaving the call site — so `Api#fetch` no longer exists and
        // the (in-source-only) receiver goes lenient: NO diagnostic.
        //
        // Under ADD semantics the on-disk AST would still register `Api#fetch ->
        // String` and the diagnostic would keep firing — a stale, WRONG type. That
        // is exactly the false positive replacement buys, verified against the CLI:
        // a `lib/` holding BOTH versions reports the error; the renamed file alone
        // does not.
        let p = TempProject::new("replace");
        p.write("other.rb", "class Other\nend\n");
        let on_disk_text =
            "class Api\n  def fetch\n    \"s\"\n  end\nend\n\nApi.new.fetch.lenght\n";
        p.write("main.rb", on_disk_text);
        let mut h = Harness::start_project(
            Duration::from_millis(10),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );
        // Control: the SAVED buffer reproduces the on-disk finding (the fixture is
        // live, so the empty set below cannot be vacuous).
        h.notify("textDocument/didOpen", open_params(&p.uri("main.rb"), on_disk_text, 1));
        assert_eq!(
            diag_keys(&h.recv_diags()),
            vec![("call.undefined-method".to_string(), 6)],
            "saved buffer: `.lenght` on the String `fetch` returns"
        );

        let renamed = "class Api\n  def grab\n    \"s\"\n  end\nend\n\nApi.new.fetch.lenght\n";
        h.notify(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": p.uri("main.rb"), "version": 2 },
                "contentChanges": [ { "text": renamed } ]
            }),
        );
        let after = h.recv_diags();
        assert!(
            after.diagnostics.is_empty(),
            "REPLACE: the on-disk `Api#fetch` is gone, so nothing types the receiver. \
             A non-empty set here means the on-disk AST was ADDED alongside the buffer's \
             (double registration = a stale type = an FP): {:?}",
            after.diagnostics
        );
        h.shutdown();
    }

    #[test]
    fn integration_s4b_deleted_on_disk_file_still_replaces_never_appends() {
        // REGRESSION (review B1): a buffer whose file is DELETED or RENAMED on
        // disk while it stays open — a `git checkout`, a `git stash`, an IDE
        // rename — must STILL replace tier-1's held AST for that path, not be
        // appended alongside it. Appending double-registers the file: the stale
        // on-disk `Api#fetch -> String` would keep typing the receiver and the
        // rename-away would keep firing `undefined method 'lenght'` — the exact
        // wrong-type FP the REPLACE rule exists to prevent.
        //
        // The trap was `fs::canonicalize` failing on a nonexistent path and the
        // resulting `None` being read as "no on-disk identity" (an untitled
        // buffer, where appending IS right). Resolving the PARENT distinguishes
        // the two.
        let p = TempProject::new("deleted");
        p.write("other.rb", "class Other\nend\n");
        let on_disk_text =
            "class Api\n  def fetch\n    \"s\"\n  end\nend\n\nApi.new.fetch.lenght\n";
        let main = p.write("main.rb", on_disk_text);
        let mut h = Harness::start_project(
            Duration::from_millis(10),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );
        h.notify("textDocument/didOpen", open_params(&p.uri("main.rb"), on_disk_text, 1));
        assert_eq!(
            h.recv_diags().diagnostics.len(),
            1,
            "control: the saved buffer reproduces the on-disk finding"
        );

        // The file vanishes from disk; the buffer stays open and is edited.
        std::fs::remove_file(&main).unwrap();
        let renamed = "class Api\n  def grab\n    \"s\"\n  end\nend\n\nApi.new.fetch.lenght\n";
        h.notify(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": p.uri("main.rb"), "version": 2 },
                "contentChanges": [ { "text": renamed } ]
            }),
        );
        let after = h.recv_diags();
        assert!(
            after.diagnostics.is_empty(),
            "a deleted-on-disk buffer must REPLACE its stale held AST, not be appended \
             next to it (double registration = a stale type = an FP): {:?}",
            after.diagnostics
        );
        h.shutdown();
    }

    #[test]
    fn integration_s4b_scale_guard_falls_back_to_single_file_and_discloses() {
        // ACCEPTANCE 4, updated for the hysteresis guard (review N2): with the
        // budget forced to zero EVERY `build_project` is over it, so the guard
        // trips on the second consecutive sample — startup is #1, the first
        // dispatch is #2 — then DISCLOSES via `window/showMessage` (the ADR-0036
        // posture-disclosure precedent) and falls back to the single-file index,
        // which cannot see `Base`.
        //
        // Deterministic: `Duration::ZERO` makes every sample over-budget by
        // construction, so no wall clock, threshold or corpus size is raced. The
        // ORDER is fixed too — `handle_result` publishes first and records the
        // sample after, so the first publish is still the overlay's answer.
        let p = TempProject::new("guard");
        p.write("base.rb", BASE_RB);
        p.write("sub.rb", SUB_RB);
        let mut h = Harness::start_project(
            Duration::from_millis(10),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            Duration::ZERO,
        );
        // Sample #1 (startup) alone must NOT disable — this is the anti-coin-flip
        // property: the first publish is still the full cross-file answer.
        h.notify("textDocument/didOpen", open_params(&p.uri("sub.rb"), SUB_RB, 1));
        assert_eq!(
            h.recv_diags().diagnostics.len(),
            1,
            "one over-budget sample must not disable the overlay"
        );
        // That dispatch WAS sample #2 → the guard trips and discloses.
        match h.recv() {
            Message::Notification(n) if n.method == "window/showMessage" => {
                let params: ShowMessageParams = serde_json::from_value(n.params).unwrap();
                assert_eq!(params.typ, MessageType::WARNING);
                assert!(
                    params.message.contains("cross-file diagnostics disabled")
                        && params.message.contains("single-file scope"),
                    "the posture is disclosed, never silently degraded: {}",
                    params.message
                );
                assert!(
                    params.message.contains("no restart needed"),
                    "the disclosure must say the decision is re-evaluated, not permanent: {}",
                    params.message
                );
            }
            other => panic!("expected the scale-guard showMessage, got {other:?}"),
        }
        // The NEXT dispatch runs on the single-file fallback: `Base` is invisible,
        // so the cross-file finding is gone.
        h.notify(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": p.uri("sub.rb"), "version": 2 },
                "contentChanges": [ { "text": SUB_RB } ]
            }),
        );
        let d = h.recv_diags();
        assert!(
            d.diagnostics.is_empty(),
            "guard tripped ⇒ single-file fallback ⇒ the cross-file finding is absent: {:?}",
            d.diagnostics
        );
        h.shutdown();
    }

    #[test]
    fn integration_s4b_guard_stays_enabled_on_a_healthy_project() {
        // The complement of the trip test, and the property the old single-sample
        // sticky guard could not offer: under a generous budget the overlay stays
        // ON across many dispatches — no drift into the fallback, and NO
        // `window/showMessage` churn (asserted by a hover round-tripping as the
        // very next message after each publish).
        let p = TempProject::new("healthy");
        p.write("base.rb", BASE_RB);
        p.write("sub.rb", SUB_RB);
        let mut h = Harness::start_project(
            Duration::from_millis(10),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            Duration::from_secs(30),
        );
        h.notify("textDocument/didOpen", open_params(&p.uri("sub.rb"), SUB_RB, 1));
        assert_eq!(h.recv_diags().diagnostics.len(), 1);
        for version in 2..=6 {
            h.notify(
                "textDocument/didChange",
                serde_json::json!({
                    "textDocument": { "uri": p.uri("sub.rb"), "version": version },
                    "contentChanges": [ { "text": SUB_RB } ]
                }),
            );
            assert_eq!(
                h.recv_diags().diagnostics.len(),
                1,
                "dispatch {version}: the overlay must still be on"
            );
            // A hover answers next ⇒ no showMessage was queued behind the publish.
            hover_sync(&h, 500 + version, &p.uri("sub.rb"));
        }
        h.shutdown();
    }

    #[test]
    fn integration_s4b_watched_file_save_reharvests_the_project_asts() {
        // ACCEPTANCE 5 (S4 plumbing × S4b substrate): a SAVED edit to a project file
        // that is NOT open changes the open buffer's diagnostics, because
        // `invalidate` re-harvests the held ASTs. This is the pay-off S4 deferred
        // ("the cross-file benefit lands in S4b").
        let p = TempProject::new("watched");
        p.write("base.rb", BASE_RB);
        p.write("sub.rb", SUB_RB);
        let mut h = Harness::start_project(
            Duration::from_secs(30),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );
        h.notify("textDocument/didOpen", open_params(&p.uri("sub.rb"), SUB_RB, 1));
        assert_eq!(h.recv_diags().diagnostics.len(), 1, "the override fires against Base");

        // `Base#helper` is deleted ON DISK; `sub.rb` (the open buffer) is untouched.
        p.write("base.rb", "class Base\nend\n");
        h.notify(
            "workspace/didChangeWatchedFiles",
            serde_json::json!({ "changes": [ { "uri": p.uri("base.rb"), "type": 2 } ] }),
        );
        let after = h.recv_diags();
        assert!(
            after.diagnostics.is_empty(),
            "the re-harvested project ASTs no longer define `Base#helper`, so the \
             override finding is gone: {:?}",
            after.diagnostics
        );
        h.shutdown();
    }

    #[test]
    fn integration_s4b_source_save_reharvests_only_that_file_and_deletion_removes_it() {
        // Review N3: a project `.rb` save must re-harvest ONLY that file's AST
        // entry — not re-parse the project on the loop thread. Behaviourally that
        // has to be INDISTINGUISHABLE from the old full rebuild, so this drives the
        // three transitions through the public protocol.
        let p = TempProject::new("reharvest");
        p.write("base.rb", BASE_RB);
        p.write("sub.rb", SUB_RB);
        p.write("untouched.rb", "class Untouched\n  def helper\n  end\nend\n");
        let mut h = Harness::start_project(
            Duration::from_secs(30),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );
        h.notify("textDocument/didOpen", open_params(&p.uri("sub.rb"), SUB_RB, 1));
        assert_eq!(h.recv_diags().diagnostics.len(), 1, "the override fires against Base");

        // (1) EDIT an existing held file → its entry is replaced in place.
        p.write("base.rb", "class Base\nend\n");
        h.notify(
            "workspace/didChangeWatchedFiles",
            serde_json::json!({ "changes": [ { "uri": p.uri("base.rb"), "type": 2 } ] }),
        );
        assert!(
            h.recv_diags().diagnostics.is_empty(),
            "`Base#helper` is gone ⇒ nothing is overridden"
        );

        // (2) RESTORE it → the entry is replaced again (not duplicated: a duplicate
        // `Base` would still resolve `helper` and the count would stay at 1 either
        // way, so the real proof is transition (3) below).
        p.write("base.rb", BASE_RB);
        h.notify(
            "workspace/didChangeWatchedFiles",
            serde_json::json!({ "changes": [ { "uri": p.uri("base.rb"), "type": 2 } ] }),
        );
        assert_eq!(h.recv_diags().diagnostics.len(), 1, "restoring Base#helper refires it");

        // (3) DELETE it → the entry is REMOVED. This is what a naive incremental
        // update gets wrong (a vanished file has nothing to re-parse, so a
        // replace-only implementation would silently keep the stale AST forever).
        std::fs::remove_file(p.root.join("lib/base.rb")).unwrap();
        h.notify(
            "workspace/didChangeWatchedFiles",
            serde_json::json!({ "changes": [ { "uri": p.uri("base.rb"), "type": 3 } ] }),
        );
        assert!(
            h.recv_diags().diagnostics.is_empty(),
            "a deleted project file must be dropped from the held table"
        );

        // (4) A NEW in-scope file takes the full-rebuild path (ordering fidelity)
        // and is picked up.
        p.write("base.rb", BASE_RB);
        h.notify(
            "workspace/didChangeWatchedFiles",
            serde_json::json!({ "changes": [ { "uri": p.uri("base.rb"), "type": 1 } ] }),
        );
        assert_eq!(h.recv_diags().diagnostics.len(), 1, "a re-created file is re-harvested");
        h.shutdown();
    }

    /// A complete structural fingerprint of a held overlay: every entry's path AND
    /// its AST's full `Debug` dump, IN ORDER. Comparing this against a fresh
    /// `build_overlay` is the strong form of N3's invariant — not just "the same
    /// files" but the same ASTs in the same positions, which is what
    /// `build_project`'s order-sensitive multi-pass harvest actually consumes.
    /// A symlink alias for a project root, removed on drop — the "workspace
    /// reached through a symlink" shape (the normal macOS `/tmp` → `/private/tmp`
    /// case, a symlinked project dir, a symlinked home).
    struct AliasLink(PathBuf);

    impl AliasLink {
        fn to(root: &Path) -> Self {
            let alias = root.parent().unwrap().join(format!(
                "{}_alias",
                root.file_name().unwrap().to_string_lossy()
            ));
            let _ = std::fs::remove_file(&alias);
            std::os::unix::fs::symlink(root, &alias).unwrap();
            Self(alias)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for AliasLink {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Enter a directory for the duration of a scope, restoring the previous cwd on
    /// drop — INCLUDING on a panic, so a failing assertion cannot leave the rest of
    /// the suite in the wrong directory.
    ///
    /// The process cwd is global, so this also serialises every test that needs it
    /// behind one mutex. Only the `project_root = "."` (production-shape) tests take
    /// it; every other test injects an absolute root precisely to avoid this.
    struct CwdGuard {
        prev: PathBuf,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl CwdGuard {
        fn enter(dir: &Path) -> Self {
            static CWD: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let lock = CWD.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::current_dir().unwrap();
            std::env::set_current_dir(dir).unwrap();
            Self { prev, _lock: lock }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.prev);
        }
    }

    fn overlay_fingerprint(files: &ProjectFiles) -> Vec<(String, String)> {
        files
            .files
            .iter()
            .map(|(p, a)| (p.to_string_lossy().into_owned(), format!("{a:?}")))
            .collect()
    }

    /// A `Session` over a real temp project, for driving [`reharvest_sources`]
    /// directly (the incremental path is loop-thread state, not protocol surface).
    fn session_for(root: &Path, cfg: Config, budget: Duration) -> (ServerContext, Session) {
        let ctx = ServerContext {
            debounce: Duration::from_secs(30),
            worker_gate: production_gate(),
            watched_files_dynamic_registration: false,
            project_root: root.to_path_buf(),
            overlay_budget: budget,
        };
        let index = Arc::new(CoreIndex::new());
        let build = build_overlay(root, &cfg, &index);
        let (results_tx, _rx) = crossbeam_channel::unbounded();
        let st = Session {
            buffers: BufferTable::new(),
            debouncer: Debouncer::new(),
            in_flight: HashSet::new(),
            epochs: HashMap::new(),
            project: Arc::new(ProjectContext {
                generation: 0,
                index,
                disable: cfg.disable_matcher(),
                folder: None,
                stamp: SeverityStamp::from_config(&cfg),
                exclude: ExcludeMatcher::from_config(root, &cfg),
                overlay: Some(build.files),
            }),
            cfg,
            config_broken: false,
            results_tx,
            guard: OverlayGuard::new(),
        };
        (ctx, st)
    }

    /// How the project root is SPELLED to the server — the axis review R-1 hid
    /// behind. Every other test injects an absolute temp dir; production passes
    /// `"."` and the workspace is routinely reached through a symlink, where the
    /// URI spelling and the canonicalized root differ.
    #[derive(Clone, Copy, Debug)]
    enum RootSpelling {
        /// `project_root` = the absolute canonical temp dir (what tests inject).
        Absolute,
        /// `project_root` = `"."` with the cwd entered through a SYMLINK to the
        /// project, and URIs spelled through that symlink — the production shape.
        DotThroughSymlink,
    }

    #[test]
    fn incremental_reharvest_is_byte_identical_to_a_full_rebuild() {
        // THE N3 INVARIANT (adversarial review of PR #43): after ANY sequence of
        // incremental updates, the held set must equal what a full rebuild of the
        // same tree would produce — entries, ASTs, and ORDER.
        //
        // The first implementation decided membership with a `ProjectScope`
        // predicate that re-derived bare-`check`'s discovery rule, and diverged from
        // it four ways (a deleted DIRECTORY, a symlinked `.rb` held under its
        // out-of-root canonical path, `paths: ["."]`, and the same canonical path
        // held twice). This test drives all of them plus the ordinary transitions,
        // across BOTH `paths:` shapes and BOTH root spellings, comparing against the
        // ground truth each time — so it fails on ANY divergence, not only the ones
        // that were found.
        for paths_yaml in ["paths:\n  - lib\n", "paths:\n  - \".\"\n"] {
            for spelling in [RootSpelling::Absolute, RootSpelling::DotThroughSymlink] {
                differential_run(paths_yaml, spelling);
            }
        }
    }

    fn differential_run(paths_yaml: &str, spelling: RootSpelling) {
        let cfg: Config = serde_yaml::from_str(paths_yaml).unwrap();
        let cfg2: Config = serde_yaml::from_str(paths_yaml).unwrap();
        let p = TempProject::new("differential");
        p.write("a.rb", "class A\nend\n");
        p.write("b.rb", BASE_RB);
        std::fs::create_dir_all(p.root.join("lib/nested")).unwrap();
        std::fs::write(p.root.join("lib/nested/c.rb"), "class C\nend\n").unwrap();
        // A symlinked `.rb` FILE: bare-`check` discovery harvests it (a symlink to a
        // file matches `Dir.glob`), and it is held under its CANONICAL, out-of-`lib`
        // path. Under `paths: ["."]` BOTH it and its target are walked, so the same
        // canonical path is held twice.
        std::fs::create_dir_all(p.root.join("shared")).unwrap();
        std::fs::write(p.root.join("shared/linked.rb"), "class Linked\nend\n").unwrap();
        std::os::unix::fs::symlink(p.root.join("shared/linked.rb"), p.root.join("lib/linked.rb"))
            .unwrap();

        // The root as the SERVER sees it, and the prefix URIs are spelled with.
        let alias = matches!(spelling, RootSpelling::DotThroughSymlink)
            .then(|| AliasLink::to(&p.root));
        let _cwd = alias.as_ref().map(|a| CwdGuard::enter(a.path()));
        let ctx_root = match &alias {
            Some(_) => PathBuf::from("."),
            None => p.root.clone(),
        };
        // URIs are spelled through the alias in the symlink case — the whole point:
        // the decoded spelling then differs from every canonical form.
        let uri_root = alias.as_ref().map_or_else(|| p.root.clone(), |a| a.path().to_path_buf());
        let uri = |name: &str| format!("file://{}", uri_root.join("lib").join(name).display());

        let (ctx, mut st) = session_for(&ctx_root, cfg2, OVERLAY_BUILD_BUDGET_DEFAULT);
        let index = Arc::new(CoreIndex::new());
        let check = |st: &Session, label: &str| {
            // Ground truth: a full rebuild under the SAME root spelling.
            let truth = build_overlay(&ctx_root, &cfg, &index);
            let held = st.project.overlay.as_ref().expect("overlay stays live");
            assert_eq!(
                overlay_fingerprint(held),
                overlay_fingerprint(&truth.files),
                "[{paths_yaml:?} / {spelling:?} / {label}] incremental state diverged \
                 from a full rebuild"
            );
        };
        check(&st, "initial");

        // (1) EDIT a held file — the fast path (replace in place).
        p.write("a.rb", "class A\n  def extra\n  end\nend\n");
        reharvest_sources(&ctx, &mut st, &[uri("a.rb")]);
        check(&st, "edit held file");

        // (2) EDIT through the SYMLINK's path. The event names lib/linked.rb; the
        // held entry is shared/linked.rb. Canonicalization is what makes these the
        // same entry — the scope predicate got this wrong (probe C).
        std::fs::write(p.root.join("shared/linked.rb"), "class Linked\n  def x\n  end\nend\n")
            .unwrap();
        reharvest_sources(&ctx, &mut st, &[uri("linked.rb")]);
        check(&st, "edit via symlink path");

        // (3) DELETE a held file whose parent still exists — removed in place.
        std::fs::remove_file(p.root.join("lib/b.rb")).unwrap();
        reharvest_sources(&ctx, &mut st, &[uri("b.rb")]);
        check(&st, "delete file");

        // (4) CREATE a new file — not held ⇒ full rebuild ⇒ correct ORDER (an
        //     append would put it last; `build_overlay` sorts).
        p.write("aa.rb", "class Aa\nend\n");
        reharvest_sources(&ctx, &mut st, &[uri("aa.rb")]);
        check(&st, "create file");

        // (5) DELETE A DIRECTORY — the path no longer resolves even via its parent,
        //     so it is not decidable incrementally (probes A / E / F). Under the
        //     symlinked spelling this is exactly R-1: the decoded path cannot be
        //     compared soundly against a canonical root, so it must NOT be ignored.
        std::fs::remove_dir_all(p.root.join("lib/nested")).unwrap();
        reharvest_sources(&ctx, &mut st, &[uri("nested/c.rb")]);
        check(&st, "delete directory");

        // (6) An out-of-project `.rb` event: ignored (or full-rebuilt) — either way
        //     the state must still match the ground truth.
        reharvest_sources(&ctx, &mut st, &["file:///nowhere/at/all/x.rb".to_string()]);
        check(&st, "unrelated file");

        // (7) A BATCH mixing an edit, a delete and a creation in one payload.
        p.write("a.rb", "class A\nend\n");
        std::fs::remove_file(p.root.join("lib/aa.rb")).unwrap();
        p.write("z.rb", "class Z\nend\n");
        reharvest_sources(&ctx, &mut st, &[uri("a.rb"), uri("aa.rb"), uri("z.rb")]);
        check(&st, "mixed batch");
    }

    #[test]
    fn overlay_off_when_the_project_has_no_files() {
        // No `lib/` ⇒ nothing to overlay ⇒ the overlay stays OFF and the guard does
        // NOT trip (an empty project is not an over-budget one, so no misleading
        // disclosure). This is the posture every pre-S4b test runs under.
        let root = std::env::temp_dir().join(format!("rigor_lsp_s4b_empty_{}", std::process::id()));
        let build = build_overlay(&root, &Config::default(), &CoreIndex::new());
        assert_eq!(build.file_count, 0, "no project files");
        assert!(build.files.files.is_empty());
        // An empty project must NOT feed the guard: a ~0 ms build of nothing is not
        // evidence the project is fast, and counting it would let an empty tree
        // re-enable an overlay that a real one had disabled. The callers gate on
        // `file_count > 0`; assert the posture stays untouched and silent.
        let mut guard = OverlayGuard::new();
        guard.enabled = false;
        assert_eq!(
            overlay_guard_message(&GuardVerdict::Unchanged, 0, Duration::ZERO, Duration::ZERO),
            None,
            "no posture flip ⇒ no disclosure"
        );
        assert!(!guard.enabled, "an empty project neither trips nor recovers the guard");
    }

    #[test]
    fn project_files_follow_bare_check_discovery() {
        // The overlay's file set is bare-`check`'s: the `paths:` roots expanded
        // recursively and SORTED per root, minus config `exclude:`.
        let p = TempProject::new("discovery");
        p.write("b.rb", "class B\nend\n");
        p.write("a.rb", "class A\nend\n");
        std::fs::create_dir_all(p.root.join("lib/nested")).unwrap();
        std::fs::write(p.root.join("lib/nested/c.rb"), "class C\nend\n").unwrap();
        std::fs::write(p.root.join("lib/notruby.txt"), "nope\n").unwrap();
        let found = project_files(&p.root, &Config::default());
        let names: Vec<String> = found
            .iter()
            .map(|f| f.rsplit('/').next().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["a.rb", "b.rb", "c.rb"], "sorted, recursive, `.rb` only");

        // `exclude:` prunes, exactly as `check`'s per-file gate does.
        let cfg: Config = serde_yaml::from_str("exclude:\n  - \"**/nested/**\"\n").unwrap();
        let pruned = project_files(&p.root, &cfg);
        assert_eq!(pruned.len(), 2, "the excluded dir is not harvested: {pruned:?}");
    }

    // ---------------------------------------------------------------------
    // Config `exclude:` for the OPEN BUFFER — `check`'s stage-1 file filter.
    //
    // `check` skips an excluded file before it is even read (`main.rs`
    // `Stage1::Excluded`), so it reports no rows for it; the LSP published
    // markers for it anyway. The gate below is the same filter, applied to the
    // buffer, against the same path SPELLING discovery matches on.
    // ---------------------------------------------------------------------

    #[test]
    fn exclude_gate_agrees_with_bare_check_discovery() {
        // THE INVARIANT (PR #45 review): a buffer is excluded IFF EVERY discovery
        // spelling of that file is excluded. `check` analyses a file if ANY name it
        // was walked under survives `exclude:`, and one file can be walked under
        // several names — a symlinked `.rb` under the LINK's name, an overlapping
        // `paths:` pair under two roots. The first cut of this gate re-derived ONE
        // canonical spelling and silently dropped three shapes `check` analyses
        // (B1/B2/B3 in the note); this differential is what pins the invariant.
        //
        // Driven across `paths:` shapes (incl. the OVERLAPPING multi-root case),
        // pattern sets, root spellings, AND both gate tiers — with a symlinked `.rb`
        // in every fixture, because the symlink and multi-root axes are exactly the
        // two the original 24-run matrix lacked.
        for paths_yaml in [
            "paths:\n  - lib\n",
            "paths:\n  - \".\"\n",
            "paths:\n  - \".\"\n  - lib\n",
            "paths:\n  - lib\n  - \".\"\n",
        ] {
            for patterns in [
                "exclude: []\n",
                "exclude:\n  - \"**/vendor/**\"\n",
                "exclude:\n  - \"**/b.rb\"\n",
                "exclude:\n  - \"**/*.rb\"\n",
                "exclude:\n  - \"lib/a.rb\"\n",
                "exclude:\n  - \"./lib/a.rb\"\n",
                "exclude:\n  - \"./lib/**\"\n",
                "exclude:\n  - \"lib/real.rb\"\n",
                "exclude:\n  - \"vendor/**\"\n",
            ] {
                for spelling in [RootSpelling::Absolute, RootSpelling::DotThroughSymlink] {
                    for overlay in [OverlayTier::Live, OverlayTier::Off] {
                        exclude_agreement_run(paths_yaml, patterns, spelling, overlay);
                    }
                }
            }
        }
    }

    /// Which tier of the gate a differential run exercises: with the overlay LIVE
    /// the discovery-membership tier answers, with it OFF (the scale guard tripped)
    /// the spelling fallback must reach the same verdict on its own.
    #[derive(Clone, Copy, Debug)]
    enum OverlayTier {
        Live,
        Off,
    }

    fn exclude_agreement_run(
        paths_yaml: &str,
        patterns: &str,
        spelling: RootSpelling,
        tier: OverlayTier,
    ) {
        let yaml = format!("{paths_yaml}{patterns}");
        let cfg: Config = serde_yaml::from_str(&yaml).unwrap();
        // The same `paths:` with NO `exclude:` — the unfiltered discovery set, i.e.
        // every candidate the gate has to answer about.
        let unfiltered: Config = serde_yaml::from_str(paths_yaml).unwrap();

        let p = TempProject::new("exclgate");
        p.write("a.rb", "class A\nend\n");
        p.write("b.rb", "class B\nend\n");
        p.write("real.rb", "class Real\nend\n");
        std::fs::create_dir_all(p.root.join("lib/vendor")).unwrap();
        std::fs::write(p.root.join("lib/vendor/v.rb"), "class V\nend\n").unwrap();
        // AXIS 1 (review N1): a symlinked `.rb` FILE inside `lib`, pointing OUT of
        // `lib`. `collect_rb_files` includes it (matching `Dir.glob`), so discovery
        // walks it under `lib/shared.rb` while its canonical path is
        // `<root>/vendor/shared.rb` — the two names carry DIFFERENT `exclude:`
        // verdicts, which is regression B1.
        std::fs::create_dir_all(p.root.join("vendor")).unwrap();
        std::fs::write(p.root.join("vendor/shared.rb"), "class Shared\nend\n").unwrap();
        std::os::unix::fs::symlink(p.root.join("vendor/shared.rb"), p.root.join("lib/shared.rb"))
            .unwrap();
        // …and one pointing INSIDE `lib` (regression B2: `exclude: ["lib/real.rb"]`
        // prunes the target's own spelling but not the link's).
        std::os::unix::fs::symlink(p.root.join("lib/real.rb"), p.root.join("lib/link.rb")).unwrap();

        let alias = matches!(spelling, RootSpelling::DotThroughSymlink)
            .then(|| AliasLink::to(&p.root));
        let _cwd = alias.as_ref().map(|a| CwdGuard::enter(a.path()));
        let root = match &alias {
            Some(_) => PathBuf::from("."),
            None => p.root.clone(),
        };
        // URIs are spelled the way the client would spell them — through the alias
        // in the symlink case, and NEVER with the symlinked file resolved.
        let uri_root = alias.as_ref().map_or_else(|| p.root.clone(), |a| a.path().to_path_buf());

        let all = project_files(&root, &unfiltered);
        let kept = project_files(&root, &cfg);
        let matcher = ExcludeMatcher::from_config(&root, &cfg);
        // The tier-1 substrate: the post-`exclude:` discovery set, canonicalized —
        // exactly what `build_overlay` holds. Built from `kept` directly so the test
        // pins the MEMBERSHIP rule rather than re-testing the harvest.
        let held = ProjectFiles {
            files: kept
                .iter()
                .filter_map(|f| {
                    Some((std::fs::canonicalize(f).ok()?, Arc::new(lower(&parse(b"")))))
                })
                .collect(),
        };
        let overlay = match tier {
            OverlayTier::Live => Some(&held),
            OverlayTier::Off => None,
        };

        // GROUND TRUTH: `check` analyses a file iff SOME discovery spelling of it
        // survived `exclude:`. Keyed on the canonical path, because that is the
        // file's identity — two spellings of one file share it.
        let mut analysed: std::collections::HashMap<PathBuf, bool> =
            std::collections::HashMap::new();
        for f in &all {
            let Ok(canonical) = std::fs::canonicalize(f) else { continue };
            *analysed.entry(canonical).or_insert(false) |= kept.contains(f);
        }

        for f in &all {
            let Ok(canonical) = std::fs::canonicalize(f) else { continue };
            // The buffer the editor would open for this discovery spelling: the URI
            // names the file as DISCOVERY did (through the link, through the alias),
            // which is what makes the symlink axis observable at all.
            let rel = spelling_relative_to_root(f, &root);
            let buf = BufferPaths::for_uri(
                &format!("file://{}", uri_root.join(&rel).display())
                    .parse::<Uri>()
                    .unwrap(),
            );
            assert_eq!(
                matcher.excludes(&buf, overlay),
                !analysed[&canonical],
                "[{yaml:?} / {spelling:?} / {tier:?}] the buffer gate and bare-`check` \
                 discovery disagree about {f}"
            );
        }
        // Non-vacuity of the loop itself: the `**/*.rb` case must prune everything
        // and the empty case nothing, so the comparison above is exercised on BOTH
        // answers somewhere in the matrix.
        if patterns.contains("**/*.rb\"") {
            assert!(kept.is_empty(), "[{yaml:?}] the catch-all pattern prunes the whole set");
        }
        if patterns == "exclude: []\n" {
            assert_eq!(kept.len(), all.len(), "[{yaml:?}] no patterns ⇒ nothing pruned");
        }
        // N3 (review): under `RootSpelling::Absolute` the relative patterns
        // (`lib/a.rb`, `./lib/**`, `vendor/**`) match nothing on EITHER side, so
        // those cells agree vacuously. They are kept because they cost nothing and
        // guard the absolute-root path against a future change that starts matching
        // them; the discriminating cells are the `DotThroughSymlink` ones, which is
        // the production root shape anyway.
    }

    /// A discovery spelling (`lib/a.rb`, `./lib/a.rb`, or an absolute one) reduced
    /// to its path relative to the project root, so a client URI can be built for
    /// it WITHOUT resolving any symlink on the way.
    fn spelling_relative_to_root(spelling: &str, root: &Path) -> PathBuf {
        let path = Path::new(spelling);
        if let Ok(rel) = path.strip_prefix(root) {
            return rel.to_path_buf();
        }
        // A relative spelling: strip a leading `./` and it is already root-relative.
        path.strip_prefix("./").unwrap_or(path).to_path_buf()
    }

    #[test]
    fn exclude_gate_leaves_pathless_and_out_of_workspace_buffers_alone() {
        // Two cases where NO `check` invocation from this root names the file, so
        // there is no spelling to match and the gate must not guess: an untitled /
        // non-`file:` buffer (`path == None`), and a file outside the workspace.
        // Both keep exactly today's behaviour.
        let p = TempProject::new("exclscope");
        let inside = p.write("a.rb", "class A\nend\n");
        let cfg: Config = serde_yaml::from_str("exclude:\n  - \"**/*.rb\"\n").unwrap();
        let matcher = ExcludeMatcher::from_config(&p.root, &cfg);

        assert!(
            matcher.excludes(&buffer_at(&inside), None),
            "the control: an in-project file IS excluded"
        );
        assert!(
            !matcher.excludes(&BufferPaths::default(), None),
            "an untitled buffer has no name to match"
        );

        let outside = TempProject::new("exclscope_other");
        let elsewhere = outside.write("a.rb", "class A\nend\n");
        assert!(
            !matcher.excludes(&buffer_at(&elsewhere), None),
            "a buffer outside the workspace is not `check`'s to exclude"
        );
    }

    /// The [`BufferPaths`] an editor would send for an existing on-disk `path`.
    fn buffer_at(path: &Path) -> BufferPaths {
        BufferPaths::for_uri(&format!("file://{}", path.display()).parse::<Uri>().unwrap())
    }

    #[test]
    fn exclude_gate_uses_the_root_relative_spelling_outside_paths() {
        // A buffer inside the workspace but outside every `paths:` root: bare
        // `check` never discovers it, so the ONLY run that reports on it is an
        // explicit `rigor check spec/x.rb` from the project root — which matches
        // `exclude:` against exactly that root-relative spelling. The gate uses it,
        // so such a buffer is silenced iff that `check` run would report nothing.
        //
        // This does NOT touch the S4b/N5 divergence (an out-of-`paths:` buffer is
        // still analysed against the full project index): it only decides whether
        // the buffer is analysed at all, on the same input `check` decides it on.
        let p = TempProject::new("exclout");
        std::fs::create_dir_all(p.root.join("spec")).unwrap();
        let spec = p.root.join("spec/x_spec.rb");
        std::fs::write(&spec, "class X\nend\n").unwrap();
        let spec = std::fs::canonicalize(&spec).unwrap();
        // The production root shape: `project_root = "."` with the cwd IN the
        // project, which is what makes a relative `exclude:` pattern meaningful.
        let _cwd = CwdGuard::enter(&p.root);

        let buf = buffer_at(&spec);
        let matching: Config = serde_yaml::from_str("exclude:\n  - \"spec/**\"\n").unwrap();
        assert!(
            ExcludeMatcher::from_config(&PathBuf::from("."), &matching).excludes(&buf, None),
            "an out-of-`paths:` buffer is excluded exactly when `rigor check \
             spec/x_spec.rb` from the project root would report nothing for it"
        );
        // The control: a pattern that does NOT cover it leaves it analysed, exactly
        // as today (the N5 divergence is untouched by this slice).
        let other: Config = serde_yaml::from_str("exclude:\n  - \"vendor/**\"\n").unwrap();
        assert!(!ExcludeMatcher::from_config(&PathBuf::from("."), &other).excludes(&buf, None));
    }

    #[test]
    fn exclude_gate_never_drops_a_symlinked_file_check_analyses() {
        // Regressions B1/B2 at the matcher seam (the E2E versions live in
        // `lsp_check_parity.rs`). `collect_rb_files` deliberately INCLUDES symlinked
        // `.rb` files (`main.rs`, the 2026-07-06 audit correction matching
        // `Dir.glob`), so discovery walks the LINK's name — and the link's name and
        // the target's name can carry opposite `exclude:` verdicts.
        let p = TempProject::new("exclsymlink");
        std::fs::create_dir_all(p.root.join("vendor")).unwrap();
        std::fs::write(p.root.join("vendor/shared.rb"), "class Shared\nend\n").unwrap();
        std::os::unix::fs::symlink(p.root.join("vendor/shared.rb"), p.root.join("lib/shared.rb"))
            .unwrap();
        p.write("real.rb", "class Real\nend\n");
        std::os::unix::fs::symlink(p.root.join("lib/real.rb"), p.root.join("lib/link.rb")).unwrap();
        let _cwd = CwdGuard::enter(&p.root);
        let root = PathBuf::from(".");

        // B1: `lib/shared.rb` → `vendor/shared.rb`, excluded by `**/vendor/**`.
        // Discovery keeps `lib/shared.rb`, so `check` analyses it.
        let b1: Config = serde_yaml::from_str("exclude:\n  - \"**/vendor/**\"\n").unwrap();
        assert!(project_files(&root, &b1).iter().any(|f| f.ends_with("lib/shared.rb")));
        assert!(
            !ExcludeMatcher::from_config(&root, &b1)
                .excludes(&buffer_at(&p.root.join("lib/shared.rb")), None),
            "B1: the link's own name survives `exclude:`, so `check` analyses the file"
        );
        // The control: a pattern covering BOTH the link's name and the target's
        // leaves no surviving spelling, so the same file IS excluded — proving the
        // gate is live and that tier 3 rescues only a genuinely surviving name.
        let both: Config =
            serde_yaml::from_str("paths:\n  - \".\"\nexclude:\n  - \"**/shared.rb\"\n").unwrap();
        assert!(
            ExcludeMatcher::from_config(&root, &both)
                .excludes(&buffer_at(&p.root.join("lib/shared.rb")), None),
            "the control: with EVERY spelling excluded the same buffer is dropped"
        );

        // B2: `lib/link.rb` → `lib/real.rb`, excluded by `lib/real.rb`. Discovery
        // keeps `lib/link.rb`, so `check` analyses the content under that name.
        let b2: Config = serde_yaml::from_str("exclude:\n  - \"lib/real.rb\"\n").unwrap();
        assert!(project_files(&root, &b2).iter().any(|f| f.ends_with("lib/link.rb")));
        assert!(
            !ExcludeMatcher::from_config(&root, &b2)
                .excludes(&buffer_at(&p.root.join("lib/link.rb")), None),
            "B2: the link's name is not excluded, so the buffer must be analysed"
        );
    }

    #[test]
    fn exclude_gate_needs_every_root_spelling_excluded() {
        // Regression B3: under OVERLAPPING `paths:` roots one file is walked twice,
        // and `check` analyses it as long as ONE spelling survives. Both root orders
        // are driven, because the first cut returned on the first containing root
        // and so gave an ORDER-DEPENDENT answer.
        let p = TempProject::new("exclmultiroot");
        p.write("a.rb", "class A\nend\n");
        let _cwd = CwdGuard::enter(&p.root);
        let root = PathBuf::from(".");
        let buf = buffer_at(&p.root.join("lib/a.rb"));

        for order in ["paths:\n  - \".\"\n  - lib\n", "paths:\n  - lib\n  - \".\"\n"] {
            let cfg: Config =
                serde_yaml::from_str(&format!("{order}exclude:\n  - \"./lib/**\"\n")).unwrap();
            // Discovery yields `./lib/a.rb` (pruned) AND `lib/a.rb` (kept).
            let kept = project_files(&root, &cfg);
            assert!(kept.iter().any(|f| f == "lib/a.rb"), "[{order:?}] one spelling survives");
            assert!(
                !ExcludeMatcher::from_config(&root, &cfg).excludes(&buf, None),
                "[{order:?}] B3: one surviving spelling means `check` analyses the file"
            );
        }
        // The control: a pattern covering BOTH spellings does exclude it.
        let both: Config =
            serde_yaml::from_str("paths:\n  - \".\"\n  - lib\nexclude:\n  - \"**/a.rb\"\n")
                .unwrap();
        assert!(project_files(&root, &both).is_empty());
        assert!(ExcludeMatcher::from_config(&root, &both).excludes(&buf, None));
    }

    #[test]
    fn an_excluded_buffer_computes_no_diagnostics() {
        // The gate at the `compute_diagnostics` seam, with its control in the same
        // test: the SAME buffer, SAME content, only the config differs.
        let p = TempProject::new("exclcompute");
        let path = p.write("typo.rb", TYPO);

        let buf = buffer_at(&path);
        let control = compute_diagnostics(
            &project_with_config_rooted(&Config::default(), &p.root),
            &buf,
            TYPO,
        );
        assert_eq!(control.0.len(), 1, "the control: the rule fires unconfigured");

        let cfg: Config = serde_yaml::from_str("exclude:\n  - \"**/typo.rb\"\n").unwrap();
        let excluded = compute_diagnostics(&project_with_config_rooted(&cfg, &p.root), &buf, TYPO);
        assert!(
            excluded.0.is_empty(),
            "an `exclude:`d buffer publishes NOTHING, as `check` reports nothing: {:?}",
            excluded.0
        );
    }

    #[test]
    fn swap_project_rebuilds_the_exclude_matcher_from_the_session_config() {
        // The mechanism behind "a buffer that BECOMES excluded ends up cleared":
        // the gate is config-derived and rebuilt by `swap_project`, so it follows
        // `st.cfg` on every `invalidate`. It cannot be driven end to end today
        // because `.rigor.yml` is read ONCE at startup (`invalidate` rebuilds from
        // the same `st.cfg`, matching the reference's `ProjectContext#invalidate!`)
        // — so the transition is exercised HERE, at the seam a config-reload slice
        // would feed, rather than claimed untested.
        let p = TempProject::new("exclswap");
        let path = p.write("a.rb", TYPO);
        let buf = buffer_at(&path);
        let (ctx, mut st) = session_for(&p.root, Config::default(), OVERLAY_BUILD_BUDGET_DEFAULT);
        assert!(
            !st.project.exclude.excludes(&buf, None),
            "the control: nothing is excluded under the starting config"
        );

        // The config gains an `exclude:` entry covering the open buffer…
        st.cfg = serde_yaml::from_str("exclude:\n  - \"**/a.rb\"\n").unwrap();
        let index = Arc::clone(&st.project.index);
        let overlay = st.project.overlay.clone();
        swap_project(&ctx, &mut st, index, overlay);

        assert!(
            st.project.exclude.excludes(&buf, None),
            "the rebuilt context's gate follows `st.cfg` — no stale matcher survives"
        );
        assert_eq!(st.project.generation, 1, "and the swap bumped the generation as always");
    }

    #[test]
    fn integration_excluded_buffer_publishes_empty_and_a_sibling_still_fires() {
        // End to end through the real loop: an `exclude:`d buffer gets an EMPTY
        // publish (a publish, not a silent skip — that is what clears the editor's
        // markers), while a non-excluded sibling in the same project still gets its
        // diagnostics. The sibling is the over-broadness control.
        let p = TempProject::new("exclloop");
        p.write("skipped.rb", TYPO);
        p.write("kept.rb", TYPO);
        p.write_config("exclude:\n  - \"**/skipped.rb\"\n");
        let mut h = Harness::start_project(
            Duration::from_secs(30),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );

        h.notify("textDocument/didOpen", open_params(&p.uri("skipped.rb"), TYPO, 1));
        let d = h.recv_diags();
        assert_eq!(d.uri.as_str(), p.uri("skipped.rb"));
        assert!(
            d.diagnostics.is_empty(),
            "an excluded buffer publishes an EMPTY set (clearing any markers): {:?}",
            d.diagnostics
        );

        h.notify("textDocument/didOpen", open_params(&p.uri("kept.rb"), TYPO, 1));
        let d = h.recv_diags();
        assert_eq!(d.uri.as_str(), p.uri("kept.rb"));
        assert_eq!(
            d.diagnostics.len(),
            1,
            "the control: a non-excluded sibling still gets its diagnostics — the \
             filter is not over-broad: {:?}",
            d.diagnostics
        );

        // An `invalidate` (didChangeConfiguration) re-analyses every open buffer:
        // the excluded one must come back EMPTY again, not with regained markers,
        // which is what proves `swap_project` carried the gate across the rebuild.
        h.notify("workspace/didChangeConfiguration", serde_json::json!({ "settings": {} }));
        let mut seen = std::collections::HashMap::new();
        for _ in 0..2 {
            let d = h.recv_diags();
            seen.insert(d.uri.as_str().to_string(), d.diagnostics.len());
        }
        assert_eq!(seen.get(&p.uri("skipped.rb")), Some(&0), "still empty after invalidate");
        assert_eq!(seen.get(&p.uri("kept.rb")), Some(&1), "and the sibling still fires");
        h.shutdown();
    }

    // ---------------------------------------------------------------------
    // Config reload (2026-08-01) — `.rigor.yml` is re-parsed by every structural
    // `invalidate`, so an edit takes effect without restarting the server. Driven
    // end to end: a real file on disk, the real watched-files notification, and
    // assertions on what the server PUBLISHES — a unit test on `reload_config`
    // alone would miss every ordering property below.
    // ---------------------------------------------------------------------

    #[test]
    fn integration_config_reload_disable_takes_effect_without_a_restart() {
        // THE HEADLINE: editing `.rigor.yml` changes the published set in the SAME
        // session. Before this slice the watcher fired, the context rebuilt, and the
        // republished answer was byte-identical to the stale one.
        let p = TempProject::new("cfgreload");
        p.write("t.rb", TYPO);
        let mut h = Harness::start_project(
            Duration::from_secs(30),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );
        h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
        assert_eq!(
            h.recv_diags().diagnostics.len(),
            1,
            "no config yet: the typo fires"
        );

        // The user adds a `disable:` and saves. The editor's watcher names the file.
        p.write_config("disable:\n  - call.undefined-method\n");
        h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
        assert!(
            h.recv_diags().diagnostics.is_empty(),
            "the NEW `disable:` is honoured on the next publish — no restart"
        );

        // …and removing it again restores the diagnostic (the reload is a re-read,
        // not a one-way accumulation of rules).
        p.write_config("paths:\n  - lib\n");
        h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
        assert_eq!(
            h.recv_diags().diagnostics.len(),
            1,
            "dropping `disable:` brings the diagnostic back"
        );
        h.shutdown();
    }

    #[test]
    fn integration_config_reload_honours_a_deleted_config_as_defaults() {
        // DELETING `.rigor.yml` is NOT the broken-file case: absent means the
        // defaults genuinely ARE the configuration, so it reloads to them
        // immediately and says nothing. This is the discriminator that forced
        // `ConfigRead` to separate `Absent` from `Malformed`.
        let p = TempProject::new("cfgdelete");
        p.write("t.rb", TYPO);
        p.write_config("disable:\n  - call.undefined-method\n");
        let mut h = Harness::start_project(
            Duration::from_secs(30),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );
        h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
        assert!(
            h.recv_diags().diagnostics.is_empty(),
            "the on-disk `disable:` is read at STARTUP too"
        );

        p.remove_config();
        h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
        // The very next message is the publish — no `window/showMessage`, because a
        // missing config is not an error to disclose.
        assert_eq!(
            h.recv_diags().diagnostics.len(),
            1,
            "a deleted config reloads to DEFAULTS (not 'keep the last good one')"
        );
        h.shutdown();
    }

    #[test]
    fn integration_malformed_config_keeps_the_last_good_one_and_warns_once() {
        // The case the feature lives or dies on: an editor writes `.rigor.yml` on
        // every save, so the server sees half-written YAML constantly.
        //
        // (a) a broken file keeps the LAST GOOD config — `Config::load`'s one-shot
        //     answer (silently substitute the defaults) would drop the user's whole
        //     `disable:` list and flood the buffer mid-keystroke;
        // (b) the warning fires on the TRANSITION, not per save;
        // (c) fixing the file reloads it and says so.
        let p = TempProject::new("cfgbroken");
        p.write("t.rb", TYPO);
        p.write_config("disable:\n  - call.undefined-method\n");
        let mut h = Harness::start_project(
            Duration::from_secs(30),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );
        h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
        assert!(h.recv_diags().diagnostics.is_empty(), "the good config suppresses it");

        // Save 1 of a half-written file.
        p.write_config("disable: [call.undefined-method\n");
        h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
        let msg = h.recv_show_message();
        assert_eq!(msg.typ, MessageType::WARNING);
        assert!(
            msg.message.contains("keeping the last good configuration"),
            "the disclosure names which config is in force: {}",
            msg.message
        );
        assert!(
            h.recv_diags().diagnostics.is_empty(),
            "(a) the last good `disable:` still suppresses the typo — the defaults \
             would have published it"
        );

        // Save 2, still broken. No second popup: the next message is the publish.
        p.write_config("disable: [call.undefined-method, still-unterminated\n");
        h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
        assert!(
            h.recv_diags().diagnostics.is_empty(),
            "(b) a second broken save re-publishes but does NOT warn again"
        );

        // Fixed — and the fix drops `disable:`, so the diagnostic comes back.
        p.write_config("paths:\n  - lib\n");
        h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
        let msg = h.recv_show_message();
        assert_eq!(msg.typ, MessageType::INFO);
        assert!(
            msg.message.contains("reloaded"),
            "(c) recovery is announced: {}",
            msg.message
        );
        assert_eq!(
            h.recv_diags().diagnostics.len(),
            1,
            "and the fixed config is the one now in force"
        );
        h.shutdown();
    }

    #[test]
    fn integration_config_broken_at_startup_falls_back_to_defaults_and_says_so() {
        // A session that BOOTS on a broken config has no last good one to keep, so
        // it takes the defaults `check` would — but it still records the broken
        // state, so the eventual fix announces itself rather than landing silently.
        let p = TempProject::new("cfgbootbroken");
        p.write("t.rb", TYPO);
        p.write_config("disable: [call.undefined-method\n");
        let mut h = Harness::start_project(
            Duration::from_secs(30),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );
        let msg = h.recv_show_message();
        assert_eq!(msg.typ, MessageType::WARNING);
        assert!(
            msg.message.contains("DEFAULT settings"),
            "startup says DEFAULTS, not 'last good' — there was never a good one: {}",
            msg.message
        );
        h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
        assert_eq!(h.recv_diags().diagnostics.len(), 1, "defaults ⇒ the typo fires");

        p.write_config("disable:\n  - call.undefined-method\n");
        h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
        let msg = h.recv_show_message();
        assert_eq!(msg.typ, MessageType::INFO, "the broken-at-boot state recovers: {msg:?}");
        assert!(
            h.recv_diags().diagnostics.is_empty(),
            "and the now-readable config takes effect"
        );
        h.shutdown();
    }

    #[test]
    fn integration_config_reload_beats_a_worker_already_in_flight() {
        // THE ORDERING PROPERTY. A worker dispatched under the OLD config is in
        // flight when the config changes. Its answer is now wrong, and it is the
        // NEWER message — so publishing it would leave the editor showing markers
        // the user's saved config forbids, permanently (nothing re-dispatches after
        // it). The generation guard the S4 slice built is what covers this: the
        // reload happens inside `invalidate`, which bumps the generation, so the
        // in-flight result is stale on the generation axis exactly as it would be
        // for an index rebuild. No second invalidation mechanism, no new race.
        let p = TempProject::new("cfgflight");
        p.write("t.rb", TYPO);
        let g = gate_recording_hold_gen0();
        let mut h = Harness::start_project(
            Duration::from_secs(30),
            g.gate.clone(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );
        // didOpen → a gen-0 worker spawns and blocks in the gate, having read the
        // config that has no `disable:`.
        h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
        hover_sync(&h, 100, &p.uri("t.rb")); // barrier: the gen-0 worker is in flight.

        p.write_config("disable:\n  - call.undefined-method\n");
        h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
        hover_sync(&h, 101, &p.uri("t.rb")); // barrier: the reload is processed.

        g.release_gen0.send(()).unwrap();
        let d = h.recv_diags();
        assert!(
            d.diagnostics.is_empty(),
            "the in-flight gen-0 result (computed under the OLD config, so carrying \
             the diagnostic) is DROPPED, and the re-dispatch publishes under the new \
             config: {:?}",
            d.diagnostics
        );
        hover_sync(&h, 102, &p.uri("t.rb")); // exactly one publish — no stale follow-up.
        let calls = g.calls.lock().unwrap().clone();
        assert!(
            calls.iter().any(|&(_, genr)| genr == 1),
            "a worker ran under the post-reload generation (proves the drop + \
             re-dispatch, not a lucky publish): {calls:?}"
        );
        h.shutdown();
    }

    #[test]
    fn integration_config_reload_picks_up_a_new_exclude_for_an_open_buffer() {
        // `exclude:` is STAGE-1 in `check` (the file is never analysed), and the LSP
        // reproduces it as an empty publish. It is rebuilt by `swap_project` from
        // `st.cfg`, so it rides the reload with no extra wiring — asserted because
        // "rides for free" is exactly the kind of claim that silently stops being
        // true.
        let p = TempProject::new("cfgexclude");
        p.write("t.rb", TYPO);
        let mut h = Harness::start_project(
            Duration::from_secs(30),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );
        h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
        assert_eq!(h.recv_diags().diagnostics.len(), 1);

        p.write_config("exclude:\n  - \"**/t.rb\"\n");
        h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
        assert!(
            h.recv_diags().diagnostics.is_empty(),
            "a newly-excluded open buffer publishes EMPTY (clearing its markers)"
        );
        h.shutdown();
    }

    #[test]
    fn integration_did_change_configuration_also_re_reads_the_file() {
        // `workspace/didChangeConfiguration` still ignores its client-specific
        // payload, but it no longer rebuilds from the startup parse — it re-reads
        // `.rigor.yml` like any structural invalidation. A client that sends this
        // instead of a watched-file event (several do) gets the same answer.
        let p = TempProject::new("cfgdidchange");
        p.write("t.rb", TYPO);
        let mut h = Harness::start_project(
            Duration::from_secs(30),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );
        h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
        assert_eq!(h.recv_diags().diagnostics.len(), 1);

        p.write_config("disable:\n  - call.undefined-method\n");
        h.notify("workspace/didChangeConfiguration", serde_json::json!({ "settings": {} }));
        assert!(
            h.recv_diags().diagnostics.is_empty(),
            "didChangeConfiguration re-reads the file, not just the context"
        );
        h.shutdown();
    }

    #[test]
    fn integration_config_reload_is_not_triggered_by_a_source_save() {
        // A `.rb` save takes the CHEAP `reharvest_sources` path (review N3), which
        // deliberately does not touch the config — a source file cannot change it.
        // Proven by making the on-disk config disagree with the live one: if a
        // source save reloaded, the still-firing diagnostic would vanish.
        let p = TempProject::new("cfgsrcsave");
        p.write("t.rb", TYPO);
        let mut h = Harness::start_project(
            Duration::from_secs(30),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );
        h.notify("textDocument/didOpen", open_params(&p.uri("t.rb"), TYPO, 1));
        assert_eq!(h.recv_diags().diagnostics.len(), 1);

        p.write_config("disable:\n  - call.undefined-method\n");
        h.notify("workspace/didChangeWatchedFiles", watched_change(&p.uri("t.rb")));
        assert_eq!(
            h.recv_diags().diagnostics.len(),
            1,
            "a source save re-harvests ASTs only — the config surface is untouched"
        );
        // …and the config-file event that follows the save does apply it.
        h.notify("workspace/didChangeWatchedFiles", watched_change(&p.config_uri()));
        assert!(h.recv_diags().diagnostics.is_empty());
        h.shutdown();
    }

    #[test]
    fn uri_path_decoding_round_trips_a_spaced_path() {
        // The overlay's REPLACE lookup keys on the canonical path decoded from the
        // buffer URI, so percent-escapes must decode (an editor sends `%20`).
        assert_eq!(percent_decode("/a/b%20c/d.rb"), "/a/b c/d.rb");
        assert_eq!(percent_decode("/plain/path.rb"), "/plain/path.rb");
        assert_eq!(percent_decode("/bad/%zz.rb"), "/bad/%zz.rb", "invalid escapes pass through");
        let p = TempProject::new("uri");
        let canonical = p.write("with space.rb", "x = 1\n");
        let uri: Uri = format!("file://{}", p.root.join("lib").join("with%20space.rb").display())
            .parse()
            .unwrap();
        assert_eq!(uri_to_canonical_path(&uri).as_deref(), Some(canonical.as_path()));
        // A non-`file:` URI, or one whose DIRECTORY does not exist, has no on-disk
        // identity ⇒ `None` ⇒ the overlay APPENDS the buffer instead of replacing.
        let untitled: Uri = "untitled:Untitled-1".parse().unwrap();
        assert!(uri_to_canonical_path(&untitled).is_none());
        let missing: Uri = "file:///no/such/directory/anywhere.rb".parse().unwrap();
        assert!(uri_to_canonical_path(&missing).is_none());

        // But a file that is GONE from an EXISTING directory still resolves (review
        // B1): it keeps the identity tier 1 recorded, so the REPLACE lookup hits.
        let gone = p.write("gone.rb", "x = 1\n");
        std::fs::remove_file(&gone).unwrap();
        let gone_uri: Uri = p.uri("gone.rb").parse().unwrap();
        assert_eq!(
            uri_to_canonical_path(&gone_uri).as_deref(),
            Some(gone.as_path()),
            "a deleted file keeps its canonical identity via its parent directory"
        );
    }

    /// PROBE A [BLOCKING]: `rm -rf` of a SUBDIRECTORY holding project files.
    /// `uri_to_canonical_path` resolves a deleted file via its PARENT; when the
    /// parent directory is gone too it returns `None`, and `reharvest_sources`
    /// `continue`s (lsp.rs:1070) — so the stale AST is never removed. The pre-N3
    /// full rebuild dropped it.
    #[test]
    fn probe_a_directory_deletion_leaves_stale_asts() {
        let p = TempProject::new("probe_a");
        std::fs::create_dir_all(p.root.join("lib/nested")).unwrap();
        std::fs::write(p.root.join("lib/nested/base.rb"), BASE_RB).unwrap();
        p.write("sub.rb", SUB_RB);
        let mut h = Harness::start_project(
            Duration::from_secs(30),
            production_gate(),
            serde_json::json!({}),
            p.root.clone(),
            OVERLAY_BUILD_BUDGET_DEFAULT,
        );
        h.notify("textDocument/didOpen", open_params(&p.uri("sub.rb"), SUB_RB, 1));
        assert_eq!(h.recv_diags().diagnostics.len(), 1, "override fires against Base");

        // rm -rf lib/nested  (a `git checkout` that drops a directory)
        std::fs::remove_dir_all(p.root.join("lib/nested")).unwrap();
        let gone = format!("file://{}", p.root.join("lib/nested/base.rb").display());
        h.notify(
            "workspace/didChangeWatchedFiles",
            serde_json::json!({ "changes": [ { "uri": gone, "type": 3 } ] }),
        );
        let d = h.recv_diags();
        assert!(
            d.diagnostics.is_empty(),
            "PROBE A FAILED: stale AST for a file in a deleted DIRECTORY survives \
             the incremental re-harvest: {:?}",
            d.diagnostics
        );
        h.shutdown();
    }

    /// PROBE D [BLOCKING, fixed]: the guard's OFF->ON flip used to happen in
    /// `handle_result`, which never re-installs the overlay. Samples arrive from
    /// workers dispatched BEFORE the disable-swap (a concurrent per-URI dispatch, a
    /// buffer closed mid-flight), so under-budget stragglers after a trip re-ENABLED
    /// the guard while `project.overlay` was still `None` — terminal, because with
    /// no overlay no further sample is ever produced, and the user was told
    /// "re-enabled ... for 0 files" (the count read from the already-emptied
    /// overlay).
    ///
    /// The fix: worker samples are IGNORED while the guard is disabled. They
    /// necessarily predate the disable-swap, so they carry no information about the
    /// current posture. Recovery lives in `invalidate`, where the overlay is being
    /// rebuilt anyway (see the companion test below).
    #[test]
    fn probe_d_worker_samples_are_ignored_while_the_guard_is_disabled() {
        let p = TempProject::new("probe_d");
        p.write("base.rb", BASE_RB);
        p.write("sub.rb", SUB_RB);
        let (server_conn, _client) = Connection::memory();
        let (ctx, mut st) = session_for(&p.root, Config::default(), Duration::from_millis(100));
        assert_eq!(st.project.overlay.as_ref().unwrap().files.len(), 2);

        // A result whose buffer is not open is DROPPED for publishing but still
        // reaches the guard — the minimal reachable shape of "a sample from a
        // dispatch that predates the current posture".
        let uri: Uri = p.uri("sub.rb").parse().unwrap();
        let feed = |st: &mut Session, ms: u64| {
            handle_result(
                &server_conn,
                &ctx,
                st,
                Computed {
                    uri: uri.clone(),
                    version: 1,
                    generation: 0,
                    epoch: 0,
                    diags: Vec::new(),
                    overlay_build: Some(Duration::from_millis(ms)),
                },
            )
            .unwrap();
        };
        feed(&mut st, 150); // over #1
        feed(&mut st, 150); // over #2 -> Disabled, overlay dropped
        assert!(!st.guard.enabled, "two over-budget samples disable");
        assert!(st.project.overlay.is_none(), "the held ASTs are dropped");

        // Stragglers must NOT flip the posture from here.
        feed(&mut st, 10);
        feed(&mut st, 10);
        assert!(
            !st.guard.enabled,
            "a worker sample that predates the disable cannot re-enable the guard"
        );
        assert!(
            st.project.overlay.is_none(),
            "and the overlay must never be ENABLED-but-empty (that state is terminal)"
        );
        drop(st);
    }

    #[test]
    fn guard_recovers_through_a_structural_rebuild() {
        // The companion to probe D: recovery is real, and it happens where the
        // overlay is rebuilt anyway. A single under-budget sample re-enables
        // (asymmetric hysteresis), the freshly built ASTs are installed, and the
        // disclosure reports the count from the NEW overlay — not 0.
        let p = TempProject::new("recover");
        p.write("base.rb", BASE_RB);
        p.write("sub.rb", SUB_RB);
        let (mut ctx, mut st) = session_for(&p.root, Config::default(), Duration::ZERO);

        // Trip it with two over-budget samples (budget ZERO => every sample is over).
        let (server_conn, _client) = Connection::memory();
        let uri: Uri = p.uri("sub.rb").parse().unwrap();
        for _ in 0..2 {
            handle_result(
                &server_conn,
                &ctx,
                &mut st,
                Computed {
                    uri: uri.clone(),
                    version: 1,
                    generation: 0,
                    epoch: 0,
                    diags: Vec::new(),
                    overlay_build: Some(Duration::from_millis(1)),
                },
            )
            .unwrap();
        }
        assert!(!st.guard.enabled && st.project.overlay.is_none(), "tripped");

        // A structural invalidation under a generous budget: ONE under-budget
        // sample restores the overlay.
        ctx.overlay_budget = Duration::from_secs(30);
        // `invalidate` can owe more than one disclosure now (a config-reload state
        // change, then the guard flip); pick the guard's out of the batch.
        let disclosures = invalidate(&ctx, &mut st);
        let msg = disclosures
            .iter()
            .map(|(_, m)| m.clone())
            .find(|m| m.contains("re-enabled"))
            .unwrap_or_else(|| panic!("a posture flip discloses: {disclosures:?}"));
        assert!(st.guard.enabled, "one under-budget rebuild re-enables");
        let restored = st.project.overlay.as_ref().expect("the overlay is re-installed");
        assert_eq!(restored.files.len(), 2, "with the freshly harvested ASTs");
        assert!(
            msg.contains("re-enabled") && msg.contains("2 files"),
            "the disclosure reports the NEW overlay's count, not the emptied one: {msg}"
        );
    }


    /// PROBE E [BLOCKING]: `touches_configured_root` compares the URI's DECODED
    /// spelling against the CANONICALIZED configured root. When the two spellings
    /// differ — a workspace reached through a symlink, the normal macOS shape
    /// (`/tmp` -> `/private/tmp`) — and the path cannot be canonicalized (a DELETED
    /// DIRECTORY, the only case where the decoded spelling is the sole candidate),
    /// the predicate answers "ignore" for an event a full rebuild WOULD have acted
    /// on, and the stale AST survives. B-1's residue.
    #[test]
    fn probe_e_alias_spelled_uri_for_a_deleted_directory_is_wrongly_ignored() {
        let p = TempProject::new("probe_e");
        p.write("a.rb", "class A\nend\n");
        std::fs::create_dir_all(p.root.join("lib/nested")).unwrap();
        std::fs::write(p.root.join("lib/nested/c.rb"), "class C\nend\n").unwrap();
        let alias = p.root.parent().unwrap().join(format!(
            "{}_alias",
            p.root.file_name().unwrap().to_string_lossy()
        ));
        let _ = std::fs::remove_file(&alias);
        std::os::unix::fs::symlink(&p.root, &alias).unwrap();

        let cfg = Config::default(); // paths: ["lib"]
        let cfg2 = Config::default();
        let (ctx, mut st) = session_for(&p.root, cfg2, OVERLAY_BUILD_BUDGET_DEFAULT);
        let index = Arc::new(CoreIndex::new());
        assert_eq!(st.project.overlay.as_ref().unwrap().files.len(), 2, "a.rb + nested/c.rb");

        // rm -rf lib/nested, announced under the ALIAS spelling.
        std::fs::remove_dir_all(p.root.join("lib/nested")).unwrap();
        let gone = format!("file://{}", alias.join("lib/nested/c.rb").display());
        let uri: Uri = gone.parse().unwrap();
        assert!(
            uri_to_canonical_path(&uri).is_none(),
            "precondition: a deleted directory leaves no canonical form"
        );
        assert!(
            !watched_event_is_ignorable(&ctx, &st.cfg, None, &uri),
            "PROBE E FAILED (rule): the event is treated as ignorable although a full \
             rebuild would act on it, so the stale entry is never dropped"
        );

        reharvest_sources(&ctx, &mut st, &[gone]);
        let truth = build_overlay(&p.root, &cfg, &index);
        let held = st.project.overlay.as_ref().unwrap();
        let _ = std::fs::remove_file(&alias);
        assert_eq!(
            overlay_fingerprint(held),
            overlay_fingerprint(&truth.files),
            "PROBE E FAILED (state): incremental kept a stale AST for a file in a \
             deleted directory"
        );
    }

    /// PROBE F [BLOCKING, fixed]: the LITERAL production shape — `project_root =
    /// "."` with the workspace entered through a SYMLINK, as an editor launched
    /// from the user-visible path gives it.
    ///
    /// This shape used to be the one the scope comparison could not survive. With
    /// `project_root = "."`, `join_root` yields the RELATIVE `"lib"`, so the
    /// literal-root comparison could never match an absolute candidate and the
    /// decision rested entirely on the canonicalized root — against which the URI's
    /// decoded, symlink-spelled path does not match either. A deleted-directory
    /// event was therefore judged out of scope and its stale AST kept. (That
    /// literal-root arm is now deleted, and the ignore rule short-circuits before
    /// any comparison when the path does not resolve — see
    /// `watched_event_is_ignorable`.)
    ///
    /// The assertion is cwd-independent TODAY only because of that short-circuit,
    /// so the production cwd shape is still set up deliberately: if the
    /// short-circuit is ever removed, this test must go back to exercising the
    /// comparison under the spelling that broke it.
    #[test]
    fn probe_f_production_root_shape_never_ignores_a_symlinked_deleted_directory() {
        let p = TempProject::new("probe_f");
        p.write("a.rb", "class A\nend\n");
        std::fs::create_dir_all(p.root.join("lib/nested")).unwrap();
        std::fs::write(p.root.join("lib/nested/c.rb"), "class C\nend\n").unwrap();
        // Declared before the cwd guard so it is dropped AFTER it: the cwd is
        // restored first, then the symlink is removed.
        let alias = AliasLink::to(&p.root);
        // Enter the workspace through the SYMLINK. `CwdGuard` holds the process-wide
        // cwd mutex for the rest of this scope and restores the previous directory
        // on drop — including on a panic — so no concurrently-running test can
        // observe (or be stranded by) this mutation.
        let _cwd = CwdGuard::enter(alias.path());

        let ctx = ServerContext {
            debounce: Duration::from_secs(30),
            worker_gate: production_gate(),
            watched_files_dynamic_registration: false,
            project_root: PathBuf::from("."), // <- production
            overlay_budget: OVERLAY_BUILD_BUDGET_DEFAULT,
        };
        let cfg = Config::default();
        std::fs::remove_dir_all(p.root.join("lib/nested")).unwrap();
        let gone = format!("file://{}", alias.path().join("lib/nested/c.rb").display());
        let uri: Uri = gone.parse().unwrap();
        let canonical = uri_to_canonical_path(&uri);
        assert!(canonical.is_none(), "precondition: deleted directory");
        assert!(
            !watched_event_is_ignorable(&ctx, &cfg, canonical.as_deref(), &uri),
            "PROBE F FAILED: under the production root shape a deleted-directory \
             event spelled through the workspace symlink is IGNORED"
        );
    }

}
