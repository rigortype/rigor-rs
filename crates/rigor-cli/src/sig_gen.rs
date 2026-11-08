//! `rigor sig-gen [options] [paths]` (ADR-14) — RBS skeleton generation.
//!
//! ## Slice scope (this port)
//!
//! The `--print` mode over **instance methods in a named `class` / `module`
//! body**: it walks each source file, infers every qualifying method's RETURN
//! type via the same [`Typer`] path `check`/`annotate` use, and prints an RBS
//! skeleton (`def name: (untyped, …) -> <erased return>`) grouped by file +
//! class. Return types render through the shared reference-faithful
//! [`crate::type_display::erase`] layer ([`rigor_types::erase_to_rbs_named`]).
//!
//! ## Parity model — byte-identical on the agreeing subset, sound-superset overall
//!
//! The one HARD guarantee is byte-identity on the methods BOTH tools emit
//! (`rbs` verified against the oracle). The emitted SETS differ by inference
//! precision, and that is BY DESIGN — see AGENTS.md "Generative-tool parity":
//! - rigor-rs types a method body against the top-level env (no per-method
//!   `ScopeIndexer`), so a def-LOCAL binding types `Dynamic` and is SKIPPED where
//!   the reference's scope pins it — rigor-rs emits FEWER (a coverage gap).
//! - conversely, rigor-rs's inference is more ROBUST on shapes the reference
//!   degrades to `untyped`/nil (a string-interpolation return, a `%i[]` word
//!   array, a top-level project-class `.new` → its instance). There rigor-rs
//!   emits a SOUND signature the reference skips — that excess is coverage, NOT
//!   a false bug report, and we TRACK it (the reference converges as it gains
//!   precision) rather than suppress it with anti-convergence guards.
//!
//! **Confidence rule** (sweep-proven refinement): the sound-superset excess
//! applies only to CONFIDENT types — any `untyped` inside a member (whole or
//! buried in a composite, `[untyped, 0]`) marks a precision hole where the
//! reference reads the same code differently, a shared-method mismatch source
//! (`Baseline#filter`), so such members skip the method.
//!
//! The remaining guards are the three AGENTS.md sanctions: fix a rigor-rs UNSOUND
//! emit (`initialize` typed as its body → skip; a `module_function` module's
//! methods — the reference spells them `def self?.name` — skip until that
//! spelling is ported), match a reference PERMANENT skip (`dynamic_top?`,
//! the block/lambda/def return barrier, multi-value-return methods are skipped
//! rather than adopt the reference's silent type drop), or avoid a WRONG emit
//! from an unported rigor-rs LIMITATION (a bare generic nominal the reference
//! *elaborates* to `Array[untyped]`).
//!
//! A source-class instance return is rendered FULLY-QUALIFIED
//! (`Rigor::Triage::Selector`, `Outer::Inner`) by [`erase_qualified`]: the file's
//! declared class/module + `Data.define`/`Struct.new` constant FQNs
//! ([`collect_source_fqns`]) resolved from the method's enclosing scope via Ruby
//! constant lookup ([`qualify_source_name`]). Because the sig-gen `SourceIndex`
//! is per-file, every source class that types to a Nominal is defined HERE, so
//! its FQN is always in the set. Candidates emit + descend in ONE source-order
//! (span) pass so a nested class declared before the outer's own methods groups
//! ahead of its parent (reference walk order).
//!
//! ## Value classes (`Data.define` / `Struct.new`) — upstream rigor#227
//!
//! Both spellings (`Point = Data.define(:x, :y)` and
//! `class Point < Data.define(:x, :y)`) generate a full declaration: the member
//! readers, a Struct's writers, a `.new` / `.[]` pair matching the constructor
//! forms the class actually accepts, and the `::Data` / `::Struct[untyped]`
//! ancestry ([`meta_class`]). A `do ... end` block's defs bind on the NEW class,
//! not the enclosing namespace, and every printed group carries the source's own
//! `class` / `module` keyword ([`declaration_header`]) — hard-coding `class`
//! turned a module into a `DuplicatedDeclarationError` the moment it held an
//! emittable method. The member types stay `untyped` (upstream types them from
//! `--params=observed`, deferred below), so the emit is byte-identical to
//! upstream master here.
//!
//! ## Deferred (later slices, each its own gate)
//!
//! - `--params=observed` (the `ObservationCollector`) — params stay `untyped`.
//!   NOTE: this is what makes the `--overwrite` `NEW_METHOD`-tightens-`untyped`
//!   replacement path (ported faithfully) actually fire — until it lands, that
//!   path is dead for BOTH tools (an initialize stub stays `(untyped) -> void`),
//!   so its absence is parity-safe. It ALSO types a value class's members, so
//!   until then [`meta_class`] renders every member `untyped`;
//! - `attr_*` reader generation;
//! - `TypeElaborator`'s generic-arity fill (`Array` → `Array[untyped]`);
//! - `Struct.new` / non-core-named `Data.define` constant RECEIVER typing — a
//!   `Const.new` types to a source class (⇒ qualified) only when `Const` collides
//!   with a core RBS name; otherwise it stays `Dynamic` and the method is skipped
//!   (an under-emit — a pre-existing inference gap, not a naming defect).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ruby_rbs::node::{MethodDefinitionKind, Node as RbsNode, RBSLocationRange};

use rigor_index::CoreIndex;
use rigor_infer::{SourceIndex, TypeEnv, Typer};
use rigor_parse::{lower, parse_bounded, LoweredAst, Node, NodeId, ParamShape, Visibility};
use rigor_types::{ClassId, Interner, Type, TypeId};

mod meta_class;
mod generics;
mod rbs_parse_error;
mod superclasses;
mod sig_env;
mod module_function;
mod top_level;
use meta_class::{MetaLayouts, MetaMember};
use sig_env::{Lookup, SigEnv};

/// A collected method to consider (instance or singleton) — the fields
/// `method_candidate` needs, unifying the instance-harvest (`MethodBody`) and the
/// singleton walk (`Node::Definition` fields) into one shape.
struct MethodSig<'a> {
    name: &'a str,
    body: &'a [NodeId],
    params: &'a Option<Vec<String>>,
    /// The full parameter structure — only consumed by the `initialize` stub.
    param_shape: &'a ParamShape,
    has_explicit_return: bool,
    /// `true` for a `def self.x` / `class << self` def — rendered `def self.name`,
    /// kind `"singleton"`, and NOT subject to the visibility / `initialize` skips
    /// (both instance-only in the reference).
    singleton: bool,
    /// `true` for an instance def that a bare `module_function` earlier in the
    /// same body made dual — rendered `def self?.name` (reference
    /// `method_def_prefix` / `@module_function_methods`). Kind stays `instance`.
    module_function: bool,
    /// The def node's source offset — the definer-table rank (last wins).
    offset: usize,
}

/// One printable RBS skeleton row (the reference's emittable `MethodCandidate`,
/// always `NEW_METHOD` in the `--print` path — `NEW_FILE` is a `--write` concept).
#[derive(Debug)]
struct Candidate {
    file: String,
    class_name: String,
    method_name: String,
    /// `"instance"` or `"singleton"`.
    kind: &'static str,
    /// The rendered one-liner, e.g. `def greeting: () -> "hello"`.
    rbs: String,
    /// The raw inferred return erased to RBS (the JSON `inferred_return` field).
    inferred_return: String,
    /// The generation-time classification (`"new_method"` or `"tighter_return"`)
    /// decided against the project's own RBS via [`SigEnv`] (ADR-14 slice 10).
    /// `"equivalent"` candidates are never constructed (dropped, as the reference
    /// filters them out) so the field is one of exactly these two.
    classification: &'static str,
    /// For a `tighter_return`, the declared return's erased RBS string (the
    /// `# [tighter, was: X]` tag / `- def …` diff line / JSON `declared_return_rbs`);
    /// `None` for a `new_method`.
    declared_return_rbs: Option<String>,
    /// The `--print` declaration line for this candidate's class group
    /// (`module Geometry`, `class Geometry::Pair < ::Data`) — see
    /// [`declaration_header`]. Stamped once the whole file's [`NamespaceInfo`] is
    /// known, so every candidate of a group carries the same string and the
    /// renderer can take the group's first (reference `declaration_header`, which
    /// reads the same per-file maps off `methods.first`).
    decl_header: String,
    /// The `sig.skipped.*` reason of a `"skipped"` row (reference
    /// `MethodCandidate#skip_reason`), counted by [`report_skipped`]. Rows the
    /// generator built but does not emit for a reason the reference does not
    /// share (an equivalent declaration, a port-only confidence skip) are
    /// `"dropped"` with no reason. Neither kind leaves [`generate_run`].
    skip_reason: Option<&'static str>,
    /// The `def` node this candidate came from: `(source offset, module_function)`.
    /// `None` for meta members, stubs and skip rows. Feeds the
    /// [`live_definitions`] slot pass (reference `definer_table` /
    /// `live_definitions`, #2008/#2023).
    definer: Option<(usize, bool)>,
}

/// `rigor sig-gen [--print] [--format text|json] [--include-private] [--config PATH] [paths]`.
/// Exit 0 on success, 64 on a usage error, 2 for a not-yet-ported mode.
pub fn cmd_sig_gen(args: &[String]) -> ExitCode {
    // Reference `SigGenCommand#build_option_parser`, `opts.on` order.
    // `--format`/`--params` are RAW (the reference validates them post-parse
    // in `validation_error`, printing `sig-gen: <msg>` + exit 64); the mode
    // flags are last-wins (`options[:mode] = …`).
    use crate::optparse::{ArgStyle, Item, OptParser, Switch, ValueKind};
    const SWITCHES: &[Switch] = &[
        Switch::new("print", &[("print", false)], ArgStyle::Flag, ValueKind::Raw, "--print", "", &["Write RBS skeletons to stdout (default)"]),
        Switch::new("diff", &[("diff", false)], ArgStyle::Flag, ValueKind::Raw, "--diff", "", &["Write a unified diff against existing RBS"]),
        Switch::new("write", &[("write", false)], ArgStyle::Flag, ValueKind::Raw, "--write", "", &["Write generated RBS to sig/<path>.rbs files"]),
        Switch::new("check", &[("check", false)], ArgStyle::Flag, ValueKind::Raw, "--check", "", &["Exit 1 when --write would change sig/; write nothing"]),
        Switch::new("overwrite", &[("overwrite", false)], ArgStyle::Flag, ValueKind::Raw, "--overwrite", "", &["Allow tighter-return updates, and inline declarations that disagree with sig/, to replace user-authored RBS"]),
        Switch::new("include-private", &[("include-private", false)], ArgStyle::Flag, ValueKind::Raw, "--include-private", "", &["Emit private / protected instance methods (default: public only)"]),
        Switch::new("effect-envelopes", &[("effect-envelopes", false)], ArgStyle::Flag, ValueKind::Raw, "--effect-envelopes", "", &["Also emit %a{rigor:v1:effect ...} for effectful methods (requires the effects: opt-in)"]),
        Switch::new("no-cache", &[("no-cache", false)], ArgStyle::Flag, ValueKind::Raw, "--no-cache", "", &["Do not read or write the analysis cache (effect collection only)"]),
        Switch::new("format", &[("format", false)], ArgStyle::Required, ValueKind::Exact(&["text", "json"]), "--format", "=FORMAT", &["Output format: text or json"]),
        Switch::new("params", &[("params", false)], ArgStyle::Required, ValueKind::Raw, "--params", "=POLICY", &["Parameter policy: untyped (default), observed, observed-strict"]),
        Switch::new("observe", &[("observe", false)], ArgStyle::Required, ValueKind::Raw, "--observe", "=PATH", &["Directory / file to scan for call-site observations (repeatable; default: the configured test_paths)"]),
        Switch::new("new-files", &[("new-files", false)], ArgStyle::Flag, ValueKind::Raw, "--new-files", "", &["Emit only new-file classifications"]),
        Switch::new("new-methods", &[("new-methods", false)], ArgStyle::Flag, ValueKind::Raw, "--new-methods", "", &["Emit only new-method classifications"]),
        Switch::new("tighter-returns", &[("tighter-returns", false)], ArgStyle::Flag, ValueKind::Raw, "--tighter-returns", "", &["Emit only tighter-return classifications"]),
        Switch::new("config", &[("config", false)], ArgStyle::Required, ValueKind::Raw, "--config", "=PATH", &["Path to the Rigor configuration file"]),
    ];
    const PARSER: OptParser =
        OptParser::new("Usage: rigor sig-gen [options] [paths]", SWITCHES);

    let items = match PARSER.parse(args).items_or_exit() {
        Ok(items) => items,
        Err(code) => return code,
    };
    let mut format = String::from("text");
    let mut params = String::from("untyped");
    let mut include_private = false;
    let mut mode = "print";
    let mut overwrite = false;
    let mut explicit_config: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut mode_given = false;
    // The first flag naming machinery this slice does not port (in argv order,
    // so the diagnostic names the first offender).
    let mut deferred: Option<String> = None;
    let defer = |flag: &str, deferred: &mut Option<String>| {
        if deferred.is_none() {
            *deferred = Some(flag.to_string());
        }
    };
    for item in items {
        match item {
            Item::Positional(p) => positional.push(p),
            Item::Opt { key, value, .. } => match key {
                "print" | "diff" | "write" | "check" => {
                    // `select_mode`: a second, different mode flag is a usage
                    // error, not last-one-wins.
                    if mode_given && mode != key {
                        mode = "conflict";
                    }
                    mode_given = true;
                    if mode != "conflict" {
                        mode = key;
                    }
                }
                "overwrite" => overwrite = true,
                "include-private" => include_private = true,
                // No persistent analysis cache exists in the port.
                "no-cache" => {}
                "format" => format = value.unwrap().as_str().to_string(),
                "params" => params = value.unwrap().as_str().to_string(),
                "observe" => defer("--observe", &mut deferred),
                "effect-envelopes" => defer("--effect-envelopes", &mut deferred),
                "new-files" => defer("--new-files", &mut deferred),
                "new-methods" => defer("--new-methods", &mut deferred),
                "tighter-returns" => defer("--tighter-returns", &mut deferred),
                "config" => match crate::config::checked_config_path(value.unwrap().as_str()) {
                    Ok(path) => explicit_config = Some(path),
                    Err(code) => return code,
                },
                _ => unreachable!("the switch table is closed"),
            },
        }
    }

    // `validation_error` — inside `parse_options`, before `Configuration.load`,
    // each failure a `sig-gen: <msg>` line + exit 64.
    if mode == "conflict" {
        eprintln!("sig-gen: --print, --diff, --write, and --check are mutually exclusive flags; pick one");
        return ExitCode::from(64);
    }
    if !matches!(format.as_str(), "text" | "json") {
        eprintln!("sig-gen: unsupported --format={format}");
        return ExitCode::from(64);
    }
    if !matches!(params.as_str(), "untyped" | "observed" | "observed-strict") {
        eprintln!("sig-gen: unsupported --params={params}");
        return ExitCode::from(64);
    }
    if params == "observed-strict" {
        eprintln!("sig-gen: --params=observed-strict is reserved until the capability-role catalog ships");
        return ExitCode::from(64);
    }
    if params == "observed" {
        defer("--params=observed", &mut deferred);
    }
    if let Some(flag) = deferred {
        eprintln!("sig-gen: `{flag}` is not yet implemented in this slice");
        return ExitCode::from(2);
    }

    // Paths: positional args, or config `paths:` when none are supplied
    // (reference `@argv.empty? ? configuration.paths : @argv`).
    let cfg = match crate::Config::load(explicit_config.as_deref().map(Path::new)) {
        Ok(c) => c,
        Err(f) => return f.report(),
    };
    let config_paths: Vec<String>;
    let raw: Vec<&str> = if positional.is_empty() {
        config_paths = cfg.paths.clone();
        config_paths.iter().map(String::as_str).collect()
    } else {
        positional.iter().map(String::as_str).collect()
    };
    let files = resolve_paths(&raw);
    // Upstream v0.4.6 (#1991 / #2204, `path_diagnostics`): a path that names
    // neither a directory nor a `.rb` file is reported as `rigor check`
    // reports it — a warning `(skipped)` beside files that will be read, an
    // error (exit 1) when nothing will be. JSON print/diff carries them in the
    // payload; every other form says them on stderr.
    let path_errors = path_diagnostics(&raw, !files.is_empty());
    let errors_in_payload = format == "json" && mode != "write";
    if !errors_in_payload {
        for (path, severity, message) in &path_errors {
            eprintln!("{path}:1:1: {severity}: {message}");
        }
    }
    let path_status = if path_errors.iter().any(|(_, sev, _)| *sev == "error") {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    };

    // The sig-gen-local, FQN-keyed declaration env, built ONCE from the project's
    // own `.rbs` under the configured signature dirs (ADR-14 slice 10). Drives
    // generation-time `new_method` / `tighter_return` classification. Sig-gen-local
    // by construction — the `check` path never sees it.
    let project_root = std::env::current_dir()
        .and_then(|d| d.canonicalize())
        .unwrap_or_else(|_| PathBuf::from("."));
    let sig_env = SigEnv::build(&cfg.all_signature_dirs(&project_root));

    if mode == "write" || mode == "check" {
        // ADR-112 WD4 — `--check` is the writer's dry-run compare mode: same
        // routing and merge decisions, nothing reaches disk, and the verdict
        // is the exit status.
        let status = cmd_write(&files, include_private, &format, overwrite, &cfg, &sig_env, mode == "check");
        return if path_status != ExitCode::SUCCESS { path_status } else { status };
    }

    // `--overwrite` only affects the write path (it governs replacing an existing
    // declaration during merge); on a print/diff run it is inert, exactly like the
    // reference (the flag lives on the Writer).

    let Generated { candidates, all_rows, unresolvable, skips, .. } =
        generate_run(&files, include_private, &sig_env, &writes);

    // `--format json` renders the candidate table regardless of print/diff mode
    // (reference `Renderer#render`); text picks the diff or print layout.
    match (format.as_str(), mode == "diff") {
        ("json", _) if !path_errors.is_empty() => {
            // The reference's key order (`Diagnostic#to_h`), which a
            // `serde_json::Value` map would sort.
            let head = candidates_json_string(&all_rows);
            let head = head.strip_suffix("\n}").unwrap_or(&head);
            let q = |v: &str| serde_json::to_string(v).unwrap();
            let rows: Vec<String> = path_errors
                .iter()
                .map(|(path, severity, message)| {
                    format!(
                        "    {{\n      \"path\": {},\n      \"line\": 1,\n      \"column\": 1,\n      \
                         \"severity\": {},\n      \"rule\": null,\n      \"source_family\": \"builtin\",\n      \
                         \"message\": {}\n    }}",
                        q(path),
                        q(severity),
                        q(message)
                    )
                })
                .collect();
            println!("{head},\n  \"diagnostics\": [\n{}\n  ]\n}}", rows.join(",\n"));
        }
        ("json", _) => render_json(&all_rows),
        (_, true) => render_diff(&candidates),
        (_, false) => render_text(&candidates),
    }
    if format == "text" {
        report_skipped(&skips);
    }
    report_unresolvable_superclasses(&unresolvable);
    path_status
}

/// `PathExpansion.unusable` + `diagnostics`: `(path, severity, message)` for
/// each path that names neither a directory nor a `.rb` file.
fn path_diagnostics(raw: &[&str], any_files: bool) -> Vec<(String, &'static str, String)> {
    let (severity, suffix) = if any_files { ("warning", " (skipped)") } else { ("error", "") };
    raw.iter()
        .filter(|p| {
            let path = Path::new(p);
            !(path.is_dir() || (path.is_file() && p.ends_with(".rb")))
        })
        .map(|p| {
            let message = if Path::new(p).exists() {
                "not a Ruby file (expected `.rb` or a directory)"
            } else {
                "no such file or directory"
            };
            (p.to_string(), severity, format!("{message}{suffix}"))
        })
        .collect()
}

/// Resolve path args to `.rb` files (reference `Generator#resolve_paths`): a
/// directory expands to its sorted `**/*.rb`, a `.rb` file passes through, and
/// anything else is silently skipped; the result is de-duplicated preserving
/// order.
fn resolve_paths(raw: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for &p in raw {
        let path = Path::new(p);
        if path.is_dir() {
            let mut in_dir = Vec::new();
            crate::collect_rb_files(path, &mut in_dir);
            in_dir.sort();
            out.extend(in_dir);
        } else if path.is_file() && p.ends_with(".rb") {
            out.push(p.to_string());
        }
    }
    out.dedup();
    out
}

/// Per-namespace metadata the `--write` tree renderer needs (unused by `--print`):
/// the declaration keyword and any plain-constant superclass, keyed by the fully-
/// qualified name.
#[derive(Default)]
struct NamespaceInfo {
    /// qualified name → `"class"` / `"module"` (reference `node_keyword`).
    kinds: std::collections::HashMap<String, &'static str>,
    /// qualified name → written superclass path (reference `superclass_suffix`).
    supers: std::collections::HashMap<String, String>,
    /// FQNs of the file's `Data.define` / `Struct.new` classes, in layout order —
    /// the reference `--write`s an empty `class Const\nend` SHELL for each so the
    /// class exists even when every member candidate is suppressed as
    /// already-declared (reference `@class_shells`).
    shells: Vec<String>,
    /// Every class header with a plain-constant superclass (whole-run once
    /// merged); resolved into `supers` by [`finalize_superclasses`].
    headers: Vec<superclasses::Header>,
    /// `(source file, shell FQN)` — which file carries each shell, for the
    /// written-set computation (a shell rides on its file's written candidates).
    shell_files: Vec<(String, String)>,
    /// qualified name → generic header spelling (`[unchecked out E]`), the
    /// run's `stamp_generic_reopenings` (#2436/#2473).
    params: HashMap<String, String>,
    /// The core-subclass shells (#2224) — written only beside a written
    /// method of their file, never as a shell-only target.
    core_shells: HashSet<String>,
}

impl NamespaceInfo {
    /// Fold one file's metadata into a whole-run one.
    fn absorb(&mut self, other: NamespaceInfo) {
        self.kinds.extend(other.kinds);
        self.supers.extend(other.supers);
        self.shells.extend(other.shells);
        self.headers.extend(other.headers);
        self.shell_files.extend(other.shell_files);
        self.params.extend(other.params);
        self.core_shells.extend(other.core_shells);
    }
}

/// Record each class/module declaration's keyword + superclass into `info`,
/// keyed by qualified name (reference `build_namespace_kinds` /
/// `build_superclasses`).
fn collect_namespace_info(ast: &LoweredAst, id: NodeId, prefix: &[String], info: &mut NamespaceInfo) {
    let (name, rooted, body, kind, superclass): (&String, bool, &[NodeId], &'static str, Option<&String>) =
        match ast.get(id) {
            Node::ClassDef { name, rooted, body, superclass_path, .. } => {
                (name, *rooted, body, "class", superclass_path.as_ref())
            }
            Node::ModuleDef { name, rooted, body, .. } => (name, *rooted, body, "module", None),
            _ => return,
        };
    let qualified = declaration_prefix(prefix, name, rooted);
    let q = qualified.join("::");
    info.kinds.insert(q.clone(), kind);
    // A plain-constant superclass is recorded from the Prism tree
    // ([`superclasses::collect`], rooted-aware) and resolved whole-run.
    let _ = superclass;
    for &child in body {
        collect_namespace_info(ast, child, &qualified, info);
    }
}

/// Generate every file's candidates and finalize the run's superclasses
/// ([`finalize_superclasses`]); the unresolvable-superclass report goes to
/// stderr after the output, via [`report_unresolvable_superclasses`].
fn generate_run(
    files: &[String],
    include_private: bool,
    sig_env: &SigEnv,
    written: &dyn Fn(&Candidate) -> bool,
) -> Generated {
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut info = NamespaceInfo::default();
    for f in files {
        let (cs, i) = generate_file_with_info(f, include_private, sig_env);
        candidates.extend(cs);
        info.absorb(i);
    }
    let (candidates, all_rows, skips, report) = finalize_superclasses(candidates, &mut info, sig_env, written);
    Generated { candidates, all_rows, info, unresolvable: report, skips }
}

/// One run's output: the emittable candidates (`new_method` / `tighter_return`),
/// the run's namespace metadata, the unresolvable-superclass report, and the
/// skip reason of every `"skipped"` row in candidate order (for
/// [`report_skipped`]).
struct Generated {
    candidates: Vec<Candidate>,
    /// Every considered row in candidate order — emittable AND `skipped`
    /// rows (reference `filter(candidates, with_skipped: true)` for the
    /// `--print --format json` payload; `dropped` rows stay out).
    all_rows: Vec<Candidate>,
    info: NamespaceInfo,
    unresolvable: Vec<UnresolvableSuperclass>,
    skips: Vec<&'static str>,
}

/// Whether a row produces a line (reference `Classification::EMITTABLE`).
fn emittable(c: &Candidate) -> bool {
    matches!(c.classification, "new_method" | "tighter_return")
}

/// The skip reasons the stderr summary counts (reference
/// `SUMMARISED_SKIP_REASONS`): every `sig.skipped.*` id except the four with a
/// report of their own (`unrenderable-rbs`, `unresolvable-superclass`,
/// `inline-declared`, `inline-differs`).
const SUMMARISED_SKIP_REASONS: &[&str] = &[
    "sig.skipped.complex-shape",
    "sig.skipped.user-authored",
    "sig.skipped.untyped-return",
    "sig.skipped.overridden-by-unsigned-subclass",
    "sig.skipped.inline-generic-class",
    "sig.skipped.generic-reopening",
    "sig.skipped.top-level-def",
    "sig.skipped.uncertain-module-function",
];

/// Reference `SigGenCommand#report_skipped` (#778): one stderr line counting
/// the methods the generator declined and why, in text mode only. The
/// breakdown keeps the order each reason first appears in.
fn report_skipped(skips: &[&'static str]) {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for reason in skips.iter().filter(|r| SUMMARISED_SKIP_REASONS.contains(r)) {
        match counts.iter_mut().find(|(r, _)| r == reason) {
            Some((_, n)) => *n += 1,
            None => counts.push((reason, 1)),
        }
    }
    if counts.is_empty() {
        return;
    }
    let total: usize = counts.iter().map(|(_, n)| n).sum();
    let breakdown: Vec<String> = counts.iter().map(|(r, n)| format!("{r}: {n}")).collect();
    eprintln!(
        "rigor sig-gen: skipped {total} method(s) it could not type or would not overwrite ({}). \
         Run with --format=json to see each one with its skip_reason.",
        breakdown.join(", ")
    );
}

/// One class dropped because its superclass chain resolves nowhere.
struct UnresolvableSuperclass {
    class_name: String,
    /// The token the report names (`Base`, or the external root).
    superclass: String,
    /// `true` for a project superclass this run wrote nothing for.
    project: bool,
}

/// Reference `Generator#resolve_superclass_spellings` +
/// `#demote_unresolvable_superclasses` (+ `unresolvable_superclass_report`):
///
/// 1. each recorded superclass token is resolved against its header's nesting
///    (innermost first, the class being opened excluded — #722/#2661) to a
///    class this run declares or the RBS environment knows (the renderers
///    write it rooted, `< ::Base`);
/// 2. a class whose superclass chain does not end in an RBS-known class or a
///    class this run writes (`written`) is DROPPED with every class nested in
///    it — its header would fail the next build (`RBS::NoSuperclassFoundError`,
///    #735/#2437/#2478) — repeating until nothing more drops;
/// 3. every surviving candidate's `--print` header is re-stamped.
fn finalize_superclasses(
    candidates: Vec<Candidate>,
    info: &mut NamespaceInfo,
    sig_env: &SigEnv,
    written: &dyn Fn(&Candidate) -> bool,
) -> (Vec<Candidate>, Vec<Candidate>, Vec<&'static str>, Vec<UnresolvableSuperclass>) {
    let index = CoreIndex::new();
    let rbs_known = |name: &str| sig_env.class_known(&index, name);
    // Every row counts, emitted or not (`candidates.each_with_object`): the
    // question is which class Ruby means by a token, not what reaches disk.
    let mut known: HashSet<String> = candidates
        .iter()
        .filter(|c| !c.class_name.is_empty())
        .map(|c| c.class_name.clone())
        .collect();
    known.extend(info.shells.iter().cloned());
    known.extend(info.kinds.keys().cloned());
    let headers = std::mem::take(&mut info.headers);
    for h in &headers {
        let declared = |c: &str| known.contains(c) || rbs_known(c);
        let resolved = superclasses::resolve_token(&h.token, &h.nesting, &h.full, &declared);
        info.supers.insert(h.full.clone(), sig_env.generics().superclass_spelling(&resolved));
    }
    let supers = info.supers.clone();

    // `demote_generic_reopenings` (#2436): a method of a class (or a class
    // nested in one) the loaded RBS declares generic with a bounded or
    // defaulted parameter is not emitted, unless the project's `sig/` already
    // declares the class (the writer then writes into that header).
    let generics = sig_env.generics();
    let candidates: Vec<Candidate> = candidates
        .into_iter()
        .map(|c| {
            if !emittable(&c)
                || sig_env.declares_class(&c.class_name)
                || !generics::namespace_prefixes(&c.class_name).iter().any(|n| generics.bounded(n))
            {
                return c;
            }
            demoted(c, "sig.skipped.generic-reopening")
        })
        .collect();

    let shell_files = info.shell_files.clone();
    let declared_by_run = |kept: &[&Candidate]| -> HashSet<String> {
        let written_cands: Vec<&&Candidate> = kept.iter().filter(|c| written(c)).collect();
        let paths: HashSet<&str> = written_cands.iter().map(|c| c.file.as_str()).collect();
        let mut declared: HashSet<String> =
            written_cands.iter().map(|c| c.class_name.clone()).collect();
        for (file, shell) in &shell_files {
            if paths.contains(file.as_str()) {
                declared.insert(shell.clone());
            }
        }
        let wrappers: Vec<String> =
            declared.iter().flat_map(|n| superclasses::wrapper_names(n)).collect();
        declared.extend(wrappers);
        declared
    };
    let owner_of = |class_name: &str, unresolvable: &HashMap<String, String>| -> Option<(String, String)> {
        if class_name.is_empty() {
            return None;
        }
        if let Some(root) = unresolvable.get(class_name) {
            return Some((class_name.to_string(), root.clone()));
        }
        let mut names: Vec<&String> = unresolvable.keys().collect();
        names.sort();
        names
            .into_iter()
            .find(|n| class_name.starts_with(&format!("{n}::")))
            .map(|n| (n.clone(), unresolvable[n].clone()))
    };
    let fixpoint = |rows: &[Candidate], mut unresolvable: HashMap<String, String>| {
        loop {
            let kept: Vec<&Candidate> =
                rows.iter().filter(|c| owner_of(&c.class_name, &unresolvable).is_none()).collect();
            let emitted = declared_by_run(&kept);
            let found: HashMap<String, String> = supers
                .keys()
                .filter_map(|name| {
                    superclasses::unresolved_root(name, &supers, &emitted, &rbs_known)
                        .map(|root| (name.clone(), root))
                })
                .collect();
            if found.len() == unresolvable.len() {
                return unresolvable;
            }
            unresolvable = found;
        }
    };
    let unresolvable = fixpoint(&candidates, HashMap::new());
    // `demote_unresolvable_superclasses` turns every row of such a class into a
    // skip, which the override demotion below then reads as unsigned.
    let candidates: Vec<Candidate> = candidates
        .into_iter()
        .map(|c| match owner_of(&c.class_name, &unresolvable) {
            Some(_) => demoted(c, "sig.skipped.unresolvable-superclass"),
            None => c,
        })
        .collect();
    let candidates = demote_overridden_base_methods(candidates, &supers);
    // Again after the override demotion: a demoted method can leave its file
    // with nothing to write, and a class shell riding on that file then
    // declares nothing a subclass elsewhere could rest on.
    let unresolvable = fixpoint(&candidates, unresolvable);

    // `stamp_generic_reopenings` / `header_type_params`: every generic class
    // (plain parameters only) the run may open a header for — each
    // candidate's class (a skipped one's too, #2580), the classes it is
    // nested in, and the shells.
    let mut names: Vec<String> = candidates
        .iter()
        .filter(|c| !c.class_name.is_empty())
        .map(|c| c.class_name.clone())
        .chain(info.shells.iter().cloned())
        .flat_map(|n| generics::namespace_prefixes(&n))
        .collect();
    names.sort();
    names.dedup();
    for n in names {
        if let Some(spelling) = generics.header_spelling(&n) {
            info.params.insert(n, spelling);
        }
    }

    // `@unresolvable_superclasses[class_name] ||= root`, in candidate order,
    // over every row: a skipped row of an unresolvable class becomes an
    // `unresolvable-superclass` skip, which the summary does not count.
    let mut owners: Vec<(String, String)> = Vec::new();
    let mut out: Vec<Candidate> = Vec::new();
    let mut all_rows: Vec<Candidate> = Vec::new();
    let mut skips: Vec<&'static str> = Vec::new();
    for mut c in candidates {
        match owner_of(&c.class_name, &unresolvable) {
            Some(owner) => {
                if !owners.iter().any(|(n, _)| *n == owner.0) {
                    owners.push(owner);
                }
                // Demoted by an earlier fixpoint pass already; a class the
                // SECOND pass (after the override demotion) first marks
                // unresolvable is demoted here — the JSON payload lists the
                // row either way.
                // The reference demotes EVERY row of the class — an earlier
                // `untyped-return` reason is replaced too.
                if c.skip_reason != Some("sig.skipped.unresolvable-superclass") {
                    c = demoted(c, "sig.skipped.unresolvable-superclass");
                }
                all_rows.push(c);
            }
            None if emittable(&c) => {
                c.decl_header = declaration_header(info, &c.class_name);
                all_rows.push(clone_candidate(&c));
                out.push(c);
            }
            None => {
                skips.extend(c.skip_reason);
                if c.skip_reason.is_some() {
                    all_rows.push(c);
                }
            }
        }
    }

    // `unresolvable_superclass_report`: a project superclass when the chain
    // closes through source classes.
    let source_classes: HashSet<&str> =
        info.kinds.iter().filter(|(_, k)| **k == "class").map(|(n, _)| n.as_str()).collect();
    let report = owners
        .into_iter()
        .map(|(class_name, token)| {
            let mut name = superclasses::bare_superclass_name(&token);
            let mut root = token.clone();
            let mut seen: HashSet<String> = HashSet::new();
            let mut project = false;
            while source_classes.contains(name.as_str()) && seen.insert(name.clone()) {
                let Some(h) = headers.iter().find(|h| h.full == name) else {
                    project = true;
                    break;
                };
                let declared = |c: &str| source_classes.contains(c) || rbs_known(c);
                root = superclasses::resolve_token(&h.token, &h.nesting, &name, &declared);
                name = superclasses::bare_superclass_name(&root);
                if rbs_known(&name) {
                    project = true;
                    break;
                }
            }
            UnresolvableSuperclass { class_name, superclass: if project { token } else { root }, project }
        })
        .collect();
    (out, all_rows, skips, report)
}

/// Reference `Generator#demote_overridden_base_methods` (#744): a base
/// class's method is not emitted when a project subclass overrides it and the
/// override is not emitted itself. RBS would resolve the subclass's call
/// through the base's declaration, a type describing the base's body rather
/// than the override the subclass runs.
///
/// An override counts as unsigned when its row is not emittable, whatever the
/// reason, kind aside (the reference pairs class and method name only). A row
/// the port drops on its own counts too: the port does not write that
/// override, so the base's declaration would be the subclass's answer.
fn demote_overridden_base_methods(
    candidates: Vec<Candidate>,
    supers: &HashMap<String, String>,
) -> Vec<Candidate> {
    let superclasses: HashMap<&str, String> = supers
        .iter()
        .map(|(name, sup)| (name.as_str(), superclasses::bare_superclass_name(sup)))
        .collect();
    if superclasses.is_empty() {
        return candidates;
    }
    let mut overridden: HashSet<(String, String)> = HashSet::new();
    for c in candidates.iter().filter(|c| !emittable(c) && !c.class_name.is_empty()) {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut current = c.class_name.as_str();
        while let Some(parent) = superclasses.get(current) {
            if !seen.insert(parent.as_str()) {
                break;
            }
            overridden.insert((parent.clone(), c.method_name.clone()));
            current = parent.as_str();
        }
    }
    if overridden.is_empty() {
        return candidates;
    }
    candidates
        .into_iter()
        .map(|c| {
            if emittable(&c) && overridden.contains(&(c.class_name.clone(), c.method_name.clone())) {
                demoted(c, "sig.skipped.overridden-by-unsigned-subclass")
            } else {
                c
            }
        })
        .collect()
}

/// Reference `demoted_candidate`: an emittable row turned into a skip.
fn demoted(mut c: Candidate, reason: &'static str) -> Candidate {
    c.classification = "skipped";
    c.skip_reason = Some(reason);
    // The reference's `demoted_candidate` rebuilds WITHOUT `rbs`, so the
    // JSON row omits the key.
    c.rbs = String::new();
    c
}

/// Reference `SigGenCommand#report_unresolvable_superclasses`.
fn report_unresolvable_superclasses(report: &[UnresolvableSuperclass]) {
    let mut project: Vec<(&str, &str)> = report
        .iter()
        .filter(|r| r.project)
        .map(|r| (r.class_name.as_str(), r.superclass.as_str()))
        .collect();
    let mut external: Vec<(&str, &str)> = report
        .iter()
        .filter(|r| !r.project)
        .map(|r| (r.class_name.as_str(), r.superclass.as_str()))
        .collect();
    project.sort();
    external.sort();
    if !project.is_empty() {
        eprintln!(
            "rigor sig-gen: skipped {} class(es) whose superclass this run wrote no signature for. \
             The superclass is declared in the project's source, but its own methods could not be typed, or its \
             `sig/` file could not be written, so a signature for these subclasses would fail to build \
             (`RBS::NoSuperclassFoundError`). Annotate or type the superclass's methods, fix its `sig/` file, or \
             hand-write its declaration, and re-run.",
            project.len()
        );
        for (c, s) in &project {
            eprintln!("  {c} < {s}");
        }
    }
    if !external.is_empty() {
        eprintln!(
            "rigor sig-gen: skipped {} class(es) whose superclass no loaded RBS declares. \
             A signature for them would fail to build (`RBS::NoSuperclassFoundError`) and would leave the \
             class LESS typed than it is now. Provide RBS for the superclass — `rbs collection install`, the \
             gem's own `sig/`, or a `signature_paths:` entry — and re-run.",
            external.len()
        );
        for (c, s) in &external {
            eprintln!("  {c} < {s}");
        }
    }
}

/// `Writer.writes?`: only a `new_method` candidate is sure to be written.
fn writes(c: &Candidate) -> bool {
    c.classification == "new_method"
}

/// Produce candidates + the `--write` namespace metadata for one source file. A
/// parse/read failure, a tree past the depth bound (rigor-rs#412), or a file
/// with no reachable named class body yields no candidates.
fn generate_file_with_info(
    path: &str,
    include_private: bool,
    sig_env: &SigEnv,
) -> (Vec<Candidate>, NamespaceInfo) {
    let Ok(source) = std::fs::read_to_string(path) else {
        return (Vec::new(), NamespaceInfo::default());
    };
    let Ok(parsed) = parse_bounded(source.as_bytes()) else {
        return (Vec::new(), NamespaceInfo::default());
    };
    // Every `Data.define` / `Struct.new` class in the file, keyed by FQN. Read
    // BEFORE the walks: it decides which constant writes are namespaces (their
    // `do ... end` block's defs bind on the new class) and which classes carry a
    // synthesised member surface.
    let layouts = meta_class::collect(&parsed);
    let ast = lower(&parsed);
    // Core index for typing / erasure AND the declared-return ancestor tail the
    // [`SigEnv`] delegates to (`declared_instance_return` / `_singleton_return`).
    let index = CoreIndex::new();
    let source_index = SourceIndex::build(&ast, &index);
    let typer = Typer::with_source(&index, &source_index);
    let mut interner = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut interner);

    // The file's declared source-class FQN set: every `class`/`module` and every
    // `Const = Data.define(...)` / `Struct.new(...)`, keyed by fully-qualified
    // name. rigor-rs's per-file SourceIndex types a project `X.new` under the
    // WRITTEN short name, but the reference emits the FULLY-QUALIFIED name
    // (`Rigor::Triage::Selector`) — so a source-class member is qualified at emit
    // time via Ruby constant resolution (longest-enclosing-prefix) against this
    // set. Because the SourceIndex is per-file, every source class that resolves
    // to a Nominal is defined HERE, so its FQN is always in this set.
    let mut fqns: std::collections::HashSet<String> = std::collections::HashSet::new();
    let root = ast.root();
    if let Node::Program { body, .. } = ast.get(root) {
        for &child in body {
            collect_source_fqns(&ast, child, &[], &mut fqns);
        }
    }

    // Meta members LEAD the candidate order (reference `collect_candidates`): a
    // value class's members and constructors read first, ahead of the methods its
    // block body or its `class ... end` body defines.
    let mut out = collect_meta_candidates(path, &layouts, sig_env);
    let mut definers: Vec<Definer> = Vec::new();
    // `meta_member_slots` (reference `definer_table`): every member a
    // `Data.define`/`Struct.new` layout declares is a CERTAIN definer of its
    // slot at `MEMBER_OFFSET` — a `def x` inside the block (or the class body)
    // steals it, so `def x: () -> untyped` must not emit beside the real
    // `def x` (rigor-rs#525 item 1).
    for (class_name, layout) in layouts.iter() {
        for member in meta_class::member_decls(layout) {
            definers.push(Definer {
                class_name: class_name.to_string(),
                kind: member.kind,
                method_name: member.method_name.clone(),
                offset: META_MEMBER_OFFSET,
                certain: true,
            });
        }
    }
    let mut info = NamespaceInfo::default();
    // Top-level `def`s join the walk in source order, as `sig.skipped.top-level-def`
    // rows (#1676). None sits inside a namespace the walk enters, so each one
    // goes in ahead of the first top-level statement that ends after it.
    let mut top_level = top_level::collect(&parsed).into_iter().peekable();
    let modes = module_function::def_modes(&parsed);
    if let Node::Program { body, .. } = ast.get(root) {
        for &child in body {
            let end = ast.get(child).span().1;
            while let Some((name, _)) = top_level.next_if(|(_, start)| *start < end) {
                out.push(non_emitted(path, "", &name, "instance", Some("sig.skipped.top-level-def")));
            }
            walk_namespace(
                &ast,
                child,
                &[],
                path,
                include_private,
                &index,
                &typer,
                &env,
                &fqns,
                sig_env,
                &layouts,
                &modes,
                &mut interner,
                &mut out,
                &mut definers,
            );
            collect_namespace_info(&ast, child, &[], &mut info);
        }
    }
    for (name, _) in top_level {
        out.push(non_emitted(path, "", &name, "instance", Some("sig.skipped.top-level-def")));
    }
    // `live_definitions` (reference): Ruby keeps the LAST certain definer of a
    // `(class, kind, name)` slot, and a def is written only when it survives
    // EVERY slot it defines. A module_function `def u` owns `[instance,u]` and
    // `[singleton,u]`; a later `def self.u` steals the singleton slot, so the
    // `self?.u` row is dropped entirely — writing both is the `rbs validate`
    // duplicate-member failure of rigor-rs#493 item 9. One surviving singleton
    // slot downgrades the row to `def self.u`; anything else drops it.
    live_definitions(&mut out, &definers);
    register_meta_classes(&layouts, &mut info);
    info.headers = superclasses::collect(&parsed);
    // `register_core_subclass_shell` (#2224): a subclass of a core or stdlib
    // class is declared as a class shell (`class MyHash < ::Hash[untyped,
    // untyped]`) so a method returning one never names an undeclared class —
    // unless the RBS environment already declares it, or the header's nesting
    // declares a constant of the superclass's first segment (`Foo::Hash`),
    // which then is the project's class, not core's.
    for h in &info.headers {
        if sig_env.class_known(&index, &h.full) || info.shells.contains(&h.full) {
            continue;
        }
        let bare = superclasses::bare_superclass_name(&h.token);
        if !index.knows_toplevel_class(&bare) {
            continue;
        }
        if !h.token.starts_with("::") {
            let first = bare.split("::").next().unwrap_or(&bare);
            let shadowed = (1..=h.nesting.len())
                .any(|i| fqns.contains(&format!("{}::{first}", h.nesting[..i].join("::"))));
            if shadowed {
                continue;
            }
        }
        info.shells.push(h.full.clone());
        info.core_shells.insert(h.full.clone());
        info.kinds.entry(h.full.clone()).or_insert("class");
    }
    info.shell_files = info.shells.iter().map(|s| (path.to_string(), s.clone())).collect();
    // The print header depends on the WHOLE file's namespace metadata, so it is
    // stamped once the walks are done rather than at construction (and again,
    // whole-run, by [`finalize_superclasses`]).
    for c in &mut out {
        c.decl_header = declaration_header(&info, &c.class_name);
    }
    (out, info)
}

/// Declare every layout-carrying class in `info`: the `class` keyword (so the
/// leaf wins over the intermediate-segment `module` default), its `::Data` /
/// `::Struct[untyped]` ancestry, and a shell entry so `--write` declares the
/// class even when every member candidate is suppressed as already-declared.
///
/// The named-subclass form (`class Point < Data.define(:x, :y)`) is registered
/// too: its `class` keyword is not in question, but its COMPUTED superclass is
/// exactly what [`collect_namespace_info`] refuses to guess at.
fn register_meta_classes(layouts: &MetaLayouts, info: &mut NamespaceInfo) {
    for (class_name, layout) in layouts.iter() {
        info.kinds.insert(class_name.to_string(), "class");
        info.supers.insert(class_name.to_string(), layout.kind.superclass().to_string());
        if !info.shells.contains(&class_name.to_string()) {
            info.shells.push(class_name.to_string());
        }
    }
}

/// One candidate per member accessor and constructor each layout-carrying class
/// synthesises. These are the members no `def` / `attr_*` in the source declares,
/// so nothing else in sig-gen can find them — and once the class is DECLARED, an
/// undeclared member reads as a missing one.
fn collect_meta_candidates(path: &str, layouts: &MetaLayouts, sig_env: &SigEnv) -> Vec<Candidate> {
    let mut out = Vec::new();
    for (class_name, layout) in layouts.iter() {
        for member in meta_class::member_decls(layout) {
            // Suppression is gated on the declaration sitting on THIS class, not
            // on the lookup merely succeeding: `::Data.new: () -> bot` and
            // `::Struct`'s factory answer the `.new` lookup for every value class,
            // and deferring to them is exactly what would leave the inherited-arity
            // false positive in place (reference `declared_on_class_itself?`).
            if sig_env.declares_directly(class_name, &member.method_name, member.kind) {
                continue;
            }
            out.push(meta_candidate(path, class_name, &member));
        }
    }
    out
}

/// Build the [`Candidate`] for one synthesised member. Always `new_method`: a
/// synthesised member has no source `def` whose declared return could be tightened.
fn meta_candidate(path: &str, class_name: &str, member: &MetaMember) -> Candidate {
    Candidate {
        file: path.to_string(),
        class_name: class_name.to_string(),
        method_name: member.method_name.clone(),
        kind: member.kind,
        rbs: member.rbs.clone(),
        inferred_return: "untyped".to_string(),
        classification: "new_method",
        declared_return_rbs: None,
        decl_header: String::new(),
        skip_reason: None,
        definer: Some((META_MEMBER_OFFSET, false)),
    }
}

/// The reference's `MEMBER_OFFSET = -1`: a meta member's definer-table entry
/// sorts BEFORE every real offset — a `def` anywhere after it steals the slot.
/// `0` stands in for `-1` (the table holds only class-scoped slots; no
/// class-level `def` can sit at byte 0).
const META_MEMBER_OFFSET: usize = 0;

/// The `--print` declaration line for a class group (reference
/// `declaration_header`). Print mode used to hard-code `class`, which turned a
/// MODULE into a class the moment it held an emittable method — output that
/// raises `RBS::DuplicatedDeclarationError` on load if the real `module` is
/// declared anywhere else (rigor#227). Defaulting to `class` when the map has no
/// entry keeps the pre-existing spelling for a leaf class.
fn declaration_header(info: &NamespaceInfo, class_name: &str) -> String {
    let params = info.params.get(class_name).map_or("", String::as_str);
    if info.kinds.get(class_name) == Some(&"module") {
        return format!("module {class_name}{params}");
    }
    match info.supers.get(class_name) {
        // `SuperclassSpelling.absolute` (#2661): written rooted.
        Some(superclass) => {
            format!("class {class_name}{params} < {}", superclasses::absolute(superclass))
        }
        None => format!("class {class_name}{params}"),
    }
}

/// Collect every declared source-class FULLY-QUALIFIED name in the file: each
/// `class`/`module` (its written `name` may itself be a `A::B` path — joined
/// onto the lexical `prefix`), and each `Const = Data.define(...)` /
/// `Struct.new(...)` constant. `prefix` is the enclosing lexical namespace.
/// Feeds [`qualify_source_name`] so a source-class return renders the reference's
/// fully-qualified spelling (`Rigor::Triage::Selector`).
fn collect_source_fqns(
    ast: &LoweredAst,
    id: NodeId,
    prefix: &[String],
    out: &mut std::collections::HashSet<String>,
) {
    match ast.get(id) {
        Node::ClassDef { name, rooted, body, .. } | Node::ModuleDef { name, rooted, body, .. } => {
            let fqn = declaration_prefix(prefix, name, *rooted).join("::");
            out.insert(fqn.clone());
            // The child prefix is the full path split (`A::B` nested under `M`
            // becomes prefix `["M", "A", "B"]` for its own body).
            let child_prefix: Vec<String> = fqn.split("::").map(str::to_string).collect();
            for &child in body {
                collect_source_fqns(ast, child, &child_prefix, out);
            }
        }
        // A `Const = Data.define(...)` / `Struct.new(...)` defines a class-valued
        // constant whose `.new` types to a `DataInstance`/Nominal the reference
        // names fully-qualified. Record its FQN so returns of it qualify.
        Node::ConstantWrite { name, value, .. }
            if !name.is_empty() && is_class_defining_call(ast, *value) =>
        {
            out.insert(qualify_join(prefix, name));
        }
        _ => {}
    }
}

/// Whether a constant-write's value is a `Data.define(...)` or `Struct.new(...)`
/// call — the class-defining constant forms whose instances the reference names
/// with the constant's fully-qualified name.
fn is_class_defining_call(ast: &LoweredAst, value: NodeId) -> bool {
    let Node::Call { receiver: Some(recv), method, .. } = ast.get(value) else {
        return false;
    };
    let Node::ConstantRead { name: recv_name, .. } = ast.get(*recv) else {
        return false;
    };
    matches!(
        (recv_name.as_str(), method.as_str()),
        ("Data", "define") | ("Struct", "new")
    )
}

/// `Source::ConstantPath.declaration_prefix`: the namespace a `class`/`module`
/// header declares — the enclosing `prefix` plus the written `name`, or, for a
/// `::`-rooted header (`class ::Box` inside `module A`), the name alone
/// (reference #722 residue 3: a rooted header re-anchors at the top level).
fn declaration_prefix(prefix: &[String], name: &str, rooted: bool) -> Vec<String> {
    let mut out: Vec<String> = if rooted { Vec::new() } else { prefix.to_vec() };
    out.extend(name.split("::").filter(|s| !s.is_empty()).map(str::to_string));
    out
}

/// Join a lexical `prefix` and a (possibly already-namespaced) `name` with `::`.
fn qualify_join(prefix: &[String], name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{}::{}", prefix.join("::"), name)
    }
}

/// Resolve a source-class SHORT name to its fully-qualified name via Ruby
/// constant lookup from the `enclosing` scope: try `<enclosing>::<short>`, then
/// walk one namespace level outward at a time, first hit in `fqns` wins; falls
/// back to `short` unchanged when nothing matches (an external / already-bare
/// name — byte-identical to the old behavior). Mirrors the reference's
/// longest-enclosing-prefix constant resolution (`resolve_override_ancestor_name`).
fn qualify_source_name(
    short: &str,
    enclosing: &str,
    fqns: &std::collections::HashSet<String>,
) -> String {
    // An already-qualified short (contains `::`) is used as written.
    let mut scope: Vec<&str> = if enclosing.is_empty() {
        Vec::new()
    } else {
        enclosing.split("::").collect()
    };
    loop {
        let candidate = if scope.is_empty() {
            short.to_string()
        } else {
            format!("{}::{}", scope.join("::"), short)
        };
        if fqns.contains(&candidate) {
            return candidate;
        }
        if scope.pop().is_none() {
            return short.to_string();
        }
    }
}

/// Erase `ty` to RBS like [`crate::type_display::erase`], but QUALIFY every
/// source-class name to its fully-qualified spelling from the `enclosing` scope
/// (reference behavior). The resolver tries the CORE index first — a core class
/// (`String`, `Integer`) is never qualified — then the source registry, whose
/// short name is run through [`qualify_source_name`]. Composite carriers
/// (unions, tuples) qualify member-by-member because the resolver is invoked per
/// class id during erasure. Sig-gen-local: the `check` path's shared
/// `type_display::erase` is untouched.
fn erase_qualified(
    interner: &Interner,
    index: &CoreIndex,
    source: &SourceIndex,
    ty: TypeId,
    enclosing: &str,
    fqns: &std::collections::HashSet<String>,
) -> String {
    let resolve = |class: ClassId| -> Option<String> {
        if let Some(core) = index.class_name_for_id(class) {
            return Some(core.to_string());
        }
        source
            .class_name_for_id(class)
            .map(|short| qualify_source_name(short, enclosing, fqns))
    };
    rigor_types::erase_to_rbs_named(interner, ty, &resolve)
}

/// The `describe(:short)` sort key with source-class names QUALIFIED — the twin
/// of [`erase_qualified`] for the member-sort key, so a union containing a
/// source-class member orders identically to the reference (whose `describe`
/// resolves a source nominal to its FQN).
fn describe_qualified(
    interner: &Interner,
    index: &CoreIndex,
    source: &SourceIndex,
    ty: TypeId,
    enclosing: &str,
    fqns: &std::collections::HashSet<String>,
) -> String {
    let resolve = |class: ClassId| -> Option<String> {
        if let Some(core) = index.class_name_for_id(class) {
            return Some(core.to_string());
        }
        source
            .class_name_for_id(class)
            .map(|short| qualify_source_name(short, enclosing, fqns))
    };
    rigor_types::describe_named(interner, ty, &resolve)
}

/// One `def`'s contribution to the definer table (reference `definer_table` /
/// `definer_slots`): a module_function instance def defines its instance slot
/// AND a singleton slot; a def after an uncertain `module_function` reset defines
/// the singleton slot only `possible`-ly (`certain = false`). Every collected
/// `def` is a direct statement of a class/module/`class << self`/`Data.define`
/// body or `class << self` inner def, so the own-kind slot is `certain` — the
/// reference's `certain_definers` set.
struct Definer {
    class_name: String,
    /// `"instance"` | `"singleton"` slot kind.
    kind: &'static str,
    method_name: String,
    offset: usize,
    certain: bool,
}

/// Recurse a `class` / `module` node, emitting a candidate per qualifying direct
/// instance method and descending into nested namespaces (prefix accumulates the
/// qualified name, reference `walk_defs`).
#[allow(clippy::too_many_arguments)]
fn walk_namespace(
    ast: &LoweredAst,
    id: NodeId,
    prefix: &[String],
    path: &str,
    include_private: bool,
    index: &CoreIndex,
    typer: &Typer,
    env: &TypeEnv,
    fqns: &std::collections::HashSet<String>,
    sig_env: &SigEnv,
    layouts: &MetaLayouts,
    modes: &HashMap<usize, module_function::Mode>,
    interner: &mut Interner,
    out: &mut Vec<Candidate>,
    definers: &mut Vec<Definer>,
) {
    let (name, rooted, visibilities, body) = match ast.get(id) {
        Node::ClassDef { name, rooted, method_visibilities, body, .. } => {
            (name, *rooted, method_visibilities.clone(), body.as_slice())
        }
        Node::ModuleDef { name, rooted, method_visibilities, body, .. } => {
            (name, *rooted, method_visibilities.clone(), body.as_slice())
        }
        // A `Const = Data.define(...) do ... end` block is the new class's own
        // body: the runtime `class_eval`s it into the anonymous class it just
        // stamped, so its defs bind on `Const`, NOT on the enclosing namespace.
        // Attributing them outward is what made upstream report a member against
        // the enclosing MODULE and then redeclare that module as a class (#227).
        // Only a layout-carrying constant qualifies — a `Class.new do ... end`
        // stays unrecognised, as it is upstream.
        Node::ConstantWrite { name, value, .. }
            if layouts.get(&qualify_join(prefix, name)).is_some() =>
        {
            let Node::Call { block_body, .. } = ast.get(*value) else { return };
            (name, false, block_visibilities(ast, block_body), block_body.as_slice())
        }
        _ => return,
    };

    let qualified = declaration_prefix(prefix, name, rooted);
    let class_name = qualified.join("::");

    // Collect instance + singleton methods in ONE pass over the class body so
    // they emit in SOURCE ORDER (the reference walks the AST top-to-bottom): a
    // direct instance `def x`, a `def self.x`, and the receiver-less inner defs
    // of a `class << self`. `method_bodies` harvests exactly the direct
    // `Definition{name:Some}` set, so walking the body for them is equivalent AND
    // recovers each def's span for the ordering (the sort key).
    //
    // A bare `module_function` (no args) seen EARLIER in this body makes every
    // SUBSEQUENT instance def dual, rendered `def self?.name` (reference
    // `@module_function_methods`), until a bare `public` / `private` /
    // `protected` ends it. After a reset that may not run, the def's kind is
    // unknown and it is skipped (`sig.skipped.uncertain-module-function`,
    // #1968). [`module_function::def_modes`] reads the body; the `:sym` ARGS
    // form does not flip the mode. It applies in a CLASS body too.
    let mut sigs: Vec<(MethodSig, Option<Visibility>, usize)> = Vec::new();
    let mut uncertain: Vec<(&str, usize)> = Vec::new();
    for &child in body {
        match ast.get(child) {
            Node::Definition {
                name: Some(n), body: b, params, param_shape, has_explicit_return, span, ..
            } => {
                let mode = modes.get(&span.0).copied().unwrap_or(module_function::Mode::Off);
                if mode == module_function::Mode::Uncertain {
                    uncertain.push((n.as_str(), span.0));
                    definers.push(Definer {
                        class_name: class_name.clone(),
                        kind: "instance",
                        method_name: n.to_string(),
                        offset: span.0,
                        certain: true,
                    });
                    definers.push(Definer {
                        class_name: class_name.clone(),
                        kind: "singleton",
                        method_name: n.to_string(),
                        offset: span.0,
                        certain: false,
                    });
                    continue;
                }
                let mf = mode == module_function::Mode::On;
                let vis = if mode == module_function::Mode::OffPrivate {
                    Some(Visibility::Private)
                } else {
                    visibilities.iter().find(|(m, _)| m == n).map(|(_, v)| *v)
                };
                sigs.push((
                    sig_of(n, b, params, param_shape, *has_explicit_return, false, mf, span.0),
                    vis,
                    span.0,
                ));
                definers.push(Definer {
                    class_name: class_name.clone(),
                    kind: "instance",
                    method_name: n.to_string(),
                    offset: span.0,
                    certain: true,
                });
                if mf {
                    definers.push(Definer {
                        class_name: class_name.clone(),
                        kind: "singleton",
                        method_name: n.to_string(),
                        offset: span.0,
                        certain: true,
                    });
                }
            }
            // `private def x` after an uncertain reset: reported like a direct
            // def (the reference skips it before reading its visibility). In
            // any other mode the port does not walk a def passed to a call.
            Node::Call { receiver: None, method, args, span, block_body, .. } => {
                // `attr_reader :x` / `attr_writer` / `attr_accessor` are
                // DEFINERS too (reference `record_attr_definers`): the call's
                // offset ranks them like a `def`, so a later `attr_reader :u`
                // steals the instance slot a `module_function def u` held.
                if !class_name.is_empty()
                    && matches!(method.as_str(), "attr_reader" | "attr_writer" | "attr_accessor")
                {
                    for &arg in args {
                        let Node::SymbolLit { value, .. } = ast.get(arg) else { continue };
                        let reads = method != "attr_writer";
                        let writes = method != "attr_reader";
                        for (mname, emits) in [(value.clone(), reads), (format!("{value}="), writes)] {
                            if emits {
                                definers.push(Definer {
                                    class_name: class_name.clone(),
                                    kind: "instance",
                                    method_name: mname,
                                    offset: span.0,
                                    certain: true,
                                });
                            }
                        }
                    }
                }
                for &arg in args {
                    if let Node::Definition { name: Some(n), span, .. } = ast.get(arg) {
                        let mode = modes.get(&span.0).copied().unwrap_or(module_function::Mode::Off);
                        if mode == module_function::Mode::Uncertain {
                            uncertain.push((n.as_str(), span.0));
                        }
                        // A def passed to `private` / `public` / `protected`
                        // (or `module_function`) still DEFINES its slots — the
                        // reference records it in `certain_definers` even
                        // though the port never emits a candidate for it.
                        definers.push(Definer {
                            class_name: class_name.clone(),
                            kind: "instance",
                            method_name: n.to_string(),
                            offset: span.0,
                            certain: true,
                        });
                        match mode {
                            module_function::Mode::On => definers.push(Definer {
                                class_name: class_name.clone(),
                                kind: "singleton",
                                method_name: n.to_string(),
                                offset: span.0,
                                certain: true,
                            }),
                            module_function::Mode::Uncertain => definers.push(Definer {
                                class_name: class_name.clone(),
                                kind: "singleton",
                                method_name: n.to_string(),
                                offset: span.0,
                                certain: false,
                            }),
                            _ => {}
                        }
                    }
                }
                // `foo(attr_reader(:x))` — a call-arg attr is an uncertain
                // definer too (`rigor_each_child` descends arguments).
                // `Definition` args are already handled above — descending
                // them would double-count the wrapped def.
                for &arg in args {
                    if !matches!(ast.get(arg), Node::Definition { .. }) {
                        collect_conditional_definers(
                            ast,
                            arg,
                            &qualified,
                            false,
                            matches!(ast.get(id), Node::ModuleDef { .. }),
                            layouts,
                            modes,
                            definers,
                        );
                    }
                }
                // `foo do; attr_reader :x; end` — the block's `def`/`attr`
                // children are uncertain definers (`walk_attr_calls` descends
                // call blocks too; a `refine`/`class_methods` block is a DSL
                // scope and skipped).
                let dsl_scope = method == "class_methods"
                    || (method == "refine" && matches!(ast.get(id), Node::ModuleDef { .. }));
                if !dsl_scope {
                    for &b in block_body {
                        collect_conditional_definers(
                            ast,
                            b,
                            &qualified,
                            false,
                            matches!(ast.get(id), Node::ModuleDef { .. }),
                            layouts,
                            modes,
                            definers,
                        );
                    }
                }
            }
            Node::Definition {
                singleton_name: Some(n), body: b, params, param_shape, has_explicit_return, span, ..
            } => {
                sigs.push((
                    sig_of(n, b, params, param_shape, *has_explicit_return, true, false, span.0),
                    None,
                    span.0,
                ));
                definers.push(Definer {
                    class_name: class_name.clone(),
                    kind: "singleton",
                    method_name: n.to_string(),
                    offset: span.0,
                    certain: true,
                });
            }
            Node::Definition { is_singleton_class: true, body: sbody, .. } => {
                for &inner in sbody {
                    if let Node::Definition {
                        name: Some(n), body: b, params, param_shape, has_explicit_return, span, ..
                    } = ast.get(inner)
                    {
                        sigs.push((
                            sig_of(n, b, params, param_shape, *has_explicit_return, true, false, span.0),
                            None,
                            span.0,
                        ));
                        definers.push(Definer {
                            class_name: class_name.clone(),
                            kind: "singleton",
                            method_name: n.to_string(),
                            offset: span.0,
                            certain: true,
                        });
                    }
                }
            }
            // Any other body statement — an `if`/`case`/`begin`/loop/block
            // — can still CARRY a `def` or `attr_*`: the reference walks
            // into every non-`def` child (`walk_attr_calls` +
            // `walk_def_children`), recording each as an UNCERTAIN definer
            // (only direct statements are `certain_definers`).
            // `if cond; attr_reader :x; end` then contests a preceding
            // `def x` (rigor-rs#525 item 2). Namespace children are walked
            // by `Emit::Nested` instead — do not double-count them.
            _ => {
                if !is_namespace_node(ast, child, &qualified, layouts) {
                    collect_conditional_definers(
                        ast,
                        child,
                        &qualified,
                        false,
                        matches!(ast.get(id), Node::ModuleDef { .. }),
                        layouts,
                        modes,
                        definers,
                    );
                }
            }
        }
    }
    // Emit own methods AND descend into nested classes in ONE source-order
    // (span) pass, so a nested class declared BEFORE the outer class's own
    // methods is FIRST-SEEN first — matching the reference's single top-to-bottom
    // walk + `group_by(&:class_name)` (a nested class's group then sorts ahead of
    // its parent's). Two separate loops (methods-then-nested) mis-ordered the
    // class groups for that shape.
    enum Emit<'a> {
        Method(&'a MethodSig<'a>, Option<Visibility>),
        Uncertain(&'a str),
        Nested(NodeId),
    }
    let mut items: Vec<(usize, Emit)> = Vec::new();
    for (sig, vis, span) in &sigs {
        items.push((*span, Emit::Method(sig, *vis)));
    }
    for (name, span) in &uncertain {
        items.push((*span, Emit::Uncertain(name)));
    }
    for &child in body {
        if is_namespace_node(ast, child, &qualified, layouts) {
            items.push((ast.get(child).span().0, Emit::Nested(child)));
        }
    }
    items.sort_by_key(|(span, _)| *span);
    for (_, item) in items {
        match item {
            Emit::Method(sig, vis) => {
                if let Some(candidate) = method_candidate(
                    ast,
                    sig,
                    vis,
                    &class_name,
                    path,
                    include_private,
                    index,
                    typer,
                    env,
                    fqns,
                    sig_env,
                    interner,
                ) {
                    out.push(candidate);
                }
            }
            Emit::Uncertain(name) => out.push(non_emitted(
                path,
                &class_name,
                name,
                "instance",
                Some("sig.skipped.uncertain-module-function"),
            )),
            Emit::Nested(child) => walk_namespace(
                ast,
                child,
                &qualified,
                path,
                include_private,
                index,
                typer,
                env,
                fqns,
                sig_env,
                layouts,
                modes,
                interner,
                out,
                definers,
            ),
        }
    }
}

/// Reference `walk_attr_calls` + the definer side of `walk_defs`: descend
/// into every non-`def` node inside a class body — `if`/`case`/`begin`/loop
/// bodies, call arguments and block bodies — recording each `def` and
/// `attr_*` it holds as an UNCERTAIN definer (`certain_definers` marks only
/// direct statements). An uncertain definer after a certain one contests the
/// slot, so `def x; if c; attr_reader :x; end; end` writes nothing
/// (rigor-rs#525 item 2). `refine`/`class_methods` block calls are DSL
/// scopes the reference skips (`skipped_block_call?`).
#[allow(clippy::too_many_arguments)]
fn collect_conditional_definers(
    ast: &LoweredAst,
    id: NodeId,
    prefix: &[String],
    in_singleton_class: bool,
    in_module: bool,
    layouts: &MetaLayouts,
    modes: &HashMap<usize, module_function::Mode>,
    definers: &mut Vec<Definer>,
) {
    let recurse = |ast: &LoweredAst,
                   child: NodeId,
                   prefix: &[String],
                   sclass: bool,
                   module: bool,
                   definers: &mut Vec<Definer>| {
        collect_conditional_definers(ast, child, prefix, sclass, module, layouts, modes, definers);
    };
    match ast.get(id) {
        Node::ClassDef { name, rooted, body, .. } | Node::ModuleDef { name, rooted, body, .. } => {
            let child_prefix = declaration_prefix(prefix, name, *rooted);
            let module = matches!(ast.get(id), Node::ModuleDef { .. });
            for &b in body {
                recurse(ast, b, &child_prefix, false, module, definers);
            }
        }
        // `Const = Data.define(...) do … end` inside a conditional: the block
        // is the new class's own body (`meta_block_body`).
        Node::ConstantWrite { name, value, .. }
            if layouts.get(&qualify_join(prefix, name)).is_some() =>
        {
            if let Node::Call { block_body, .. } = ast.get(*value) {
                let mut child_prefix = prefix.to_vec();
                child_prefix.push(name.clone());
                for &b in block_body {
                    recurse(ast, b, &child_prefix, false, false, definers);
                }
            }
        }
        Node::Definition { is_singleton_class: true, body, .. } => {
            for &b in body {
                recurse(ast, b, prefix, true, in_module, definers);
            }
        }
        // A `def` is a definer itself; its BODY is out of bounds
        // (`walk_attr_calls` returns at DefNode).
        Node::Definition { name, singleton_name, span, .. } => {
            if let Some(n) = name.as_ref().or(singleton_name.as_ref()) {
                let kind = if in_singleton_class || singleton_name.is_some() {
                    "singleton"
                } else {
                    "instance"
                };
                definers.push(Definer {
                    class_name: prefix.join("::"),
                    kind,
                    method_name: n.clone(),
                    offset: span.0,
                    certain: false,
                });
                if !in_singleton_class
                    && matches!(
                        modes.get(&span.0),
                        Some(module_function::Mode::On) | Some(module_function::Mode::Uncertain)
                    )
                {
                    definers.push(Definer {
                        class_name: prefix.join("::"),
                        kind: "singleton",
                        method_name: n.clone(),
                        offset: span.0,
                        certain: false,
                    });
                }
            }
        }
        Node::Call { receiver, method, args, block_body, span, .. } => {
            // `class_methods` is a DSL scope only RECEIVERLESS
            // (`class_methods_block?`); `refine` likewise on a nil or `self`
            // receiver (`refine_call?`) and only in a MODULE body — `Class`
            // undefines `refine`, so the same spelling in a `class` body is
            // the class's own method and its block walks as any other block
            // (`class_body_refine?` / #1689). A `obj.class_methods` /
            // `obj.refine` block is an ordinary block and descends.
            let refine_shaped = method == "refine"
                && receiver
                    .map(|r| matches!(ast.get(r), Node::SelfExpr { .. }))
                    .unwrap_or(true);
            let dsl = (method == "class_methods" && receiver.is_none())
                || (refine_shaped && in_module);
            if dsl && !block_body.is_empty() {
                return;
            }
            if !prefix.is_empty()
                && !in_singleton_class
                && matches!(method.as_str(), "attr_reader" | "attr_writer" | "attr_accessor")
            {
                for &arg in args {
                    let Node::SymbolLit { value, .. } = ast.get(arg) else { continue };
                    let reads = method != "attr_writer";
                    let writes = method != "attr_reader";
                    for (mname, emits) in [(value.clone(), reads), (format!("{value}="), writes)] {
                        if emits {
                            definers.push(Definer {
                                class_name: prefix.join("::"),
                                kind: "instance",
                                method_name: mname,
                                offset: span.0,
                                certain: false,
                            });
                        }
                    }
                }
            }
            for &b in args.iter().chain(block_body.iter()) {
                recurse(ast, b, prefix, in_singleton_class, in_module, definers);
            }
        }
        Node::If { predicate, then_body, else_body, .. } => {
            recurse(ast, *predicate, prefix, in_singleton_class, in_module, definers);
            for &b in then_body.iter().chain(else_body.iter()) {
                recurse(ast, b, prefix, in_singleton_class, in_module, definers);
            }
        }
        Node::Case { predicate, branches, else_body, .. } => {
            if let Some(p) = predicate {
                recurse(ast, *p, prefix, in_singleton_class, in_module, definers);
            }
            for &b in branches.iter().chain(else_body.iter()) {
                recurse(ast, b, prefix, in_singleton_class, in_module, definers);
            }
        }
        Node::When { conditions, body, .. } => {
            for &b in conditions.iter().chain(body.iter()) {
                recurse(ast, b, prefix, in_singleton_class, in_module, definers);
            }
        }
        Node::Loop { predicate, body, .. } => {
            if let Some(p) = predicate {
                recurse(ast, *p, prefix, in_singleton_class, in_module, definers);
            }
            for &b in body {
                recurse(ast, b, prefix, in_singleton_class, in_module, definers);
            }
        }
        Node::BeginRescue { body, .. } | Node::Statements { body, .. } => {
            for &b in body {
                recurse(ast, b, prefix, in_singleton_class, in_module, definers);
            }
        }
        // Expression-level containers carry the same children
        // (`rigor_each_child` descends everywhere): `_x = if c; attr_reader
        // :x; end` and `c && attr_reader(:x)` record just the same.
        Node::LocalVariableWrite { value, .. }
        | Node::LocalVariableOpWrite { value, .. }
        | Node::VariableWrite { value, .. }
        | Node::InstanceVariableWrite { value, .. }
        | Node::ConstantWrite { value, .. } => {
            recurse(ast, *value, prefix, in_singleton_class, in_module, definers);
        }
        Node::MultiWrite { value, target_exprs, .. } => {
            recurse(ast, *value, prefix, in_singleton_class, in_module, definers);
            for &b in target_exprs {
                recurse(ast, b, prefix, in_singleton_class, in_module, definers);
            }
        }
        Node::IndexWrite { receiver, indices, value, .. } => {
            if let Some(r) = receiver {
                recurse(ast, *r, prefix, in_singleton_class, in_module, definers);
            }
            for &b in indices.iter().chain(std::iter::once(value)) {
                recurse(ast, b, prefix, in_singleton_class, in_module, definers);
            }
        }
        Node::Logical { left, right, .. } => {
            recurse(ast, *left, prefix, in_singleton_class, in_module, definers);
            recurse(ast, *right, prefix, in_singleton_class, in_module, definers);
        }
        Node::Lambda { body, .. } | Node::Return { values: body, .. } => {
            for &b in body {
                recurse(ast, b, prefix, in_singleton_class, in_module, definers);
            }
        }
        Node::InterpolatedString { parts, .. } | Node::InterpolatedSymbol { parts, .. } => {
            for &b in parts {
                recurse(ast, b, prefix, in_singleton_class, in_module, definers);
            }
        }
        Node::ArrayLit { elements, .. } | Node::HashLit { elements, .. } => {
            for &b in elements {
                recurse(ast, b, prefix, in_singleton_class, in_module, definers);
            }
        }
        Node::Range { left, right, .. } => {
            for bound in [left, right].into_iter().flatten() {
                recurse(ast, *bound, prefix, in_singleton_class, in_module, definers);
            }
        }
        Node::AttrWrite { receiver, value, .. } => {
            if let Some(r) = receiver {
                recurse(ast, *r, prefix, in_singleton_class, in_module, definers);
            }
            recurse(ast, *value, prefix, in_singleton_class, in_module, definers);
        }
        _ => {}
    }
}

/// Whether `id` opens a namespace [`walk_namespace`] descends into: a real
/// `class`/`module`, or a layout-carrying constant write whose block body is the
/// new class's own body.
fn is_namespace_node(
    ast: &LoweredAst,
    id: NodeId,
    prefix: &[String],
    layouts: &MetaLayouts,
) -> bool {
    match ast.get(id) {
        Node::ClassDef { .. } | Node::ModuleDef { .. } => true,
        Node::ConstantWrite { name, value, .. } => {
            layouts.get(&qualify_join(prefix, name)).is_some()
                && matches!(ast.get(*value), Node::Call { .. })
        }
        _ => false,
    }
}

/// The instance-method visibility table of a `do ... end` class body, read from
/// the LOWERED statements (a block body carries no `ClassDef.method_visibilities`).
/// Mirrors the reference's discovery: a BARE `private` / `protected` / `public`
/// flips the running default for every SUBSEQUENT def, while the
/// `private :foo` args form back-patches the named method and leaves the default
/// alone. A `private def foo` records at the unchanged running default — the
/// reference's own tracking gap, preserved.
fn block_visibilities(ast: &LoweredAst, body: &[NodeId]) -> Vec<(String, Visibility)> {
    let mut current = Visibility::Public;
    let mut out: Vec<(String, Visibility)> = Vec::new();
    for &child in body {
        match ast.get(child) {
            Node::Definition { name: Some(n), .. } => out.push((n.clone(), current)),
            Node::Call { method, receiver: None, args, .. } => {
                let Some(modifier) = visibility_of_modifier(method) else { continue };
                if args.is_empty() {
                    current = modifier;
                    continue;
                }
                for &arg in args {
                    let (Node::SymbolLit { value, .. } | Node::StringLit { value, .. }) =
                        ast.get(arg)
                    else {
                        continue;
                    };
                    if let Some(slot) = out.iter_mut().rev().find(|(n, _)| n == value) {
                        slot.1 = modifier;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Map a receiver-less call name to its visibility, or `None` when it is not one
/// of the three modifiers.
fn visibility_of_modifier(name: &str) -> Option<Visibility> {
    match name {
        "public" => Some(Visibility::Public),
        "protected" => Some(Visibility::Protected),
        "private" => Some(Visibility::Private),
        _ => None,
    }
}

/// Build a [`MethodSig`] borrowing the def's arena fields.
#[allow(clippy::too_many_arguments)]
fn sig_of<'a>(
    name: &'a str,
    body: &'a [NodeId],
    params: &'a Option<Vec<String>>,
    param_shape: &'a ParamShape,
    has_explicit_return: bool,
    singleton: bool,
    module_function: bool,
    offset: usize,
) -> MethodSig<'a> {
    MethodSig { name, body, params, param_shape, has_explicit_return, singleton, module_function, offset }
}

/// Classify + render one method (instance or singleton), or `None` when skipped
/// (private/protected without `--include-private`, a non-simple parameter shape,
/// `initialize`, or an `untyped` / `Dynamic[top]` / low-confidence return).
#[allow(clippy::too_many_arguments)]
fn method_candidate(
    ast: &LoweredAst,
    sig: &MethodSig,
    visibility: Option<Visibility>,
    class_name: &str,
    path: &str,
    include_private: bool,
    index: &CoreIndex,
    typer: &Typer,
    env: &TypeEnv,
    fqns: &std::collections::HashSet<String>,
    sig_env: &SigEnv,
    interner: &mut Interner,
) -> Option<Candidate> {
    // Visibility: skip private / protected unless `--include-private` (reference
    // `visibility_excludes?` — returns false for a singleton AND for a
    // module_function def, whose `self?.` row writes the singleton half the
    // running `private` never touched — `mf; private; def g; mf; def h` emits
    // `self?.h`, rigor-rs#493 item 10).
    if !include_private
        && !sig.singleton
        && !sig.module_function
        && matches!(visibility, Some(Visibility::Private | Visibility::Protected))
    {
        return None;
    }

    // `initialize` (instance only) is special: the reference emits a `-> void`
    // constructor STUB with the FULL param shape rendered as `untyped`, never the
    // inferred body type — and EXCLUDES a trivial (all-empty) initialize (the
    // `Object#initialize` RBS covers it). Checked BEFORE the `simple_parameter_shape`
    // gate below, since the stub renders any param shape (kwargs/optionals/splat).
    // A `def self.initialize` is an ordinary singleton method, not a constructor.
    if !sig.singleton && sig.name == "initialize" {
        if sig.param_shape.is_trivial() {
            return None;
        }
        let params = render_initialize_params(sig.param_shape);
        // `block_signature_suffix`: a `&block` belongs AFTER the parens —
        // inside them (`(…, ?{ (?) -> void })`) RBS rejects the whole file.
        let block = if sig.param_shape.has_block { " ?{ (*untyped) -> untyped }" } else { "" };
        return Some(Candidate {
            file: path.to_string(),
            class_name: class_name.to_string(),
            method_name: "initialize".to_string(),
            kind: "instance",
            rbs: format!("def initialize: ({params}){block} -> void"),
            // The reference stub's `inferred_return` is `untyped` (the rbs is
            // `-> void`, but the candidate carries the fallback type).
            inferred_return: "untyped".to_string(),
            // `initialize` BYPASSES env lookup entirely (probe K: an identical
            // declared `initialize` is still emitted `new_method`) — keep the stub
            // path exactly as-is, before any classification.
            classification: "new_method",
            definer: Some((sig.offset, sig.module_function)),
            declared_return_rbs: None,
            decl_header: String::new(),
            skip_reason: None,
        });
    }

    let kind = if sig.singleton { "singleton" } else { "instance" };
    // A row the reference writes a candidate for but this port does not emit:
    // `reason` is the reference's own skip reason where both engines skip for
    // it, `None` (a `"dropped"` row) where the port declines on its own.
    let row = |reason: Option<&'static str>| Some(non_emitted(path, class_name, sig.name, kind, reason));

    // Simple parameter shape: rigor-rs sets `params = None` for exactly the
    // splat/post/kwargs/block/optional forms the reference's
    // `simple_parameter_shape?` rejects. Only plain requireds qualify.
    let Some(arity) = sig.params.as_ref().map(Vec::len) else { return row(None) };

    // Explicit-return union (reference `DefReturnTyper#union_with_explicit_returns`,
    // oracle-probed 2026-07-10): the return type is `union(tail, every collectible
    // `return E` type)` — a bare `return` contributes `nil`; a `return` inside a
    // BLOCK or a nested def is BARRIERED (reference `RETURN_BARRIER_NODES` —
    // block/lambda/def — a deliberate design, matched here); a MULTI-value
    // `return a, b` makes the method SKIP (the reference silently drops its type,
    // an unsound emit we do not adopt — under-emit is FP-safe); members sort by
    // their `describe(:short)` string (reference `Combinator#sort_members`) and
    // dedup; any `untyped`-erasing member skips the method (`dynamic_top?` on the
    // erased union).
    let Some(returns) = collect_explicit_returns(ast, sig) else { return row(None) };

    // Tail type (reference `body_last_expression` + `safe_type_of`): the last
    // statement's type; an assignment tail evaluates to its RHS; a `return E`
    // tail evaluates to its value (`nil` when bare).
    let Some(tail_ty) = def_return_type(ast, typer, index, class_name, fqns, sig.body, env, interner) else { return row(None) };

    // Assemble the member list: flatten(tail) + each return's type (bare → nil),
    // dedup by TypeId (structural identity via the hash-consing interner).
    let mut members: Vec<TypeId> = Vec::new();
    let push_flat = |interner: &mut Interner, members: &mut Vec<TypeId>, ty: TypeId| {
        let flat: Vec<TypeId> = match interner.get(ty) {
            Type::Union(ms) => ms.clone(),
            _ => vec![ty],
        };
        for m in flat {
            if !members.contains(&m) {
                members.push(m);
            }
        }
    };
    push_flat(interner, &mut members, tail_ty);
    for ret in &returns {
        let ty = match ret {
            Some(v) => typer.type_of(ast, *v, env, interner),
            None => interner.nil(),
        };
        push_flat(interner, &mut members, ty);
    }

    // `dynamic_top?` (a reference PERMANENT skip): any Top/Dynamic member — or any
    // member erasing to `untyped` — collapses the erased union to `untyped`; the
    // method is skipped rather than emitted as `-> untyped`.
    if members
        .iter()
        .any(|&m| matches!(interner.get(m), Type::Top | Type::Dynamic(_)))
    {
        return row(Some("sig.skipped.untyped-return"));
    }

    // Sort by the DESCRIBE string (reference `sort_members` — `describe(:short)`,
    // NOT the erased form), qualifying source-class names so a union's member
    // ORDER matches the reference (which describes a source nominal by its FQN).
    members.sort_by_key(|&m| describe_qualified(interner, index, typer.source(), m, class_name, fqns));
    let mut erased_members: Vec<String> = Vec::new();
    for &m in &members {
        // Erase with QUALIFIED source-class names (reference emits the FQN
        // `Rigor::Triage::Selector`, not the written short `Selector`).
        let e = erase_qualified(interner, index, typer.source(), m, class_name, fqns);
        // Any `untyped` ANYWHERE in a member (whole `untyped`, or buried inside a
        // composite — `[untyped, 0]`, `Hash[String, untyped]`) skips the method:
        // an untyped hole marks a point where rigor-rs's inference lost precision,
        // and the reference's inference reads the SAME code differently there
        // (sweep-proven: `Baseline#filter` emitted `[untyped, untyped]` vs the
        // reference's `[Array[untyped], 0 | Integer]` — a shared-method byte
        // mismatch). The sound-superset excess applies only to CONFIDENT types.
        if e == "untyped" {
            // The whole member erases to `untyped`, so the reference's union
            // does too (`dynamic_top?` reads the erased form).
            return row(Some("sig.skipped.untyped-return"));
        }
        if e.contains("untyped") {
            // …unless the untyped sits inside a bare generic's type-arg slot:
            // `Range[untyped]` / `Array[untyped]` are the reference's own
            // elaborated emits (rigor-rs#493 item 7), not inference holes.
            if !is_nominal_untyped_args(&e) {
                return row(None);
            }
        }
        // A bare GENERIC nominal member (`Array` / `Hash` / …) would be
        // `Array[untyped]` after the reference's `TypeElaborator` fill (deferred
        // here), so its presence skips the method rather than emit an
        // under-elaborated form that would byte-diverge on a shared method.
        if is_bare_generic_name(&e) {
            return row(None);
        }
        if !erased_members.contains(&e) {
            erased_members.push(e);
        }
    }
    let erased = erased_members.join(" | ");

    let head = if arity == 0 {
        "()".to_string()
    } else {
        format!("({})", vec!["untyped"; arity].join(", "))
    };
    let ret = paren_wrap_union(&erased);
    // reference `method_def_prefix`, in that precedence: a singleton is
    // `def self.`, a bare-`module_function`-governed instance def is the DUAL
    // `def self?.`, everything else `def `.
    let prefix = if sig.singleton {
        "def self."
    } else if sig.module_function {
        "def self?."
    } else {
        "def "
    };
    let rbs = format!("{prefix}{}: {head} -> {ret}", sig.name);

    // Generation-time env classification (ADR-14 slice 10). `None` ⇒ the method
    // is DROPPED (equivalent to an already-declared return, or a conservative
    // drop against an unresolvable / incomplete-chain declaration) — the same
    // observable output as the reference building an EQUIVALENT candidate and the
    // renderer's `EMITTABLE` filter discarding it.
    let Some((classification, declared_return_rbs)) =
        classify(sig, class_name, &erased, &members, ast, index, sig_env, interner)
    else {
        return row(None);
    };

    // Declared `untyped`: the reference's tighter-return writes the inferred
    // member WIDENED to its nominal — `def i = 1` against `-> untyped` emits
    // `() -> Integer`, not `() -> 1` (rigor-rs#544). Members that erase to a
    // literal spelling re-erase to their class name; others keep their form.
    let (rbs, erased) = if declared_return_rbs.as_deref() == Some("untyped") {
        let mut widened: Vec<String> = Vec::new();
        for (m, e) in members.iter().zip(&erased_members) {
            if e == "nil" {
                // A `nil` member is not a pinned literal — it keeps its
                // spelling inside the widened union.
                if !widened.contains(&e.clone()) {
                    widened.push(e.clone());
                }
                continue;
            }
            if matches!(e.as_str(), "true" | "false") {
                // A bool literal member widens to the `(false | true)` pair,
                // not `TrueClass` / `FalseClass` (rigor-rs#544).
                for b in ["false", "true"] {
                    if !widened.contains(&b.to_string()) {
                        widened.push(b.to_string());
                    }
                }
                continue;
            }
            let e = if matches!(interner.get(*m), Type::Constant(_))
                && index.class_name_of(interner, *m) != Some(e.as_str())
            {
                index
                    .class_name_of(interner, *m)
                    .map(str::to_string)
                    .unwrap_or_else(|| e.clone())
            } else {
                e.clone()
            };
            if !widened.contains(&e) {
                widened.push(e);
            }
        }
        // The reference's `erase_to_rbs` union lists members in SORTED order
        // (`Integer < String`, `Integer < nil`, `false < true`).
        widened.sort();
        let erased = widened.join(" | ");
        let ret = paren_wrap_union(&erased);
        (format!("{prefix}{}: {head} -> {ret}", sig.name), erased)
    } else {
        (rbs, erased)
    };

    Some(Candidate {
        file: path.to_string(),
        class_name: class_name.to_string(),
        method_name: sig.name.to_string(),
        kind,
        rbs,
        inferred_return: erased,
        classification,
        declared_return_rbs,
        decl_header: String::new(),
        skip_reason: None,
        definer: Some((sig.offset, sig.module_function)),
    })
}

/// A row for a method the generator considered and does not emit: `"skipped"`
/// with the reference's `sig.skipped.*` reason, or `"dropped"` (no reason) for a
/// port-only decline or an equivalent declaration. A top-level `def` has an
/// empty `class_name` (the reference's `nil`).
fn non_emitted(
    path: &str,
    class_name: &str,
    method_name: &str,
    kind: &'static str,
    reason: Option<&'static str>,
) -> Candidate {
    Candidate {
        file: path.to_string(),
        class_name: class_name.to_string(),
        method_name: method_name.to_string(),
        kind,
        rbs: String::new(),
        inferred_return: String::new(),
        classification: if reason.is_some() { "skipped" } else { "dropped" },
        declared_return_rbs: None,
        decl_header: String::new(),
        skip_reason: reason,
        definer: None,
    }
}

/// Reference `live_definitions` over the candidate rows: `definer_table` is
/// `definers` (collected alongside the walk), `live_definer?` keeps the LAST
/// certain entry of a slot unless an uncertain entry sits after it (contested
/// → nothing is written), and a candidate survives only when its def owns
/// every slot it defines — except the singleton-only survival, which a
/// `def self?.u` downgrades to `def self.u`.
fn live_definitions(out: &mut Vec<Candidate>, definers: &[Definer]) {
    /// `(class_name, kind, method_name)` — the reference's definer-table key.
    type Slot = (String, &'static str, String);
    let mut table: HashMap<Slot, Vec<(usize, bool)>> = HashMap::new();
    for d in definers {
        table
            .entry((d.class_name.clone(), d.kind, d.method_name.clone()))
            .or_default()
            .push((d.offset, d.certain));
    }
    let live_at = |slot: &Slot, offset: usize| -> bool {
        let Some(entries) = table.get(slot) else { return true };
        if entries.len() <= 1 {
            return true;
        }
        let Some(last_certain) = entries.iter().filter(|(_, c)| *c).map(|(o, _)| *o).max() else {
            return false;
        };
        if entries.iter().any(|(o, c)| !*c && *o > last_certain) {
            return false;
        }
        offset == last_certain
    };
    for c in out.iter_mut() {
        let Some((offset, module_function)) = c.definer else { continue };

        let own = (c.class_name.clone(), c.kind, c.method_name.clone());
        let live = live_at(&own, offset).then_some(own);
        let mut single = None;
        if module_function {
            let s = (c.class_name.clone(), "singleton", c.method_name.clone());
            if live_at(&s, offset) {
                single = Some(s);
            }
        }
        let all = live.is_some() && (!module_function || single.is_some());
        if all {
            continue;
        }
        if live.is_none() && single.is_some() {
            // The instance half was replaced; the singleton copy Ruby keeps is
            // written `def self.name` (reference `live_definitions`).
            c.kind = "singleton";
            c.rbs = c.rbs.replacen("def self?.", "def self.", 1);
            continue;
        }
        // Nothing survived — mark the row dropped (invisible downstream, as a
        // `"dropped"` classification already is).
        c.skip_reason = None;
        c.classification = "dropped";
    }
    out.retain(|c| c.classification != "dropped" || c.skip_reason.is_some());
}

/// Classify one already-inferred candidate against the project's own RBS
/// ([`SigEnv`]), ported from the reference `classify_def`'s
/// `lookup_existing_method` → `compare_against_declared` tail (probes A/N/O/P,
/// oracle-confirmed). Returns `(classification, declared_return_rbs)`, or `None`
/// to DROP the candidate (an EQUIVALENT declaration, or a conservative drop).
///
/// `inferred_erased` is the inferred return's erased RBS string (the equivalence
/// key); `members` is the deduped inferred member set — a single member is a bare
/// carrier eligible for tightening, more than one is a union that never tightens
/// a bare declared class (an FP-safe under-emit).
#[allow(clippy::too_many_arguments)]
fn classify(
    sig: &MethodSig,
    class_name: &str,
    inferred_erased: &str,
    members: &[TypeId],
    ast: &LoweredAst,
    index: &CoreIndex,
    sig_env: &SigEnv,
    interner: &Interner,
) -> Option<(&'static str, Option<String>)> {
    match sig_env.lookup(index, class_name, sig.name, sig.singleton) {
        // NotDeclared ⇒ a fresh method (emit `# [new]`).
        Lookup::NotDeclared => Some(("new_method", None)),
        // Declared but the return is unresolvable, or the ancestor chain is
        // incomplete ⇒ conservative DROP (never a wrong `# [new]` tag).
        Lookup::Declared(None) => None,
        Lookup::Declared(Some(decl)) => {
            // Equivalent: the inferred erases to the declared string (probe C/P).
            if inferred_erased == decl {
                return None;
            }
            // Declared `untyped` accepts every concrete inferred form and is
            // never accepted back — always `tighter` (rigor-rs#544). The
            // single-member gate below does not apply: a union widens
            // member-wise in the emit. A whole-return `nil` reads as
            // equivalent (the reference's `untyped`-accepts-`nil`), so it
            // drops; a literal `nil` INSIDE a union member stays `nil`.
            if decl == "untyped" {
                if inferred_erased == "nil" {
                    return None;
                }
                return Some(("tighter_return", Some(decl)));
            }
            // A tightening must be a SINGLE bare carrier whose nominal-of is
            // exactly the declared class. A union (`> 1` member) never bare-tightens
            // a single declared class (FP-safe under-emit; e.g. declared union member
            // loss, `Integer | Float` narrowing `Numeric`).
            let inferred_ty = match members {
                [single] => *single,
                _ => return None,
            };
            // Wider / unrelated: the inferred's nominal must ACCEPT the
            // declared class — `decl` is the inferred class itself or one of
            // its ancestors (declared `Numeric`, inferred `4.2` → `Float`;
            // rigor-rs#544). String equality alone missed every strict
            // subtype. An ancestor chain that fails to resolve (unknown or
            // incomplete) refuses — the decline side.
            let inferred_class = index.class_name_of(interner, inferred_ty);
            let accepts = inferred_class == Some(decl.as_str())
                || inferred_class
                    .and_then(|c| index.ancestor_names(c))
                    .is_some_and(|chain| chain.contains(&decl.as_str()));
            if !accepts {
                return None;
            }
            // `pins_literal_over_declaration?` (#837): a proposal that erases to
            // an RBS literal (`"hi"`, `1`, `:a`, `true`, `nil`) never tightens a
            // declaration; the declared nominal is the author's abstraction over
            // the body. A constant erasing to its class name (a Float) still
            // may; a Range's and a Set's erasure carries a type argument, so
            // they are excluded by class. (The inferred value's class is `decl`
            // here: the check above returned otherwise.)
            // `rbs_literal?`: a Constant is PINNED only when its erasure is a
            // literal spelling — i.e. differs from its own class name, not from
            // `decl` (a Float constant erases to `Float`, an ordinary nominal;
            // rigor-rs#544).
            if matches!(interner.get(inferred_ty), Type::Constant(_))
                && !matches!(decl.as_str(), "Range" | "Set")
                && inferred_class != Some(inferred_erased)
            {
                return None;
            }
            // Collection→shape lenience loss: declared bare `Array`/`Hash`/… vs an
            // inferred `Tuple`/`HashShape` (`narrows_collection_to_shape?`).
            if narrows_collection_to_shape(&decl, interner, inferred_ty) {
                return None;
            }
            // `computed_literal_tightening?`: an inferred `Constant` whose def's RAW
            // tail statement is NOT a directly-authored literal node — the precision
            // came from inference over an internal computation, not the author's
            // contract (probe P: `def hash; [1].size; end` folds `1` but the tail
            // `[1].size` is a Call). NB: the RAW `sig.body.last()` node, NOT the
            // assignment-unwrapped typing tail.
            let raw_tail = ast.get(*sig.body.last()?);
            if computed_literal_tightening(interner, inferred_ty, raw_tail) {
                return None;
            }
            Some(("tighter_return", Some(decl)))
        }
    }
}

/// The reference `narrows_collection_to_shape?`: a declared generic-collection
/// nominal whose inferred form collapsed to a fixed `Tuple` / `HashShape`. The
/// member list is the reference's `GENERIC_COLLECTION_CLASSES` constant
/// (`generator.rb`), read verbatim — NOT guessed.
fn narrows_collection_to_shape(declared: &str, interner: &Interner, inferred: TypeId) -> bool {
    const GENERIC_COLLECTION_CLASSES: &[&str] =
        &["Array", "Hash", "Set", "Range", "Enumerable", "Enumerator", "Enumerator::Lazy"];
    if !GENERIC_COLLECTION_CLASSES.contains(&declared) {
        return false;
    }
    matches!(interner.get(inferred), Type::Tuple(_) | Type::HashShape(_))
}

/// The reference `computed_literal_tightening?`: the inferred type is a
/// `Type::Constant` AND the def's RAW last statement is not a directly-authored
/// literal node. The reference's `body_last_expression` does NOT unwrap an
/// assignment, so `def m; x = 1; end` DROPS (the raw tail is a `LocalVariableWrite`,
/// not an `IntegerLit`) even though its typing tail unwraps to `Constant<1>`.
fn computed_literal_tightening(interner: &Interner, inferred: TypeId, raw_tail: &Node) -> bool {
    if !matches!(interner.get(inferred), Type::Constant(_)) {
        return false;
    }
    !matches!(
        raw_tail,
        Node::IntegerLit { .. }
            | Node::FloatLit { .. }
            | Node::StringLit { .. }
            | Node::SymbolLit { .. }
            | Node::TrueLit { .. }
            | Node::FalseLit { .. }
            | Node::NilLit { .. }
    )
}

/// Collect the def's collectible explicit-return value expressions, or `None`
/// when the method must be SKIPPED. Each element is `Some(value NodeId)` for a
/// single-value `return e` / `None` for a bare `return` (→ `nil`). Ports the
/// reference `DefReturnTyper#collect_return_types` semantics over the lowered
/// arena:
///
/// - **Barriers** (reference `RETURN_BARRIER_NODES` = block / lambda / def): a
///   `return` inside a `Call`'s `block_body` or a nested def/class/module is NOT
///   collected. A lambda's `return` never lowers to [`Node::Return`] at all (the
///   lambda routes through the recovered-children fallthrough), so the lambda
///   barrier holds structurally.
/// - **Multi-value** `return a, b`: the reference silently contributes NOTHING
///   (emitting a signature that misses the tuple — an unsound emit); rigor-rs
///   SKIPS the method instead (under-emit, FP-safe, no shared-method mismatch).
/// - **Residual ambiguity**: `has_explicit_return` trips on returns inside
///   lambdas AND inside unhandled wrappers; the former the reference barriers
///   (safe to emit) but the latter it collects. When the flag is set yet NO
///   [`Node::Return`] exists anywhere in the def, the two are indistinguishable
///   → skip (rare, FP-safe).
///
/// Membership is by span containment against the def's body-statement spans
/// (the arena is flat; spans nest strictly), mirroring how outline/flow walks
/// resolve nesting.
fn collect_explicit_returns(ast: &LoweredAst, sig: &MethodSig) -> Option<Vec<Option<NodeId>>> {
    if !sig.has_explicit_return {
        return Some(Vec::new());
    }

    let regions: Vec<(usize, usize)> =
        sig.body.iter().map(|&id| ast.get(id).span()).collect();
    let within = |s: (usize, usize), regions: &[(usize, usize)]| {
        regions.iter().any(|&(rs, re)| rs <= s.0 && s.1 <= re)
    };

    // Barrier regions inside this def: block bodies + nested class-like scopes.
    // (The def's own body statements are the regions, so any Definition matched
    // within them is a NESTED def, never the def itself.)
    let mut barriers: Vec<(usize, usize)> = Vec::new();
    for (_, node) in ast.iter() {
        match node {
            Node::Call { block_body, span, .. }
                if !block_body.is_empty() && within(*span, &regions) =>
            {
                for &b in block_body {
                    barriers.push(ast.get(b).span());
                }
            }
            Node::Definition { span, .. }
            | Node::ClassDef { span, .. }
            | Node::ModuleDef { span, .. }
                if within(*span, &regions) =>
            {
                barriers.push(*span);
            }
            _ => {}
        }
    }

    let mut found_any = false;
    let mut collected: Vec<Option<NodeId>> = Vec::new();
    for (_, node) in ast.iter() {
        if let Node::Return { values, span } = node {
            if !within(*span, &regions) {
                continue;
            }
            found_any = true;
            if within(*span, &barriers) {
                continue; // block / nested-def barrier (reference design)
            }
            match values.len() {
                0 => collected.push(None),
                1 => collected.push(Some(values[0])),
                _ => return None, // multi-value return → skip (see above)
            }
        }
    }

    // Flag set but no Return lowered → lambda-or-unhandled ambiguity → skip.
    if !found_any {
        return None;
    }
    Some(collected)
}

/// A method's inferred return type, or `None` for an empty body (reference
/// `DefReturnTyper`): the last statement's type, an assignment tail evaluating to
/// its RHS value, a `return E` tail to its value (`nil` when bare — the oracle
/// types a tail `return 42` as `42`; a multi-value tail declines). Typed against
/// the top-level env — a def-LOCAL binding types `Dynamic` (the documented
/// `annotate` deferral) and is then skipped upstream.
#[allow(clippy::too_many_arguments)]
fn def_return_type(
    ast: &LoweredAst,
    typer: &Typer,
    index: &CoreIndex,
    class_name: &str,
    fqns: &std::collections::HashSet<String>,
    body: &[NodeId],
    env: &TypeEnv,
    interner: &mut Interner,
) -> Option<TypeId> {
    let &tail = body.last()?;
    let target = match ast.get(tail) {
        Node::LocalVariableWrite { value, .. }
        | Node::LocalVariableOpWrite { value, .. }
        | Node::VariableWrite { value, .. }
        | Node::InstanceVariableWrite { value, .. }
        | Node::ConstantWrite { value, .. } => *value,
        Node::Return { values, .. } => match values.len() {
            0 => return Some(interner.nil()),
            1 => values[0],
            _ => return None,
        },
        _ => tail,
    };
    // Literal carriers `type_of` leaves Dynamic — sig-gen still emits their
    // nominal (rigor-rs#493 item 7): `Range[bound-class-union]`, `Regexp`,
    // `Rational`, `Complex`. Interpolated regexps stay declined (they lower
    // through recovery, so no dedicated node exists here).
    if let Some(lit) = literal_tail_nominal(ast, typer, env, target, index, class_name, fqns, interner) {
        return Some(lit);
    }
    Some(typer.type_of(ast, target, env, interner))
}

/// The sig-gen-side nominal for a literal tail `check` leaves Dynamic
/// (rigor-rs#493 item 7). `Range[T]` takes the union of its bounds' nominal
/// CLASS names — `1..2` ⇒ `Integer`, `1...2.5` ⇒ `Float | Integer`; a
/// `Dynamic` or `nil` bound contributes nothing (`(x..2)` ⇒ `Range[Integer]`,
/// `(x..)` ⇒ `Range[untyped]`).
#[allow(clippy::too_many_arguments)]
fn literal_tail_nominal(
    ast: &LoweredAst,
    typer: &Typer,
    env: &TypeEnv,
    target: NodeId,
    index: &CoreIndex,
    class_name: &str,
    fqns: &std::collections::HashSet<String>,
    interner: &mut Interner,
) -> Option<TypeId> {
    let bare = |name: &str, interner: &mut Interner| {
        index
            .class_id(name)
            .map(|class| interner.intern(Type::Nominal { class, args: vec![] }))
    };
    match ast.get(target) {
        Node::RegexpLit { .. } => bare("Regexp", interner),
        Node::RationalLit { .. } => bare("Rational", interner),
        Node::ImaginaryLit { .. } => bare("Complex", interner),
        Node::Range { left, right, .. } => {
            // Each bound contributes its ERASED member form: a scalar
            // `Constant` widens to its class (`1` ⇒ `Integer`), a
            // Tuple/HashShape keeps its literal shape (`[1]`, `{ a: 1 }`),
            // and a bare generic nominal elaborates its args to `untyped`
            // (`Array.new` ⇒ `Array[untyped]`). `nil` and `Dynamic` members
            // contribute nothing (rigor-rs#493 item 7).
            let mut arg_members: Vec<TypeId> = Vec::new();
            for bound in [left, right].into_iter().flatten() {
                let ty = typer.type_of(ast, *bound, env, interner);
                // A bound that types DIRECTLY to a scalar Constant widens to
                // its class; a UNION bound's members contribute as-is —
                // literal `2` stays `2`, a `nil` member stays `nil` (the
                // reference's range_endpoint_element_type recurses over the
                // member list but does not widen inside it).
                let (ms, widen) = match interner.get(ty) {
                    Type::Union(ms) => (ms.clone(), false),
                    _ => (vec![ty], true),
                };
                for m in ms {
                    if matches!(interner.get(m), Type::Dynamic(_)) {
                        continue;
                    }
                    let member = match interner.get(m) {
                        // `nil` contributes nothing as a WHOLE bound (but a
                        // union's `nil` member is kept, above).
                        Type::Constant(rigor_types::Scalar::Nil) if widen => continue,
                        Type::Constant(_) if widen => {
                            let Some(cname) = index.class_name_of(interner, m) else {
                                continue;
                            };
                            match index.class_id(cname) {
                                Some(class) => {
                                    interner.intern(Type::Nominal { class, args: vec![] })
                                }
                                None => continue,
                            }
                        }
                        // A bare generic (`Array`, `Hash`, …) elaborates to
                        // `Class[untyped]` — the reference's TypeElaborator
                        // fills the args.
                        Type::Nominal { class, args }
                            if args.is_empty()
                                && is_bare_generic_name(
                                    index.class_name_for_id(*class).unwrap_or(""),
                                ) =>
                        {
                            let (class, untyped) = (*class, interner.untyped());
                            interner.intern(Type::Nominal {
                                class,
                                args: vec![untyped],
                            })
                        }
                        _ => m,
                    };
                    if !arg_members.contains(&member) {
                        arg_members.push(member);
                    }
                }
            }
            // The erased arg lists its members in the reference's
            // describe-sorted union order (`Float | Integer` for `1...2.5`,
            // `Integer | [1]` for `[1]..2`).
            arg_members.sort_by_key(|&m| {
                erase_qualified(interner, index, typer.source(), m, class_name, fqns)
            });
            let arg = match arg_members.len() {
                0 => interner.untyped(),
                1 => arg_members[0],
                // Mint the union verbatim — `Algebra::join` would re-sort by
                // the interner's canonical order, but the erased arg follows
                // the reference's describe-sorted member list.
                _ => interner.intern(Type::Union(arg_members)),
            };
            index
                .class_id("Range")
                .map(|class| interner.intern(Type::Nominal { class, args: vec![arg] }))
        }
        _ => None,
    }
}

/// Whether an erased return is a bare (no type-args) core GENERIC class name —
/// the reference's `TypeElaborator` would fill it to `Class[untyped, …]`, which
/// this slice does not port, so such a return is skipped (a coverage gap, never a
/// wrong emit). Checked on the ERASED string: a value-pinned `Array[Integer]` /
/// `[1, 2]` carries a bracket so it is not bare and still emits; only the exact
/// bare class name matches. The list covers the core generics rigor-rs can infer
/// as a bare return; a bare generic OUTSIDE it is a residual (rare — RBS method
/// returns carry their type args, and literals fold to `Tuple`/`HashShape`).
fn is_bare_generic_name(erased: &str) -> bool {
    const GENERIC: &[&str] =
        &["Array", "Hash", "Set", "Range", "Enumerator", "Enumerator::Lazy"];
    GENERIC.contains(&erased)
}

/// Render an `initialize` stub's parameter list — every param `untyped`
/// (params-observed typing is a later slice), in the reference's
/// `render_initialize_param_list` order: requireds → optionals (`?untyped`) →
/// rest (`*untyped`) → keywords (`name: untyped` / `?name: untyped`) → keyword-
/// rest (`**untyped`). The block is the caller's suffix (`block_signature_suffix`);
/// posts are not carried by [`ParamShape`] and are omitted.
fn render_initialize_params(shape: &ParamShape) -> String {
    let mut parts: Vec<String> = Vec::new();
    for _ in 0..shape.required {
        parts.push("untyped".to_string());
    }
    for _ in 0..shape.optional {
        parts.push("?untyped".to_string());
    }
    if shape.has_rest {
        parts.push("*untyped".to_string());
    }
    for (name, optional) in &shape.keywords {
        let marker = if *optional { "?" } else { "" };
        parts.push(format!("{marker}{name}: untyped"));
    }
    if shape.has_kwrest {
        parts.push("**untyped".to_string());
    }
    parts.join(", ")
}

/// Wrap a rendered return in parens iff it is a TOP-LEVEL union (a ` | ` at
/// bracket depth 0), so `A | B` becomes `(A | B)` in method position (reference
/// `paren_wrap_union` / `top_level_union?`).
fn paren_wrap_union(rendered: &str) -> String {
    if !rendered.contains(" | ") {
        return rendered.to_string();
    }
    let mut depth = 0i32;
    let bytes = rendered.as_bytes();
    for (i, &ch) in bytes.iter().enumerate() {
        match ch {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b' ' if depth == 0 && bytes.get(i + 1) == Some(&b'|') => {
                return format!("({rendered})");
            }
            _ => {}
        }
    }
    rendered.to_string()
}

// ---------------------------------------------------------------------------
// Rendering (reference `Renderer#render_print` / `render_json`)
// ---------------------------------------------------------------------------

/// `--print` text: `# <path>`, then per class `class <name>` / `  # [new]` /
/// `  <rbs>` / `end`, a blank line after each file group.
fn render_text(candidates: &[Candidate]) {
    if candidates.is_empty() {
        println!("No candidates");
        return;
    }
    // Group by file preserving first-seen order.
    let mut files: Vec<&str> = Vec::new();
    for c in candidates {
        if !files.contains(&c.file.as_str()) {
            files.push(&c.file);
        }
    }
    for file in files {
        println!("# {file}");
        let items: Vec<&Candidate> = candidates.iter().filter(|c| c.file == file).collect();
        // Group by class preserving order.
        let mut classes: Vec<&str> = Vec::new();
        for c in &items {
            if !classes.contains(&c.class_name.as_str()) {
                classes.push(&c.class_name);
            }
        }
        for class in classes {
            // The group's declaration line — its own `class` / `module` keyword and
            // any ancestry. Every candidate of a group carries the same string, so
            // the first is the group's (reference reads `methods.first`).
            let header = items
                .iter()
                .find(|c| c.class_name == class)
                .map(|c| c.decl_header.clone())
                .unwrap_or_else(|| format!("class {class}"));
            println!("{header}");
            for c in items.iter().filter(|c| c.class_name == class) {
                println!("  # {}", candidate_tag(c));
                println!("  {}", c.rbs);
            }
            println!("end");
        }
        println!();
    }
}

/// `--diff` text: per candidate `--- <path>: <class>#<method>` / `+ <rbs>` /
/// blank line (reference `render_diff`). rigor-rs emits only NEW methods (no
/// existing-RBS comparison), so there is never a `- def …` declared line — the
/// same shape the reference produces for a `new_method`.
fn render_diff(candidates: &[Candidate]) {
    if candidates.is_empty() {
        println!("No candidates");
        return;
    }
    print!("{}", diff_string(candidates));
}

/// The `--print` comment tag for a candidate (reference `render_classes`):
/// `[new]` for a `new_method`, `[tighter, was: <declared>]` for a
/// `tighter_return`.
fn candidate_tag(c: &Candidate) -> String {
    match &c.declared_return_rbs {
        Some(declared) => format!("[tighter, was: {declared}]"),
        None => "[new]".to_string(),
    }
}

/// Build the `--diff` text body (extracted from [`render_diff`] for testability).
/// A `tighter_return` prints its declared line `- def <name>: () -> <declared>`
/// before the `+` line (reference `render_diff`): the `()` param list and the
/// BARE method name are HARDCODED even for a singleton (`- def build: …` sits
/// above `+ def self.build: …`), and the header stays `Class#method`.
fn diff_string(candidates: &[Candidate]) -> String {
    let mut out = String::new();
    for c in candidates {
        out.push_str(&format!("--- {}: {}#{}\n", c.file, c.class_name, c.method_name));
        if let Some(declared) = &c.declared_return_rbs {
            out.push_str(&format!("- def {}: () -> {declared}\n", c.method_name));
        }
        out.push_str(&format!("+ {}\n\n", c.rbs));
    }
    out
}

/// `--print --format json`: `{ "candidates": [ … ] }` with the reference's
/// per-candidate key set (`file`/`class`/`method`/`kind`/`classification`/`rbs`/
/// `inferred_return`). serde alphabetizes keys (the established insignificant-
/// order divergence).
fn render_json(candidates: &[Candidate]) {
    println!("{}", candidates_json_string(candidates));
}

/// The `--print --format json` payload as a pretty `String` — `{ "candidates":
/// [...] }` (reference `Renderer#render_json`). Extracted so the MCP `sig_gen`
/// tool can reuse it without going through stdout.
fn candidates_json_string(candidates: &[Candidate]) -> String {
    use serde_json::json;
    let rows: Vec<_> = candidates.iter().map(candidate_json).collect();
    crate::json_document::stamp_pretty(&serde_json::to_string_pretty(&json!({ "candidates": rows })).unwrap(), None)
}

/// MCP `sig_gen` tool seam (reference `rigor_sig_gen`, a READ-ONLY
/// `sig-gen --print --format=json`): resolve `raw_paths` (or the config `paths:`
/// when empty) to `.rb` files, build the sig-gen-local [`SigEnv`] from the
/// project signature dirs, generate candidates, and return the `{ "candidates":
/// [...] }` JSON. `--params=observed` is NOT exposed — it is substrate-blocked
/// (see `docs/notes/20260711-siggen-params-observed-substrate-blocked.md`); this
/// seam is always the `untyped` param policy.
pub fn mcp_report_json(raw_paths: &[&str], explicit_config: Option<&Path>) -> String {
    let cfg = crate::Config::load(explicit_config).unwrap_or_else(|f| {
        eprintln!("rigor: {}", f.message);
        crate::Config::default()
    });
    let config_paths: Vec<&str>;
    let raw: &[&str] = if raw_paths.is_empty() {
        config_paths = cfg.paths.iter().map(String::as_str).collect();
        &config_paths
    } else {
        raw_paths
    };
    let files = resolve_paths(raw);
    let project_root = std::env::current_dir()
        .and_then(|d| d.canonicalize())
        .unwrap_or_else(|_| PathBuf::from("."));
    let sig_env = SigEnv::build(&cfg.all_signature_dirs(&project_root));
    candidates_json_string(&generate_run(&files, false, &sig_env, &writes).all_rows)
}

/// One candidate's JSON object (reference `MethodCandidate#to_h`): the per-
/// candidate key set with the real `classification`, and `declared_return_rbs`
/// present ONLY on a `tighter_return` (the reference `.compact`s the nil).
fn candidate_json(c: &Candidate) -> serde_json::Value {
    use serde_json::json;
    // `MethodCandidate#to_h.compact`, in the reference's key order:
    // `class` is nil for a top-level `def`, and a nil `rbs` /
    // `inferred_return` (every skipped row) leaves the key out entirely.
    let mut obj = json!({ "file": c.file });
    if !c.class_name.is_empty() {
        obj.as_object_mut().unwrap().insert("class".to_string(), json!(c.class_name));
    }
    obj.as_object_mut().unwrap().insert("method".to_string(), json!(c.method_name));
    obj.as_object_mut().unwrap().insert("kind".to_string(), json!(c.kind));
    obj.as_object_mut()
        .unwrap()
        .insert("classification".to_string(), json!(c.classification));
    if !c.rbs.is_empty() {
        obj.as_object_mut().unwrap().insert("rbs".to_string(), json!(c.rbs));
    }
    if !c.inferred_return.is_empty() {
        obj.as_object_mut().unwrap().insert("inferred_return".to_string(), json!(c.inferred_return));
    }
    if let Some(dr) = &c.declared_return_rbs {
        obj.as_object_mut().unwrap().insert("declared_return_rbs".to_string(), json!(dr));
    }
    if let Some(reason) = c.skip_reason {
        obj.as_object_mut().unwrap().insert("skip_reason".to_string(), json!(reason));
    }
    obj
}

// ---------------------------------------------------------------------------
// `--write` (reference `Writer`) — CREATE + UPDATE/merge
// ---------------------------------------------------------------------------

/// The outcome of writing one target `.rbs` file (reference `WriteResult`).
struct WriteResult {
    source: String,
    target: String,
    /// `"created"` | `"updated"` | `"noop"` | `"skipped_outside_sig_root"` |
    /// `"skipped_invalid_encoding"` | `"skipped_unparseable"`.
    action: &'static str,
    applied: Vec<Candidate>,
    /// Candidates the merge declined because a user-authored member of the same
    /// `(name, kind)` already exists with a DIFFERENT declared return (reference
    /// `merge_into_existing_class`'s `skipped` accumulator). Empty on create.
    skipped: Vec<SkipEntry>,
    /// The refusal detail (reference `WriteResult#error`), on
    /// `skipped_invalid_encoding` / `skipped_unparseable` only.
    error: Option<String>,
    /// `replaced` (reference `WriteResult#replaced`): applied candidates that
    /// rewrote an existing member under `--overwrite`, with the old line.
    replaced: Vec<(Candidate, String)>,
    /// `repaired_headers` (#2580): `Name[params]` per bare header repaired.
    repaired: Vec<String>,
}

/// One skipped (user-authored-conflict) candidate + the classification metadata
/// the JSON report surfaces. `reason` is always `user_authored` so it is
/// hardcoded in the renderer.
struct SkipEntry {
    candidate: Candidate,
    /// The candidate's own classification (`new_method` / `tighter_return`).
    classification: &'static str,
    /// The declared return generation compared against, on a `tighter_return`.
    declared_return_rbs: Option<String>,
}

/// `rigor sig-gen --write [paths]` — CREATE + UPDATE (reference `Writer`).
///
/// Each candidate is routed to its target `.rbs` (reference `PathMapper`): the
/// [`LayoutIndex`] maps its `class_name` to an existing sig file (consolidated
/// layout) FIRST, falling back to the 1:1 mirror. Candidates are then grouped by
/// target; a MISSING target is created (`create_new`), an EXISTING one is merged
/// through [`update_existing`] — new members spliced before the class's `end`,
/// user-authored conflicts preserved. A file's candidates may split across a
/// consolidated target and a mirror target (per-candidate grouping).
fn cmd_write(
    files: &[String],
    include_private: bool,
    format: &str,
    overwrite: bool,
    cfg: &crate::Config,
    sig_env: &SigEnv,
    dry_run: bool,
) -> ExitCode {
    let project_root = std::env::current_dir()
        .and_then(|d| d.canonicalize())
        .unwrap_or_else(|_| PathBuf::from("."));
    let source_root = cfg
        .paths
        .first()
        .and_then(|p| Path::new(p).file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "lib".to_string());
    let sig_root = cfg
        .signature_paths
        .first()
        .and_then(|p| Path::new(p).file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sig".to_string());

    // Pre-scan the configured signature dirs so a candidate whose class already
    // lives in a consolidated `.rbs` routes there (reference `LayoutIndex`).
    let layout = LayoutIndex::build(&cfg.signature_paths, &project_root);

    // Generate candidates + namespace metadata per source file, routing each
    // candidate to its target via the layout index (reference `write_all`).
    let mut merged = NamespaceInfo::default();
    let mut tagged: Vec<(PathBuf, Candidate)> = Vec::new();
    // Each `Data.define`/`Struct.new` shell FQN routed to its target — the same
    // target as its enclosing class (a shell lives in the source file with its
    // constant). Preserves source order per file for stable shell rendering.
    let mut tagged_shells: Vec<(PathBuf, String)> = Vec::new();
    // `Generator#written?` under a target writer (#2461): a `new_method`
    // candidate aimed at a target the writer will write — inside the sig root,
    // and either missing or an existing UTF-8 file that parses.
    let sig_root_dir_w = project_root.join(&sig_root);
    let accepts = |target: &Path| -> bool {
        if !target.starts_with(&sig_root_dir_w) {
            return false;
        }
        if !target.exists() {
            return true;
        }
        std::fs::read(target)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .is_some_and(|src| ruby_rbs::node::parse(&src).is_ok())
    };
    let written = |c: &Candidate| -> bool {
        writes(c)
            && accepts(&target_for(&c.file, &c.class_name, &source_root, &sig_root, &project_root, &layout))
    };
    let Generated { candidates: all_candidates, info: run_info, unresolvable, skips, .. } =
        generate_run(files, include_private, sig_env, &written);
    for c in all_candidates {
        let target =
            target_for(&c.file, &c.class_name, &source_root, &sig_root, &project_root, &layout);
        tagged.push((target, c));
    }
    {
        let info = &run_info;
        // A core-subclass shell rides on its file's written methods; a file
        // with none writes nothing (reference `declared_by_run`).
        let files_with_methods: HashSet<&str> =
            tagged.iter().filter(|(_, c)| writes(c)).map(|(_, c)| c.file.as_str()).collect();
        for (f, shell) in &info.shell_files {
            if info.core_shells.contains(shell) && !files_with_methods.contains(f.as_str()) {
                continue;
            }
            // Route a shell to its ENCLOSING class's target (a shell rides in the
            // same file as the class it is nested in — e.g. `Rigor::Triage::Selector`
            // routes with `Rigor::Triage`), so a consolidated layout keeps the
            // shell beside its class. A top-level shell (no `::`) routes by itself.
            let route_name = shell.rsplit_once("::").map(|(head, _)| head).unwrap_or(shell);
            let target =
                target_for(f, route_name, &source_root, &sig_root, &project_root, &layout);
            tagged_shells.push((target, shell.clone()));
        }
    }
    merged.absorb(run_info);

    // Group by target, preserving first-seen order (reference groups by target).
    // A shell-only target (a file with a `Data.define` but no methods) still gets
    // a file so the shell declaration exists.
    let mut targets: Vec<PathBuf> = Vec::new();
    for (t, _) in &tagged {
        if !targets.contains(t) {
            targets.push(t.clone());
        }
    }
    for (t, _) in &tagged_shells {
        if !targets.contains(t) {
            targets.push(t.clone());
        }
    }

    let sig_root_dir = project_root.join(&sig_root);
    let mut results: Vec<WriteResult> = Vec::new();
    for target in targets {
        let group: Vec<Candidate> =
            tagged.iter().filter(|(t, _)| *t == target).map(|(_, c)| clone_candidate(c)).collect();
        let shells: Vec<String> = tagged_shells
            .iter()
            .filter(|(t, _)| *t == target)
            .map(|(_, s)| s.clone())
            .collect();
        let source = group
            .first()
            .map(|c| c.file.clone())
            .or_else(|| {
                // A shell-only target has no candidate: recover the source file.
                tagged_shells.iter().find(|(t, _)| *t == target).map(|_| String::new())
            })
            .unwrap_or_default();
        let target_str = target.to_string_lossy().into_owned();

        if !target.starts_with(&sig_root_dir) {
            results.push(WriteResult {
                source,
                target: target_str,
                action: "skipped_outside_sig_root",
                applied: Vec::new(),
                skipped: Vec::new(),
                error: None,
            replaced: Vec::new(),
        repaired: Vec::new(),
            });
            continue;
        }
        if target.exists() {
            results.push(update_existing(
                source,
                &target,
                target_str,
                group,
                &shells,
                &merged.kinds,
                &merged.supers,
                &merged.params,
                overwrite,
                dry_run,
            ));
            continue;
        }
        let content = render_new_file(&group, &shells, &merged);
        // Reference `RbsValidity.source_error` on the create path
        // (writer.rb `create_new`): a render defect never reaches disk — the
        // whole target is refused (`:skipped_invalid_rbs`), same as a broken
        // splice on the update path. The check runs BEFORE the dry-run
        // branch: `--check` reports a refusal the same way `--write` would.
        if let Some(detail) = rbs_parse_error::parsing_error_message("(rigor sig-gen)", &content) {
            results.push(WriteResult {
                source,
                target: target_str,
                action: "skipped_invalid_rbs",
                applied: Vec::new(),
                skipped: Vec::new(),
                error: Some(detail),
                replaced: Vec::new(),
                repaired: Vec::new(),
            });
            continue;
        }
        if !dry_run {
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::write(&target, content).is_err() {
                eprintln!("sig-gen: failed to write {target_str}");
                return ExitCode::from(1);
            }
        }
        results.push(WriteResult {
            source,
            target: target_str,
            action: "created",
            applied: group,
            skipped: Vec::new(),
            error: None,
            replaced: Vec::new(),
            repaired: Vec::new(),
        });
    }

    // `out_of_date` — every result a `--write` would change or refuse; only
    // `noop` / `skipped_outside_sig_root` stay silent under `--check`.
    let stale: Vec<&WriteResult> = results
        .iter()
        .filter(|r| !matches!(r.action, "noop" | "skipped_outside_sig_root"))
        .collect();
    if dry_run {
        match format {
            "json" => render_check_json(&stale),
            _ => render_check_text(&stale),
        }
        if format == "text" && !stale.is_empty() {
            report_check_failure(&stale);
        }
        if format == "text" {
            report_skipped(&skips);
        }
        report_unresolvable_superclasses(&unresolvable);
        return if stale.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(1) };
    }
    match format {
        "json" => render_write_json(&results),
        _ => render_write_text(&results),
    }
    if format == "text" {
        report_skipped(&skips);
    }
    report_unresolvable_superclasses(&unresolvable);
    // v0.4.7 `TARGET_REFUSALS`: a refused target means the user asked for a
    // write and did not get one — exit 1.
    if results.iter().any(|r| {
        matches!(r.action, "skipped_invalid_rbs" | "skipped_invalid_encoding" | "skipped_unparseable")
    }) {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

/// A shallow copy of a candidate (its `kind` is `&'static str`).
fn clone_candidate(c: &Candidate) -> Candidate {
    Candidate {
        file: c.file.clone(),
        class_name: c.class_name.clone(),
        method_name: c.method_name.clone(),
        kind: c.kind,
        rbs: c.rbs.clone(),
        inferred_return: c.inferred_return.clone(),
        classification: c.classification,
        declared_return_rbs: c.declared_return_rbs.clone(),
        decl_header: c.decl_header.clone(),
        skip_reason: c.skip_reason,
        definer: c.definer,
    }
}

/// Map a candidate to its target `.rbs` (reference `PathMapper#target_for`):
/// consult the [`LayoutIndex`] by `class_name` FIRST (a class already declared in
/// a consolidated sig file routes there), and only on a miss fall back to the 1:1
/// mirror mapping (strip the source-root first component, swap the extension,
/// place under the sig root).
fn target_for(
    source: &str,
    class_name: &str,
    source_root: &str,
    sig_root: &str,
    project_root: &Path,
    layout: &LayoutIndex,
) -> PathBuf {
    if let Some(existing) = layout.file_for(class_name) {
        return existing.clone();
    }
    mirror_target(source, source_root, sig_root, project_root)
}

/// The 1:1 mirror `.rb` → `.rbs` mapping (the reference `PathMapper` fallback).
fn mirror_target(source: &str, source_root: &str, sig_root: &str, project_root: &Path) -> PathBuf {
    let sp = Path::new(source);
    let rel: PathBuf = if sp.is_absolute() {
        let canon = sp.canonicalize().unwrap_or_else(|_| sp.to_path_buf());
        canon.strip_prefix(project_root).map(Path::to_path_buf).unwrap_or(canon)
    } else {
        sp.to_path_buf()
    };
    // Strip the leading source-root component (`lib/` → ``) when present.
    let stripped: PathBuf = {
        let mut comps = rel.components();
        match comps.clone().next() {
            Some(first) if first.as_os_str() == std::ffi::OsStr::new(source_root) => {
                comps.next();
                comps.as_path().to_path_buf()
            }
            _ => rel.clone(),
        }
    };
    let mut target = project_root.join(sig_root).join(stripped);
    target.set_extension("rbs");
    target
}

/// The 2-space RBS indent (reference `Writer::INDENT`).
const INDENT: &str = "  ";

/// Render a NEW sig file's content (reference `render_new_file` /
/// `render_tree_nodes`): build a namespace tree from the candidates, APPEND the
/// `Data.define`/`Struct.new` shells (empty class nodes, after the method/nested
/// children so a class body renders methods → real nested classes → shells, the
/// reference order), then render each top-level node joined by a blank line.
fn render_new_file(candidates: &[Candidate], shells: &[String], info: &NamespaceInfo) -> String {
    let mut roots: Vec<TreeNode> = Vec::new();
    for c in candidates {
        let segs: Vec<&str> = c.class_name.split("::").collect();
        insert_into_tree(&mut roots, &segs, &c.rbs);
    }
    for shell in shells {
        let segs: Vec<&str> = shell.split("::").collect();
        insert_shell_into_tree(&mut roots, &segs);
    }
    roots
        .iter()
        .map(|n| render_tree_node(n, info, 0, &[]))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Ensure an EMPTY tree node exists at the shell's class-name path `segs`,
/// creating intermediate nodes as needed (reference `@class_shells` tree nodes).
/// A leaf that already exists (the constant is ALSO referenced elsewhere) is left
/// untouched — never duplicated.
fn insert_shell_into_tree(nodes: &mut Vec<TreeNode>, segs: &[&str]) {
    let Some((head, rest)) = segs.split_first() else { return };
    let idx = match nodes.iter().position(|n| n.name == *head) {
        Some(i) => i,
        None => {
            nodes.push(TreeNode { name: head.to_string(), children: Vec::new(), methods: Vec::new() });
            nodes.len() - 1
        }
    };
    if !rest.is_empty() {
        insert_shell_into_tree(&mut nodes[idx].children, rest);
    }
    // rest empty ⇒ the leaf node now exists (empty) — nothing more to add.
}

/// A namespace-tree node: a name segment, ordered children, and the method RBS
/// lines declared directly at this level.
struct TreeNode {
    name: String,
    children: Vec<TreeNode>,
    methods: Vec<String>,
}

/// Insert a method's RBS under the class-name path `segs` (creating intermediate
/// nodes), preserving first-seen order (reference `insert_into_tree`).
fn insert_into_tree(nodes: &mut Vec<TreeNode>, segs: &[&str], rbs: &str) {
    let Some((head, rest)) = segs.split_first() else { return };
    let idx = match nodes.iter().position(|n| n.name == *head) {
        Some(i) => i,
        None => {
            nodes.push(TreeNode { name: head.to_string(), children: Vec::new(), methods: Vec::new() });
            nodes.len() - 1
        }
    };
    if rest.is_empty() {
        nodes[idx].methods.push(rbs.to_string());
    } else {
        insert_into_tree(&mut nodes[idx].children, rest, rbs);
    }
}

/// Render one tree node (reference `render_tree_node`): `<indent><keyword> <name>
/// <super?>\n<body><indent>end\n`, body = method lines then child blocks.
fn render_tree_node(node: &TreeNode, info: &NamespaceInfo, depth: usize, prefix: &[String]) -> String {
    let indent = INDENT.repeat(depth);
    let mut qual = prefix.to_vec();
    qual.push(node.name.clone());
    let qualified = qual.join("::");
    let keyword = node_keyword(node, info, &qualified);
    let superclass = if keyword == "class" {
        info.supers
            .get(&qualified)
            .map(|s| format!(" < {}", superclasses::absolute(s)))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let inner = INDENT.repeat(depth + 1);
    let mut body = String::new();
    for m in &node.methods {
        body.push_str(&format!("{inner}{m}\n"));
    }
    for child in &node.children {
        body.push_str(&render_tree_node(child, info, depth + 1, &qual));
    }
    let params = info.params.get(&qualified).map_or("", String::as_str);
    format!("{indent}{keyword} {}{params}{superclass}\n{body}{indent}end\n", node.name)
}

/// The declaration keyword for a node (reference `node_keyword`): the recorded
/// kind, else `class` for a leaf-with-methods, else `module`.
fn node_keyword(node: &TreeNode, info: &NamespaceInfo, qualified: &str) -> &'static str {
    if let Some(k) = info.kinds.get(qualified) {
        return k;
    }
    if !node.methods.is_empty() && node.children.is_empty() {
        "class"
    } else {
        "module"
    }
}

/// `--write` text report (reference `render_write_text`): `No changes` when
/// EVERY result is `noop`, else one line per created / updated / outside-sig-root
/// target (a `noop` result prints nothing).
/// `render_write_skips`: one line per proposal the writer declined, so a change
/// `--diff` or `--print` showed is never dropped without a reason.
fn render_write_skips(r: &WriteResult) {
    for entry in &r.skipped {
        let c = &entry.candidate;
        let separator = if c.kind == "singleton" { "." } else { "#" };
        println!("  skipped {}{separator}{} (user-authored): {}", c.class_name, c.method_name, c.rbs);
    }
}

fn render_write_text(results: &[WriteResult]) {
    if results.iter().all(|r| r.action == "noop" && r.skipped.is_empty()) {
        println!("No changes");
        return;
    }
    for r in results {
        match r.action {
            "created" => println!("created {} ({} method(s))", r.target, r.applied.len()),
            "updated" => {
                // `applied_counts`: `+N` added lines, `replaced N` counted
                // apart (a rewrite is not an addition), then repairs.
                let mut counts = format!("+{}", r.applied.len() - r.replaced.len());
                if !r.replaced.is_empty() {
                    counts.push_str(&format!(", replaced {}", r.replaced.len()));
                }
                if !r.repaired.is_empty() {
                    counts.push_str(&format!(
                        ", repaired {} generic header(s): {}",
                        r.repaired.len(),
                        r.repaired.join(", ")
                    ));
                }
                println!("updated {} ({counts}, skipped {} user-authored)", r.target, r.skipped.len());
                render_write_skips(r);
            }
            // `render_write_unchanged` (#1975): a target every proposal for
            // which the project already declares.
            "noop" if !r.skipped.is_empty() => {
                println!("unchanged {} (skipped {} user-authored)", r.target, r.skipped.len());
                render_write_skips(r);
            }
            "skipped_outside_sig_root" => {
                println!("skipped {} -> {} (outside sig root)", r.source, r.target);
                render_write_skips(r);
            }
            "skipped_invalid_encoding" | "skipped_unparseable" | "skipped_invalid_rbs" => {
                render_write_refusal(r)
            }
            _ => {}
        }
    }
}

/// The `REFUSED` block for a target the writer declines — shared by
/// [`render_write_text`] and [`render_check_text`] (reference
/// `render_write_invalid*`).
fn render_write_refusal(r: &WriteResult) {
    match r.action {
        "skipped_invalid_encoding" => {
            println!(
                "REFUSED {} — the existing file is not valid UTF-8, so it was not updated",
                r.target
            );
            println!(
                "  Re-save the file as UTF-8 and re-run; sig-gen never modifies a file it cannot \
                 read faithfully."
            );
        }
        "skipped_unparseable" => {
            println!("REFUSED {} — the existing file was not updated", r.target);
            println!("  {}", r.error.as_deref().unwrap_or_default());
            println!("  Fix the syntax error and re-run; sig-gen never modifies a file it cannot parse.");
        }
        _ => {
            println!(
                "REFUSED {} — the generated RBS does not parse, so it was not written",
                r.target
            );
            println!("  {}", r.error.as_deref().unwrap_or_default());
            println!("  This is a bug in Rigor's RBS rendering, not in your code — please report it.");
        }
    }
}

/// `applied_counts` (reference `applied_counts`): `+N` added lines, with
/// `, replaced N` when a rewrite applied and the repaired generic headers.
fn applied_counts(r: &WriteResult) -> String {
    let mut counts = format!("+{}", r.applied.len() - r.replaced.len());
    if !r.replaced.is_empty() {
        counts.push_str(&format!(", replaced {}", r.replaced.len()));
    }
    if !r.repaired.is_empty() {
        counts.push_str(&format!(
            ", repaired {} generic header(s): {}",
            r.repaired.len(),
            r.repaired.join(", ")
        ));
    }
    counts
}

/// `--check` text report (reference `render_check_text` /
/// `render_check_change`): `sig/ is up to date`, else one `would create` /
/// `would update` line per stale target with the member lines it would add
/// (`  - ` before `  + ` for a `--overwrite` replacement).
fn render_check_text(stale: &[&WriteResult]) {
    if stale.is_empty() {
        println!("sig/ is up to date");
        return;
    }
    for r in stale {
        match r.action {
            "created" => {
                println!("would create {} ({} method(s))", r.target, r.applied.len());
                render_check_applied(r);
            }
            "updated" => {
                println!("would update {} ({})", r.target, applied_counts(r));
                render_check_applied(r);
            }
            _ => render_write_refusal(r),
        }
    }
}

/// The `  - declared_rbs` (a replaced member) / `  + rbs` lines of one check
/// entry (reference `render_check_change`'s per-candidate loop).
fn render_check_applied(r: &WriteResult) {
    for c in &r.applied {
        if let Some((_, old)) = r.replaced.iter().find(|(rc, _)| {
            rc.method_name == c.method_name && rc.kind == c.kind
        }) {
            println!("  - {old}");
        }
        for line in c.rbs.lines() {
            println!("  + {line}");
        }
    }
}

/// `--check --format json` (reference `render_check`): `{up_to_date, results,
/// refused}` — the write entries with `created`/`updated` renamed to
/// `would_*`. The port never produces `sig.skipped.inline-differs` rows (the
/// inline-declaration feature is unported), so `refused` is always `[]`.
fn render_check_json(stale: &[&WriteResult]) {
    use serde_json::json;
    let rows: Vec<_> = stale
        .iter()
        .map(|r| {
            let applied: Vec<_> = r.applied.iter().map(candidate_json).collect();
            let skipped: Vec<_> = r
                .skipped
                .iter()
                .map(|s| {
                    let c = &s.candidate;
                    let mut obj = json!({
                        "file": c.file, "class": c.class_name, "method": c.method_name,
                        "kind": c.kind, "classification": s.classification, "rbs": c.rbs,
                        "inferred_return": c.inferred_return, "write_skip_reason": "user_authored",
                    });
                    if let Some(dr) = &s.declared_return_rbs {
                        obj.as_object_mut()
                            .unwrap()
                            .insert("declared_return_rbs".to_string(), json!(dr));
                    }
                    obj
                })
                .collect();
            let action = match r.action {
                "created" => "would_create",
                "updated" => "would_update",
                a => a,
            };
            let mut row = json!({
                "source": r.source, "target": r.target, "action": action,
                "applied": applied, "skipped": skipped,
            });
            if let Some(e) = &r.error {
                row.as_object_mut().unwrap().insert("error".to_string(), json!(e));
            }
            if !r.replaced.is_empty() {
                let replaced: Vec<_> =
                    r.replaced.iter().map(|(c, _)| candidate_json(c)).collect();
                row.as_object_mut().unwrap().insert("replaced".to_string(), json!(replaced));
            }
            if !r.repaired.is_empty() {
                row.as_object_mut().unwrap().insert("repaired_headers".to_string(), json!(r.repaired));
            }
            row
        })
        .collect();
    println!(
        "{}",
        crate::json_document::stamp_pretty(
            &serde_json::to_string_pretty(&json!({
                "up_to_date": stale.is_empty(),
                "results": rows,
                "refused": [],
            }))
            .unwrap(),
            None
        )
    );
}

/// The `--check` stderr verdict (reference `report_check_failure` +
/// `refused_targets_line`): out-of-date targets counted apart from refused
/// ones, and a refused target is listed as a hand fix unless the refusal was
/// Rigor's own render.
fn report_check_failure(stale: &[&WriteResult]) {
    let (refusals, pending): (Vec<&&WriteResult>, Vec<&&WriteResult>) = stale.iter().partition(|r| {
        matches!(r.action, "skipped_invalid_rbs" | "skipped_invalid_encoding" | "skipped_unparseable")
    });
    if !pending.is_empty() {
        eprintln!(
            "rigor sig-gen --check: {} signature file(s) out of date; run `rigor sig-gen --write` with the same options to update them.",
            pending.len()
        );
    }
    if !refusals.is_empty() {
        let by_hand: Vec<&str> = refusals
            .iter()
            .filter(|r| r.action != "skipped_invalid_rbs")
            .map(|r| r.target.as_str())
            .collect();
        let head = format!(
            "rigor sig-gen --check: {} signature file(s) refused; `--write` does not update them",
            refusals.len()
        );
        if by_hand.is_empty() {
            eprintln!("{head}. The generated RBS did not parse: a Rigor bug, see its REFUSED line on stdout.");
        } else {
            eprintln!("{head}. Repair by hand: {}.", by_hand.join(", "));
        }
    }
}

/// `--write --format json` report (reference `render_write_json` / `to_h`).
/// Each applied candidate carries the reference's per-candidate key set; each
/// skipped entry is the candidate's fields (with an overridden `classification`
/// and optional `declared_return_rbs`) plus `write_skip_reason: "user_authored"`.
fn render_write_json(results: &[WriteResult]) {
    use serde_json::json;
    let rows: Vec<_> = results
        .iter()
        .map(|r| {
            let applied: Vec<_> = r.applied.iter().map(candidate_json).collect();
            let skipped: Vec<_> = r
                .skipped
                .iter()
                .map(|s| {
                    let c = &s.candidate;
                    let mut obj = json!({
                        "file": c.file, "class": c.class_name, "method": c.method_name,
                        "kind": c.kind, "classification": s.classification, "rbs": c.rbs,
                        "inferred_return": c.inferred_return, "write_skip_reason": "user_authored",
                    });
                    if let Some(dr) = &s.declared_return_rbs {
                        obj.as_object_mut()
                            .unwrap()
                            .insert("declared_return_rbs".to_string(), json!(dr));
                    }
                    obj
                })
                .collect();
            let mut row = json!({
                "source": r.source, "target": r.target, "action": r.action,
                "applied": applied, "skipped": skipped,
            });
            if let Some(e) = &r.error {
                row.as_object_mut().unwrap().insert("error".to_string(), json!(e));
            }
            // `to_h`: `replaced` is present only when a member was rewritten
            // (pre-#1076 payloads stay byte-identical without it).
            if !r.replaced.is_empty() {
                let replaced: Vec<_> =
                    r.replaced.iter().map(|(c, _)| candidate_json(c)).collect();
                row.as_object_mut().unwrap().insert("replaced".to_string(), json!(replaced));
            }
            if !r.repaired.is_empty() {
                row.as_object_mut().unwrap().insert("repaired_headers".to_string(), json!(r.repaired));
            }
            row
        })
        .collect();
    println!(
        "{}",
        crate::json_document::stamp_pretty(&serde_json::to_string_pretty(&json!({ "results": rows })).unwrap(), None)
    );
}

// ---------------------------------------------------------------------------
// LayoutIndex (reference `LayoutIndex`) — qualified-class-name → sig-file map
// ---------------------------------------------------------------------------

/// Pre-scans every configured signature directory's `.rbs` files to build a
/// `FQN → sig-file path` map so a candidate whose class is already declared in a
/// consolidated file routes there (reference `LayoutIndex`). First-found wins on
/// duplicate declarations; an unparseable file is skipped silently.
struct LayoutIndex {
    map: HashMap<String, PathBuf>,
}

impl LayoutIndex {
    /// Build from the configured signature dirs (each resolved under
    /// `project_root` when relative). A SORTED recursive `**/*.rbs` walk parses
    /// every file and records each class/module FQN → file (first-found-wins);
    /// any per-file read/parse failure drops just that file.
    fn build(signature_paths: &[String], project_root: &Path) -> Self {
        let mut map: HashMap<String, PathBuf> = HashMap::new();
        for sp in signature_paths {
            if sp.is_empty() {
                continue;
            }
            let dir = {
                let p = Path::new(sp);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    project_root.join(p)
                }
            };
            if !dir.is_dir() {
                continue;
            }
            let mut files: Vec<PathBuf> = Vec::new();
            collect_rbs_files(&dir, &mut files);
            files.sort();
            for f in files {
                let Ok(src) = std::fs::read_to_string(&f) else { continue };
                let Ok(sig) = ruby_rbs::node::parse(&src) else { continue };
                record_layout_decls(sig.declarations().iter(), &[], &f, &mut map);
            }
        }
        LayoutIndex { map }
    }

    /// The sig file already declaring `class_name`, or `None`.
    fn file_for(&self, class_name: &str) -> Option<&PathBuf> {
        self.map.get(class_name)
    }
}

/// Recursively collect `**/*.rbs` files under `dir` (unsorted; the caller sorts).
fn collect_rbs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rbs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rbs") {
            out.push(path);
        }
    }
}

/// Record each class/module declaration's FQN → `path` (first-found-wins),
/// recursing into nested decls (reference `record_decl`).
fn record_layout_decls<'a>(
    decls: impl Iterator<Item = RbsNode<'a>>,
    prefix: &[String],
    path: &Path,
    map: &mut HashMap<String, PathBuf>,
) {
    for decl in decls {
        let (local, members): (String, ruby_rbs::node::NodeList<'a>) = match decl {
            RbsNode::Class(c) => (decl_full_name(&c.name()), c.members()),
            RbsNode::Module(m) => (decl_full_name(&m.name()), m.members()),
            _ => continue,
        };
        let full = if prefix.is_empty() {
            local.clone()
        } else {
            format!("{}::{}", prefix.join("::"), local)
        };
        map.entry(full).or_insert_with(|| path.to_path_buf());
        let mut child_prefix = prefix.to_vec();
        child_prefix.push(local);
        record_layout_decls(members.iter(), &child_prefix, path, map);
    }
}

/// The full written name of a class/module decl: its namespace path segments
/// joined with `::` then the trailing name (reference `decl.name.to_s`, leading
/// `::` stripped). A compact `class Foo::Bar` yields `"Foo::Bar"`.
fn decl_full_name(tn: &ruby_rbs::node::TypeNameNode) -> String {
    let mut parts: Vec<String> = Vec::new();
    for seg in tn.namespace().path().iter() {
        if let RbsNode::Symbol(s) = seg {
            parts.push(s.as_str().to_string());
        }
    }
    parts.push(tn.name().as_str().to_string());
    parts.join("::")
}

// ---------------------------------------------------------------------------
// update_existing (reference `Writer#update_existing`) — merge into a target
// ---------------------------------------------------------------------------

/// A member of an existing class decl, collected for the partition + equivalence
/// check (reference `collect_member_pairs` + the return-text extraction).
struct MemberInfo {
    name: String,
    /// `"instance"` | `"singleton"` | `"singleton_instance"` (attrs → instance).
    kind: &'static str,
    /// The member's declared return text (after the last depth-0 `->` for a
    /// method; the declared type for an attr), or `None` when unextractable.
    return_text: Option<String>,
    /// The member declaration's byte range in the source `[start, end)` — the
    /// splice window for `--overwrite` replacement (reference `member.location`).
    /// `None` for an attr member (attrs are never replacement targets here — the
    /// generator emits attr candidates as `initialize`/method rows, and the
    /// reference's `find_method_member` only matches `MethodDefinition`s).
    span: Option<(usize, usize)>,
    /// The member declaration's raw source text — consumed by `count_untyped` in
    /// the `--overwrite` `NEW_METHOD` tightening test (reference `tightens_untyped?`).
    text: String,
}

/// A candidate whose `(name, kind)` collides with an existing member, paired
/// with that member's replacement metadata (reference `conflicting` tuple).
struct Conflict {
    /// The existing member's declared return text (equivalence check / skip tag).
    return_text: Option<String>,
    /// The existing member declaration's byte span, for `--overwrite` replacement.
    span: Option<(usize, usize)>,
    /// The existing member declaration's raw text, for `count_untyped`.
    text: String,
}

/// Owned snapshot of a found class decl: the byte offset of its closing `end`
/// token's start, plus its member pairs. Extracted BEFORE any mutation so the
/// borrow of the parsed tree ends before the source string is spliced.
struct ClassDeclInfo {
    end_start: usize,
    members: Vec<MemberInfo>,
}

/// Merge a per-target candidate group into an EXISTING `.rbs` (reference
/// `Writer#update_existing`). Parse the target for splice locations; a parse
/// failure yields `noop` with the file untouched. Per class group (first-seen
/// order) the class is found (→ merge) or not (→ append), re-parsing fresh from
/// the current source before each so byte offsets are never stale. The file is
/// written only when at least one method was applied (`updated`).
#[allow(clippy::too_many_arguments)]
fn update_existing(
    source_path: String,
    target: &Path,
    target_str: String,
    candidates: Vec<Candidate>,
    shells: &[String],
    kinds: &HashMap<String, &'static str>,
    supers: &HashMap<String, String>,
    params: &HashMap<String, String>,
    overwrite: bool,
    dry_run: bool,
) -> WriteResult {
    let Ok(bytes) = std::fs::read(target) else {
        return WriteResult {
            source: source_path,
            target: target_str,
            action: "noop",
            applied: Vec::new(),
            skipped: Vec::new(),
            error: None,
            replaced: Vec::new(),
        repaired: Vec::new(),
        };
    };
    // v0.4.7 (#2480/#2587): an existing target that is not UTF-8, or does not
    // parse as RBS, is REFUSED (left untouched, the run exits 1), not a
    // silent `noop`.
    let refused = |action: &'static str, error: String| WriteResult {
        source: source_path.clone(),
        target: target_str.clone(),
        action,
        applied: Vec::new(),
        skipped: Vec::new(),
        error: Some(error),
            replaced: Vec::new(),
        repaired: Vec::new(),
    };
    let Ok(source) = String::from_utf8(bytes) else {
        return refused("skipped_invalid_encoding", format!("{target_str} is not valid UTF-8"));
    };
    if let Some(detail) = rbs_parse_error::parsing_error_message(&target_str, &source) {
        return refused(
            "skipped_unparseable",
            format!("{target_str} does not parse as RBS: {detail}"),
        );
    }

    // Shell injection on the MERGE path is deferred: when a file already exists,
    // any shell it needs was written on the CREATE run (`render_new_file`), so a
    // `--write` re-run stays idempotent (the shell is present, no conflict). The
    // only uncovered case is merging into a USER-authored sig that lacks a shell
    // for a `Data.define` return — a documented follow-up, not a regression.
    let MergeOutcome { source: merged, action, applied, replaced, skipped, repaired } =
        apply_merge_with(source, candidates, kinds, supers, params, shells, overwrite);
    // Reference `RbsValidity.source_error`: a spliced result that does not
    // re-parse is a writer defect — refuse to write it (`:skipped_invalid_rbs`)
    // rather than leave the consumer's `rigor check` to quarantine the file.
    if action == "updated" {
        if let Some(detail) = rbs_parse_error::parsing_error_message("(rigor sig-gen)", &merged) {
            return refused("skipped_invalid_rbs", detail);
        }
        if !dry_run {
            let _ = std::fs::write(target, &merged);
        }
    }
    WriteResult {
        source: source_path,
        target: target_str,
        action,
        applied,
        replaced,
        skipped,
        error: None,
        repaired,
    }
}

/// The pure result of merging a candidate group into an existing `.rbs` source
/// string — the file-I/O-free core of [`update_existing`], shared with tests.
struct MergeOutcome {
    source: String,
    action: &'static str,
    applied: Vec<Candidate>,
    /// The applied candidates that REPLACED an existing member line
    /// (`--overwrite`), paired with the replaced line — the
    /// `applied_counts` `, replaced N` tail and `--check`'s `  - ` diff
    /// lines (reference `WriteResult#replaced` / `declared_rbs`).
    replaced: Vec<(Candidate, String)>,
    skipped: Vec<SkipEntry>,
    /// `repaired_headers`: one `Name[params]` per bare generic header given
    /// its parameters (#2580/#2602).
    repaired: Vec<String>,
}

/// Merge a candidate group into `source` (reference `Writer#update_existing`
/// minus disk I/O). A parse-failure gate leaves the source byte-untouched with
/// `action: "noop"`; otherwise each class group (class-name order) is merged or
/// appended, re-parsing fresh before each so offsets are never stale.
#[cfg(test)]
fn apply_merge(
    source: String,
    candidates: Vec<Candidate>,
    supers: &HashMap<String, String>,
    overwrite: bool,
) -> MergeOutcome {
    apply_merge_with(source, candidates, &HashMap::new(), supers, &HashMap::new(), &[], overwrite)
}

/// [`apply_merge`] with the run's generic header spellings (`params`): new
/// headers carry them, and — last — every bare header in the file for a class
/// in `params` is repaired (reference `repair_generic_headers`).
fn apply_merge_with(
    mut source: String,
    candidates: Vec<Candidate>,
    kinds: &HashMap<String, &'static str>,
    supers: &HashMap<String, String>,
    params: &HashMap<String, String>,
    shells: &[String],
    overwrite: bool,
) -> MergeOutcome {
    // Parse-failure gate: a malformed target is left byte-untouched (reference
    // `parse_signature` → nil → `:noop`).
    if ruby_rbs::node::parse(&source).is_err() {
        return MergeOutcome {
            source,
            action: "noop",
            applied: Vec::new(),
            replaced: Vec::new(),
            skipped: Vec::new(),
            repaired: Vec::new(),
        };
    }
    let mut applied: Vec<Candidate> = Vec::new();
    let mut replaced: Vec<(Candidate, String)> = Vec::new();
    let mut skipped: Vec<SkipEntry> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    for (class_name, group) in group_by_class(candidates) {
        touched.push(class_name.clone());
        merge_class(&mut source, &class_name, group, kinds, supers, params, overwrite, &mut applied, &mut replaced, &mut skipped);
    }
    // `collapse_nested_declarations`: touched `Foo::Bar` decls move under an
    // existing `Foo` (rigor-rs#530). Runs after every merge so the parents
    // are in place; `repair_generic_headers` still goes last.
    touched.extend(shells.iter().cloned());
    collapse_nested_declarations(&mut source, &touched);
    let repaired = repair_generic_headers(&mut source, params);
    // `state_changed?`: a repaired header is a change too.
    let action = if applied.is_empty() && repaired.is_empty() { "noop" } else { "updated" };
    MergeOutcome { source, action, applied, replaced, skipped, repaired }
}

/// Reference `Writer#repair_generic_headers` (#2580/#2602): a declaration in
/// this file for a class the run stamps generic, opened with no type
/// parameters, is a guaranteed `GenericParameterMismatchError`; it gets the
/// stamp's spelling. A declaration whose body names one of the parameters
/// (as a word not preceded by `::` or `:`) is left bare — under the
/// parameters that name would bind to the parameter. Returns one
/// `Name[params]` entry per declaration repaired.
fn repair_generic_headers(source: &mut String, params: &HashMap<String, String>) -> Vec<String> {
    let mut names: Vec<&String> = params.keys().collect();
    names.sort();
    let mut repaired: Vec<String> = Vec::new();
    for name in names {
        let spelling = &params[name];
        let param_names: Vec<String> = spelling
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .filter_map(|p| p.split_whitespace().last().map(str::to_string))
            .collect();
        // (name end, body) of every bare opening, from a fresh parse.
        let bare: Vec<(usize, usize)> = match ruby_rbs::node::parse(source) {
            Ok(sig) => {
                let mut out = Vec::new();
                bare_openings(source, sig.declarations().iter(), &[], name, &mut out);
                out
            }
            Err(_) => continue,
        };
        if bare.is_empty() {
            continue;
        }
        let shadowed = bare.iter().any(|&(start, end)| {
            let body = &source[start..end];
            param_names.iter().any(|p| names_word(body, p))
        });
        if shadowed {
            continue;
        }
        let mut positions: Vec<usize> = bare.iter().map(|&(start, _)| start).collect();
        positions.sort_unstable();
        for &pos in positions.iter().rev() {
            source.insert_str(pos, spelling);
        }
        for _ in &bare {
            repaired.push(format!("{name}{spelling}"));
        }
    }
    repaired
}

/// Every declaration of `target` in `decls` with no type parameters, as
/// `(name end offset, declaration end offset)`.
fn bare_openings<'a>(
    source: &str,
    decls: impl Iterator<Item = RbsNode<'a>>,
    prefix: &[String],
    target: &str,
    out: &mut Vec<(usize, usize)>,
) {
    for decl in decls {
        let (local, members, bare, name_end, decl_end) = match &decl {
            RbsNode::Class(c) => (
                decl_full_name(&c.name()),
                c.members(),
                c.type_params().iter().next().is_none(),
                c.name_location().end(),
                c.location().end(),
            ),
            RbsNode::Module(m) => (
                decl_full_name(&m.name()),
                m.members(),
                m.type_params().iter().next().is_none(),
                m.name_location().end(),
                m.location().end(),
            ),
            _ => continue,
        };
        let full = if prefix.is_empty() { local.clone() } else { format!("{}::{}", prefix.join("::"), local) };
        if full == target && bare {
            let start = clamp_offset(name_end, source.len());
            out.push((start, clamp_offset(decl_end, source.len()).max(start)));
        }
        let mut child = prefix.to_vec();
        child.push(local);
        bare_openings(source, members.iter(), &child, target, out);
    }
}

/// `/(?<![\w:])NAME(?!\w)/` over `text`.
fn names_word(text: &str, name: &str) -> bool {
    let bytes = text.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut from = 0;
    while let Some(i) = text[from..].find(name) {
        let at = from + i;
        let end = at + name.len();
        let before_ok = at == 0 || !(is_word(bytes[at - 1]) || bytes[at - 1] == b':');
        let after_ok = end >= bytes.len() || !is_word(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = at + 1;
    }
    false
}

/// Group candidates by `class_name` (merged in class-name order, reference
/// `groups.keys.sort`).
/// (reference `candidates.group_by(&:class_name)`).
fn group_by_class(candidates: Vec<Candidate>) -> Vec<(String, Vec<Candidate>)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<Candidate>> = HashMap::new();
    for c in candidates {
        if !groups.contains_key(&c.class_name) {
            order.push(c.class_name.clone());
        }
        groups.entry(c.class_name.clone()).or_default().push(c);
    }
    // The reference merges `groups.keys.sort` — class-name order, not first-seen
    // order — which is what orders the `skipped` report lines too (rigor-rs#493
    // item 5).
    order.sort();
    order.into_iter().map(|k| (k.clone(), groups.remove(&k).unwrap())).collect()
}

/// Merge one class group: find the class by FQN (re-parsing fresh so offsets are
/// current), then merge into it or append a new class block (reference
/// `merge_class`).
#[allow(clippy::too_many_arguments)]
fn merge_class(
    source: &mut String,
    class_name: &str,
    candidates: Vec<Candidate>,
    kinds: &HashMap<String, &'static str>,
    supers: &HashMap<String, String>,
    params: &HashMap<String, String>,
    overwrite: bool,
    applied: &mut Vec<Candidate>,
    replaced: &mut Vec<(Candidate, String)>,
    skipped: &mut Vec<SkipEntry>,
) {
    // Extract owned decl info before mutating (the parsed tree borrows `source`).
    let found: Option<ClassDeclInfo> = match ruby_rbs::node::parse(source) {
        Ok(sig) => find_and_extract(source, sig.declarations().iter(), &[], class_name),
        Err(_) => None,
    };
    match found {
        Some(info) => merge_into_existing(source, &info, candidates, overwrite, applied, replaced, skipped),
        None => insert_namespace_chain(source, class_name, candidates, kinds, supers, params, applied),
    }
}

/// Recursively find the decl whose FQN matches `target` and extract its owned
/// [`ClassDeclInfo`] (reference `find_class_decl_in`).
fn find_and_extract<'a>(
    source: &str,
    decls: impl Iterator<Item = RbsNode<'a>>,
    prefix: &[String],
    target: &str,
) -> Option<ClassDeclInfo> {
    for decl in decls {
        let (local, members, end_loc): (String, ruby_rbs::node::NodeList<'a>, RBSLocationRange) =
            match &decl {
                RbsNode::Class(c) => (decl_full_name(&c.name()), c.members(), c.end_location()),
                RbsNode::Module(m) => (decl_full_name(&m.name()), m.members(), m.end_location()),
                _ => continue,
            };
        let full = if prefix.is_empty() {
            local.clone()
        } else {
            format!("{}::{}", prefix.join("::"), local)
        };
        if full == target {
            return Some(ClassDeclInfo {
                end_start: clamp_offset(end_loc.start(), source.len()),
                members: collect_member_pairs(source, members.iter()),
            });
        }
        let mut child_prefix = prefix.to_vec();
        child_prefix.push(local);
        if let Some(found) = find_and_extract(source, members.iter(), &child_prefix, target) {
            return Some(found);
        }
    }
    None
}

/// Collect `(name, kind, return_text)` for every method-like member of a class
/// (reference `collect_member_pairs` / `collect_pairs_for_member`). `alias` does
/// NOT count; `attr_writer` contributes `name=`; `attr_accessor` contributes
/// both `name` and `name=`.
fn collect_member_pairs<'a>(
    source: &str,
    members: impl Iterator<Item = RbsNode<'a>>,
) -> Vec<MemberInfo> {
    let mut out: Vec<MemberInfo> = Vec::new();
    for member in members {
        match member {
            RbsNode::MethodDefinition(md) => {
                let kind = match md.kind() {
                    MethodDefinitionKind::Instance => "instance",
                    MethodDefinitionKind::Singleton => "singleton",
                    MethodDefinitionKind::SingletonInstance => "singleton_instance",
                };
                let loc = md.location();
                let start = clamp_offset(loc.start(), source.len());
                let end = clamp_offset(loc.end(), source.len());
                let text = slice_of(source, loc);
                let return_text = extract_method_return_text(text);
                out.push(MemberInfo {
                    name: md.name().as_str().to_string(),
                    kind,
                    return_text,
                    span: Some((start, end)),
                    text: text.to_string(),
                });
            }
            RbsNode::AttrReader(a) => {
                let rt = attr_type_text(source, &a.type_());
                out.push(MemberInfo {
                    name: a.name().as_str().to_string(),
                    kind: "instance",
                    return_text: rt,
                    span: None,
                    text: String::new(),
                });
            }
            RbsNode::AttrWriter(a) => {
                let rt = attr_type_text(source, &a.type_());
                out.push(MemberInfo {
                    name: format!("{}=", a.name().as_str()),
                    kind: "instance",
                    return_text: rt,
                    span: None,
                    text: String::new(),
                });
            }
            RbsNode::AttrAccessor(a) => {
                let rt = attr_type_text(source, &a.type_());
                let name = a.name().as_str().to_string();
                out.push(MemberInfo {
                    name: name.clone(),
                    kind: "instance",
                    return_text: rt.clone(),
                    span: None,
                    text: String::new(),
                });
                out.push(MemberInfo {
                    name: format!("{name}="),
                    kind: "instance",
                    return_text: rt,
                    span: None,
                    text: String::new(),
                });
            }
            _ => {}
        }
    }
    out
}

/// The declared type text of an attr member — its `type` node's source slice
/// (reference "the type text after `:` via location"), trimmed.
fn attr_type_text(source: &str, type_node: &RbsNode) -> Option<String> {
    let s = slice_of(source, type_node.location()).trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Merge a candidate group into a found existing class (reference
/// `merge_into_existing_class`): partition into NEW (spliced) vs CONFLICTING.
/// Under `--overwrite`, eligible conflicts (a `tighter_return`, or a
/// `new_method` that strictly removes an `untyped` slot) have their existing
/// declaration REPLACED in place and move to `applied`; every other conflict is
/// equivalence-checked — dropped when the return matches, else skipped.
fn merge_into_existing(
    source: &mut String,
    info: &ClassDeclInfo,
    candidates: Vec<Candidate>,
    overwrite: bool,
    applied: &mut Vec<Candidate>,
    replaced: &mut Vec<(Candidate, String)>,
    skipped: &mut Vec<SkipEntry>,
) {
    let mut new_methods: Vec<Candidate> = Vec::new();
    let mut conflicting: Vec<(Candidate, Conflict)> = Vec::new();
    for c in candidates {
        match info.members.iter().find(|m| m.name == c.method_name && m.kind == c.kind) {
            None => new_methods.push(c),
            Some(m) => conflicting.push((
                c,
                Conflict { return_text: m.return_text.clone(), span: m.span, text: m.text.clone() },
            )),
        }
    }

    // Splice NEW members before the class's closing `end` — reference
    // `insert_into_class` at `member_indent_depth(decl)` (one level below the
    // `end` token's own column), inserted at the `end` LINE's start when it
    // carries only whitespace (`insert_before_end`): a nested decl's `end`
    // sits at col >0, and splicing at the token would eat its indent and
    // leave `end` at column 0. This runs BEFORE any replacement: it inserts
    // at `end_start`, after every existing member, so the captured member
    // spans (all below `end_start`) stay valid (reference order:
    // `insert_into_class` then `replace_eligible_conflicts`).
    if !new_methods.is_empty() {
        let at = info.end_start.min(source.len());
        let depth = (at - line_start_index(source, at)) / INDENT.len() + 1;
        let member_indent = INDENT.repeat(depth);
        let addition: String =
            new_methods.iter().map(|c| format!("{member_indent}{}\n", c.rbs)).collect();
        let line_start = line_start_index(source, at);
        if blank_range(source, line_start, at) {
            source.insert_str(line_start, &addition);
        } else {
            source.insert_str(at, &format!("\n{addition}"));
        }
        applied.extend(new_methods);
    }

    if overwrite {
        // Split conflicts into REPLACEABLE (reference `eligible_for_replacement?`)
        // and the rest. Eligible = a `tighter_return` (the classifier already
        // proved a strict subtype), OR a `new_method` whose new RBS has strictly
        // fewer `untyped` tokens than the existing declaration (reference
        // `tightens_untyped?` — the `--params=observed` initialize-tightening
        // case). An attr member has no method span and is never replaced.
        let mut eligible: Vec<(Candidate, (usize, usize))> = Vec::new();
        let mut rest: Vec<(Candidate, Option<String>)> = Vec::new();
        for (c, m) in conflicting {
            let ok = match m.span {
                Some(sp) if c.classification == "tighter_return" => Some(sp),
                Some(sp)
                    if c.classification == "new_method"
                        && count_untyped(&c.rbs) < count_untyped(&m.text) =>
                {
                    Some(sp)
                }
                _ => None,
            };
            match ok {
                Some(sp) => eligible.push((c, sp)),
                None => rest.push((c, m.return_text)),
            }
        }
        // Apply replacements from the HIGHEST byte offset downward so each splice
        // leaves earlier offsets valid (reference sorts by `-member_position`).
        eligible.sort_by_key(|(_, (start, _))| std::cmp::Reverse(*start));
        for (c, (start, end)) in eligible {
            replaced.push((clone_candidate(&c), source[start..end].to_string()));
            source.replace_range(start..end, &c.rbs);
            applied.push(c);
        }
        for (c, _) in rest {
            skip_conflict(c, skipped);
        }
        return;
    }

    // No `--overwrite`: every conflict is preserved as user-authored.
    for (c, _) in conflicting {
        skip_conflict(c, skipped);
    }
}

/// Record one preserved (user-authored) conflict as a [`SkipEntry`] (reference
/// `merge_into_existing_class`'s `skipped` accumulator): every conflicting
/// candidate, with the classification and declared return generation gave it.
/// The reference does not compare the member's text here; an equal return was
/// already dropped as equivalent by the generator, and one the generator could
/// not compare (a class whose definition does not build, #482) is reported as
/// a skip.
fn skip_conflict(c: Candidate, skipped: &mut Vec<SkipEntry>) {
    let classification = c.classification;
    let declared_return_rbs = c.declared_return_rbs.clone();
    skipped.push(SkipEntry { candidate: c, classification, declared_return_rbs });
}

/// Count bare `untyped` type tokens in an RBS fragment (reference
/// `count_untyped` — word-boundary matched so it is not counted inside an
/// identifier). Used by the `--overwrite` `NEW_METHOD` tightening test.
fn count_untyped(rbs: &str) -> usize {
    let bytes = rbs.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while let Some(pos) = rbs[i..].find("untyped") {
        let start = i + pos;
        let end = start + "untyped".len();
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            count += 1;
        }
        i = end;
    }
    count
}

/// Whether a byte is part of a Ruby/RBS identifier word (`\w`: alphanumeric or
/// underscore) — the word-boundary test for [`count_untyped`].
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn line_start_index(source: &str, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    source[..pos].rfind('\n').map_or(0, |i| i + 1)
}

/// Reference `blank_range?`: only spaces/tabs between `from` and `to`.
fn blank_range(source: &str, from: usize, to: usize) -> bool {
    source[from..to].chars().all(|c| c == ' ' || c == '\t')
}

/// Reference `insert_namespace_chain` + `render_chain_snippet`: the part of
/// `class_name`'s chain the file does not yet declare is rendered as a NESTED
/// block inside the deepest declared ancestor — `class Sub2 < ::A::Base`
/// lands inside the existing `module A`, never as a flat top-level
/// `class A::Sub2` (rigor-rs#493 item 11). With no ancestor the whole chain
/// appends top-level, still nested.
#[allow(clippy::too_many_arguments)]
fn insert_namespace_chain(
    source: &mut String,
    class_name: &str,
    candidates: Vec<Candidate>,
    kinds: &HashMap<String, &'static str>,
    supers: &HashMap<String, String>,
    params: &HashMap<String, String>,
    applied: &mut Vec<Candidate>,
) {
    let segments: Vec<&str> = class_name.split("::").collect();
    // `split_at_existing_ancestor`: the longest leading run the file declares.
    let mut split = 0usize;
    let mut anchor: Option<ClassDeclInfo> = None;
    for i in (1..segments.len()).rev() {
        let prefix = segments[..i].join("::");
        let Ok(sig) = ruby_rbs::node::parse(source) else { break };
        if let Some(info) = find_and_extract(source, sig.declarations().iter(), &[], &prefix) {
            split = i;
            anchor = Some(info);
            break;
        }
    }
    let missing = &segments[split.min(segments.len() - 1)..];
    if missing.is_empty() {
        return;
    }
    // `member_indent_depth`: a member sits one level below the decl's keyword
    // column — the `end` token's column stands in (canonical RBS keeps them
    // at the same indent).
    let depth = anchor.as_ref().map_or(0, |a| {
        (a.end_start - line_start_index(source, a.end_start)) / INDENT.len() + 1
    });
    let mut snippet = String::new();
    for (j, seg) in missing.iter().enumerate() {
        let qualified = [segments[..split].join("::"), missing[..=j].join("::")]
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("::");
        let is_leaf = j == missing.len() - 1;
        // `node_keyword`: kinds first, `class` for a method-bearing leaf,
        // `module` for an intermediate segment.
        let keyword = kinds
            .get(&qualified)
            .copied()
            .unwrap_or(if is_leaf { "class" } else { "module" });
        snippet.push_str(&format!("{}{} {}", INDENT.repeat(depth + j), keyword, seg));
        if let Some(p) = params.get(&qualified) {
            snippet.push_str(p);
        }
        if keyword == "class" {
            if let Some(spr) = supers.get(&qualified) {
                snippet.push_str(&format!(" < {}", superclasses::absolute(spr)));
            }
        }
        snippet.push('\n');
    }
    let member_indent = INDENT.repeat(depth + missing.len());
    for c in &candidates {
        snippet.push_str(&format!("{member_indent}{}\n", c.rbs));
    }
    for j in (0..missing.len()).rev() {
        snippet.push_str(&format!("{}end\n", INDENT.repeat(depth + j)));
    }
    match anchor {
        Some(a) => {
            // `insert_before_end`: an `end` starting its own line takes the
            // snippet at the line's start; otherwise before the token.
            let line_start = line_start_index(source, a.end_start);
            if blank_range(source, line_start, a.end_start) {
                source.insert_str(line_start, &snippet);
            } else {
                source.insert_str(a.end_start, &format!("\n{snippet}"));
            }
        }
        None => {
            // `append_top_level`: separated by exactly one blank line.
            if !source.is_empty() {
                if !source.ends_with('\n') {
                    source.push('\n');
                }
                if !source.ends_with("\n\n") {
                    source.push('\n');
                }
            }
            source.push_str(&snippet);
        }
    }
    applied.extend(candidates);
}

/// Owned snapshot of a class/module declaration for the relocation pass
/// (reference `collapse_nested_declarations` reads `RBS` locations).
struct RelocDecl {
    /// `decl.location` start/end byte offsets.
    start: usize,
    end: usize,
    /// `decl.location[:name]` byte offsets — the head rename's splice region.
    name_start: usize,
    name_end: usize,
    /// `decl.location[:end]` start byte offset (the `insert_before_end` anchor).
    end_start: usize,
    /// `decl.location[:keyword].start_column` — drives both indent depths.
    keyword_col: usize,
    /// `leading_positions`: annotation + comment start offsets.
    leading: Vec<usize>,
    /// `decl.name.to_s.start_with?("::")` — a rooted decl keeps its layout.
    rooted: bool,
}

/// [`find_and_extract`] returning the fuller [`RelocDecl`] the relocation
/// pass needs (reference `find_class_decl` used from `relocate_under_parent`).
fn find_reloc_decl<'a>(
    source: &str,
    decls: impl Iterator<Item = RbsNode<'a>>,
    prefix: &[String],
    target: &str,
) -> Option<RelocDecl> {
    for decl in decls {
        let (local, members, info) = match &decl {
            RbsNode::Class(c) => (
                decl_full_name(&c.name()),
                c.members(),
                (
                    c.location(),
                    c.name_location(),
                    c.end_location(),
                    c.keyword_location(),
                    c.annotations(),
                    c.comment().map(|cm| cm.location()),
                ),
            ),
            RbsNode::Module(m) => (
                decl_full_name(&m.name()),
                m.members(),
                (
                    m.location(),
                    m.name_location(),
                    m.end_location(),
                    m.keyword_location(),
                    m.annotations(),
                    m.comment().map(|cm| cm.location()),
                ),
            ),
            _ => continue,
        };
        let (loc, name_loc, end_loc, kw_loc, annotations, comment_loc) = info;
        let full = if prefix.is_empty() {
            local.clone()
        } else {
            format!("{}::{}", prefix.join("::"), local)
        };
        if full == target {
            let mut leading: Vec<usize> = annotations
                .iter()
                .filter_map(|a| match a {
                    RbsNode::Annotation(an) => {
                        Some(clamp_offset(an.location().start(), source.len()))
                    }
                    _ => None,
                })
                .collect();
            if let Some(cl) = comment_loc {
                leading.push(clamp_offset(cl.start(), source.len()));
            }
            return Some(RelocDecl {
                start: clamp_offset(loc.start(), source.len()),
                end: clamp_offset(loc.end(), source.len()),
                name_start: clamp_offset(name_loc.start(), source.len()),
                name_end: clamp_offset(name_loc.end(), source.len()),
                end_start: clamp_offset(end_loc.start(), source.len()),
                keyword_col: clamp_offset(kw_loc.start(), source.len())
                    - line_start_index(source, clamp_offset(kw_loc.start(), source.len())),
                leading,
                rooted: slice_of(source, name_loc).starts_with("::"),
            });
        }
        let mut child_prefix = prefix.to_vec();
        child_prefix.push(local);
        if let Some(found) = find_reloc_decl(source, members.iter(), &child_prefix, target) {
            return Some(found);
        }
    }
    None
}

/// Reference `collapse_nested_declarations`: relocate each touched `A::B`
/// declaration under an existing `A` in the same file, shallowest first
/// (rigor-rs#530).
fn collapse_nested_declarations(source: &mut String, names: &[String]) {
    let mut uniq: Vec<&String> = names.iter().collect();
    uniq.sort();
    uniq.dedup();
    for name in uniq {
        relocate_under_parent(source, name);
    }
}

/// Reference `relocate_under_parent`: move `qualified`'s own declaration
/// under its existing parent decl, or leave the file untouched when any
/// precondition fails (no parent, overlap, `::`-rooted head, non-isolated
/// region, unparseable intermediate).
fn relocate_under_parent(source: &mut String, qualified: &str) {
    let segments: Vec<&str> = qualified.split("::").collect();
    if segments.len() < 2 {
        return;
    }
    let parent_name = segments[..segments.len() - 1].join("::");
    let (parent, decl) = match ruby_rbs::node::parse(source) {
        Ok(sig) => match (
            find_reloc_decl(source, sig.declarations().iter(), &[], &parent_name),
            find_reloc_decl(source, sig.declarations().iter(), &[], qualified),
        ) {
            (Some(p), Some(d)) => (p, d),
            _ => return,
        },
        Err(_) => return,
    };
    // `overlapping?`: either decl already nests the other — nothing to move.
    if parent.start < decl.end && decl.start < parent.end {
        return;
    }
    if decl.rooted {
        return;
    }
    let Some(region) = decl_region(source, &decl) else { return };
    let block = relocated_block(source, &decl, &parent, segments[segments.len() - 1], &region);
    // `apply_relocation`: cut, re-parse for the (shifted) parent anchor,
    // splice in before its `end`.
    let spliced = splice_out(source, &region);
    let anchor = match ruby_rbs::node::parse(&spliced) {
        Ok(sig) => find_reloc_decl(&spliced, sig.declarations().iter(), &[], &parent_name),
        Err(_) => None,
    };
    let Some(anchor) = anchor else { return };
    let line_start = line_start_index(&spliced, anchor.end_start);
    let mut merged = spliced;
    if blank_range(&merged, line_start, anchor.end_start) {
        merged.insert_str(line_start, &block);
    } else {
        merged.insert_str(anchor.end_start, &format!("\n{block}"));
    }
    *source = merged;
}

/// Reference `decl_region`: the declaration's full source region extended
/// over its leading comment/annotations, ending just past its closing
/// newline. `None` when either edge shares its line with other text.
fn decl_region(source: &str, decl: &RelocDecl) -> Option<(usize, usize)> {
    let start_pos = decl
        .leading
        .iter()
        .copied()
        .chain(std::iter::once(decl.start))
        .min()?;
    let line_start = line_start_index(source, start_pos);
    if !blank_range(source, line_start, start_pos) {
        return None;
    }
    let finish = decl.end;
    let line_end = source[finish..].find('\n').map_or(source.len(), |i| finish + i);
    if !blank_range(source, finish, line_end) {
        return None;
    }
    Some((line_start, if line_end < source.len() { line_end + 1 } else { line_end }))
}

/// Reference `relocated_block` + `reindent`: the moved text with the
/// `Foo::Bar` head shortened to `Bar` and every line pushed in to the
/// parent's member depth (a non-positive delta leaves the text alone).
fn relocated_block(
    source: &str,
    decl: &RelocDecl,
    parent: &RelocDecl,
    short_name: &str,
    region: &(usize, usize),
) -> String {
    let text = &source[region.0..region.1];
    let head = decl.name_start - region.0;
    let tail = decl.name_end - region.0;
    let renamed = format!("{}{}{}", &text[..head], short_name, &text[tail..]);
    let member_depth = parent.keyword_col / INDENT.len() + 1;
    let decl_depth = decl.keyword_col / INDENT.len();
    let Some(delta) = member_depth.checked_sub(decl_depth) else {
        return renamed;
    };
    let prefix = INDENT.repeat(delta);
    let mut out = String::with_capacity(renamed.len() + prefix.len() * renamed.lines().count());
    for line in renamed.split_inclusive('\n') {
        if line.trim().is_empty() {
            out.push_str(line);
        } else {
            out.push_str(&prefix);
            out.push_str(line);
        }
    }
    if !renamed.ends_with('\n') && renamed.rsplit('\n').next().is_some_and(|l| !l.trim().is_empty()) {
        // `split_inclusive` already emitted the trailing unterminated line.
    }
    out
}

/// Reference `splice_out`: remove `region`, collapsing the newline run left
/// at the seam so the cut never shows as a widening gap (or a trailing blank
/// line at EOF). Spacing already fine is left exactly as written.
fn splice_out(source: &str, region: &(usize, usize)) -> String {
    let before = &source[..region.0];
    let after = &source[region.1..];
    if before.trim().is_empty() {
        return after.trim_start_matches('\n').to_string();
    }
    if after.trim().is_empty() {
        let trimmed = before.trim_end_matches('\n');
        return format!("{trimmed}\n");
    }
    let seam = before.len() - before.trim_end_matches('\n').len()
        + (after.len() - after.trim_start_matches('\n').len());
    if seam <= 2 {
        return format!("{before}{after}");
    }
    format!(
        "{}\n\n{}",
        before.trim_end_matches('\n'),
        after.trim_start_matches('\n')
    )
}

/// Extract a method's RETURN TEXT: the substring after the LAST `->` at bracket
/// depth 0 within `text`, trimmed. `None` when no depth-0 `->` is found
/// (extraction failure — design-note refinement 2).
fn extract_method_return_text(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut last: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'-' if depth == 0 && bytes.get(i + 1) == Some(&b'>') => {
                last = Some(i + 2);
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    let start = last?;
    let s = text.get(start..)?.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Slice `source` by an [`RBSLocationRange`], bounds-checking the i32 offsets.
fn slice_of(source: &str, loc: RBSLocationRange) -> &str {
    let len = source.len();
    let start = clamp_offset(loc.start(), len);
    let end = clamp_offset(loc.end(), len).max(start);
    source.get(start..end).unwrap_or("")
}

/// Clamp an i32 RBS byte offset into `0..=len` (pitfall 8: validate before use).
fn clamp_offset(v: i32, len: usize) -> usize {
    if v < 0 {
        0
    } else {
        (v as usize).min(len)
    }
}

#[cfg(test)]
mod tests;

/// Whether every `untyped` in `erased` sits inside a `ClassName[…]` type-arg
/// slot — `Range[untyped]`, `Hash[untyped, untyped]`, `Range[Array[untyped] |
/// Integer]` — the reference's elaborated emit, exempt from the
/// buried-`untyped` skip (rigor-rs#493 item 7). An `untyped` inside a TUPLE
/// literal's `[…]` (a bracket not preceded by a class identifier) still
/// counts as an inference hole.
fn is_nominal_untyped_args(erased: &str) -> bool {
    let bytes = erased.as_bytes();
    let mut found = false;
    let mut idx = 0;
    while let Some(off) = erased[idx..].find("untyped") {
        let pos = idx + off;
        found = true;
        // Find the innermost UNCLOSED `[` enclosing this occurrence.
        let mut stack: Vec<usize> = Vec::new();
        for (i, &b) in bytes.iter().enumerate().take(pos) {
            match b {
                b'[' => stack.push(i),
                b']' => {
                    stack.pop();
                }
                _ => {}
            }
        }
        let Some(&open) = stack.last() else { return false };
        // The `[` must be a generic-args bracket: preceded immediately by a
        // class-identifier tail (`Foo[`, `Foo::Bar[`).
        let head: String = erased[..open]
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
            .collect();
        let head: String = head.chars().rev().collect();
        if !head.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            return false;
        }
        idx = pos + 7;
    }
    found
}
