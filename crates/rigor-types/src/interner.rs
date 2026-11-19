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
    /// Iterative, allocation-free: hash-consing dedups carriers by structure,
    /// so two distinct `TypeId`s can never compare `Equal` — the FIRST
    /// differing unit of the left-to-right expansion decides the whole
    /// comparison. Each level scans its leading atoms and descends into its
    /// first differing child pair (everything after it is moot), reproducing
    /// the recursion's order and early exit with no call stack and no work
    /// stack — depth is bounded by neither (rigor-rs#427).
    pub fn cmp(&self, a: TypeId, b: TypeId) -> Ordering {
        let (mut a, mut b) = (a, b);
        'pairs: loop {
            if a == b {
                return Ordering::Equal;
            }
            let (ta, tb) = (self.get(a), self.get(b));
            let tag = ta.tag().cmp(&tb.tag());
            if tag != Ordering::Equal {
                return tag;
            }
            match (ta, tb) {
                (Type::Constant(x), Type::Constant(y)) => return x.cmp(y),
                (
                    Type::IntegerRange { min: amin, max: amax },
                    Type::IntegerRange { min: bmin, max: bmax },
                ) => return amin.cmp(bmin).then(amax.cmp(bmax)),
                (
                    Type::Nominal { class: ac, args: aargs },
                    Type::Nominal { class: bc, args: bargs },
                ) => {
                    let c = ac.cmp(bc);
                    if c != Ordering::Equal {
                        return c;
                    }
                    match first_differing(aargs, bargs) {
                        Some((x, y)) => {
                            a = x;
                            b = y;
                        }
                        None => return aargs.len().cmp(&bargs.len()),
                    }
                }
                // Singleton has no args: order solely by its ClassId.
                (Type::Singleton(ac), Type::Singleton(bc)) => return ac.cmp(bc),
                (Type::Tuple(x), Type::Tuple(y))
                | (Type::Intersection(x), Type::Intersection(y))
                | (Type::Union(x), Type::Union(y)) => match first_differing(x, y) {
                    Some((x, y)) => {
                        a = x;
                        b = y;
                    }
                    None => return x.len().cmp(&y.len()),
                },
                (Type::HashShape(x), Type::HashShape(y)) => {
                    // Member count, then per member: key, optional flag, then
                    // the value pair — the first differing value decides.
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
                        if mx.value != my.value {
                            a = mx.value;
                            b = my.value;
                            continue 'pairs;
                        }
                    }
                    return Ordering::Equal;
                }
                (
                    Type::DataInstance { class: ac, members: am },
                    Type::DataInstance { class: bc, members: bm },
                ) => {
                    // Class, then member count, then per member: the name and
                    // the value pair.
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
                        if av != bv {
                            a = *av;
                            b = *bv;
                            continue 'pairs;
                        }
                    }
                    return Ordering::Equal;
                }
                (
                    Type::Refined { base: ab, refinement: ar },
                    Type::Refined { base: bb, refinement: br },
                ) => {
                    if ab != bb {
                        a = *ab;
                        b = *bb;
                    } else {
                        return ar.cmp(br);
                    }
                }
                (Type::App { uri: au, args: aa }, Type::App { uri: bu, args: ba }) => {
                    let u = au.cmp(bu);
                    if u != Ordering::Equal {
                        return u;
                    }
                    match first_differing(aa, ba) {
                        Some((x, y)) => {
                            a = x;
                            b = y;
                        }
                        None => return aa.len().cmp(&ba.len()),
                    }
                }
                (
                    Type::Difference { base: ab, removed: ar },
                    Type::Difference { base: bb, removed: br },
                ) => {
                    if ab != bb {
                        a = *ab;
                        b = *bb;
                    } else if ar != br {
                        a = *ar;
                        b = *br;
                    } else {
                        return Ordering::Equal;
                    }
                }
                (Type::Complement(x), Type::Complement(y))
                | (Type::Dynamic(x), Type::Dynamic(y)) => {
                    a = *x;
                    b = *y;
                }
                // Tag-only carriers (Top, Bottom, Void, SelfType, Instance,
                // ClassType) are fully ordered by tag, already handled above.
                _ => return Ordering::Equal,
            }
        }
    }
}

/// The first pair of differing ids between two member slices (the unit the
/// comparison descends into), or `None` when every shared position agrees.
fn first_differing(a: &[TypeId], b: &[TypeId]) -> Option<(TypeId, TypeId)> {
    a.iter()
        .zip(b.iter())
        .find(|&(&x, &y)| x != y)
        .map(|(&x, &y)| (x, y))
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

    /// The recursive `cmp` this PR replaced, kept verbatim as the differential
    /// oracle for [`Interner::cmp`]'s equivalence fuzz.
    fn recursive_cmp(i: &Interner, a: TypeId, b: TypeId) -> Ordering {
        fn cmp_slice(i: &Interner, a: &[TypeId], b: &[TypeId]) -> Ordering {
            for (x, y) in a.iter().zip(b.iter()) {
                let c = recursive_cmp(i, *x, *y);
                if c != Ordering::Equal {
                    return c;
                }
            }
            a.len().cmp(&b.len())
        }
        if a == b {
            return Ordering::Equal;
        }
        let (ta, tb) = (i.get(a), i.get(b));
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
            ) => ac.cmp(bc).then_with(|| cmp_slice(i, aargs, bargs)),
            (Type::Singleton(ac), Type::Singleton(bc)) => ac.cmp(bc),
            (Type::Tuple(x), Type::Tuple(y)) => cmp_slice(i, x, y),
            (Type::HashShape(x), Type::HashShape(y)) => {
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
                    let v = recursive_cmp(i, mx.value, my.value);
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
                    let v = recursive_cmp(i, *av, *bv);
                    if v != Ordering::Equal {
                        return v;
                    }
                }
                Ordering::Equal
            }
            (
                Type::Refined { base: ab, refinement: ar },
                Type::Refined { base: bb, refinement: br },
            ) => recursive_cmp(i, *ab, *bb).then(ar.cmp(br)),
            (Type::App { uri: au, args: aa }, Type::App { uri: bu, args: ba }) => {
                au.cmp(bu).then_with(|| cmp_slice(i, aa, ba))
            }
            (Type::Intersection(x), Type::Intersection(y)) => cmp_slice(i, x, y),
            (
                Type::Difference { base: ab, removed: ar },
                Type::Difference { base: bb, removed: br },
            ) => recursive_cmp(i, *ab, *bb).then_with(|| recursive_cmp(i, *ar, *br)),
            (Type::Complement(x), Type::Complement(y)) => recursive_cmp(i, *x, *y),
            (Type::Dynamic(x), Type::Dynamic(y)) => recursive_cmp(i, *x, *y),
            (Type::Union(x), Type::Union(y)) => cmp_slice(i, x, y),
            _ => Ordering::Equal,
        }
    }

    /// A deterministic xorshift so the fuzz needs no dev-dependency.
    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// A random shallow tree of carrier shapes covering every `cmp` arm that
    /// produces children (Tuple/Union/Intersection/HashShape/DataInstance/
    /// Nominal/App args, Refined/Difference/Complement/Dynamic wrappers).
    fn random_ty(i: &mut Interner, rng: &mut XorShift, depth: u32) -> TypeId {
        let pick = |i: &mut Interner, rng: &mut XorShift, n: u32| -> Vec<TypeId> {
            (0..n)
                .map(|_| random_ty(i, rng, depth.saturating_sub(1)))
                .collect()
        };
        if depth == 0 {
            return match rng.below(6) {
                0 => i.top(),
                1 => i.bottom(),
                2 => i.int(rng.below(8) as i64),
                3 => i.intern(Type::Constant(Scalar::Str(format!("s{}", rng.below(4))))),
                4 => i.intern(Type::IntegerRange {
                    min: Some(rng.below(4) as i64),
                    max: Some(4 + rng.below(4) as i64),
                }),
                _ => i.intern(Type::Singleton(ClassId(rng.below(4) as u32))),
            };
        }
        let n = (rng.below(3) + 1) as u32;
        let kids = pick(i, rng, n);
        match rng.below(11) {
            0 => i.intern(Type::Tuple(kids)),
            1 => i.intern(Type::Union(kids)),
            2 => i.intern(Type::Intersection(kids)),
            3 => i.intern(Type::HashShape(
                kids.iter()
                    .map(|&v| ShapeMember {
                        key: ShapeKey::Sym(format!("k{}", rng.below(4))),
                        value: v,
                        optional: rng.below(2) == 0,
                    })
                    .collect(),
            )),
            4 => i.intern(Type::DataInstance {
                class: ClassId(rng.below(3) as u32),
                members: kids
                    .iter()
                    .map(|&v| (format!("m{}", rng.below(4)), v))
                    .collect(),
            }),
            5 => i.intern(Type::Nominal {
                class: ClassId(rng.below(4) as u32),
                args: kids,
            }),
            6 => i.intern(Type::Refined {
                base: kids[0],
                refinement: crate::ty::RefinementId(rng.below(3) as u32),
            }),
            7 => i.intern(Type::Difference {
                base: kids[0],
                removed: *kids.last().unwrap(),
            }),
            8 => i.intern(Type::Complement(kids[0])),
            9 => i.intern(Type::App {
                uri: crate::ty::HktUri(format!("uri{}", rng.below(3))),
                args: kids,
            }),
            _ => i.intern(Type::Dynamic(kids[0])),
        }
    }

    #[test]
    fn cmp_matches_the_recursion_on_random_types() {
        // Differential fuzz: the stackless loop vs the verbatim recursion,
        // ~50k pairs over trees of depth <= 4. The loop relies on "distinct
        // ids are never deep-equal", so also assert it never returns Equal
        // for distinct ids.
        let mut rng = XorShift(0x9E3779B97F4A7C15);
        let mut i = Interner::new();
        let mut pool: Vec<TypeId> = Vec::new();
        for _ in 0..300 {
            pool.push(random_ty(&mut i, &mut rng, 3));
        }
        for _ in 0..50_000 {
            let a = pool[rng.below(pool.len() as u64) as usize];
            let b = pool[rng.below(pool.len() as u64) as usize];
            let got = i.cmp(a, b);
            let want = recursive_cmp(&i, a, b);
            assert_eq!(got, want, "cmp diverged on {a:?} vs {b:?}");
            if a != b {
                assert_ne!(got, Ordering::Equal, "distinct ids compared Equal");
            }
        }
    }
}
