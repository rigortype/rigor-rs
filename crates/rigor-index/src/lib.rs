//! The index layer (ADR-0004): declaration discovery, ancestor linearization
//! (with visibility), constant/method resolution, built on the `ruby-rbs`
//! parser behind a rigor-rs-owned trait. Rubydex is an optional accelerator.
//!
//! Verified in the spike: RBS exposes typed method definitions (return types,
//! parameter types, variance, overloads, generics) — see spike/probe_rbs.rb.
//! The Rust `ruby-rbs` crate parses the same grammar (network-gated to confirm
//! its public API surfaces them; else a thin extraction layer over its AST).
//!
//! ## Tracer-bullet stub
//!
//! For the first vertical slice this crate ships a tiny *hardcoded* core-method
//! table — just enough to type-check method existence on a literal receiver
//! (ADR-0023 tier-0/tier-3 in miniature). The names below are real Ruby core
//! methods, but the table is intentionally partial.
//!
// TODO(spec): replace [`CoreIndex`] with the real RBS-backed index (`ruby-rbs`,
// network-gated per the module preamble): full ancestor linearization,
// visibility, overloads, generics, and the project-discovered in-source layer.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use rigor_types::{Interner, Scalar, Type, TypeId};

/// A small, hardcoded table of core classes to a subset of their real instance
/// method names. Used only to decide *method existence* on a known receiver
/// class in the tracer-bullet slice (ADR-0023).
pub struct CoreIndex {
    /// `class name -> set of known method names`.
    methods: HashMap<&'static str, HashSet<&'static str>>,
}

impl Default for CoreIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl CoreIndex {
    /// Build the hardcoded core-method table. Method lists are a real (but
    /// partial) subset of Ruby core; absence here is *not* proof a method is
    /// undefined in real Ruby, which is exactly why the rule only fires on a
    /// class that is present in this table (zero-false-positive, ADR-0023).
    pub fn new() -> Self {
        let mut methods: HashMap<&'static str, HashSet<&'static str>> = HashMap::new();

        methods.insert(
            "String",
            [
                "length", "size", "upcase", "downcase", "capitalize", "reverse",
                "strip", "chomp", "chars", "bytes", "split", "gsub", "sub",
                "include?", "start_with?", "end_with?", "empty?", "to_s",
                "to_str", "to_sym", "to_i", "to_f", "+", "*", "==", "<=>", "[]",
            ]
            .into_iter()
            .collect(),
        );

        methods.insert(
            "Integer",
            [
                "+", "-", "*", "/", "%", "**", "abs", "succ", "pred", "times",
                "upto", "downto", "to_s", "to_i", "to_f", "even?", "odd?",
                "zero?", "==", "<", ">", "<=", ">=", "<=>", "digits",
            ]
            .into_iter()
            .collect(),
        );

        methods.insert(
            "Float",
            [
                "+", "-", "*", "/", "abs", "ceil", "floor", "round", "to_s",
                "to_i", "to_f", "nan?", "infinite?", "==", "<=>",
            ]
            .into_iter()
            .collect(),
        );

        methods.insert(
            "Symbol",
            [
                "to_s", "to_sym", "to_proc", "length", "size", "upcase",
                "downcase", "==", "<=>", "[]",
            ]
            .into_iter()
            .collect(),
        );

        methods.insert(
            "TrueClass",
            ["to_s", "&", "|", "^", "!"].into_iter().collect(),
        );
        methods.insert(
            "FalseClass",
            ["to_s", "&", "|", "^", "!"].into_iter().collect(),
        );

        methods.insert(
            "NilClass",
            ["to_s", "to_a", "to_h", "to_i", "nil?", "inspect", "&", "|"]
                .into_iter()
                .collect(),
        );

        Self { methods }
    }

    /// Whether `class_name` is one this index models at all. The rule must stay
    /// silent on classes outside the table (ADR-0023: never guess).
    pub fn knows_class(&self, class_name: &str) -> bool {
        self.methods.contains_key(class_name)
    }

    /// Whether `class_name` is known to define an instance `method`. Returns
    /// `false` both for a known class missing the method *and* for an unknown
    /// class — callers MUST gate on [`CoreIndex::knows_class`] first to keep
    /// the zero-false-positive contract.
    pub fn class_has_method(&self, class_name: &str, method: &str) -> bool {
        self.methods
            .get(class_name)
            .is_some_and(|set| set.contains(method))
    }

    /// Map a concrete [`TypeId`] to its core class name, when known.
    ///
    /// Only *value-pinned* `Constant` literals and the nominal scalars are
    /// resolved here: `Constant["Hello"]` -> `"String"`, `Constant[3]` ->
    /// `"Integer"`, `nil` -> `"NilClass"`. A `Dynamic`/`top`/unknown carrier
    /// returns `None` so the rule stays silent (ADR-0023 tier-5 fallback).
    pub fn class_name_of(&self, interner: &Interner, ty: TypeId) -> Option<&'static str> {
        match interner.get(ty) {
            Type::Constant(scalar) => Some(scalar_class(scalar)),
            // TODO(spec): resolve `Type::Nominal { class, .. }` via the real
            // ClassId->name table once the RBS-backed index lands; the
            // tracer-bullet slice only ever types literal receivers.
            _ => None,
        }
    }
}

/// The Ruby core class name for a value-pinned scalar literal.
fn scalar_class(scalar: &Scalar) -> &'static str {
    match scalar {
        Scalar::Int(_) => "Integer",
        Scalar::Str(_) => "String",
        Scalar::Sym(_) => "Symbol",
        Scalar::Bool(true) => "TrueClass",
        Scalar::Bool(false) => "FalseClass",
        Scalar::Nil => "NilClass",
        Scalar::Float(_) => "Float",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rigor_types::Type;

    #[test]
    fn known_string_methods_resolve() {
        let idx = CoreIndex::new();
        assert!(idx.knows_class("String"));
        assert!(idx.class_has_method("String", "length"));
        assert!(!idx.class_has_method("String", "lenght"));
    }

    #[test]
    fn unknown_class_is_not_known() {
        let idx = CoreIndex::new();
        assert!(!idx.knows_class("MyWidget"));
        // Even a plausible method on an unmodeled class returns false.
        assert!(!idx.class_has_method("MyWidget", "call"));
    }

    #[test]
    fn class_name_of_literal_carriers() {
        let idx = CoreIndex::new();
        let mut i = Interner::new();
        let s = i.intern(Type::Constant(Scalar::Str("Hello".into())));
        let n = i.int(3);
        let nil = i.nil();
        assert_eq!(idx.class_name_of(&i, s), Some("String"));
        assert_eq!(idx.class_name_of(&i, n), Some("Integer"));
        assert_eq!(idx.class_name_of(&i, nil), Some("NilClass"));
    }

    #[test]
    fn class_name_of_dynamic_is_none() {
        let idx = CoreIndex::new();
        let mut i = Interner::new();
        let u = i.untyped();
        assert_eq!(idx.class_name_of(&i, u), None);
    }
}
