//! Flow writes: the span-keyed `(span, name)` tables of local rebinds and
//! in-place mutations that the flow passes widen by span containment
//! ([`collect_flow_writes`], [`toplevel_rebinds`], [`indexed_flow_writes`]),
//! the mutator-method tables behind them, and the env joins/widenings that
//! consume them.

use std::collections::HashSet;

use rigor_parse::{Compound, IndexTargetKey, LoweredAst, Node, NodeId};
use rigor_types::{Interner, ShapeKey};

use crate::{SourceIndex, TypeEnv};

/// Whether `name` is REBOUND (not merely mutated) somewhere inside `span`.
pub(crate) fn rebound_within(
    rebinds: &[(rigor_parse::Span, String)],
    span: rigor_parse::Span,
    name: &str,
) -> bool {
    rebinds.iter().any(|(ws, n)| n == name && span.0 <= ws.0 && ws.1 <= span.1)
}

/// The smallest span covering every node in `ids`, or `None` when empty.
pub(crate) fn span_hull(ast: &LoweredAst, ids: &[NodeId]) -> Option<rigor_parse::Span> {
    let mut it = ids.iter().map(|&id| ast.get(id).span());
    let first = it.next()?;
    Some(it.fold(first, |acc, s| (acc.0.min(s.0), acc.1.max(s.1))))
}

// The `SHAPE_MUTATORS` name tables
// (`reference/rigor/lib/rigor/inference/mutation_widening.rb:85-110` plus
// `hash_lookup_mutation.rb:22` and `string_mutation.rb:25`) live in
// `rigor_parse::mutators`: the `OperandEffects.any?` gate the lowering now
// applies (rigor-rs#361) needs `is_shape_mutator` there, and `rigor-parse`
// cannot depend on this crate. Re-exported so the existing
// `crate::flow_writes::…` / `crate::{ARRAY_MUTATORS,…}` paths keep working.
pub(crate) use rigor_parse::mutators::{
    ARRAY_MUTATORS, HASH_MUTATORS, STRING_MUTATORS, is_shape_mutator,
};
// `HASH_LOOKUP_MUTATORS` is exercised from `tests.rs` only today.
#[cfg(test)]
pub(crate) use rigor_parse::mutators::HASH_LOOKUP_MUTATORS;

/// The REBIND half of [`collect_flow_writes`] — local assignments only, with the
/// in-place-mutation entries left out. The collection-shape pass needs the two
/// apart: a mutation KEEPS the nominal while a rebind kills it, and the merged
/// table cannot tell them apart.
pub(crate) fn collect_rebind_writes(ast: &LoweredAst) -> Vec<(rigor_parse::Span, String)> {
    let mut out: Vec<(NodeId, rigor_parse::Span, String)> = ast
        .iter()
        .flat_map(|(id, n)| match n {
            Node::LocalVariableWrite { name, span, .. }
            | Node::LocalVariableOpWrite { name, span, .. } => vec![(id, *span, name.clone())],
            Node::MultiWrite { targets, span, .. } => targets
                .bound_names()
                .into_iter()
                .map(|(name, _)| (id, *span, name))
                .collect(),
            Node::Loop { index, .. } => for_index_rebinds(index)
                .into_iter()
                .map(|(s, n)| (id, s, n))
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    drop_shadowed_writes(ast, &mut out);
    let mut out: Vec<(rigor_parse::Span, String)> =
        out.into_iter().map(|(_, s, n)| (s, n)).collect();
    drop_inert_writes(ast, &mut out);
    drop_blocked_writes(ast, &mut out);
    out
}

/// The `(name, span)` entries an `h[k]` index target contributes to a write
/// table — `h[k], z = …` stores through `[]=` on `h` (a mutation, NOT a rebind:
/// the receiver keeps its binding and widens its carrier, exactly as `h[k] = v`
/// does — `IndexWriteWidening` / `MutationWidening.widen_receiver_aliases`,
/// rigor-rs#134). Each entry is keyed by the TARGET's own span: it sits inside
/// the owning `MultiWrite` / `for` / `rescue` construct, so span-containment
/// widening in the enclosing flow constructs sees it. The stored `drop key`
/// is the narrowing-invalidation half — [`toplevel_mutations`] consumes it;
/// this rebind/mutation widening table does not.
fn index_target_writes(entries: rigor_parse::IndexWrites) -> Vec<(rigor_parse::Span, String)> {
    entries
        .into_iter()
        .map(|(name, span, _)| (span, name))
        .collect()
}

/// The [`ShapeKey`] half of a [`rigor_parse::IndexTargetKey`] — the literal
/// index a `h[k]` index-target `[]=` store addresses, for
/// [`crate::flow_eval::Typer::drop_indexed_mutation`] to drop exactly that
/// slot's record (rigor-rs#342).
pub(crate) fn index_target_drop_key(key: Option<IndexTargetKey>) -> Option<ShapeKey> {
    key.map(|k| match k {
        IndexTargetKey::Sym(s) => ShapeKey::Sym(s),
        IndexTargetKey::Str(s) => ShapeKey::Str(s),
        IndexTargetKey::Int(i) => ShapeKey::Int(i),
    })
}

/// Drop every `(id, _, name)` write that sits inside a closure's shadow scope
/// and names a local the closure BINDS — it rebinds the closure's own
/// parameter/local, never the outer binding of the same name (rigor-rs#166 for
/// literal blocks; rigor-rs#137 extends the scope set with `block_params` and
/// the crossed-closure recovered children, [`closure_shadow_scopes`]).
fn drop_shadowed_writes(
    ast: &LoweredAst,
    writes: &mut Vec<(NodeId, rigor_parse::Span, String)>,
) {
    let shadow_scopes = closure_shadow_scopes(ast);
    writes.retain(|(id, _, name)| {
        !shadow_scopes.iter().any(|(descendants, bound)| {
            descendants.contains(id) && bound.contains(name)
        })
    });
}

/// A `for` index's bound names as rebind entries, each keyed by its own target
/// span — which lies inside the loop's span, so the loop widens them at its exit
/// exactly as it widens a body write (rigor-rs#151). The reference binds the
/// element type; widening is the FP-safe floor of that.
fn for_index_rebinds(index: &[(String, rigor_parse::Span)]) -> Vec<(rigor_parse::Span, String)> {
    index.iter().map(|(name, span)| (*span, name.clone())).collect()
}

/// Drop every write that sits inside a [`StatementsKind::Inert`] carrier (a
/// `defined?` operand, an `END` / `BEGIN` body): the reference never evaluates
/// it in sequence, so it neither binds nor widens (rigor-rs#153). With the
/// entry gone, the construct that holds it widens nothing for it.
///
/// [`StatementsKind::Inert`]: rigor_parse::StatementsKind::Inert
fn drop_inert_writes(ast: &LoweredAst, writes: &mut Vec<(rigor_parse::Span, String)>) {
    writes.retain(|(span, _)| !ast.in_inert_carrier(*span));
}

/// Drop every write that sits inside a `Recovered::blocked` position — the
/// wrapper-flattened never-bound sibling of [`drop_inert_writes`]: a
/// `when`/`in` condition under a rescue modifier, a dead arm, a `super`/
/// `yield` operand under a wrapper. The reference discards that position's
/// post-scope, so the write neither binds nor widens — `x = (case v when
/// (q = 1; Integer) then 1 end) rescue nil` must not rebind `q`
/// (rigor-rs#357).
fn drop_blocked_writes(ast: &LoweredAst, writes: &mut Vec<(rigor_parse::Span, String)>) {
    writes.retain(|(span, _)| !ast.in_blocked_carrier(*span));
}

/// In-place mutator methods that invalidate a value-pinned literal carrier
/// (`Tuple` / `HashShape` / a `Constant` String) bound to a local — the union of
/// the reference's `MutationWidening::ARRAY_MUTATORS`, `HASH_MUTATORS` and
/// `StringMutation::MUTATORS` (`reference/rigor/lib/rigor/inference/
/// mutation_widening.rb:85-104`, `string_mutation.rb:25`), minus the
/// `PURE_SELF_RETURNERS` (`freeze`/`dup`/`clone`/`itself`), which never appear
/// here. A call `local.<m>(…)` for `m` in this set mutates `local`'s content, so
/// the literal arity/pair-set/value the carrier tracked is no longer justified —
/// the binding must widen (see [`collect_flow_writes`]).
///
/// The String half is upstream's `StringMutation.widen_constant`, which the port
/// had never carried: `s = "ab"; s.upcase!; if s == "ab"` folded here and fired
/// `flow.always-truthy-condition` where the oracle widens `s` to `String`. Pin
/// `e59b7b89` grew the String table 26 -> 35 (`delete_prefix!`, `encode!`,
/// `scrub!`, …), which turned more of the same shape into oracle silence.
///
/// `HashLookupMutation::MUTATORS` (`default=` / `default_proc=` /
/// `compare_by_identity`) is deliberately NOT here: upstream keeps a local's
/// present keys readable through them (`h = { a: 1 }; h.compare_by_identity;
/// if h[:a]` still fires on the oracle), so widening would be a coverage loss;
/// their shape-opening half is issue #1280.
pub(crate) const MUTATOR_METHODS: &[&str] = &[
    // ARRAY mutators
    "<<", "push", "append", "prepend", "unshift", "concat", "insert", "pop", "shift", "delete",
    "delete_at", "delete_if", "reject!", "clear", "compact!", "replace", "fill", "[]=", "map!",
    "collect!", "select!", "filter!", "keep_if", "uniq!", "flatten!", "sort!", "sort_by!",
    "reverse!", "rotate!", "shuffle!", "slice!",
    // HASH mutators not already listed above
    "store", "merge!", "update", "transform_keys!", "transform_values!",
    // STRING mutators not already listed above
    "setbyte", "bytesplice", "append_as_bytes", "force_encoding", "sub!", "gsub!", "tr!",
    "tr_s!", "delete!", "squeeze!", "succ!", "next!", "upcase!", "downcase!", "capitalize!",
    "swapcase!", "strip!", "lstrip!", "rstrip!", "chomp!", "chop!", "delete_prefix!",
    "delete_suffix!", "encode!", "scrub!", "unicode_normalize!",
];

/// Collect every flow-write `(span, name)` in the arena, once, for
/// span-containment widening in the flow passes. Orphan-proof: a write under a
/// lossily-lowered wrapper is still found by its span. Records two kinds:
///
/// - local-variable rebinds (`LocalVariableWrite`/`LocalVariableOpWrite`) — the
///   assignment invalidates the prior binding;
/// - **multi-write targets** (`a, (b, c), *rest = rhs`) — every local name the
///   destructure binds, keyed by the WHOLE multi-write span (the same
///   whole-statement key a single-target write uses). Before `Node::MultiWrite`
///   existed these names were absent from the arena entirely, so a multi-write
///   rebind never widened an earlier straight-line binding — a live
///   `flow.always-truthy-condition` false positive;
/// - **in-place content mutations** — a call `local.<mutator>(…)` whose receiver
///   is a bare local read and whose method is in [`MUTATOR_METHODS`], keyed by the
///   whole-call span. This is the port of the reference's `MutationWidening`
///   (`widen_after_call` + `widen_after_block`): the mutator forgets the literal
///   shape, so the containing flow construct widens `local` the same way a rebind
///   inside it would. `ast.iter()` already descends nested block/case bodies, so a
///   mutation deep inside an `each`/`case` is found and its span is contained by
///   the enclosing construct; a straight-line mutation is its own containing span
///   and widens through the catch-all/`If` arms;
/// - **index-target stores** — `h[k]` as a multi-assign target (`h[:a], z = 1, 2`),
///   `for` index (`for h[:k] in xs`) or `rescue` reference (`rescue => h[:e]`):
///   each stores through `[]=` on `h` (`IndexWriteWidening`, rigor-rs#134). Keyed
///   by the TARGET's span, so it widens inside whichever construct owns it.
pub fn collect_flow_writes(ast: &LoweredAst) -> Vec<(rigor_parse::Span, String)> {
    // The bool is `scan_visible`: the mark names a CONTENT mutation the
    // reference's writeback text scan (`content_mutation_target` — a
    // mutator `CallNode`, a compound index write, or an `IndexTargetNode`)
    // sees even inside a never-evaluated operand of an iterated body
    // (rigor-rs#312). Rebinds are never scan-visible.
    let mut out: Vec<(NodeId, rigor_parse::Span, String, bool)> = ast
        .iter()
        .flat_map(|(id, n)| match n {
            Node::LocalVariableWrite { name, span, .. }
            | Node::LocalVariableOpWrite { name, span, .. } => {
                vec![(id, *span, name.clone(), false)]
            }
            Node::MultiWrite { targets, span, .. } => {
                let mut entries: Vec<(NodeId, rigor_parse::Span, String, bool)> = targets
                    .bound_names()
                    .into_iter()
                    .map(|(name, _)| (id, *span, name, false))
                    .collect();
                // An `h[k]` target stores through `[]=` on `h` — a receiver
                // MUTATION at the target's span, not a rebind (rigor-rs#134).
                entries.extend(
                    index_target_writes(targets.index_writes())
                        .into_iter()
                        .map(|(s, n)| (id, s, n, true)),
                );
                entries
            }
            Node::Call { receiver: Some(r), method, span, .. }
                if MUTATOR_METHODS.contains(&method.as_str()) =>
            {
                match ast.get(*r) {
                    Node::LocalVariableRead { name, .. } => {
                        vec![(id, *span, name.clone(), true)]
                    }
                    _ => Vec::new(),
                }
            }
            // `h[k] ||= v` / `h[k] &&= v` / `h[k] op= v` — a compound index
            // write stores through `[]=` on its receiver, so a bare-local
            // receiver is a content mutation of the binding, keyed by the
            // whole-statement span exactly like the `[]=` `Call` arm. The
            // reference routes all three into `widen_for_mutator` /
            // `widen_receiver_aliases` with method `[]=` (`index_write_
            // widening.rb`, upstream #560).
            Node::IndexWrite { receiver: Some(r), span, .. } => match ast.get(*r) {
                Node::LocalVariableRead { name, .. } => {
                    vec![(id, *span, name.clone(), true)]
                }
                _ => Vec::new(),
            },
            Node::Loop { index, index_writes, .. } => {
                let mut entries: Vec<(NodeId, rigor_parse::Span, String, bool)> =
                    for_index_rebinds(index)
                        .into_iter()
                        .map(|(s, n)| (id, s, n, false))
                        .collect();
                // `for h[:k] in xs` stores each element through `[]=` on `h`.
                entries.extend(
                    index_target_writes(index_writes.clone())
                        .into_iter()
                        .map(|(s, n)| (id, s, n, true)),
                );
                entries
            }
            // `rescue => h[:e]` stores the exception through `[]=` on `h`
            // (rigor-rs#134); `bound_name` stays out here — a method-level
            // `rescue => e` keeps folding on the reference too (see
            // `indexed_flow_writes`).
            Node::BeginRescue { clauses, .. } => clauses
                .iter()
                .flat_map(|c| index_target_writes(c.index_writes.clone()))
                .map(|(s, n)| (id, s, n, true))
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    let shadow_scopes = closure_shadow_scopes(ast);
    out.retain(|(id, _, name, _)| {
        !shadow_scopes.iter().any(|(descendants, bound)| {
            descendants.contains(id) && bound.contains(name)
        })
    });
    // The inert filter: a write inside a never-evaluated operand binds
    // nothing, so its mark always drops — EXCEPT a scan-visible content
    // mutation inside an ITERATED body's scanned operand, which the
    // reference's writeback applies anyway (rigor-rs#312). The blocked
    // filter is the same shape for `Recovered::blocked` positions (a
    // `when`/`in` condition under a rescue modifier, a dead arm — the
    // reference discards their post-scope, rigor-rs#357).
    out.retain(|(_, w, _, scan)| {
        (!ast.in_inert_carrier(*w) || (*scan && ast.in_scanned_inert_carrier(*w)))
            && (!ast.in_blocked_carrier(*w)
                || (*scan && ast.in_iterative_blocked_carrier(*w)))
    });
    out.into_iter().map(|(_, s, n, _)| (s, n)).collect()
}

/// Every rebind of a TOP-LEVEL local, span-keyed for [`widen_flow_writes`]: a
/// plain, operator or multiple write, and a `rescue => e` binding, outside any
/// `def` / `class` / `module` body (each its own local scope). Unlike
/// [`collect_flow_writes`] it carries no receiver mutation — a mutator changes a
/// value's contents, not which value the local names.
///
/// A block (`foo { |w| … }` / `do…end`) or lambda (`->(w) { … }`) opens a
/// SHADOW scope rather than an excluded one: a write inside it to a name the
/// block binds — any parameter form, a `;`-declared block-local, a numbered
/// param, or a name first assigned in the body — is a block-scoped write, not
/// a rebind of the top-level local it shadows (rigor-rs#166, a #148 coverage
/// regression). Prism's `locals` list for the block/lambda node is the exact
/// bound set: it already excludes captured outer locals, so a write to a
/// name the block does NOT bind (`{ |x| w = 2 }`, `w` top-level) still counts
/// as a rebind — the must-stay-declined rows. The nested-block case is covered
/// STRUCTURALLY, not by span: an inner block's body stays reachable from the
/// enclosing block's body roots, so the enclosing `locals` list shadows a
/// write there (a heredoc's body escapes its opener's span — see below).
pub(crate) fn toplevel_rebinds(ast: &LoweredAst) -> Vec<(rigor_parse::Span, String)> {
    let (scopes, shadow_scopes) = toplevel_scope_filters(ast);
    let mut out = collect_rebind_entries(ast);
    out.retain(|(id, w, name)| {
        !scopes.iter().any(|s| s.0 <= w.0 && w.1 <= s.1)
            && !shadow_scopes.iter().any(|(descendants, bound)| {
                descendants.contains(id) && bound.contains(name)
            })
    });
    let mut out: Vec<(rigor_parse::Span, String)> =
        out.into_iter().map(|(_, s, n)| (s, n)).collect();
    drop_inert_writes(ast, &mut out);
    drop_blocked_writes(ast, &mut out);
    out
}

/// The rebind entries [`toplevel_rebinds`]/[`local_rebinds`] filter —
/// `(node, span, name)` per local-variable write, masgn bound name, `rescue`
/// reference binding and `for` index rebind — before any scope filter runs.
fn collect_rebind_entries(ast: &LoweredAst) -> Vec<(NodeId, rigor_parse::Span, String)> {
    let mut out: Vec<(NodeId, rigor_parse::Span, String)> = Vec::new();
    for (id, n) in ast.iter() {
        match n {
            Node::LocalVariableWrite { name, span, .. }
            | Node::LocalVariableOpWrite { name, span, .. } => {
                out.push((id, *span, name.clone()));
            }
            Node::MultiWrite { targets, span, .. } => {
                out.extend(
                    targets
                        .bound_names()
                        .into_iter()
                        .map(|(name, _)| (id, *span, name)),
                );
            }
            Node::BeginRescue { clauses, .. } => out.extend(
                clauses
                    .iter()
                    .filter_map(|c| c.bound_name.clone().map(|name| (id, c.span, name))),
            ),
            Node::Loop { index, .. } => {
                out.extend(for_index_rebinds(index).into_iter().map(|(s, n)| (id, s, n)));
            }
            _ => {}
        }
    }
    out
}

/// [`toplevel_rebinds`] WITHOUT the `def`/`class`/`module` scope exclusion —
/// rebinds in EVERY local scope, still minus the block/lambda shadow filter,
/// inert carriers and blocked-carrier writes. The nilable walker
/// (`nilable.rs`) descends a `def` body with a fresh env, so a
/// `for h[k] in xs; h = …; end` INSIDE one still needs its inner rebinds
/// listed to tell a rebind of the index-target local from the `[]=` mutation
/// the target performs (rigor-rs#352 review). A write inside a NESTED def may
/// wrongly count for an enclosing construct's `rebound_within` test — a
/// one-way decline, never a new fire.
pub(crate) fn local_rebinds(ast: &LoweredAst) -> Vec<(rigor_parse::Span, String)> {
    let (_scopes, shadow_scopes) = toplevel_scope_filters(ast);
    let mut out = collect_rebind_entries(ast);
    out.retain(|(id, _w, name)| {
        !shadow_scopes.iter().any(|(descendants, bound)| {
            descendants.contains(id) && bound.contains(name)
        })
    });
    let mut out: Vec<(rigor_parse::Span, String)> =
        out.into_iter().map(|(_, s, n)| (s, n)).collect();
    drop_inert_writes(ast, &mut out);
    drop_blocked_writes(ast, &mut out);
    out
}

/// The two scope filters [`toplevel_rebinds`] and [`toplevel_mutations`]
/// share: the `def` / `class` / `module` spans (each an independent local
/// scope) and the block/lambda SHADOW scopes as `(body descendant ids, names
/// the scope binds)` — a write or mutation of a name a block binds is
/// block-scoped, not a top-level-local one (rigor-rs#166). The membership
/// test is STRUCTURAL, never span-based: a heredoc's body lines follow its
/// opener, so an interpolation write lies inside the enclosing span while
/// evaluating in the outer scope.
/// A block/lambda shadow scope: its body's reachable nodes plus the names it
/// binds (owned — [`closure_shadow_scopes`] unions the Prism `locals` with the
/// lowered `block_params` and the recovered-child bindings table).
type ShadowScope = (HashSet<NodeId>, Vec<String>);

fn toplevel_scope_filters(
    ast: &LoweredAst,
) -> (Vec<rigor_parse::Span>, Vec<ShadowScope>) {
    let scopes: Vec<rigor_parse::Span> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::Definition { .. } | Node::ClassDef { .. } | Node::ModuleDef { .. } => {
                Some(n.span())
            }
            _ => None,
        })
        .collect();
    (scopes, closure_shadow_scopes(ast))
}

/// Every in-place mutation of a TOP-LEVEL local — a `local.<mutator>(…)` call
/// with a bare `LocalVariableRead` receiver, plus the `[]=` store an `h[k]`
/// index target performs in a `MultiWrite`, `for` index or `rescue` reference
/// (rigor-rs#134) — as `(call span, name, method, drop_key)`, subject to the same
/// def/class/module and block-binding filters as
/// [`toplevel_rebinds`]. [`crate::flow_eval::Typer::build_toplevel_check_env`]
/// widens these statements: a mutator call rewrites the literal carrier the
/// binding tracked (a `Tuple`'s arity, a `HashShape`'s pair set, a `Constant`
/// String's value) exactly as the reference's `MutationWidening` does
/// (`mutation_widening.rb`). Without it `a = []; a[0, 2] = [1, 2]` kept `a` at
/// `Tuple[]`, so `a.last` folded to `nil` and `x.succ` fired `for nil` where
/// both references are silent (rigor-rs#139). The caller decides whether a
/// contained mutation rewrote its receiver unconditionally (the statement IS
/// the call — mint the nominal) or conditionally (inside a branch, block or
/// value position — it widens to `Dynamic`, handing the convergence question
/// to the collection-shape pass).
/// The tuple is `(call span, receiver name, method, drop_key)`:
/// `drop_key` is the literal index a `[]=` store addresses — a real
/// `local[key] = v` call OR an `h[k]` index target (`h[k], z = …`,
/// `for h[k] in xs`, `rescue => h[k]`), both of which the reference routes
/// through `IndexedNarrowing.invalidate_indexed_write` to drop the one
/// `(local, key)` record (a `[]=` `CallNode` via `invalidate_after_call`,
/// rigor-rs#325; an `IndexTargetNode` via `widen_index_target`,
/// rigor-rs#342). `None` everywhere else — a compound index write does NOT
/// invalidate the record (`eval_index_write` / `eval_index_or_write` never
/// run `invalidate_after_call`), and a non-`[]=` mutator drops every record
/// rooted at the receiver.
pub(crate) fn toplevel_mutations(
    ast: &LoweredAst,
) -> Vec<(rigor_parse::Span, String, String, Option<ShapeKey>)> {
    let (scopes, shadow_scopes) = toplevel_scope_filters(ast);
    let mut out: Vec<(NodeId, rigor_parse::Span, String, String, Option<ShapeKey>)> = Vec::new();
    for (id, n) in ast.iter() {
        match n {
            Node::Call {
                receiver: Some(r),
                method,
                args,
                span,
                ..
            } => {
                // `IndexedNarrowing.mutator?` reads the whole SHAPE_MUTATORS
                // table — the HashLookupMutation names (`default=` …)
                // included: they never change a binding here, but they DO
                // drop indexed narrowings rooted at the receiver.
                if !is_shape_mutator(method) {
                    continue;
                }
                if let Node::LocalVariableRead { name, .. } = ast.get(*r) {
                    let drop_key = if method == "[]=" {
                        args.first().and_then(|&a| stable_index_key(ast.get(a)))
                    } else {
                        None
                    };
                    out.push((id, *span, name.clone(), method.clone(), drop_key));
                }
            }
            // An `h[k]` index target is a `[]=` call whose call node Prism
            // does not make: `h[k], z = …`, `for h[k] in xs`,
            // `rescue => h[k]` all store through `[]=` on `h`
            // (`IndexWriteWidening`, rigor-rs#134). Keyed by the TARGET span —
            // inside the owning construct, so a contained store widens
            // conditionally and `bind_check_statement`'s `MultiWrite` arm can
            // still mint the unconditional carrier by widening that span.
            // The literal `k` rides `drop_key`: `widen_index_target` runs
            // `invalidate_indexed_write` on the target exactly as a `[]=`
            // `CallNode` (rigor-rs#342) — without it the `h[k] ||= v` slot
            // record survived the overwrite and a later `h[k]` read the
            // stale narrowing where the oracle reads the stored value.
            Node::MultiWrite { targets, .. } => {
                for (name, tspan, key) in targets.index_writes() {
                    out.push((
                        id,
                        tspan,
                        name,
                        "[]=".to_string(),
                        index_target_drop_key(key),
                    ));
                }
            }
            Node::Loop { index_writes, .. } => {
                for (name, tspan, key) in index_writes {
                    out.push((
                        id,
                        *tspan,
                        name.clone(),
                        "[]=".to_string(),
                        index_target_drop_key(key.clone()),
                    ));
                }
            }
            Node::BeginRescue { clauses, .. } => {
                for c in clauses {
                    for (name, tspan, key) in &c.index_writes {
                        out.push((
                            id,
                            *tspan,
                            name.clone(),
                            "[]=".to_string(),
                            index_target_drop_key(key.clone()),
                        ));
                    }
                }
            }
            _ => {}
        }
        // `h[k] ||= v` / `h[k] &&= v` / `h[k] op= v` — the reference's
        // `IndexWriteWidening` (`index_write_widening.rb`, upstream #560)
        // routes all three into `widen_for_mutator` with method `[]=`,
        // widening the bare-local receiver's carrier exactly as `h[k] = v`.
        // `drop_key` stays `None`: `eval_index_write` /
        // `eval_index_or_write` do not run `invalidate_after_call`, so an
        // earlier `h[k]` record survives the compound write.
        if let Node::IndexWrite {
            receiver: Some(r),
            span,
            ..
        } = n
        {
            if let Node::LocalVariableRead { name, .. } = ast.get(*r) {
                out.push((id, *span, name.clone(), "[]=".to_string(), None));
            }
        }
        // `h.attr ||= v` / `h.attr &&= v` / `h.attr op= v` — a compound
        // ATTRIBUTE write on a bare-local receiver. `eval_attribute_compound_write`
        // (`statement_evaluator.rb:980`) applies `widen_attribute_write` with
        // the WRITER name (`attr=`), so `default`/`default_proc`/
        // `compare_by_identity` — and every other shape mutator — open their
        // carrier and drop the indexed narrowings rooted at `h`
        // (`IndexWriteInvalidation.mutator?` accepts
        // `HashLookupMutation::MUTATORS`, rigor-rs#343). `drop_key` is `None`:
        // a non-`[]=` mutator drops every record rooted at the receiver.
        //
        // Only a write the reference EVALUATES widens — `evaluated` is false
        // for the operand positions `OperandEffects` never hands to
        // `evaluate` (call receiver/argument, splat, literal container,
        // interpolation, `return`, `rescue` modifier, `in` pattern), under a
        // suppressed carrier, and inside a deferred block/lambda body.
        if let Node::AttrWrite {
            receiver: Some(r),
            write_name,
            evaluated: true,
            span,
            ..
        } = n
        {
            if is_shape_mutator(write_name)
                && let Node::LocalVariableRead { name, .. } = ast.get(*r)
            {
                out.push((id, *span, name.clone(), write_name.clone(), None));
            }
        }
    }
    out.retain(|(id, w, name, ..)| {
        !ast.in_inert_carrier(*w)
            // A mutation mark under a `Recovered::blocked` position never
            // lands — unless an ITERATED body's writeback text scan covers
            // it (rigor-rs#312) or the mark is a compound ATTRIBUTE write,
            // whose `widen_attribute_write` the reference lands even in
            // never-evaluated positions (rigor-rs#343, rigor-rs#357).
            && (!ast.in_blocked_carrier(*w)
                || ast.in_iterative_blocked_carrier(*w)
                || matches!(ast.get(*id), Node::AttrWrite { .. }))
            && !scopes.iter().any(|s| s.0 <= w.0 && w.1 <= s.1)
            && !shadow_scopes.iter().any(|(descendants, bound)| {
                descendants.contains(id) && bound.contains(name)
            })
    });
    out.into_iter()
        .map(|(_, s, n, m, k)| (s, n, m, k))
        .collect()
}

/// Every literal block/lambda SHADOW scope — `(structural body descendants,
/// bound names)` — as an OWNED table for use-site lookups outside the
/// rebind/mutation filters (rigor-rs#137, upstream rigor#1245). A local a
/// closure binds reads `Dynamic[top]` at any use site inside its body: the
/// closure is a lexical boundary, its `locals` the bound set, so the
/// inherited outer binding of the same name does not reach inside.
///
/// The name set is `block_locals` ∪ `block_params`: Prism's `locals` already
/// covers every parameter form, `;` locals, and body-introduced names; the
/// parameter list adds only the implicit `it`, which Prism keeps out of
/// `locals` but which still binds (and can only ever shadow an env binding,
/// so the union is the safe set). Membership is STRUCTURAL — the same
/// [`descendants_of`] walk `toplevel_rebinds` relies on — never span-based:
/// a heredoc body escapes its opener's span while evaluating in its scope.
pub fn closure_shadow_scopes(ast: &LoweredAst) -> Vec<(HashSet<NodeId>, Vec<String>)> {
    let mut scopes: Vec<(HashSet<NodeId>, Vec<String>)> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::Call {
                block_body,
                block_locals,
                block_params,
                ..
            } => {
                let mut bound = block_locals.clone();
                for (name, _) in block_params {
                    if !bound.contains(name) {
                        bound.push(name.clone());
                    }
                }
                (!bound.is_empty()).then(|| (descendants_of(ast, block_body), bound))
            }
            Node::Lambda { body, locals, .. } if !locals.is_empty() => {
                Some((descendants_of(ast, body), locals.clone()))
            }
            _ => None,
        })
        .collect();
    // A block/lambda CROSSED by a wrapper's recovery (`super { |o| o + 1 }` —
    // no `Node::Call` carries its `locals`) records the same shadow set on the
    // recovered child id, whose subtree IS the closure body.
    for &(id, ref bound) in ast.closure_bindings() {
        scopes.push((descendants_of(ast, &[NodeId(id)]), bound.clone()));
    }
    scopes
}

/// The local names a literal block binds — `block_locals` ∪ `block_params`
/// ([`closure_shadow_scopes`]'s name set), for the flow passes that seed a
/// block body's env from the outer bindings: a bound name must not carry its
/// outer binding into the block (rigor-rs#137).
pub(crate) fn block_bound_names<'a>(
    block_locals: &'a [String],
    block_params: &'a [(String, rigor_parse::BlockParamKind)],
) -> impl Iterator<Item = &'a str> {
    block_locals
        .iter()
        .map(String::as_str)
        .chain(block_params.iter().map(|(n, _)| n.as_str()))
}

/// Every node reachable from `roots` through child links, roots included —
/// the structural "inside a block body" test for [`toplevel_rebinds`]. This
/// walk's own child table ([`node_child_ids`]) still leaves a `Range` bound
/// unlinked — `flow_children` links it for the eval-order replay
/// (rigor-rs#306), but for shadow-scope membership the safe side is keeping
/// the write a rebind, so a write in one declines rather than risks a false
/// positive. A `def` parameter default is likewise unreachable here,
/// but the `def`-scope span filter already drops it before this test runs.
fn descendants_of(ast: &LoweredAst, roots: &[NodeId]) -> HashSet<NodeId> {
    let mut seen = HashSet::new();
    let mut stack: Vec<NodeId> = roots.to_vec();
    while let Some(id) = stack.pop() {
        if seen.insert(id) {
            node_child_ids(ast.get(id), &mut stack);
        }
    }
    seen
}

/// Push `n`'s child node ids — every variant field that links lowered children
/// into the arena. Used only by [`descendants_of`]; missing a variant can only
/// under-mark a body descendant, which keeps the write a rebind and declines
/// (the zero-FP-safe direction).
fn node_child_ids(n: &Node, out: &mut Vec<NodeId>) {
    match n {
        Node::Program { body, .. }
        | Node::Statements { body, .. }
        | Node::Definition { body, .. }
        | Node::ClassDef { body, .. }
        | Node::ModuleDef { body, .. }
        | Node::Lambda { body, .. } => out.extend_from_slice(body),
        Node::LocalVariableWrite { value, .. }
        | Node::LocalVariableOpWrite { value, .. }
        | Node::VariableWrite { value, .. }
        | Node::InstanceVariableWrite { value, .. }
        | Node::ConstantWrite { value, .. } => out.push(*value),
        Node::MultiWrite { value, target_exprs, .. } => {
            out.push(*value);
            out.extend_from_slice(target_exprs);
        }
        Node::IndexWrite {
            receiver,
            indices,
            value,
            ..
        } => {
            out.extend(receiver.iter().copied());
            out.extend_from_slice(indices);
            out.push(*value);
        }
        // `recv.attr op= v` — receiver + value are the node's children
        // (rigor-rs#343).
        Node::AttrWrite {
            receiver, value, ..
        } => {
            out.extend(receiver.iter().copied());
            out.push(*value);
        }
        Node::InterpolatedString { parts, .. } | Node::InterpolatedSymbol { parts, .. } => {
            out.extend_from_slice(parts);
        }
        Node::Call {
            receiver,
            args,
            block_body,
            ..
        } => {
            out.extend(receiver.iter().copied());
            out.extend_from_slice(args);
            out.extend_from_slice(block_body);
        }
        Node::If {
            predicate,
            then_body,
            else_body,
            ..
        } => {
            out.push(*predicate);
            out.extend_from_slice(then_body);
            out.extend_from_slice(else_body);
        }
        Node::Case {
            predicate,
            branches,
            else_body,
            ..
        } => {
            out.extend(predicate.iter().copied());
            out.extend_from_slice(branches);
            out.extend_from_slice(else_body);
        }
        Node::When {
            conditions, body, ..
        } => {
            out.extend_from_slice(conditions);
            out.extend_from_slice(body);
        }
        Node::Loop {
            predicate, body, ..
        } => {
            out.extend(predicate.iter().copied());
            out.extend_from_slice(body);
        }
        Node::BeginRescue {
            body,
            ensure_body,
            clauses,
            ..
        } => {
            out.extend_from_slice(body);
            out.extend_from_slice(ensure_body);
            for clause in clauses {
                out.extend_from_slice(&clause.exceptions);
                out.extend_from_slice(&clause.body);
            }
        }
        Node::Logical { left, right, .. } => {
            out.push(*left);
            out.push(*right);
        }
        Node::ArrayLit { elements, .. } | Node::HashLit { elements, .. } => {
            out.extend_from_slice(elements);
        }
        Node::Return { values, .. } => out.extend_from_slice(values),
        _ => {}
    }
}

/// The flow writes that need the project index or a cross-node lookup, appended
/// to [`collect_flow_writes`]'s per-node set. Two kinds, both pure widening:
///
/// - **argument-position mutation** — a call `f(…, local, …)` whose callee
///   mutates the matching POSITIONAL parameter in place
///   (`SourceIndex::method_mutates_param`). Keyed by the whole-call span, like
///   the receiver-side mutator entry, so the enclosing construct widens `local`.
///   This is the caller-side half of the reference's `MutationWidening`;
///   `xs = []; fill xs; if xs.length == 1` must not fold (rigor-survey
///   `rspec-core/lib/rspec/core/world.rb:179`).
/// - **block-scoped rescue binding** — a `rescue => e` clause inside a BLOCK
///   body writes `e` in the enclosing method's scope when `e` is already a local
///   there, and a block runs an unknown number of times, so the binding must
///   widen. `RescueClause::bound_name` is not a `LocalVariableWrite` node, so
///   the per-node scan cannot see it. Keyed by the enclosing BLOCK CALL's span,
///   NOT the clause's: a method-level `begin … rescue => e; end` must keep
///   folding, because the reference keeps folding it (probed both ways), and
///   keying on the clause would widen that case too — a coverage loss, not a
///   false positive, but a needless one. rigor-survey
///   `net-imap-0.6.4.1/lib/net/imap.rb:1470`.
pub(crate) fn indexed_flow_writes(
    ast: &LoweredAst,
    source: &SourceIndex,
) -> Vec<(rigor_parse::Span, String)> {
    let mut out = Vec::new();

    for (_, n) in ast.iter() {
        if let Node::Call { method, args, span, .. } = n {
            for (i, &arg) in args.iter().enumerate() {
                if !source.method_mutates_param(method, i) {
                    continue;
                }
                if let Node::LocalVariableRead { name, .. } = ast.get(arg) {
                    out.push((*span, name.clone()));
                }
            }
        }
    }

    // Spans of every call that carries a block, innermost-first per clause.
    let block_calls: Vec<rigor_parse::Span> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::Call { block_body, span, .. } if !block_body.is_empty() => Some(*span),
            _ => None,
        })
        .collect();
    for (_, n) in ast.iter() {
        let Node::BeginRescue { clauses, .. } = n else {
            continue;
        };
        for clause in clauses {
            let Some(name) = &clause.bound_name else {
                continue;
            };
            let enclosing = block_calls
                .iter()
                .filter(|b| b.0 <= clause.span.0 && clause.span.1 <= b.1)
                .min_by_key(|b| b.1 - b.0);
            if let Some(b) = enclosing {
                out.push((*b, name.clone()));
            }
        }
    }

    drop_inert_writes(ast, &mut out);
    out
}

/// Extend a lexical self-qualified name with a nested class/module `name`,
/// mirroring the `SourceIndex` qualified-owner walk so the flow-eval self context
/// and the fold table agree (`Some("Gitlab")` + `"Database"` -> `Gitlab::
/// Database`). An empty enclosing prefix (top level) yields the bare name.
pub(crate) fn qualify_self(prefix: Option<&str>, name: &str) -> String {
    match prefix {
        Some(p) if !p.is_empty() => format!("{p}::{name}"),
        _ => name.to_string(),
    }
}

/// Widen (to `Dynamic`) every tracked local whose write span is contained in
/// `span` — the conservative invalidation a control-flow construct applies.
pub(crate) fn widen_flow_writes(
    writes: &[(rigor_parse::Span, String)],
    span: rigor_parse::Span,
    env: &mut TypeEnv,
    interner: &mut Interner,
) {
    let u = interner.untyped();
    for (wspan, name) in writes {
        if span.0 <= wspan.0 && wspan.1 <= span.1 {
            env.insert(name.clone(), u);
        }
    }
}

/// Drop the `Array.new`-provenance of every local whose write span is contained
/// in `span` — the `penv` counterpart of [`widen_flow_writes`] (a reassignment
/// inside `span` invalidates the "still bound to `Array.new(nominal)`" fact).
pub(crate) fn widen_penv_writes(
    writes: &[(rigor_parse::Span, String)],
    span: rigor_parse::Span,
    penv: &mut HashSet<String>,
) {
    for (wspan, name) in writes {
        if span.0 <= wspan.0 && wspan.1 <= span.1 {
            penv.remove(name);
        }
    }
}

/// Join two branch environments: a binding survives only when both sides map it
/// to the IDENTICAL `TypeId`; every disagreement, and every local bound in only
/// one branch, widens to `Dynamic`. This is the branch-merge that makes a
/// surviving `Type::Constant` sound to witness as always-truthy/falsey.
pub(crate) fn join_flow_envs(a: &TypeEnv, b: &TypeEnv, interner: &mut Interner) -> TypeEnv {
    let u = interner.untyped();
    let mut out = TypeEnv::with_capacity(a.len());
    for (k, av) in a {
        let v = match b.get(k) {
            Some(bv) if bv == av => *av,
            _ => u,
        };
        out.insert(k.clone(), v);
    }
    for k in b.keys() {
        if !a.contains_key(k) {
            out.insert(k.clone(), u);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Indexed stored-slot narrowing (rigor-rs#325) — the port of the reference's
// `IndexedNarrowing` side table (`indexed_narrowing.rb` +
// `Scope#with_indexed_narrowing`). A `h[k] ||= v` that evaluated inline
// records `h[k] -> narrow_truthy(h[k]) | v`; a later `h[k]` reads it.
// The record lives in the flat `TypeEnv` under a synthetic key that can
// never collide with a local name, so env joins intersect it away for
// free and rebind/mutator invalidation is an env edit.
// ---------------------------------------------------------------------------

/// The env key an `(h, k)` indexed narrowing is recorded under —
/// `h\u{1f}Sym("a")` shaped. `\u{1f}` can never appear in a Ruby local
/// name, so a record can never collide with a real binding, and
/// `join_flow_envs` treats it like any disagreement: two branches whose
/// records differ widen the key to `Dynamic` — which is exactly the
/// "no narrowing survives the join" answer (`eval_index_or_write`'s
/// record is post-scope state that joins away when the other side lacks
/// it, rigor-rs#325).
pub fn indexed_narrowing_key(name: &str, key: &ShapeKey) -> String {
    format!("{name}\u{1f}{key:?}")
}

/// Drop every indexed narrowing rooted at `name` — the reference's
/// `Scope#without_indexed_narrowings_for`: a rebind of `name` or a
/// shape-mutator call on it invalidates all of its slot records.
pub fn drop_indexed_narrowings(env: &mut TypeEnv, name: &str) {
    let prefix = format!("{name}\u{1f}");
    env.retain(|k, _| !k.starts_with(prefix.as_str()));
}

/// `IndexedNarrowing.stable_key` (`indexed_narrowing.rb:63`): the literal
/// `ShapeKey` an index argument names — Symbol / String / Integer literals
/// only (`STABLE_KEY_NODES`); a variable, splat or interpolated key
/// declines.
pub(crate) fn stable_index_key(node: &Node) -> Option<ShapeKey> {
    match node {
        Node::SymbolLit { value, .. } => Some(ShapeKey::Sym(value.clone())),
        Node::StringLit { value, .. } => Some(ShapeKey::Str(value.clone())),
        Node::IntegerLit {
            value: Some(v), ..
        } => Some(ShapeKey::Int(*v)),
        _ => None,
    }
}

/// `IndexedNarrowing.element_address` (`indexed_narrowing.rb:198`): the
/// `(receiver_local, literal_key)` address of a single-key element read
/// `h[k]` — or of a compound index write `h[k] ||= v`, whose stored value
/// IS that element — bare (parentheses are already unwrapped at lower
/// time). `None` for any other receiver shape, a missing `[]` argument, a
/// `h[k] { }` read with a block, or a multi-index form.
pub(crate) fn element_slot(ast: &LoweredAst, id: NodeId) -> Option<(String, ShapeKey)> {
    let (receiver, key_node) = match ast.get(id) {
        Node::Call {
            receiver: Some(r),
            method,
            args,
            block_body,
            ..
        } if method == "[]" && args.len() == 1 && block_body.is_empty() => (*r, args[0]),
        Node::IndexWrite {
            receiver: Some(r),
            indices,
            ..
        } if indices.len() == 1 => (*r, indices[0]),
        _ => return None,
    };
    let Node::LocalVariableRead { name, .. } = ast.get(receiver) else {
        return None;
    };
    stable_index_key(ast.get(key_node)).map(|key| (name.clone(), key))
}

/// A `h[k] ||= v` write that evaluated inline (`operand`): records
/// `h[k] -> narrow_truthy(h[k]) | v` under [`indexed_narrowing_key`].
#[derive(Clone, Debug)]
pub(crate) struct SlotWrite {
    /// The `IndexWrite` node's id — the scope filters key on it.
    pub id: NodeId,
    /// The write's whole span — the same key `mutations` applies it under.
    pub span: rigor_parse::Span,
    /// The receiver local (`h`).
    pub name: String,
    /// The literal index (`:a`).
    pub key: ShapeKey,
    /// The receiver node — `slot_stored_type` reads `receiver[k]` through it
    /// for the `current` half (`index_read_type`).
    pub receiver: NodeId,
    /// The single index node.
    pub key_node: NodeId,
    /// The rvalue node.
    pub value: NodeId,
}

/// A `h[k].<mutator>` or `h[k][j] = v` call — the reference's
/// `widen_mutated_slot`: the mutator widens the RECORDED slot's value
/// (`MutationWidening.widen_for_mutator` + the `string_slot_floor`
/// fallback), never the receiver binding.
#[derive(Clone, Debug)]
pub(crate) struct SlotMutation {
    /// The call node's id — the scope filters key on it.
    pub id: NodeId,
    /// The call's whole span.
    pub span: rigor_parse::Span,
    /// The slot's receiver local (`h`).
    pub name: String,
    /// The slot's literal key.
    pub key: ShapeKey,
    /// The mutator method (`<<`, `[]=`, `strip!`, …).
    pub method: String,
}

/// The indexed-narrowing facts one file contributes (rigor-rs#325).
#[derive(Default)]
pub(crate) struct IndexedFlow {
    /// The `h[k] ||= v` records — `eval_index_or_write`'s
    /// `with_indexed_narrowing` half.
    pub slot_writes: Vec<SlotWrite>,
    /// The span of every `operand`-flagged `IndexWrite` — including the
    /// `slot_writes` — the lenient carrier mint applies to (an evaluated
    /// `h[k] op= v` mints `h`'s nominal rather than `Dynamic` exactly as
    /// `eval_index_write` widens the receiver).
    pub operand_spans: HashSet<rigor_parse::Span>,
    /// Element-mutator calls on a recorded slot.
    pub slot_mutations: Vec<SlotMutation>,
}

/// Collect the file's [`IndexedFlow`]. Mirrors `collect_flow_writes`'s
/// filters: closure-shadowed receivers drop out structurally, and an
/// `Inert`-carrier write drops unless it is a scan-visible content
/// mutation inside an iterated body's scanned operand (rigor-rs#312) —
/// every indexed write is content-shaped, so `scan_visible` holds for all
/// of them. `def`/`class`/`module` scope filtering happens at the consumers
/// (`flow_eval_scope`'s `use_scopes` retain), the same place the write
/// tables get theirs.
pub(crate) fn collect_indexed_flow(ast: &LoweredAst) -> IndexedFlow {
    let shadow_scopes = closure_shadow_scopes(ast);
    let shadowed = |id: NodeId, name: &str| {
        shadow_scopes
            .iter()
            .any(|(descendants, bound)| descendants.contains(&id) && bound.contains(&name.to_string()))
    };
    // The same inert-carrier gate `collect_flow_writes` applies to
    // scan-visible content mutations.
    let write_dropped = |span: rigor_parse::Span| {
        ast.in_inert_carrier(span) && !ast.in_scanned_inert_carrier(span)
    };
    let mut flow = IndexedFlow::default();
    for (id, n) in ast.iter() {
        match n {
            Node::IndexWrite {
                receiver: Some(r),
                indices,
                value,
                compound,
                operand,
                span,
            } => {
                if !*operand {
                    continue;
                }
                if !write_dropped(*span) {
                    flow.operand_spans.insert(*span);
                }
                // `single_index_argument` + `stable_address`: only a
                // single-literal-key `||=` on a bare local records.
                if !matches!(compound, Compound::Or)
                    || indices.len() != 1
                    || write_dropped(*span)
                {
                    continue;
                }
                let Node::LocalVariableRead { name, .. } = ast.get(*r) else {
                    continue;
                };
                if shadowed(id, name) {
                    continue;
                }
                if let Some(key) = stable_index_key(ast.get(indices[0])) {
                    flow.slot_writes.push(SlotWrite {
                        id,
                        span: *span,
                        name: name.clone(),
                        key,
                        receiver: *r,
                        key_node: indices[0],
                        value: *value,
                    });
                }
            }
            Node::Call {
                receiver: Some(r),
                method,
                span,
                ..
            } if method == "[]=" || is_shape_mutator(method) => {
                if write_dropped(*span) {
                    continue;
                }
                if let Some((name, key)) = element_slot(ast, *r) {
                    if shadowed(id, &name) {
                        continue;
                    }
                    flow.slot_mutations.push(SlotMutation {
                        id,
                        span: *span,
                        name,
                        key,
                        method: method.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    flow
}
