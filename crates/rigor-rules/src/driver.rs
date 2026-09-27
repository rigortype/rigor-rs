//! The single converged walk (ADR-0005): `analyze*` builds the typer and the
//! envs once and runs every rule pass over the file.

use rigor_index::CoreIndex;
use rigor_infer::Typer;
use rigor_parse::{LoweredAst, Node};
use rigor_types::Interner;

use crate::{
    arg_is_pure_nil, check_always_raises, check_always_truthy, check_argument_type_mismatch,
    check_call, check_collection_call, check_narrowed_call, check_nil_receiver,
    check_override_visibility, check_unreachable_branch, check_wrong_arity, dead_assignments_in_def,
    duplicate_hash_key_diagnostics, ivar_write_mismatch_diagnostics, qualified_class_names,
    raise_non_exception_diagnostics, return_in_ensure_diagnostics, unresolved_toplevel_diagnostics,
    Diagnostic, ScopedEnv,
};

// ---------------------------------------------------------------------------
// analyze()
// ---------------------------------------------------------------------------

/// Analyze a lowered AST and return all diagnostics, in source order.
///
/// This is the single converged walk (ADR-0005): it builds the top-level type
/// environment once, then visits every node, applying every call rule
/// (`call.undefined-method`, `call.wrong-arity`, `call.possible-nil-receiver`)
/// in the SAME pass. At most one diagnostic is emitted per call site, matching
/// the reference's one-diagnostic-per-offending-call discipline.
pub fn analyze(ast: &LoweredAst, interner: &mut Interner, index: &CoreIndex) -> Vec<Diagnostic> {
    // Single-file API: build a per-file source index then delegate. Preserves the
    // existing signature + tests. The project pass (the CLI) builds ONE
    // project-wide source over all files and calls `analyze_with_source` directly.
    let source = rigor_infer::SourceIndex::build(ast, index);
    analyze_with_source(ast, interner, index, &source)
}

/// Analyze a lowered AST against an EXTERNALLY-built [`SourceIndex`] — the
/// project-wide variant the CLI builds once over every file. Splitting this out
/// lets the bare-constant singleton gate (`!source.knows_class(name)`) see class
/// names defined in OTHER files, so a project model referenced in a file that
/// does not define it (`Group.where(...)`) is never singleton-typed and stays
/// silent (the cross-file zero-FP keystone).
pub fn analyze_with_source(
    ast: &LoweredAst,
    interner: &mut Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
) -> Vec<Diagnostic> {
    analyze_with_source_and_folder(ast, interner, index, source, None)
}

/// As [`analyze_with_source`], plus the optional ADR-0008 real-Ruby folder for
/// sidecar-routed constant folds (full-fidelity mode). `folder = None` is
/// byte-identical to [`analyze_with_source`] (the sound subset). The folder must
/// be `Sync` — one instance is shared across the file-parallel analysis.
pub fn analyze_with_source_and_folder(
    ast: &LoweredAst,
    interner: &mut Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    folder: Option<&(dyn rigor_infer::RubyFolder + Sync)>,
) -> Vec<Diagnostic> {
    // A typer over the real RBS index AND the (project-wide) source index, so
    // `X.new` types to an instance and a bare constant `X` types to its class
    // object (`Singleton(X)`) for class-method witnessing. The source index also
    // drives RETURN-TYPE inference for chaining. The folder (if wired) lets a
    // sidecar-foldable literal call the Rust core declined resolve to a `Constant`.
    // C1: attach the current file's lexical class/module scopes so the typer's
    // `ConstantRead` arm resolves each use site's lexical prefix (span
    // containment) and applies the precise constant-shadow gate.
    let scopes = rigor_infer::lexical_scopes(ast);
    let typer = Typer::with_source_and_folder(index, source, folder)
        .with_lexical_scopes(&scopes)
        .with_file_key(ast.file_key());
    let env = ScopedEnv::build(&typer, ast, interner);
    // ADR-0038 Slice 1: the per-call nil-receiver snapshot map (call node id ->
    // non-nil core arm), computed ONCE over the whole program via the threaded
    // flow-eval. `check_nil_receiver` fires from it (block / top-level scopes,
    // not only inside a named `def`).
    let nil_snaps = typer.nilable_receiver_snapshots(ast, interner);
    // Class-narrowing snapshot map (census mechanism 1): call node id -> class
    // name `C` the receiver local was `is_a?`/`case-when` narrowed to (from
    // `Dynamic`/`Top` only). `check_narrowed_call` fires `call.undefined-method`
    // from it — and ONLY that rule (spec pitfall 7: a narrowed receiver must not
    // become witnessable by wrong-arity/ATM in this slice).
    // …and, from the SAME pass, the disjoint-guard suppression set: call node
    // ids whose bare-local receiver the reference collapsed to `Bot`
    // (docs/notes/20260808-disjoint-guard-suppression.md). `Bot` has no dispatch
    // surface, so the reference emits NOTHING at those sites — measured across
    // `undefined-method`, `wrong-arity` and `argument-type-mismatch` — and the
    // whole call site is skipped rather than one rule.
    let narrowing = typer.class_narrowing_pass(ast, interner);
    let class_snaps = narrowing.calls;
    let dead_calls = narrowing.dead;
    // Collection-shape snapshot map (collection-shape slice, stage 1): call node
    // id -> "Array"/"Hash", the class a bare-local receiver's collection carrier
    // (literal seed, mutator-widened kept nominal, or tier-folded chain result)
    // dispatches on. `check_collection_call` fires `call.undefined-method` from
    // it — and ONLY that rule, same envelope as the class-narrowing map.
    let coll_snaps = typer.collection_shape_snapshots(ast, interner);
    let mut out = Vec::new();

    // Visit nodes in id order, which is source-discovery order, so diagnostics
    // come out deterministically (ADR-0020 determinism).
    let calls: Vec<_> = ast
        .iter()
        .filter_map(|(id, node)| match node {
            Node::Call {
                receiver: Some(recv),
                method,
                args,
                block_body,
                message_span,
                safe_nav,
                args_all_plain,
                args_plain_positional,
                ..
            } => Some((
                id,
                *recv,
                method.clone(),
                args.clone(),
                !block_body.is_empty(),
                *message_span,
                *safe_nav,
                *args_all_plain,
                *args_plain_positional,
            )),
            _ => None,
        })
        .collect();

    for (call_id, recv, method, args, has_block, message_span, safe_nav, args_all_plain, args_plain_positional) in calls {
        // DEAD RECEIVER (disjoint-guard suppression): the reference's guarded
        // scope binds this receiver to `Bot`, which responds to no method and
        // carries no signature, so no receiver-driven rule of the reference can
        // fire here — `undefined-method`, `wrong-arity` and
        // `argument-type-mismatch` are each measured silent inside the branch
        // while an unguarded control fires. Skipping the whole site (including
        // the independent argument-type axis below) is what the oracle shows;
        // calls whose receiver is a DIFFERENT local, and a call nested in this
        // one's ARGUMENTS, keep their own node ids and are untouched (probes
        // `scope_other_local`, `nest_arg_other`).
        if dead_calls.contains(&call_id) {
            continue;
        }
        // Rule precedence at one call site (avoid double-emit):
        //   1. undefined-method  (method absent on the receiver class, incl. nil)
        //   2. wrong-arity       (method present but arg count out of envelope)
        //   3. possible-nil-receiver (union receiver with a nil arm)
        // The reference emits exactly one of these per call; we mirror that by
        // returning the first that fires.
        // Ruby method bodies are independent local scopes, so a use site inside a
        // `def` never reads the file's top-level locals (`ScopedEnv::at`).
        let gate_env = env.gate_at(message_span);
        let env = env.at(message_span);
        // `nil&.m` never dispatches: the reference's `safe_navigation_receiver`
        // turns a receiver that is exactly nil into `bot` for undefined-method.
        // A `T | nil` union still flows through unchanged, as it does there.
        let nil_skip = safe_nav && {
            let recv_ty = typer.type_of(ast, recv, env, interner);
            arg_is_pure_nil(interner, index, typer.source(), recv_ty)
        };
        let diag = (!nil_skip)
            .then(|| {
                check_call(
                    ast, recv, &method, message_span, safe_nav, env, &typer, interner, index,
                )
            })
            .flatten()
            .or_else(|| {
                check_narrowed_call(
                    call_id, ast, recv, &method, message_span, safe_nav, gate_env, &typer,
                    interner, index, &class_snaps,
                )
            })
            .or_else(|| {
                check_collection_call(
                    call_id, ast, recv, &method, message_span, safe_nav, gate_env, &typer,
                    interner, index, &coll_snaps,
                )
            })
            .or_else(|| {
                check_wrong_arity(
                    ast, recv, &method, &args, args_plain_positional, has_block, message_span,
                    env, &typer, interner, index,
                )
            })
            .or_else(|| {
                check_nil_receiver(call_id, &method, message_span, safe_nav, &nil_snaps, index)
            })
            .or_else(|| {
                check_always_raises(
                    ast, recv, &method, &args, has_block, message_span, env, &typer, interner,
                    index,
                )
            });
        if let Some(diag) = diag {
            out.push(diag);
        }

        // `call.argument-type-mismatch` is an INDEPENDENT axis (argument types),
        // NOT part of the one-per-site validity precedence above: the reference
        // emits it ALONGSIDE `call.wrong-arity` at the same call site (a bad-arity
        // AND wrong-typed-first-arg call yields both). Its own gate keeps it off
        // sites the undefined-method rule owns (it requires the method to be
        // RBS-known on the receiver).
        if let Some(diag) = check_argument_type_mismatch(
            ast,
            recv,
            &method,
            &args,
            args_all_plain,
            env,
            &typer,
            interner,
            index,
        ) {
            out.push(diag);
        }
    }

    // Second pass — `flow.dead-assignment` (ADR-0030). A pure AST/structural
    // check, independent of the typer/index above: it walks each NAMED method
    // body and fires on a plain local write never read in that body. Mirrors the
    // reference `DeadAssignmentCollector` exactly (see `dead_assignments_in_def`).
    // Every NAMED `def` — top-level, class/module body, or nested — lowers to a
    // `Node::Definition { name: Some(..) }` in the arena (a class's direct `def`s
    // are lowered statements, not synthetic copies), so iterating the arena hits
    // each method body EXACTLY ONCE, matching the reference's full DFS over every
    // `DefNode`. A name-less Definition (`class << self`) is skipped — the
    // reference fires only inside named `DefNode`s. The `MethodBody` harvest on
    // ClassDef/ModuleDef is a duplicate VIEW of these same defs (for tier-4b
    // return inference); we deliberately do NOT walk it here, to avoid a double
    // emit.
    for (def_id, node) in ast.iter() {
        if let Node::Definition {
            name: Some(def_name),
            body,
            span,
            param_span,
            ..
        } = node
        {
            dead_assignments_in_def(ast, def_id, def_name, body, *span, *param_span, &mut out);
        }
    }

    // Third pass — `def.override-visibility-reduced` (ADR-35 slice 1). A purely
    // STRUCTURAL def-family check: iterate every `ClassDef`/`ModuleDef`, and for
    // each instance method in its discovered visibility table, fire iff the
    // override strictly REDUCES the visibility of the nearest project ancestor
    // method it overrides. The override span is the method-NAME token of the
    // matching `Definition` in the class body. The OVERRIDING class is identified
    // by its FULLY LEXICALLY-QUALIFIED name (so the project-wide qualified
    // override index resolves its ancestors precisely — the zero-FP keystone).
    // See `check_override_visibility` for the full gate.
    let qualified_names = qualified_class_names(ast);
    for (class_id, node) in ast.iter() {
        let (body, method_visibilities) = match node {
            Node::ClassDef { name, body, method_visibilities, .. }
            | Node::ModuleDef { name, body, method_visibilities, .. }
                if !name.is_empty() =>
            {
                (body, method_visibilities)
            }
            _ => continue,
        };
        let Some(qualified) = qualified_names.get(&class_id) else {
            continue; // un-namable ⇒ skip.
        };
        // Iterate the class body's DIRECT named `Definition` children (the
        // overriding defs), anchoring on each one's name token. A def's recorded
        // visibility comes from the per-node table (by name); a method-name with
        // no direct Definition child (e.g. the untracked `private def foo` form,
        // whose def is a call argument, not a body statement) is simply not seen
        // here — which is correct (that form is silent anyway).
        for &child_id in body {
            let Node::Definition {
                name: Some(method),
                name_span: Some(name_span),
                ..
            } = ast.get(child_id)
            else {
                continue;
            };
            let Some(override_vis) = method_visibilities
                .iter()
                .find(|(m, _)| m == method)
                .map(|(_, v)| *v)
            else {
                continue; // not in the table (singleton / untracked) ⇒ silent.
            };
            if let Some(diag) =
                check_override_visibility(source, qualified, method, override_vis, *name_span)
            {
                out.push(diag);
            }
        }
    }

    // Fourth pass — `flow.unreachable-branch` (ADR-0030). A purely SYNTACTIC,
    // AST/structural check, independent of the typer/index above: it walks every
    // `Node::If` (`if`/`unless`/ternary — Prism parses a ternary as an IfNode too)
    // and fires iff the predicate is a LITERAL node and the resulting dead branch
    // is non-empty. The keyword-inversion (read from `is_unless`) decides which
    // branch is dead, so the diagnostic anchors on the DEAD branch — never on live
    // code. Mirrors the reference's `unreachable_branch_diagnostic`. Iterating the
    // arena hits every `if`/`unless` exactly once (each lowers to one Node::If).
    for (_id, node) in ast.iter() {
        if let Node::If {
            predicate,
            then_body,
            else_body,
            is_unless,
            ..
        } = node
        {
            if let Some(diag) =
                check_unreachable_branch(ast, *predicate, then_body, else_body, *is_unless)
            {
                out.push(diag);
            }
        }
    }

    // Fifth pass — `flow.always-truthy-condition` (ADR-0022 first flow slice). The
    // inferred-constant counterpart to the syntactic `unreachable-branch`: a
    // predicate that the dominating flow scope folds to a `Type::Constant` (e.g.
    // `x = 5; if x`). `always_truthy_snapshots` runs ONE flow-sensitive
    // constant-propagation pass over the file and records, per non-loop/block
    // `if`/`unless`, the predicate's folded type under branch-joined bindings —
    // a strict under-approximation of the reference folder (zero-FP keystone).
    // The rule then applies the reference's remaining skip envelope (syntactic
    // literal → owned by unreachable-branch; defensive predicate call) and fires
    // when the snapshot is a constant. Loop/block suppression is already baked in
    // (those predicates are absent from the snapshot map).
    let truthy_snapshots = typer.always_truthy_snapshots(ast, interner);
    for (id, node) in ast.iter() {
        if let Node::If { predicate, .. } = node {
            if let Some(diag) =
                check_always_truthy(ast, id, *predicate, &truthy_snapshots, interner)
            {
                out.push(diag);
            }
        }
    }

    // Sixth pass — `call.unresolved-toplevel` (ref ADR-34). An implicit-self call
    // (`receiver: None`) at TOPLEVEL scope whose name resolves against NEITHER the
    // `Object`/`Kernel` instance surface NOR a same-file toplevel `def`. Toplevel
    // = the call's span is not contained in any `def`/`class`/`module` span
    // (span-containment, orphan-proof; ADR-24 leniency keeps in-body implicit-self
    // calls silent). See `check_unresolved_toplevel` for the gate.
    unresolved_toplevel_diagnostics(ast, index, source, &mut out);

    // Seventh pass — `call.raise-non-exception` (v0.3.0). Its OWN walk over
    // receiver-None `raise`/`fail` calls (the main call walk is receiver-Some
    // only) — NOT toplevel-restricted, so it fires inside method bodies too. The
    // operand is typed through the shared typer; the verdict + FP gates
    // (project-class bail, module bail, duck `#exception`, redefinition, unknown
    // decline) mirror the reference exactly.
    raise_non_exception_diagnostics(ast, index, source, &typer, &env, interner, &mut out);

    // Eighth pass — `flow.duplicate-hash-key` (v0.3.0). Purely syntactic: walk
    // every Hash literal's precomputed value-pinned key list and fire on a repeat.
    duplicate_hash_key_diagnostics(ast, &mut out);

    // Ninth pass — `flow.return-in-ensure` (v0.3.0). Purely syntactic with a
    // frame-aware envelope: walk every `begin/ensure`'s ensure body for `return`s.
    return_in_ensure_diagnostics(ast, &mut out);

    // Tenth pass — `def.ivar-write-mismatch` (since 0.1.2). Groups each class's
    // instance-method `@x = value` writes by (qualified class, ivar) and fires
    // when a later write's concrete class differs from the canonical one. Types
    // each rvalue through the shared typer (empty local env) plus the rescue-bound
    // exception resolution (increment a); the `Integer()`/`Float()`/`String()`
    // NOMINAL fold (increment b) lives in the typer, so it flows through
    // `type_of` transparently here.
    ivar_write_mismatch_diagnostics(ast, interner, index, source, &typer, &mut out);

    out
}
