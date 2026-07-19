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

pub mod plugins;
mod rbs;

pub use rbs::{ClassOrdering, OverloadSignature, RbsSource, RetainedParamType};

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

    /// Build the index with the named config-gated plugins applied (ADR-25, the
    /// first plugin slice). Each id in `enabled` (from `.rigor.yml`'s `plugins:`)
    /// is resolved to a bundled RBS payload via [`plugins::bundled_plugin`]; an
    /// UNKNOWN / unbundled id is silently ignored (never an error), matching the
    /// reference, which simply can't load a gem it doesn't have. Resolved plugins'
    /// RBS is ingested on top of the core surface (see
    /// [`rbs::CoreData::load_with_plugins`]).
    ///
    /// **Gating:** `with_plugins(&[])` is byte-identical to [`Self::new`], so the
    /// default no-config path stays unchanged. The plugin selectors (and the new
    /// chained witnesses they enable) appear ONLY when a plugin is named in config.
    pub fn with_plugins(enabled: &[String]) -> Self {
        Self::for_project(enabled, &[])
    }

    /// Build the index with config-gated plugins AND the project's own `sig/`
    /// RBS ingested on top (ADR-0033, the ADR-0007 project-signature leg). Each
    /// dir in `sig_dirs` (from `.rigor.yml`'s `signature_paths:`, default
    /// `["sig"]`) is folded through the SAME native parser + reopen-union merge
    /// as core + plugin RBS — no Ruby runtime. A project's own classes thereby
    /// join [`Self::knows_class`], so the dispatch rules witness them exactly as
    /// the reference's `rbs_class_known?` gate does.
    ///
    /// **Gating:** `for_project(enabled, &[])` is byte-identical to
    /// [`Self::with_plugins`], and with no `sig/` on disk the ingestion is inert,
    /// so the default no-config path stays unchanged.
    pub fn for_project(enabled: &[String], sig_dirs: &[std::path::PathBuf]) -> Self {
        let resolved: Vec<&'static plugins::BundledPlugin> = enabled
            .iter()
            .filter_map(|id| plugins::bundled_plugin(id))
            .collect();
        Self {
            data: rbs::CoreData::load_for_project(&resolved, sig_dirs),
        }
    }

    /// Which RBS signature source backs this index (embedded vendored set, the
    /// `RIGOR_RBS_CORE_DIR` override, or the conservative stub). Surfaced by
    /// `rigor doctor` so the standalone-vs-override coverage state is observable
    /// (audit-R1 / ADR-0007).
    pub fn rbs_source(&self) -> &rbs::RbsSource {
        self.data.source()
    }

    /// How many distinct classes the loaded RBS surface registered — a coarse
    /// coverage signal reported by `rigor doctor`.
    pub fn class_count(&self) -> usize {
        self.data.class_count()
    }

    /// Whether `class_name` is one this index models at all. The rule must stay
    /// silent on classes outside the loaded set (ADR-0023: never guess).
    pub fn knows_class(&self, class_name: &str) -> bool {
        self.data.knows_class(class_name)
    }

    /// ADR-0042 Slice 2: whether `qname` (a fully-qualified name like
    /// `"ERB::Util"`) is in the qualified registry — an entry NOT collapsed
    /// onto a shared short key. Used by the typer's `ConstantRead` arm to mint
    /// a `Singleton` for an unambiguous namespaced constant so a class-method
    /// typo on it can be witnessed.
    pub fn knows_qualified_class(&self, qname: &str) -> bool {
        self.data.knows_qualified_class(qname)
    }

    /// Whether `class_name` was INTRODUCED by project-`sig/` ingestion (ADR-0033)
    /// rather than carried by a bundled (core/stdlib/plugin) RBS. The dispatch
    /// rules witness an `X.new` instance-method typo on such a project-authored
    /// class (the reference treats project sig as authoritative) while staying
    /// lenient on a bundled stdlib/gem class (`Pathname.new.typo`). Always
    /// `false` when no `sig/` was ingested, so the default path is unchanged.
    pub fn is_project_sig_class(&self, class_name: &str) -> bool {
        self.data.is_project_sig_class(class_name)
    }

    /// ADR-0042 Slice 4: whether the QUALIFIED name `qname` was introduced by
    /// project `sig/` — the witness gate for a nested project-sig `.new` typo
    /// (`Outer::Inner.new.spni`). See [`rbs::CoreData::is_qualified_project_sig_class`].
    pub fn is_qualified_project_sig_class(&self, qname: &str) -> bool {
        self.data.is_qualified_project_sig_class(qname)
    }

    /// Whether `class_name` was declared at GENUINE top level (empty namespace)
    /// in the loaded RBS — a conservative companion to [`CoreIndex::knows_class`].
    ///
    /// `knows_class` is true for any short name in the index, INCLUDING a name
    /// that only exists because a namespaced/nested decl (`class Process::Status`)
    /// was registered by its short key (`"Status"`). That made a project class
    /// sharing the short name falsely resolve to the namespaced stdlib class and
    /// inherit its (lacking) class-method surface ⇒ false positive. This method
    /// returns `true` ONLY for names with a genuine top-level declaration, so a
    /// caller can refuse to treat an ambiguous short name as a known top-level
    /// class. Instance-method behavior and `knows_class` are unchanged (additive).
    pub fn knows_toplevel_class(&self, class_name: &str) -> bool {
        self.data.knows_toplevel_class(class_name)
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

    /// ADR-0042 Slice 3: instance-method existence over the ISOLATED qualified
    /// entry (project `Status`'s own surface, NOT the short-key merge with
    /// stdlib `Process::Status`). Differs from [`Self::class_has_method`] ONLY
    /// when the name collides with a NESTED same-leaf class (toplevel-vs-toplevel
    /// collisions still merge identically). Used by the source-range project-sig
    /// witness so a shadow class does not silently inherit the stdlib surface.
    pub fn qualified_class_has_method(&self, class_name: &str, method: &str) -> bool {
        self.data.qualified_class_has_method(class_name, method)
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

    /// The subtyping relation of two RBS-known class names — a faithful port of
    /// the reference `Environment#class_ordering`. See
    /// [`rbs::CoreData::class_ordering`]. Read by `call.raise-non-exception`.
    pub fn class_ordering(&self, lhs: &str, rhs: &str) -> ClassOrdering {
        self.data.class_ordering(lhs, rhs)
    }

    /// Whether `class_name` was declared as an RBS `module` (not a `class`) — the
    /// analogue of the reference `Environment#rbs_module?`. See
    /// [`rbs::CoreData::is_module`]. Read by `call.raise-non-exception`.
    pub fn is_module(&self, class_name: &str) -> bool {
        self.data.is_module(class_name)
    }

    /// **Sig-gen only — NOT a diagnostic predicate.** Whether the flattened
    /// ancestor chain of `class` is fully loaded. See
    /// [`rbs::CoreData::chain_complete`].
    pub fn chain_complete(&self, class: &str) -> bool {
        self.data.chain_complete(class)
    }

    /// **Sig-gen only — NOT a diagnostic predicate.** Precise three-valued
    /// declared instance-return lookup (`None` = not declared on a complete
    /// chain; `Some(None)` = declared/incomplete-chain but return unresolvable;
    /// `Some(Some(c))` = declared, returns `c`). See
    /// [`rbs::CoreData::declared_instance_return`].
    pub fn declared_instance_return(&self, class: &str, method: &str) -> Option<Option<&'static str>> {
        self.data.declared_instance_return(class, method)
    }

    /// **Sig-gen only — NOT a diagnostic predicate.** The singleton counterpart
    /// of [`Self::declared_instance_return`]. See
    /// [`rbs::CoreData::declared_singleton_return`].
    pub fn declared_singleton_return(&self, class: &str, method: &str) -> Option<Option<&'static str>> {
        self.data.declared_singleton_return(class, method)
    }

    /// Enumerate every INSTANCE method callable on `class_name` over its ancestor
    /// chain (own + inherited + module + aliases), sorted + deduped. Empty for an
    /// unknown class. Advisory (no completeness gate) — for LSP completion (§12).
    pub fn instance_method_names(&self, class_name: &str) -> Vec<&'static str> {
        self.data.instance_method_names(class_name)
    }

    /// Enumerate every SINGLETON (class-object) method callable on `class_name`
    /// (own/inherited `def self.x`, extended-module instance methods, singleton
    /// aliases, and the `Class`/`Module`/`Object`/`Kernel`/`BasicObject` instance
    /// surface). Sorted + deduped, empty for an unknown class. Advisory — for LSP
    /// completion on a `Singleton` receiver (§12).
    pub fn singleton_method_names(&self, class_name: &str) -> Vec<&'static str> {
        self.data.singleton_method_names(class_name)
    }

    /// The RETURN class name of a core method, resolved over the receiver class's
    /// flattened ancestor chain — the **instance** counterpart of the free
    /// [`method_return`], reading THIS index's data so a config-gated plugin's
    /// reopened return (e.g. `String#squish -> String` from
    /// `activesupport-core-ext`) is visible. The free function reads the
    /// plugin-unaware process-global; the analysis pipeline must use this method
    /// so chained typing on a plugin selector witnesses correctly (ADR-25).
    pub fn method_return(&self, class: &str, method: &str) -> Option<&'static str> {
        self.data.method_return(class, method)
    }

    /// The RETURN class of the CLASS method `class.method` — the singleton
    /// counterpart of [`Self::method_return`], plugin-aware for the same reason.
    /// Diagnostic-grade (all-overloads-agree, see
    /// [`rbs::CoreData::singleton_method_return`]): lets `Date.today` type
    /// `Date` so a chained AS-method typo witnesses (M2-GO slice 4).
    pub fn singleton_method_return(&self, class: &str, method: &str) -> Option<&'static str> {
        self.data.singleton_method_return(class, method)
    }

    /// Whether `class#method`'s author-declared RBS return is `void` (ADR-100;
    /// `static.value-use.void`). First-definer-wins over the ancestor chain.
    pub fn method_return_is_void(&self, class: &str, method: &str) -> bool {
        self.data.method_return_is_void(class, method)
    }

    /// The singleton twin of [`Self::method_return_is_void`].
    pub fn singleton_method_is_void(&self, class: &str, method: &str) -> bool {
        self.data.singleton_method_is_void(class, method)
    }


    /// The RETURN class of a core method **called WITH a block**, over THIS
    /// index's data — the instance counterpart of [`method_return_with_block`],
    /// plugin-aware for the same reason as [`Self::method_return`].
    pub fn method_return_with_block(&self, class: &str, method: &str) -> Option<&'static str> {
        self.data.method_return_with_block(class, method)
    }

    /// The RETURN class of a core method together with whether the RBS return is
    /// nilable (`Optional`, `String?`) — `(class, nilable)`, or `None` when the
    /// return is not a resolvable concrete class. Used ONLY by
    /// `call.possible-nil-receiver` to mint a `C | nil` carrier from a CERTAIN
    /// nilable RBS return on a KNOWN core receiver; no other rule consumes the
    /// nil bit. See [`crate::rbs::CoreData::method_return_nilable`].
    pub fn method_return_nilable(&self, class: &str, method: &str) -> Option<(&'static str, bool)> {
        self.data.method_return_nilable(class, method)
    }

    /// The arity envelope `(min, max)` of a core method over THIS index's data —
    /// the instance counterpart of [`method_arity`], plugin-aware so a config-
    /// gated plugin's reopened method has its arity checked (ADR-25).
    pub fn method_arity(&self, class: &str, method: &str) -> Option<(usize, Option<usize>)> {
        self.data.method_arity(class, method)
    }

    // --- ATM substrate (`call.argument-type-mismatch`, Slice 3) ---------------
    //
    // Thin delegates onto the per-overload argument-compatibility substrate
    // (`rbs::CoreData`, Slices 1-2). The rules crate is the first (and only)
    // consumer; see `check_argument_type_mismatch`.

    /// Per-overload positional shapes of the instance method `class#method`
    /// (per-overload, per-parameter), or `None` if unknown on the ancestor chain.
    pub fn method_overloads(&self, class: &str, method: &str) -> Option<&[OverloadSignature]> {
        self.data.method_overloads(class, method)
    }

    /// Per-overload positional shapes of the CLASS method `class.method`, or
    /// `None` if unknown on the singleton superclass chain.
    pub fn singleton_method_overloads(
        &self,
        class: &str,
        method: &str,
    ) -> Option<&[OverloadSignature]> {
        self.data.singleton_method_overloads(class, method)
    }

    /// Whether an RBS parameter type provably admits a `nil` argument
    /// (conservative-true; only a proven rejection returns `false`). The nil
    /// channel of `call.argument-type-mismatch` fires when this is `false`.
    pub fn param_admits_nil(&self, t: &RetainedParamType) -> bool {
        self.data.param_admits_nil(t)
    }

    /// Whether an RBS parameter type provably accepts a (non-nil) argument of
    /// class `arg_class` (conservative-true; only a proven rejection returns
    /// `false`). Resolves `type` aliases / interfaces — used by the MULTI-overload
    /// non-nil channel (the single-overload channel gates on a faithful param
    /// first, so alias resolution never over-fires there).
    pub fn param_accepts_arg_class(&self, t: &RetainedParamType, arg_class: &str) -> bool {
        self.data.param_accepts_arg_class(t, arg_class)
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
            // Shaped carriers erase to their nominal container for DISPATCH /
            // witnessing (the reference's RBS erasure): a value-pinned `Tuple`
            // dispatches as `Array`, a `HashShape` as `Hash`, an `IntegerRange`
            // as `Integer`. This keeps `[1, 2].frist` witnessing on Array while
            // the carrier's value-pinned DISPLAY (`[1, 2]`) rides through
            // `describe_named`. Any core-modeled Array/Hash/Integer method stays
            // silent, so this only ever witnesses a genuinely-absent method.
            Type::Tuple(_) => Some("Array"),
            Type::HashShape(_) => Some("Hash"),
            Type::IntegerRange { .. } => Some("Integer"),
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

/// The RETURN class name of a core method **called WITH a block**, resolved over
/// the receiver class's flattened ancestor chain — the block-overload return the
/// reference selects (`OverloadSelector` with `block_required: true`). This is
/// what lets a block-bearing call chain type correctly: `arr.map { } -> Array`
/// (so `.frist` on it is witnessed), `h.select { } -> Hash` (so `.keys` is
/// valid and stays silent), `x.tap { } -> x` (the receiver itself).
///
/// Returns `None` when the block form is not precisely modeled — no block
/// overload, or a generic/union/void/nilable/unknown block-overload return. The
/// inference engine treats `None` as "degrade to `Dynamic[top]`" rather than
/// guessing (zero-FP), matching the deferred-then-recovered behavior described
/// in `docs/CURRENT_WORK.md` §4.
pub fn method_return_with_block(class: &str, method: &str) -> Option<&'static str> {
    global().method_return_with_block(class, method)
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
    fn with_plugins_activesupport_core_ext_reopens_core_classes() {
        // ADR-25 — config-gated plugin ingest. With the plugin enabled, the
        // ActiveSupport core-extension selectors join the core surface, and their
        // RBS return types drive chained typing. Guarded on the real RBS being
        // loaded (the plugin reopens core classes that must already exist); under
        // the stub fallback the plugin merge is conservative (still zero-FP).
        let idx = CoreIndex::with_plugins(&["activesupport-core-ext".to_string()]);
        if !idx.knows_class("String") {
            return; // stub fallback — nothing to assert.
        }

        // Methods returning a concrete CORE class ⇒ chained witness enabled.
        assert!(idx.class_has_method("String", "squish"));
        assert_eq!(idx.method_return("String", "squish"), Some("String"));
        assert!(idx.class_has_method("String", "underscore"));
        assert_eq!(idx.method_return("String", "underscore"), Some("String"));
        assert!(idx.class_has_method("Hash", "symbolize_keys"));
        assert_eq!(idx.method_return("Hash", "symbolize_keys"), Some("Hash"));

        // Methods returning `untyped` (Duration) ⇒ known (no direct FP) but no
        // concrete return ⇒ chained call stays Dynamic/silent (matches reference).
        assert!(idx.class_has_method("Integer", "minutes"));
        assert_eq!(idx.method_return("Integer", "minutes"), None);

        // Singleton (class-method) extension: `Time.current` becomes known.
        assert!(idx.class_has_singleton_method("Time", "current"));

        // Gem-name alias resolves identically.
        let by_gem = CoreIndex::with_plugins(&["rigor-activesupport-core-ext".to_string()]);
        assert!(by_gem.class_has_method("String", "squish"));
        assert_eq!(by_gem.method_return("String", "squish"), Some("String"));
        assert!(by_gem.class_has_singleton_method("Time", "current"));
    }

    #[test]
    fn without_plugins_baseline_unchanged() {
        // Gating: `with_plugins(&[])` AND `new()` are byte-identical — the
        // plugin selectors are ABSENT, so the direct calls still witness (the
        // reference, plugin-less, agrees). This is the no-regression keystone.
        let empty = CoreIndex::with_plugins(&[]);
        let plain = CoreIndex::new();
        if !plain.knows_class("String") {
            return; // stub fallback.
        }
        for idx in [&empty, &plain] {
            assert!(!idx.class_has_method("String", "squish"));
            assert_eq!(idx.method_return("String", "squish"), None);
            assert!(!idx.class_has_method("Integer", "minutes"));
            assert!(!idx.class_has_method("Hash", "symbolize_keys"));
            // `Time.current` (AS extension) must be witnessed absent (no plugin).
            assert!(!idx.class_has_singleton_method("Time", "current"));
        }
    }

    #[test]
    fn unknown_plugin_id_is_ignored_not_error() {
        // An unbundled / misspelled plugin id resolves to nothing and is silently
        // dropped — the index is the plain baseline, never an error.
        let idx = CoreIndex::with_plugins(&["not-a-real-plugin".to_string()]);
        if !idx.knows_class("String") {
            return;
        }
        assert!(!idx.class_has_method("String", "squish"));
    }

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
    fn instance_method_names_enumerate_over_ancestors() {
        let idx = CoreIndex::new();
        let m = idx.instance_method_names("String");
        // Own + inherited + alias names are all present.
        assert!(m.contains(&"upcase"), "own String method");
        assert!(m.contains(&"length"), "String method");
        assert!(m.contains(&"tap"), "inherited from Kernel/Object");
        // Sorted + deduped.
        assert!(m.windows(2).all(|w| w[0] <= w[1]), "sorted");
        assert!(idx.instance_method_names("Integer").contains(&"times"));
        // Unknown class ⇒ empty.
        assert!(idx.instance_method_names("MyWidget").is_empty());
    }

    #[test]
    fn singleton_method_names_include_class_methods_and_object_surface() {
        let idx = CoreIndex::new();
        let m = idx.singleton_method_names("Time");
        assert!(m.contains(&"now"), "Time.now class method");
        assert!(m.contains(&"new"), "Time.new via Class instance surface");
        assert!(idx.singleton_method_names("MyWidget").is_empty());
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
    fn block_form_return_resolves_rbs_overload() {
        // The block-overload return types, RBS-derived (the reference's
        // `block_required: true` selection). Guarded on the real RBS tree being
        // loaded — under the stub fallback block returns are unmodeled (None,
        // still zero-FP), so the assertions only run when Enumerable is present.
        let idx = CoreIndex::new();
        if !idx.knows_class("Enumerable") || !idx.class_has_method("Array", "map") {
            return;
        }
        // Enumerable/Array block forms -> Array.
        assert_eq!(method_return_with_block("Array", "map"), Some("Array"));
        assert_eq!(method_return_with_block("Array", "select"), Some("Array"));
        assert_eq!(method_return_with_block("Array", "flat_map"), Some("Array"));
        // Hash block forms: select (alias of filter) -> Hash; reject -> Hash;
        // map -> Array (Enumerable). These are the cases the placeholder regressed.
        assert_eq!(method_return_with_block("Hash", "select"), Some("Hash"));
        assert_eq!(method_return_with_block("Hash", "reject"), Some("Hash"));
        assert_eq!(method_return_with_block("Hash", "map"), Some("Array"));
        // `self`-returning block forms resolve to the RECEIVER's own class.
        assert_eq!(method_return_with_block("Array", "each"), Some("Array"));
        assert_eq!(method_return_with_block("Array", "tap"), Some("Array"));
        assert_eq!(method_return_with_block("String", "tap"), Some("String"));
        assert_eq!(method_return_with_block("Hash", "each"), Some("Hash"));
        // A method with no block overload, or a typo, has no block return.
        assert_eq!(method_return_with_block("String", "lenght"), None);
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
            // Defect 1: `SecureRandom extend Random::Formatter` makes
            // `SecureRandom.hex` a real class method. The old superclass-only
            // walk missed it ⇒ false positive. Now it is PRESENT (true) — either
            // a resolved extended class method, or (if Random::Formatter is not
            // loaded) the surface is conservatively incomplete ⇒ still true.
            // NEVER false.
            if idx.knows_class("SecureRandom") {
                assert!(
                    idx.class_has_singleton_method("SecureRandom", "hex"),
                    "SecureRandom.hex (via extend Random::Formatter) must NOT be \
                     witnessed absent"
                );
            }
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

    #[test]
    fn extended_singleton_methods_resolve() {
        // Defect 1: a class method that comes from `extend M` (M's instance
        // methods folded onto the class object) must NOT be witnessed absent.
        // `SecureRandom` does `extend Random::Formatter`, so `SecureRandom.hex`,
        // `.uuid`, `.alphanumeric` are real class methods. Guarded on real RBS.
        let idx = CoreIndex::new();
        if idx.knows_class("SecureRandom") {
            for m in ["hex", "uuid", "alphanumeric"] {
                assert!(
                    idx.class_has_singleton_method("SecureRandom", m),
                    "SecureRandom.{m} (via extend Random::Formatter) must be present"
                );
            }
            eprintln!(
                "[extend probe] SecureRandom.hex={} SecureRandom.uuid={} \
                 SecureRandom.bogus_xyz={}",
                idx.class_has_singleton_method("SecureRandom", "hex"),
                idx.class_has_singleton_method("SecureRandom", "uuid"),
                idx.class_has_singleton_method("SecureRandom", "bogus_xyz"),
            );
        }
    }

    #[test]
    fn singleton_aliases_resolve() {
        // A class method defined as a SINGLETON alias must not be witnessed
        // absent: `alias self.pwd self.getwd` (Dir), `alias self.fnmatch?
        // self.fnmatch` (File), `alias self.escape self.shellescape` (Shellwords).
        // These were a 23-FP family. Guarded on real RBS.
        let idx = CoreIndex::new();
        if idx.knows_class("Dir") {
            assert!(idx.class_has_singleton_method("Dir", "pwd"), "Dir.pwd is an alias of getwd");
            assert!(idx.class_has_singleton_method("Dir", "getwd"), "Dir.getwd is the alias target");
        }
        if idx.knows_class("File") {
            assert!(idx.class_has_singleton_method("File", "fnmatch?"), "File.fnmatch? aliases fnmatch");
        }
        if idx.knows_class("Shellwords") {
            for m in ["escape", "split", "join"] {
                assert!(
                    idx.class_has_singleton_method("Shellwords", m),
                    "Shellwords.{m} is a singleton alias of shell{m}"
                );
            }
        }
        // Real-corpus FP audit (algorithms `Regexp.compile`): a singleton alias
        // whose TARGET is a base-class method (`alias self.compile self.new`,
        // where `new` is `Class#new`, not an own `def self.new`) must resolve.
        if idx.knows_class("Regexp") {
            assert!(
                idx.class_has_singleton_method("Regexp", "compile"),
                "Regexp.compile aliases self.new (a Class#new base method)"
            );
        }
    }

    #[test]
    fn knows_toplevel_class_distinguishes_namespaced() {
        // Defect 2: a short name that only ever appears NAMESPACED (e.g.
        // `class Process::Status`, registered by short key "Status") must NOT be
        // treated as a known top-level class — otherwise a project model sharing
        // that short name resolves to the stdlib class and inherits its lacking
        // class-method surface ⇒ false positive.
        let idx = CoreIndex::new();
        if idx.knows_class("Time") {
            // Genuine top-level core classes.
            assert!(idx.knows_toplevel_class("Time"));
            assert!(idx.knows_toplevel_class("Array"));
            // `Status` is in the index (Process::Status, by short key) but is
            // NOT top-level — verified by inspection of the loaded set.
            assert!(
                idx.knows_class("Status"),
                "Status is registered (Process::Status by short key)"
            );
            assert!(
                !idx.knows_toplevel_class("Status"),
                "Process::Status is namespaced-only ⇒ not a top-level class"
            );
            // A name absent from the index entirely is not top-level either.
            assert!(!idx.knows_toplevel_class("MyWidget"));
            eprintln!(
                "[toplevel probe] Time={} Array={} Status(knows)={} \
                 Status(toplevel)={}",
                idx.knows_toplevel_class("Time"),
                idx.knows_toplevel_class("Array"),
                idx.knows_class("Status"),
                idx.knows_toplevel_class("Status"),
            );
        }
    }

    #[test]
    fn class_ordering_and_is_module_over_real_rbs() {
        let idx = CoreIndex::new();
        if !idx.knows_class("Exception") {
            return; // stub fallback — Exception/String absent.
        }
        use ClassOrdering::*;
        // Reflexive equality.
        assert_eq!(idx.class_ordering("String", "String"), Equal);
        // Exception descendants order as subclass.
        assert_eq!(idx.class_ordering("ArgumentError", "Exception"), Subclass);
        assert_eq!(idx.class_ordering("RuntimeError", "Exception"), Subclass);
        // Unrelated fully-known classes are disjoint.
        assert_eq!(idx.class_ordering("Integer", "Exception"), Disjoint);
        assert_eq!(idx.class_ordering("Symbol", "String"), Disjoint);
        // An unloaded class is unknown.
        assert_eq!(idx.class_ordering("NotAClass_xyz", "Exception"), Unknown);
        // The `::` prefix is stripped before comparison.
        assert_eq!(idx.class_ordering("::String", "String"), Equal);
        // Module bit: modules true, classes false.
        assert!(idx.is_module("Comparable"));
        assert!(idx.is_module("Kernel"));
        assert!(!idx.is_module("Class"));
        assert!(!idx.is_module("Object"));
        assert!(!idx.is_module("Exception"));
        assert!(!idx.is_module("NotAClass_xyz"));
    }

    #[test]
    fn knows_toplevel_class_under_stub() {
        // Under the stub fallback (no RBS dir), a curated class is top-level and
        // an unknown name is not. We can only assert this branch when running on
        // the stub; when the real RBS is live the curated names are top-level too
        // (a superset), so the assertions below hold in BOTH cases for these
        // genuine top-level names.
        let idx = CoreIndex::new();
        assert!(idx.knows_toplevel_class("Array"));
        assert!(!idx.knows_toplevel_class("DefinitelyNotAClass_xyz"));
    }
}


#[cfg(test)]
mod singleton_return_tests {
    use super::*;

    /// M2-GO slice 4: diagnostic-grade singleton returns. Concrete unanimous
    /// returns resolve; divergent-overload methods (`Regexp.last_match`:
    /// `MatchData?` vs `String?`) decline by the all-overloads-agree collapse.
    #[test]
    fn singleton_method_return_unanimous_and_divergent() {
        let idx = CoreIndex::new();
        assert_eq!(idx.singleton_method_return("Time", "now"), Some("Time"));
        assert_eq!(idx.singleton_method_return("Time", "at"), Some("Time"));
        assert_eq!(idx.singleton_method_return("Date", "today"), Some("Date"));
        // Divergent overload returns MUST decline (the FP keystone).
        assert_eq!(idx.singleton_method_return("Regexp", "last_match"), None);
        // Unknown method / class decline.
        assert_eq!(idx.singleton_method_return("Time", "no_such"), None);
        assert_eq!(idx.singleton_method_return("NoSuchClass", "now"), None);
    }
}

#[cfg(test)]
mod singleton_alias_return_tests {
    use super::*;

    /// A singleton ALIAS resolves through its target with instance-binding
    /// preserved (`alias self.pwd self.getwd` → `Dir.pwd -> String`).
    #[test]
    fn singleton_alias_resolves_to_target_return() {
        let idx = CoreIndex::new();
        assert_eq!(idx.singleton_method_return("Dir", "pwd"), Some("String"));
        assert_eq!(idx.singleton_method_return("Dir", "getwd"), Some("String"));
    }
}

#[cfg(test)]
mod void_return_tests {
    use super::*;

    /// ADR-100: `-> void` returns are tracked (project sigs are the main
    /// source; core RBS also declares them, e.g. `Comparable#clamp`-adjacent
    /// void writers). A non-void method reads false; unknown reads false.
    #[test]
    fn void_flags_resolve_over_project_sigs() {
        let dir = std::env::temp_dir().join("rigor_void_sig_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("w.rbs"),
            "class Widget\n  def fire: () -> void\n  def spin: () -> Integer\n  def self.reset: () -> void\nend\n",
        )
        .unwrap();
        let idx = CoreIndex::for_project(&[], std::slice::from_ref(&dir));
        assert!(idx.method_return_is_void("Widget", "fire"));
        assert!(!idx.method_return_is_void("Widget", "spin"));
        assert!(idx.singleton_method_is_void("Widget", "reset"));
        assert!(!idx.singleton_method_is_void("Widget", "fire"));
        assert!(!idx.method_return_is_void("NoSuch", "fire"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
