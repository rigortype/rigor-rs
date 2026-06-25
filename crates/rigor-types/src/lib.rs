//! The type lattice (ADR-0005): a single interned `Type` enum behind copyable
//! `TypeId` handles; lattice ops are exhaustive `match`. Identifiers/symbols are
//! interned, literal values kept inline. Variants are added reluctantly
//! (start narrow). This is a skeleton.
#![allow(dead_code)]

/// A copyable handle into the type interner.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TypeId(u32);

/// A scalar literal, carried inline (not interned) per ADR-0005.
#[derive(Clone, PartialEq, Debug)]
pub enum Scalar {
    Int(i64),
    Str(String),
    Sym(String),
    Bool(bool),
    Nil,
}

/// The type lattice (skeleton — variants grow with the inference engine).
#[derive(Clone, PartialEq, Debug)]
pub enum Type {
    Top,
    Bottom,
    /// First-class escape hatch (ADR-0001): gradual typing, not an error.
    Dynamic(TypeId),
    Nominal(u32),
    Constant(Scalar),
    /// Normalized through a builder (ADR-0005), never assembled directly.
    Union(Vec<TypeId>),
}

/// Interner skeleton — arena of `Type` + hash-consing for dedup (TODO).
#[derive(Default)]
pub struct Interner {}
