//! Scalar literals, newtype identifiers, and the interned `Type` carrier set.
//!
//! Grounded in `docs/type-specification/value-lattice.md` and `special-types.md`.
//! Per ADR-0005/0019: the carrier is interned behind a copyable [`TypeId`];
//! literal scalar payloads are kept inline rather than interned.

use std::cmp::Ordering;

/// A copyable handle into the type interner. Cheap to copy and compare.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct TypeId(pub(crate) u32);

impl TypeId {
    /// The raw interner index. Exposed for deterministic ordering / debugging.
    pub fn index(self) -> u32 {
        self.0
    }
}

/// Identifier of a nominal class (`Integer`, `String`, a user class, ...).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct ClassId(pub u32);

/// Identifier of a refinement predicate (`non-empty-string`, `positive-int`, ...).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct RefinementId(pub u32);

/// URI of a registered higher-kinded type constructor (ADR-20), e.g.
/// `json::value`. Stored as an owned string; the defunctionalised constructor
/// is resolved through the HKT registry elsewhere.
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct HktUri(pub String);

/// A scalar literal, carried inline (not interned) per ADR-0005.
///
/// `Float` carries an `f64`; to keep `Type` totally `Eq`/`Hash`/`Ord` for
/// hash-consing and canonical union ordering, the float is compared and hashed
/// by its raw bits (see [`Type`]'s manual trait impls), so e.g. two `NaN`s with
/// the same bit pattern are considered equal here.
#[derive(Clone, Debug)]
pub enum Scalar {
    Int(i64),
    Str(String),
    Sym(String),
    Bool(bool),
    Nil,
    Float(f64),
}

impl Scalar {
    /// A small, stable discriminant tag for ordering across scalar variants.
    fn tag(&self) -> u8 {
        match self {
            Scalar::Nil => 0,
            Scalar::Bool(_) => 1,
            Scalar::Int(_) => 2,
            Scalar::Float(_) => 3,
            Scalar::Str(_) => 4,
            Scalar::Sym(_) => 5,
        }
    }
}

impl PartialEq for Scalar {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Scalar::Int(a), Scalar::Int(b)) => a == b,
            (Scalar::Str(a), Scalar::Str(b)) => a == b,
            (Scalar::Sym(a), Scalar::Sym(b)) => a == b,
            (Scalar::Bool(a), Scalar::Bool(b)) => a == b,
            (Scalar::Nil, Scalar::Nil) => true,
            // Compare floats by raw bits so the type is a true equivalence
            // relation (required for hash-consing determinism).
            (Scalar::Float(a), Scalar::Float(b)) => a.to_bits() == b.to_bits(),
            _ => false,
        }
    }
}

impl Eq for Scalar {}

impl std::hash::Hash for Scalar {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.tag().hash(state);
        match self {
            Scalar::Int(v) => v.hash(state),
            Scalar::Str(v) => v.hash(state),
            Scalar::Sym(v) => v.hash(state),
            Scalar::Bool(v) => v.hash(state),
            Scalar::Nil => {}
            Scalar::Float(v) => v.to_bits().hash(state),
        }
    }
}

impl PartialOrd for Scalar {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Scalar {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Scalar::Int(a), Scalar::Int(b)) => a.cmp(b),
            (Scalar::Str(a), Scalar::Str(b)) => a.cmp(b),
            (Scalar::Sym(a), Scalar::Sym(b)) => a.cmp(b),
            (Scalar::Bool(a), Scalar::Bool(b)) => a.cmp(b),
            (Scalar::Nil, Scalar::Nil) => Ordering::Equal,
            (Scalar::Float(a), Scalar::Float(b)) => a.to_bits().cmp(&b.to_bits()),
            _ => self.tag().cmp(&other.tag()),
        }
    }
}

/// A key in a hash-shape. Ruby hash literals pin any value-pinned scalar key —
/// the reference's `HashShape::ALLOWED_KEY_CLASSES` (Symbol, String, Integer,
/// Float, true, false, nil). Key identity is Ruby `Hash#eql?`: `1` (an `Int`)
/// and `1.0` (a `Float`) are DISTINCT keys, while `1.0` and `1.00` collide —
/// reproduced here because `Float` is stored by its raw `f64` bits (matching
/// [`Scalar::Float`]'s `to_bits` convention), so the derived `Eq`/`Hash`/`Ord`
/// give the runtime's collision semantics for free.
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum ShapeKey {
    Sym(String),
    Str(String),
    Int(i64),
    /// A float key, stored by raw bits (see the type doc): `1.0` == `1.00`,
    /// distinct from the `Int(1)` key.
    Float(u64),
    /// The `true` / `false` singleton keys.
    Bool(bool),
    /// The `nil` singleton key.
    Nil,
    /// A non-literal / not-yet-modeled key (never built from a hash literal;
    /// retained as the erase/display fallback).
    Other,
}

/// One member of a [`Type::HashShape`]: a key, its value type, and whether the
/// key is *optional* (key-absent).
///
/// CRITICAL (special-types.md / normalization.md): an optional key is NOT the
/// same as a present-`nil` value. Absence is not a stored value, so the
/// `optional` flag MUST NOT be folded into the value type by nil-widening.
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct ShapeMember {
    pub key: ShapeKey,
    pub value: TypeId,
    pub optional: bool,
}

/// The interned type carrier set. The full lattice from value-lattice.md.
///
/// `Dynamic(TypeId)` wraps the static facet `T` of a dynamic-origin value
/// (`untyped == Dynamic[top]`). Per ADR note, dynamic *provenance* (which kind
/// of dynamic source) is a SIDE-CHANNEL and deliberately NOT a field here, so it
/// cannot affect structural equality or hashing.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Type {
    /// Greatest static value type (TypeScript `unknown`-like). Distinct from
    /// `untyped`/`Dynamic[top]`.
    Top,
    /// The empty type. `bot <: T` for all `T`.
    Bottom,
    /// Dynamic-origin wrapper around a static facet. `untyped == Dynamic[Top]`.
    /// Provenance is a side-channel, NOT carried here.
    Dynamic(TypeId),
    /// A nominal class applied to type arguments (`Array[Integer]`, ...).
    Nominal { class: ClassId, args: Vec<TypeId> },
    /// The class object itself (`singleton(C)`): the value of the constant `C`,
    /// distinct from `Nominal{C}` which is an instance of `C`.
    Singleton(ClassId),
    /// A value-pinned literal (`Constant[3]`, `Constant["hi"]`, ...).
    Constant(Scalar),
    /// Per-position array shape (`Tuple[Constant[1], Constant["a"]]`).
    Tuple(Vec<TypeId>),
    /// Per-key hash shape. Members preserve openness and optional/present-nil
    /// distinction (see [`ShapeMember`]).
    HashShape(Vec<ShapeMember>),
    /// A bounded integer range. `None` bound means open in that direction.
    IntegerRange { min: Option<i64>, max: Option<i64> },
    /// A base type restricted by a refinement predicate.
    Refined { base: TypeId, refinement: RefinementId },
    /// Set difference `base - removed` (negative fact / complement display).
    Difference { base: TypeId, removed: TypeId },
    /// Intersection of carriers (`A & B & ...`).
    Intersection(Vec<TypeId>),
    /// Complement `~T` of a carrier.
    Complement(TypeId),
    /// Defunctionalised higher-kinded application `App[uri, args...]` (ADR-20).
    App { uri: HktUri, args: Vec<TypeId> },
    /// A `Data.define` instance with named members.
    DataInstance { class: ClassId, members: Vec<(String, TypeId)> },
    /// Result marker: expression whose value should not be used. Not an
    /// ordinary value type (special-types.md).
    Void,
    /// Result marker: the receiver's own type (`self`).
    SelfType,
    /// Result marker: an instance of the surrounding class.
    Instance,
    /// Result marker: the class object itself.
    ClassType,
    /// Union of carriers (`A | B | ...`). Built only through the normalizing
    /// builder; members held in canonical order.
    Union(Vec<TypeId>),
}

impl Type {
    /// Stable discriminant tag, used as the primary key of the total order over
    /// `Type` (canonical union/intersection member ordering). The numeric
    /// values are arbitrary but MUST stay stable for deterministic output.
    pub(crate) fn tag(&self) -> u8 {
        match self {
            Type::Bottom => 0,
            Type::Top => 1,
            Type::Constant(_) => 2,
            Type::IntegerRange { .. } => 3,
            Type::Nominal { .. } => 4,
            Type::Tuple(_) => 5,
            Type::HashShape(_) => 6,
            Type::DataInstance { .. } => 7,
            Type::Refined { .. } => 8,
            Type::App { .. } => 9,
            Type::Intersection(_) => 10,
            Type::Difference { .. } => 11,
            Type::Complement(_) => 12,
            Type::Dynamic(_) => 13,
            Type::Union(_) => 14,
            Type::Void => 15,
            Type::SelfType => 16,
            Type::Instance => 17,
            Type::ClassType => 18,
            Type::Singleton(_) => 19,
        }
    }
}
