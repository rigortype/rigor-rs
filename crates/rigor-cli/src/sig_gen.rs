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
//!   array, a project-class `.new` → its instance, a partially-`untyped` shape).
//!   There rigor-rs emits a SOUND signature the reference skips — that excess is
//!   coverage, NOT a false bug report, and we TRACK it (the reference converges as
//!   it gains precision) rather than suppress it with anti-convergence guards.
//!
//! The only guards are the three AGENTS.md sanctions: fix a rigor-rs UNSOUND emit
//! (`initialize` typed as its body → skip; the reference's `-> void` stub is a
//! later slice), match a reference PERMANENT skip (`dynamic_top?`'s whole-`untyped`
//! return), or avoid a WRONG emit from an unported rigor-rs LIMITATION (a bare
//! generic nominal the reference *elaborates* to `Array[untyped]`, and an explicit
//! `return` whose union rigor-rs cannot yet reconstruct from the AST).
//!
//! ## Deferred (later slices, each its own gate)
//!
//! - `--diff` / `--write` (the `Writer`), `--format json` write-report;
//! - `--params=observed` (the `ObservationCollector`) — params stay `untyped`;
//! - singleton (`def self.x` / `class << self`), `module_function`,
//!   `Const = Data.define(...)` class shells, `attr_*` reader candidates;
//! - `TIGHTER_RETURN` / `EQUIVALENT` classification against existing project RBS
//!   (a method that already resolves to an RBS declaration is OMITTED — the
//!   reference emits `tighter-return`; omitting it is FP-safe, a coverage gap);
//! - `TypeElaborator`'s generic-arity fill (`Array` → `Array[untyped]`): a bare
//!   GENERIC nominal return is skipped rather than under-elaborated (FP-safe);
//! - the explicit-`return` union (`DefReturnTyper#union_with_explicit_returns`):
//!   a method with any `return E` is skipped (rigor-rs's AST keeps only the
//!   `has_explicit_return` flag, not the return expressions) — FP-safe.

use std::path::Path;
use std::process::ExitCode;

use rigor_index::CoreIndex;
use rigor_infer::{SourceIndex, TypeEnv, Typer};
use rigor_parse::{lower, parse, LoweredAst, MethodBody, Node, NodeId, Visibility};
use rigor_types::{Interner, Type, TypeId};

/// One printable RBS skeleton row (the reference's emittable `MethodCandidate`,
/// always `NEW_METHOD` in the `--print` path — `NEW_FILE` is a `--write` concept).
#[derive(Debug)]
struct Candidate {
    file: String,
    class_name: String,
    method_name: String,
    /// `"instance"` — singleton methods are a deferred slice.
    kind: &'static str,
    /// The rendered one-liner, e.g. `def greeting: () -> "hello"`.
    rbs: String,
    /// The raw inferred return erased to RBS (the JSON `inferred_return` field).
    inferred_return: String,
}

/// `rigor sig-gen [--print] [--format text|json] [--include-private] [--config PATH] [paths]`.
/// Exit 0 on success, 64 on a usage error, 2 for a not-yet-ported mode.
pub fn cmd_sig_gen(args: &[String]) -> ExitCode {
    let mut format = "text";
    let mut include_private = false;
    let mut explicit_config: Option<&str> = None;
    let mut positional: Vec<&str> = Vec::new();

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--print" => {} // the only supported mode (default)
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
            // Recognised reference flags whose machinery is a later slice.
            "--diff" | "--write" | "--overwrite" => {
                eprintln!(
                    "sig-gen: `{arg}` is not yet implemented in this slice (only --print)"
                );
                return ExitCode::from(2);
            }
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

    let candidates: Vec<Candidate> =
        files.iter().flat_map(|p| generate_file(p, include_private)).collect();

    match format {
        "json" => render_json(&candidates),
        _ => render_text(&candidates),
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

/// Produce the printable candidates for one source file. A parse/read failure
/// (or a file with no reachable named class body) yields no candidates.
fn generate_file(path: &str, include_private: bool) -> Vec<Candidate> {
    let Ok(source) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let ast = lower(&parse(source.as_bytes()));
    // Core-only env: the existing-project-RBS comparison (tighter/equivalent) is
    // a deferred slice, so every qualifying def is a fresh `NEW_METHOD`.
    let index = CoreIndex::new();
    let source_index = SourceIndex::build(&ast, &index);
    let typer = Typer::with_source(&index, &source_index);
    let mut interner = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut interner);

    let mut out = Vec::new();
    let root = ast.root();
    if let Node::Program { body, .. } = ast.get(root) {
        for &child in body {
            walk_namespace(
                &ast, child, &[], path, include_private, &index, &typer, &env, &mut interner, &mut out,
            );
        }
    }
    out
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

    for method in method_bodies {
        if let Some(candidate) = method_candidate(
            ast, method, visibilities, &class_name, path, include_private, index, typer, env, interner,
        ) {
            out.push(candidate);
        }
    }

    // Descend into nested class/module declarations in this body.
    for &child in body {
        if matches!(ast.get(child), Node::ClassDef { .. } | Node::ModuleDef { .. }) {
            walk_namespace(ast, child, &qualified, path, include_private, index, typer, env, interner, out);
        }
    }
}

/// Classify + render one instance method, or `None` when it is skipped
/// (private/protected without `--include-private`, a non-simple parameter shape,
/// or an `untyped` / `Dynamic[top]` inferred return).
#[allow(clippy::too_many_arguments)]
fn method_candidate(
    ast: &LoweredAst,
    method: &MethodBody,
    visibilities: &[(String, Visibility)],
    class_name: &str,
    path: &str,
    include_private: bool,
    index: &CoreIndex,
    typer: &Typer,
    env: &TypeEnv,
    interner: &mut Interner,
) -> Option<Candidate> {
    // Visibility: skip private / protected unless `--include-private`
    // (reference `visibility_excludes?`; instance methods only in this slice).
    if !include_private {
        if let Some((_, vis)) = visibilities.iter().find(|(n, _)| n == &method.name) {
            if matches!(vis, Visibility::Private | Visibility::Protected) {
                return None;
            }
        }
    }

    // Simple parameter shape: rigor-rs sets `params = None` for exactly the
    // splat/post/kwargs/block/optional forms the reference's
    // `simple_parameter_shape?` rejects. Only plain requireds qualify.
    let arity = method.params.as_ref()?.len();

    // `initialize` is special: the reference emits it (when it has non-trivial
    // params) as a `-> void` constructor STUB, never the inferred body type, and
    // excludes the trivial no-arg form. rigor-rs types the body tail (e.g. an
    // `@m = Mutex.new` → `Mutex`), which is WRONG for a constructor. Skip it here
    // (the void-stub is a later slice) — FP-safe.
    if method.name == "initialize" {
        return None;
    }

    // Explicit-return union (reference `DefReturnTyper#union_with_explicit_returns`):
    // the return type is `union(tail, every `return E` type)`. rigor-rs's lowering
    // records only the FLAG `has_explicit_return`, not the return EXPRESSIONS, so
    // it cannot reconstruct that union — and a `return E` whose `E` is Dynamic (an
    // ivar/param read) would make the reference's union erase to `untyped` and be
    // SKIPPED. Since rigor-rs would otherwise see only the concrete tail and
    // over-emit (the `Nominal#erase_to_rbs` / `DiffCommand#run` cases), any method
    // with an explicit return is skipped here — FP-safe (a coverage gap). Typing
    // the return expressions is a later slice (needs them preserved in the AST).
    if method.has_explicit_return {
        return None;
    }

    // Return inference (reference `DefReturnTyper` — the shared `annotate` logic):
    // the last statement's type, an assignment tail evaluating to its RHS.
    let ret_ty = def_return_type(ast, typer, &method.body, env, interner)?;
    let erased = crate::type_display::erase(interner, index, typer.source(), ret_ty);

    // Skip a WHOLE-`untyped` / `Top` / `Dynamic` return (reference `dynamic_top?`,
    // a PERMANENT design skip): emitting `-> untyped` obscures rather than helps.
    // A PARTIALLY-`untyped` shape (`Hash[String, untyped]`, `{ k: untyped }`) is a
    // SOUND signature — rigor-rs emits it even though the reference degrades that
    // body to whole-`untyped` and skips (a reference inference GAP we track, not
    // encode; AGENTS.md "Generative-tool parity").
    if erased == "untyped" || matches!(interner.get(ret_ty), Type::Top | Type::Dynamic(_)) {
        return None;
    }
    // A bare GENERIC nominal (`Array` / `Hash` / …) would be `Array[untyped]`
    // after the reference's `TypeElaborator` fill (deferred here), so skip it
    // rather than emit an under-elaborated form (FP-safe coverage gap). Checked on
    // the ERASED string (a value-pinned `Array[Integer]` / `[1, 2]` carries a
    // bracket and is NOT bare, so it still emits).
    if is_bare_generic_name(&erased) {
        return None;
    }

    let head = if arity == 0 {
        "()".to_string()
    } else {
        format!("({})", vec!["untyped"; arity].join(", "))
    };
    let ret = paren_wrap_union(&erased);
    let rbs = format!("def {}: {head} -> {ret}", method.name);

    Some(Candidate {
        file: path.to_string(),
        class_name: class_name.to_string(),
        method_name: method.name.clone(),
        kind: "instance",
        rbs,
        inferred_return: erased,
    })
}

/// A method's inferred return type, or `None` for an empty body (reference
/// `DefReturnTyper`): the last statement's type, an assignment tail evaluating to
/// its RHS value. Typed against the top-level env — a def-LOCAL binding types
/// `Dynamic` (the documented `annotate` deferral) and is then skipped upstream.
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
                println!("  # [new]");
                println!("  {}", c.rbs);
            }
            println!("end");
        }
        println!();
    }
}

/// `--print --format json`: `{ "candidates": [ … ] }` with the reference's
/// per-candidate key set (`file`/`class`/`method`/`kind`/`classification`/`rbs`/
/// `inferred_return`). serde alphabetizes keys (the established insignificant-
/// order divergence).
fn render_json(candidates: &[Candidate]) {
    use serde_json::json;
    let rows: Vec<_> = candidates
        .iter()
        .map(|c| {
            json!({
                "file": c.file,
                "class": c.class_name,
                "method": c.method_name,
                "kind": c.kind,
                "classification": "new_method",
                "rbs": c.rbs,
                "inferred_return": c.inferred_return,
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&json!({ "candidates": rows })).unwrap());
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
        let out = generate_file(file.to_str().unwrap(), include_private);
        let _ = std::fs::remove_file(&file);
        out
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
    fn skips_methods_with_explicit_return() {
        // An explicit `return` means the true return type is a union rigor-rs
        // cannot reconstruct (the return expressions are not in the AST), so the
        // method is skipped — matching the reference skipping `Nominal#erase_to_rbs`
        // (which has `return class_name`).
        let src = "class Foo\n  def m\n    return bar if cond\n    \"tail\"\n  end\nend\n";
        assert!(candidates_tagged("expret", src, false).is_empty());
    }

    #[test]
    fn skips_initialize_constructor() {
        // `initialize` types to its body tail (`@m = X.new` → `X`), which is
        // WRONG for a constructor; the reference emits a `-> void` stub instead.
        // Skip it — FP-safe.
        let src = "class Foo\n  def initialize\n    @x = 1\n  end\nend\n";
        assert!(candidates_tagged("init", src, false).is_empty());
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
}
