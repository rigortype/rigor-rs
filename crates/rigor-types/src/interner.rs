//! The type interner: an arena of `Type` plus a hash-cons map producing
//! copyable [`TypeId`]s. Dedup is by structural equality (ADR-0005).

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::ty::{Scalar, Type, TypeId};

/// Arena + hash-cons map. Every structurally-equal `Type` interns to the same
/// [`TypeId`], so identity comparison of `TypeId`s decides structural equality
/// of the (shallow) carrier.
pub struct Interner {
    arena: Vec<Type>,
    dedup: HashMap<Type, TypeId>,
    // Cached ids of the pre-interned atomics.
    top: TypeId,
    bottom: TypeId,
    nil: TypeId,
    true_: TypeId,
    false_: TypeId,
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    /// Build an interner with the common atomics pre-interned.
    pub fn new() -> Self {
        let mut me = Interner {
            arena: Vec::new(),
            dedup: HashMap::new(),
            // placeholders, overwritten just below
            top: TypeId(0),
            bottom: TypeId(0),
            nil: TypeId(0),
            true_: TypeId(0),
            false_: TypeId(0),
        };
        me.top = me.intern(Type::Top);
        me.bottom = me.intern(Type::Bottom);
        me.nil = me.intern(Type::Constant(Scalar::Nil));
        me.true_ = me.intern(Type::Constant(Scalar::Bool(true)));
        me.false_ = me.intern(Type::Constant(Scalar::Bool(false)));
        me
    }

    /// Intern a carrier, returning its (deduplicated) handle.
    pub fn intern(&mut self, ty: Type) -> TypeId {
        if let Some(&id) = self.dedup.get(&ty) {
            return id;
        }
        let id = TypeId(self.arena.len() as u32);
        self.arena.push(ty.clone());
        self.dedup.insert(ty, id);
        id
    }

    /// Resolve a handle back to its carrier.
    pub fn get(&self, id: TypeId) -> &Type {
        &self.arena[id.0 as usize]
    }

    /// Number of distinct interned carriers.
    pub fn len(&self) -> usize {
        self.arena.len()
    }

    /// Whether the interner holds no carriers. Never true after [`Interner::new`].
    pub fn is_empty(&self) -> bool {
        self.arena.is_empty()
    }

    // --- pre-interned atomics ------------------------------------------------

    /// `top` — greatest static value type.
    pub fn top(&self) -> TypeId {
        self.top
    }
    /// `bot` — the empty type.
    pub fn bottom(&self) -> TypeId {
        self.bottom
    }
    /// The singleton `nil` value.
    pub fn nil(&self) -> TypeId {
        self.nil
    }
    /// `Constant[true]`.
    pub fn true_(&self) -> TypeId {
        self.true_
    }
    /// `Constant[false]`.
    pub fn false_(&self) -> TypeId {
        self.false_
    }

    /// `untyped == Dynamic[top]`. Interned on demand (depends on `top`).
    pub fn untyped(&mut self) -> TypeId {
        let top = self.top;
        self.intern(Type::Dynamic(top))
    }

    /// Convenience: intern an integer constant.
    pub fn int(&mut self, v: i64) -> TypeId {
        self.intern(Type::Constant(Scalar::Int(v)))
    }

    // --- total order over carriers (for canonicalization) --------------------

    /// A deterministic total order over interned carriers, used to canonicalize
    /// union/intersection member lists. Primary key is the carrier
    /// discriminant tag; ties break structurally, recursing through child
    /// `TypeId`s by *resolved structure* (not raw index) so the order is stable
    /// regardless of interning sequence.
    pub fn cmp(&self, a: TypeId, b: TypeId) -> Ordering {
        if a == b {
            return Ordering::Equal;
        }
        let (ta, tb) = (self.get(a), self.get(b));
        let tag = ta.tag().cmp(&tb.tag());
        if tag != Ordering::Equal {
            return tag;
        }
        match (ta, tb) {
            (Type::Constant(x), Type::Constant(y)) => x.cmp(y),
            (
                Type::IntegerRange { min: amin, max: amax },
                Type::IntegerRange { min: bmin, max: bmax },
            ) => amin.cmp(bmin).then(amax.cmp(bmax)),
            (
                Type::Nominal { class: ac, args: aargs },
                Type::Nominal { class: bc, args: bargs },
            ) => ac.cmp(bc).then_with(|| self.cmp_slice(aargs, bargs)),
            (Type::Tuple(x), Type::Tuple(y)) => self.cmp_slice(x, y),
            (Type::HashShape(x), Type::HashShape(y)) => {
                // Compare keys/optional structurally, values recursively.
                let by_len = x.len().cmp(&y.len());
                if by_len != Ordering::Equal {
                    return by_len;
                }
                for (mx, my) in x.iter().zip(y.iter()) {
                    let k = mx.key.cmp(&my.key);
                    if k != Ordering::Equal {
                        return k;
                    }
                    let o = mx.optional.cmp(&my.optional);
                    if o != Ordering::Equal {
                        return o;
                    }
                    let v = self.cmp(mx.value, my.value);
                    if v != Ordering::Equal {
                        return v;
                    }
                }
                Ordering::Equal
            }
            (
                Type::DataInstance { class: ac, members: am },
                Type::DataInstance { class: bc, members: bm },
            ) => {
                let c = ac.cmp(bc);
                if c != Ordering::Equal {
                    return c;
                }
                let by_len = am.len().cmp(&bm.len());
                if by_len != Ordering::Equal {
                    return by_len;
                }
                for ((an, av), (bn, bv)) in am.iter().zip(bm.iter()) {
                    let n = an.cmp(bn);
                    if n != Ordering::Equal {
                        return n;
                    }
                    let v = self.cmp(*av, *bv);
                    if v != Ordering::Equal {
                        return v;
                    }
                }
                Ordering::Equal
            }
            (
                Type::Refined { base: ab, refinement: ar },
                Type::Refined { base: bb, refinement: br },
            ) => self.cmp(*ab, *bb).then(ar.cmp(br)),
            (Type::App { uri: au, args: aa }, Type::App { uri: bu, args: ba }) => {
                au.cmp(bu).then_with(|| self.cmp_slice(aa, ba))
            }
            (Type::Intersection(x), Type::Intersection(y)) => self.cmp_slice(x, y),
            (
                Type::Difference { base: ab, removed: ar },
                Type::Difference { base: bb, removed: br },
            ) => self.cmp(*ab, *bb).then_with(|| self.cmp(*ar, *br)),
            (Type::Complement(x), Type::Complement(y)) => self.cmp(*x, *y),
            (Type::Dynamic(x), Type::Dynamic(y)) => self.cmp(*x, *y),
            (Type::Union(x), Type::Union(y)) => self.cmp_slice(x, y),
            // Tag-only carriers (Top, Bottom, Void, SelfType, Instance,
            // ClassType) are fully ordered by tag, already handled above.
            _ => Ordering::Equal,
        }
    }

    /// Lexicographic order over two slices of `TypeId`, using [`Interner::cmp`].
    fn cmp_slice(&self, a: &[TypeId], b: &[TypeId]) -> Ordering {
        for (x, y) in a.iter().zip(b.iter()) {
            let c = self.cmp(*x, *y);
            if c != Ordering::Equal {
                return c;
            }
        }
        a.len().cmp(&b.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_returns_same_id() {
        let mut i = Interner::new();
        let a = i.int(7);
        let b = i.int(7);
        assert_eq!(a, b);
        // A distinct literal interns to a distinct id.
        let c = i.int(8);
        assert_ne!(a, c);
    }

    #[test]
    fn atomics_are_preinterned_and_stable() {
        let i = Interner::new();
        assert_eq!(i.get(i.top()), &Type::Top);
        assert_eq!(i.get(i.bottom()), &Type::Bottom);
        assert_eq!(i.get(i.nil()), &Type::Constant(Scalar::Nil));
        assert_eq!(i.get(i.true_()), &Type::Constant(Scalar::Bool(true)));
        assert_eq!(i.get(i.false_()), &Type::Constant(Scalar::Bool(false)));
    }

    #[test]
    fn untyped_is_dynamic_top() {
        let mut i = Interner::new();
        let u = i.untyped();
        assert_eq!(i.get(u), &Type::Dynamic(i.top()));
        // ... and interns consistently.
        let u2 = i.untyped();
        assert_eq!(u, u2);
    }
}
