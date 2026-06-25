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

use rigor_types::{ClassId, Interner, Scalar, Type, TypeId};

/// The core classes this index registers, in a fixed order. The slice index of
/// a name in this array IS its [`ClassId`] (see [`CoreIndex::class_id`]), so the
/// mapping is stable and reversible (ADR-0019: a `Type::Nominal { class }` can
/// be mapped back to its name).
///
// TODO(spec): the real RBS-backed index assigns ClassIds across the full
// ancestor graph (user classes, modules, generics); this fixed core array is
// the tracer-bullet stand-in (ADR-0004).
const CORE_CLASSES: [&str; 9] = [
    "String",
    "Integer",
    "Float",
    "Symbol",
    "Array",
    "Hash",
    "NilClass",
    "TrueClass",
    "FalseClass",
];

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

        methods.insert(
            "Array",
            [
                "length", "size", "first", "last", "push", "pop", "shift",
                "unshift", "map", "each", "select", "reject", "include?",
                "empty?", "reverse", "sort", "join", "to_a", "+", "*", "==",
                "[]", "<<",
            ]
            .into_iter()
            .collect(),
        );

        methods.insert(
            "Hash",
            [
                "length", "size", "keys", "values", "fetch", "store", "merge",
                "each", "map", "select", "reject", "include?", "key?", "empty?",
                "to_h", "to_a", "==", "[]", "[]=",
            ]
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
    ///
    /// A method counts as "known on a class" iff it appears in the union of the
    /// curated method set AND the return-type / arity stub tables. Those tables
    /// add real methods the bare `methods` set omits, so keeping them consistent
    /// here prevents a folded/typed method from being flagged as undefined.
    pub fn class_has_method(&self, class_name: &str, method: &str) -> bool {
        if self
            .methods
            .get(class_name)
            .is_some_and(|set| set.contains(method))
        {
            return true;
        }
        // Consistency: a method with a known return type or arity is a known
        // method, even if it was omitted from the curated `methods` set.
        method_return(class_name, method).is_some() || method_arity(class_name, method).is_some()
    }

    // --- class registry (name <-> ClassId) -----------------------------------

    /// Intern a core class name to its stable [`ClassId`], if registered. The
    /// id is the position of the name in [`CORE_CLASSES`] (ADR-0019: a
    /// `Type::Nominal { class }` carries this id and is reversible).
    pub fn class_id(&self, class_name: &str) -> Option<ClassId> {
        CORE_CLASSES
            .iter()
            .position(|&c| c == class_name)
            .map(|idx| ClassId(idx as u32))
    }

    /// Resolve a [`ClassId`] back to its core class name, if it names a
    /// registered core class.
    pub fn class_name_for_id(&self, class: ClassId) -> Option<&'static str> {
        CORE_CLASSES.get(class.0 as usize).copied()
    }

    /// Map a concrete [`TypeId`] to its core class name, when known.
    ///
    /// Resolved carriers:
    /// - *value-pinned* `Constant` literals and nominal scalars:
    ///   `Constant["Hello"]` -> `"String"`, `Constant[3]` -> `"Integer"`,
    ///   `nil` -> `"NilClass"`.
    /// - `Type::Nominal { class, .. }` whose `ClassId` names a registered core
    ///   class (ADR-0019) — this is what lets a CHAINED call's result type
    ///   (`s.downcase : String`) resolve so the next `.lenght` can be flagged.
    ///
    /// A `Dynamic`/`top`/unknown carrier returns `None` so the rule stays silent
    /// (ADR-0023 tier-5 fallback).
    pub fn class_name_of(&self, interner: &Interner, ty: TypeId) -> Option<&'static str> {
        match interner.get(ty) {
            Type::Constant(scalar) => Some(scalar_class(scalar)),
            Type::Nominal { class, .. } => self.class_name_for_id(*class),
            // TODO(spec): resolve refined / shaped carriers (Tuple -> Array,
            // HashShape -> Hash, IntegerRange -> Integer) once the RBS-backed
            // index lands; the tracer-bullet slice resolves literals + nominals.
            _ => None,
        }
    }
}

/// The RETURN class name of a curated core-method stub set: given a receiver
/// `class` and `method`, the class of the value the call evaluates to.
///
/// Used by tier-3-ish dispatch (ADR-0023) so a CHAINED call types correctly:
/// `s.downcase` -> `"String"` lets the next `.lenght` resolve against `String`
/// and flag the typo. Returns `None` when the return is unknown / not modeled
/// (e.g. `Array#first` whose element type we don't track) so the receiver
/// degrades to `Dynamic[top]` rather than guessing.
///
/// Boolean-returning predicates report `"TrueClass"` as their representative
/// boolean class — it is a real class that the index models and on which the
/// common boolean methods exist, sufficient for method-existence checks in this
/// slice.
///
// TODO(spec): real RBS-backed return types, overloads, generics. `Array#first`
// needs the element type; predicate returns should be the `bool` union
// (`true | false`) not a single class; encoding/format methods route to the
// Ruby sidecar (ADR-0008).
pub fn method_return(class: &str, method: &str) -> Option<&'static str> {
    let ret = match (class, method) {
        // String -> String
        (
            "String",
            "upcase" | "downcase" | "reverse" | "strip" | "chomp" | "to_s"
            | "to_str" | "gsub" | "sub" | "capitalize" | "+" | "*",
        ) => "String",
        // String -> Integer
        ("String", "length" | "size" | "to_i" | "index") => "Integer",
        // String -> Symbol
        ("String", "to_sym") => "Symbol",
        // String -> bool (represented by TrueClass)
        ("String", "include?" | "start_with?" | "end_with?" | "empty?") => "TrueClass",

        // Integer -> Integer
        ("Integer", "+" | "-" | "*" | "/" | "%" | "**" | "abs" | "succ" | "pred") => "Integer",
        // Integer -> String
        ("Integer", "to_s") => "String",
        // Integer -> bool (TrueClass)
        ("Integer", "even?" | "odd?" | "zero?") => "TrueClass",

        // Float -> Float / Integer / String
        ("Float", "+" | "-" | "*" | "/" | "abs" | "round" | "ceil" | "floor") => "Float",
        ("Float", "to_i") => "Integer",
        ("Float", "to_s") => "String",
        ("Float", "nan?" | "infinite?") => "TrueClass",

        // Symbol -> String / Symbol / bool
        ("Symbol", "to_s") => "String",
        ("Symbol", "to_sym") => "Symbol",
        ("Symbol", "length" | "size") => "Integer",

        // Array -> Integer (size queries); element-typed methods left None.
        ("Array", "length" | "size") => "Integer",
        ("Array", "empty?" | "include?") => "TrueClass",
        ("Array", "reverse" | "sort") => "Array",
        ("Array", "join" | "to_s") => "String",
        // Array#first / #last return the element type (unknown here) -> None.

        // Hash -> Integer / Array / bool
        ("Hash", "length" | "size") => "Integer",
        ("Hash", "keys" | "values" | "to_a") => "Array",
        ("Hash", "empty?" | "include?" | "key?") => "TrueClass",

        // NilClass
        ("NilClass", "to_s") => "String",
        ("NilClass", "to_a") => "Array",
        ("NilClass", "to_i") => "Integer",
        ("NilClass", "nil?") => "TrueClass",

        // TrueClass / FalseClass
        ("TrueClass" | "FalseClass", "to_s") => "String",
        ("TrueClass" | "FalseClass", "&" | "|" | "^" | "!") => "TrueClass",

        _ => return None,
    };
    Some(ret)
}

/// The arity contract `(min, max)` of a curated core-method stub set, where
/// `max == None` means variadic (no upper bound). Returns `None` when the
/// method's arity is not modeled here.
///
/// Only a handful are pinned in this slice — enough to exercise the table; the
/// rest are unmodeled.
///
// TODO(spec): real RBS-backed arities, including optional/keyword/block
// parameters and per-overload arities (ADR-0023 tier-3).
pub fn method_arity(class: &str, method: &str) -> Option<(usize, Option<usize>)> {
    let arity = match (class, method) {
        // String
        ("String", "upcase") => (0, Some(0)),
        ("String", "downcase") => (0, Some(0)),
        ("String", "length") => (0, Some(0)),
        ("String", "size") => (0, Some(0)),
        // `gsub`/`sub` accept either a replacement String (2 args) or a block
        // (1 arg), so the positional-arity envelope is 1..2 (matches the
        // reference's `expected 1..2`). 3+ positional args is wrong-arity.
        ("String", "gsub") => (1, Some(2)),
        ("String", "sub") => (1, Some(2)),
        ("String", "include?") => (1, Some(1)),
        ("String", "+") => (1, Some(1)),
        ("String", "*") => (1, Some(1)),

        // Integer
        ("Integer", "abs") => (0, Some(0)),
        ("Integer", "succ") => (0, Some(0)),
        ("Integer", "+") => (1, Some(1)),
        ("Integer", "to_s") => (0, Some(1)), // optional radix

        // Array (variadic example)
        ("Array", "push") => (0, None),

        _ => return None,
    };
    Some(arity)
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

    #[test]
    fn class_id_round_trips() {
        let idx = CoreIndex::new();
        for name in ["String", "Integer", "Float", "Symbol", "Array", "Hash"] {
            let id = idx.class_id(name).expect("registered class");
            assert_eq!(idx.class_name_for_id(id), Some(name));
        }
        // Unregistered names have no id.
        assert_eq!(idx.class_id("MyWidget"), None);
    }

    #[test]
    fn class_name_of_resolves_nominal() {
        // A `Type::Nominal { class }` for String resolves back to "String" — the
        // load-bearing behaviour for chained-call result typing.
        let idx = CoreIndex::new();
        let mut i = Interner::new();
        let string_id = idx.class_id("String").unwrap();
        let nominal = i.intern(Type::Nominal {
            class: string_id,
            args: vec![],
        });
        assert_eq!(idx.class_name_of(&i, nominal), Some("String"));
    }

    #[test]
    fn method_return_curated_set() {
        assert_eq!(method_return("String", "downcase"), Some("String"));
        assert_eq!(method_return("String", "length"), Some("Integer"));
        assert_eq!(method_return("String", "empty?"), Some("TrueClass"));
        assert_eq!(method_return("Integer", "to_s"), Some("String"));
        assert_eq!(method_return("Integer", "succ"), Some("Integer"));
        // Unmodeled element-typed return stays None (never guess).
        assert_eq!(method_return("Array", "first"), None);
        assert_eq!(method_return("String", "lenght"), None);
    }

    #[test]
    fn method_arity_curated_set() {
        assert_eq!(method_arity("String", "gsub"), Some((1, Some(2))));
        assert_eq!(method_arity("String", "upcase"), Some((0, Some(0))));
        assert_eq!(method_arity("Array", "push"), Some((0, None)));
        assert_eq!(method_arity("String", "unmodeled"), None);
    }

    #[test]
    fn return_and_arity_methods_count_as_known() {
        // `String#index` is in the return table but not the curated method set;
        // it must still count as a known method (consistency contract).
        let idx = CoreIndex::new();
        assert_eq!(method_return("String", "index"), Some("Integer"));
        assert!(idx.class_has_method("String", "index"));
        // A genuine typo is still unknown.
        assert!(!idx.class_has_method("String", "lenght"));
    }
}
