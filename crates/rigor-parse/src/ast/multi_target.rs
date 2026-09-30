//! Multiple-assignment and `for`-index targets: the owned `MultiTargets` tree
//! and its structural (non-arena) lowering.

use crate::ruby_prism::{self, Node as PrismNode};

use super::{
    collect_recoverable_children, constant_string, integer_value, span_of, Recovered, ScopeMarks,
    Span,
};

/// `IndexedNarrowing.stable_key` (`indexed_narrowing.rb` `STABLE_KEY_NODES`)
/// for an index TARGET: the literal index a `h[k]` store addresses — Symbol /
/// String / Integer literals only; a variable, splat or interpolated key
/// declines. Mirrors [`rigor_types::ShapeKey`]'s three stable variants
/// (rigor-parse cannot see that type); `rigor-infer` converts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexTargetKey {
    Sym(String),
    Str(String),
    Int(i64),
}

/// The `[]=` stores an index target performs: `(receiver local, target span,
/// drop key)` triples — the name-keyed half of the reference's
/// `Result#index_targets`, plus the key the store invalidates. `drop key` is
/// `Some` only when the receiver is a bare `LocalVariableReadNode` and the
/// FIRST index argument is a [`IndexTargetKey`] literal — the reference's
/// `IndexedNarrowing.invalidate_indexed_write` (`indexed_narrowing.rb:142`),
/// which `widen_index_target` runs on the `IndexTargetNode` exactly as a
/// `[]=` `CallNode` (`stable_address` reads `receiver`/`arguments`; a
/// multi-index `h[:a, :b]` still drops `(h, :a)`).
pub type IndexWrites = Vec<(String, Span, Option<IndexTargetKey>)>;

/// One target slot of a multiple assignment (`a, (b, c), *rest = rhs`).
///
/// Mirrors the target kinds the reference's `MultiTargetBinder` recognises
/// (`reference/rigor/lib/rigor/inference/multi_target_binder.rb:29-46`):
/// a `LocalVariableTargetNode` binds a name, a nested `MultiTargetNode`
/// recurses, and every other target kind (`InstanceVariableTargetNode`,
/// `ConstantTargetNode`, `CallTargetNode`,
/// `ConstantPathTargetNode`, `ImplicitRestNode`, an anonymous `*`) is silently
/// skipped — it has no observable contribution to the local-variable scope.
///
/// A skipped target is still materialised as [`MultiTarget::Ignored`] so it
/// keeps its POSITION: the tuple decomposition is positional, so dropping an
/// ignorable slot would shift every later target onto the wrong element.
///
/// The exception is `IndexTargetNode` (`h[k]`): it binds no name either, but it
/// STORES through `[]=` on its receiver — a multi-assign slot
/// (`h[:a], z = 1, 2`), a `for` index or a `rescue =>` reference — so the
/// receiver's local(s) must widen exactly as `h[k] = v` widens them (upstream
/// rigor#1209/#1211, `IndexWriteWidening.widen` → `MutationWidening.
/// widen_receiver_aliases`). [`MultiTarget::Index`] carries the receiver's
/// mutated LOCAL reads (`receivers`), the local-only port of
/// `ReceiverAlias.mutated_reads`; the stored slot's value is not modelled (the
/// flow passes widen the binding rather than joining content evidence).
#[derive(Clone, Debug)]
pub enum MultiTarget {
    /// A plain local target (`a`). `name_span` is the Prism
    /// `LocalVariableTargetNode` location, which IS the name token — the
    /// name-anchored span the enum's other write variants carry.
    Local { name: String, name_span: Span },
    /// A nested multi-target (`(b, c)` in `a, (b, c) = …`). The binder recurses
    /// with this slot's type as the new right-hand side.
    Nested(MultiTargets),
    /// An index target (`h[k]`). `receivers` is every local the receiver
    /// expression can evaluate to (empty for an ivar/constant/call receiver);
    /// `span` is the whole `IndexTargetNode` location. `key` is the
    /// `[]=` store's stable drop key — `Some` only for a bare-local receiver
    /// with a literal first index argument (`IndexedNarrowing.
    /// invalidate_indexed_write`), so a `receivers` holding it is exactly the
    /// one name `stable_receiver` resolves.
    Index {
        receivers: Vec<String>,
        span: Span,
        key: Option<IndexTargetKey>,
    },
    /// A target with no observable local binding (ivar / constant / call /
    /// const-path target, an implicit rest `a, = …`, an anonymous `*`).
    Ignored { span: Span },
}

impl MultiTarget {
    /// The byte span of this target slot.
    pub fn span(&self) -> Span {
        match self {
            MultiTarget::Local { name_span, .. } => *name_span,
            MultiTarget::Nested(t) => t.span,
            MultiTarget::Index { span, .. } | MultiTarget::Ignored { span } => *span,
        }
    }

    /// Push every local name bound anywhere under this target (recursing into a
    /// nested multi-target) onto `out`, with its name span. An index target
    /// binds no local — its store is reported by [`Self::collect_index_writes`].
    pub fn collect_bound_names(&self, out: &mut Vec<(String, Span)>) {
        match self {
            MultiTarget::Local { name, name_span } => out.push((name.clone(), *name_span)),
            MultiTarget::Nested(t) => t.collect_bound_names(out),
            MultiTarget::Index { .. } | MultiTarget::Ignored { .. } => {}
        }
    }

    /// Push every `(receiver local, target span, drop key)` this target
    /// stores through `[]=` (recursing into a nested multi-target and a
    /// `*rest` slot) — the name-keyed half of the reference's
    /// `Result#index_targets` (`multi_target_binder.rb`), which keys each
    /// write by the target node; the span plays that role here. A receiver
    /// that names no local contributes nothing (a strict decline — never a
    /// new write). The drop key — `Some` only on a bare-local receiver —
    /// rides every emitted entry (there is exactly one then), matching the
    /// `(local, key)` invalidation `widen_index_target` runs.
    pub fn collect_index_writes(&self, out: &mut IndexWrites) {
        match self {
            MultiTarget::Index {
                receivers,
                span,
                key,
            } => {
                out.extend(receivers.iter().map(|r| (r.clone(), *span, key.clone())));
            }
            MultiTarget::Nested(t) => t.collect_index_writes(out),
            MultiTarget::Local { .. } | MultiTarget::Ignored { .. } => {}
        }
    }
}

/// The `lefts` / `rest` (splat) / `rights` target triple shared by Prism's
/// `MultiWriteNode` (`a, b = rhs`) and `MultiTargetNode` (the nested `(b, c)`
/// form) — the reference treats them uniformly for exactly this reason
/// (`multi_target_binder.rb:20-22`).
///
/// A composite child-group struct in the style of [`RescueClause`].
///
/// [`RescueClause`]: crate::ast::RescueClause
#[derive(Clone, Debug)]
pub struct MultiTargets {
    /// Targets before the splat (all of them when there is no splat).
    pub lefts: Vec<MultiTarget>,
    /// The `*rest` slot, when the target list has one. Its mere PRESENCE
    /// changes the decomposition (the reference's `rest_present:`), so an
    /// anonymous `*` and an implicit rest (`a, = …`) are recorded as
    /// [`MultiTarget::Ignored`] rather than dropped.
    pub rest: Option<Box<MultiTarget>>,
    /// Targets after the splat.
    pub rights: Vec<MultiTarget>,
    pub span: Span,
}

impl MultiTargets {
    /// Every local name bound by this target tree, in source order.
    pub fn collect_bound_names(&self, out: &mut Vec<(String, Span)>) {
        for t in &self.lefts {
            t.collect_bound_names(out);
        }
        if let Some(r) = &self.rest {
            r.collect_bound_names(out);
        }
        for t in &self.rights {
            t.collect_bound_names(out);
        }
    }

    /// Convenience: the bound names of this target tree.
    pub fn bound_names(&self) -> Vec<(String, Span)> {
        let mut out = Vec::new();
        self.collect_bound_names(&mut out);
        out
    }

    /// Push every `(receiver local, target span)` stored through `[]=` under
    /// this target tree onto `out` — fixed, nested and splatted index targets
    /// alike, in source order.
    pub fn collect_index_writes(&self, out: &mut IndexWrites) {
        for t in &self.lefts {
            t.collect_index_writes(out);
        }
        if let Some(r) = &self.rest {
            r.collect_index_writes(out);
        }
        for t in &self.rights {
            t.collect_index_writes(out);
        }
    }

    /// Convenience: the `[]=` index-target writes of this target tree. The
    /// owning write form (`MultiWriteNode`, `for` index, `rescue =>`) widens
    /// each named local as `h[k] = v` does.
    pub fn index_writes(&self) -> IndexWrites {
        let mut out = Vec::new();
        self.collect_index_writes(&mut out);
        out
    }
}

/// Lower a Prism `lefts` / `rest` / `rights` target triple (shared by
/// `MultiWriteNode` and the nested `MultiTargetNode`) into the owned
/// [`MultiTargets`] group.
///
/// Targets are lowered STRUCTURALLY, not into the node arena: a target binds a
/// name, it is not a value expression, so materialising one as an arena node
/// would make it look like a read/write to the span-scanning structural walks.
/// Every target that lowers to [`MultiTarget::Index`] or
/// [`MultiTarget::Ignored`] contributes its RECOVERABLE descendants (local
/// reads / writes / calls) to `recovered`, so the caller can lower them into
/// the arena and keep them visible to the structural walks — the old
/// recovered-children carrier did exactly this. Those embedded expressions
/// are collected with NO join mark and BLOCKED: the reference's
/// `MultiTargetBinder` types them through `index_write_arg_types` without
/// applying their scope effects, so a compound index write inside a target
/// never widens its receiver no matter what scope-joining construct wraps
/// the multi-assign (rigor-rs#312 review).
pub(crate) fn lower_multi_targets<'pr>(
    lefts: &ruby_prism::NodeList<'pr>,
    rest: Option<&PrismNode<'pr>>,
    rights: &ruby_prism::NodeList<'pr>,
    span: Span,
    recovered: &mut Vec<Recovered<'pr>>,
    iterative: bool,
) -> MultiTargets {
    MultiTargets {
        lefts: lefts
            .iter()
            .map(|t| lower_multi_target(&t, recovered, iterative))
            .collect(),
        // `rest` is recorded whenever Prism reports one — an anonymous `*` and
        // an implicit rest (`a, = xs`) become `Ignored`, because the reference's
        // `rest_present:` keys on PRESENCE, not on bindability.
        rest: rest.map(|t| Box::new(lower_multi_target(t, recovered, iterative))),
        rights: rights
            .iter()
            .map(|t| lower_multi_target(&t, recovered, iterative))
            .collect(),
        span,
    }
}

/// Lower one target slot. A `LocalVariableTargetNode` binds its name, a nested
/// `MultiTargetNode` recurses, a `SplatNode` unwraps to its expression (so a
/// `*rest` slot carries the inner local name), and everything else is
/// [`MultiTarget::Ignored`] — exactly the reference's recognised set
/// (`multi_target_binder.rb:29-46`).
fn lower_multi_target<'pr>(
    node: &PrismNode<'pr>,
    recovered: &mut Vec<Recovered<'pr>>,
    iterative: bool,
) -> MultiTarget {
    if let Some(t) = node.as_local_variable_target_node() {
        return MultiTarget::Local {
            name: constant_string(t.name().as_slice()),
            name_span: span_of(&t.location()),
        };
    }
    if let Some(t) = node.as_multi_target_node() {
        return MultiTarget::Nested(lower_multi_targets(
            &t.lefts(),
            t.rest().as_ref(),
            &t.rights(),
            span_of(&t.location()),
            recovered,
            iterative,
        ));
    }
    if let Some(s) = node.as_splat_node() {
        // `*rest` — unwrap to the inner target. An anonymous `*` (no
        // expression) keeps `Ignored`; an index-target rest (`*h[k]`) keeps
        // its `[]=` store, matching the reference's `bind_rest_target`.
        return match s.expression() {
            Some(e) => match lower_multi_target(&e, recovered, iterative) {
                // A splat's expression is never a nested multi-target in valid
                // Ruby; guard anyway so the binder's rest slot only ever sees
                // the shapes the reference's `bind_rest_target` handles.
                MultiTarget::Nested(_) => MultiTarget::Ignored { span: span_of(&s.location()) },
                other => other,
            },
            None => MultiTarget::Ignored { span: span_of(&s.location()) },
        };
    }
    if let Some(t) = node.as_index_target_node() {
        // `h[k]` — binds no local but stores through `[]=` on its receiver:
        // keep the receiver's mutated local reads so the owner can widen them
        // (rigor-rs#134). The embedded expressions still lower into
        // `recovered` exactly as before — blocked, since the binder applies
        // none of their scope effects. Inside an iterated body the
        // content-writeback text scan still sees a compound index write
        // (`while w; a, b[h[:a] ||= 1] = xs; end` widens `h` on the oracle
        // — rigor-rs#312), so the seed keeps `iterative`.
        recovered.extend(collect_recoverable_children(
            node,
            ScopeMarks {
                blocked: true,
                iterative,
                ..ScopeMarks::default()
            },
        ));
        return MultiTarget::Index {
            receivers: mutated_local_reads(&t.receiver(), 0),
            span: span_of(&t.location()),
            key: index_target_key(&t),
        };
    }
    // A non-local target can still EMBED expressions that READ locals or CALL
    // methods (`item[3] = …` reads `item`; `obj.foo, bar = …` calls `obj`).
    recovered.extend(collect_recoverable_children(
        node,
        ScopeMarks {
            blocked: true,
            iterative,
            ..ScopeMarks::default()
        },
    ));
    MultiTarget::Ignored { span: span_of(&node.location()) }
}

/// The local-variable reads an in-place mutation of `receiver` reaches — the
/// LOCAL half of the reference's `ReceiverAlias.mutated_reads`
/// (`reference/rigor/lib/rigor/inference/receiver_alias.rb`): a bare local read
/// names itself, a local WRITE (`(buf ||= [])[k] = v` mutates `buf`) reads the
/// variable it writes, and the transparent / branch-selecting forms — parens,
/// statement lists, `if`/`unless`/`&&`/`||` — contribute every member's reads.
/// An ivar / classvar / global / call receiver names no local binding here (the
/// flow passes widen by local name only), and the depth cap mirrors
/// `WALK_DEPTH_CAP`.
fn mutated_local_reads(node: &PrismNode<'_>, depth: u8) -> Vec<String> {
    const WALK_DEPTH_CAP: u8 = 6;
    if depth > WALK_DEPTH_CAP {
        return Vec::new();
    }
    if let Some(r) = node.as_local_variable_read_node() {
        return vec![constant_string(r.name().as_slice())];
    }
    if node.as_it_local_variable_read_node().is_some() {
        return vec!["it".to_string()];
    }
    // A local write evaluates to the variable it writes.
    let local_write_name = node
        .as_local_variable_write_node()
        .map(|w| w.name())
        .or_else(|| node.as_local_variable_or_write_node().map(|w| w.name()))
        .or_else(|| node.as_local_variable_and_write_node().map(|w| w.name()))
        .or_else(|| node.as_local_variable_operator_write_node().map(|w| w.name()));
    if let Some(name) = local_write_name {
        return vec![constant_string(name.as_slice())];
    }
    if let Some(p) = node.as_parentheses_node() {
        return p.body().map(|b| mutated_local_reads(&b, depth + 1)).unwrap_or_default();
    }
    if let Some(s) = node.as_statements_node() {
        return s
            .body()
            .iter()
            .last()
            .map(|b| mutated_local_reads(&b, depth + 1))
            .unwrap_or_default();
    }
    if let Some(e) = node.as_else_node() {
        return e
            .statements()
            .map(|s| mutated_local_reads(&s.as_node(), depth + 1))
            .unwrap_or_default();
    }
    if let Some(i) = node.as_if_node() {
        return [
            i.statements().map(|s| s.as_node()),
            i.subsequent(),
        ]
        .into_iter()
        .flatten()
        .flat_map(|b| mutated_local_reads(&b, depth + 1))
        .collect();
    }
    if let Some(u) = node.as_unless_node() {
        return [
            u.statements().map(|s| s.as_node()),
            u.else_clause().map(|e| e.as_node()),
        ]
        .into_iter()
        .flatten()
        .flat_map(|b| mutated_local_reads(&b, depth + 1))
        .collect();
    }
    if let Some(o) = node.as_or_node() {
        return [o.left(), o.right()]
            .into_iter()
            .flat_map(|b| mutated_local_reads(&b, depth + 1))
            .collect();
    }
    if let Some(a) = node.as_and_node() {
        return [a.left(), a.right()]
            .into_iter()
            .flat_map(|b| mutated_local_reads(&b, depth + 1))
            .collect();
    }
    Vec::new()
}

/// The `(local, key)` address an index target's `[]=` store invalidates — the
/// reference's `IndexedNarrowing.invalidate_indexed_write`, which reads the
/// `IndexTargetNode`'s `receiver`/`arguments` exactly as a `[]=` `CallNode`'s
/// (`indexed_narrowing.rb:142`; `widen_index_target` runs it for the
/// multi-assign slot, the `for` index and the `rescue =>` reference alike).
/// `stable_receiver` keys on the node KIND, so only a bare
/// `LocalVariableReadNode` qualifies — a write receiver (`(buf ||= {})[:k]`),
/// a parenthesised read, or an `it` read declines even though
/// [`mutated_local_reads`] names a local for it — and only the FIRST index
/// argument addresses the store (`h[:a, :b] = …` still drops `(h, :a)`).
fn index_target_key(t: &ruby_prism::IndexTargetNode<'_>) -> Option<IndexTargetKey> {
    t.receiver().as_local_variable_read_node()?;
    let first = t.arguments()?.arguments().iter().next()?;
    stable_key_node(&first)
}

/// `IndexedNarrowing.stable_key` (`indexed_narrowing.rb:63`): Symbol / String
/// / Integer literals only (`STABLE_KEY_NODES`). A Bignum declines like the
/// lowered `IntegerLit { value: None }` the infer-side `stable_index_key`
/// reads — no slot record can key on it either.
fn stable_key_node(node: &PrismNode<'_>) -> Option<IndexTargetKey> {
    if let Some(s) = node.as_symbol_node() {
        return Some(IndexTargetKey::Sym(constant_string(s.unescaped())));
    }
    if let Some(s) = node.as_string_node() {
        return Some(IndexTargetKey::Str(constant_string(s.unescaped())));
    }
    if let Some(i) = node.as_integer_node() {
        return integer_value(&i.value()).map(IndexTargetKey::Int);
    }
    None
}

/// What a `for` index target writes: `(bound local names, index-target receiver
/// locals)` — the reference's `bind_for_index` set (`statement_evaluator.rb`).
/// A `LocalVariableTargetNode` binds a name; a `MultiTargetNode` decomposes as
/// the multi-assign binder reads it (`*rest` slot included); an
/// `IndexTargetNode` — the whole index (`for h[:k] in xs`), a multi-target slot
/// (`for w, h[:k] in pairs`) or a bare splat index (`for *h[:k] in xs`, which
/// Prism gives as a `SplatNode`, not a `MultiTargetNode`) — stores the element
/// through `[]=` on its receiver, so its receiver's locals widen (rigor-rs#134).
/// A non-local index (`@a`, `A`, `a.b`) binds no local and yields nothing, and
/// so does a bare `for *w in xs`, which the reference leaves unbound.
pub(crate) fn for_index_writes(index: &PrismNode<'_>) -> (Vec<(String, Span)>, IndexWrites) {
    if let Some(t) = index.as_local_variable_target_node() {
        return (
            vec![(constant_string(t.name().as_slice()), span_of(&t.location()))],
            Vec::new(),
        );
    }
    if let Some(t) = index.as_multi_target_node() {
        let mut ignored = Vec::new();
        let targets = lower_multi_targets(
            &t.lefts(),
            t.rest().as_ref(),
            &t.rights(),
            span_of(&t.location()),
            // `ignored` is dropped — this caller needs the target tree only.
            // The for-INDEX (the target tree) is not inside the body, so no
            // content writeback reaches it — `iterative: false`.
            &mut ignored,
            false,
        );
        return (targets.bound_names(), targets.index_writes());
    }
    if let Some(t) = index.as_index_target_node() {
        let key = index_target_key(&t);
        return (
            Vec::new(),
            mutated_local_reads(&t.receiver(), 0)
                .into_iter()
                .map(|r| (r, span_of(&t.location()), key.clone()))
                .collect(),
        );
    }
    if let Some(s) = index.as_splat_node() {
        // `for *h[:k] in xs` — the store is `*h[:k] = element`; the reference
        // widens the receiver with its undecomposable-rest floor (untyped).
        if let Some(t) = s.expression().and_then(|e| e.as_index_target_node()) {
            let key = index_target_key(&t);
            return (
                Vec::new(),
                mutated_local_reads(&t.receiver(), 0)
                    .into_iter()
                    .map(|r| (r, span_of(&t.location()), key.clone()))
                    .collect(),
            );
        }
    }
    (Vec::new(), Vec::new())
}

/// The local-variable reads an in-place mutation of a `rescue =>` reference
/// target reaches — `rescue => h[:e]` stores the exception through `[]=` on
/// `h` (`bind_rescue_reference`, rigor-rs#134). Empty for any other reference
/// kind (a local binds a name instead; ivar / constant / call references name
/// no local binding).
pub(crate) fn rescue_reference_index_writes(reference: &PrismNode<'_>) -> IndexWrites {
    match reference.as_index_target_node() {
        Some(t) => {
            let key = index_target_key(&t);
            mutated_local_reads(&t.receiver(), 0)
                .into_iter()
                .map(|r| (r, span_of(&t.location()), key.clone()))
                .collect()
        }
        None => Vec::new(),
    }
}
