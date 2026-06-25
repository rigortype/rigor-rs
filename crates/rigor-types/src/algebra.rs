//! Lattice algebra over [`TypeId`]: `join` (`|`), `meet` (`&`), `difference`
//! (`-`), plus a `normalize` canonicalization step.
//!
//! Grounded in `value-lattice.md` (Dynamic algebra) and `normalization.md`.
//! The Dynamic-origin algebra is threaded EXACTLY:
//!
//! ```text
//! Dynamic[A] | Dynamic[B] = Dynamic[A | B]
//! T          | Dynamic[U] = Dynamic[T | U]
//! Dynamic[T] & U          = Dynamic[T & U]
//! Dynamic[T] - U          = Dynamic[T - U]
//! ```
//!
//! and `untyped == Dynamic[top]`.

use crate::interner::Interner;
use crate::ty::{Scalar, Type, TypeId};

/// Operations are free functions taking the interner so callers keep a single
/// shared arena.
pub struct Algebra;

impl Algebra {
    /// Join (`A | B`): the least carrier containing both. Threads the Dynamic
    /// algebra and normalizes the resulting union.
    pub fn join(i: &mut Interner, a: TypeId, b: TypeId) -> TypeId {
        if a == b {
            return a;
        }

        // bot is the identity for join: T | bot = T.
        if Self::is_bottom(i, a) {
            return b;
        }
        if Self::is_bottom(i, b) {
            return a;
        }
        // top absorbs in join: T | top = top. (Plain top, NOT Dynamic[top].)
        if Self::is_plain_top(i, a) || Self::is_plain_top(i, b) {
            return i.top();
        }

        // Dynamic algebra: the wrapper survives a join and absorbs the static
        // side into its facet.
        match (Self::as_dynamic(i, a), Self::as_dynamic(i, b)) {
            (Some(fa), Some(fb)) => {
                // Dynamic[A] | Dynamic[B] = Dynamic[A | B]
                let facet = Self::join(i, fa, fb);
                return i.intern(Type::Dynamic(facet));
            }
            (Some(fa), None) => {
                // Dynamic[A] | U = Dynamic[A | U]
                let facet = Self::join(i, fa, b);
                return i.intern(Type::Dynamic(facet));
            }
            (None, Some(fb)) => {
                // T | Dynamic[U] = Dynamic[T | U]
                let facet = Self::join(i, a, fb);
                return i.intern(Type::Dynamic(facet));
            }
            (None, None) => {}
        }

        // Ordinary union: collect members (flattening nested unions) and
        // normalize.
        let mut members = Vec::new();
        Self::collect_union_members(i, a, &mut members);
        Self::collect_union_members(i, b, &mut members);
        Self::make_union(i, members)
    }

    /// Meet (`A & B`): the greatest carrier contained in both. Threads the
    /// Dynamic algebra.
    pub fn meet(i: &mut Interner, a: TypeId, b: TypeId) -> TypeId {
        if a == b {
            return a;
        }

        // bot absorbs in meet: T & bot = bot.
        if Self::is_bottom(i, a) || Self::is_bottom(i, b) {
            return i.bottom();
        }
        // top is the identity for meet: T & top = T. (Plain top only.)
        if Self::is_plain_top(i, a) {
            return b;
        }
        if Self::is_plain_top(i, b) {
            return a;
        }

        // Dynamic[T] & U = Dynamic[T & U]; provenance is preserved.
        match (Self::as_dynamic(i, a), Self::as_dynamic(i, b)) {
            (Some(fa), Some(fb)) => {
                let facet = Self::meet(i, fa, fb);
                return i.intern(Type::Dynamic(facet));
            }
            (Some(fa), None) => {
                let facet = Self::meet(i, fa, b);
                return i.intern(Type::Dynamic(facet));
            }
            (None, Some(fb)) => {
                let facet = Self::meet(i, a, fb);
                return i.intern(Type::Dynamic(facet));
            }
            (None, None) => {}
        }

        // Ordinary intersection: flatten and normalize.
        let mut members = Vec::new();
        Self::collect_intersection_members(i, a, &mut members);
        Self::collect_intersection_members(i, b, &mut members);
        Self::make_intersection(i, members)
    }

    /// Difference (`A - B`): values in `A` not in `B`. Threads the Dynamic
    /// algebra: `Dynamic[T] - U = Dynamic[T - U]`.
    pub fn difference(i: &mut Interner, a: TypeId, b: TypeId) -> TypeId {
        if Self::is_bottom(i, a) {
            return i.bottom();
        }
        if Self::is_bottom(i, b) {
            return a; // A - bot = A
        }
        if a == b {
            return i.bottom(); // A - A = bot
        }

        // Dynamic[T] - U = Dynamic[T - U]. Only the LEFT operand's wrapper is
        // preserved (we are removing from a dynamic-origin value).
        if let Some(fa) = Self::as_dynamic(i, a) {
            // If the right is dynamic too, remove its facet from the left facet.
            let rhs = Self::as_dynamic(i, b).unwrap_or(b);
            let facet = Self::difference(i, fa, rhs);
            return i.intern(Type::Dynamic(facet));
        }

        // Skeleton structural cases. Full finite-set difference is deferred.
        // TODO(spec): normalize finite set difference and complement when the
        // domain is known (normalization.md); preserve negative facts as scope
        // facts over a positive domain without minting a positive domain from
        // the excluded value alone.
        i.intern(Type::Difference { base: a, removed: b })
    }

    /// Canonicalize a carrier in place (returns a possibly-new interned id):
    /// flatten nested Union/Intersection, drop `bot` from unions / `top` from
    /// intersections, sort + dedup members deterministically, and collapse a
    /// singleton union/intersection to its sole member.
    ///
    /// Critically, normalization does NOT subsumption-collapse a value-pinned
    /// `Constant` against a co-member nominal base (`1 | Integer` stays two
    /// members), and does NOT rewrite `true | false` to `bool` (that is a
    /// DISPLAY concern only — identity is preserved here).
    pub fn normalize(i: &mut Interner, id: TypeId) -> TypeId {
        match i.get(id).clone() {
            Type::Union(members) => {
                let mut flat = Vec::new();
                for m in members {
                    let n = Self::normalize(i, m);
                    Self::collect_union_members(i, n, &mut flat);
                }
                Self::make_union(i, flat)
            }
            Type::Intersection(members) => {
                let mut flat = Vec::new();
                for m in members {
                    let n = Self::normalize(i, m);
                    Self::collect_intersection_members(i, n, &mut flat);
                }
                Self::make_intersection(i, flat)
            }
            Type::Dynamic(facet) => {
                // Preserve the wrapper; normalize the static facet only.
                let f = Self::normalize(i, facet);
                i.intern(Type::Dynamic(f))
            }
            // Other carriers are returned as-is in this skeleton.
            // TODO(spec): normalize T? -> T | nil, finite difference/complement,
            // literal widening past the precision cap, void | bot -> void in
            // result summaries (normalization.md / special-types.md).
            _ => id,
        }
    }

    // --- helpers -------------------------------------------------------------

    fn is_bottom(i: &Interner, id: TypeId) -> bool {
        matches!(i.get(id), Type::Bottom)
    }

    /// Plain `top` — NOT `Dynamic[top]`. `untyped` must not absorb like `top`.
    fn is_plain_top(i: &Interner, id: TypeId) -> bool {
        matches!(i.get(id), Type::Top)
    }

    /// If `id` is `Dynamic[T]`, return the static facet `T`.
    fn as_dynamic(i: &Interner, id: TypeId) -> Option<TypeId> {
        match i.get(id) {
            Type::Dynamic(f) => Some(*f),
            _ => None,
        }
    }

    /// Push the union members of `id` into `out`, flattening nested unions and
    /// dropping `bot`.
    fn collect_union_members(i: &Interner, id: TypeId, out: &mut Vec<TypeId>) {
        match i.get(id) {
            Type::Bottom => {} // T | bot = T : drop
            Type::Union(ms) => {
                let ms = ms.clone();
                for m in ms {
                    Self::collect_union_members(i, m, out);
                }
            }
            _ => out.push(id),
        }
    }

    /// Push the intersection members of `id` into `out`, flattening nested
    /// intersections and dropping `top`.
    fn collect_intersection_members(i: &Interner, id: TypeId, out: &mut Vec<TypeId>) {
        match i.get(id) {
            Type::Top => {} // T & top = T : drop
            Type::Intersection(ms) => {
                let ms = ms.clone();
                for m in ms {
                    Self::collect_intersection_members(i, m, out);
                }
            }
            _ => out.push(id),
        }
    }

    /// Sort + dedup `members` by the interner's total order, then build a
    /// canonical `Union` (collapsing the empty/singleton cases).
    fn make_union(i: &mut Interner, mut members: Vec<TypeId>) -> TypeId {
        // If any member is plain top, the whole union is top.
        if members.iter().any(|&m| Self::is_plain_top(i, m)) {
            return i.top();
        }
        members.sort_by(|&x, &y| i.cmp(x, y));
        members.dedup();
        match members.len() {
            0 => i.bottom(), // empty union = bot
            1 => members[0],
            _ => i.intern(Type::Union(members)),
        }
    }

    /// Sort + dedup `members`, then build a canonical `Intersection`.
    fn make_intersection(i: &mut Interner, mut members: Vec<TypeId>) -> TypeId {
        // If any member is bot, the whole intersection is bot.
        if members.iter().any(|&m| Self::is_bottom(i, m)) {
            return i.bottom();
        }
        members.sort_by(|&x, &y| i.cmp(x, y));
        members.dedup();
        match members.len() {
            0 => i.top(), // empty intersection = top
            1 => members[0],
            _ => i.intern(Type::Intersection(members)),
        }
    }
}

/// Is this union (structurally) exactly `true | false`? Used by display to
/// render `bool` without changing identity. Lives here because it inspects
/// union membership. Returns false for anything else.
pub fn is_bool_pair(i: &Interner, id: TypeId) -> bool {
    if let Type::Union(ms) = i.get(id) {
        if ms.len() == 2 {
            let mut has_true = false;
            let mut has_false = false;
            for &m in ms {
                match i.get(m) {
                    Type::Constant(Scalar::Bool(true)) => has_true = true,
                    Type::Constant(Scalar::Bool(false)) => has_false = true,
                    _ => return false,
                }
            }
            return has_true && has_false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nominal(i: &mut Interner, class: u32) -> TypeId {
        i.intern(Type::Nominal {
            class: crate::ty::ClassId(class),
            args: vec![],
        })
    }

    #[test]
    fn join_meet_difference_basic() {
        let mut i = Interner::new();
        let int = nominal(&mut i, 1);
        let bot = i.bottom();
        let top = i.top();

        // T | bot = T ; T & top = T ; T | top = top ; T & bot = bot.
        assert_eq!(Algebra::join(&mut i, int, bot), int);
        assert_eq!(Algebra::meet(&mut i, int, top), int);
        assert_eq!(Algebra::join(&mut i, int, top), top);
        assert_eq!(Algebra::meet(&mut i, int, bot), bot);

        // A - A = bot ; A - bot = A.
        assert_eq!(Algebra::difference(&mut i, int, int), bot);
        assert_eq!(Algebra::difference(&mut i, int, bot), int);
    }

    #[test]
    fn dynamic_join_identities() {
        let mut i = Interner::new();
        let top = i.top();
        let a = nominal(&mut i, 1);
        let b = nominal(&mut i, 2);
        let da = i.intern(Type::Dynamic(a));
        let db = i.intern(Type::Dynamic(b));

        // Dynamic[A] | Dynamic[B] = Dynamic[A | B]
        let lhs = Algebra::join(&mut i, da, db);
        let ab = Algebra::join(&mut i, a, b);
        let expected = i.intern(Type::Dynamic(ab));
        assert_eq!(lhs, expected);

        // T | Dynamic[U] = Dynamic[T | U]
        let lhs2 = Algebra::join(&mut i, a, db);
        let aub = Algebra::join(&mut i, a, b);
        let expected2 = i.intern(Type::Dynamic(aub));
        assert_eq!(lhs2, expected2);

        // untyped (Dynamic[top]) joined with anything stays Dynamic[top],
        // because top absorbs in the facet join.
        let untyped = i.intern(Type::Dynamic(top));
        let j = Algebra::join(&mut i, untyped, a);
        assert_eq!(j, untyped);
    }

    #[test]
    fn dynamic_meet_and_difference_preserve_wrapper() {
        let mut i = Interner::new();
        let top = i.top();
        let string = nominal(&mut i, 10);
        let untyped = i.intern(Type::Dynamic(top));

        // untyped & String = Dynamic[String], not plain String.
        let m = Algebra::meet(&mut i, untyped, string);
        let expected = i.intern(Type::Dynamic(string));
        assert_eq!(m, expected);
        assert_ne!(m, string);

        // Dynamic[T] - U = Dynamic[T - U]: wrapper preserved.
        let d = Algebra::difference(&mut i, untyped, string);
        match i.get(d) {
            Type::Dynamic(_) => {}
            other => panic!("expected Dynamic wrapper, got {other:?}"),
        }
    }

    #[test]
    fn constant_does_not_collapse_against_nominal_base() {
        // `1 | Integer` stays two members (no subsumption collapse).
        let mut i = Interner::new();
        let one = i.int(1);
        let integer = nominal(&mut i, 1); // pretend ClassId(1) == Integer
        let u = Algebra::join(&mut i, one, integer);
        match i.get(u) {
            Type::Union(ms) => assert_eq!(ms.len(), 2, "1 | Integer must keep 2 members"),
            other => panic!("expected a 2-member union, got {other:?}"),
        }
    }

    #[test]
    fn true_or_false_keeps_identity() {
        // true | false stays a 2-member union internally; display handles bool.
        let mut i = Interner::new();
        let t = i.true_();
        let f = i.false_();
        let u = Algebra::join(&mut i, t, f);
        assert!(is_bool_pair(&i, u));
        match i.get(u) {
            Type::Union(ms) => assert_eq!(ms.len(), 2),
            other => panic!("expected true|false union, got {other:?}"),
        }
    }

    #[test]
    fn union_member_order_is_deterministic() {
        let mut i = Interner::new();
        let a = nominal(&mut i, 5);
        let b = nominal(&mut i, 3);
        let u1 = Algebra::join(&mut i, a, b);
        let u2 = Algebra::join(&mut i, b, a);
        // Order-independent: both join orders intern to the same canonical id.
        assert_eq!(u1, u2);
    }

    #[test]
    fn nested_union_flattens_on_normalize() {
        let mut i = Interner::new();
        let a = nominal(&mut i, 1);
        let b = nominal(&mut i, 2);
        let c = nominal(&mut i, 3);
        let inner = i.intern(Type::Union(vec![a, b]));
        let outer = i.intern(Type::Union(vec![inner, c]));
        let n = Algebra::normalize(&mut i, outer);
        match i.get(n) {
            Type::Union(ms) => assert_eq!(ms.len(), 3, "nested union must flatten"),
            other => panic!("expected flattened 3-member union, got {other:?}"),
        }
    }
}
