//! Multiple-assignment and `for`-index targets: the owned `MultiTargets` tree
//! and its structural (non-arena) lowering.

use crate::ruby_prism::{self, Node as PrismNode};

use super::{collect_recoverable_children, constant_string, span_of, Span};

/// One target slot of a multiple assignment (`a, (b, c), *rest = rhs`).
///
/// Mirrors the target kinds the reference's `MultiTargetBinder` recognises
/// (`reference/rigor/lib/rigor/inference/multi_target_binder.rb:29-46`):
/// a `LocalVariableTargetNode` binds a name, a nested `MultiTargetNode`
/// recurses, and every other target kind (`InstanceVariableTargetNode`,
/// `ConstantTargetNode`, `IndexTargetNode`, `CallTargetNode`,
/// `ConstantPathTargetNode`, `ImplicitRestNode`, an anonymous `*`) is silently
/// skipped — it has no observable contribution to the local-variable scope.
///
/// A skipped target is still materialised as [`MultiTarget::Ignored`] so it
/// keeps its POSITION: the tuple decomposition is positional, so dropping an
/// ignorable slot would shift every later target onto the wrong element.
#[derive(Clone, Debug)]
pub enum MultiTarget {
    /// A plain local target (`a`). `name_span` is the Prism
    /// `LocalVariableTargetNode` location, which IS the name token — the
    /// name-anchored span the enum's other write variants carry.
    Local { name: String, name_span: Span },
    /// A nested multi-target (`(b, c)` in `a, (b, c) = …`). The binder recurses
    /// with this slot's type as the new right-hand side.
    Nested(MultiTargets),
    /// A target with no observable local binding (ivar / constant / index /
    /// call / const-path target, an implicit rest `a, = …`, an anonymous `*`).
    Ignored { span: Span },
}

impl MultiTarget {
    /// The byte span of this target slot.
    pub fn span(&self) -> Span {
        match self {
            MultiTarget::Local { name_span, .. } => *name_span,
            MultiTarget::Nested(t) => t.span,
            MultiTarget::Ignored { span } => *span,
        }
    }

    /// Push every local name bound anywhere under this target (recursing into a
    /// nested multi-target) onto `out`, with its name span.
    pub fn collect_bound_names(&self, out: &mut Vec<(String, Span)>) {
        match self {
            MultiTarget::Local { name, name_span } => out.push((name.clone(), *name_span)),
            MultiTarget::Nested(t) => t.collect_bound_names(out),
            MultiTarget::Ignored { .. } => {}
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
}

/// Lower a Prism `lefts` / `rest` / `rights` target triple (shared by
/// `MultiWriteNode` and the nested `MultiTargetNode`) into the owned
/// [`MultiTargets`] group.
///
/// Targets are lowered STRUCTURALLY, not into the node arena: a target binds a
/// name, it is not a value expression, so materialising one as an arena node
/// would make it look like a read/write to the span-scanning structural walks.
/// Every target that lowers to [`MultiTarget::Ignored`] contributes its
/// RECOVERABLE descendants (local reads / writes / calls) to `recovered`, so the
/// caller can lower them into the arena and keep them visible to the structural
/// walks — the old recovered-children carrier did exactly this.
pub(crate) fn lower_multi_targets<'pr>(
    lefts: &ruby_prism::NodeList<'pr>,
    rest: Option<&PrismNode<'pr>>,
    rights: &ruby_prism::NodeList<'pr>,
    span: Span,
    recovered: &mut Vec<PrismNode<'pr>>,
) -> MultiTargets {
    MultiTargets {
        lefts: lefts.iter().map(|t| lower_multi_target(&t, recovered)).collect(),
        // `rest` is recorded whenever Prism reports one — an anonymous `*` and
        // an implicit rest (`a, = xs`) become `Ignored`, because the reference's
        // `rest_present:` keys on PRESENCE, not on bindability.
        rest: rest.map(|t| Box::new(lower_multi_target(t, recovered))),
        rights: rights.iter().map(|t| lower_multi_target(&t, recovered)).collect(),
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
    recovered: &mut Vec<PrismNode<'pr>>,
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
        ));
    }
    if let Some(s) = node.as_splat_node() {
        // `*rest` — unwrap to the inner target. An anonymous `*` (no
        // expression) or a non-local inner target stays `Ignored`.
        return match s.expression() {
            Some(e) => match lower_multi_target(&e, recovered) {
                // A splat's expression is never a nested multi-target in valid
                // Ruby; guard anyway so the binder's rest slot only ever sees
                // the two shapes the reference's `bind_rest_target` handles.
                MultiTarget::Nested(_) => MultiTarget::Ignored { span: span_of(&s.location()) },
                other => other,
            },
            None => MultiTarget::Ignored { span: span_of(&s.location()) },
        };
    }
    // A non-local target can still EMBED expressions that READ locals or CALL
    // methods (`item[3] = …` reads `item`; `obj.foo, bar = …` calls `obj`).
    recovered.extend(collect_recoverable_children(node));
    MultiTarget::Ignored { span: span_of(&node.location()) }
}

/// The local names a `for` index target binds, each with its target span — the
/// reference's `bind_for_index` set (`statement_evaluator.rb`): a
/// `LocalVariableTargetNode`, or the local slots of a `MultiTargetNode` (as the
/// multi-assign binder reads them, a `*rest` slot included). A non-local index
/// (`@a`, `A`, `h[:k]`, `a.b`) binds no local and yields nothing, and so does a
/// bare `for *w in xs`, which the reference leaves unbound.
pub(crate) fn for_index_names(index: &PrismNode<'_>) -> Vec<(String, Span)> {
    if let Some(t) = index.as_local_variable_target_node() {
        return vec![(constant_string(t.name().as_slice()), span_of(&t.location()))];
    }
    if let Some(t) = index.as_multi_target_node() {
        let mut ignored = Vec::new();
        return lower_multi_targets(
            &t.lefts(),
            t.rest().as_ref(),
            &t.rights(),
            span_of(&t.location()),
            &mut ignored,
        )
        .bound_names();
    }
    Vec::new()
}
