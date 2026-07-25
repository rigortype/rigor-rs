//! Destructuring binder for multiple assignment — a faithful port of the
//! reference's `Rigor::Inference::MultiTargetBinder`
//! (`reference/rigor/lib/rigor/inference/multi_target_binder.rb`).
//!
//! Decomposes a tuple-shaped right-hand-side type against a
//! [`MultiTargets`] tree and produces an ordered `name -> TypeId` binding list.
//! Pure: it never mutates its inputs and returns a fresh `Vec` on every call.
//! The list is ORDERED (the reference returns a `Hash` built in the same order),
//! so a duplicated target name (`a, a = xs`) resolves to the last binding when
//! the caller applies the list in order — exactly the Ruby-`Hash` semantics.
//!
//! The reference's second surface (block-parameter destructuring via
//! `BlockParameterBinder`, `|(a, b), c|`) is NOT wired here: rigor-rs does not
//! lower block parameters into the arena, so there is no `MultiTargets` to bind.
//! Everything else is ported rule-for-rule:
//!
//! | reference | here |
//! | --- | --- |
//! | `visit` | [`visit`] |
//! | `decompose` | [`decompose`] |
//! | `decompose_tuple` | [`decompose_tuple`] |
//! | `decompose_default` (non-tuple RHS ⇒ every slot `Dynamic[top]`) | [`decompose_default`] |
//! | `slot_type` (missing slot ⇒ `Constant[nil]`) | [`slot_type`] |
//! | `soften_optional_slot` | [`soften_optional_slot`] |
//! | `bind_target` / `bind_rest_target` | [`bind_target`] / [`bind_rest_target`] |

use rigor_parse::{MultiTarget, MultiTargets};
use rigor_types::{Algebra, Interner, Scalar, Type, TypeId};

/// Bind a multi-assignment target tree against the right-hand side's type.
/// Returns the `(name, type)` pairs in source order (reference `bind`).
#[must_use]
pub fn bind(targets: &MultiTargets, rhs_type: TypeId, interner: &mut Interner) -> Vec<(String, TypeId)> {
    let mut out = Vec::new();
    visit(targets, rhs_type, interner, &mut out);
    out
}

/// Reference `visit`: split the RHS into per-slot types, then bind each slot.
fn visit(
    node: &MultiTargets,
    rhs_type: TypeId,
    interner: &mut Interner,
    out: &mut Vec<(String, TypeId)>,
) {
    let (fronts, rest_type, backs) = decompose(
        rhs_type,
        node.lefts.len(),
        node.rights.len(),
        node.rest.is_some(),
        interner,
    );
    for (t, ty) in node.lefts.iter().zip(fronts) {
        bind_target(t, ty, interner, out);
    }
    if let (Some(rest), Some(ty)) = (node.rest.as_deref(), rest_type) {
        bind_rest_target(rest, ty, out);
    }
    for (t, ty) in node.rights.iter().zip(backs) {
        bind_target(t, ty, interner, out);
    }
}

/// Reference `decompose`: a `Type::Tuple` RHS decomposes element-wise; every
/// other carrier (`Nominal[Array]`, `Dynamic[top]`, `Top`, `Bot`, a union, …)
/// collapses to `Dynamic[top]` per slot.
fn decompose(
    rhs_type: TypeId,
    front_count: usize,
    back_count: usize,
    rest_present: bool,
    interner: &mut Interner,
) -> (Vec<TypeId>, Option<TypeId>, Vec<TypeId>) {
    match interner.get(rhs_type) {
        Type::Tuple(elements) => {
            let elements = elements.clone();
            decompose_tuple(&elements, front_count, back_count, rest_present, interner)
        }
        _ => decompose_default(front_count, back_count, rest_present, interner),
    }
}

/// Reference `decompose_tuple`: front/rest/back split of a known-arity tuple.
/// `middle_end` reproduces `[elements.size - back_count, front_count].max` in
/// signed arithmetic (the Ruby expression goes negative when the tuple is
/// shorter than the trailing target count).
fn decompose_tuple(
    elements: &[TypeId],
    front_count: usize,
    back_count: usize,
    rest_present: bool,
    interner: &mut Interner,
) -> (Vec<TypeId>, Option<TypeId>, Vec<TypeId>) {
    let fronts: Vec<TypeId> =
        (0..front_count).map(|i| slot_type(elements, i, interner)).collect();
    if rest_present {
        let middle_end =
            std::cmp::max(elements.len() as isize - back_count as isize, front_count as isize)
                as usize;
        // `elements[front_count...middle_end] || []` — `middle_end >= front_count`
        // always holds, so the only empty case is a start past the end.
        let middle: Vec<TypeId> = if front_count >= elements.len() {
            Vec::new()
        } else {
            elements[front_count..std::cmp::min(middle_end, elements.len())].to_vec()
        };
        let rest_type = interner.intern(Type::Tuple(middle));
        let backs: Vec<TypeId> =
            (0..back_count).map(|i| slot_type(elements, middle_end + i, interner)).collect();
        (fronts, Some(rest_type), backs)
    } else {
        let backs: Vec<TypeId> =
            (0..back_count).map(|i| slot_type(elements, front_count + i, interner)).collect();
        (fronts, None, backs)
    }
}

/// Reference `decompose_default`: a non-tuple RHS gives every slot (and the
/// rest slot) `Dynamic[top]` — Slice 5 phase 2 stays conservative on
/// dynamic-arity right-hand sides.
fn decompose_default(
    front_count: usize,
    back_count: usize,
    rest_present: bool,
    interner: &mut Interner,
) -> (Vec<TypeId>, Option<TypeId>, Vec<TypeId>) {
    let u = interner.untyped();
    (
        vec![u; front_count],
        if rest_present { Some(u) } else { None },
        vec![u; back_count],
    )
}

/// Reference `slot_type`: a MISSING slot is `Constant[nil]` (the runtime value
/// of an over-destructured positional); a PRESENT slot is FP-softened by
/// [`soften_optional_slot`].
fn slot_type(elements: &[TypeId], index: usize, interner: &mut Interner) -> TypeId {
    match elements.get(index) {
        None => interner.intern(Type::Constant(Scalar::Nil)),
        Some(&element) => soften_optional_slot(element, interner),
    }
}

/// Reference `soften_optional_slot` (ADR-57 slice 3 work-item 2) — an explicit
/// FP-discipline decision, NOT an optimization, so it is ported exactly.
///
/// A destructured slot that flow typed as `X | nil` drops the `nil` and keeps
/// `X`. The canonical case upstream is haml's `parse_tag`, which returns a
/// 9-tuple whose `last_line` slot widens to `Dynamic[top]?` through a
/// loop-nested destructure; at the call site the nil-ness is guarded by a
/// CORRELATED invariant across slots that per-slot flow cannot see, so
/// manufacturing a `T?` per slot fires a spurious `possible nil receiver` on
/// working code. A BARE `nil` slot stays `nil` (there is nothing to soften) and
/// a non-optional slot keeps its precise type unchanged.
fn soften_optional_slot(element: TypeId, interner: &mut Interner) -> TypeId {
    let Type::Union(members) = interner.get(element) else {
        return element;
    };
    let members = members.clone();
    if !members.iter().any(|&m| is_nil_literal(interner, m)) {
        return element;
    }
    let non_nil: Vec<TypeId> =
        members.iter().copied().filter(|&m| !is_nil_literal(interner, m)).collect();
    // A bare `nil` slot: nothing to soften.
    let Some((&first, rest)) = non_nil.split_first() else {
        return element;
    };
    rest.iter().fold(first, |acc, &m| Algebra::join(interner, acc, m))
}

/// Reference `nil_literal?`: the `Constant[nil]` member, not `NilClass`.
fn is_nil_literal(interner: &Interner, member: TypeId) -> bool {
    matches!(interner.get(member), Type::Constant(Scalar::Nil))
}

/// Reference `bind_target`: a local target binds, a nested multi-target
/// recurses with the slot type as its new RHS, anything else is skipped.
fn bind_target(
    target: &MultiTarget,
    ty: TypeId,
    interner: &mut Interner,
    out: &mut Vec<(String, TypeId)>,
) {
    match target {
        MultiTarget::Local { name, .. } => out.push((name.clone(), ty)),
        MultiTarget::Nested(inner) => visit(inner, ty, interner, out),
        MultiTarget::Ignored { .. } => {}
    }
}

/// Reference `bind_rest_target`: ONLY a local target under the splat binds. An
/// anonymous `*`, an implicit rest, and a non-local splat target are skipped —
/// and a nested multi-target is NOT recursed into (the reference's rest arm
/// handles only the two local-ish node kinds).
fn bind_rest_target(rest: &MultiTarget, ty: TypeId, out: &mut Vec<(String, TypeId)>) {
    if let MultiTarget::Local { name, .. } = rest {
        out.push((name.clone(), ty));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rigor_parse::{LoweredAst, Node, Span};

    /// Lower `src` and return the first `MultiWrite`'s target tree.
    fn targets(src: &str) -> MultiTargets {
        let src = src.as_bytes().to_vec();
        let result = rigor_parse::parse(&src);
        let ast: LoweredAst = rigor_parse::lower(&result);
        ast.iter()
            .find_map(|(_, n)| match n {
                Node::MultiWrite { targets, .. } => Some(targets.clone()),
                _ => None,
            })
            .expect("a MultiWrite node")
    }

    fn names(b: &[(String, TypeId)]) -> Vec<&str> {
        b.iter().map(|(n, _)| n.as_str()).collect()
    }

    const NOWHERE: Span = (0, 0);

    fn local(name: &str) -> MultiTarget {
        MultiTarget::Local { name: name.to_string(), name_span: NOWHERE }
    }

    #[test]
    fn tuple_rhs_decomposes_element_wise() {
        let mut i = Interner::new();
        let (a, b) = (i.int(1), i.int(2));
        let rhs = i.intern(Type::Tuple(vec![a, b]));
        let t = targets("a, b = [1, 2]\n");
        let out = bind(&t, rhs, &mut i);
        assert_eq!(names(&out), ["a", "b"]);
        assert_eq!(out[0].1, a);
        assert_eq!(out[1].1, b);
    }

    #[test]
    fn missing_slot_is_nil() {
        let mut i = Interner::new();
        let a = i.int(1);
        let rhs = i.intern(Type::Tuple(vec![a]));
        let t = targets("a, b = [1]\n");
        let out = bind(&t, rhs, &mut i);
        assert_eq!(out[1].1, i.nil());
    }

    #[test]
    fn non_tuple_rhs_gives_every_slot_untyped() {
        let mut i = Interner::new();
        let rhs = i.intern(Type::Nominal { class: rigor_types::ClassId(1), args: vec![] });
        let t = targets("a, b = xs\n");
        let out = bind(&t, rhs, &mut i);
        let u = i.untyped();
        assert!(out.iter().all(|(_, ty)| *ty == u));
    }

    #[test]
    fn splat_binds_the_middle_sub_tuple() {
        let mut i = Interner::new();
        let (a, b, c, d) = (i.int(1), i.int(2), i.int(3), i.int(4));
        let rhs = i.intern(Type::Tuple(vec![a, b, c, d]));
        let t = targets("a, *m, z = [1, 2, 3, 4]\n");
        let out = bind(&t, rhs, &mut i);
        assert_eq!(names(&out), ["a", "m", "z"]);
        assert_eq!(out[0].1, a);
        assert_eq!(out[1].1, i.intern(Type::Tuple(vec![b, c])));
        assert_eq!(out[2].1, d);
    }

    #[test]
    fn splat_over_a_short_tuple_is_empty_and_backs_still_resolve() {
        let mut i = Interner::new();
        let a = i.int(1);
        let rhs = i.intern(Type::Tuple(vec![a]));
        // front=1, back=1, tuple has 1 element: middle_end = max(1-1, 1) = 1.
        let t = targets("a, *m, z = [1]\n");
        let out = bind(&t, rhs, &mut i);
        assert_eq!(out[0].1, a);
        assert_eq!(out[1].1, i.intern(Type::Tuple(vec![])));
        assert_eq!(out[2].1, i.nil(), "the over-destructured trailing slot is nil");
    }

    #[test]
    fn nested_target_recurses() {
        let mut i = Interner::new();
        let (b, c) = (i.int(2), i.int(3));
        let inner = i.intern(Type::Tuple(vec![b, c]));
        let a = i.int(1);
        let rhs = i.intern(Type::Tuple(vec![a, inner]));
        let t = targets("a, (b, c) = [1, [2, 3]]\n");
        let out = bind(&t, rhs, &mut i);
        assert_eq!(names(&out), ["a", "b", "c"]);
        assert_eq!(out[1].1, b);
        assert_eq!(out[2].1, c);
    }

    #[test]
    fn ignorable_targets_keep_their_position() {
        let mut i = Interner::new();
        let (a, b) = (i.int(1), i.int(2));
        let rhs = i.intern(Type::Tuple(vec![a, b]));
        // `@x` is an ivar target — skipped, but slot 0 must still be ITS slot.
        let t = targets("@x, b = [1, 2]\n");
        let out = bind(&t, rhs, &mut i);
        assert_eq!(names(&out), ["b"]);
        assert_eq!(out[0].1, b, "b takes slot 1, not slot 0");
    }

    #[test]
    fn anonymous_splat_is_present_but_binds_nothing() {
        let mut i = Interner::new();
        let (a, b, c) = (i.int(1), i.int(2), i.int(3));
        let rhs = i.intern(Type::Tuple(vec![a, b, c]));
        let t = targets("a, *, z = [1, 2, 3]\n");
        let out = bind(&t, rhs, &mut i);
        assert_eq!(names(&out), ["a", "z"]);
        assert_eq!(out[1].1, c, "the rest slot still consumed the middle");
    }

    #[test]
    fn optional_slot_is_softened_to_its_non_nil_part() {
        let mut i = Interner::new();
        let s = i.intern(Type::Nominal { class: rigor_types::ClassId(1), args: vec![] });
        let nil = i.nil();
        let opt = Algebra::join(&mut i, s, nil);
        assert!(matches!(i.get(opt), Type::Union(_)), "precondition: a union");
        let rhs = i.intern(Type::Tuple(vec![opt]));
        let t = targets("a, = xs\n");
        let out = bind(&t, rhs, &mut i);
        assert_eq!(out[0].1, s, "the nil arm is dropped");
    }

    #[test]
    fn bare_nil_slot_stays_nil() {
        let mut i = Interner::new();
        let nil = i.nil();
        let rhs = i.intern(Type::Tuple(vec![nil]));
        let t = targets("a, b = [nil, nil]\n");
        let out = bind(&t, rhs, &mut i);
        assert_eq!(out[0].1, nil);
    }

    #[test]
    fn duplicate_name_resolves_to_the_last_slot() {
        let mut i = Interner::new();
        let (a, b) = (i.int(1), i.int(2));
        let rhs = i.intern(Type::Tuple(vec![a, b]));
        let t = MultiTargets {
            lefts: vec![local("a"), local("a")],
            rest: None,
            rights: vec![],
            span: NOWHERE,
        };
        let out = bind(&t, rhs, &mut i);
        assert_eq!(out.last().unwrap().1, b);
    }
}
