//! Flow writes: the span-keyed `(span, name)` tables of local rebinds and
//! in-place mutations that the flow passes widen by span containment
//! ([`collect_flow_writes`], [`toplevel_rebinds`], [`indexed_flow_writes`]),
//! the mutator-method tables behind them, and the env joins/widenings that
//! consume them.

use std::collections::HashSet;

use rigor_parse::{Compound, IndexTargetKey, LoweredAst, Node, NodeId, StatementsKind};
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
    ARRAY_MUTATORS, HASH_MUTATORS, STRING_MUTATORS, is_content_mutator, is_shape_mutator,
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
/// `scrub!`, …), and v0.4.2 35 -> 42 (Ruby 4.1 bit operations, #1691), which
/// turned more of the same shape into oracle silence.
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
    // Upstream v0.4.2 (#1691): Ruby 4.1's bit operations, declared by the
    // vendored `core_overlay/string.rbs` whatever Ruby runs the analysis.
    "bit_set", "bit_clear", "bit_flip", "bitwise_not!", "bitwise_and!", "bitwise_or!",
    "bitwise_xor!",
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
            // A pattern-binding marker's `names` are real local rebinds on
            // the oracle (`case v; in [s]; end`, `v => s`, `v in [s]`) —
            // counted so `widen_flow_writes` widens the rebound name like any
            // other write (rigor-rs#352 review). Not scan-visible.
            Node::UnmodeledWrite { span, names } => names
                .iter()
                .map(|name| (id, *span, name.clone(), false))
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    // Upstream v0.4.6 (#2285, `DeepErasure.widen_element_aliases`): a block
    // call or `for` whose body mutates an element in place drops the shapes
    // its receiver's locals carry at every depth. Recorded as a content
    // mutation of each local, keyed by the whole call / loop span (the
    // reference widens on the scope AFTER the construct), so the binding
    // widens — coarser than `DeepErasure.widen`, never more diagnostics.
    let element_mutations = ast.element_mutations();
    if !element_mutations.is_empty() {
        for (id, n) in ast.iter() {
            let span = match n {
                Node::Call { span, .. } | Node::Loop { span, .. } => *span,
                _ => continue,
            };
            for (s, names) in element_mutations {
                if *s == span {
                    out.extend(names.iter().map(|name| (id, span, name.clone(), true)));
                }
            }
        }
    }
    // rigor-rs#409: a scan-visible mark inside a `while`/`until` body
    // survives a deeper shadow scope — see [`loop_body_spans`].
    let iterated_bodies = loop_body_spans(ast);
    let shadow_scopes = closure_shadow_scopes(ast);
    out.retain(|(id, w, name, scan)| {
        !shadow_scopes.iter().any(|(descendants, bound)| {
            descendants.contains(id)
                && bound.contains(name)
                // rigor-rs#409/#501: a scan-visible mark inside a
                // `while`/`until` body survives a deeper shadow scope
                // (`each { |h| h[:a] = 9 }` under `while` still rejoins
                // the OUTER `h` — `loop_content_writeback`'s textual
                // name match sees any depth) — but ONLY when the scope
                // sits inside the iterated body. A shadow scope
                // ENCLOSING the loop still wins:
                // `captured_content_mutations` tests `receiver.depth >
                // scope_nesting(ancestors)`, so the writeback rejoins
                // `h` in the block's own scope only and the outer
                // narrowing survives.
                && !(*scan
                    && iterated_bodies.iter().any(|b| {
                        contains_span(*b, *w)
                            && descendants
                                .iter()
                                .all(|&d| contains_span(*b, ast.get(d).span()))
                    }))
        })
    });
    // The inert filter: a write inside a never-evaluated operand binds
    // nothing, so its mark always drops — EXCEPT a scan-visible content
    // mutation inside an ITERATED body's scanned operand, which the
    // reference's writeback applies anyway (rigor-rs#312). The blocked
    // filter is the same shape for `Recovered::blocked` positions (a
    // `when`/`in` condition under a rescue modifier, a dead arm — the
    // reference discards their post-scope, rigor-rs#357).
    out.retain(|(id, w, _, scan)| {
        // A NAMED `UnmodeledWrite` (pattern-binding locals) survives an inert
        // carrier only when that carrier is the `in` clause the marker itself
        // stands on — `in_outer_inert_carrier` — never a blocked carrier
        // (`case … in [s] rescue …` discards the scope, rigor-rs#357).
        let pattern_bind =
            matches!(ast.get(*id), Node::UnmodeledWrite { names, .. } if !names.is_empty());
        (!ast.in_inert_carrier(*w)
            || (*scan && ast.in_scanned_inert_carrier(*w))
            || (pattern_bind && !ast.in_outer_inert_carrier(*w)))
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
    retain_flowable_rebinds(ast, &mut out);
    out.into_iter().map(|(_, s, n)| (s, n)).collect()
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
            // A pattern-binding marker's `names` rebind locals exactly like a
            // plain write on the oracle (`case v; in [s]; end` inside a `for
            // s[0] in …` span makes the post-join `s` NOT the pre-loop String
            // — rigor-rs#352 review).
            Node::UnmodeledWrite { span, names } => {
                out.extend(names.iter().map(|name| (id, *span, name.clone())));
            }
            _ => {}
        }
    }
    // Upstream v0.4.6 (#2285, `DeepErasure.widen_element_aliases`): a block
    // call / `for` whose body mutates an element in place re-binds its
    // receiver's locals with every shape dropped. Read as a rebind of each
    // local at the whole call / loop span — the binding widens, which only
    // declines more than the reference's nominal-keeping widen.
    let element_mutations = ast.element_mutations();
    if !element_mutations.is_empty() {
        for (id, n) in ast.iter() {
            let span = match n {
                Node::Call { span, .. } | Node::Loop { span, .. } => *span,
                _ => continue,
            };
            for (s, names) in element_mutations {
                if *s == span {
                    out.extend(names.iter().map(|name| (id, span, name.clone())));
                }
            }
        }
    }
    out
}

/// `outer` contains `inner`.
fn contains_span(outer: rigor_parse::Span, inner: rigor_parse::Span) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}

/// The statement spans of every `while`/`until` body — the region
/// `loop_content_writeback` text-scans by receiver name at ANY depth
/// (`content_mutation_target { |_r| true }`), so a `|h|` bound inside
/// one still marks the OUTER `h` and the shadow filters must not drop
/// the mark (rigor-rs#409). Outside an iterated body the closure's own
/// binding wins: `{ |h| h[:a] = 9 }` at top level shadows `h` on the
/// oracle too — `content_writeback_block_captures` honors the param
/// binding. A `for` body is no iterated body — it joins scopes rather
/// than writing back (same shape test as
/// `flow_eval::Typer::path_content_writeback`).
fn loop_body_spans(ast: &LoweredAst) -> Vec<rigor_parse::Span> {
    ast.iter()
        .flat_map(|(_, n)| match n {
            Node::Loop {
                predicate: Some(_),
                index,
                index_writes,
                body,
                ..
            } if index.is_empty() && index_writes.is_empty() => body
                .iter()
                .map(|b| ast.get(*b).span())
                .collect(),
            _ => Vec::new(),
        })
        .collect()
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
    retain_flowable_rebinds(ast, &mut out);
    out.into_iter().map(|(_, s, n)| (s, n)).collect()
}

/// [`drop_inert_writes`] + [`drop_blocked_writes`] on id-carrying rebind
/// entries, so a NAMED `UnmodeledWrite` (pattern-binding locals) survives the
/// `in` clause's own inert carrier — `in_outer_inert_carrier`, the marker is a
/// sibling sharing the carrier's span — while still dropping inside a wider
/// inert carrier or any blocked position (`case … in [s] rescue …`,
/// rigor-rs#357 / #352 review).
fn retain_flowable_rebinds(ast: &LoweredAst, out: &mut Vec<(NodeId, rigor_parse::Span, String)>) {
    out.retain(|(id, w, _)| {
        let pattern_bind =
            matches!(ast.get(*id), Node::UnmodeledWrite { names, .. } if !names.is_empty());
        (!ast.in_inert_carrier(*w) || (pattern_bind && !ast.in_outer_inert_carrier(*w)))
            && !ast.in_blocked_carrier(*w)
    });
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
    let iterated_bodies = loop_body_spans(ast);
    out.retain(|(id, w, name, method, _)| {
        (!ast.in_inert_carrier(*w)
            // …unless an iterated body's writeback TEXT SCAN covers the
            // position — `while c; END { h[:a] = 9 }; break; end` still
            // drops `h[:a]` post-loop on the oracle (rigor-rs#363
            // review). The operand never evaluates, so only a
            // CONTENT_MUTATORS mark survives here — a compound ATTRIBUTE
            // write's widening stays suppressed (`END { h.default ||=
            // 0 }` keeps the record on both).
            || (is_content_mutator(method) && ast.in_scanned_inert_carrier(*w)))
            // A mutation mark under a `Recovered::blocked` position never
            // lands — unless an ITERATED body's writeback text scan covers
            // it (rigor-rs#312) or the mark is a compound ATTRIBUTE write,
            // whose `widen_attribute_write` the reference lands even in
            // never-evaluated positions (rigor-rs#343, rigor-rs#357).
            && (!ast.in_blocked_carrier(*w)
                || ast.in_iterative_blocked_carrier(*w)
                || matches!(ast.get(*id), Node::AttrWrite { .. }))
            && !scopes.iter().any(|s| s.0 <= w.0 && w.1 <= s.1)
            // rigor-rs#409: a content-mutation mark inside a `while`/`until`
            // body survives a deeper shadow scope — the iterated body's
            // writeback text scan matches the receiver name at any depth,
            // so `|h|`'s own `h[:a] = 9` still rejoins the OUTER `h`.
            // Compound ATTRIBUTE writes are not scanned
            // (`content_mutation_target` reads mutator CallNodes and index
            // writes only), so they keep the shadow filter.
            && !shadow_scopes.iter().any(|(descendants, bound)| {
                descendants.contains(id)
                    && bound.contains(name)
                    // rigor-rs#501: same depth pairing as
                    // `collect_flow_writes` — the iterated body
                    // rescues the mark from the shadow only when the
                    // shadow scope sits inside it — and only for a
                    // scanned (content-mutator) mark, never a compound
                    // attribute write.
                    && !(is_content_mutator(method)
                        && iterated_bodies.iter().any(|b| {
                            contains_span(*b, *w)
                                && descendants.iter().all(|&d| {
                                    contains_span(*b, ast.get(d).span())
                                })
                        }))
            })
    });
    out.into_iter()
        .map(|(_, s, n, m, k)| (s, n, m, k))
        .collect()
}

/// Every compound ATTRIBUTE write a literal block/lambda body's OWN scope
/// evaluates — `(owning body node, write span, receiver name)` — for the
/// use-site replay `check_env_at` runs inside a barrier scope
/// (rigor-rs#366). `x.each { h.default ||= 0; h[:a].m }` still runs
/// `widen_attribute_write` — inside the block's scope — dropping every
/// indexed narrowing rooted at `h` for the in-body reads that follow it,
/// while the OUTER env's records survive. `evaluated` is `false` there (the
/// effect never lands on the enclosing scope, rigor-rs#343), so these live
/// in a separate side table keyed by the INNERMOST deferred body: a write
/// in a nested block belongs to the inner body, and one in a
/// `def`/`class`/`module` body to no closure's scope — its `h` is a
/// different local entirely.
///
/// The writer must be a shape mutator exactly as `toplevel_mutations`
/// requires (`IndexedNarrowing.mutator?` — `h.foo ||= 0` drops nothing), and
/// the receiver a bare local, as `widen_attribute_write`'s
/// `local_variable_node?` gate requires. `None`-owner writes (top level or
/// a fresh scope) are `toplevel_mutations`' business.
/// Whether `node` owns a deferred body whose writes
/// [`closure_mutations`] keys to the node itself — a `Call` with a
/// literal block (`block_span` set) or a `Lambda`. Shared with
/// `flow_eval`'s `closure_descend`, which re-keys the replay owner on
/// exactly this predicate: the collection key and the replay must agree
/// (rigor-rs#379).
pub(crate) fn owns_deferred_body(node: &Node) -> bool {
    matches!(
        node,
        Node::Call {
            block_span: Some(_),
            ..
        } | Node::Lambda { .. }
    )
}

pub(crate) fn closure_mutations(
    ast: &LoweredAst,
) -> Vec<(NodeId, rigor_parse::Span, String)> {
    let mut out = Vec::new();
    // (node, innermost deferred body it belongs to). `NodeId` suffices —
    // the owner's identity is what the replay matches, never its span.
    let mut stack: Vec<(NodeId, Option<NodeId>)> = vec![(ast.root(), None)];
    while let Some((id, owner)) = stack.pop() {
        let n = ast.get(id);
        if owner.is_some() {
            if let Node::AttrWrite {
                receiver: Some(r),
                write_name,
                closure_evaluated: true,
                span,
                ..
            } = n
            {
                // `toplevel_mutations`' `in_inert_carrier` retain, mirrored:
                // a write inside a `defined?` operand / `END { }` /
                // `BEGIN { }` / `super`/`yield` operand never evaluates, so
                // replaying it inside the block's scope drops indexed
                // records the reference keeps — `each { END {
                // h.default ||= 0 }; h[:a].frobnicate }` must keep
                // `h[:a]`'s `"s" | 1` (rigor-rs#381).
                if !ast.in_inert_carrier(*span)
                    && is_shape_mutator(write_name)
                    && let Node::LocalVariableRead { name, .. } = ast.get(*r)
                {
                    out.push((owner.unwrap(), *span, name.clone()));
                }
            }
        }
        match n {
            Node::Call {
                receiver,
                args,
                block_body,
                ..
            } => {
                // A `&expr` block-pass (`block_span` `None`) is an operand
                // evaluated in the CALL's scope — its writes stay owned by
                // the enclosing body; a literal block body is the next
                // innermost deferred scope.
                let inner = if owns_deferred_body(n) { Some(id) } else { owner };
                for &c in receiver.iter().chain(args.iter()) {
                    stack.push((c, owner));
                }
                for &c in block_body {
                    stack.push((c, inner));
                }
            }
            Node::Lambda { body, .. } => {
                for &c in body {
                    stack.push((c, Some(id)));
                }
            }
            Node::Definition { body, .. }
            | Node::ClassDef { body, .. }
            | Node::ModuleDef { body, .. } => {
                for &c in body {
                    stack.push((c, None));
                }
            }
            _ => {
                let mut children = Vec::new();
                node_child_ids(n, &mut children);
                for c in children {
                    stack.push((c, owner));
                }
            }
        }
    }
    out
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
    let mut out = argument_floor_writes(ast, source);

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

/// Upstream v0.4.7 (ADR-127, #2515 and its follow-ups) — the callee
/// mutation floor, as an FP-safe over-approximation. The reference floors a
/// mutable local a call passes when the callee's write census (aliases,
/// `tap`, keyword / rest / kwrest parameters, hand-offs, returns, blocks;
/// `callee_write_census.rb`, `callee_mutation_chain.rb`) says the body may
/// write it, and outright (`FLOOR_ALL` / `FLOOR_EVERY` / `OPAQUE_CALLEES`)
/// for an inherited or ambiguous def, a `method_missing`, a literal
/// definer and an opaque sink. The port does not resolve receivers, so it
/// floors every local a call passes when the call MAY reach a project body:
///
/// - the call's name has a project body in any form
///   ([`SourceIndex::call_may_mutate_arguments`]), or the project defines
///   `method_missing`;
/// - an opaque sink: `call` / `instance_exec` / `class_exec` / `module_exec`
///   on an explicit receiver, or a `send` / `__send__` / `public_send`
///   whose target is no literal (a literal target is read as that name).
///
/// A local is passed when its bare read is an argument value — positional,
/// a keyword value, `*splat` / `**splat`, nested in a literal, an arm of a
/// conditional, or the receiver of `tap` / `itself` — or when it was stored
/// into a passed local (`args = [p, o]; add(*args)`, `args << o`,
/// `y = o`), transitively. A local whose every write in the file is an
/// immutable literal (Integer, Float, Symbol, `nil`, `true`, `false`) is
/// skipped: the reference gates its floor on a mutable carrier, so `n = 1;
/// go(n); n == 1` keeps folding there.
///
/// Every entry only widens, so an extra one costs coverage and never adds a
/// diagnostic. Keyed by the call's span, like the receiver-side mutator
/// entry: a call inside a block widens at the enclosing block call (#2400).
pub(crate) fn argument_floor_writes(
    ast: &LoweredAst,
    source: &SourceIndex,
) -> Vec<(rigor_parse::Span, String)> {
    use std::collections::HashMap;
    let mut out = Vec::new();
    // Which locals each local may hold: `y = <expr>` and the content
    // mutators `y << x` / `y.push(x)` / `y[k] = x` / ….
    let mut holds: HashMap<String, HashSet<String>> = HashMap::new();
    // Names with a write whose value is not an immutable literal (or that
    // is not a plain write at all).
    let mut mutable_names: HashSet<String> = HashSet::new();
    for (_, n) in ast.iter() {
        match n {
            Node::LocalVariableWrite { name, value, .. } => {
                let mut held = Vec::new();
                passed_locals(ast, *value, &mut held);
                holds.entry(name.clone()).or_default().extend(held);
                if !matches!(
                    ast.get(*value),
                    Node::IntegerLit { .. }
                        | Node::FloatLit { .. }
                        | Node::SymbolLit { .. }
                        | Node::NilLit { .. }
                        | Node::TrueLit { .. }
                        | Node::FalseLit { .. }
                ) {
                    mutable_names.insert(name.clone());
                }
            }
            Node::LocalVariableOpWrite { name, .. } => {
                mutable_names.insert(name.clone());
            }
            Node::MultiWrite { targets, .. } => {
                for (name, _) in targets.bound_names() {
                    mutable_names.insert(name);
                }
            }
            Node::Call { receiver: Some(r), method, args, .. }
                if FLOOR_STORE_METHODS.contains(&method.as_str()) =>
            {
                if let Node::LocalVariableRead { name, .. } = ast.get(*r) {
                    let mut held = Vec::new();
                    for &a in args {
                        passed_locals(ast, a, &mut held);
                    }
                    holds.entry(name.clone()).or_default().extend(held);
                }
            }
            Node::IndexWrite { receiver: Some(r), value, .. } => {
                if let Node::LocalVariableRead { name, .. } = ast.get(*r) {
                    let mut held = Vec::new();
                    passed_locals(ast, *value, &mut held);
                    holds.entry(name.clone()).or_default().extend(held);
                }
            }
            _ => {}
        }
    }
    // A name never plainly written (a parameter, a block or pattern binding)
    // may hold anything.
    let written: HashSet<&str> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::LocalVariableWrite { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    let immutable = |name: &str| written.contains(name) && !mutable_names.contains(name);
    let def_spans: Vec<rigor_parse::Span> = ast
        .iter()
        .filter_map(|(_, n)| matches!(n, Node::Definition { .. }).then(|| n.span()))
        .collect();
    let inside = |o: rigor_parse::Span, i: rigor_parse::Span| o.0 <= i.0 && i.1 <= o.1;
    for (_, n) in ast.iter() {
        let Node::Call { receiver, method, args, span, .. } = n else {
            continue;
        };
        if args.is_empty() || !call_may_floor(ast, source, receiver.is_some(), method, args) {
            continue;
        }
        let exempt = readonly_positions(ast, source, n);
        let mut passed = Vec::new();
        for (i, &a) in args.iter().enumerate() {
            if !exempt.contains(&i) {
                passed_locals(ast, a, &mut passed);
            }
        }
        // The transitive closure over `holds`.
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(name) = passed.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            if let Some(h) = holds.get(&name) {
                passed.extend(h.iter().cloned());
            }
        }
        // A local holding a floored one changed too (`alias_args = args`).
        loop {
            let grown: Vec<String> = holds
                .iter()
                .filter(|(z, held)| !seen.contains(*z) && held.iter().any(|h| seen.contains(h)))
                .map(|(z, _)| z.clone())
                .collect();
            if grown.is_empty() {
                break;
            }
            seen.extend(grown);
        }
        // `floor_splat_arguments`' documented limit: only the type says which
        // local a splatted element is, so an equal literal another local
        // holds floors too. Over-approximated as every local of the scope.
        let splat = args.iter().any(|&a| {
            matches!(ast.get(a), Node::Statements { kind: StatementsKind::Recovered, .. })
        });
        if splat && !seen.is_empty() {
            let scope = def_spans
                .iter()
                .filter(|d| inside(**d, *span))
                .min_by_key(|d| d.1 - d.0)
                .copied();
            for (_, w) in ast.iter() {
                let Node::LocalVariableWrite { name, span: ws, .. } = w else {
                    continue;
                };
                let innermost = def_spans
                    .iter()
                    .filter(|d| inside(**d, *ws))
                    .min_by_key(|d| d.1 - d.0)
                    .copied();
                if innermost == scope {
                    seen.insert(name.clone());
                }
            }
        }
        let mut names: Vec<String> = seen.into_iter().filter(|n| !immutable(n)).collect();
        names.sort();
        out.extend(names.into_iter().map(|name| (*span, name)));
    }
    out
}

/// [`argument_floor_writes`] for the TOP-LEVEL scope: a call inside a
/// `def` / `class` / `module` body floors that scope's locals, not the
/// top-level ones. A block's call still counts (it may name a captured
/// top-level local; a name the block binds itself only over-widens).
pub(crate) fn toplevel_argument_floor(
    ast: &LoweredAst,
    source: &SourceIndex,
) -> Vec<(rigor_parse::Span, String)> {
    let (scopes, _) = toplevel_scope_filters(ast);
    let mut out = argument_floor_writes(ast, source);
    out.retain(|(w, _)| !scopes.iter().any(|s| s.0 <= w.0 && w.1 <= s.1));
    drop_inert_writes(ast, &mut out);
    out
}

/// The nodes whose value an OVER-APPROXIMATED callee floor may have made
/// less precise than the reference's (PR #481 review, scratch
/// `review/f/e03.rb`). [`argument_floor_writes`] floors a local the
/// reference may keep precise (its write census proves the callee
/// read-only). A floored local reads `Dynamic`, which fires nothing itself,
/// but a JOIN whose predicate the reference folds through that local keeps
/// both arms here: `go(h); w = h[:f] == "json" ? 1 : "s"; w.foo` reads
/// `"s" | 1` where the reference reads `"s"` — a matched row with another
/// message, or a `nil` arm the reference never has. The rules decline a
/// diagnostic whose judged value reaches one of these nodes
/// ([`diagnostic_reads_floor_taint`]).
///
/// Floored: a `(call, local)` entry the Pass-1e census does not prove (it
/// proves what the reference floors too), for a local some write of which
/// in its scope (the innermost `def`, or the top level) is a literal or
/// constant carrier the reference could fold through. Tainted: a join
/// (`if` / ternary / `case` / loop / `&&` / `||`) whose predicate's value
/// reaches a floored or tainted local; a local written inside a tainted
/// join's region, or from a value reaching a tainted join or local; to a
/// fixpoint. The result holds the tainted joins and every read of a
/// tainted local.
pub fn floor_tainted_reads(ast: &LoweredAst, source: &SourceIndex) -> HashSet<NodeId> {
    let floors = argument_floor_writes(ast, source);
    if floors.is_empty() {
        return HashSet::new();
    }
    // Proven floors: the old positional census.
    let mut proven: HashSet<(rigor_parse::Span, String)> = HashSet::new();
    for (_, n) in ast.iter() {
        if let Node::Call { method, args, span, .. } = n {
            for (i, &a) in args.iter().enumerate() {
                if let Node::LocalVariableRead { name, .. } = ast.get(a) {
                    if source.method_mutates_param(method, i) {
                        proven.insert((*span, name.clone()));
                    }
                }
            }
        }
    }
    let def_spans: Vec<rigor_parse::Span> = ast
        .iter()
        .filter_map(|(_, n)| matches!(n, Node::Definition { .. }).then(|| n.span()))
        .collect();
    let scope_of = |s: rigor_parse::Span| {
        def_spans
            .iter()
            .filter(|d| d.0 <= s.0 && s.1 <= d.1)
            .min_by_key(|d| d.1 - d.0)
            .copied()
    };
    type Key = (Option<rigor_parse::Span>, String);
    // Locals with a literal / constant carrier write.
    let mut carriers: HashSet<Key> = HashSet::new();
    for (_, n) in ast.iter() {
        if let Node::LocalVariableWrite { name, value, span, .. } = n {
            let mut root = *value;
            while let Node::Call { receiver: Some(r), .. } = ast.get(root) {
                root = *r;
            }
            if matches!(
                ast.get(root),
                Node::ArrayLit { .. }
                    | Node::HashLit { .. }
                    | Node::StringLit { .. }
                    | Node::ConstantRead { .. }
            ) {
                carriers.insert((scope_of(*span), name.clone()));
            }
        }
    }
    let floored: HashSet<Key> = floors
        .into_iter()
        .filter(|f| !proven.contains(f))
        .map(|(s, name)| (scope_of(s), name))
        .filter(|k| carriers.contains(k))
        .collect();
    if floored.is_empty() {
        return HashSet::new();
    }
    let key_of = |id: NodeId| match ast.get(id) {
        Node::LocalVariableRead { name, span } => Some((scope_of(*span), name.clone())),
        _ => None,
    };
    // Every join: its node, the value roots of its predicate, its span.
    let mut conds: Vec<(NodeId, Vec<NodeId>, rigor_parse::Span)> = Vec::new();
    for (id, n) in ast.iter() {
        let preds: Vec<NodeId> = match n {
            Node::If { predicate, .. } => vec![*predicate],
            Node::Case { predicate, branches, .. } => {
                let mut p: Vec<NodeId> = predicate.iter().copied().collect();
                for &b in branches {
                    if let Node::When { conditions, .. } = ast.get(b) {
                        p.extend_from_slice(conditions);
                    }
                }
                p
            }
            Node::Loop { predicate: Some(p), .. } => vec![*p],
            Node::Logical { left, .. } => vec![*left],
            _ => continue,
        };
        let mut roots = Vec::new();
        for p in preds {
            value_roots(ast, p, &mut roots);
        }
        if !roots.is_empty() {
            conds.push((id, roots, n.span()));
        }
    }
    // Each plain local write: its target, the joins around it, and its
    // value's roots.
    let mut writes: Vec<(Key, Vec<NodeId>, Vec<NodeId>)> = Vec::new();
    for (_, n) in ast.iter() {
        let (Node::LocalVariableWrite { name, value, span, .. }
        | Node::LocalVariableOpWrite { name, value, span, .. }) = n
        else {
            continue;
        };
        let around: Vec<NodeId> = conds
            .iter()
            .filter(|(_, _, r)| r.0 <= span.0 && span.1 <= r.1)
            .map(|(c, _, _)| *c)
            .collect();
        let mut roots = Vec::new();
        value_roots(ast, *value, &mut roots);
        writes.push(((scope_of(*span), name.clone()), around, roots));
    }
    let mut tainted: HashSet<Key> = floored.clone();
    let mut tainted_joins: HashSet<NodeId> = HashSet::new();
    loop {
        let mut grown = false;
        for (cid, roots, _) in &conds {
            if !tainted_joins.contains(cid)
                && roots.iter().any(|&r| key_of(r).is_some_and(|k| tainted.contains(&k)))
            {
                tainted_joins.insert(*cid);
                grown = true;
            }
        }
        for (target, around, roots) in &writes {
            if tainted.contains(target) {
                continue;
            }
            let hit = around.iter().any(|c| tainted_joins.contains(c))
                || roots.iter().any(|&r| {
                    tainted_joins.contains(&r) || key_of(r).is_some_and(|k| tainted.contains(&k))
                });
            if hit {
                tainted.insert(target.clone());
                grown = true;
            }
        }
        if !grown {
            break;
        }
    }
    let mut out: HashSet<NodeId> = tainted_joins;
    for (id, _) in ast.iter() {
        if let Some(k) = key_of(id) {
            // A floored local alone reads `Dynamic` and fires nothing; only
            // a local a tainted join wrote carries a wider-than-reference
            // value.
            if tainted.contains(&k) && !floored.contains(&k) {
                out.insert(id);
            }
        }
    }
    out
}

/// The nodes an expression's VALUE may come from: a local read, a join
/// (and its arms), a literal container's elements, a call's receiver (and
/// a receiverless call's arguments, which a parameter-bound return may
/// hand back). An interpolation, an explicit-receiver call's arguments, a
/// splat and a predicate are not value sources.
fn value_roots(ast: &LoweredAst, id: NodeId, out: &mut Vec<NodeId>) {
    match ast.get(id) {
        Node::LocalVariableRead { .. } => out.push(id),
        Node::If { then_body, else_body, .. } => {
            out.push(id);
            for &e in then_body.iter().chain(else_body) {
                value_roots(ast, e, out);
            }
        }
        Node::Case { branches, else_body, .. } => {
            out.push(id);
            for &b in branches {
                if let Node::When { body, .. } = ast.get(b) {
                    for &e in body {
                        value_roots(ast, e, out);
                    }
                }
            }
            for &e in else_body {
                value_roots(ast, e, out);
            }
        }
        Node::Logical { left, right, .. } => {
            out.push(id);
            value_roots(ast, *left, out);
            value_roots(ast, *right, out);
        }
        Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
            if let Some(&last) = body.last() {
                value_roots(ast, last, out);
            }
        }
        Node::BeginRescue { body, .. } => {
            if let Some(&last) = body.last() {
                value_roots(ast, last, out);
            }
        }
        Node::ArrayLit { elements, .. } | Node::HashLit { elements, .. } => {
            for &e in elements {
                value_roots(ast, e, out);
            }
        }
        Node::LocalVariableWrite { value, .. } | Node::LocalVariableOpWrite { value, .. } => {
            value_roots(ast, *value, out);
        }
        Node::Call { receiver: Some(r), .. } => value_roots(ast, *r, out),
        Node::Call { receiver: None, args, .. } => {
            for &a in args {
                value_roots(ast, a, out);
            }
        }
        _ => {}
    }
}

/// Which part of the expression a diagnostic judges, for
/// [`diagnostic_reads_floor_taint`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaintAnchor {
    /// The receiver of the call whose method name starts at the anchor
    /// (`call.undefined-method`, `call.wrong-arity`,
    /// `call.possible-nil-receiver`).
    Receiver,
    /// Its receiver and arguments (`call.argument-type-mismatch`,
    /// `call.raise-non-exception`).
    ReceiverAndArguments,
    /// Every node starting at the anchor (a flow rule's predicate).
    Expression,
}

/// Whether the value a diagnostic anchored at `start` judges may come from
/// a [`floor_tainted_reads`] node ([`value_roots`]). Over-matching only
/// declines more.
pub fn diagnostic_reads_floor_taint(
    ast: &LoweredAst,
    start: usize,
    anchor: TaintAnchor,
    tainted: &HashSet<NodeId>,
) -> bool {
    let mut roots: Vec<NodeId> = Vec::new();
    for (id, n) in ast.iter() {
        match anchor {
            TaintAnchor::Expression => {
                if n.span().0 == start {
                    value_roots(ast, id, &mut roots);
                    // A predicate's fold reads every operand.
                    if let Node::Call { args, .. } = n {
                        for &a in args {
                            value_roots(ast, a, &mut roots);
                        }
                    }
                }
            }
            _ => {
                if let Node::Call { receiver, args, message_span, .. } = n {
                    if message_span.0 == start {
                        if let Some(r) = receiver {
                            value_roots(ast, *r, &mut roots);
                        }
                        if anchor == TaintAnchor::ReceiverAndArguments {
                            for &a in args {
                                value_roots(ast, a, &mut roots);
                            }
                        }
                    }
                }
            }
        }
    }
    roots.iter().any(|r| tainted.contains(r))
}

/// rigor-rs#363, re-measured at v0.4.7: whether a mutation at `span` inside
/// a lexical loop's `break` argument stays off the post-loop scope (the
/// locals-only `join_break_scopes`). At v0.4.7 only a compound ATTRIBUTE
/// write (`break h.default ||= 0`, a `Node::AttrWrite`) keeps the indexed
/// record there; a mutator CALL — `break h.merge!(…)`, `h.clear`,
/// `h.delete(:a)`, `h.compare_by_identity`, `(h.default = 0)`, `h[:a] << 2`,
/// `for …; break h.store(:a, 9)`, a block's `h[:a] = 9` — drops it on both
/// v0.4.6 and v0.4.7 (scratch `frag/brk/t05`–`t16`, `brk3/w1`–`w7`).
/// Answering `false` only keeps a drop, so it can never add a diagnostic.
pub(crate) fn break_join_suppresses(ast: &LoweredAst, span: rigor_parse::Span, method: &str) -> bool {
    ast.locals_only_suppresses(span, is_content_mutator(method))
        && ast.iter().any(|(_, n)| matches!(n, Node::AttrWrite { span: s, .. } if *s == span))
}

/// Receiver methods whose result is never the receiver nor one of its
/// elements, and which write nothing: a parameter read only through these is
/// read-only for [`readonly_positions`]. `to_s` / `to_a` / `to_h` are out
/// (they may return the receiver: `CalleeWriteCensus::SELF_RETURNERS`).
const READONLY_METHODS: &[&str] = &[
    "size", "length", "count", "empty?", "any?", "none?", "nil?", "key?", "has_key?",
    "include?", "member?", "is_a?", "kind_of?", "instance_of?", "respond_to?", "frozen?",
    "hash", "==", "!=", "eql?", "equal?", "inspect",
];

/// Store methods whose result is their receiver: a parameter handed to one
/// on another parameter is held, not written (`acc.push(flag)`).
const HOLDING_STORES: &[&str] = &["push", "<<", "append", "unshift", "add"];

/// The argument positions of `call` the reference keeps reading precisely
/// after it: a receiverless call of a name exactly one project body answers
/// (a plain top-level `def` of this file with plain positional parameters,
/// no `method_missing` anywhere), made at the top level or in a top-level
/// `def` outside any block, whose parameter at that position the def only
/// reads ([`READONLY_METHODS`] receivers, or a [`HOLDING_STORES`] argument
/// on another parameter the def only stores into or reads). The reference's
/// write census finds no write there, so its floor leaves the local alone
/// (`def ro(h) = h.size; ro(h7); h7[:a].upcase` still fires). Everything
/// else is empty: an allow-list, so a shape it does not name floors.
fn readonly_positions(ast: &LoweredAst, source: &SourceIndex, call: &Node) -> HashSet<usize> {
    let none = HashSet::new();
    let Node::Call { receiver: None, method, args, span, args_plain_positional: true, .. } = call
    else {
        return none;
    };
    if !source.unique_project_body(method) {
        return none;
    }
    let inside = |o: rigor_parse::Span, i: rigor_parse::Span| o.0 <= i.0 && i.1 <= o.1;
    // The call's context: no class/module body, block, lambda or
    // non-top-level def around it.
    let mut def_node: Option<NodeId> = None;
    for (id, n) in ast.iter() {
        let s = n.span();
        match n {
            Node::ClassDef { .. } | Node::ModuleDef { .. } | Node::Lambda { .. }
                if inside(s, *span) =>
            {
                return none;
            }
            Node::Call { block_body, .. } if !block_body.is_empty() => {
                if let Some(h) = span_hull(ast, block_body) {
                    if inside(h, *span) {
                        return none;
                    }
                }
            }
            Node::Definition { name, .. } if name.as_deref() == Some(method.as_str()) => {
                if def_node.is_some() {
                    return none;
                }
                def_node = Some(id);
            }
            Node::Definition { name: None, .. } if inside(s, *span) => return none,
            _ => {}
        }
    }
    let Some(def_id) = def_node else { return none };
    let Node::Definition { params: Some(names), span: dspan, .. } = ast.get(def_id) else {
        return none;
    };
    if names.len() != args.len() {
        return none;
    }
    // The def itself sits at the top level: no scope node encloses it.
    let top_level = !ast.iter().any(|(id, n)| {
        id != def_id
            && matches!(n, Node::ClassDef { .. } | Node::ModuleDef { .. } | Node::Definition { .. }
                | Node::Lambda { .. } | Node::Call { .. })
            && n.span() != *dspan
            && inside(n.span(), *dspan)
    });
    if !top_level {
        return none;
    }
    // Parent links over the def's subtree.
    let mut parent: std::collections::HashMap<NodeId, NodeId> = std::collections::HashMap::new();
    let mut stack = vec![def_id];
    let mut seen: HashSet<NodeId> = HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let mut kids = Vec::new();
        node_child_ids(ast.get(id), &mut kids);
        for k in kids {
            parent.entry(k).or_insert(id);
            stack.push(k);
        }
    }
    // Every node inside the def's span must be linked: an unlinked read
    // could be anywhere.
    let in_def: Vec<(NodeId, &Node)> = ast
        .iter()
        .filter(|(id, n)| *id != def_id && inside(*dspan, n.span()))
        .collect();
    if in_def.iter().any(|(id, _)| !seen.contains(id)) {
        return none;
    }
    let rebound = |p: &str| {
        in_def.iter().any(|(_, n)| match n {
            Node::LocalVariableWrite { name, .. } | Node::LocalVariableOpWrite { name, .. } => {
                name == p
            }
            Node::MultiWrite { targets, .. } => targets.bound_names().iter().any(|(b, _)| b == p),
            Node::UnmodeledWrite { names, .. } => names.iter().any(|b| b == p),
            _ => false,
        })
    };
    let reads = |p: &str| -> Vec<NodeId> {
        in_def
            .iter()
            .filter(|(_, n)| matches!(n, Node::LocalVariableRead { name, .. } if name == p))
            .map(|(id, _)| *id)
            .collect()
    };
    let read_only_receiver = |id: NodeId| match parent.get(&id).map(|&p| ast.get(p)) {
        Some(Node::Call { receiver: Some(r), method, block_body, .. }) => {
            *r == id && block_body.is_empty() && READONLY_METHODS.contains(&method.as_str())
        }
        _ => false,
    };
    let store_receiver = |id: NodeId| match parent.get(&id).map(|&p| ast.get(p)) {
        Some(Node::Call { receiver: Some(r), method, block_body, .. }) => {
            *r == id && block_body.is_empty() && HOLDING_STORES.contains(&method.as_str())
        }
        _ => false,
    };
    // A parameter the def only stores into or reads: nothing takes an
    // element back out of it.
    let store_only = |q: &str| {
        !rebound(q) && reads(q).into_iter().all(|id| read_only_receiver(id) || store_receiver(id))
    };
    let held_by_store = |id: NodeId, p: &str| match parent.get(&id).map(|&x| ast.get(x)) {
        Some(Node::Call { receiver: Some(r), method, args, block_body, .. })
            if block_body.is_empty()
                && HOLDING_STORES.contains(&method.as_str())
                && args.contains(&id) =>
        {
            matches!(ast.get(*r), Node::LocalVariableRead { name, .. }
                if name != p && names.contains(name) && store_only(name))
        }
        _ => false,
    };
    let mut out = HashSet::new();
    for (i, p) in names.iter().enumerate() {
        if !matches!(ast.get(args[i]), Node::LocalVariableRead { .. }) || rebound(p) {
            continue;
        }
        if reads(p).into_iter().all(|id| read_only_receiver(id) || held_by_store(id, p)) {
            out.insert(i);
        }
    }
    out
}

/// The methods whose argument a receiver local holds afterwards (the
/// `holds` table of [`argument_floor_writes`]).
const FLOOR_STORE_METHODS: &[&str] = &[
    "<<", "push", "append", "unshift", "prepend", "insert", "[]=", "store", "concat", "add",
    "merge!", "update", "replace",
];

/// Whether a call of `method` may run a project body (or code no def shows)
/// against its arguments. See [`argument_floor_writes`].
fn call_may_floor(
    ast: &LoweredAst,
    source: &SourceIndex,
    has_receiver: bool,
    method: &str,
    args: &[NodeId],
) -> bool {
    if source.call_may_mutate_arguments(method) {
        return true;
    }
    match method {
        // A receiverless `call` is a self-call to a def of that name;
        // `instance_exec` and the rest run their block on any receiver.
        "call" => has_receiver,
        "instance_exec" | "class_exec" | "module_exec" => true,
        "send" | "__send__" | "public_send" => match ast.get(args[0]) {
            Node::SymbolLit { value, .. } | Node::StringLit { value, .. } => {
                source.call_may_mutate_arguments(value)
            }
            _ => true,
        },
        _ => false,
    }
}

/// The locals whose object `id`'s value may be: a bare read, through a
/// literal container, a splat carrier, a conditional arm, an assignment's
/// value or a `tap` / `itself` receiver. See [`argument_floor_writes`].
fn passed_locals(ast: &LoweredAst, id: NodeId, out: &mut Vec<String>) {
    match ast.get(id) {
        Node::LocalVariableRead { name, .. } => out.push(name.clone()),
        Node::ArrayLit { elements, .. } | Node::HashLit { elements, .. } => {
            for &e in elements {
                passed_locals(ast, e, out);
            }
        }
        Node::Statements { body, .. } => {
            for &e in body {
                passed_locals(ast, e, out);
            }
        }
        Node::If { then_body, else_body, .. } => {
            for &e in then_body.iter().chain(else_body) {
                passed_locals(ast, e, out);
            }
        }
        Node::Logical { left, right, .. } => {
            passed_locals(ast, *left, out);
            passed_locals(ast, *right, out);
        }
        // An `else` arm lowers as a `BeginRescue` carrier.
        Node::BeginRescue { body, clauses, .. } => {
            for &e in body.iter().chain(clauses.iter().flat_map(|c| c.body.iter())) {
                passed_locals(ast, e, out);
            }
        }
        Node::Case { branches, else_body, .. } => {
            for &e in branches.iter().chain(else_body) {
                passed_locals(ast, e, out);
            }
        }
        Node::When { body, .. } => {
            for &e in body {
                passed_locals(ast, e, out);
            }
        }
        Node::LocalVariableWrite { value, .. } | Node::LocalVariableOpWrite { value, .. } => {
            passed_locals(ast, *value, out);
        }
        Node::Call { receiver: Some(r), method, .. } if method == "tap" || method == "itself" => {
            passed_locals(ast, *r, out);
        }
        _ => {}
    }
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

/// Upstream #1837 (rigor #1231 / #1457, `widen_clause_entry`): a `rescue`
/// arm runs after the `begin` body raised from any point in it, and an
/// `ensure` clause after the body, its `else` or a rescue arm raised or
/// finished, so the clause reads each local those wrote as `Dynamic[top]`.
/// `region` is the span of the writing statements (the protected body for a
/// rescue arm; everything before the `ensure` statements for an `ensure`).
/// A write inside a `def` / `class` / `module` nested in the region belongs to
/// another scope and does not count; a block's write does (it may rebind an
/// outer local). Only a name `env` already binds, to something other than
/// `Dynamic[top]`, is rebound. `writes` may over-approximate the reference's
/// rebind set (it carries in-place mutations): every extra name only widens,
/// which can take a diagnostic away and never adds one.
pub(crate) fn widen_clause_entry(
    writes: &[(rigor_parse::Span, String)],
    region: rigor_parse::Span,
    scope_spans: &[rigor_parse::Span],
    env: &mut TypeEnv,
    interner: &mut Interner,
) {
    let u = interner.untyped();
    let inside = |outer: rigor_parse::Span, inner: rigor_parse::Span| {
        outer.0 <= inner.0 && inner.1 <= outer.1
    };
    for (wspan, name) in writes {
        if !inside(region, *wspan)
            || scope_spans.iter().any(|s| inside(region, *s) && inside(*s, *wspan))
        {
            continue;
        }
        if let Some(ty) = env.get_mut(name) {
            *ty = u;
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
    /// The span of every `def` / `class` / `module` / `class <<` node — the
    /// reference's `SCOPE_BODY_NODES`, whose writes are another scope's
    /// ([`widen_clause_entry`], upstream #1837).
    pub scope_spans: Vec<rigor_parse::Span>,
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
            Node::Definition { span, .. }
            | Node::ClassDef { span, .. }
            | Node::ModuleDef { span, .. } => flow.scope_spans.push(*span),
            _ => {}
        }
    }
    flow
}
