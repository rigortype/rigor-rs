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
//! *elaborates* to `Array[untyped]`; a NESTED source-class instance the
//! reference names fully-qualified — rigor-rs has only the written short name).
//!
//! ## Deferred (later slices, each its own gate)
//!
//! - `--diff` / `--write` (the `Writer`), `--format json` write-report;
//! - `--params=observed` (the `ObservationCollector`) — params stay `untyped`;
//! - singleton (`def self.x` / `class << self`), `module_function` `self?.`
//!   spelling, `Const = Data.define(...)` class shells, `attr_*` readers;
//! - `TIGHTER_RETURN` / `EQUIVALENT` classification against existing project RBS
//!   (a method that already resolves to an RBS declaration is OMITTED — the
//!   reference emits `tighter-return`; omitting it is FP-safe, a coverage gap);
//! - `TypeElaborator`'s generic-arity fill (`Array` → `Array[untyped]`);
//! - QUALIFIED source-class naming (`Rigor::Plugin::ProtocolContract`) — unlocks
//!   the nested source-class returns skipped above.

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

    // Written names of NESTED class/module declarations (non-empty lexical
    // prefix). rigor-rs's SourceIndex types a project `X.new` under the WRITTEN
    // short name, but the reference emits the FULLY-QUALIFIED name
    // (`Rigor::Plugin::ProtocolContract`) — so a nested source-class member
    // would byte-diverge on a shared method. Top-level classes' written name IS
    // the qualified name (oracle-probed byte-identical), so only nested ones
    // must skip until qualified source-class naming is ported.
    let mut nested_classes: std::collections::HashSet<String> = std::collections::HashSet::new();
    let root = ast.root();
    if let Node::Program { body, .. } = ast.get(root) {
        for &child in body {
            collect_nested_class_names(&ast, child, false, &mut nested_classes);
        }
    }

    let mut out = Vec::new();
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
                &nested_classes,
                &mut interner,
                &mut out,
            );
        }
    }
    out
}

/// Collect the written names of class/module declarations that are lexically
/// NESTED (inside another class/module). `inside` is true once we are within any
/// namespace body.
fn collect_nested_class_names(
    ast: &LoweredAst,
    id: NodeId,
    inside: bool,
    out: &mut std::collections::HashSet<String>,
) {
    let (name, body) = match ast.get(id) {
        Node::ClassDef { name, body, .. } | Node::ModuleDef { name, body, .. } => (name, body),
        _ => return,
    };
    if inside {
        out.insert(name.clone());
    }
    for &child in body {
        collect_nested_class_names(ast, child, true, out);
    }
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
    nested_classes: &std::collections::HashSet<String>,
    interner: &mut Interner,
    out: &mut Vec<Candidate>,
) {
    let (name, method_bodies, visibilities, body, is_module) = match ast.get(id) {
        Node::ClassDef { name, method_bodies, method_visibilities, body, .. } => {
            (name, method_bodies, method_visibilities, body, false)
        }
        Node::ModuleDef { name, method_bodies, method_visibilities, body, .. } => {
            (name, method_bodies, method_visibilities, body, true)
        }
        _ => return,
    };

    let mut qualified = prefix.to_vec();
    qualified.push(name.clone());
    let class_name = qualified.join("::");

    // A module with a bare `module_function` call: the reference emits its
    // methods as `def self?.name` (the dual instance+singleton RBS spelling —
    // `@module_function_methods` in the reference walker). rigor-rs's lowering
    // does not track module_function state, so emitting `def name` here would
    // byte-diverge on a shared method — skip the whole module's methods until
    // the `self?.` spelling is ported (FP-safe under-emit; nested namespaces
    // still descend).
    let module_function_active = is_module
        && body.iter().any(|&c| {
            matches!(ast.get(c), Node::Call { method, receiver: None, .. } if method == "module_function")
        });

    if !module_function_active {
        for method in method_bodies {
            if let Some(candidate) = method_candidate(
                ast,
                method,
                visibilities,
                &class_name,
                path,
                include_private,
                index,
                typer,
                env,
                nested_classes,
                interner,
            ) {
                out.push(candidate);
            }
        }
    }

    // Descend into nested class/module declarations in this body.
    for &child in body {
        if matches!(ast.get(child), Node::ClassDef { .. } | Node::ModuleDef { .. }) {
            walk_namespace(
                ast,
                child,
                &qualified,
                path,
                include_private,
                index,
                typer,
                env,
                nested_classes,
                interner,
                out,
            );
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
    nested_classes: &std::collections::HashSet<String>,
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
    let returns = collect_explicit_returns(ast, method)?;

    // Tail type (reference `body_last_expression` + `safe_type_of`): the last
    // statement's type; an assignment tail evaluates to its RHS; a `return E`
    // tail evaluates to its value (`nil` when bare).
    let tail_ty = def_return_type(ast, typer, &method.body, env, interner)?;

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
    // NOT the erased form), then erase each member.
    members.sort_by_key(|&m| crate::type_display::describe(interner, index, typer.source(), m));
    let mut erased_members: Vec<String> = Vec::new();
    for &m in &members {
        let e = crate::type_display::erase(interner, index, typer.source(), m);
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
        // A NESTED source-class instance renders its WRITTEN short name here but
        // the reference emits the fully-qualified name — skip until qualified
        // source-class naming is ported (top-level classes match byte-for-byte
        // and still emit).
        if nested_classes.contains(&e) {
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
fn collect_explicit_returns(ast: &LoweredAst, method: &MethodBody) -> Option<Vec<Option<NodeId>>> {
    if !method.has_explicit_return {
        return Some(Vec::new());
    }

    let regions: Vec<(usize, usize)> =
        method.body.iter().map(|&id| ast.get(id).span()).collect();
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
    fn nested_source_class_instance_skips_but_top_level_emits() {
        // A NESTED class's instance renders its written short name (`Inner`) but
        // the reference emits the qualified `Outer::Inner` — skip. A TOP-LEVEL
        // class's written name IS qualified — emit (oracle byte-identical).
        let src = "module Outer\n  class Inner\n  end\n  class Maker\n    def make\n      Inner.new\n    end\n  end\nend\n";
        assert!(candidates_tagged("nestcls", src, false).is_empty());
    }

    #[test]
    fn module_function_module_skips_methods() {
        // The reference spells a module_function module's methods `def self?.m`;
        // rigor-rs doesn't track module_function yet → skip the module's methods.
        let src = "module Util\n  module_function\n\n  def helper\n    1\n  end\nend\n";
        assert!(candidates_tagged("modfunc", src, false).is_empty());
    }

    #[test]
    fn untyped_inside_composite_member_skips() {
        // `[x, 0]` with x untyped erases `[untyped, 0]` — an inference hole the
        // reference reads differently (sweep-proven mismatch source) → skip.
        let src = "class A\n  def m(x)\n    [x, 0]\n  end\nend\n";
        assert!(candidates_tagged("untycomp", src, false).is_empty());
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
