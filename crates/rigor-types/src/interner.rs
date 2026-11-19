//! The type interner: an arena of `Type` plus a hash-cons map producing
//! copyable [`TypeId`]s. Dedup is by structural equality (ADR-0005).

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use crate::ty::{Scalar, Type, TypeId};

/// The next [`Interner::uid`].
static NEXT_UID: AtomicU64 = AtomicU64::new(1);

/// Arena + hash-cons map. Every structurally-equal `Type` interns to the same
/// [`TypeId`], so identity comparison of `TypeId`s decides structural equality
/// of the (shallow) carrier.
pub struct Interner {
    /// A process-unique id — memo keys compare this instead of an address,
    /// which a dropped interner's replacement can reuse (rigor-rs#658:
    /// `infer_one_return` drops a scratch interner per method in the same
    /// stack slot).
    uid: u64,
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
            uid: NEXT_UID.fetch_add(1, AtomicOrdering::Relaxed),
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

    /// The process-unique id minted at construction — see the field docs.
    pub fn uid(&self) -> u64 {
        self.uid
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
    /// discriminant tag; ties break structurally through child `TypeId`s by
    /// *resolved structure* (not raw index) so the order is stable regardless
    /// of interning sequence.
    ///
    /// Iterative: an inferred carrier's depth is unbounded (rigor-rs#427), so
    /// the structural walk runs on an explicit heap stack instead of the call
    /// stack. Each popped [`CmpWork::Pair`] expands to the same left-to-right
    /// comparison sequence the recursion produced, pushed in reverse so the
    /// earliest pending unit sits on top; `Equal` atoms and same-id pairs are
    /// never pushed (both are no-ops), so the stack holds live depth only.
    pub fn cmp(&self, a: TypeId, b: TypeId) -> Ordering {
        let mut stack = vec![CmpWork::Pair(a, b)];
        while let Some(work) = stack.pop() {
            match work {
                CmpWork::Atom(ord) => {
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
                CmpWork::Pair(x, y) => {
                    if x == y {
                        continue;
                    }
                    let (ta, tb) = (self.get(x), self.get(y));
                    let tag = ta.tag().cmp(&tb.tag());
                    if tag != Ordering::Equal {
                        return tag;
                    }
                    match (ta, tb) {
                        (Type::Constant(x), Type::Constant(y)) => {
                            push_cmp_atom(&mut stack, x.cmp(y))
                        }
                        (
                            Type::IntegerRange { min: amin, max: amax },
                            Type::IntegerRange { min: bmin, max: bmax },
                        ) => {
                            push_cmp_atom(&mut stack, amax.cmp(bmax));
                            push_cmp_atom(&mut stack, amin.cmp(bmin));
                        }
                        (
                            Type::Nominal { class: ac, args: aargs },
                            Type::Nominal { class: bc, args: bargs },
                        ) => {
                            push_cmp_slice(&mut stack, aargs, bargs);
                            push_cmp_atom(&mut stack, ac.cmp(bc));
                        }
                        // Singleton has no args: order solely by its ClassId.
                        (Type::Singleton(ac), Type::Singleton(bc)) => {
                            push_cmp_atom(&mut stack, ac.cmp(bc))
                        }
                        (Type::Tuple(x), Type::Tuple(y)) => {
                            push_cmp_slice(&mut stack, x, y)
                        }
                        (Type::HashShape(x), Type::HashShape(y)) => {
                            // Per member: key, optional flag, then the value
                            // pair; the member-count atom leads the sequence.
                            for (mx, my) in x.iter().zip(y.iter()).rev() {
                                push_cmp_pair(&mut stack, mx.value, my.value);
                                push_cmp_atom(&mut stack, mx.optional.cmp(&my.optional));
                                push_cmp_atom(&mut stack, mx.key.cmp(&my.key));
                            }
                            push_cmp_atom(&mut stack, x.len().cmp(&y.len()));
                        }
                        (
                            Type::DataInstance { class: ac, members: am },
                            Type::DataInstance { class: bc, members: bm },
                        ) => {
                            // Class first, then count, then per member the
                            // name atom and the value pair.
                            for ((an, av), (bn, bv)) in am.iter().zip(bm.iter()).rev() {
                                push_cmp_pair(&mut stack, *av, *bv);
                                push_cmp_atom(&mut stack, an.cmp(bn));
                            }
                            push_cmp_atom(&mut stack, am.len().cmp(&bm.len()));
                            push_cmp_atom(&mut stack, ac.cmp(bc));
                        }
                        (
                            Type::Refined { base: ab, refinement: ar },
                            Type::Refined { base: bb, refinement: br },
                        ) => {
                            push_cmp_atom(&mut stack, ar.cmp(br));
                            push_cmp_pair(&mut stack, *ab, *bb);
                        }
                        (
                            Type::App { uri: au, args: aa },
                            Type::App { uri: bu, args: ba },
                        ) => {
                            push_cmp_slice(&mut stack, aa, ba);
                            push_cmp_atom(&mut stack, au.cmp(bu));
                        }
                        (Type::Intersection(x), Type::Intersection(y)) => {
                            push_cmp_slice(&mut stack, x, y)
                        }
                        (
                            Type::Difference { base: ab, removed: ar },
                            Type::Difference { base: bb, removed: br },
                        ) => {
                            push_cmp_pair(&mut stack, *ar, *br);
                            push_cmp_pair(&mut stack, *ab, *bb);
                        }
                        (Type::Complement(x), Type::Complement(y)) => {
                            push_cmp_pair(&mut stack, *x, *y)
                        }
                        (Type::Dynamic(x), Type::Dynamic(y)) => {
                            push_cmp_pair(&mut stack, *x, *y)
                        }
                        (Type::Union(x), Type::Union(y)) => {
                            push_cmp_slice(&mut stack, x, y)
                        }
                        // Tag-only carriers (Top, Bottom, Void, SelfType,
                        // Instance, ClassType) are fully ordered by tag,
                        // already handled above.
                        _ => {}
                    }
                }
            }
        }
        Ordering::Equal
    }
}

/// One pending unit of [`Interner::cmp`]'s structural walk: an `Atom` is a
/// scalar ordering resolved eagerly at expansion time (a `ClassId`, key or
/// arity comparison); a `Pair` defers a child-carrier comparison so the walk
/// never recurses (rigor-rs#427).
enum CmpWork {
    Atom(Ordering),
    Pair(TypeId, TypeId),
}

/// Push a scalar ordering unless it is `Equal` (a no-op in the sequence).
fn push_cmp_atom(stack: &mut Vec<CmpWork>, ord: Ordering) {
    if ord != Ordering::Equal {
        stack.push(CmpWork::Atom(ord));
    }
}

/// Push a child comparison unless both ids coincide (always `Equal`).
fn push_cmp_pair(stack: &mut Vec<CmpWork>, x: TypeId, y: TypeId) {
    if x != y {
        stack.push(CmpWork::Pair(x, y));
    }
}

/// The pending work of a lexicographic slice comparison, pushed in reverse:
/// the length tie-break first (evaluated last), then the pairwise children
/// last-first — the same order the recursion's zipped walk plus trailing
/// `a.len().cmp(&b.len())` produced.
fn push_cmp_slice(stack: &mut Vec<CmpWork>, a: &[TypeId], b: &[TypeId]) {
    push_cmp_atom(stack, a.len().cmp(&b.len()));
    for (&x, &y) in a.iter().zip(b.iter()).rev() {
        push_cmp_pair(stack, x, y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::{ClassId, ShapeKey, ShapeMember};

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
    fn singleton_interns_by_class_id() {
        let mut i = Interner::new();
        // Two singletons of the same class intern to the same id.
        let a = i.intern(Type::Singleton(ClassId(7)));
        let b = i.intern(Type::Singleton(ClassId(7)));
        assert_eq!(a, b);
        // Singleton(7) != Singleton(8).
        let c = i.intern(Type::Singleton(ClassId(8)));
        assert_ne!(a, c);
        // Singleton(7) is distinct from Nominal{class: 7} (the instance).
        let n = i.intern(Type::Nominal {
            class: ClassId(7),
            args: vec![],
        });
        assert_ne!(a, n);
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

    /// `D` levels of `Tuple([…])` wrapped around `inner`.
    fn nest(i: &mut Interner, inner: TypeId, d: usize) -> TypeId {
        let mut t = inner;
        for _ in 0..d {
            t = i.intern(Type::Tuple(vec![t]));
        }
        t
    }

    #[test]
    fn cmp_pins_lexicographic_order() {
        let mut i = Interner::new();
        let one = i.int(1);
        let two = i.int(2);
        assert_eq!(i.cmp(one, one), Ordering::Equal);
        assert_eq!(i.cmp(one, two), Ordering::Less);
        assert_eq!(i.cmp(two, one), Ordering::Greater);
        // Tag gap wins over content: Top (1) < Constant (2) < Tuple (5).
        let top = i.top();
        assert_eq!(i.cmp(top, one), Ordering::Less);
        assert_eq!(i.cmp(one, top), Ordering::Greater);

        // Tuple: pairwise members first, then length (a prefix sorts lower).
        let t1 = i.intern(Type::Tuple(vec![one]));
        let t12 = i.intern(Type::Tuple(vec![one, two]));
        let t2 = i.intern(Type::Tuple(vec![two]));
        assert_eq!(i.cmp(t1, t12), Ordering::Less);
        assert_eq!(i.cmp(t2, t1), Ordering::Greater);
        assert_eq!(i.cmp(t1, t2), Ordering::Less);
        assert_eq!(i.cmp(two, t2), Ordering::Less);

        // One level down decides a nested pair.
        let deep1 = i.intern(Type::Tuple(vec![t1]));
        let deep2 = i.intern(Type::Tuple(vec![t2]));
        assert_eq!(i.cmp(deep1, deep2), Ordering::Less);

        // HashShape: member count, then key, then optional, then value.
        let member = |key: &str, value: TypeId, optional: bool| ShapeMember {
            key: ShapeKey::Sym(key.to_string()),
            value,
            optional,
        };
        let a1 = i.intern(Type::HashShape(vec![member("a", one, false)]));
        let a2 = i.intern(Type::HashShape(vec![member("a", two, false)]));
        let b1 = i.intern(Type::HashShape(vec![member("b", one, false)]));
        let a1_opt = i.intern(Type::HashShape(vec![member("a", one, true)]));
        assert_eq!(i.cmp(a1, a2), Ordering::Less); // value decides
        assert_eq!(i.cmp(a1, b1), Ordering::Less); // key decides
        assert_eq!(i.cmp(a1, a1_opt), Ordering::Less); // required < optional
        let two_members = i.intern(Type::HashShape(vec![
            member("a", one, false),
            member("b", one, false),
        ]));
        assert_eq!(i.cmp(a1, two_members), Ordering::Less); // count leads

        // Union: Union tag (14) < Singleton tag (19); members sort pairwise
        // then by count.
        let u12 = i.intern(Type::Union(vec![one, two]));
        let u1 = i.intern(Type::Union(vec![one]));
        let s = i.intern(Type::Singleton(ClassId(1)));
        assert_eq!(i.cmp(u12, u1), Ordering::Greater);
        assert_eq!(i.cmp(u12, s), Ordering::Less);
    }

    #[test]
    fn cmp_deep_chains_use_heap_not_call_stack() {
        // rigor-rs#427: two same-tag chains differing only at the leaf. The
        // recursive cmp overflowed a 512 KiB stack far below this depth; the
        // explicit work stack is bounded by heap, not frames.
        const DEPTH: usize = 200_000;
        let mut i = Interner::new();
        let leaf_a = i.intern(Type::Nominal {
            class: ClassId(1),
            args: vec![],
        });
        let leaf_b = i.intern(Type::Nominal {
            class: ClassId(2),
            args: vec![],
        });
        let a = nest(&mut i, leaf_a, DEPTH);
        let b = nest(&mut i, leaf_b, DEPTH);
        let handle = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || i.cmp(a, b))
            .unwrap();
        // Class 1 < Class 2 at the leaf decides the whole comparison.
        assert_eq!(handle.join().unwrap(), Ordering::Less);
    }
}
