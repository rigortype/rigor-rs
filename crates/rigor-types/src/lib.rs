//! `rigor-types` — the type-lattice foundation for rigor-rs (ADR-0005/0019).
//!
//! A single interned [`Type`] carrier behind copyable [`TypeId`] handles, with
//! the value lattice, the `Dynamic[T]` algebra, the two type relations
//! (subtyping vs. gradual consistency), trinary certainty, and best-effort
//! display. The design is grounded in the reference specification under
//! `docs/type-specification/` (value-lattice, relations-and-certainty,
//! normalization, special-types).
//!
//! This is a faithful SKELETON: the carrier shapes, the algebra, and the
//! load-bearing invariants are real and tested; deeper structural reasoning is
//! marked with `// TODO(spec):` where the specs go further than this layer.
#![allow(dead_code)]

pub mod algebra;
pub mod certainty;
pub mod display;
pub mod interner;
pub mod relations;
pub mod ty;

// Re-export the everyday surface at the crate root.
pub use algebra::{is_bool_pair, Algebra};
pub use certainty::{Certainty, Evidence, Relation};
pub use display::{describe, describe_named, erase_to_rbs_named, ruby_float_to_s};
pub use interner::Interner;
pub use relations::{consistent, subtype};
pub use ty::{
    ClassId, HktUri, RefinementId, Scalar, ShapeKey, ShapeMember, Type, TypeId,
};

#[cfg(test)]
mod integration_tests {
    //! Cross-module invariants that exercise more than one module together.
    use super::*;
    use crate::ty::ClassId;

    fn nominal(i: &mut Interner, class: u32) -> TypeId {
        i.intern(Type::Nominal {
            class: ClassId(class),
            args: vec![],
        })
    }

    #[test]
    fn untyped_equals_dynamic_top() {
        let mut i = Interner::new();
        let untyped = i.untyped();
        let dynamic_top = i.intern(Type::Dynamic(i.top()));
        assert_eq!(untyped, dynamic_top);
        assert_eq!(i.get(untyped), &Type::Dynamic(i.top()));
    }

    #[test]
    fn untyped_is_not_top() {
        // The headline distinction: untyped (Dynamic[top]) is a different
        // carrier from top, and the two relations treat it differently.
        let mut i = Interner::new();
        let top = i.top();
        let untyped = i.untyped();
        assert_ne!(top, untyped);

        let a = nominal(&mut i, 1);
        // top absorbs in join; untyped does NOT (it wraps the facet instead).
        assert_eq!(Algebra::join(&mut i, a, top), top);
        let j = Algebra::join(&mut i, a, untyped);
        assert!(matches!(i.get(j), Type::Dynamic(_)));
    }

    #[test]
    fn dynamic_algebra_full_round() {
        let mut i = Interner::new();
        let a = nominal(&mut i, 1);
        let b = nominal(&mut i, 2);
        let da = i.intern(Type::Dynamic(a));
        let db = i.intern(Type::Dynamic(b));

        // Dynamic[A] | Dynamic[B] = Dynamic[A | B]
        let ab = Algebra::join(&mut i, a, b);
        assert_eq!(
            Algebra::join(&mut i, da, db),
            i.intern(Type::Dynamic(ab))
        );

        // Dynamic[T] & U = Dynamic[T & U]
        let m = Algebra::meet(&mut i, da, b);
        let ab_meet = Algebra::meet(&mut i, a, b);
        assert_eq!(m, i.intern(Type::Dynamic(ab_meet)));

        // Dynamic[T] - U = Dynamic[T - U]
        let d = Algebra::difference(&mut i, da, b);
        assert!(matches!(i.get(d), Type::Dynamic(_)));
    }

    #[test]
    fn interner_dedup_across_constructions() {
        let mut i = Interner::new();
        let a = nominal(&mut i, 1);
        let b = nominal(&mut i, 1);
        assert_eq!(a, b);
    }

    #[test]
    fn subtype_reflexive_and_distinct_from_consistency() {
        let mut i = Interner::new();
        let a = nominal(&mut i, 1);
        let untyped = i.untyped();
        // Reflexivity of subtyping.
        assert_eq!(subtype(&i, a, a).certainty, Certainty::Yes);
        // Consistency crosses the dynamic boundary; subtyping does not decide
        // it the same way.
        assert_eq!(consistent(&i, a, untyped).certainty, Certainty::Yes);
        assert_eq!(consistent(&i, untyped, a).certainty, Certainty::Yes);
    }
}
