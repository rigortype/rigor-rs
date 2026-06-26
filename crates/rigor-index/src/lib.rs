//! The index layer (ADR-0004): declaration discovery, ancestor linearization
//! (with visibility), constant/method resolution, built on the `ruby-rbs`
//! parser behind a rigor-rs-owned trait. Rubydex is an optional accelerator.
//!
//! ## Real RBS-backed index
//!
//! [`CoreIndex::new`] parses a curated set of Ruby's **core** RBS signatures
//! (String, Integer, …) and their ancestors (Object, BasicObject, Kernel,
//! Comparable, Numeric, Enumerable) out of the `rbs` gem's `core/*.rbs`, using
//! the `ruby-rbs` crate as a *parser only* (ADR-0004: we own the index, reuse
//! only the parser). From that parse it builds, per class: the instance-method
//! set, each method's resolved **return type** and **arity envelope**, and the
//! **superclass + included modules** — then flattens an **ancestor chain** so
//! method existence is decided over the full linearization (the zero
//! false-positive keystone).
//!
//! The RBS directory is located via `RIGOR_RBS_CORE_DIR`, else a default mise
//! path. When the directory is absent (CI, other machines), the index falls
//! back to a small *hardcoded* core-method stub so unit tests and downstream
//! crates still work without a Ruby install — it never panics.
//!
// TODO(spec): ADR-0007 vendor + embed the RBS at build time (no runtime path /
// no Ruby dependency): pre-parse the stdlib into an embedded `rigor-vendored`
// form so startup is instant and distribution stays single-binary, Ruby-free.
#![allow(dead_code)]

use std::sync::OnceLock;

use rigor_types::{ClassId, Interner, Scalar, Type, TypeId};

mod rbs;

/// The core classes this index registers, in a fixed order. The slice index of
/// a name in this array IS its [`ClassId`] (see [`CoreIndex::class_id`]), so the
/// mapping is stable and reversible (ADR-0019: a `Type::Nominal { class }` can
/// be mapped back to its name).
///
// TODO(spec): the real RBS-backed index assigns ClassIds across the full
// ancestor graph (user classes, modules, generics); this fixed core array is
// the carrier for nominal round-tripping in the current slice (ADR-0004). It is
// the surface the inference engine mints `Nominal { class }` ids against, so it
// only lists the concrete value classes a return type can resolve TO.
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

/// A real, RBS-backed core index. For each loaded class it holds the resolved
/// instance-method table (return class + arity), and the flattened ancestor
/// chain used to decide method existence over the full linearization.
///
/// When the core RBS cannot be located, [`per_class`](CoreIndex::data) is built
/// from a hardcoded stub instead (see [`rbs::CoreData::stub`]), so the public
/// API behaves identically — just over fewer methods.
pub struct CoreIndex {
    data: rbs::CoreData,
}

impl Default for CoreIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl CoreIndex {
    /// Build the index: parse the curated core RBS if available, else fall back
    /// to the hardcoded stub. Never panics.
    pub fn new() -> Self {
        Self {
            data: rbs::CoreData::load(),
        }
    }

    /// Whether `class_name` is one this index models at all. The rule must stay
    /// silent on classes outside the loaded set (ADR-0023: never guess).
    pub fn knows_class(&self, class_name: &str) -> bool {
        self.data.knows_class(class_name)
    }

    /// Whether `class_name` is known to define an instance `method`, walking the
    /// **full flattened ancestor chain** (the class + supers + included
    /// modules). A method counts as present if ANY ancestor defines it.
    ///
    /// Zero-false-positive contract (the keystone of this slice): if the
    /// ancestor chain is not *fully loaded* (some ancestor is missing from the
    /// curated set), this returns `true` — "unknown ⇒ assume present" — so the
    /// undefined-method rule stays silent rather than risk a false positive.
    /// Absence is only ever witnessed (returns `false`) when every ancestor in
    /// the chain is loaded and none defines the method.
    ///
    /// Returns `false` for an entirely unknown class too — callers MUST gate on
    /// [`CoreIndex::knows_class`] first (they do).
    pub fn class_has_method(&self, class_name: &str, method: &str) -> bool {
        self.data.class_has_method(class_name, method)
    }

    /// Whether the class OBJECT `class_name` responds to a singleton (class)
    /// `method` — e.g. `Time.now`, `Array.new`. The singleton surface is the
    /// class's own + inherited `def self.x` methods (up the superclass chain)
    /// UNION the instance methods of `Class`/`Module`/`Object`/`Kernel`/
    /// `BasicObject` (the class object is an instance of `Class`).
    ///
    /// Same zero-false-positive contract as [`CoreIndex::class_has_method`]:
    /// returns `true` ("assume present ⇒ stay silent") unless the FULL singleton
    /// surface is loaded and known to lack the method. An unknown class, an
    /// incomplete superclass chain, or any missing base class ⇒ `true`. Absence
    /// (`false`, witnessable) is only returned when the whole surface is known —
    /// this is what lets the analyzer flag e.g. `Time.current` (an ActiveSupport
    /// extension absent from core `Time`'s singleton surface).
    pub fn class_has_singleton_method(&self, class_name: &str, method: &str) -> bool {
        self.data.class_has_singleton_method(class_name, method)
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
            // index lands; the current slice resolves literals + nominals.
            _ => None,
        }
    }
}

/// Process-global index used by the free [`method_return`] / [`method_arity`]
/// functions, which have no `&self` receiver (their call sites in rigor-infer /
/// rigor-rules pass only `(class, method)`). Built once, lazily, from the same
/// real RBS (or stub) as [`CoreIndex::new`].
fn global() -> &'static rbs::CoreData {
    static GLOBAL: OnceLock<rbs::CoreData> = OnceLock::new();
    GLOBAL.get_or_init(rbs::CoreData::load)
}

/// The RETURN class name of a core method, resolved over the receiver class's
/// flattened ancestor chain (first defining ancestor wins).
///
/// Returns `None` when the return type is unknown / not a concrete class this
/// index models — e.g. a `bool` union (`true | false`), a generic element type,
/// `void`, `self`, or a return to a class outside [`CORE_CLASSES`]. The
/// inference engine treats `None` as "degrade to `Dynamic[top]`" rather than
/// guessing (ADR-0008/0023 zero-FP).
///
/// Used by tier-3-ish dispatch so a CHAINED call types correctly: `s.downcase`
/// -> `"String"` lets the next `.lenght` resolve against `String` and flag it.
pub fn method_return(class: &str, method: &str) -> Option<&'static str> {
    global().method_return(class, method)
}

/// The arity envelope `(min, max)` of a core method, resolved over the receiver
/// class's flattened ancestor chain. `min` is the smallest required-positional
/// count across the method's overloads; `max` is `None` (variadic) when any
/// overload takes a positional rest (`*args`), else the largest
/// required+optional count. Returns `None` when the method's arity is not
/// modeled (method unknown on the loaded chain).
pub fn method_arity(class: &str, method: &str) -> Option<(usize, Option<usize>)> {
    global().method_arity(class, method)
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
        // Real RBS: String#length exists; a typo does not.
        assert!(idx.class_has_method("String", "length"));
        assert!(!idx.class_has_method("String", "lenght"));
    }

    #[test]
    fn aliased_methods_resolve() {
        // `alias size length` in string.rbs: `String#size` must be known and
        // inherit `length`'s return type + arity (no false positive on s.size).
        let idx = CoreIndex::new();
        assert!(idx.class_has_method("String", "size"));
        assert_eq!(method_return("String", "size"), Some("Integer"));
        assert_eq!(method_arity("String", "size"), method_arity("String", "length"));
        // A genuine typo of the alias is still witnessed absent.
        assert!(!idx.class_has_method("String", "sizee"));
    }

    #[test]
    fn stdlib_reopen_methods_resolve() {
        // The reference loads core ⊕ DEFAULT_LIBRARIES, so a stdlib reopen like
        // `class Hash ... def to_json: ... -> String` (json.rbs) is in scope.
        // No false positive on `h.to_json`; the return resolves to String.
        // NOTE: requires the RBS stdlib tree; under the stub fallback these are
        // absent and the class stays conservatively silent (still zero-FP).
        let idx = CoreIndex::new();
        if idx.class_has_method("Hash", "to_json") {
            assert_eq!(method_return("Hash", "to_json"), Some("String"));
            assert!(idx.class_has_method("String", "to_json"));
            assert!(idx.class_has_method("Array", "to_json"));
            // `Object#to_yaml` arrives via the yaml⇒psych manifest dependency.
            assert!(idx.class_has_method("Object", "to_yaml"));
            // A typo of the stdlib method is still witnessed absent.
            assert!(!idx.class_has_method("Hash", "to_jsom"));
        }
    }

    #[test]
    fn inherited_methods_resolve() {
        // The keystone: methods inherited from Kernel/Object must count as
        // present (no false positive on `s.frozen?`, `s.tap`, `s.class`).
        let idx = CoreIndex::new();
        for m in ["frozen?", "tap", "class", "is_a?", "inspect"] {
            assert!(
                idx.class_has_method("String", m),
                "inherited method String#{m} should be known"
            );
        }
        // Integer / Float / Symbol see Kernel/Object too.
        assert!(idx.class_has_method("Integer", "frozen?"));
        assert!(idx.class_has_method("Integer", "to_s"));
    }

    #[test]
    fn typos_on_inherited_chain_are_absent() {
        // A genuine typo is still witnessed absent across the WHOLE chain.
        let idx = CoreIndex::new();
        assert!(!idx.class_has_method("Integer", "upcase"));
        assert!(!idx.class_has_method("NilClass", "upcase"));
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
    fn method_return_resolves_self_and_known_classes() {
        // String#upcase -> String, String#length -> Integer (real RBS).
        assert_eq!(method_return("String", "upcase"), Some("String"));
        assert_eq!(method_return("String", "length"), Some("Integer"));
        assert_eq!(method_return("Integer", "to_s"), Some("String"));
        // A typo has no return.
        assert_eq!(method_return("String", "lenght"), None);
    }

    #[test]
    fn method_arity_envelopes() {
        // String#include? : (string) -> bool  =>  (1, Some(1)).
        assert_eq!(method_arity("String", "include?"), Some((1, Some(1))));
        // String#gsub has overloads with 1 or 2 required positionals => (1, 2).
        assert_eq!(method_arity("String", "gsub"), Some((1, Some(2))));
        // Nullary length => (0, 0).
        assert_eq!(method_arity("String", "length"), Some((0, Some(0))));
        // A typo has no arity.
        assert_eq!(method_arity("String", "unmodeled_xyz"), None);
    }

    #[test]
    fn singleton_methods_resolve() {
        // Class-method (singleton) resolution. Guarded on the real RBS being
        // loaded: when `Time` and the five base classes are present, the
        // singleton surface is complete and absence is witnessable; under the
        // stub fallback the surface is incomplete and everything stays silent
        // (still zero false positive), so we only assert the live behaviour
        // when the real RBS is in scope.
        let idx = CoreIndex::new();
        let real_rbs = idx.knows_class("Time")
            && idx.knows_class("Class")
            && idx.knows_class("Module")
            && idx.knows_class("Object")
            && idx.knows_class("Kernel")
            && idx.knows_class("BasicObject");

        if real_rbs {
            // Real singleton method: `Time.now` exists.
            assert!(
                idx.class_has_singleton_method("Time", "now"),
                "Time.now is a real singleton method"
            );
            // ActiveSupport extension absent from core `Time`'s singleton
            // surface ⇒ witnessable absent (Time < Object, fully core, complete).
            assert!(
                !idx.class_has_singleton_method("Time", "current"),
                "Time.current (AS extension) must be witnessed absent"
            );
            // Inherited from `Module` instance methods on the class object — the
            // critical no-false-positive case.
            assert!(
                idx.class_has_singleton_method("Time", "name"),
                "Time.name (inherited from Module) must be present"
            );
            // Inherited Kernel instance method on the class object.
            assert!(
                idx.class_has_singleton_method("Time", "tap"),
                "Time.tap (inherited from Kernel) must be present"
            );
            // Array.new is a real singleton method; Array.wrap is AS-only.
            assert!(
                idx.class_has_singleton_method("Array", "new"),
                "Array.new is a real singleton method"
            );
            assert!(
                !idx.class_has_singleton_method("Array", "wrap"),
                "Array.wrap (AS extension) must be witnessed absent"
            );
            // An unknown class ⇒ silent (present).
            assert!(idx.class_has_singleton_method("MyWidget", "whatever"));

            // Report the probe results explicitly (visible with --nocapture).
            eprintln!(
                "[singleton probe] Time.now={} Time.current={} Time.name={} \
                 Time.tap={} Array.new={} Array.wrap={}",
                idx.class_has_singleton_method("Time", "now"),
                idx.class_has_singleton_method("Time", "current"),
                idx.class_has_singleton_method("Time", "name"),
                idx.class_has_singleton_method("Time", "tap"),
                idx.class_has_singleton_method("Array", "new"),
                idx.class_has_singleton_method("Array", "wrap"),
            );
        } else {
            // Stub fallback: stay conservative (always present / silent).
            assert!(idx.class_has_singleton_method("Time", "current"));
            assert!(idx.class_has_singleton_method("MyWidget", "whatever"));
        }
    }
}
