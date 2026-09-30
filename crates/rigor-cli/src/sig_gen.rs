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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ruby_rbs::node::{MethodDefinitionKind, Node as RbsNode, RBSLocationRange};

use rigor_index::CoreIndex;
use rigor_infer::{SourceIndex, TypeEnv, Typer};
use rigor_parse::{lower, parse, LoweredAst, Node, NodeId, ParamShape, Visibility};
use rigor_types::{ClassId, Interner, Type, TypeId};

mod meta_class;
mod sig_env;
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
        Switch::new("overwrite", &[("overwrite", false)], ArgStyle::Flag, ValueKind::Raw, "--overwrite", "", &["Allow tighter-return updates to replace user-authored RBS"]),
        Switch::new("include-private", &[("include-private", false)], ArgStyle::Flag, ValueKind::Raw, "--include-private", "", &["Emit private / protected instance methods (default: public only)"]),
        Switch::new("effect-envelopes", &[("effect-envelopes", false)], ArgStyle::Flag, ValueKind::Raw, "--effect-envelopes", "", &["Also emit %a{rigor:v1:effect ...} for effectful methods (requires the effects: opt-in)"]),
        Switch::new("no-cache", &[("no-cache", false)], ArgStyle::Flag, ValueKind::Raw, "--no-cache", "", &["Do not read or write the analysis cache (effect collection only)"]),
        Switch::new("format", &[("format", false)], ArgStyle::Required, ValueKind::Raw, "--format", "=FORMAT", &["Output format: text or json"]),
        Switch::new("params", &[("params", false)], ArgStyle::Required, ValueKind::Raw, "--params", "=POLICY", &["Parameter policy: untyped (default), observed, observed-strict"]),
        Switch::new("observe", &[("observe", false)], ArgStyle::Required, ValueKind::Raw, "--observe", "=PATH", &["Directory / file to scan for call-site observations (repeatable)"]),
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
                "print" => mode = "print",
                "diff" => mode = "diff",
                "write" => mode = "write",
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
                "config" => explicit_config = Some(value.unwrap().as_str().to_string()),
                _ => unreachable!("the switch table is closed"),
            },
        }
    }

    // `validation_error` — inside `parse_options`, before `Configuration.load`,
    // each failure a `sig-gen: <msg>` line + exit 64.
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

    // The sig-gen-local, FQN-keyed declaration env, built ONCE from the project's
    // own `.rbs` under the configured signature dirs (ADR-14 slice 10). Drives
    // generation-time `new_method` / `tighter_return` classification. Sig-gen-local
    // by construction — the `check` path never sees it.
    let project_root = std::env::current_dir()
        .and_then(|d| d.canonicalize())
        .unwrap_or_else(|_| PathBuf::from("."));
    let sig_env = SigEnv::build(&cfg.all_signature_dirs(&project_root));

    if mode == "write" {
        return cmd_write(&files, include_private, &format, overwrite, &cfg, &sig_env);
    }

    // `--overwrite` only affects the write path (it governs replacing an existing
    // declaration during merge); on a print/diff run it is inert, exactly like the
    // reference (the flag lives on the Writer).

    let candidates: Vec<Candidate> =
        files.iter().flat_map(|p| generate_file(p, include_private, &sig_env)).collect();

    // `--format json` renders the candidate table regardless of print/diff mode
    // (reference `Renderer#render`); text picks the diff or print layout.
    match (format.as_str(), mode == "diff") {
        ("json", _) => render_json(&candidates),
        (_, true) => render_diff(&candidates),
        (_, false) => render_text(&candidates),
    }
    ExitCode::SUCCESS
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
}

/// Record each class/module declaration's keyword + superclass into `info`,
/// keyed by qualified name (reference `build_namespace_kinds` /
/// `build_superclasses`).
fn collect_namespace_info(ast: &LoweredAst, id: NodeId, prefix: &[String], info: &mut NamespaceInfo) {
    let (name, body, kind, superclass): (&String, &[NodeId], &'static str, Option<&String>) =
        match ast.get(id) {
            Node::ClassDef { name, body, superclass_path, .. } => {
                (name, body, "class", superclass_path.as_ref())
            }
            Node::ModuleDef { name, body, .. } => (name, body, "module", None),
            _ => return,
        };
    let mut qualified = prefix.to_vec();
    qualified.push(name.clone());
    let q = qualified.join("::");
    info.kinds.insert(q.clone(), kind);
    if let Some(sp) = superclass {
        info.supers.insert(q, sp.clone());
    }
    for &child in body {
        collect_namespace_info(ast, child, &qualified, info);
    }
}

/// Produce the printable candidates for one source file (drops the write-only
/// [`NamespaceInfo`]).
fn generate_file(path: &str, include_private: bool, sig_env: &SigEnv) -> Vec<Candidate> {
    generate_file_with_info(path, include_private, sig_env).0
}

/// Produce candidates + the `--write` namespace metadata for one source file. A
/// parse/read failure (or a file with no reachable named class body) yields no
/// candidates.
fn generate_file_with_info(
    path: &str,
    include_private: bool,
    sig_env: &SigEnv,
) -> (Vec<Candidate>, NamespaceInfo) {
    let Ok(source) = std::fs::read_to_string(path) else {
        return (Vec::new(), NamespaceInfo::default());
    };
    let parsed = parse(source.as_bytes());
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
    let mut info = NamespaceInfo::default();
    if let Node::Program { body, .. } = ast.get(root) {
        for &child in body {
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
                &mut interner,
                &mut out,
            );
            collect_namespace_info(&ast, child, &[], &mut info);
        }
    }
    register_meta_classes(&layouts, &mut info);
    // The print header depends on the WHOLE file's namespace metadata, so it is
    // stamped once the walks are done rather than at construction.
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
    }
}

/// The `--print` declaration line for a class group (reference
/// `declaration_header`). Print mode used to hard-code `class`, which turned a
/// MODULE into a class the moment it held an emittable method — output that
/// raises `RBS::DuplicatedDeclarationError` on load if the real `module` is
/// declared anywhere else (rigor#227). Defaulting to `class` when the map has no
/// entry keeps the pre-existing spelling for a leaf class.
fn declaration_header(info: &NamespaceInfo, class_name: &str) -> String {
    if info.kinds.get(class_name) == Some(&"module") {
        return format!("module {class_name}");
    }
    match info.supers.get(class_name) {
        Some(superclass) => format!("class {class_name} < {superclass}"),
        None => format!("class {class_name}"),
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
        Node::ClassDef { name, body, .. } | Node::ModuleDef { name, body, .. } => {
            let fqn = qualify_join(prefix, name);
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
    interner: &mut Interner,
    out: &mut Vec<Candidate>,
) {
    let (name, visibilities, body) = match ast.get(id) {
        Node::ClassDef { name, method_visibilities, body, .. } => {
            (name, method_visibilities.clone(), body.as_slice())
        }
        Node::ModuleDef { name, method_visibilities, body, .. } => {
            (name, method_visibilities.clone(), body.as_slice())
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
            (name, block_visibilities(ast, block_body), block_body.as_slice())
        }
        _ => return,
    };

    let mut qualified = prefix.to_vec();
    qualified.push(name.clone());
    let class_name = qualified.join("::");

    // Collect instance + singleton methods in ONE pass over the class body so
    // they emit in SOURCE ORDER (the reference walks the AST top-to-bottom): a
    // direct instance `def x`, a `def self.x`, and the receiver-less inner defs
    // of a `class << self`. `method_bodies` harvests exactly the direct
    // `Definition{name:Some}` set, so walking the body for them is equivalent AND
    // recovers each def's span for the ordering (the sort key).
    //
    // `mf_active` tracks a bare `module_function` (no args) seen EARLIER in this
    // body — it makes every SUBSEQUENT instance def dual, rendered `def self?.name`
    // (reference `@module_function_methods`). Position matters: a def BEFORE the
    // call stays a plain instance method. The `module_function :sym` ARGS form does
    // NOT flip the mode (oracle-probed) and is ignored. It applies in a CLASS body
    // too (rule_catalog.rb), not just a module.
    let mut mf_active = false;
    let mut sigs: Vec<(MethodSig, Option<Visibility>, usize)> = Vec::new();
    for &child in body {
        match ast.get(child) {
            // A BARE `module_function` (no args) flips the mode for later defs.
            Node::Call { method, receiver: None, args, .. }
                if method == "module_function" && args.is_empty() =>
            {
                mf_active = true;
            }
            Node::Definition {
                name: Some(n), body: b, params, param_shape, has_explicit_return, span, ..
            } => {
                let vis = visibilities.iter().find(|(m, _)| m == n).map(|(_, v)| *v);
                sigs.push((
                    sig_of(n, b, params, param_shape, *has_explicit_return, false, mf_active),
                    vis,
                    span.0,
                ));
            }
            Node::Definition {
                singleton_name: Some(n), body: b, params, param_shape, has_explicit_return, span, ..
            } => {
                sigs.push((
                    sig_of(n, b, params, param_shape, *has_explicit_return, true, false),
                    None,
                    span.0,
                ));
            }
            Node::Definition { is_singleton_class: true, body: sbody, .. } => {
                for &inner in sbody {
                    if let Node::Definition {
                        name: Some(n), body: b, params, param_shape, has_explicit_return, span, ..
                    } = ast.get(inner)
                    {
                        sigs.push((
                            sig_of(n, b, params, param_shape, *has_explicit_return, true, false),
                            None,
                            span.0,
                        ));
                    }
                }
            }
            _ => {}
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
        Nested(NodeId),
    }
    let mut items: Vec<(usize, Emit)> = Vec::new();
    for (sig, vis, span) in &sigs {
        items.push((*span, Emit::Method(sig, *vis)));
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
                interner,
                out,
            ),
        }
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
) -> MethodSig<'a> {
    MethodSig { name, body, params, param_shape, has_explicit_return, singleton, module_function }
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
    // `visibility_excludes?` — returns false for a singleton, so singletons are
    // never visibility-skipped).
    if !include_private
        && !sig.singleton
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
        return Some(Candidate {
            file: path.to_string(),
            class_name: class_name.to_string(),
            method_name: "initialize".to_string(),
            kind: "instance",
            rbs: format!("def initialize: ({params}) -> void"),
            // The reference stub's `inferred_return` is `untyped` (the rbs is
            // `-> void`, but the candidate carries the fallback type).
            inferred_return: "untyped".to_string(),
            // `initialize` BYPASSES env lookup entirely (probe K: an identical
            // declared `initialize` is still emitted `new_method`) — keep the stub
            // path exactly as-is, before any classification.
            classification: "new_method",
            declared_return_rbs: None,
            decl_header: String::new(),
        });
    }

    // Simple parameter shape: rigor-rs sets `params = None` for exactly the
    // splat/post/kwargs/block/optional forms the reference's
    // `simple_parameter_shape?` rejects. Only plain requireds qualify.
    let arity = sig.params.as_ref()?.len();

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
    let returns = collect_explicit_returns(ast, sig)?;

    // Tail type (reference `body_last_expression` + `safe_type_of`): the last
    // statement's type; an assignment tail evaluates to its RHS; a `return E`
    // tail evaluates to its value (`nil` when bare).
    let tail_ty = def_return_type(ast, typer, sig.body, env, interner)?;

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
        return None;
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
        if e.contains("untyped") {
            return None;
        }
        // A bare GENERIC nominal member (`Array` / `Hash` / …) would be
        // `Array[untyped]` after the reference's `TypeElaborator` fill (deferred
        // here), so its presence skips the method rather than emit an
        // under-elaborated form that would byte-diverge on a shared method.
        if is_bare_generic_name(&e) {
            return None;
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
    let (classification, declared_return_rbs) =
        classify(sig, class_name, &erased, &members, ast, index, sig_env, interner)?;

    Some(Candidate {
        file: path.to_string(),
        class_name: class_name.to_string(),
        method_name: sig.name.to_string(),
        kind: if sig.singleton { "singleton" } else { "instance" },
        rbs,
        inferred_return: erased,
        classification,
        declared_return_rbs,
        decl_header: String::new(),
    })
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
            // A tightening must be a SINGLE bare carrier whose nominal-of is
            // exactly the declared class. A union (`> 1` member) never bare-tightens
            // a single declared class (FP-safe under-emit; e.g. declared union member
            // loss, `Integer | Float` narrowing `Numeric`).
            let inferred_ty = match members {
                [single] => *single,
                _ => return None,
            };
            // Wider / unrelated: the inferred's nominal is not the declared class
            // (probe F — declared `Integer`, inferred `"hi"` → `String`).
            if index.class_name_of(interner, inferred_ty) != Some(decl.as_str()) {
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
fn def_return_type(
    ast: &LoweredAst,
    typer: &Typer,
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
    Some(typer.type_of(ast, target, env, interner))
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
/// rest (`**untyped`) → block (`?{ (?) -> void }`). Posts are omitted (as the
/// reference does).
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
    if shape.has_block {
        parts.push("?{ (?) -> void }".to_string());
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
    serde_json::to_string_pretty(&json!({ "candidates": rows })).unwrap()
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
    let candidates: Vec<Candidate> =
        files.iter().flat_map(|p| generate_file(p, false, &sig_env)).collect();
    candidates_json_string(&candidates)
}

/// One candidate's JSON object (reference `MethodCandidate#to_h`): the per-
/// candidate key set with the real `classification`, and `declared_return_rbs`
/// present ONLY on a `tighter_return` (the reference `.compact`s the nil).
fn candidate_json(c: &Candidate) -> serde_json::Value {
    use serde_json::json;
    let mut obj = json!({
        "file": c.file,
        "class": c.class_name,
        "method": c.method_name,
        "kind": c.kind,
        "classification": c.classification,
        "rbs": c.rbs,
        "inferred_return": c.inferred_return,
    });
    if let Some(dr) = &c.declared_return_rbs {
        obj.as_object_mut().unwrap().insert("declared_return_rbs".to_string(), json!(dr));
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
    /// `"created"` | `"updated"` | `"noop"` | `"skipped_outside_sig_root"`.
    action: &'static str,
    applied: Vec<Candidate>,
    /// Candidates the merge declined because a user-authored member of the same
    /// `(name, kind)` already exists with a DIFFERENT declared return (reference
    /// `merge_into_existing_class`'s `skipped` accumulator). Empty on create.
    skipped: Vec<SkipEntry>,
}

/// One skipped (user-authored-conflict) candidate + the classification metadata
/// the JSON report surfaces (design-note refinement 2). `reason` is always
/// `user_authored` so it is hardcoded in the renderer.
struct SkipEntry {
    candidate: Candidate,
    /// `"tighter_return"` when the existing return text was extractable and
    /// differs; `"new_method"` when extraction failed (residual — see the note).
    classification: &'static str,
    /// The existing member's extracted return text (trimmed), present only for
    /// `tighter_return`.
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
    for f in files {
        let (candidates, info) = generate_file_with_info(f, include_private, sig_env);
        for c in candidates {
            let target =
                target_for(&c.file, &c.class_name, &source_root, &sig_root, &project_root, &layout);
            tagged.push((target, c));
        }
        for shell in &info.shells {
            // Route a shell to its ENCLOSING class's target (a shell rides in the
            // same file as the class it is nested in — e.g. `Rigor::Triage::Selector`
            // routes with `Rigor::Triage`), so a consolidated layout keeps the
            // shell beside its class. A top-level shell (no `::`) routes by itself.
            let route_name = shell.rsplit_once("::").map(|(head, _)| head).unwrap_or(shell);
            let target =
                target_for(f, route_name, &source_root, &sig_root, &project_root, &layout);
            tagged_shells.push((target, shell.clone()));
        }
        merged.kinds.extend(info.kinds);
        merged.supers.extend(info.supers);
        merged.shells.extend(info.shells);
    }

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
                &merged.supers,
                overwrite,
            ));
            continue;
        }
        let content = render_new_file(&group, &shells, &merged);
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&target, content).is_err() {
            eprintln!("sig-gen: failed to write {target_str}");
            return ExitCode::from(1);
        }
        results.push(WriteResult {
            source,
            target: target_str,
            action: "created",
            applied: group,
            skipped: Vec::new(),
        });
    }

    match format {
        "json" => render_write_json(&results),
        _ => render_write_text(&results),
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
        info.supers.get(&qualified).map(|s| format!(" < {s}")).unwrap_or_default()
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
    format!("{indent}{keyword} {}{superclass}\n{body}{indent}end\n", node.name)
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
fn render_write_text(results: &[WriteResult]) {
    if results.iter().all(|r| r.action == "noop") {
        println!("No changes");
        return;
    }
    for r in results {
        match r.action {
            "created" => println!("created {} ({} method(s))", r.target, r.applied.len()),
            "updated" => println!(
                "updated {} (+{}, skipped {} user-authored)",
                r.target,
                r.applied.len(),
                r.skipped.len()
            ),
            "skipped_outside_sig_root" => {
                println!("skipped {} -> {} (outside sig root)", r.source, r.target)
            }
            _ => {}
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
            json!({
                "source": r.source, "target": r.target, "action": r.action,
                "applied": applied, "skipped": skipped,
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&json!({ "results": rows })).unwrap());
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
fn update_existing(
    source_path: String,
    target: &Path,
    target_str: String,
    candidates: Vec<Candidate>,
    shells: &[String],
    supers: &HashMap<String, String>,
    overwrite: bool,
) -> WriteResult {
    let Ok(source) = std::fs::read_to_string(target) else {
        return WriteResult {
            source: source_path,
            target: target_str,
            action: "noop",
            applied: Vec::new(),
            skipped: Vec::new(),
        };
    };

    // Shell injection on the MERGE path is deferred: when a file already exists,
    // any shell it needs was written on the CREATE run (`render_new_file`), so a
    // `--write` re-run stays idempotent (the shell is present, no conflict). The
    // only uncovered case is merging into a USER-authored sig that lacks a shell
    // for a `Data.define` return — a documented follow-up, not a regression.
    let _ = shells;
    let MergeOutcome { source: merged, action, applied, skipped } =
        apply_merge(source, candidates, supers, overwrite);
    if action == "updated" {
        let _ = std::fs::write(target, &merged);
    }
    WriteResult { source: source_path, target: target_str, action, applied, skipped }
}

/// The pure result of merging a candidate group into an existing `.rbs` source
/// string — the file-I/O-free core of [`update_existing`], shared with tests.
struct MergeOutcome {
    source: String,
    action: &'static str,
    applied: Vec<Candidate>,
    skipped: Vec<SkipEntry>,
}

/// Merge a candidate group into `source` (reference `Writer#update_existing`
/// minus disk I/O). A parse-failure gate leaves the source byte-untouched with
/// `action: "noop"`; otherwise each class group (first-seen order) is merged or
/// appended, re-parsing fresh before each so offsets are never stale.
fn apply_merge(
    mut source: String,
    candidates: Vec<Candidate>,
    supers: &HashMap<String, String>,
    overwrite: bool,
) -> MergeOutcome {
    // Parse-failure gate: a malformed target is left byte-untouched (reference
    // `parse_signature` → nil → `:noop`).
    if ruby_rbs::node::parse(&source).is_err() {
        return MergeOutcome { source, action: "noop", applied: Vec::new(), skipped: Vec::new() };
    }
    let mut applied: Vec<Candidate> = Vec::new();
    let mut skipped: Vec<SkipEntry> = Vec::new();
    for (class_name, group) in group_by_class(candidates) {
        merge_class(&mut source, &class_name, group, supers, overwrite, &mut applied, &mut skipped);
    }
    let action = if applied.is_empty() { "noop" } else { "updated" };
    MergeOutcome { source, action, applied, skipped }
}

/// Group candidates by `class_name`, preserving first-seen class order
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
    order.into_iter().map(|k| (k.clone(), groups.remove(&k).unwrap())).collect()
}

/// Merge one class group: find the class by FQN (re-parsing fresh so offsets are
/// current), then merge into it or append a new class block (reference
/// `merge_class`).
fn merge_class(
    source: &mut String,
    class_name: &str,
    candidates: Vec<Candidate>,
    supers: &HashMap<String, String>,
    overwrite: bool,
    applied: &mut Vec<Candidate>,
    skipped: &mut Vec<SkipEntry>,
) {
    // Extract owned decl info before mutating (the parsed tree borrows `source`).
    let found: Option<ClassDeclInfo> = match ruby_rbs::node::parse(source) {
        Ok(sig) => find_and_extract(source, sig.declarations().iter(), &[], class_name),
        Err(_) => None,
    };
    match found {
        Some(info) => merge_into_existing(source, &info, candidates, overwrite, applied, skipped),
        None => append_new_class(source, class_name, candidates, supers.get(class_name), applied),
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

    // Splice NEW members before the class's closing `end` token (fixed 2-space
    // indent, one `"  {rbs}\n"` per method, concatenated — reference
    // `insert_into_class`). The token-start splice + fixed indent reproduces the
    // oracle's nested-case bytes with no special-casing. This runs BEFORE any
    // replacement: it inserts at `end_start`, after every existing member, so the
    // captured member spans (all below `end_start`) stay valid (reference order:
    // `insert_into_class` then `replace_eligible_conflicts`).
    if !new_methods.is_empty() {
        let addition: String =
            new_methods.iter().map(|c| format!("{INDENT}{}\n", c.rbs)).collect();
        let at = info.end_start.min(source.len());
        source.insert_str(at, &addition);
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
            source.replace_range(start..end, &c.rbs);
            applied.push(c);
        }
        for (c, existing_rt) in rest {
            skip_conflict(c, existing_rt, skipped);
        }
        return;
    }

    // No `--overwrite`: every conflict is preserved as user-authored.
    for (c, m) in conflicting {
        let existing_rt = m.return_text;
        skip_conflict(c, existing_rt, skipped);
    }
}

/// Record one preserved (user-authored) conflict as a [`SkipEntry`] (reference
/// `merge_into_existing_class`'s `skipped` accumulator). A candidate GENERATION
/// already classified `tighter_return` carries its own `declared_return_rbs`
/// resolved from the [`SigEnv`] — trust it, do NOT re-derive from the target text
/// (amendment: no double-divergence). Only a `new_method` conflict (the class
/// escaped env classification — e.g. a consolidated target whose class the
/// generation env did not see) falls back to the write-time return extraction:
/// an equal return drops silently, a differing one skips as `tighter_return`,
/// an unextractable one as `new_method`.
fn skip_conflict(c: Candidate, existing_rt: Option<String>, skipped: &mut Vec<SkipEntry>) {
    if c.classification == "tighter_return" {
        let declared_return_rbs = c.declared_return_rbs.clone();
        skipped.push(SkipEntry { candidate: c, classification: "tighter_return", declared_return_rbs });
        return;
    }
    let cand_rt = extract_method_return_text(&c.rbs);
    match (existing_rt, cand_rt) {
        (Some(er), Some(cr)) if er.trim() == cr.trim() => {
            // Equivalent → drop silently (not applied, not skipped).
        }
        (Some(er), Some(_)) => skipped.push(SkipEntry {
            candidate: c,
            classification: "tighter_return",
            declared_return_rbs: Some(er.trim().to_string()),
        }),
        _ => skipped.push(SkipEntry {
            candidate: c,
            classification: "new_method",
            declared_return_rbs: None,
        }),
    }
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

/// Append a NEW class block for a class not declared in the file (reference
/// `append_new_class`): a COMPACT qualified header + `< Super` when known, body
/// lines at 2-space indent, ONE leading blank line, and a trailing-newline
/// repair on the original file first. All methods are applied.
fn append_new_class(
    source: &mut String,
    class_name: &str,
    candidates: Vec<Candidate>,
    superclass: Option<&String>,
    applied: &mut Vec<Candidate>,
) {
    let body = candidates
        .iter()
        .map(|c| format!("{INDENT}{}", c.rbs))
        .collect::<Vec<_>>()
        .join("\n");
    let header = match superclass {
        Some(s) => format!("class {class_name} < {s}"),
        None => format!("class {class_name}"),
    };
    if !source.ends_with('\n') {
        source.push('\n');
    }
    source.push_str(&format!("\n{header}\n{body}\nend\n"));
    applied.extend(candidates);
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
