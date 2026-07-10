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
//! ## Deferred (later slices, each its own gate)
//!
//! - `--params=observed` (the `ObservationCollector`) — params stay `untyped`.
//!   NOTE: this is what makes the `--overwrite` `NEW_METHOD`-tightens-`untyped`
//!   replacement path (ported faithfully) actually fire — until it lands, that
//!   path is dead for BOTH tools (an initialize stub stays `(untyped) -> void`),
//!   so its absence is parity-safe;
//! - `Const = Data.define(...)` / `Struct.new(...)` empty CLASS SHELLS (the
//!   reference `--write`s a `class Selector\nend` decl for the constant; rigor-rs
//!   qualifies RETURNS of it but does not yet generate the shell — an under-emit,
//!   a valid RBS subset), `attr_*` reader generation;
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

mod sig_env;
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
}

/// `rigor sig-gen [--print] [--format text|json] [--include-private] [--config PATH] [paths]`.
/// Exit 0 on success, 64 on a usage error, 2 for a not-yet-ported mode.
pub fn cmd_sig_gen(args: &[String]) -> ExitCode {
    let mut format = "text";
    let mut include_private = false;
    let mut write = false;
    let mut diff = false;
    let mut overwrite = false;
    let mut explicit_config: Option<&str> = None;
    let mut positional: Vec<&str> = Vec::new();

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--print" => {} // the default mode
            "--write" => write = true,
            "--diff" => diff = true,
            "--overwrite" => overwrite = true,
            "--include-private" => include_private = true,
            "--format" => match it.next().map(String::as_str) {
                Some(f @ ("text" | "json")) => format = f,
                other => {
                    eprintln!("sig-gen: --format expects `text` or `json`, got {other:?}");
                    return ExitCode::from(64);
                }
            },
            "--config" => match it.next() {
                Some(p) => explicit_config = Some(p),
                None => {
                    eprintln!("sig-gen: --config expects a path");
                    return ExitCode::from(64);
                }
            },
            other if other.starts_with("--params") || other.starts_with("--observe") => {
                eprintln!("sig-gen: `{other}` is not yet implemented in this slice (params stay untyped)");
                return ExitCode::from(2);
            }
            other if other.starts_with("--") => {
                eprintln!("sig-gen: unknown option `{other}`");
                return ExitCode::from(64);
            }
            other => positional.push(other),
        }
    }

    // Paths: positional args, or config `paths:` when none are supplied
    // (reference `@argv.empty? ? configuration.paths : @argv`).
    let cfg = crate::Config::load(explicit_config.map(Path::new));
    let config_paths: Vec<&str>;
    let raw: &[&str] = if positional.is_empty() {
        config_paths = cfg.paths.iter().map(String::as_str).collect();
        &config_paths
    } else {
        &positional
    };
    let files = resolve_paths(raw);

    // The sig-gen-local, FQN-keyed declaration env, built ONCE from the project's
    // own `.rbs` under the configured signature dirs (ADR-14 slice 10). Drives
    // generation-time `new_method` / `tighter_return` classification. Sig-gen-local
    // by construction — the `check` path never sees it.
    let project_root = std::env::current_dir()
        .and_then(|d| d.canonicalize())
        .unwrap_or_else(|_| PathBuf::from("."));
    let sig_env = SigEnv::build(&cfg.all_signature_dirs(&project_root));

    if write {
        return cmd_write(&files, include_private, format, overwrite, &cfg, &sig_env);
    }

    // `--overwrite` only affects the write path (it governs replacing an existing
    // declaration during merge); on a print/diff run it is inert, exactly like the
    // reference (the flag lives on the Writer).

    let candidates: Vec<Candidate> =
        files.iter().flat_map(|p| generate_file(p, include_private, &sig_env)).collect();

    // `--format json` renders the candidate table regardless of print/diff mode
    // (reference `Renderer#render`); text picks the diff or print layout.
    match (format, diff) {
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
    let ast = lower(&parse(source.as_bytes()));
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

    let mut out = Vec::new();
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
                &mut interner,
                &mut out,
            );
            collect_namespace_info(&ast, child, &[], &mut info);
        }
    }
    (out, info)
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
    interner: &mut Interner,
    out: &mut Vec<Candidate>,
) {
    let (name, method_bodies, visibilities, body) = match ast.get(id) {
        Node::ClassDef { name, method_bodies, method_visibilities, body, .. } => {
            (name, method_bodies, method_visibilities, body)
        }
        Node::ModuleDef { name, method_bodies, method_visibilities, body, .. } => {
            (name, method_bodies, method_visibilities, body)
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
    let _ = (method_bodies, visibilities); // superseded by the body walk below
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
        if matches!(ast.get(child), Node::ClassDef { .. } | Node::ModuleDef { .. }) {
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
                interner,
                out,
            ),
        }
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
            Node::Call { block_body, span, .. } if !block_body.is_empty() => {
                if within(*span, &regions) {
                    for &b in block_body {
                        barriers.push(ast.get(b).span());
                    }
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
            println!("class {class}");
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
    use serde_json::json;
    let rows: Vec<_> = candidates.iter().map(candidate_json).collect();
    println!("{}", serde_json::to_string_pretty(&json!({ "candidates": rows })).unwrap());
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
    for f in files {
        let (candidates, info) = generate_file_with_info(f, include_private, sig_env);
        merged.kinds.extend(info.kinds);
        merged.supers.extend(info.supers);
        for c in candidates {
            let target =
                target_for(&c.file, &c.class_name, &source_root, &sig_root, &project_root, &layout);
            tagged.push((target, c));
        }
    }

    // Group by target, preserving first-seen order (reference groups by target).
    let mut targets: Vec<PathBuf> = Vec::new();
    for (t, _) in &tagged {
        if !targets.contains(t) {
            targets.push(t.clone());
        }
    }

    let sig_root_dir = project_root.join(&sig_root);
    let mut results: Vec<WriteResult> = Vec::new();
    for target in targets {
        let group: Vec<Candidate> =
            tagged.iter().filter(|(t, _)| *t == target).map(|(_, c)| clone_candidate(c)).collect();
        let source = group.first().map(|c| c.file.clone()).unwrap_or_default();
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
                &merged.supers,
                overwrite,
            ));
            continue;
        }
        let content = render_new_file(&group, &merged);
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
/// `render_tree_nodes`): build a namespace tree from the candidates, then render
/// each top-level node, joined by a blank line.
fn render_new_file(candidates: &[Candidate], info: &NamespaceInfo) -> String {
    let mut roots: Vec<TreeNode> = Vec::new();
    for c in candidates {
        let segs: Vec<&str> = c.class_name.split("::").collect();
        insert_into_tree(&mut roots, &segs, &c.rbs);
    }
    roots
        .iter()
        .map(|n| render_tree_node(n, info, 0, &[]))
        .collect::<Vec<_>>()
        .join("\n")
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
mod tests {
    use super::*;

    fn candidates_tagged(tag: &str, src: &str, include_private: bool) -> Vec<Candidate> {
        // Write to a UNIQUE temp file per test so parallel runs never race on a
        // shared path (write to `generate_file`'s read path is the point).
        let dir = std::env::temp_dir().join(format!("rigor_siggen_test_{tag}"));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("t.rb");
        std::fs::write(&file, src).unwrap();
        // An EMPTY sig env: no project RBS ⇒ every candidate is `new_method`
        // (these tests exercise inference/rendering, not env classification —
        // that has its own `sig_env` unit tests + the oracle E2E gate).
        let env = SigEnv::build(&[]);
        let out = generate_file(file.to_str().unwrap(), include_private, &env);
        let _ = std::fs::remove_file(&file);
        out
    }

    /// Generate candidates for `rb_src` with a project sig env built from
    /// `rbs_src` (the sig-gen-local [`SigEnv`]) — the env-classification unit
    /// harness. A fresh unique dir per tag isolates parallel runs.
    fn candidates_with_sig(tag: &str, rb_src: &str, rbs_src: &str) -> Vec<Candidate> {
        let dir = std::env::temp_dir().join(format!("rigor_siggen_env_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join("sig")).unwrap();
        let rb = dir.join("lib/foo.rb");
        std::fs::write(&rb, rb_src).unwrap();
        std::fs::write(dir.join("sig/foo.rbs"), rbs_src).unwrap();
        let env = SigEnv::build(&[dir.join("sig")]);
        let out = generate_file(rb.to_str().unwrap(), false, &env);
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn env_no_sig_class_is_new_method() {
        // Probe A: the class is absent from the env ⇒ NEW_METHOD (`# [new]`).
        let cs = candidates_with_sig("no_env", "class Foo\n  def hash\n    1\n  end\nend\n", "class Bar\nend\n");
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].classification, "new_method");
        assert_eq!(cs[0].declared_return_rbs, None);
        assert_eq!(cs[0].rbs, "def hash: () -> 1");
    }

    #[test]
    fn env_empty_decl_resolves_inherited_tighter() {
        // Probe N: an EMPTY `class Foo` in sig ⇒ `hash` resolves through Object ⇒
        // tighter, was: Integer.
        let cs = candidates_with_sig("empty_decl", "class Foo\n  def hash\n    1\n  end\nend\n", "class Foo\nend\n");
        assert_eq!(cs[0].classification, "tighter_return");
        assert_eq!(cs[0].declared_return_rbs.as_deref(), Some("Integer"));
    }

    #[test]
    fn env_fqn_gate_resolves_nested_class() {
        // Probe Q1: the gate is FQN-keyed — a nested `M::Foo` resolves (rigor-index
        // short-name folding cannot; this is why SigEnv exists).
        let cs = candidates_with_sig(
            "fqn",
            "module M\n  class Foo\n    def hash\n      1\n    end\n  end\nend\n",
            "module M\n  class Foo\n  end\nend\n",
        );
        assert_eq!(cs[0].class_name, "M::Foo");
        assert_eq!(cs[0].classification, "tighter_return");
        assert_eq!(cs[0].declared_return_rbs.as_deref(), Some("Integer"));
    }

    #[test]
    fn env_project_superclass_resolves_tighter() {
        // Probe O: `class Foo < Base`, Base declares `greeting: () -> String`.
        let cs = candidates_with_sig(
            "super",
            "class Foo\n  def greeting\n    \"hi\"\n  end\nend\n",
            "class Base\n  def greeting: () -> String\nend\n\nclass Foo < Base\nend\n",
        );
        assert_eq!(cs[0].classification, "tighter_return");
        assert_eq!(cs[0].declared_return_rbs.as_deref(), Some("String"));
    }

    #[test]
    fn env_inherited_equivalent_drops() {
        // Probe P: `def hash; [1].size; end` folds `1` but the raw tail `[1].size`
        // is not a literal ⇒ computed_literal_tightening ⇒ DROP (No candidates).
        let cs = candidates_with_sig("equiv", "class Foo\n  def hash\n    [1].size\n  end\nend\n", "class Foo\nend\n");
        assert!(cs.is_empty(), "computed-literal drop: {cs:?}");
    }

    #[test]
    fn env_initialize_bypasses_classification() {
        // Probe K: an identical declared `initialize` still emits `# [new]`.
        let cs = candidates_with_sig(
            "init",
            "class Foo\n  def initialize(a)\n    @a = a\n  end\nend\n",
            "class Foo\n  def initialize: (untyped) -> void\nend\n",
        );
        assert_eq!(cs[0].classification, "new_method");
        assert_eq!(cs[0].rbs, "def initialize: (untyped) -> void");
    }

    #[test]
    fn env_attr_reader_classifies_like_method() {
        // Probe I: `attr_reader name: String` ⇒ `def name` tighter, was: String.
        let cs = candidates_with_sig("attr", "class Foo\n  def name\n    \"n\"\n  end\nend\n", "class Foo\n  attr_reader name: String\nend\n");
        assert_eq!(cs[0].classification, "tighter_return");
        assert_eq!(cs[0].declared_return_rbs.as_deref(), Some("String"));
    }

    #[test]
    fn env_singleton_own_and_inherited() {
        // Probe Q2: own `def self.build: () -> Integer` ⇒ tighter.
        let own = candidates_with_sig(
            "sing_own",
            "class Foo\n  def self.build\n    1\n  end\nend\n",
            "class Foo\n  def self.build: () -> Integer\nend\n",
        );
        assert_eq!(own[0].classification, "tighter_return");
        assert_eq!(own[0].kind, "singleton");
        assert_eq!(own[0].declared_return_rbs.as_deref(), Some("Integer"));

        // Probe Q3: `def self.hash` on an empty `class Foo` inherits `Object#hash`
        // through the class object's `Class`/`Module`/`Object` ancestry.
        let inh = candidates_with_sig(
            "sing_inh",
            "class Foo\n  def self.hash\n    1\n  end\nend\n",
            "class Foo\nend\n",
        );
        assert_eq!(inh[0].classification, "tighter_return");
        assert_eq!(inh[0].declared_return_rbs.as_deref(), Some("Integer"));
    }

    #[test]
    fn env_literal_tightening_emits_but_computed_drops() {
        // A directly-authored literal tail tightens; a computed constant drops.
        let lit = candidates_with_sig("lit", "class Foo\n  def m\n    1\n  end\nend\n", "class Foo\n  def m: () -> Integer\nend\n");
        assert_eq!(lit[0].classification, "tighter_return");
        // Assignment tail: `x = 1` types Constant<1> but the RAW tail is a write,
        // NOT a literal ⇒ computed_literal_tightening ⇒ DROP (no assignment unwrap).
        let asn = candidates_with_sig("asn", "class Foo\n  def m\n    x = 1\n  end\nend\n", "class Foo\n  def m: () -> Integer\nend\n");
        assert!(asn.is_empty(), "assignment tail is not a direct literal: {asn:?}");
    }

    #[test]
    fn env_collection_to_shape_drops() {
        // Declared bare `Array`, inferred `[1, 2]` (Tuple) ⇒
        // narrows_collection_to_shape ⇒ DROP.
        let cs = candidates_with_sig("coll", "class Foo\n  def m\n    [1, 2]\n  end\nend\n", "class Foo\n  def m: () -> Array\nend\n");
        assert!(cs.is_empty(), "collection→shape drop: {cs:?}");
    }

    #[test]
    fn env_unresolvable_declared_returns_drop() {
        // optional / union / untyped / multi-overload / generic-args declared ⇒
        // Declared(None) or not-tighter ⇒ DROP (No candidates).
        for (tag, decl) in [
            ("opt", "def m: () -> String?"),
            ("uni", "def m: () -> (String | Integer)"),
            ("unt", "def m: () -> untyped"),
            ("gen", "def m: () -> Array[Integer]"),
            ("wid", "def m: () -> Integer"), // wider: inferred "hi" is String
        ] {
            let rbs = format!("class Foo\n  {decl}\nend\n");
            let cs = candidates_with_sig(tag, "class Foo\n  def m\n    \"hi\"\n  end\nend\n", &rbs);
            assert!(cs.is_empty(), "{tag}: expected DROP, got {cs:?}");
        }
        // A genuine tightening still emits (declared String, inferred "hi").
        let ok = candidates_with_sig("tight", "class Foo\n  def m\n    \"hi\"\n  end\nend\n", "class Foo\n  def m: () -> String\nend\n");
        assert_eq!(ok[0].classification, "tighter_return");
        assert_eq!(ok[0].declared_return_rbs.as_deref(), Some("String"));
    }

    #[test]
    fn env_incomplete_chain_drops_not_new() {
        // Pitfall g: an unknown superclass (`< Unknown`, no sig) makes the chain
        // incomplete ⇒ DROP (conservative), NOT `# [new]` — a wrong `# [new]` on a
        // method the reference tags `# [tighter]` would be the hard-guarantee break.
        let cs = candidates_with_sig(
            "incomplete",
            "class Foo\n  def zzz\n    \"hi\"\n  end\nend\n",
            "class Foo < Unknown\nend\n",
        );
        assert!(cs.is_empty(), "incomplete-chain drop: {cs:?}");
    }

    #[test]
    fn emits_value_pinned_returns_for_public_instance_methods() {
        let src = "class Foo\n  def greeting\n    \"hello\"\n  end\n\n  def count\n    42\n  end\nend\n";
        let cs = candidates_tagged("emit", src, false);
        let rbs: Vec<&str> = cs.iter().map(|c| c.rbs.as_str()).collect();
        assert_eq!(rbs, vec!["def greeting: () -> \"hello\"", "def count: () -> 42"]);
        assert!(cs.iter().all(|c| c.class_name == "Foo" && c.kind == "instance"));
    }

    #[test]
    fn skips_private_methods_by_default_and_includes_with_flag() {
        let src = "class Foo\n  def pub\n    1\n  end\n\n  private\n\n  def secret\n    2\n  end\nend\n";
        let public_only = candidates_tagged("priv", src, false);
        assert_eq!(public_only.iter().map(|c| c.method_name.as_str()).collect::<Vec<_>>(), vec!["pub"]);
        let with_private = candidates_tagged("priv", src, true);
        assert_eq!(
            with_private.iter().map(|c| c.method_name.as_str()).collect::<Vec<_>>(),
            vec!["pub", "secret"]
        );
    }

    #[test]
    fn skips_complex_parameter_shapes() {
        // A keyword / splat / block param declines (params = None) → skipped.
        let src = "class Foo\n  def kw(a:)\n    1\n  end\n\n  def splat(*a)\n    2\n  end\nend\n";
        assert!(candidates_tagged("cplx", src, false).is_empty());
    }

    #[test]
    fn emits_required_positional_params_as_untyped() {
        let src = "class Foo\n  def add(a, b)\n    1\n  end\nend\n";
        let cs = candidates_tagged("pos", src, false);
        assert_eq!(cs[0].rbs, "def add: (untyped, untyped) -> 1");
    }

    #[test]
    fn nests_qualified_class_name() {
        let src = "module A\n  class B\n    def m\n      1\n    end\n  end\nend\n";
        let cs = candidates_tagged("nest", src, false);
        assert_eq!(cs[0].class_name, "A::B");
    }

    #[test]
    fn skips_untyped_return() {
        // A def-local binding types Dynamic against the top-level env → skipped.
        let src = "class Foo\n  def m(x)\n    x\n  end\nend\n";
        assert!(candidates_tagged("untyped", src, false).is_empty());
    }

    #[test]
    fn unions_explicit_returns_with_tail_in_describe_order() {
        // Oracle-probed matrix (2026-07-10): members sort by describe(:short) —
        // `"s"` < `1` — and the union paren-wraps in method position.
        let src = "class A\n  def m(x)\n    return 1 if x\n    \"s\"\n  end\nend\n";
        let cs = candidates_tagged("union", src, false);
        assert_eq!(cs[0].rbs, "def m: (untyped) -> (\"s\" | 1)");
    }

    #[test]
    fn bare_return_contributes_nil() {
        let src = "class A\n  def m(x)\n    return if x\n    \"s\"\n  end\nend\n";
        let cs = candidates_tagged("bareret", src, false);
        assert_eq!(cs[0].rbs, "def m: (untyped) -> (\"s\" | nil)");
    }

    #[test]
    fn tail_return_types_as_its_value_and_dedups() {
        // A tail `return 42` types 42 (oracle) and dedups against the collected
        // return; a same-value return collapses to the single member.
        let src = "class A\n  def t\n    return 42\n  end\n  def s(x)\n    return 1 if x\n    1\n  end\nend\n";
        let cs = candidates_tagged("tailret", src, false);
        assert_eq!(cs[0].rbs, "def t: () -> 42");
        assert_eq!(cs[1].rbs, "def s: (untyped) -> 1");
    }

    #[test]
    fn block_return_is_barriered_and_multi_return_skips() {
        // A return inside a block is barriered (reference RETURN_BARRIER_NODES —
        // union is tail-only); a multi-value return skips the method (the
        // reference silently drops its type, an unsound emit we do not adopt).
        let src = "class A\n  def b(x)\n    [1].each { return 5 }\n    \"s\"\n  end\n  def m(x)\n    return 1, 2 if x\n    \"s\"\n  end\nend\n";
        let cs = candidates_tagged("blockret", src, false);
        assert_eq!(cs.len(), 1, "only the block-barriered method emits: {cs:?}");
        assert_eq!(cs[0].rbs, "def b: (untyped) -> \"s\"");
    }

    #[test]
    fn nested_source_class_instance_renders_fully_qualified() {
        // A NESTED class's instance return is QUALIFIED to its FQN via Ruby
        // constant resolution from the enclosing scope (`Inner` written in
        // `Outer::Maker` resolves to `Outer::Inner`) — byte-identical to the
        // reference, which names a source nominal fully-qualified.
        let src = "module Outer\n  class Inner\n  end\n  class Maker\n    def make\n      Inner.new\n    end\n  end\nend\n";
        let cs = candidates_tagged("nestcls", src, false);
        assert_eq!(cs.len(), 1, "{cs:?}");
        assert_eq!(cs[0].rbs, "def make: () -> Outer::Inner");
    }

    #[test]
    fn data_define_constant_return_is_fully_qualified() {
        // `Const = Data.define(...)` typed to a source nominal renders the
        // reference's fully-qualified constant name (`Rigor::Triage::Selector`),
        // NOT the written short `Selector`. `Selector` resolves as a source class
        // here only because it collides with a core RBS name; the qualification
        // logic is what this asserts.
        let src = "module Rigor\n  class Triage\n    Selector = Data.define(:a)\n    def make\n      Selector.new(a: 1)\n    end\n  end\nend\n";
        let cs = candidates_tagged("datadef", src, false);
        assert_eq!(cs.len(), 1, "{cs:?}");
        assert_eq!(cs[0].rbs, "def make: () -> Rigor::Triage::Selector");
    }

    #[test]
    fn qualify_source_name_walks_enclosing_scope_outward() {
        let mut fqns = std::collections::HashSet::new();
        fqns.insert("Rigor::Triage".to_string());
        fqns.insert("Rigor::Triage::Selector".to_string());
        fqns.insert("Top".to_string());
        // same-scope constant
        assert_eq!(qualify_source_name("Selector", "Rigor::Triage", &fqns), "Rigor::Triage::Selector");
        // outer sibling: `Triage` referenced from `Rigor::Sibling` → `Rigor::Triage`
        assert_eq!(qualify_source_name("Triage", "Rigor::Sibling", &fqns), "Rigor::Triage");
        // top-level self-reference
        assert_eq!(qualify_source_name("Top", "Top", &fqns), "Top");
        // unknown name is left bare (external / not in file)
        assert_eq!(qualify_source_name("Unknown", "Rigor::Triage", &fqns), "Unknown");
    }

    #[test]
    fn bare_module_function_makes_subsequent_defs_dual() {
        // Position matters: a def BEFORE the bare call stays instance; after it,
        // the def is dual (`def self?.`). Applies in a CLASS body too.
        let src = "module U\n  def before\n    1\n  end\n  module_function\n  def after\n    2\n  end\nend\n";
        let cs = candidates_tagged("mfpos", src, false);
        assert_eq!(
            cs.iter().map(|c| c.rbs.as_str()).collect::<Vec<_>>(),
            vec!["def before: () -> 1", "def self?.after: () -> 2"]
        );
        // Kind stays `instance` — only the rbs prefix changes.
        assert!(cs.iter().all(|c| c.kind == "instance"));

        let clssrc = "class C\n  module_function\n  def helper\n    1\n  end\nend\n";
        let cc = candidates_tagged("mfcls", clssrc, false);
        assert_eq!(cc[0].rbs, "def self?.helper: () -> 1");
    }

    #[test]
    fn module_function_with_args_does_not_flip_mode() {
        // `module_function :sym` (args form) neither flips the mode nor marks the
        // method (oracle-probed) — both defs stay plain instance methods.
        let src = "module N\n  def a\n    4\n  end\n  module_function :a\n  def b\n    5\n  end\nend\n";
        let cs = candidates_tagged("mfargs", src, false);
        assert_eq!(
            cs.iter().map(|c| c.rbs.as_str()).collect::<Vec<_>>(),
            vec!["def a: () -> 4", "def b: () -> 5"]
        );
    }

    #[test]
    fn singleton_prefix_wins_over_module_function() {
        // reference `method_def_prefix` checks singleton FIRST.
        let src = "class C\n  module_function\n  def self.s\n    7\n  end\nend\n";
        let cs = candidates_tagged("mfsing", src, false);
        assert_eq!(cs[0].rbs, "def self.s: () -> 7");
        assert_eq!(cs[0].kind, "singleton");
    }

    #[test]
    fn emits_singleton_methods_both_forms() {
        // `def self.x` and a `class << self` inner def both render `def self.NAME`.
        let src = "class A\n  def self.build\n    \"b\"\n  end\n  class << self\n    def via\n      :s\n    end\n  end\nend\n";
        let cs = candidates_tagged("sing", src, false);
        let rbs: Vec<&str> = cs.iter().map(|c| c.rbs.as_str()).collect();
        assert!(rbs.contains(&"def self.build: () -> \"b\""), "{rbs:?}");
        assert!(rbs.contains(&"def self.via: () -> :s"), "{rbs:?}");
        assert!(cs.iter().all(|c| c.kind == "singleton"));
    }

    #[test]
    fn instance_and_singleton_emit_in_source_order() {
        // `def self.build` (line 2) precedes `def inst` (line 5): source order,
        // not instance-then-singleton.
        let src = "class A\n  def self.build\n    1\n  end\n  def inst\n    2\n  end\nend\n";
        let cs = candidates_tagged("order", src, false);
        assert_eq!(
            cs.iter().map(|c| c.method_name.as_str()).collect::<Vec<_>>(),
            vec!["build", "inst"]
        );
    }

    #[test]
    fn untyped_inside_composite_member_skips() {
        // `[x, 0]` with x untyped erases `[untyped, 0]` — an inference hole the
        // reference reads differently (sweep-proven mismatch source) → skip.
        let src = "class A\n  def m(x)\n    [x, 0]\n  end\nend\n";
        assert!(candidates_tagged("untycomp", src, false).is_empty());
    }

    #[test]
    fn diff_string_emits_header_and_plus_line_per_candidate() {
        let c = |cls: &str, m: &str, rbs: &str| Candidate {
            file: "lib/f.rb".into(),
            class_name: cls.into(),
            method_name: m.into(),
            kind: "instance",
            rbs: rbs.into(),
            inferred_return: String::new(),
            classification: "new_method",
            declared_return_rbs: None,
        };
        let cands = [
            c("Foo", "greeting", "def greeting: () -> \"h\""),
            c("Foo", "build", "def self.build: () -> 42"),
        ];
        assert_eq!(
            diff_string(&cands),
            "--- lib/f.rb: Foo#greeting\n+ def greeting: () -> \"h\"\n\n\
             --- lib/f.rb: Foo#build\n+ def self.build: () -> 42\n\n"
        );
    }

    #[test]
    fn mirror_target_maps_lib_to_sig() {
        let root = Path::new("/proj");
        assert_eq!(mirror_target("lib/foo.rb", "lib", "sig", root), PathBuf::from("/proj/sig/foo.rbs"));
        assert_eq!(
            mirror_target("lib/a/b.rb", "lib", "sig", root),
            PathBuf::from("/proj/sig/a/b.rbs")
        );
        // A path not under the source root keeps its full relative path.
        assert_eq!(mirror_target("app/x.rb", "lib", "sig", root), PathBuf::from("/proj/sig/app/x.rbs"));
    }

    #[test]
    fn target_for_consults_layout_index_before_mirror() {
        let root = Path::new("/proj");
        let mut layout = LayoutIndex { map: HashMap::new() };
        layout.map.insert("Foo".to_string(), PathBuf::from("/proj/sig/consolidated.rbs"));
        // Class in the index → routed to the consolidated file.
        assert_eq!(
            target_for("lib/foo.rb", "Foo", "lib", "sig", root, &layout),
            PathBuf::from("/proj/sig/consolidated.rbs")
        );
        // Class NOT in the index → 1:1 mirror.
        assert_eq!(
            target_for("lib/bar.rb", "Bar", "lib", "sig", root, &layout),
            PathBuf::from("/proj/sig/bar.rbs")
        );
    }

    #[test]
    fn render_new_file_wraps_nested_namespaces_with_kinds_and_super() {
        let cand = |class: &str, rbs: &str| Candidate {
            file: "lib/x.rb".into(),
            class_name: class.into(),
            method_name: "m".into(),
            kind: "instance",
            rbs: rbs.into(),
            inferred_return: String::new(),
            classification: "new_method",
            declared_return_rbs: None,
        };
        let mut info = NamespaceInfo::default();
        info.kinds.insert("Outer".into(), "module");
        info.kinds.insert("Outer::Inner".into(), "class");
        info.supers.insert("Outer::Inner".into(), "Base".into());
        let out = render_new_file(&[cand("Outer::Inner", "def m: () -> :s")], &info);
        assert_eq!(out, "module Outer\n  class Inner < Base\n    def m: () -> :s\n  end\nend\n");
    }

    #[test]
    fn render_new_file_leaf_class_defaults_to_class_keyword() {
        let cand = Candidate {
            file: "lib/x.rb".into(),
            class_name: "Foo".into(),
            method_name: "g".into(),
            kind: "instance",
            rbs: "def g: () -> \"h\"".into(),
            inferred_return: String::new(),
            classification: "new_method",
            declared_return_rbs: None,
        };
        // No kinds recorded → a leaf with methods defaults to `class`.
        let out = render_new_file(&[cand], &NamespaceInfo::default());
        assert_eq!(out, "class Foo\n  def g: () -> \"h\"\nend\n");
    }

    #[test]
    fn dynamic_return_member_skips_method() {
        // `return bar` (an unresolved call → Dynamic) poisons the union →
        // dynamic_top? → skip, matching the reference's untyped-return skip
        // (the Nominal#erase_to_rbs / DiffCommand#run over-emit fix).
        let src = "class A\n  def m(c)\n    return bar if c\n    \"tail\"\n  end\nend\n";
        assert!(candidates_tagged("dynret", src, false).is_empty());
    }

    #[test]
    fn trivial_initialize_is_excluded() {
        // An all-empty-param initialize is EXCLUDED (Object#initialize covers it).
        let src = "class Foo\n  def initialize\n    @x = 1\n  end\nend\n";
        assert!(candidates_tagged("init0", src, false).is_empty());
    }

    #[test]
    fn initialize_stub_renders_full_param_shape_as_void() {
        // Oracle-probed matrix: requireds/optionals/rest/keywords/kwrest/block →
        // the reference's `render_initialize_param_list` spelling, `-> void`.
        let cases = [
            ("class B\n  def initialize(a, b)\n    @a = a\n  end\nend\n", "def initialize: (untyped, untyped) -> void"),
            ("class C\n  def initialize(a, b = 1)\n    @a = a\n  end\nend\n", "def initialize: (untyped, ?untyped) -> void"),
            ("class D\n  def initialize(name:, age: 0)\n    @n = name\n  end\nend\n", "def initialize: (name: untyped, ?age: untyped) -> void"),
            ("class E\n  def initialize(*a, **o, &b)\n    @a = a\n  end\nend\n", "def initialize: (*untyped, **untyped, ?{ (?) -> void }) -> void"),
            ("class F\n  def initialize(a, b = 1, *r, c:, d: 2)\n    @a = a\n  end\nend\n", "def initialize: (untyped, ?untyped, *untyped, c: untyped, ?d: untyped) -> void"),
        ];
        for (i, (src, want)) in cases.iter().enumerate() {
            let cs = candidates_tagged(&format!("initm{i}"), src, false);
            assert_eq!(cs.len(), 1, "case {i}: {cs:?}");
            assert_eq!(&cs[0].rbs, want, "case {i}");
            assert_eq!(cs[0].kind, "instance");
        }
    }

    #[test]
    fn def_self_initialize_is_an_ordinary_singleton() {
        // `def self.initialize` is NOT a constructor — a normal singleton method.
        let src = "class Foo\n  def self.initialize(a)\n    \"x\"\n  end\nend\n";
        let cs = candidates_tagged("initsing", src, false);
        assert_eq!(cs[0].rbs, "def self.initialize: (untyped) -> \"x\"");
    }

    #[test]
    fn emits_sound_project_class_instance_return() {
        // `Bar.new` types as a source-class `Bar` instance (ADR-0023 tier-4). The
        // reference degrades a project-class `.new` to `Dynamic` and skips, but
        // rigor-rs emits the SOUND `-> Bar` — coverage excess we track, not encode
        // (AGENTS.md "Generative-tool parity"; the reference converges as it gains
        // project-instance return typing).
        let src = "class Bar\nend\n\nclass Foo\n  def make\n    Bar.new\n  end\nend\n";
        let cs = candidates_tagged("srccls", src, false);
        let make = cs.iter().find(|c| c.method_name == "make").expect("make emitted");
        assert_eq!(make.rbs, "def make: () -> Bar");
    }

    #[test]
    fn skips_bare_generic_nominal_return() {
        // `[1, 2].map { }` loses the value-pin to a bare `Array` in rigor-rs; the
        // reference would elaborate to `Array[untyped]`, so rigor-rs skips it
        // (FP-safe) rather than emit an under-elaborated `-> Array`.
        let src = "class Foo\n  def mapped\n    [1, 2, 3].map { |x| x }\n  end\nend\n";
        assert!(candidates_tagged("bare", src, false).is_empty());
    }

    #[test]
    fn is_bare_generic_name_only_matches_bare_generics() {
        assert!(is_bare_generic_name("Array"));
        assert!(is_bare_generic_name("Hash"));
        // Value-pinned / parameterised / scalar forms still emit.
        assert!(!is_bare_generic_name("Array[Integer]"));
        assert!(!is_bare_generic_name("[1, 2]"));
        assert!(!is_bare_generic_name("String"));
        assert!(!is_bare_generic_name("42"));
    }

    // -- UPDATE/merge + LayoutIndex ------------------------------------------

    /// A merge-candidate factory for the `apply_merge` tests.
    fn mc(class: &str, method: &str, kind: &'static str, rbs: &str) -> Candidate {
        Candidate {
            file: "lib/f.rb".into(),
            class_name: class.into(),
            method_name: method.into(),
            kind,
            rbs: rbs.into(),
            inferred_return: String::new(),
            classification: "new_method",
            declared_return_rbs: None,
        }
    }

    /// A `tighter_return` candidate (the classifier proved a strict subtype), the
    /// only kind `--overwrite` replaces from generation classification.
    fn mc_tighter(
        class: &str,
        method: &str,
        kind: &'static str,
        rbs: &str,
        declared: &str,
    ) -> Candidate {
        let mut c = mc(class, method, kind, rbs);
        c.classification = "tighter_return";
        c.declared_return_rbs = Some(declared.into());
        c
    }

    #[test]
    fn member_pairs_cover_attr_writer_accessor_and_kind_rules_but_not_alias() {
        // attr_writer → `name=`; attr_accessor → both; alias never counts;
        // def kinds map instance / singleton / singleton_instance.
        let rbs = "class Foo\n  def a: () -> Integer\n  def self.s: () -> String\n  def self?.d: () -> bool\n  attr_reader r: String\n  attr_writer w: Integer\n  attr_accessor acc: String\n  alias al a\nend\n";
        let sig = ruby_rbs::node::parse(rbs).unwrap();
        let RbsNode::Class(c) = sig.declarations().iter().next().unwrap() else { panic!() };
        let pairs = collect_member_pairs(rbs, c.members().iter());
        let got: Vec<(String, &str)> =
            pairs.iter().map(|m| (m.name.clone(), m.kind)).collect();
        assert_eq!(
            got,
            vec![
                ("a".into(), "instance"),
                ("s".into(), "singleton"),
                ("d".into(), "singleton_instance"),
                ("r".into(), "instance"),
                ("w=".into(), "instance"),
                ("acc".into(), "instance"),
                ("acc=".into(), "instance"),
            ],
            "alias `al` must not appear; attr rules applied"
        );
    }

    #[test]
    fn return_text_extraction_takes_last_depth_zero_arrow() {
        assert_eq!(extract_method_return_text("def m: () -> String"), Some("String".into()));
        // A block-typed param carries an inner `->` at depth > 0 — ignored.
        assert_eq!(
            extract_method_return_text("def m: () ?{ () -> void } -> Integer"),
            Some("Integer".into())
        );
        // Union return, wrapped.
        assert_eq!(
            extract_method_return_text("def m: (untyped) -> (\"s\" | 1)"),
            Some("(\"s\" | 1)".into())
        );
        // No arrow → extraction failure.
        assert_eq!(extract_method_return_text("attr_reader name: String"), None);
    }

    #[test]
    fn member_pair_return_text_for_method_and_attr() {
        let rbs = "class Foo\n  def m: (untyped) -> Integer\n  attr_reader r: String?\nend\n";
        let sig = ruby_rbs::node::parse(rbs).unwrap();
        let RbsNode::Class(c) = sig.declarations().iter().next().unwrap() else { panic!() };
        let pairs = collect_member_pairs(rbs, c.members().iter());
        assert_eq!(pairs[0].return_text.as_deref(), Some("Integer"));
        assert_eq!(pairs[1].return_text.as_deref(), Some("String?"));
    }

    #[test]
    fn merge_inserts_new_method_flat_before_end() {
        let src = "class Foo\n  def existing: () -> String\nend\n".to_string();
        let out =
            apply_merge(src, vec![mc("Foo", "newm", "instance", "def newm: () -> 1")], &HashMap::new(), false);
        assert_eq!(out.action, "updated");
        assert_eq!(out.applied.len(), 1);
        assert_eq!(
            out.source,
            "class Foo\n  def existing: () -> String\n  def newm: () -> 1\nend\n"
        );
    }

    #[test]
    fn merge_inserts_new_method_nested_reproduces_indent_quirk() {
        // The token-start splice + fixed 2-space indent: the inserted line renders
        // 4-space and the inner `end` drops to column 0 (oracle-verified).
        let src = "module Outer\n  class Inner\n    def existing: () -> String\n  end\nend\n"
            .to_string();
        let out = apply_merge(
            src,
            vec![mc("Outer::Inner", "newm", "instance", "def newm: () -> 5")],
            &HashMap::new(),
            false,
        );
        assert_eq!(
            out.source,
            "module Outer\n  class Inner\n    def existing: () -> String\n    def newm: () -> 5\nend\nend\n"
        );
    }

    #[test]
    fn merge_equivalent_conflict_drops_silently() {
        // Same declared return → dropped: not applied, not skipped → noop.
        let src = "class Foo\n  def greeting: () -> \"hi\"\nend\n".to_string();
        let out = apply_merge(
            src.clone(),
            vec![mc("Foo", "greeting", "instance", "def greeting: () -> \"hi\"")],
            &HashMap::new(),
            false,
        );
        assert_eq!(out.action, "noop");
        assert!(out.applied.is_empty() && out.skipped.is_empty());
        assert_eq!(out.source, src, "byte-untouched when everything drops");
    }

    #[test]
    fn merge_different_conflict_skips_user_authored_with_declared_return() {
        let src = "class Foo\n  def greeting: () -> String\nend\n".to_string();
        let out = apply_merge(
            src.clone(),
            vec![mc("Foo", "greeting", "instance", "def greeting: () -> \"hi\"")],
            &HashMap::new(),
            false,
        );
        assert_eq!(out.action, "noop"); // nothing applied
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].classification, "tighter_return");
        assert_eq!(out.skipped[0].declared_return_rbs.as_deref(), Some("String"));
        assert_eq!(out.source, src);
    }

    #[test]
    fn overwrite_replaces_tighter_conflict_in_place_and_applies_it() {
        // Under --overwrite a tighter_return conflict REPLACES the declared line,
        // moves to `applied`, and leaves 0 skipped (reference `apply_replacement`).
        let src = "class Foo\n  def greeting: () -> String\nend\n".to_string();
        let out = apply_merge(
            src,
            vec![mc_tighter("Foo", "greeting", "instance", "def greeting: () -> \"hi\"", "String")],
            &HashMap::new(),
            true,
        );
        assert_eq!(out.action, "updated");
        assert_eq!(out.applied.len(), 1);
        assert!(out.skipped.is_empty());
        assert_eq!(out.source, "class Foo\n  def greeting: () -> \"hi\"\nend\n");
    }

    #[test]
    fn overwrite_replaces_multiple_tighter_conflicts_offsets_stay_valid() {
        // Replacements apply highest-offset-first so earlier spans stay valid even
        // as line lengths change; all three land byte-correctly.
        let src =
            "class Foo\n  def a: () -> String\n  def b: () -> String\n  def c: () -> String\nend\n"
                .to_string();
        let out = apply_merge(
            src,
            vec![
                mc_tighter("Foo", "a", "instance", "def a: () -> \"aa\"", "String"),
                mc_tighter("Foo", "b", "instance", "def b: () -> \"bb\"", "String"),
                mc_tighter("Foo", "c", "instance", "def c: () -> \"cc\"", "String"),
            ],
            &HashMap::new(),
            true,
        );
        assert_eq!(out.applied.len(), 3);
        assert_eq!(
            out.source,
            "class Foo\n  def a: () -> \"aa\"\n  def b: () -> \"bb\"\n  def c: () -> \"cc\"\nend\n"
        );
    }

    #[test]
    fn overwrite_off_still_preserves_tighter_conflict_as_skipped() {
        // Without --overwrite the same candidate is preserved (byte-untouched).
        let src = "class Foo\n  def greeting: () -> String\nend\n".to_string();
        let out = apply_merge(
            src.clone(),
            vec![mc_tighter("Foo", "greeting", "instance", "def greeting: () -> \"hi\"", "String")],
            &HashMap::new(),
            false,
        );
        assert_eq!(out.action, "noop");
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.source, src);
    }

    #[test]
    fn overwrite_new_method_only_replaces_when_it_removes_an_untyped_slot() {
        // A new_method conflict is eligible ONLY when its RBS has strictly fewer
        // `untyped` tokens than the existing decl (reference `tightens_untyped?`).
        // Tightening: existing `(untyped) -> void`, candidate `(String) -> void`.
        let src = "class Foo\n  def initialize: (untyped) -> void\nend\n".to_string();
        let tighten = apply_merge(
            src.clone(),
            vec![mc("Foo", "initialize", "instance", "def initialize: (String) -> void")],
            &HashMap::new(),
            true,
        );
        assert_eq!(tighten.applied.len(), 1, "removes one untyped ⇒ replaced");
        assert_eq!(tighten.source, "class Foo\n  def initialize: (String) -> void\nend\n");

        // Not tightening: same untyped count ⇒ preserved, not replaced.
        let same = apply_merge(
            src.clone(),
            vec![mc("Foo", "initialize", "instance", "def initialize: (untyped) -> void")],
            &HashMap::new(),
            true,
        );
        assert_eq!(same.action, "noop", "equal untyped count + equal return ⇒ drop");
        assert_eq!(same.source, src);
    }

    #[test]
    fn count_untyped_is_word_boundary_matched() {
        assert_eq!(count_untyped("(untyped, untyped) -> void"), 2);
        assert_eq!(count_untyped("() -> void"), 0);
        // `untyped` inside an identifier is not a type token.
        assert_eq!(count_untyped("(my_untyped_thing) -> untyped"), 1);
    }

    #[test]
    fn merge_instance_and_singleton_are_distinct_identities() {
        // An existing INSTANCE `def build` does NOT block a SINGLETON candidate.
        let src = "class Foo\n  def build: () -> String\nend\n".to_string();
        let out = apply_merge(
            src,
            vec![mc("Foo", "build", "singleton", "def self.build: () -> 1")],
            &HashMap::new(),
            false,
        );
        assert_eq!(out.action, "updated");
        assert_eq!(
            out.source,
            "class Foo\n  def build: () -> String\n  def self.build: () -> 1\nend\n"
        );
    }

    #[test]
    fn merge_attr_reader_blocks_matching_method_candidate() {
        let src = "class Foo\n  attr_reader name: String\nend\n".to_string();
        let out = apply_merge(
            src.clone(),
            vec![mc("Foo", "name", "instance", "def name: () -> \"n\"")],
            &HashMap::new(),
            false,
        );
        // attr_reader name: String vs candidate "n" → different → skipped.
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].declared_return_rbs.as_deref(), Some("String"));
        assert_eq!(out.source, src);
    }

    #[test]
    fn append_new_class_compact_header_and_leading_blank() {
        let src = "class Foo\n  def existing: () -> String\nend\n".to_string();
        let mut supers = HashMap::new();
        supers.insert("Bar".to_string(), "Base".to_string());
        let out = apply_merge(
            src,
            vec![mc("Bar", "added", "instance", "def added: () -> 7")],
            &supers,
            false,
        );
        assert_eq!(out.action, "updated");
        assert_eq!(
            out.source,
            "class Foo\n  def existing: () -> String\nend\n\nclass Bar < Base\n  def added: () -> 7\nend\n"
        );
    }

    #[test]
    fn append_new_class_qualified_name_stays_compact() {
        // A class not in the file with a qualified name uses `class A::B`, NOT
        // nested modules.
        let src = "class Foo\nend\n".to_string();
        let out = apply_merge(
            src,
            vec![mc("A::B", "m", "instance", "def m: () -> 1")],
            &HashMap::new(),
            false,
        );
        assert_eq!(out.source, "class Foo\nend\n\nclass A::B\n  def m: () -> 1\nend\n");
    }

    #[test]
    fn append_repairs_missing_trailing_newline() {
        let src = "class Foo\nend".to_string(); // no trailing newline
        let out = apply_merge(
            src,
            vec![mc("Bar", "m", "instance", "def m: () -> 1")],
            &HashMap::new(),
            false,
        );
        assert_eq!(out.source, "class Foo\nend\n\nclass Bar\n  def m: () -> 1\nend\n");
    }

    #[test]
    fn malformed_target_is_noop_and_untouched() {
        let src = "class Foo\n  def existing: (( -> \nend\n".to_string();
        let out = apply_merge(
            src.clone(),
            vec![mc("Foo", "newm", "instance", "def newm: () -> 1")],
            &HashMap::new(),
            false,
        );
        assert_eq!(out.action, "noop");
        assert!(out.applied.is_empty() && out.skipped.is_empty());
        assert_eq!(out.source, src);
    }

    #[test]
    fn layout_index_first_found_wins_and_skips_parse_failures() {
        let dir = std::env::temp_dir().join(format!("rigor_layout_{}", std::process::id()));
        let sig = dir.join("sig");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(sig.join("nested")).unwrap();
        // Two files declare Foo; sorted walk means `a.rbs` (< `z.rbs`) wins.
        std::fs::write(sig.join("a.rbs"), "class Foo\nend\n").unwrap();
        std::fs::write(sig.join("z.rbs"), "class Foo\nend\nclass Bar\nend\n").unwrap();
        // A nested, consolidated declaration is indexed by its FQN.
        std::fs::write(sig.join("nested/x.rbs"), "module M\n  class Inner\n  end\nend\n").unwrap();
        // A malformed file is skipped silently (its Baz never appears).
        std::fs::write(sig.join("broken.rbs"), "class Baz (( bad\n").unwrap();

        let layout = LayoutIndex::build(&["sig".to_string()], &dir);
        assert_eq!(layout.file_for("Foo"), Some(&sig.join("a.rbs")));
        assert_eq!(layout.file_for("Bar"), Some(&sig.join("z.rbs")));
        assert_eq!(layout.file_for("M::Inner"), Some(&sig.join("nested/x.rbs")));
        assert_eq!(layout.file_for("Baz"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
