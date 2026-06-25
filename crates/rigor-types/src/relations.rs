//! The two type relations: subtyping (`A <: B`, value-set inclusion) and
//! gradual consistency (`consistent(A, B)`, dynamic-boundary compatibility).
//!
//! Grounded in `relations-and-certainty.md` and `special-types.md`. The two are
//! deliberately NOT unified:
//!
//! - Subtyping is reflexive + transitive, checked against the **static facet**
//!   when one is available (for `Dynamic[T]`, the witness is `T`).
//! - Consistency is **symmetric** in the dynamic direction and **non-transitive**;
//!   it is the ONLY relation that crosses the dynamic boundary. `untyped` is
//!   consistent with every type — but `untyped` is NOT `top`.
//!
//! Both return a [`Relation`] (a [`Certainty`] + [`Evidence`]). This is a
//! skeleton: it encodes the two-relation distinction and the key identities,
//! returning `Maybe` where precise structural reasoning is deferred.

use crate::certainty::{Certainty, Evidence, Relation};
use crate::interner::Interner;
use crate::ty::{Type, TypeId};

/// `A <: B` — value-set inclusion. Reflexive and transitive. For `Dynamic[T]`,
/// the static facet `T` is used as the value-set witness on each side; the
/// dynamic *boundary* crossing is the job of [`consistent`], not this relation.
pub fn subtype(i: &Interner, a: TypeId, b: TypeId) -> Relation {
    // Reflexive: T <: T.
    if a == b {
        return Relation::yes(Evidence::Subtype);
    }

    // bot <: T and T <: top hold for every static value type.
    if matches!(i.get(a), Type::Bottom) {
        return Relation::yes(Evidence::Subtype);
    }
    if matches!(i.get(b), Type::Top) {
        return Relation::yes(Evidence::Subtype);
    }

    // Subtyping uses the static facet of a Dynamic operand. NOTE: this is NOT
    // the gradual-boundary crossing — it is value-set reasoning on the witness.
    let a_facet = facet(i, a);
    let b_facet = facet(i, b);
    if a_facet != a || b_facet != b {
        return subtype(i, a_facet, b_facet);
    }

    // A union is a subtype of B iff every member is.
    if let Type::Union(members) = i.get(a) {
        let members = members.clone();
        let mut acc = Certainty::Yes;
        for m in members {
            acc = acc.and(subtype(i, m, b).certainty);
        }
        return Relation::new(acc, Evidence::Subtype);
    }
    // A is a subtype of a union B iff it is a subtype of some member.
    if let Type::Union(members) = i.get(b) {
        let members = members.clone();
        let mut acc = Certainty::No;
        for m in members {
            acc = acc.or(subtype(i, a, m).certainty);
        }
        return Relation::new(acc, Evidence::Subtype);
    }

    // TODO(spec): nominal hierarchy, Constant <: nominal-base, IntegerRange
    // inclusion, Tuple/HashShape width+depth, refinement implication,
    // intersection/complement reasoning (value-lattice.md, relations-and-
    // certainty.md). Until then, the analyzer cannot prove either side.
    Relation::maybe(Evidence::Subtype)
}

/// `consistent(A, B)` — gradual consistency. Symmetric; the ONLY relation that
/// crosses the dynamic boundary. `untyped` (`Dynamic[top]`) is consistent with
/// everything.
pub fn consistent(i: &Interner, a: TypeId, b: TypeId) -> Relation {
    // Symmetric dynamic crossing: if EITHER side is dynamic-origin, the values
    // may cross the boundary. consistent(Dynamic[T], U) and consistent(U,
    // Dynamic[T]) both hold.
    if is_dynamic(i, a) || is_dynamic(i, b) {
        return Relation::yes(Evidence::Consistency);
    }

    // With no dynamic participant, consistency degenerates toward ordinary
    // overlap. Reflexive and (here) decided by subtyping in either direction.
    if a == b {
        return Relation::yes(Evidence::Consistency);
    }
    let fwd = subtype(i, a, b).certainty;
    let bwd = subtype(i, b, a).certainty;
    // consistent if related in either direction; symmetric by construction.
    let c = fwd.or(bwd);
    // TODO(spec): true gradual consistency also holds for overlapping but
    // non-subtype carriers (e.g. two unions sharing a member). Until structural
    // overlap is modeled, fall back to the subtyping witness.
    Relation::new(c, Evidence::Consistency)
}

// --- helpers ----------------------------------------------------------------

fn is_dynamic(i: &Interner, id: TypeId) -> bool {
    matches!(i.get(id), Type::Dynamic(_))
}

/// The static facet of `id`: unwraps a single `Dynamic[T]` to `T`, else `id`.
fn facet(i: &Interner, id: TypeId) -> TypeId {
    match i.get(id) {
        Type::Dynamic(f) => *f,
        _ => id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::ClassId;

    fn nominal(i: &mut Interner, class: u32) -> TypeId {
        i.intern(Type::Nominal {
            class: ClassId(class),
            args: vec![],
        })
    }

    #[test]
    fn subtype_is_reflexive() {
        let mut i = Interner::new();
        let t = nominal(&mut i, 1);
        assert_eq!(subtype(&i, t, t).certainty, Certainty::Yes);
    }

    #[test]
    fn bottom_top_identities() {
        let mut i = Interner::new();
        let t = nominal(&mut i, 1);
        let bot = i.bottom();
        let top = i.top();
        assert_eq!(subtype(&i, bot, t).certainty, Certainty::Yes);
        assert_eq!(subtype(&i, t, top).certainty, Certainty::Yes);
    }

    #[test]
    fn subtype_vs_consistency_distinction() {
        // untyped is NOT top. Two unrelated nominals are NOT subtypes, but each
        // IS consistent with `untyped` (the dynamic boundary crossing).
        let mut i = Interner::new();
        let top = i.top();
        let a = nominal(&mut i, 1);
        let b = nominal(&mut i, 2);
        let untyped = i.intern(Type::Dynamic(top));

        // Unrelated nominals: subtyping cannot prove inclusion.
        assert_eq!(subtype(&i, a, b).certainty, Certainty::Maybe);

        // But consistency crosses the dynamic boundary: yes, both directions.
        assert_eq!(consistent(&i, a, untyped).certainty, Certainty::Yes);
        assert_eq!(consistent(&i, untyped, a).certainty, Certainty::Yes);

        // And the two relations carry distinct evidence.
        assert_eq!(subtype(&i, a, a).evidence, Evidence::Subtype);
        assert_eq!(consistent(&i, a, untyped).evidence, Evidence::Consistency);
    }

    #[test]
    fn subtyping_uses_dynamic_static_facet() {
        // Dynamic[A] <: A is decided by the facet, so it is reflexive-yes.
        let mut i = Interner::new();
        let a = nominal(&mut i, 1);
        let da = i.intern(Type::Dynamic(a));
        assert_eq!(subtype(&i, da, a).certainty, Certainty::Yes);
    }
}
