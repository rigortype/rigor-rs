//! Real RBS-backed core data: parse the Ruby `core/*.rbs` set with the
//! `ruby-rbs` crate (parser only — ADR-0004), extract per-class method tables
//! (return class + arity envelope) and the super/include graph, then flatten an
//! ancestor chain so method existence is decided over the full linearization.
//!
//! The signature set is **vendored and embedded at build time** (ADR-0007): the
//! whole `core/` ⊕ the `DEFAULT_LIBRARIES` stdlib closure is copied under
//! `vendor/rbs/`, `build.rs` emits `$OUT_DIR/embedded_rbs.rs` (the
//! [`EMBEDDED_RBS`] `(path, contents)` table), and [`CoreData::load`] ingests
//! those bytes by default — no runtime filesystem dependency on a local rbs gem.
//! `RIGOR_RBS_CORE_DIR` remains an override seam (ADR-0007 / audit-R2): when set,
//! the loader reads from that directory at runtime exactly as before, for
//! out-of-band stdlib-RBS refreshes.
//!
//! Falls back to a hardcoded stub only in the degenerate case (embedded set
//! empty / override dir absent or unparsable), so the crate never panics.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use ruby_rbs::node::{
    parse, AliasKind, AttributeKind, ClassNode, InterfaceNode, MethodDefinitionKind,
    MethodDefinitionVisibility, ModuleNode, Node, SignatureNode, TypeAliasNode,
};

// The build-time-embedded RBS signature set: `EMBEDDED_RBS: &[(&str, &str)]`,
// one `(relative-path, file-contents)` entry per vendored `.rbs`, in
// deterministic sorted-by-path order (see `build.rs`). The ingest is
// order-independent for class membership, so any total order yields the same
// index; sorted is chosen for reproducibility.
include!(concat!(env!("OUT_DIR"), "/embedded_rbs.rs"));

// `%a{rigor:v1:conforms-to _Interface}` (issue #129, ADR-0044).
mod conformance;
pub use conformance::{
    parse_conforms_to, ConformanceFinding, ConformanceKind, RBS_EXTENDED_UNRESOLVED,
    UNSATISFIED_CONFORMANCE,
};

/// The stdlib libraries loaded on top of `core/` — the reference's
/// `DEFAULT_LIBRARIES` (`Rigor::Environment::DEFAULT_LIBRARIES`). Each name maps
/// to `<RBS_ROOT>/stdlib/<lib>/0/*.rbs`. A lib whose dir is absent is skipped
/// silently (e.g. `prism` / `rbs` ship RBS with their own gems, not in the rbs
/// stdlib tree). Loading these matches the reference's default RBS universe so a
/// stdlib reopen like `class Hash ... def to_json` is in scope (no false
/// `call.undefined-method` on `h.to_json`).
const DEFAULT_LIBRARIES: &[&str] = &[
    "pathname", "optparse", "json", "yaml", "fileutils", "tempfile", "tmpdir",
    "stringio", "forwardable", "digest", "securerandom",
    "uri", "logger", "date",
    "pp", "delegate", "observable", "abbrev", "find", "tsort", "singleton",
    "shellwords", "benchmark", "base64", "did_you_mean",
    "monitor", "mutex_m", "timeout",
    "open3", "erb", "etc", "ipaddr", "bigdecimal", "bigdecimal-math",
    "prettyprint", "random-formatter", "time", "open-uri", "resolv",
    "csv", "pstore", "objspace", "io-console", "cgi", "cgi-escape",
    "strscan",
    "prism", "rbs",
];

/// Classes the REFERENCE loads a declaration for but whose DEFINITION it cannot
/// build **as it loads in this project's dev environment** —
/// `RBS::DefinitionBuilder#build_instance` / `#build_singleton` raise, so
/// `RbsLoader#instance_definition` / `#singleton_definition` fail-soft to `nil`
/// and the dispatcher degrades EVERY call on them to `Dynamic[Top]`. The oracle
/// is therefore silent on any method of these classes — real or misspelled — and
/// witnessing absence over rigor-rs's (buildable) copy of the same signatures is
/// a false positive by ADR-0002. Measured, not theorised: `BigMath.frobnicate(1)`
/// and `Bundler.frobnicate` both fired here while the pinned oracle stayed
/// silent (`docs/notes/20260731-bigmath-ingestion-asymmetry.md`).
///
/// **EMPTY as of the `v0.3.2` pin (2026-08-09) — and that is the mechanism
/// working, not the mechanism dying.** Upstream `v0.3.2` fixed every collision
/// this table mirrored: #299/#300/#301 rewrote
/// `data/vendored_gem_sigs/{bundler,rubygems}/` to stop re-declaring anything the
/// `rbs` gem's `sig/shims/{bundler,rubygems}.rbs` already declares, added
/// `data/vendored_gem_sigs/racc/` so `Nokogiri::CSS::Parser`'s declared
/// superclass resolves, repaired the `Gem::SourceList` dangling reference, and
/// dropped the duplicate `BigMath` declaration. `harness/unbuildable_classes.rb`
/// now reports **0 classes** against the pin, on a host that HAS the `bigdecimal`
/// gem installed (4.1.2) — i.e. the `BigMath` entry went away for a pin reason,
/// not an environment one. Twelve entries → zero; rigor-rs resumes witnessing
/// `Bundler*` / `Gem::*` / `BigMath` / `Nokogiri::CSS::Parser` receivers.
///
/// **This set is keyed to (pin × rbs version × THE HOST'S INSTALLED GEMS), not
/// to the pin alone**, because `RBS::EnvironmentLoader#add(library:)` prefers an
/// installed gem's own `sig/` over `rbs`'s `stdlib/<lib>/` copy — so *which*
/// signature files are in the room depends on what is installed. Measured A/B on
/// the `v0.3.1` pin (see the note): with the `bigdecimal` gem absent from
/// `GEM_PATH`, that reference BUILT `BigMath` and the set shrank from 12 to 11.
/// The hazard survives the table going empty: a future re-population is only
/// meaningful when derived where the gates run.
/// `harness/unbuildable_classes.rb` tags each entry's sources `[env]` / `[pin]`
/// and MUST be run where the gates run.
///
/// The failures it mirrored were all SOURCE COLLISIONS inside the reference's own
/// load set, every one involving a signature source rigor-rs deliberately does
/// NOT vendor (the `bigdecimal` gem's `sig/`, the `rbs` gem's `sig/shims/`),
/// which is why rigor-rs's index built cleanly instead and had to be blinded by
/// hand. The historical twelve and their individual causes are recorded in
/// `docs/notes/20260731-bigmath-ingestion-asymmetry.md` and
/// `docs/notes/20260809-repin-v032.md`.
///
/// **Why a table and not a derivation.** rigor-rs cannot compute this set from
/// its own tree: the colliding declaration is precisely the one it does not
/// carry. Mirroring the reference's real load set instead (vendoring the
/// `bigdecimal` and `rbs` gems' `sig/`) would drag those gems' whole class
/// surface into `knows_class` — the failure mode `PROVENANCE.md` records for the
/// `prism` supplement (8 fresh FPs). So the set is DATA, on the same footing as
/// the vendored signatures themselves, and it is regenerated from the pinned
/// oracle by `harness/unbuildable_classes.rb` (`--check` verifies this list
/// against the reference and is part of the pin-bump ritual — run it where the
/// gates run, per the environment caveat above). If upstream fixes a collision,
/// regeneration drops the entry and rigor-rs starts witnessing again — the table
/// converges rather than freezing a gap.
///
/// Names are FULLY QUALIFIED. A top-level name is applied to both the short-key
/// `classes` map and the qualified registry; a NAMESPACED name is applied to the
/// qualified registry ONLY, because the short map deliberately collapses
/// `Bundler::Definition` onto the shared leaf `"Definition"` and blanking that
/// would silence unrelated classes.
///
/// Each entry is `(name, instance_fails, singleton_fails)`. The reference builds
/// the two definitions independently and they fail independently — `Bundler` and
/// `Gem::Requirement` build their INSTANCE definition cleanly and only raise on
/// the singleton — so the two sides are tracked apart. Collapsing them into one
/// flag would still be FP-safe (more silence, never more noise) but would drop
/// instance-method witnessing the oracle actually performs.
const UNBUILDABLE_DEFINITIONS: &[(&str, bool, bool)] = &[];

/// Apply [`UNBUILDABLE_DEFINITIONS`] to a finished index: empty the affected
/// entries' method tables (so no return type, arity or overload resolves — the
/// reference's `definition == nil`) and flag them so the existence gates answer
/// "assume present ⇒ stay silent" instead of witnessing absence over an empty
/// surface. `knows_class` / `knows_toplevel_class` are deliberately UNTOUCHED:
/// the reference's `class_known?` reads `class_decls`, which the failed build
/// does not remove, so `BigMath` still resolves as a constant there and must here
/// (dropping the class instead would trade this FP for a
/// `call.unresolved-toplevel` one).
///
/// A name absent from the index is skipped silently: the set is pinned to the
/// reference, and a `sig_dirs` / plugin-free load or a future vendoring change
/// may simply not carry one of these classes.
fn mark_unbuildable_definitions(
    classes: &mut HashMap<&'static str, ClassEntry>,
    qualified: &mut HashMap<&'static str, ClassEntry>,
) {
    fn blank(entry: &mut ClassEntry, instance: bool, singleton: bool) {
        if instance {
            entry.instance_unbuildable = true;
            entry.methods.clear();
            entry.tuple_returns.clear();
            entry.method_overloads.clear();
            entry.overloading_method_overloads.clear();
            entry.block_returns.clear();
            entry.block_free_returns.clear();
            entry.void_methods.clear();
            entry.aliases.clear();
        }
        if singleton {
            entry.singleton_unbuildable = true;
            entry.singleton_methods.clear();
            entry.singleton_tuple_returns.clear();
            entry.singleton_method_overloads.clear();
            entry.overloading_singleton_overloads.clear();
            entry.singleton_block_free_returns.clear();
            entry.void_singleton_methods.clear();
            entry.singleton_aliases.clear();
        }
    }
    for &(name, instance, singleton) in UNBUILDABLE_DEFINITIONS {
        if let Some(entry) = qualified.get_mut(name) {
            blank(entry, instance, singleton);
        }
        // Short-key map: only for a genuinely top-level name (see the constant's
        // doc — a nested name's short key is a merge of unrelated classes).
        if !name.contains("::") {
            if let Some(entry) = classes.get_mut(name) {
                blank(entry, instance, singleton);
            }
        }
    }
}

/// The original runtime core-RBS directory (rbs-4.0.3 gem under mise). ADR-0007
/// replaced this default with the vendored, build-time-[`EMBEDDED_RBS`] set, so
/// it is no longer on the default load path — kept only as documentation of the
/// source the vendored tree was generated from. `RIGOR_RBS_CORE_DIR` (any dir)
/// is the live override seam; this constant is not read at runtime.
#[allow(dead_code)]
const DEFAULT_CORE_DIR: &str = "/Users/megurine/.local/share/mise/installs/ruby/4.0.5/\
lib/ruby/gems/4.0.0/gems/rbs-4.0.3/core";

/// An arity envelope `(min, max)`: `min` is the smallest required-positional
/// count across overloads; `max` is `None` (variadic) when any overload takes a
/// positional rest, else the largest required+optional count.
type Arity = (usize, Option<usize>);

/// An arity envelope that may be UNMODELLED — the reference's `arity_eligible?`
/// gate (`analysis/check_rules.rb`), ported.
///
/// `compute_arity_envelope` there returns nil — no arity check at all, in either
/// direction — as soon as ANY overload of the method declares a required keyword
/// or a trailing positional, or is an `UntypedFunction` (`(?) -> untyped`, which
/// exposes no arity accessors). The stated reason is that the rule's own
/// plain-positional pre-check cannot see a required keyword the caller must
/// pass, so the positional envelope alone is not a safe thing to fire on.
///
/// This is not a corner: 507 of the 11,115 methods in the oracle's configless
/// universe are ineligible, and four of them are `Kernel`'s numeric conversion
/// functions (`Integer` / `Float` / `Rational` / `Complex`, each carrying an
/// `(…, exception: bool) -> …` overload). rigor-rs used to compute the
/// positional envelope for those regardless and fired
/// `call.wrong-arity` where the oracle is silent — a measured ADR-0002 false
/// positive. Their keyword-free siblings (`Array` / `Hash` / `String`) stay
/// eligible and keep firing, which is what makes the gate per-method rather than
/// a retreat from the family.
type ArityEnvelope = Option<Arity>;

/// The subtyping relation of two class names, mirroring the reference's
/// `class_ordering` result atoms (`:equal` / `:subclass` / `:superclass` /
/// `:disjoint` / `:unknown`). See [`CoreData::class_ordering`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassOrdering {
    /// The two names denote the same class.
    Equal,
    /// `lhs` is a proper descendant of `rhs` (its ancestry includes `rhs`).
    Subclass,
    /// `lhs` is a proper ancestor of `rhs` (`rhs`'s ancestry includes `lhs`).
    Superclass,
    /// Neither is an ancestor of the other (both chains fully known).
    Disjoint,
    /// The relation cannot be proven (an unloaded class or an incomplete chain).
    Unknown,
}

/// A one-level structural tag for a single RBS parameter type — the ATM
/// shared-substrate leaf (Slice 1, retention only). Each variant keeps just
/// enough shape for a later argument-compatibility walk (Slice 2) and message
/// labels (Slice 3) WITHOUT retaining the full type AST: the named kinds keep
/// their type arguments (`Range[int]` ⇒ `ClassInstance("Range", [Alias("int",
/// [])])`) so the witness label can reproduce the reference's
/// `param.type.to_s` generic rendering (`Range[::int]` — issue #304), while the
/// genuinely-structural wrappers — `Union`, `Optional`, `Tuple` — recurse into
/// their members. Everything else collapses to [`Other`](Self::Other), whose
/// `String` is the exact WRITTEN form of the type (sliced from the RBS source)
/// so a diagnostic can quote it verbatim later. The acceptance walks consult
/// only the head name (a concrete argument class cannot satisfy or refute on
/// the args), so retained args are label/data only.
///
/// The interned names ride `&'static str` (the file-wide interning discipline);
/// the `Other` leaf is an owned `String` because its vocabulary is unbounded.
/// This type is RETAINED but read by NO consumer in Slice 1 — the accessors
/// ([`CoreData::method_overloads`], [`CoreData::resolve_type_alias`],
/// [`CoreData::interface_methods`]) exist and are unit-tested, but nothing wires
/// them into a rule yet (the slice is output-inert by contract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetainedParamType {
    /// A concrete class instance: `Integer`, `String`,
    /// `Array[Integer]` ⇒ `ClassInstance("Array", [ClassInstance("Integer", [])])`.
    ClassInstance(&'static str, Vec<RetainedParamType>),
    /// A reference to a `type` alias (RBS lowercase alias, e.g. the `string` in
    /// `(string) -> void`; a generic alias keeps its args: `range[int]` ⇒
    /// `Alias("range", [Alias("int", [])])`). NOT expanded here — Slice 2 owns
    /// bounded expansion via [`CoreData::resolve_type_alias`].
    Alias(&'static str, Vec<RetainedParamType>),
    /// A reference to an `interface` (RBS `_`-prefixed, e.g. `_ToStr`; a generic
    /// interface keeps its args: `_Each[E]`). The required method-name set is
    /// retained separately in [`CoreData::interface_methods`].
    Interface(&'static str, Vec<RetainedParamType>),
    /// A bare type variable (`E`, `I`). Method-level BOUNDS are not folded at
    /// retention: the reference substitutes them only in the multi-overload
    /// channel (`resolve_param_bounds`, `check_rules.rb`), so consumers
    /// substitute via [`RetainedParamType::substitute_vars`] against
    /// [`OverloadSignature::type_param_bounds`].
    Variable(&'static str),
    /// A union `A | B | ...` — each member retained one level deep.
    Union(Vec<RetainedParamType>),
    /// An optional `T?` — the inner type retained one level deep.
    Optional(Box<RetainedParamType>),
    /// A fixed tuple `[A, B]` — element-wise so a generic argument such as
    /// `Hash[[K, V], Integer]` renders faithfully; semantically conservative
    /// like `Other` (the acceptance walks admit it).
    Tuple(Vec<RetainedParamType>),
    /// Any other type shape (base types `bool`/`nil`/`untyped`/`void`/`self`,
    /// literals, records, procs, singletons, intersections, …). The `String`
    /// is the verbatim written form sliced from the RBS source.
    Other(String),
}

impl RetainedParamType {
    /// Substitute the method-level bounded type parameters — the reference's
    /// `resolve_param_bounds` (`check_rules.rb`), which the reference applies
    /// ONLY when collecting the multi-overload parameter set (the
    /// single-overload channel walks the raw `param.type`, where a bare
    /// variable admits `nil` and translates to `untyped` — so it never fires).
    /// Recurses through every retained wrapper and the named kinds' type
    /// arguments; an `Other` leaf is opaque and carried verbatim.
    pub fn substitute_vars(&self, subst: &[(&'static str, RetainedParamType)]) -> RetainedParamType {
        match self {
            RetainedParamType::Variable(name) => subst
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, bound)| bound.clone())
                .unwrap_or_else(|| self.clone()),
            RetainedParamType::ClassInstance(name, args)
            | RetainedParamType::Alias(name, args)
            | RetainedParamType::Interface(name, args) => {
                let args = args.iter().map(|a| a.substitute_vars(subst)).collect();
                match self {
                    RetainedParamType::ClassInstance(..) => {
                        RetainedParamType::ClassInstance(name, args)
                    }
                    RetainedParamType::Alias(..) => RetainedParamType::Alias(name, args),
                    _ => RetainedParamType::Interface(name, args),
                }
            }
            RetainedParamType::Union(members) | RetainedParamType::Tuple(members) => {
                let members = members
                    .iter()
                    .map(|m| m.substitute_vars(subst))
                    .collect();
                match self {
                    RetainedParamType::Union(_) => RetainedParamType::Union(members),
                    _ => RetainedParamType::Tuple(members),
                }
            }
            RetainedParamType::Optional(inner) => {
                RetainedParamType::Optional(Box::new(inner.substitute_vars(subst)))
            }
            RetainedParamType::Other(_) => self.clone(),
        }
    }
}

/// A structured RBS **return** descriptor — the richer carrier the flat
/// `Option<&'static str>` return path structurally cannot hold.
///
/// [`method_signature`]'s return slot is a single class name, so a `-> [ Integer,
/// Process::Status ]` collapses to `None` (⇒ `Dynamic[top]`) and the per-position
/// precision is lost. This descriptor is the rigor-rs analogue of the reference's
/// `RbsTypeTranslator` (`rbs_type_translator.rb`), restricted to the two shapes a
/// rigor-rs `Type` can carry losslessly today:
///
/// | RBS | reference | here |
/// | --- | --- | --- |
/// | `ClassInstance` (`String`, `Array[X]`, `Process::Status`) | `nominal_of` | [`Class`](Self::Class) |
/// | `Tuple` (`[A, B]`) | `tuple_of` | [`Tuple`](Self::Tuple) (recursive) |
/// | everything else (union / optional / literal / interface / variable / `untyped` / `void` / …) | its precise carrier | [`Unknown`](Self::Unknown) |
///
/// [`Unknown`](Self::Unknown) is deliberately the reference's `untyped`
/// (`Dynamic[top]`) degrade rather than a decline: the reference's translator is
/// total, so an element it models more precisely than we can (`String?` ⇒
/// `String | nil`) must not delete the SIBLING elements' precision. A
/// `Dynamic[top]` slot is silent in every rule, so the deviation only ever loses
/// recall (never a false positive).
///
/// A [`Class`](Self::Class) name is the name as WRITTEN in the RBS reference —
/// namespace path plus leaf (`Process::Status`), matching the key shape of the
/// ADR-0042 qualified registry — with type arguments dropped (`Array[Integer]` ⇒
/// `Class("Array")`), the same one-level discipline [`RetainedParamType`] and the
/// flat return path already use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RbsReturnShape {
    /// A concrete class instance, type arguments dropped, named as written
    /// (`Integer`, `Process::Status`).
    Class(&'static str),
    /// An RBS tuple `[A, B]`, elements in declaration order.
    Tuple(Vec<RbsReturnShape>),
    /// A shape this descriptor does not model — the `Dynamic[top]` degrade.
    Unknown,
}

impl RbsReturnShape {
    /// Every [`Class`](Self::Class) name reachable in this shape (including
    /// nested tuples), appended to `out`. Feeds the id-registration sweep that
    /// gives an RBS-only element class (`Process::Status`) a registry identity.
    fn collect_class_names(&self, out: &mut Vec<&'static str>) {
        match self {
            RbsReturnShape::Class(name) => out.push(name),
            RbsReturnShape::Tuple(elems) => {
                for e in elems {
                    e.collect_class_names(out);
                }
            }
            RbsReturnShape::Unknown => {}
        }
    }
}

/// One RBS overload's positional-parameter shape, retained per-overload (NOT
/// merged into the arity envelope). The ATM substrate (Slice 1) keeps every
/// overload separately — `Integer#+` has four, one per numeric operand type —
/// where the existing [`Arity`] path collapses them to a single `(min, max)`
/// envelope. Required and optional positionals carry their one-level
/// [`RetainedParamType`] tag; the remaining shapes are kept as presence flags
/// only (a later argument check disqualifies an overload that has any of them
/// rather than reasoning about them precisely).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverloadSignature {
    /// The required positional parameters, in order, one-level-tagged.
    pub required_positionals: Vec<RetainedParamType>,
    /// The optional positional parameters (`?T`), in order, one-level-tagged.
    pub optional_positionals: Vec<RetainedParamType>,
    /// The declared NAME of each required positional (`str` in `(String str)`),
    /// parallel to `required_positionals`; `None` for an unnamed param. Consumed
    /// by `call.argument-type-mismatch`'s single-overload message (the
    /// reference's ``parameter `str' of `` prefix, `check_rules.rb:2324`).
    pub required_positional_names: Vec<Option<&'static str>>,
    /// The declared name of each optional positional, parallel to
    /// `optional_positionals`; `None` for an unnamed param.
    pub optional_positional_names: Vec<Option<&'static str>>,
    /// `true` iff the overload declares a rest positional (`*T`).
    pub has_rest_positionals: bool,
    /// `true` iff the overload declares any required keyword.
    pub has_required_keywords: bool,
    /// `true` iff the overload declares any optional keyword.
    pub has_optional_keywords: bool,
    /// `true` iff the overload declares a rest keyword (`**T`).
    pub has_rest_keywords: bool,
    /// `true` iff the overload declares any trailing positional (a positional
    /// after a rest, e.g. `(*T, U)`).
    pub has_trailing_positionals: bool,
    /// `true` iff the overload declares a block the caller MUST supply
    /// (`{ … }`, not `?{ … }`) — the reference's
    /// `OverloadSelector.overload_requires_block?`. A block-less call site never
    /// engages such an overload.
    pub block_required: bool,
    /// The method type's BOUNDED type parameters — `[I < _ToInt, T] (I index)`
    /// records `("I", _ToInt)` here and leaves the positional params' `I` as a
    /// raw [`RetainedParamType::Variable`]. The reference substitutes the bound
    /// only in the MULTI-overload channel (`resolve_param_bounds`,
    /// `check_rules.rb`), so consumers apply [`RetainedParamType::substitute_vars`]
    /// at collection time; the single-overload channel sees the raw variable —
    /// which admits `nil` and is never a faithful param — exactly as the
    /// reference declines there.
    pub type_param_bounds: Vec<(&'static str, RetainedParamType)>,
    /// The overload's RETURN type in its VERBATIM written form, whitespace-
    /// normalised (`Array[[ E, X ]]` ⇒ `"Array[[ E, X ]]"`). Two overloads
    /// answer the *same* return iff these strings are equal.
    ///
    /// The flat [`CoreData::method_return`] slot compares only the ERASED head
    /// class, so `Array[[E, X]]` and `Array[Array[E | U]]` "agree" there while
    /// the reference translates them to two distinct types and joins them into
    /// `Dynamic[union]` (upstream #521). This field is what lets a consumer see
    /// that difference. When the source slice is unavailable the builder stores
    /// a per-overload sentinel (`"\0unresolved<i>"`) rather than an empty
    /// string, so an unreadable return never *agrees* with anything — the
    /// conservative direction.
    pub return_form: String,
}

/// Sentinel for a return type that is only knowable at the CALL SITE: a block
/// overload declaring `self` (`Array#each { } -> self`, `Kernel#tap { } ->
/// self`, stored in [`ClassEntry::block_returns`]) or an instance method
/// declaring `instance` (`Hash#compact: () -> instance` since rbs 4.1, stored in
/// [`ClassEntry::methods`]). At lookup time it resolves to the RECEIVER's own
/// class name — the value the accessor was queried with — so `x.tap { } : x`,
/// `arr.each { } : arr` and `h.compact : h`'s class. Resolving to the RECEIVER
/// rather than the declaring class is what makes it right under inheritance:
/// `my_hash.compact` is a `MyHash`, not a `Hash`. A distinct value (not a real
/// class name) so it can never collide with an actual `ClassInstanceType`
/// return.
const SELF_RETURN: &str = "\0self";

/// Drop a [`SELF_RETURN`] sentinel to `None` (⇒ Dynamic) for a lookup whose
/// receiver the flat return slot cannot name — the singleton paths that read an
/// INSTANCE method's return through an `extend` or a base class. Declining is
/// always sound; the sentinel must never escape as a class name.
fn drop_call_site_return(ret: Option<&'static str>) -> Option<&'static str> {
    ret.filter(|r| *r != SELF_RETURN)
}

/// The closed set of class names whose instance type admits a `nil` argument —
/// a faithful copy of the reference `NIL_COMPATIBLE_CLASS_NAMES`
/// (`check_rules.rb:2053`). A `ClassInstance` param admits nil iff its
/// (namespace-stripped) name is one of these: `NilClass` itself, and the three
/// universal ancestors nil is an instance of. Every other concrete class
/// (`String`, `Integer`, …) rejects nil. Consumed by [`CoreData::param_admits_nil`].
const NIL_COMPATIBLE_CLASS_NAMES: [&str; 4] = ["NilClass", "Object", "BasicObject", "Kernel"];

/// One stored method definition as the per-class tables carry it:
/// `(return class name if resolvable, arity envelope, nilable)` — see the
/// `ClassEntry::methods` docs for the slot semantics.
type StoredMethodDef = (Option<&'static str>, ArityEnvelope, bool);

/// Per-class data extracted from RBS: its instance methods (name -> resolved
/// return class + arity), its direct superclass, and its included modules.
#[derive(Default, Clone)]
struct ClassEntry {
    /// `method name -> (return class name if resolvable, arity envelope,
    /// nilable)`. `nilable` is `true` iff the RBS return is an `Optional`
    /// (`String?`) over a resolvable `ClassInstanceType` — i.e. the method
    /// yields `C | nil`. It is `false` for a plain non-optional return, and
    /// is meaningful ONLY when the return class name is `Some` (a `None`
    /// return collapses to Dynamic and carries no nilability). Consumed solely
    /// by `call.possible-nil-receiver` via [`CoreData::method_return_nilable`];
    /// no existing rule reads it (return-class / arity stay as before).
    methods: HashMap<&'static str, (Option<&'static str>, ArityEnvelope, bool)>,
    /// `method name -> TUPLE return element shapes`, populated ONLY for instance
    /// methods whose RBS return is a tuple (`String#partition: (...) -> [String,
    /// String, String]`). ADDITIVE beside `methods`, whose flat return slot
    /// collapses a tuple to `None`: the entries here are a strict extension of
    /// what that path drops, so every existing consumer is unchanged. First write
    /// wins on reopen, mirroring `methods`. See [`tuple_return`].
    tuple_returns: HashMap<&'static str, Vec<RbsReturnShape>>,
    /// The singleton twin of `tuple_returns` (`Process.wait2: (...) -> [Integer,
    /// Process::Status]`).
    singleton_tuple_returns: HashMap<&'static str, Vec<RbsReturnShape>>,
    /// Instance methods whose author-declared RBS return is `void` (ADR-100:
    /// the strongest "do not rely on this return" signal; the reference widens
    /// it to `top` and records a void origin). Meaningful only for keys present
    /// in `methods` from the SAME definition (reopen first-write-wins is
    /// preserved by the merge). Consumed by `static.value-use.void`.
    void_methods: HashSet<&'static str>,
    /// The singleton twin of `void_methods` (`def self.x: () -> void`).
    void_singleton_methods: HashSet<&'static str>,
    /// Instance methods this declaration marks PRIVATE — either per-`def`
    /// (`private def respond_to_missing?: ...`) or by falling under a bare
    /// `private` section modifier. Rides the same first-write-wins reopen
    /// discipline as `void_methods` (it is the same definition's visibility).
    /// Read ONLY by [`CoreData::instance_method_names`], i.e. LSP completion:
    /// the DIAGNOSTIC predicates deliberately ignore it, because a private
    /// method is still *present* and witnessing its absence would be a false
    /// positive (`send(:foo)` and implicit-self calls both dispatch to it).
    private_methods: HashSet<&'static str>,
    /// INSTANCE methods contributed by RBS ATTRIBUTE members — `attr_reader x`
    /// ⇒ `x`, `attr_writer x` ⇒ `x=`, `attr_accessor x` ⇒ both — recorded as
    /// bare EXISTENCE, deliberately separate from `methods`.
    ///
    /// Measured 2026-08-08 (qualified-witnessing S1): rigor-rs's ingestion
    /// simply DROPPED every `Node::Attr*` member, so `URI::Generic#host`
    /// (`stdlib/uri/0/generic.rbs:245`, an `attr_reader`) and
    /// `Gem::Specification#version` read as PROVEN ABSENT. Nothing exercised
    /// that until the class-narrowing witness started resolving qualified guard
    /// classes, at which point it is a live false positive. 47 attribute members
    /// in the vendored stdlib, 58 in the overlay.
    ///
    /// Kept out of `methods` on purpose: that map feeds return typing, arity
    /// envelopes, the ATM overload substrate and sig-gen, none of which this
    /// slice measured for attributes. Existence-only is the whole fix and can
    /// only ever REMOVE a diagnostic (the reference models attributes, so it
    /// never fires on one).
    attr_methods: HashSet<&'static str>,
    /// The SINGLETON twin of [`Self::attr_methods`] (`attr_reader self.x`).
    singleton_attr_methods: HashSet<&'static str>,
    /// `method name -> per-overload positional shapes`, the ATM substrate
    /// (Slice 1). ADDITIVE alongside `methods`: the merged arity/return path
    /// above is untouched; this retains the per-overload, per-parameter detail
    /// that `method_signature` discards. First write wins on reopen (mirroring
    /// `methods`). Read only via [`CoreData::method_overloads`]; no rule wires
    /// it yet.
    method_overloads: HashMap<&'static str, Vec<OverloadSignature>>,
    /// `singleton method name -> per-overload positional shapes`, the ATM
    /// substrate for CLASS-method dispatch (`CGI.parse(...)`, `Base64.decode64`).
    /// The singleton twin of `method_overloads`: populated for `def self.x`
    /// (`Singleton`) AND `def self?.x` (`SingletonInstance`, which also feeds the
    /// instance map). First write wins on reopen. Read only via
    /// [`CoreData::singleton_method_overloads`]; ADDITIVE and output-inert for
    /// every existing rule (the singleton arity/return path is untouched).
    singleton_method_overloads: HashMap<&'static str, Vec<OverloadSignature>>,
    /// Overloads contributed by an OVERLOADING method reopen (`def +:
    /// (BigDecimal) -> BigDecimal | ...` — RBS's trailing `...` appends the
    /// previously-defined overloads). Kept ASIDE from `method_overloads` because
    /// the first-write-wins reopen merge would otherwise drop them; the global
    /// merge PREPENDS these onto the base definition's overload list (RBS
    /// semantics: the reopen's own overloads come first), which is how the
    /// reference renders `5 + nil` as `expected BigDecimal | Integer | Float |
    /// Rational | Complex`. Instance side.
    overloading_method_overloads: Vec<(&'static str, Vec<OverloadSignature>)>,
    /// The singleton twin of `overloading_method_overloads`.
    overloading_singleton_overloads: Vec<(&'static str, Vec<OverloadSignature>)>,
    /// `method name -> block-overload return class name`, populated ONLY for
    /// methods that declare a block-bearing overload whose return is a
    /// resolvable concrete class (a `ClassInstanceType` like `Hash#filter { }
    /// -> ::Hash[K,V]` / `Enumerable#map { } -> ::Array[U]`) or the literal
    /// receiver itself (a `self` return like `Array#each { } -> self` /
    /// `Kernel#tap { } -> self`). The latter is stored as the sentinel
    /// [`SELF_RETURN`] and resolved to the receiver's own class at lookup time.
    /// Mirrors the reference's `block_required: true` overload selection
    /// (`rbs_dispatch.rb`): a block at the call site picks the block overload,
    /// and ITS return type is what the call yields. Methods with no block
    /// overload, or whose block overload returns a generic/union/void/unknown
    /// shape, are simply absent here (⇒ the block call stays Dynamic / silent).
    block_returns: HashMap<&'static str, &'static str>,
    /// Collection-shape stage 2a/2c: `method name -> the return class agreed by
    /// the BLOCK-FREE overloads alone`, populated ONLY for methods that declare
    /// BOTH a block-bearing and a block-free overload — i.e. exactly the set the
    /// flat `methods` return slot can lose to the block overload's divergent
    /// return. `String#split: (…) -> Array[String] | (…) { … } -> self` and
    /// `Dir.glob: (…) -> Array[String] | (…) { … } -> nil` are the archetypes:
    /// the flat slot collapses to `None` (⇒ Dynamic) even though a BLOCK-FREE
    /// call site unambiguously yields `Array`.
    ///
    /// Read ONLY from the block-free call path
    /// ([`CoreData::method_return_block_free`]), mirroring the reference's
    /// `OverloadSelector` with `block_required: false` — a block call still rides
    /// `block_returns`. Only a bare concrete `ClassInstanceType` return is
    /// recorded, and every block-free overload must agree; `self`/`instance`/
    /// nilable/`void`/union/generic returns and any disagreement are simply
    /// absent (⇒ the existing Dynamic decline, zero-FP).
    block_free_returns: HashMap<&'static str, &'static str>,
    /// The singleton twin of `block_free_returns` (`Dir.glob`, `Dir.[]`).
    singleton_block_free_returns: HashMap<&'static str, &'static str>,
    /// Singleton (class-level) methods `def self.x` (and the singleton half of
    /// `def self?.x`). Keyed by name -> `(resolved return class, arity envelope)`.
    /// The singleton class inherits down the SUPERCLASS chain, so resolving a
    /// class method walks these maps up `superclass`. The return-class slot
    /// mirrors the instance `methods` table's resolution discipline (a single
    /// bare concrete `ClassInstanceType` ⇒ `Some(name)`, else `None`) and is read
    /// ONLY by the sig-gen-only [`Self::declared_singleton_return`]; the existence
    /// check ([`Self::class_has_singleton_method`]) uses just the key set.
    singleton_methods: HashMap<&'static str, (Option<&'static str>, ArityEnvelope, bool)>,
    /// Instance-method aliases `new_name -> old_name` (RBS `alias size length`).
    /// The alias target is resolved at lookup time so `new_name` inherits
    /// `old_name`'s existence / return type / arity (the old name may live on
    /// the same class or anywhere up the ancestor chain).
    aliases: HashMap<&'static str, &'static str>,
    /// Singleton (class-method) aliases `new -> old` (RBS `alias self.pwd
    /// self.getwd`, `alias self.escape self.shellescape`). Resolved at singleton
    /// lookup time over the singleton chain. These are COMMON in core/stdlib
    /// (File/Dir/Shellwords/…); omitting them makes the singleton surface look
    /// complete-but-missing-`new` and witnesses a real class method as absent.
    singleton_aliases: HashMap<&'static str, &'static str>,
    /// Direct superclass name, if any (`None` ⇒ implicit `Object`, except the
    /// roots which are seeded explicitly).
    superclass: Option<&'static str>,
    /// ADR-0042 Slice 5: the superclass reference AS WRITTEN — full namespace
    /// path with a leading `"::"` preserved for an absolute reference
    /// (`"Digest::Class"`, `"::Class"`) — paired with the OUTER lexical
    /// context it must be resolved in (the chain of enclosing declarations'
    /// qualified keys, outermost→innermost; the class itself is NOT in it,
    /// mirroring the reference's `outer_context` for a super clause,
    /// `environment.rb:600`). The short `superclass` field above stays
    /// leaf-only; this is read ONLY by the qualified return-lookup path.
    superclass_written: Option<(&'static str, Vec<&'static str>)>,
    /// ADR-0042 Slice 5: `include` references as written (same spelling rules
    /// as `superclass_written`) paired with their INNER lexical context (which
    /// DOES contain the declaring class itself — the reference resolves member
    /// names in `inner_context`, `environment.rb:608`). Read only by the
    /// qualified return-lookup path; the short `includes` list is unchanged.
    includes_written: Vec<(&'static str, Vec<&'static str>)>,
    /// The [`Self::prepends`] twin carrying the reference AS WRITTEN plus the
    /// INNER lexical context, exactly as `includes_written` does.
    prepends_written: Vec<(&'static str, Vec<&'static str>)>,
    /// A `module` declaration's SELF-TYPE constraints as WRITTEN
    /// (`module PPMethods : _PPMethodsRequired`, `module Kernel : BasicObject`),
    /// paired with the OUTER lexical context — the self-type clause is resolved
    /// like a `< X` super clause, not like a member. RBS folds these into the
    /// MODULE'S OWN instance definition (the reference's `PP::PPMethods` has
    /// exactly its 11 declared methods plus the interface's `text`/`breakable`/
    /// `group`, and NOT `Object`'s surface), so the lookups consult them for the
    /// leaf module only and never propagate them to an includer.
    self_types_written: Vec<(&'static str, Vec<&'static str>)>,
    /// ADR-0042 Slice 5: every INNER lexical context this entry's members were
    /// ingested under — one per reopen spelling, deduped (`class Digest::Class`
    /// contributes `["Digest::Class"]`; a `module Digest; class Class` reopen
    /// would contribute `["Digest", "Digest::Class"]`). Member-level type
    /// references whose namespace the flat tables discarded (return-class
    /// leaves, block-return leaves) resolve against ALL of these and must
    /// agree — any disagreement declines (FP-safe under-emit, never a guess).
    member_ctxs: Vec<Vec<&'static str>>,
    /// Issue #168: every name-bearing type this entry's members REFERENCE —
    /// the port's `unresolved_referenced_types` input (`collect_member_references`
    /// / `collect_type_references` in the reference loader): instance-side method
    /// signatures (all overloads, `initialize` and `def self.x` excluded —
    /// `validate_type_params` never reaches them), non-singleton attribute
    /// types, include/extend/prepend type ARGUMENTS, superclass type
    /// arguments, and module self-type arguments — with each name stored the
    /// way `resolve_type_names` left it (`use`-mapped / `::`-anchored /
    /// root-only). Header NAMES (the superclass itself, a mixin's module
    /// name, a module self-type's name) are deliberately NOT here: the
    /// reference fails the whole definition build on those rather than
    /// stubbing them, which `project_sig_chain_ok` mirrors instead. Read
    /// only by [`synthesize_missing_referenced_types`].
    referenced_type_names: Vec<&'static str>,
    /// `true` when this name was declared as a `module` (not a `class`) in RBS —
    /// the analogue of the reference's `Environment#rbs_module?`. Read ONLY by
    /// `call.raise-non-exception`'s instance path (a value typed as a module
    /// includer could be an Exception at runtime, so it must stay silent). Set on
    /// the module ingest, OR-merged across reopens.
    is_module: bool,
    /// Included module names (in source order).
    includes: Vec<&'static str>,
    /// `prepend`ed module names (in source order). A prepended module sits
    /// BEFORE the prepending class in Ruby's method resolution order (an
    /// `include` sits after it), so the walks push these ahead of the class
    /// itself — first-definer-wins then matches RBS's own linearization.
    /// Without them `class LoadError; prepend DidYouMean::Correctable; end`
    /// contributed nothing and `LoadError#corrections` read as proven-absent
    /// (a false positive) while the reference resolves it.
    prepends: Vec<&'static str>,
    /// `extend`ed module names (in source order). An `extend M` directive folds
    /// `M`'s INSTANCE methods into THIS class/module's SINGLETON surface (the
    /// class object gains them as class methods — e.g. `SecureRandom` does
    /// `extend Random::Formatter`, so `SecureRandom.hex` is a real class method).
    extends: Vec<&'static str>,
    /// `true` when the REFERENCE cannot build this class's INSTANCE definition —
    /// `RbsLoader#instance_definition` fail-softs to `nil`, so every instance
    /// call on it degrades to `Dynamic[Top]` and the oracle is silent about it.
    /// See [`UNBUILDABLE_DEFINITIONS`] for the mechanism and the provenance of
    /// the set. Set post-`finish` by [`mark_unbuildable_definitions`], which also
    /// EMPTIES the instance-side tables so no return type resolves either — the
    /// two halves together are what "the definition is nil" means.
    instance_unbuildable: bool,
    /// The singleton twin of [`Self::instance_unbuildable`]. Tracked SEPARATELY
    /// because the reference builds the two sides independently and they fail
    /// independently: `Bundler` and `Gem::Requirement` build their instance
    /// definition fine and only their SINGLETON build raises, so the oracle still
    /// witnesses their instance methods. Conflating the two would be FP-safe but
    /// would silence rigor-rs where the oracle speaks.
    singleton_unbuildable: bool,
}

/// Which signature source the loaded [`CoreData`] was built from. Surfaced by
/// `rigor doctor` so the embedded-vs-override coverage state is observable
/// (audit-R1 / ADR-0007): the standalone default is [`Embedded`](RbsSource::Embedded),
/// the out-of-band refresh seam is [`Override`](RbsSource::Override), and
/// [`Stub`](RbsSource::Stub) is the degenerate "nothing parsed" fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RbsSource {
    /// The build-time-embedded vendored RBS set (the standalone default — no
    /// runtime filesystem dependency).
    Embedded,
    /// The `RIGOR_RBS_CORE_DIR` override directory was set AND usable; the path
    /// carried is the dir that was ingested.
    Override(String),
    /// Neither the override nor the embedded set yielded any classes — the
    /// hardcoded conservative stub.
    Stub,
}

/// The loaded core data backing [`crate::CoreIndex`] and the free
/// `method_return` / `method_arity` functions.
pub struct CoreData {
    /// Which signature source this data was built from (embedded / override /
    /// stub). Informational only — used by `rigor doctor` to report the active
    /// RBS coverage source.
    source: RbsSource,
    /// `class name -> entry`. Keys are `&'static str` (leaked once at load) so
    /// resolved return-class names can flow out as `&'static str`.
    classes: HashMap<&'static str, ClassEntry>,
    /// Short names that were declared at GENUINE top level (empty namespace) in
    /// at least one declaration — e.g. `class Time` ⇒ `"Time"`, but a name that
    /// only ever appears namespaced (`class Process::Status`) is NOT here. Used
    /// by [`Self::knows_toplevel_class`] so an ambiguous short name shared by a
    /// project class and a namespaced stdlib class is not falsely treated as a
    /// known top-level core class (defect 2).
    toplevel_classes: HashSet<&'static str>,
    /// Class names INTRODUCED by project-`sig/` ingestion (ADR-0033) — those the
    /// project's own signatures declared that no bundled (core/stdlib/plugin) RBS
    /// already carried. The dispatch rules treat these as AUTHORITATIVE for
    /// witnessing an `X.new` instance method typo (the reference witnesses a
    /// project-sig class but stays lenient on a bundled stdlib/gem class like
    /// `Pathname`), so this set is the provenance gate that keeps the two apart.
    /// Empty when no `sig/` was ingested.
    project_sig_classes: HashSet<&'static str>,
    /// ADR-0042 Slice 4: the QUALIFIED-key twin of `project_sig_classes` — the
    /// fully-qualified names the project `sig/` INTRODUCED (`Outer::Inner`),
    /// so a `.new` typo on a NESTED project-sig class witnesses through the
    /// qualified path the reference uses (`is_project_sig_class` is short-key
    /// and would miss `Outer::Inner`).
    qualified_project_sig_classes: HashSet<&'static str>,
    /// ATM substrate (Slice 1): `type` alias name → its right-hand-side one-level
    /// [`RetainedParamType`] tag (`type string = String | _ToStr` ⇒
    /// `"string" → Union([ClassInstance("String", []), Interface("_ToStr", [])])`).
    /// The RHS is stored RAW (aliases inside it are NOT expanded); bounded
    /// expansion with a cycle cap is Slice 2's job. Read only via
    /// [`Self::resolve_type_alias`]; no rule wires it yet.
    type_alias_defs: HashMap<&'static str, RetainedParamType>,
    /// ATM substrate: `type` alias name → its declared type parameter names
    /// (`type range[T] = ...` ⇒ `"range" → ["T"]`; a nullary alias maps to an
    /// empty vec). A use site `range[int]` substitutes positionally — the
    /// reference `expand_alias2(name, args)` semantics — when the
    /// translated-describe label expands the alias. Read via
    /// [`Self::type_alias_params`].
    type_alias_params: HashMap<&'static str, Vec<&'static str>>,
    /// ATM substrate (Slice 1): `interface` name → its declared method names, in
    /// declaration order (`interface _ToStr; def to_str: ...; end` ⇒
    /// `"_ToStr" → ["to_str"]`). Read only via [`Self::interface_methods`]; no
    /// rule wires it yet.
    interface_method_names: HashMap<&'static str, Vec<&'static str>>,
    /// ADR-0042 Slice 1: the qualified-key registry — `"ERB::Util"` and
    /// `"CGI::Util"` are DISTINCT entries here, unlike `classes` where both
    /// collapse onto the shared short key `"Util"`. PURELY ADDITIVE: no
    /// existing accessor reads this; it backs only the new
    /// `knows_qualified_class` / `qualified_declares_instance` /
    /// `qualified_declares_singleton` accessors below (Slice 2's seam).
    qualified: HashMap<&'static str, ClassEntry>,
    /// ADR-0042 Slice 1: leaf (short) name -> the qualified keys sharing it.
    /// Backs [`Self::resolve_short_unambiguous`].
    short_to_qualified: HashMap<&'static str, Vec<&'static str>>,
    /// Collection-shape stage 2b: TOP-LEVEL RBS **object constant**
    /// declarations — `ENV: RBS::Unnamed::ENVClass`, `ARGV: Array[String]`,
    /// recorded under the declared type's LEAF name (`ENVClass`), the same short
    /// key the class tables use —
    /// `STDOUT: IO` — mapped to the declared type's class name. The whole
    /// vendored set is 18 declarations; only those whose declared type is a bare
    /// `ClassInstanceType` are recorded (`CROSS_COMPILING: true?` is not one).
    ///
    /// Nested constant declarations (`class Float; INFINITY: Float; end`) are
    /// deliberately NOT recorded: their name is namespace-relative and the flat
    /// key would be wrong. First write wins.
    ///
    /// Read via [`Self::object_constant_class`] from ONE place — the typer's
    /// constant-receiver arm, which types the CALL's return, never the constant
    /// itself. So this adds no new witnessing surface for the constant.
    object_constants: HashMap<&'static str, &'static str>,
    /// Issue #168: the qualified name of EVERY type the loaded env declares —
    /// classes, modules, interfaces, type aliases and class/module aliases —
    /// the `RBS::TypeNameResolver#all_names` analogue. The member-level
    /// resolver consults `qualified` (classes only) because a return can only
    /// ever mint a class nominal; the missing-type scan and the
    /// `resolve-type-names: false` root check must answer about ANY declared
    /// type, so this wider set rides along.
    type_names: HashSet<&'static str>,
    /// Issue #168: qualified names the missing-referenced-type stub pass
    /// SYNTHESIZED — the reference's `synthesized_type_names`. A receiver
    /// typed by one is `Dynamic[top]` there (never a method-bearing class),
    /// so the undefined-method gates must never witness through them either.
    synthesized_type_names: HashSet<&'static str>,
    /// Issue #129: the `rigor:v1:conforms-to` scan tables (see
    /// [`conformance`]). Read only by [`Self::conformance_findings`].
    conformance: conformance::ConformanceData,
}

impl CoreData {
    /// Build from the real RBS universe (the reference's default: ALL of
    /// `core/*.rbs` ⊕ the `DEFAULT_LIBRARIES` stdlib set). Never panics: any
    /// per-file parse error is skipped, and stdlib reopens of core classes
    /// (`class Hash ...`) merge into the existing entry (see [`Builder::merge`]).
    ///
    /// **Default:** ingest the build-time-[`EMBEDDED_RBS`] vendored set — no
    /// runtime filesystem dependency (ADR-0007). **Override:** if
    /// `RIGOR_RBS_CORE_DIR` is set, read from that directory at runtime exactly
    /// as before (the out-of-band stdlib-RBS refresh seam, audit-R2): the WHOLE
    /// dir plus the `DEFAULT_LIBRARIES` stdlib closure rooted at `<dir>/../stdlib`.
    /// The embedded path feeds the SAME bytes to the SAME parser as the override
    /// path ([`ingest_rbs_source`]), so the resulting index is byte-identical to
    /// what the override dir produced when it holds the same signatures.
    pub fn load() -> Self {
        Self::load_with_plugins(&[])
    }

    /// Build the core data, THEN ingest each bundled plugin's RBS on top
    /// (ADR-25, config-gated). With `plugins` empty this is byte-identical to
    /// [`Self::load`] — the default no-config path is unchanged.
    ///
    /// The core source is resolved exactly as [`Self::load`] does (the
    /// `RIGOR_RBS_CORE_DIR` override if set, else the embedded vendored set),
    /// and each plugin's `(name, contents)` entries are then fed to the SAME
    /// [`ingest_rbs_source`] / `ruby-rbs` parser. The existing [`Builder::merge`]
    /// reopen-union folds each plugin's reopened `class String ... def squish`
    /// into the EXISTING `String` entry, so the plugin selectors join the core
    /// surface — byte-identical to feeding the reference's bundled RBS through
    /// the core path (the zero-FP keystone). Unknown plugin ids are filtered out
    /// by the caller ([`crate::CoreIndex::with_plugins`]); here every entry is a
    /// real bundled payload.
    pub fn load_with_plugins(plugins: &[&crate::plugins::BundledPlugin]) -> Self {
        Self::load_for_project(plugins, &[])
    }

    /// Build the core data + bundled plugins (as [`Self::load_with_plugins`]),
    /// THEN ingest each project signature directory's `*.rbs` on top (ADR-0033).
    /// With `sig_dirs` empty this is byte-identical to [`Self::load_with_plugins`],
    /// so the no-config / no-`sig/` path is unchanged.
    ///
    /// The project sig is folded through the SAME native `ruby-rbs` parser and
    /// the SAME reopen-union [`Builder::merge`] as core + plugin RBS — no Ruby
    /// runtime, no new format (the ADR-0007 project-signature leg). A project's
    /// own classes thereby join the loaded set, so [`Self::knows_class`] (the
    /// dispatch-rule gate, the analogue of the reference's `rbs_class_known?`)
    /// witnesses them. A `sig_dir` that doesn't exist on disk is inert
    /// ([`ingest_rbs_dir`] skips a non-directory). User-authored RBS degrades
    /// soundly: a per-file parse failure drops only that file's declarations
    /// (ADR-0016), and there is no global resolve pass that a malformed file
    /// could collapse (ADR-0033).
    pub fn load_for_project(
        plugins: &[&crate::plugins::BundledPlugin],
        sig_dirs: &[PathBuf],
    ) -> Self {
        Self::load_for_project_parts(plugins, sig_dirs, &[])
    }

    /// [`Self::load_for_project`] with the rbs-collection gem dirs apart from
    /// the `signature_paths:` ones. Both are project signature files to every
    /// rule; only the `conforms-to` scan tells them apart (issue #129: a class
    /// first declared by a collection gem sits where the port cannot place it
    /// in the reference's declaration order, so its directives stay silent).
    pub fn load_for_project_parts(
        plugins: &[&crate::plugins::BundledPlugin],
        sig_dirs: &[PathBuf],
        collection_dirs: &[PathBuf],
    ) -> Self {
        // 1) Resolve the core source (override dir, else embedded), folding into a
        //    fresh builder — the SAME logic [`Self::load`] previously inlined.
        let mut builder = Builder::default();
        let mut source = RbsSource::Embedded;
        if let Ok(dir) = std::env::var("RIGOR_RBS_CORE_DIR") {
            let dir = PathBuf::from(dir);
            if Self::ingest_dir_into(&mut builder, &dir) {
                source = RbsSource::Override(dir.display().to_string());
            }
            // Override set but unusable (absent / nothing parsed): fall through
            // to the embedded default rather than failing.
        }
        if source == RbsSource::Embedded {
            ingest_embedded(&mut builder);
        }

        // 2) Ingest each bundled plugin's RBS on top of whichever core source was
        //    used. The reopen-union merge handles classes already present.
        for plugin in plugins {
            for (name, contents) in plugin.rbs {
                let key = intern(&format!("plugin:{}:{name}", plugin.id));
                builder.conformance.set_origin(Some(conformance::Origin::Plugin(key)));
                ingest_rbs_source(&mut builder, name, contents);
                builder.conformance.set_origin(None);
            }
        }

        // 3) Ingest the project's own `sig/` RBS on top of core + plugin
        //    (ADR-0033). Same parser, same reopen-union merge. Snapshot the
        //    class keyset first so the names the project sig INTRODUCES (vs
        //    reopens of an already-bundled class) are recorded as project-sig
        //    provenance — the witnessing gate for `X.new` typos.
        let pre_sig: HashSet<&'static str> = builder.classes.keys().copied().collect();
        let pre_sig_qualified: HashSet<&'static str> =
            builder.qualified.keys().copied().collect();
        ingest_project_dirs(&mut builder, sig_dirs, collection_dirs);
        // Issue #129: the capability-role catalogue goes in AFTER the project's
        // own signatures, per declaration (conformance table only).
        builder.conformance.ingest_capability_roles();
        let conformance = std::mem::take(&mut builder.conformance).finish();
        let project_sig_classes: HashSet<&'static str> = builder
            .classes
            .keys()
            .copied()
            .filter(|k| !pre_sig.contains(k))
            .collect();
        let qualified_project_sig_classes: HashSet<&'static str> = builder
            .qualified
            .keys()
            .copied()
            .filter(|k| !pre_sig_qualified.contains(k))
            .collect();

        // Issue #168: stub the project-declared references the reference's
        // `stub_missing_referenced_types` fixes up — BEFORE `finish`, so the
        // synthesized names join the same maps a written decl would (and the
        // project-sig snapshots above already exclude them).
        synthesize_missing_referenced_types(&mut builder, &qualified_project_sig_classes);

        let (
            mut classes,
            toplevel_classes,
            type_alias_defs,
            type_alias_params,
            interface_method_names,
            mut qualified,
            short_to_qualified,
            object_constants,
            type_names,
            synthesized_type_names,
        ) = builder.finish();
        // 4) Neutralise the classes whose definition the REFERENCE cannot build
        //    (see `UNBUILDABLE_DEFINITIONS`). Applied LAST, after project `sig/`
        //    and plugins: the reference's collision is in its bundled load set, so
        //    a project reopening one of these classes does not repair the build
        //    there either — it still resolves `Dynamic[Top]`.
        mark_unbuildable_definitions(&mut classes, &mut qualified);
        if !classes.is_empty() {
            return Self {
                source,
                classes,
                toplevel_classes,
                project_sig_classes,
                qualified_project_sig_classes,
                type_alias_defs,
                type_alias_params,
                interface_method_names,
                qualified,
                short_to_qualified,
                object_constants,
                type_names,
                synthesized_type_names,
                conformance,
            };
        }
        // Fallback: nothing parsed (shouldn't happen) ⇒ hardcoded stub. The stub
        // carries no plugin selectors, which stays conservative (zero-FP).
        Self::stub()
    }

    /// The runtime-filesystem ingest path (the `RIGOR_RBS_CORE_DIR` override):
    /// fold the WHOLE `dir` plus the `DEFAULT_LIBRARIES` stdlib closure rooted at
    /// `<dir>/../stdlib` INTO `builder`. Returns `true` when the dir exists and
    /// something parsed (so the caller knows the override is usable), `false` when
    /// the dir is absent or nothing parsed (caller then falls back to the embedded
    /// default). Folding into a passed-in builder (rather than building `Self`)
    /// lets [`Self::load_with_plugins`] ingest plugin RBS on top of the SAME
    /// builder. This is the same core/stdlib logic the default previously ran.
    fn ingest_dir_into(builder: &mut Builder, dir: &std::path::Path) -> bool {
        if !dir.is_dir() {
            return false;
        }

        // 1) The WHOLE core dir (~62 files), not a curated subset — so every
        //    core class + its full ancestor chain is loaded.
        ingest_rbs_dir(builder, dir);

        // 2) The DEFAULT_LIBRARIES stdlib set, rooted at `<core>/../stdlib`,
        //    transitively closed over each lib's `manifest.yaml` deps (the
        //    reference resolves these — e.g. `yaml` ⇒ `psych` ships the
        //    `Object#to_yaml` reopen, `csv` ⇒ `stringio`). Each lib is
        //    `stdlib/<lib>/0/*.rbs`; an absent lib (e.g. `prism`/`rbs`, or a
        //    dep like `socket` not in this tree) is skipped silently.
        if let Some(root) = dir.parent() {
            let stdlib = root.join("stdlib");
            let mut loaded: HashSet<String> = HashSet::new();
            let mut queue: Vec<String> =
                DEFAULT_LIBRARIES.iter().map(|s| s.to_string()).collect();
            while let Some(lib) = queue.pop() {
                if !loaded.insert(lib.clone()) {
                    continue;
                }
                let lib_dir = stdlib.join(&lib).join("0");
                if !lib_dir.is_dir() {
                    continue; // ships RBS elsewhere / not in this tree ⇒ skip.
                }
                ingest_rbs_dir(builder, &lib_dir);
                // Enqueue manifest dependencies (transitive closure).
                for dep in manifest_deps(&lib_dir.join("manifest.yaml")) {
                    if !loaded.contains(&dep) {
                        queue.push(dep);
                    }
                }
            }
        }

        // 3) The rigor-owned overlay tree (`<root>/overlay/**`), loaded LAST for
        //    the same reason the embedded path loads it last: it only fills holes
        //    upstream RBS leaves, so upstream must win on any conflict. Keeping
        //    the override seam in step with the embedded default is what stops a
        //    `RIGOR_RBS_CORE_DIR` refresh from silently re-opening the
        //    `DidYouMean.formatter` class of false positive.
        if let Some(root) = dir.parent() {
            ingest_rbs_dir(builder, &root.join("overlay"));
        }

        // "Usable" means the core dir yielded at least one class. The builder may
        // already hold classes from a prior fold, but the override is only ever
        // ingested into a FRESH builder, so non-empty ⇒ this dir parsed.
        !builder.classes.is_empty()
    }

    /// Whether the class is in the loaded set.
    pub fn knows_class(&self, class_name: &str) -> bool {
        self.classes.contains_key(class_name)
    }


    /// Whether `class_name` was declared at GENUINE top level (empty namespace)
    /// in at least one RBS declaration. Conservative companion to
    /// [`Self::knows_class`]: returns `true` ONLY for names that genuinely have a
    /// top-level declaration. A name that exists in the index solely because a
    /// namespaced/nested decl (`class Process::Status`) was registered by its
    /// short key (`"Status"`) returns `false`, so a project class sharing that
    /// short name is not falsely resolved to the namespaced stdlib class
    /// (defect 2). Instance-method behavior and `knows_class` are unchanged.
    pub fn knows_toplevel_class(&self, class_name: &str) -> bool {
        self.toplevel_classes.contains(class_name)
    }

    /// ADR-0042 Slice 1: whether `qname` (a fully qualified name like
    /// `"ERB::Util"`) is in the qualified registry. Unlike [`Self::knows_class`],
    /// this does NOT collapse `ERB::Util` and `CGI::Util` onto a shared short
    /// key — each qualified name is its own entry. This is the Slice-2 seam;
    /// no existing rule calls it yet.
    pub fn knows_qualified_class(&self, qname: &str) -> bool {
        self.qualified.contains_key(qname)
    }

    /// ADR-0042 Slice 1: resolve a bare short name to its qualified key, but
    /// ONLY when unambiguous — i.e. exactly one qualified key shares that leaf.
    /// Returns `None` when the short name is unknown OR ambiguous (2+
    /// qualified keys share it, e.g. `"Util"` ⇒ both `ERB::Util` and
    /// `CGI::Util`): the ambiguity-collapses-to-nothing rule, so an ambiguous
    /// short name is never silently resolved to the wrong one. This is the
    /// Slice-2 seam; no existing rule calls it yet.
    pub fn resolve_short_unambiguous(&self, short: &str) -> Option<&'static str> {
        match self.short_to_qualified.get(short) {
            Some(quals) if quals.len() == 1 => Some(quals[0]),
            _ => None,
        }
    }

    /// ADR-0042 Slice 1: whether the qualified class/module `qname` declares
    /// `method` as a SINGLETON (class) method, checking ONLY that entry's own
    /// `singleton_methods` — NO ancestor-chain walk (Slice 2 owns full
    /// resolution over the qualified registry). `false` for an unknown
    /// `qname`. This is the Slice-2 seam; no existing rule calls it yet.
    pub fn qualified_declares_singleton(&self, qname: &str, method: &str) -> bool {
        self.qualified
            .get(qname)
            .is_some_and(|entry| entry.singleton_methods.contains_key(method))
    }

    /// ADR-0042 Slice 1: the instance-method twin of
    /// [`Self::qualified_declares_singleton`] — checks ONLY the qualified
    /// entry's own `methods`, no ancestor-chain walk. `false` for an unknown
    /// `qname`. This is the Slice-2 seam; no existing rule calls it yet.
    pub fn qualified_declares_instance(&self, qname: &str, method: &str) -> bool {
        self.qualified
            .get(qname)
            .is_some_and(|entry| entry.methods.contains_key(method))
    }

    /// ADR-0042 Slice 2: the qualified-registry analogue of
    /// [`Self::class_has_singleton_method`] for a namespaced receiver
    /// (`ERB::Util.html_escape`). The qualified entry's OWN singleton surface
    /// (own `def self.x` + every `extend`ed module's instance methods, resolved
    /// short — modules are typically top-level) PLUS the base-object surface
    /// (`Class`/`Module`/`Object`/`Kernel`/`BasicObject`, reused verbatim). A
    /// superclass or `extend` target that is not resolvable truncates the
    /// surface ⇒ conservative silent (never a false positive). References are
    /// kept short in this slice (measure-first); a nested unresolvable
    /// reference just marks the surface incomplete.
    fn qualified_class_has_singleton_method(&self, qname: &str, method: &str) -> bool {
        let Some(entry) = self.qualified.get(qname) else {
            return true; // unknown ⇒ silent
        };
        // See `qualified_class_has_method`, singleton side: an unbuildable
        // singleton definition has no known surface, and its emptied tables must
        // not read as proven-absent.
        if entry.singleton_unbuildable {
            return true;
        }
        // Measure-first scope (ADR-0042 Slice 2): witness a qualified-singleton
        // absence ONLY for a MODULE, whose class-object surface is its own
        // `module_function`s + `extend`s + the base-object surface — fully
        // modelled here. A CLASS additionally inherits class methods down its
        // superclass chain, which this slice does NOT walk over the qualified
        // registry (references are stored short pending ADR step 3); witnessing
        // absence on a class therefore over-fires (measured: 36 FPs on
        // dependabot-core, all `singleton(Gem::Specification)` — inherited class
        // methods judged absent). Stay silent on qualified classes until the
        // chain walk lands.
        if !entry.is_module {
            return true;
        }
        // (1) own singleton methods, resolving a singleton ALIAS to its target
        //     (`alias self.h self.html_escape` on ERB::Util — measured: 3 rails
        //     FPs on `ERB::Util.h`). Bounded one hop is enough for the aliases
        //     seen; a target that is itself an alias resolves via recursion.
        if entry.singleton_methods.contains_key(method)
            || entry.singleton_attr_methods.contains(method)
            || self.qualified_singleton_alias_resolves(qname, method, 0)
        {
            return true;
        }
        // (2) extended modules' instance methods (resolved short: an extended
        //     module is almost always a top-level or already-qualified name);
        //     an unresolvable extend truncates the surface.
        let mut complete = entry.superclass.is_none_or(|s| self.classes.contains_key(s));
        for &module in &entry.extends {
            let resolved = if self.classes.contains_key(module) {
                Some(module)
            } else {
                self.resolve_short_unambiguous(module)
            };
            match resolved {
                Some(m) => {
                    let (chain, _) = self.ancestors(m);
                    if self.lookup_on_chain(&chain, method).is_some()
                        || self.attr_on_chain(&chain, method)
                    {
                        return true;
                    }
                }
                None => complete = false,
            }
        }
        // (3) the base-object surface (shared with the short-key path).
        let (bases_found, bases_loaded) = self.singleton_bases_lookup(method);
        if bases_found {
            return true;
        }
        // Witness absence only when the whole surface is known.
        if complete && bases_loaded {
            return false;
        }
        true
    }

    /// Whether `method` on the qualified module `qname` resolves through a
    /// singleton ALIAS to a real singleton method (`alias self.h
    /// self.html_escape`). Bounded recursion (target may itself be aliased).
    fn qualified_singleton_alias_resolves(&self, qname: &str, method: &str, depth: usize) -> bool {
        if depth >= 16 {
            return false;
        }
        let Some(entry) = self.qualified.get(qname) else {
            return false;
        };
        let Some(&target) = entry.singleton_aliases.get(method) else {
            return false;
        };
        entry.singleton_methods.contains_key(target)
            || self.qualified_singleton_alias_resolves(qname, target, depth + 1)
    }

    /// Walk the full flattened ancestor chain; a method is present if ANY
    /// ancestor defines it directly OR via an instance `alias`. Conservative
    /// gate: if the chain is not fully loaded (an ancestor missing from the
    /// set), return `true` ("assume present") so absence is never falsely
    /// witnessed. Absence (`false`) is only returned when every ancestor is
    /// loaded and none defines (or aliases) the method.
    pub fn class_has_method(&self, class_name: &str, method: &str) -> bool {
        if !self.classes.contains_key(class_name) {
            return false;
        }
        let (chain, complete) = self.ancestors(class_name);
        if self.lookup_on_chain(&chain, method).is_some() {
            return true;
        }
        // S1: an RBS ATTRIBUTE member (`attr_reader host: String?`) defines a
        // real method the `methods` table does not carry. See
        // `ClassEntry::attr_methods`.
        if self.attr_on_chain(&chain, method) {
            return true;
        }
        // Not found across the chain. Only witness absence if the chain is
        // fully loaded; otherwise assume present (zero false positive).
        !complete
    }

    /// Whether any class on a SHORT-key ancestor `chain` declares `method` as
    /// an RBS attribute member.
    fn attr_on_chain(&self, chain: &[&'static str], method: &str) -> bool {
        chain
            .iter()
            .any(|a| self.classes.get(a).is_some_and(|e| e.attr_methods.contains(method)))
    }

    /// The QUALIFIED-registry twin of [`Self::attr_on_chain`].
    fn qualified_attr_on_chain(&self, chain: &[&'static str], method: &str) -> bool {
        chain
            .iter()
            .any(|a| self.qualified.get(a).is_some_and(|e| e.attr_methods.contains(method)))
    }

    /// ADR-0042 Slice 3: the qualified-registry analogue of
    /// [`Self::class_has_method`] — instance-method existence over the ISOLATED
    /// qualified entry (`qualified["Status"]` = the project's own surface, NOT
    /// the short-key merge of project `Status` + stdlib `Process::Status`). The
    /// LEAF's own methods + instance aliases come from the qualified entry; its
    /// ANCESTORS (superclass / includes — stored short, but ancestors are
    /// top-level/global names) resolve through the existing short-key chain
    /// walk, so `Object`/`Kernel`/`BasicObject` are found. A class with no
    /// declared superclass defaults to `Object` (mirroring [`Self::finish`],
    /// which defaults only the short map). Absence is witnessed ONLY when the
    /// whole chain is loaded (conservative-complete, never a false positive).
    pub fn qualified_class_has_method(&self, qname: &str, method: &str) -> bool {
        let Some(entry) = self.qualified.get(qname) else {
            return true; // unknown ⇒ silent
        };
        // The reference cannot build this INSTANCE definition ⇒ no known surface
        // ⇒ silent (`UNBUILDABLE_DEFINITIONS`). Checked before the leaf lookup
        // because the tables were emptied, which would otherwise read as
        // proven-absent.
        if entry.instance_unbuildable {
            return true;
        }
        // Leaf's own instance methods + instance aliases.
        if entry.methods.contains_key(method) || Self::instance_alias_resolves(entry, method) {
            return true;
        }
        // Ancestors: walk each include + the superclass through the SHORT-key
        // chain (ancestors are global names). A class's implicit `Object`
        // superclass is defaulted here (the qualified map is not Object-defaulted
        // in `finish`).
        let mut order: Vec<&'static str> = Vec::new();
        let mut seen: HashSet<&'static str> = HashSet::new();
        let mut complete = true;
        for pre in &entry.prepends {
            self.collect(pre, &mut order, &mut seen, &mut complete);
        }
        for inc in &entry.includes {
            self.collect(inc, &mut order, &mut seen, &mut complete);
        }
        // A MODULE gets `Object` here too. RBS gives a module declaration an
        // implicit self-type of `::Object` when it declares none, so a value
        // that satisfies `is_a?(Digest::Instance)` carries Object's surface —
        // oracle-verified 2026-08-08: the reference is SILENT on
        // `Digest::Instance#frozen?` while it FIRES on a typo (probe q5). The
        // pre-S1 `!entry.is_module` guard made every Object method on a module
        // guard target read as proven-absent, which S2 would have turned into a
        // live false positive.
        let sup = entry.superclass.or({
            if qname != "BasicObject" {
                Some("Object")
            } else {
                None
            }
        });
        if let Some(s) = sup {
            self.collect(s, &mut order, &mut seen, &mut complete);
        }
        if self.lookup_on_chain(&order, method).is_some() || self.attr_on_chain(&order, method) {
            return true;
        }
        // S1 (2026-08-08): the SHORT-key ancestor walk above under-reports
        // inherited methods for a namespaced entry, because a nested
        // superclass/include is stored LEAF-only and the leaf may be the wrong
        // class or a merged composite. Measured "proven absent" while the
        // reference is SILENT: `Digest::SHA256#hexdigest` / `#digest`
        // (`Digest::Base → Digest::Class → include Digest::Instance`, whose
        // leaves `Base`/`Class` are both ambiguous), probes v1/v2/v3/p7b.
        //
        // The PR #64 machinery already resolves those references AS WRITTEN
        // against their recorded lexical context (`collect_qualified` →
        // `resolve_written_ref`), so consult that chain too. It is an
        // additional PRESENT source, never a new absence witness: the
        // short-key result above is left untouched, and a walk that could not
        // resolve every link (`complete == false`) reads as PRESENT — the safe
        // direction for an absence witness, and exactly the spec's
        // "residual ambiguity ⇒ treat the method as present" rule.
        let (qchain, qcomplete) = self.qualified_ancestors(qname);
        if !qcomplete
            || self.qualified_lookup_on_chain(&qchain, method, 0).is_some()
            || self.qualified_attr_on_chain(&qchain, method)
        {
            return true;
        }
        // A module's SELF-TYPE constraint is part of ITS OWN instance surface in
        // RBS: `module PPMethods : _PPMethodsRequired` resolves `#text` /
        // `#breakable` / `#group` from the interface, and the reference's
        // definition for `PP::PPMethods` carries exactly those 3 alongside its
        // own 11. Consulted here as an additional PRESENT source only — never a
        // new absence witness, and never propagated to an includer (the
        // reference does not give a class that `include`s a self-typed module
        // the self type's methods).
        if self.qualified_self_type_provides(qname, method) {
            return true;
        }
        // Absent across the leaf + its resolvable ancestry: witness only when
        // the chain is fully loaded.
        !complete
    }

    /// Whether `qname`'s own SELF-TYPE constraints supply `method`. An
    /// INTERFACE self type (`: _PPMethodsRequired`) contributes the interface's
    /// declared method names; a CLASS/MODULE self type (`: BasicObject`)
    /// contributes that entry's flattened qualified surface. A self type that
    /// resolves to nothing answers `true` — the surface is then not fully
    /// known, and an absence witness on it would be a false positive.
    ///
    /// Deliberately NON-recursive (a self type's own self type is not
    /// consulted): the walk cannot cycle, and the 2026-09-09 1202-class diff is
    /// the check that nothing is left uncovered.
    fn qualified_self_type_provides(&self, qname: &str, method: &str) -> bool {
        let Some(entry) = self.qualified.get(qname) else {
            return false;
        };
        for (w, ctx) in &entry.self_types_written {
            let leaf = w.rsplit("::").next().unwrap_or(w);
            if leaf.starts_with('_') {
                // The interface table is keyed by the LEAF name (see
                // `Builder::ingest_interface`).
                match self.interface_method_names.get(leaf) {
                    Some(names) => {
                        if names.contains(&method) {
                            return true;
                        }
                    }
                    None => return true,
                }
                continue;
            }
            match self.resolve_written_ref(w, ctx) {
                Some(k) => {
                    let (chain, complete) = self.qualified_ancestors(k);
                    if !complete
                        || self.qualified_lookup_on_chain(&chain, method, 0).is_some()
                        || self.qualified_attr_on_chain(&chain, method)
                    {
                        return true;
                    }
                }
                None => return true,
            }
        }
        false
    }

    /// Whether an INSTANCE `alias` on `entry` resolves `method` to a real
    /// instance method (`alias size length`). Walks the alias chain iteratively
    /// (bounded) — a free helper (no `self`): resolution stays within one
    /// `entry`'s own alias table. Mirrors the singleton alias resolution.
    fn instance_alias_resolves(entry: &ClassEntry, method: &str) -> bool {
        let mut cur = method;
        for _ in 0..16 {
            match entry.aliases.get(cur) {
                Some(&target) => {
                    if entry.methods.contains_key(target) {
                        return true;
                    }
                    cur = target;
                }
                None => return false,
            }
        }
        false
    }

    /// Whether `name` was declared as a `module` in RBS (the analogue of the
    /// reference `Environment#rbs_module?`). `false` for a class or an unknown
    /// name. Read only by `call.raise-non-exception`'s instance path.
    ///
    /// SHORT-key map: a nested declaration is filed under its LEAF name, so this
    /// answers `false` for `"Digest::Instance"` and `true` for the bare
    /// `"Instance"` (merged, defect-2 style, with every other nested `Instance`).
    /// Callers holding a qualified name want [`Self::is_qualified_module`].
    pub fn is_module(&self, name: &str) -> bool {
        self.classes.get(name).is_some_and(|e| e.is_module)
    }

    /// Whether `qname` — a name spelled as the QUALIFIED registry files it
    /// (`"Digest::Instance"`, `"Enumerable"`) — was declared as an RBS `module`.
    ///
    /// This is the faithful shape of the reference's `Environment#rbs_module?`,
    /// which parses the name and looks `env.class_decls[rbs_name]` up EXACTLY:
    /// no short-key collapse, so a project `class Instance` cannot make
    /// `Digest::Instance`'s moduleness leak onto it (or the reverse). A top-level
    /// module's qualified key IS its bare name, so `Kernel` / `Enumerable` /
    /// `Comparable` answer `true` here too. `false` for a class and for an
    /// unknown name (fail-soft, exactly as the reference's `rescue`).
    pub fn is_qualified_module(&self, qname: &str) -> bool {
        let qname = qname.strip_prefix("::").unwrap_or(qname);
        self.qualified.get(qname).is_some_and(|e| e.is_module)
    }

    /// The subtyping relation of two RBS-known class names, a faithful port of
    /// the reference `Environment::RbsHierarchy#class_ordering`
    /// (`environment/rbs_hierarchy.rb`): `Equal` when the (namespace-stripped)
    /// names match; `Unknown` when either class is unloaded; else `Subclass` when
    /// `lhs`'s ancestry includes `rhs`, `Superclass` when `rhs`'s ancestry
    /// includes `lhs`, else `Disjoint`.
    ///
    /// One conservative deviation from the reference (whose RBS `ancestors`
    /// always yields the COMPLETE linearization): rigor-rs's ancestor walk can be
    /// incomplete when some referenced ancestor is not loaded. When the two
    /// classes are UNRELATED (neither contains the other) but a chain is
    /// incomplete, this returns `Unknown` rather than `Disjoint`, so the caller
    /// never proves disjointness from a partial chain. A positive
    /// `Subclass`/`Superclass` witness stands regardless of completeness (finding
    /// the target IS the proof). For the raise rule's Exception/String targets the
    /// vendored chains are complete, so this matches the reference exactly.
    pub fn class_ordering(&self, lhs: &str, rhs: &str) -> ClassOrdering {
        let lhs = lhs.strip_prefix("::").unwrap_or(lhs);
        let rhs = rhs.strip_prefix("::").unwrap_or(rhs);
        if lhs == rhs {
            return ClassOrdering::Equal;
        }
        if !self.classes.contains_key(lhs) || !self.classes.contains_key(rhs) {
            return ClassOrdering::Unknown;
        }
        let (lhs_anc, lhs_complete) = self.ancestors(lhs);
        let (rhs_anc, rhs_complete) = self.ancestors(rhs);
        if lhs_anc.contains(&rhs) {
            return ClassOrdering::Subclass;
        }
        if rhs_anc.contains(&lhs) {
            return ClassOrdering::Superclass;
        }
        if lhs_complete && rhs_complete {
            ClassOrdering::Disjoint
        } else {
            ClassOrdering::Unknown
        }
    }

    /// Enumerate every PUBLIC INSTANCE method name callable on `class_name` — its
    /// own methods plus those inherited over the flattened ancestor chain
    /// (superclass and included modules), plus instance `alias` names. Sorted and
    /// deduped. Empty when the class is unknown. Used by LSP completion (§12);
    /// unlike the diagnostic predicates this is advisory, so it enumerates the
    /// full known surface without a completeness gate.
    ///
    /// Private methods are EXCLUDED (LSP v4, matching the reference
    /// `CompletionProvider#method_completions`' `next nil unless method.public?`):
    /// a completion popup answers "what may I write after this receiver", and
    /// `"x".respond_to_missing?` is a NoMethodError. Visibility is judged
    /// per-declaring-ancestor, so a subclass that redefines an inherited private
    /// method publicly still offers it.
    pub fn instance_method_names(&self, class_name: &str) -> Vec<&'static str> {
        if !self.classes.contains_key(class_name) {
            return Vec::new();
        }
        let (chain, _complete) = self.ancestors(class_name);
        let mut set: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
        for &anc in &chain {
            if let Some(entry) = self.classes.get(anc) {
                set.extend(
                    entry.methods.keys().copied().filter(|m| !entry.private_methods.contains(m)),
                );
                // An `alias` member carries no visibility of its own in the RBS
                // AST, so it stays public — the conservative direction for an
                // advisory list (offering one extra name, never hiding a real one).
                set.extend(entry.aliases.keys().copied());
                // S1: attribute-generated methods are real completions too.
                set.extend(entry.attr_methods.iter().copied());
            }
        }
        set.into_iter().collect()
    }

    /// Enumerate every SINGLETON (class-object) method name callable on the class
    /// object `class_name`: the `def self.x` methods up its superclass chain, the
    /// instance methods of every `extend`ed module, singleton aliases, and the
    /// instance methods of the base classes the class object is itself an instance
    /// of (`Class`/`Module`/`Object`/`Kernel`/`BasicObject`). Sorted + deduped.
    /// Mirrors the surface of [`Self::class_has_singleton_method`]; advisory
    /// (no completeness gate), for LSP completion on a `Singleton` receiver.
    pub fn singleton_method_names(&self, class_name: &str) -> Vec<&'static str> {
        if !self.classes.contains_key(class_name) {
            return Vec::new();
        }
        let mut set: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();

        // The singleton superclass chain (the singleton class inherits down
        // `superclass`); on each, own `def self.x` + singleton aliases + the
        // instance methods of every `extend`ed module.
        let mut seen: HashSet<&str> = HashSet::new();
        let mut cur = Some(class_name);
        while let Some(name) = cur {
            let Some((&key, entry)) = self.classes.get_key_value(name) else { break };
            if !seen.insert(key) {
                break; // cycle guard.
            }
            set.extend(entry.singleton_methods.keys().copied());
            set.extend(entry.singleton_aliases.keys().copied());
            for &module in &entry.extends {
                for m in self.instance_method_names(module) {
                    set.insert(m);
                }
            }
            cur = entry.superclass;
        }

        // The class object is itself an instance of `Class` (→ `Module` →
        // `Object` → `Kernel`/`BasicObject`), so those instance methods respond
        // on it (`.new`, `.name`, `.tap`, …).
        for base in ["Class", "Module", "Object", "Kernel", "BasicObject"] {
            for m in self.instance_method_names(base) {
                set.insert(m);
            }
        }
        set.into_iter().collect()
    }

    /// LSP v4 `Foo::|` completion: the immediate child namespaces of `parent_fqn`
    /// in the qualified registry, as `(leaf name, is_module)` pairs, sorted and
    /// deduped. `"Process"` yields `[("Status", false), ("UID", true), …]`.
    ///
    /// Immediate children only — a leaf still containing `::` is a grandchild and
    /// is not writable at this cursor. Mirrors the reference's
    /// `enumerate_constant_children`, which walks `RbsLoader#known_class_names_set`
    /// for `parent::<one segment>`; the qualified registry (ADR-0042) is this
    /// port's equivalent surface, so like the reference this sees RBS-declared and
    /// project-`sig/` namespaces, NOT constants defined in the edited buffer.
    pub fn namespace_children(&self, parent_fqn: &str) -> Vec<(&'static str, bool)> {
        let prefix = format!("{parent_fqn}::");
        let mut set: std::collections::BTreeMap<&'static str, bool> =
            std::collections::BTreeMap::new();
        for (&qual, entry) in &self.qualified {
            let Some(leaf) = qual.strip_prefix(prefix.as_str()) else {
                continue;
            };
            if leaf.is_empty() || leaf.contains("::") {
                continue;
            }
            set.insert(leaf, entry.is_module);
        }
        set.into_iter().collect()
    }

    /// Whether the class OBJECT `class_name` responds to a singleton (class)
    /// method `method`. Conservative (zero false positive): returns `true`
    /// ("present ⇒ stay silent") unless the full singleton surface is known to
    /// lack it.
    ///
    /// The singleton surface is the union of:
    ///   (a) `class_name`'s own `def self.x` methods PLUS those of every
    ///       superclass up its chain (the singleton class inherits down the
    ///       superclass chain), AND the INSTANCE methods of every module each of
    ///       those classes `extend`s (`extend M` folds `M`'s instance methods
    ///       into the class object — e.g. `SecureRandom extend Random::Formatter`
    ///       makes `SecureRandom.hex` a class method); AND
    ///   (b) the INSTANCE methods of `Class`/`Module`/`Object`/`Kernel`/
    ///       `BasicObject` — the class object is itself an instance of `Class`,
    ///       so e.g. `Time.name`, `Time.new`, `Time.tap`, `Time.instance_methods`
    ///       are all present and must NOT be witnessed absent.
    ///
    /// Absence (`false`) is returned ONLY when the whole surface is known: the
    /// class is loaded, its superclass chain is COMPLETE, all five base classes
    /// are loaded, and none of (a)/(b) defines `method`. If the class is unknown,
    /// its chain is incomplete, or any base class is missing ⇒ `true`.
    pub fn class_has_singleton_method(&self, class_name: &str, method: &str) -> bool {
        // ADR-0042 Slice 2: a QUALIFIED name (`ERB::Util`) is absent from the
        // short-key `classes` map but present in the qualified registry — route
        // it to the qualified singleton resolution. A top-level name (its own
        // qualified key == its short key) is handled by the short-key path
        // below unchanged (this branch only fires for a genuinely-namespaced
        // name the short map lacks), so no existing behavior moves.
        if !self.classes.contains_key(class_name) && self.qualified.contains_key(class_name) {
            return self.qualified_class_has_singleton_method(class_name, method);
        }
        // Unknown class ⇒ stay silent.
        if !self.classes.contains_key(class_name) {
            return true;
        }
        // (a) Own + inherited singleton methods, walking the superclass chain.
        let (found, complete) = self.singleton_lookup(class_name, method);
        if found {
            return true;
        }
        // (b) Instance methods of the class object's own ancestry (it is a
        //     `Class`) — `new`, `name`, etc. from Class/Module/Object/Kernel/
        //     BasicObject. Their presence is also a completeness precondition.
        let (bases_found, bases_loaded) = self.singleton_bases_lookup(method);
        if bases_found {
            return true;
        }
        // Not found anywhere. Witness absence ONLY when the whole surface is
        // known: the singleton superclass chain is complete AND all five base
        // classes are loaded. Otherwise stay silent.
        if complete && bases_loaded {
            return false;
        }
        true
    }

    /// Whether `method` is an instance method of the class object's own ancestry
    /// — the five base classes a class object is/inherits (Class, Module, Object,
    /// Kernel, BasicObject) — plus whether all five are loaded (a completeness
    /// precondition). Shared by [`Self::class_has_singleton_method`] and the
    /// singleton-alias resolution, so an alias whose TARGET is a base method
    /// (`alias self.compile self.new`, where `new` is `Class#new`) resolves.
    fn singleton_bases_lookup(&self, method: &str) -> (bool, bool) {
        const BASES: [&str; 5] = ["Class", "Module", "Object", "Kernel", "BasicObject"];
        let mut loaded = true;
        let mut found = false;
        for base in BASES {
            if !self.classes.contains_key(base) {
                loaded = false;
                continue;
            }
            let (chain, _) = self.ancestors(base);
            if self.lookup_on_chain(&chain, method).is_some() {
                found = true;
            }
        }
        (found, loaded)
    }

    /// Walk `class_name` and its superclass chain collecting OWN singleton
    /// methods AND the instance methods of every `extend`ed module on the way.
    /// Returns `(found, complete)`: `found` is whether `method` is on that
    /// surface; `complete` is `false` if a referenced superclass OR any
    /// `extend`ed module is not in the loaded set (surface truncated), mirroring
    /// the completeness notion of [`Self::ancestors`]. The incompleteness from a
    /// missing extended module is the critical no-false-positive guard: if e.g.
    /// `Random::Formatter` (extended by `SecureRandom`) is not loaded, the
    /// surface is unknown ⇒ caller stays silent.
    fn singleton_lookup(&self, class_name: &str, method: &str) -> (bool, bool) {
        // Gather the singleton superclass chain (the singleton class inherits
        // down `superclass`), tracking completeness: a referenced superclass or
        // `extend`ed module that isn't loaded truncates the surface.
        let mut chain: Vec<&'static str> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut complete = true;
        let mut cur = Some(class_name);
        while let Some(name) = cur {
            let Some((&key, entry)) = self.classes.get_key_value(name) else {
                complete = false; // referenced superclass not loaded.
                break;
            };
            if !seen.insert(key) {
                break; // Defensive: cycle guard.
            }
            // Same reason as `collect`, singleton side: an unbuildable singleton
            // definition contributes no KNOWN class-method surface, so the chain
            // must read as truncated.
            if entry.singleton_unbuildable {
                complete = false;
            }
            chain.push(key);
            for &module in &entry.extends {
                // `extend M` folds M's INSTANCE methods into this class object,
                // so an M whose instance definition does not build truncates the
                // singleton surface just as an unloaded M does.
                if self
                    .classes
                    .get(module)
                    .is_none_or(|m| m.instance_unbuildable)
                {
                    complete = false;
                }
            }
            cur = entry.superclass;
        }
        let found = self.singleton_on_chain(&chain, method, 0);
        (found, complete)
    }

    /// Whether `method` is on the class object's surface across the singleton
    /// `chain`: a direct `def self.x` on any class, an INSTANCE method of any
    /// `extend`ed module, or a singleton ALIAS resolving (bounded) to one of
    /// those. Existence-only; completeness is computed by the caller.
    fn singleton_on_chain(&self, chain: &[&'static str], method: &str, depth: usize) -> bool {
        // (1) A direct singleton method on any class in the chain — including
        //     one generated by a SINGLETON attribute member (`attr_reader
        //     self.x`), S1.
        for &anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if entry.singleton_methods.contains_key(method)
                    || entry.singleton_attr_methods.contains(method)
                {
                    return true;
                }
            }
        }
        // (2) An `extend`ed module's INSTANCE method (extend folds M's instance
        //     methods into the class object's singleton surface).
        for &anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                for &module in &entry.extends {
                    if self.classes.contains_key(module) {
                        let (mod_chain, _) = self.ancestors(module);
                        if self.lookup_on_chain(&mod_chain, method).is_some()
                            || self.attr_on_chain(&mod_chain, method)
                        {
                            return true;
                        }
                    }
                }
            }
        }
        // (3) A singleton alias `method -> old`, resolved over the same chain.
        //     Bounded to defend against a pathological alias cycle.
        if depth >= 16 {
            return false;
        }
        for &anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if let Some(&old) = entry.singleton_aliases.get(method) {
                    // The alias target may live on the singleton chain OR on the
                    // base-class surface (`alias self.compile self.new`, where
                    // `new` is `Class#new`), so check both.
                    if self.singleton_on_chain(chain, old, depth + 1)
                        || self.singleton_bases_lookup(old).0
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Resolve a method's return class over the ancestor chain (first defining
    /// ancestor wins), resolving through `alias` definitions. `None` if the
    /// return is not a known concrete class (or the method is unknown).
    pub fn method_return(&self, class_name: &str, method: &str) -> Option<&'static str> {
        // ADR-0042 Slice 5: a namespaced receiver (absent from the short map,
        // present in the qualified registry) routes to qualified resolution; a
        // top-level name never takes this branch (its qualified key IS its
        // short key), so short-key behavior is byte-identical.
        if !self.classes.contains_key(class_name) && self.qualified.contains_key(class_name) {
            return self.qualified_method_return_core(class_name, method).map(|(r, _)| r);
        }
        let (chain, _) = self.ancestors(class_name);
        let ret = self.lookup_on_chain(&chain, method).and_then(|(ret, _, _)| ret);
        self.resolve_call_site_return(ret, class_name)
    }

    /// Resolve the [`SELF_RETURN`] sentinel against the class the lookup was
    /// QUERIED with (the receiver), yielding that class's interned key so the
    /// result round-trips to a Nominal the rules can witness against. A receiver
    /// the index does not model resolves to `None` (⇒ Dynamic), never to the
    /// sentinel string itself. Every other return passes through untouched.
    fn resolve_call_site_return(
        &self,
        ret: Option<&'static str>,
        receiver: &str,
    ) -> Option<&'static str> {
        match ret {
            Some(r) if r == SELF_RETURN => self.classes.get_key_value(receiver).map(|(&k, _)| k),
            other => other,
        }
    }

    /// The structured TUPLE return of `class_name#method` — the descriptor
    /// [`Self::method_return`] cannot carry (a tuple collapses to `None` there).
    /// First-definer-wins over the flattened ancestor chain, the same walk
    /// [`Self::method_return_is_void`] rides; aliases are NOT chased (an
    /// under-emit, never a wrong answer). `None` ⇒ no tuple return ⇒ every
    /// caller behaves exactly as before.
    pub fn method_tuple_return(&self, class_name: &str, method: &str) -> Option<&[RbsReturnShape]> {
        // ADR-0042 Slice 5: see `method_return` — namespaced receivers only.
        if !self.classes.contains_key(class_name) && self.qualified.contains_key(class_name) {
            return self.qualified_method_tuple_return(class_name, method);
        }
        let (chain, _) = self.ancestors(class_name);
        for anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if entry.methods.contains_key(method) {
                    return entry.tuple_returns.get(method).map(|v| v.as_slice());
                }
            }
        }
        None
    }

    /// The singleton twin of [`Self::method_tuple_return`]
    /// (`Process.wait2 -> [Integer, Process::Status]`), walked over the own
    /// superclass chain like [`Self::singleton_method_return`]. Singleton aliases
    /// are not chased (under-emit).
    pub fn singleton_method_tuple_return(
        &self,
        class_name: &str,
        method: &str,
    ) -> Option<&[RbsReturnShape]> {
        // ADR-0042 Slice 5: see `method_return` — namespaced receivers only.
        if !self.classes.contains_key(class_name) && self.qualified.contains_key(class_name) {
            return self.qualified_singleton_tuple_return(class_name, method);
        }
        let mut seen: HashSet<&str> = HashSet::new();
        let mut cur = Some(class_name);
        while let Some(name) = cur {
            let (&key, entry) = self.classes.get_key_value(name)?;
            if !seen.insert(key) {
                return None; // superclass cycle guard.
            }
            if entry.singleton_methods.contains_key(method) {
                return entry.singleton_tuple_returns.get(method).map(|v| v.as_slice());
            }
            cur = entry.superclass;
        }
        None
    }

    /// Every class name reachable as an element of ANY tuple return in the loaded
    /// RBS (instance + singleton, nested tuples included), sorted + deduped.
    ///
    /// The id-registration seam: a tuple element can name a class that never
    /// appears as a constant in the analyzed source (`Process::Status` is reached
    /// only THROUGH `Process.wait2`), so the source-side registry has no identity
    /// to mint a `Nominal` against. Enumerating the closed set here lets the
    /// registry pre-register exactly those names — declaration-driven, with no
    /// name special-casing.
    pub fn tuple_return_class_names(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for entry in self.classes.values() {
            for shapes in entry.tuple_returns.values().chain(entry.singleton_tuple_returns.values())
            {
                for shape in shapes {
                    shape.collect_class_names(&mut out);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The RETURN class of the CLASS method `class_name.method` (M2-GO slice 4)
    /// — the singleton counterpart of [`Self::method_return`], diagnostic-grade:
    /// the stored return is already collapsed under the all-overloads-agree
    /// discipline (class, nil bit AND instance-ness must agree across every
    /// overload, else `None` — so `Regexp.last_match`, whose overloads return
    /// `MatchData?` vs `String?`, declines by construction). An `-> instance`
    /// return resolves LATE-BOUND to the QUERIED class (an inherited
    /// `Date.today -> instance` called as `DateTime.today` yields a DateTime).
    ///
    /// Deliberately narrower than `singleton_return_lookup` (the sig-gen
    /// surface, untouched): only own/inherited `def self.x` resolves; the
    /// `extend`ed-module and base-object (`Class`/`Module`) surfaces stay
    /// untyped — `name`/`to_s` are handled by the caller (C3a Part B) and
    /// anything else declines (FP-safe under-emit).
    pub fn singleton_method_return(&self, class_name: &str, method: &str) -> Option<&'static str> {
        // ADR-0042 Slice 5: see `method_return` — namespaced receivers only.
        if !self.classes.contains_key(class_name) && self.qualified.contains_key(class_name) {
            return self.qualified_singleton_method_return(class_name, method, 0);
        }
        self.singleton_method_return_inner(class_name, method, 0)
    }

    /// Whether `class_name#method`'s author-declared RBS return is `void`
    /// (ADR-100, consumed by `static.value-use.void`). Resolved at the FIRST
    /// ancestor that defines the method — the same first-definer-wins walk
    /// `method_return` rides — so an override that redeclares a non-void
    /// return correctly reads non-void. Aliases are not chased (under-emit).
    pub fn method_return_is_void(&self, class_name: &str, method: &str) -> bool {
        // ADR-0042 Slice 5: see `method_return` — namespaced receivers only.
        if !self.classes.contains_key(class_name) && self.qualified.contains_key(class_name) {
            return self.qualified_method_return_is_void(class_name, method);
        }
        let (chain, _) = self.ancestors(class_name);
        for anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if entry.methods.contains_key(method) {
                    return entry.void_methods.contains(method);
                }
            }
        }
        false
    }

    /// The singleton twin of [`Self::method_return_is_void`]
    /// (`def self.x: () -> void`), walked over the own superclass chain like
    /// [`Self::singleton_method_return`].
    pub fn singleton_method_is_void(&self, class_name: &str, method: &str) -> bool {
        // ADR-0042 Slice 5: see `method_return` — namespaced receivers only.
        if !self.classes.contains_key(class_name) && self.qualified.contains_key(class_name) {
            return self.qualified_singleton_is_void(class_name, method);
        }
        let mut seen: HashSet<&str> = HashSet::new();
        let mut cur = Some(class_name);
        while let Some(name) = cur {
            let Some((&key, entry)) = self.classes.get_key_value(name) else {
                return false;
            };
            if !seen.insert(key) {
                return false;
            }
            if entry.singleton_methods.contains_key(method) {
                return entry.void_singleton_methods.contains(method);
            }
            cur = entry.superclass;
        }
        false
    }

    fn singleton_method_return_inner(
        &self,
        class_name: &str,
        method: &str,
        depth: usize,
    ) -> Option<&'static str> {
        // Alias-chain bound (`alias self.pwd self.getwd` → one hop; a
        // pathological alias cycle terminates here).
        if depth >= 16 {
            return None;
        }
        let (&self_key, _) = self.classes.get_key_value(class_name)?;
        let mut seen: HashSet<&str> = HashSet::new();
        let mut cur = Some(class_name);
        while let Some(name) = cur {
            let Some((&key, entry)) = self.classes.get_key_value(name) else {
                return None; // unknown ancestor: the chain is not fully modeled.
            };
            if !seen.insert(key) {
                return None; // superclass cycle guard.
            }
            if let Some(&(ret, _, is_instance)) = entry.singleton_methods.get(method) {
                return if is_instance { Some(self_key) } else { ret };
            }
            // A singleton ALIAS (`alias self.pwd self.getwd`) resolves through
            // its target on the QUERIED class (instance-binding preserved).
            if let Some(&target) = entry.singleton_aliases.get(method) {
                return self.singleton_method_return_inner(class_name, target, depth + 1);
            }
            cur = entry.superclass;
        }
        None
    }

    /// Resolve `class_name#method` to `(return class, nilable)` over the
    /// ancestor chain — the nil-aware variant of [`Self::method_return`], used
    /// ONLY by `call.possible-nil-receiver`. `nilable` is `true` iff the RBS
    /// return is an `Optional` (`String?` ⇒ `(String, true)`); a plain return
    /// is `(C, false)`. `None` when the return is not a resolvable concrete
    /// class (so the nil-receiver pass never mints `T | nil` from a Dynamic /
    /// unknown return). Nilability rides the same alias resolution as the class
    /// (`String#size -> length` inherits `length`'s nilability).
    pub fn method_return_nilable(
        &self,
        class_name: &str,
        method: &str,
    ) -> Option<(&'static str, bool)> {
        // ADR-0042 Slice 5: see `method_return` — namespaced receivers only.
        if !self.classes.contains_key(class_name) && self.qualified.contains_key(class_name) {
            return self.qualified_method_return_core(class_name, method);
        }
        let (chain, _) = self.ancestors(class_name);
        self.lookup_on_chain(&chain, method)
            .and_then(|(ret, _, nilable)| {
                self.resolve_call_site_return(ret, class_name).map(|c| (c, nilable))
            })
    }

    /// Resolve the RETURN class of `class_name#method` **when called with a
    /// block**, over the flattened ancestor chain — the block-overload return
    /// the reference selects via `block_required: true` (`rbs_dispatch.rb`).
    ///
    /// Returns `Some(class)` only when the method (or an alias of it, e.g.
    /// `Hash#select -> filter`) declares a block-bearing overload whose return
    /// is a resolvable concrete class. A `self`-returning block overload
    /// (`Array#each { } -> self`, `Kernel#tap { } -> self`) resolves to
    /// `class_name` itself (the receiver's own class). `None` ⇒ the block form
    /// isn't precisely modeled (no block overload, or a generic/union/void/
    /// unknown return) ⇒ the caller declines to `Dynamic` (zero-FP).
    pub fn method_return_with_block(&self, class_name: &str, method: &str) -> Option<&'static str> {
        // ADR-0042 Slice 5: see `method_return` — namespaced receivers only.
        if !self.classes.contains_key(class_name) && self.qualified.contains_key(class_name) {
            return self.qualified_method_return_with_block(class_name, method);
        }
        let (chain, _) = self.ancestors(class_name);
        let ret = self.lookup_block_return_on_chain(&chain, method, 0)?;
        if ret == SELF_RETURN {
            // `self` block return ⇒ the receiver type itself. Hand back the
            // receiver's interned `&'static` name (matching the stored key) only
            // when the index actually models the class, so the result
            // round-trips to a Nominal the rules can witness against.
            self.classes.get_key_value(class_name).map(|(&k, _)| k)
        } else {
            Some(ret)
        }
    }

    /// rigor-rs#140 (upstream rigor#1105): the flattened ancestor names of
    /// `class_name` (itself first), or `None` when the class is unknown or the
    /// chain is INCOMPLETE — an unresolvable ancestor must never read as
    /// absent (the same conservative gate [`Self::class_has_method`] applies).
    /// Used by the exactly-once block-timing proof, which asks every ancestor
    /// whether a project reopening patched the method under it.
    pub fn ancestor_names(&self, class_name: &str) -> Option<Vec<&'static str>> {
        if !self.classes.contains_key(class_name) {
            return None;
        }
        let (chain, complete) = self.ancestors(class_name);
        complete.then_some(chain)
    }

    /// rigor-rs#140 (upstream rigor#1105): the FIRST ancestor on `class_name`'s
    /// flattened chain that DIRECTLY declares `method` — its own `methods`
    /// table or an instance `alias` — or `None` when the class is unknown, the
    /// chain is incomplete, or no ancestor declares it. This is the resolved
    /// declaration's OWNER, the fact the reference's `defined_in` check
    /// (`BlockCallTiming.exactly_once_owner?`) gates on: `Array#tap` resolves
    /// to `Kernel`, while a class that overrides `tap` itself reports that
    /// class, never Kernel.
    pub fn declaring_ancestor(&self, class_name: &str, method: &str) -> Option<&'static str> {
        if !self.classes.contains_key(class_name) {
            return None;
        }
        let (chain, complete) = self.ancestors(class_name);
        if !complete {
            return None;
        }
        chain.into_iter().find(|anc| {
            self.classes.get(anc).is_some_and(|entry| {
                entry.methods.contains_key(method) || entry.aliases.contains_key(method)
            })
        })
    }

    /// Collection-shape stage 2b: the class an RBS TOP-LEVEL object constant is
    /// declared to hold (`"ENV"` ⇒ `"RBS::Unnamed::ENVClass"`), or `None`.
    /// See [`Self::object_constants`] for the recording discipline.
    pub fn object_constant_class(&self, name: &str) -> Option<&'static str> {
        self.object_constants.get(name).copied()
    }

    /// The return class of `class_name#method` **when called WITHOUT a block** —
    /// the block-free-overloads-only companion of [`Self::method_return`],
    /// answering only where the flat slot LOSES the return to a divergent block
    /// overload (`String#split: (…) -> Array[String] | (…) { … } -> self`).
    ///
    /// FIRST-DEFINER-WINS, strictly: the walk stops at the first ancestor whose
    /// `methods` table declares `method`, and answers from THAT entry's
    /// `block_free_returns` alone — an override with no divergent block overload
    /// therefore declines instead of inheriting a further ancestor's slot.
    /// Instance `alias`es are resolved on the same chain, exactly like
    /// [`Self::lookup_on_chain_depth`].
    ///
    /// A NAMESPACED receiver absent from the short map declines (`None`): the
    /// qualified registry has no twin for this slot, which is a pure under-emit
    /// (the caller stays Dynamic ⇒ silent), never a wrong answer.
    pub fn method_return_block_free(
        &self,
        class_name: &str,
        method: &str,
    ) -> Option<&'static str> {
        if !self.classes.contains_key(class_name) {
            return None;
        }
        let (chain, _) = self.ancestors(class_name);
        self.lookup_block_free_return_on_chain(&chain, method, 0)
    }

    fn lookup_block_free_return_on_chain(
        &self,
        chain: &[&'static str],
        method: &str,
        depth: usize,
    ) -> Option<&'static str> {
        for anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if entry.methods.contains_key(method) {
                    // First definer: its slot is the only admissible answer.
                    return entry.block_free_returns.get(method).copied();
                }
            }
        }
        if depth >= 16 {
            return None;
        }
        for anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if let Some(&old) = entry.aliases.get(method) {
                    if let Some(found) =
                        self.lookup_block_free_return_on_chain(chain, old, depth + 1)
                    {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    /// The singleton twin of [`Self::method_return_block_free`] (`Dir.glob`,
    /// `Dir.[]`), walked over the own superclass chain like
    /// [`Self::singleton_method_return`]. Singleton aliases resolve through
    /// their target on the QUERIED class. A namespaced receiver declines.
    pub fn singleton_method_return_block_free(
        &self,
        class_name: &str,
        method: &str,
    ) -> Option<&'static str> {
        self.singleton_block_free_inner(class_name, method, 0)
    }

    fn singleton_block_free_inner(
        &self,
        class_name: &str,
        method: &str,
        depth: usize,
    ) -> Option<&'static str> {
        if depth >= 16 {
            return None;
        }
        let mut seen: HashSet<&str> = HashSet::new();
        let mut cur = Some(class_name);
        while let Some(name) = cur {
            let (&key, entry) = self.classes.get_key_value(name)?;
            if !seen.insert(key) {
                return None; // superclass cycle guard.
            }
            if entry.singleton_methods.contains_key(method) {
                return entry.singleton_block_free_returns.get(method).copied();
            }
            if let Some(&target) = entry.singleton_aliases.get(method) {
                return self.singleton_block_free_inner(class_name, target, depth + 1);
            }
            cur = entry.superclass;
        }
        None
    }

    /// Walk the chain for `method`'s block-overload return, resolving instance
    /// `alias`es exactly like [`lookup_on_chain_depth`] (so `Hash#select`, an
    /// `alias select filter`, inherits `filter`'s block return). The first
    /// ancestor that records a block return for `method` wins.
    fn lookup_block_return_on_chain(
        &self,
        chain: &[&'static str],
        method: &str,
        depth: usize,
    ) -> Option<&'static str> {
        for anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if let Some(&ret) = entry.block_returns.get(method) {
                    return Some(ret);
                }
            }
        }
        if depth >= 16 {
            return None;
        }
        for anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if let Some(&old) = entry.aliases.get(method) {
                    if let Some(found) = self.lookup_block_return_on_chain(chain, old, depth + 1) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    // -- ADR-0042 Slice 5: qualified-key routing for the return-lookup family --
    //
    // The existence gates (`class_has_singleton_method`, `class_has_method`)
    // were routed through the qualified registry in Slices 2-3; the RETURN
    // lookups above still opened with the short-key `classes` map, so every
    // namespaced receiver (`Digest::SHA256`) missed to `None` ⇒ Dynamic and the
    // chained call went unwitnessed. Each public member of the family
    // (`method_return`, `method_tuple_return`, `method_return_nilable`,
    // `method_return_with_block`, `method_return_is_void`,
    // `singleton_method_return`, `singleton_method_tuple_return`,
    // `singleton_method_is_void`) now routes a name that is ABSENT from the
    // short map but PRESENT in the qualified registry to the twins below; a
    // top-level name never takes the new branch, so short-key behavior is
    // byte-identical.
    //
    // Name resolution is the FP boundary of this slice and is two-tiered:
    //
    // * A FULL-FIDELITY reference (`superclass_written` / `includes_written`:
    //   the written path + absolute bit + its recorded lexical context)
    //   resolves by RBS's own deterministic rule — innermost context scope
    //   outward, then root — via [`Self::resolve_written_ref`]. No information
    //   was lost, so the first hit IS the reference's answer, not a guess.
    // * A LEAF-ONLY reference (the flat return slots, whose namespace and
    //   absolute bit `method_signature` discarded) resolves via
    //   [`Self::resolve_member_type_ref`]: every scope on the walk that holds
    //   the leaf is a candidate, and anything other than EXACTLY ONE candidate
    //   (unanimous across all recorded member contexts) DECLINES — the leaf
    //   `Class` (∈ {`::Class`, `Digest::Class`}) and leaf `Base`
    //   (∈ {`Random::Base`, `Digest::Base`}) shapes must never resolve by
    //   guess. Declining loses coverage; guessing manufactures FPs.

    /// `TypeNameResolver#resolve_namespace0` for a WRITTEN
    /// (namespace-preserving) type reference against `ctx` (innermost scope
    /// LAST): an absolute `"::X::Y"` looks up its exact key; a relative name
    /// binds its HEAD segment at the first lexical scope — innermost-outward,
    /// then root — holding `scope::head`, then requires each tail segment
    /// under the bound namespace. `None` when nothing binds
    /// (`resolve_namespace0`'s `nil` — the name stays written-relative and
    /// `absolute!` later reads it as `::`-rooted, so the root `written`
    /// lookup is the correct fallback there) — a conservative decline.
    ///
    /// When the binding scope holds `scope::head` (or `cur::seg`) as a KNOWN
    /// non-class/module name — a class/module alias (`aliased_name?` binds,
    /// and the reference then normalizes through the alias target the port
    /// does not record), interface or type alias (`has_type_name?` binds
    /// them too) — the reference resolves through it and this walk cannot
    /// follow. Returning an outer/root namesake instead would mint a type
    /// the reference never produced — a false positive — so those decline
    /// as well (issue #168 review).
    fn resolve_written_ref(&self, written: &str, ctx: &[&'static str]) -> Option<&'static str> {
        if let Some(abs) = written.strip_prefix("::") {
            return self.qualified.get_key_value(abs).map(|(&k, _)| k);
        }
        let mut segs = written.split("::");
        let head = segs.next()?;
        let root_fallback = || self.qualified.get_key_value(written).map(|(&k, _)| k);
        let head_hit = ctx
            .iter()
            .rev()
            .map(|scope| format!("{scope}::{head}"))
            .chain(std::iter::once(head.to_string()))
            .find_map(|cand| {
                if let Some((&k, _)) = self.qualified.get_key_value(cand.as_str()) {
                    Some(Ok(k))
                } else if self.type_names.contains(cand.as_str()) {
                    Some(Err(()))
                } else {
                    None
                }
            });
        let mut cur = match head_hit {
            Some(Ok(k)) => k,
            Some(Err(())) => return None,
            None => return root_fallback(),
        };
        for seg in segs {
            let cand = format!("{cur}::{seg}");
            if let Some((&k, _)) = self.qualified.get_key_value(cand.as_str()) {
                cur = k;
            } else if self.type_names.contains(cand.as_str()) {
                return None;
            } else {
                return root_fallback();
            }
        }
        Some(cur)
    }

    /// Resolve a type reference whose stored spelling the flat slots carried:
    /// a leading `"::"` (absolute / `use`-mapped / `resolve-type-names:
    /// false` root-only — issue #168) looks up its exact key; a leaf/written
    /// path resolves in ONE lexical context, every scope on the
    /// innermost-outward walk (plus the root) that holds the name a
    /// candidate, and the resolution succeeds ONLY when exactly one distinct
    /// candidate exists. Two or more ⇒ the discarded qualifier could have
    /// picked either ⇒ DECLINE (`None`) — never guess.
    fn resolve_leaf_unique(&self, leaf: &str, ctx: &[&'static str]) -> Option<&'static str> {
        if let Some(abs) = leaf.strip_prefix("::") {
            return self.qualified.get_key_value(abs).map(|(&k, _)| k);
        }
        let mut found: Option<&'static str> = None;
        for scope in ctx.iter().rev() {
            let cand = format!("{scope}::{leaf}");
            if let Some((&k, _)) = self.qualified.get_key_value(cand.as_str()) {
                match found {
                    Some(f) if f != k => return None,
                    _ => found = Some(k),
                }
            }
        }
        if let Some((&k, _)) = self.qualified.get_key_value(leaf) {
            match found {
                Some(f) if f != k => return None,
                _ => found = Some(k),
            }
        }
        found
    }

    /// Resolve a member-level type reference (a flat return-class name) of the
    /// entry `definer`, in EVERY member context the entry was ingested under,
    /// adopting only a unanimous answer. A definer with no recorded context
    /// (pre-Slice-5 stub data) or any disagreement declines.
    ///
    /// The per-context resolver is chosen by the STORED name's fidelity
    /// (issue #168):
    ///
    /// * A name a PROJECT signature stores is full-fidelity — `member_name`
    ///   under a `FileSigCtx` keeps the written namespace (`Ns::Impl`,
    ///   `Impl`, `::X`), so it resolves by RBS's own deterministic rule —
    ///   [`Self::resolve_written_ref`]: head segment innermost-outward,
    ///   FIRST hit binds (`Ns::Impl` shadows a root `Impl` written inside
    ///   `module Ns`), a bound head that fails its tail falls back to the
    ///   written path at root (the reference keeps the relative name, which
    ///   `validate_type_name`'s `absolute!` then reads as `::`-rooted).
    /// * A bundled entry's flat LEAF (`Instance`, namespace discarded by
    ///   `type_name_str` at ingest) keeps [`Self::resolve_leaf_unique`]'s
    ///   uniqueness requirement: there first-hit could mint a class the
    ///   discarded qualifier would not have resolved to.
    fn resolve_member_type_ref(&self, definer: &str, name: &str) -> Option<&'static str> {
        let entry = self.qualified.get(definer)?;
        let project = self.qualified_project_sig_classes.contains(definer);
        let mut agreed: Option<&'static str> = None;
        for ctx in &entry.member_ctxs {
            let r = if project {
                self.resolve_written_ref(name, ctx)?
            } else {
                self.resolve_leaf_unique(name, ctx)?
            };
            match agreed {
                Some(prev) if prev != r => return None,
                _ => agreed = Some(r),
            }
        }
        agreed
    }

    /// Whether every `Class` name inside a tuple-return shape list resolves —
    /// unambiguously, unanimously — to ITSELF (the stored written name IS the
    /// qualified key RBS resolution arrives at from `definer`). Only then may
    /// the qualified path hand the stored shapes onward verbatim: the caller
    /// interns element nominals by the literal stored name, so a name that
    /// resolves elsewhere (or not at all) would mint the wrong class ⇒ decline
    /// the whole tuple. `Unknown` elements are fine (they intern to Dynamic).
    fn tuple_shapes_self_resolving(&self, definer: &str, shapes: &[RbsReturnShape]) -> bool {
        shapes.iter().all(|s| match s {
            RbsReturnShape::Class(name) => {
                self.resolve_member_type_ref(definer, name) == Some(name)
            }
            RbsReturnShape::Tuple(inner) => self.tuple_shapes_self_resolving(definer, inner),
            RbsReturnShape::Unknown => true,
        })
    }

    /// The implicit-`Object` superclass default for a QUALIFIED entry (the
    /// qualified map is not Object-defaulted in [`Builder::finish`]), mirroring
    /// [`Self::qualified_class_has_method`]: classes (not modules, not
    /// `BasicObject`) fall back to `Object`.
    fn qualified_default_superclass(&self, key: &'static str, entry: &ClassEntry) -> Option<&'static str> {
        if !entry.is_module && key != "BasicObject" {
            self.qualified.get_key_value("Object").map(|(&k, _)| k)
        } else {
            None
        }
    }

    /// The flattened INSTANCE ancestor chain of a qualified entry — the class
    /// itself, its includes, then its superclass's chain, in the SAME order as
    /// the short-key [`Self::ancestors`] — with every reference resolved
    /// through [`Self::resolve_written_ref`]. The walk STOPS at the first
    /// unresolvable reference or unbuildable entry: everything already pushed
    /// precedes the break in first-definer-wins order, so a hit on the prefix
    /// is genuinely the first definer, while a miss on a truncated chain is
    /// inconclusive (the caller's `None`/`false` fallthrough is the
    /// conservative answer either way).
    fn qualified_ancestors_prefix(&self, qname: &str) -> Vec<&'static str> {
        self.qualified_ancestors(qname).0
    }

    /// [`Self::qualified_ancestors_prefix`] plus the COMPLETENESS bit its
    /// callers discard: `false` when the walk stopped early (an unresolvable
    /// reference or an unbuildable entry), so the chain is a prefix and a MISS
    /// on it is inconclusive. S1's absence witness needs the bit.
    fn qualified_ancestors(&self, qname: &str) -> (Vec<&'static str>, bool) {
        let mut order: Vec<&'static str> = Vec::new();
        let Some((&start, _)) = self.qualified.get_key_value(qname) else {
            return (order, false);
        };
        let mut seen: HashSet<&'static str> = HashSet::new();
        let mut ok = true;
        self.collect_qualified(start, &mut order, &mut seen, &mut ok);
        (order, ok)
    }

    fn collect_qualified(
        &self,
        key: &'static str,
        order: &mut Vec<&'static str>,
        seen: &mut HashSet<&'static str>,
        ok: &mut bool,
    ) {
        if !*ok {
            return;
        }
        let Some(entry) = self.qualified.get(key) else {
            *ok = false;
            return;
        };
        // An unbuildable instance definition has no known surface (its tables
        // were emptied); walking past it could skip the true first definer.
        if entry.instance_unbuildable {
            *ok = false;
            return;
        }
        if !seen.insert(key) {
            return;
        }
        // See `collect`: a PREPENDED module precedes the prepending class.
        for (w, ctx) in &entry.prepends_written {
            match self.resolve_written_ref(w, ctx) {
                Some(m) => self.collect_qualified(m, order, seen, ok),
                None => {
                    *ok = false;
                    return;
                }
            }
            if !*ok {
                return;
            }
        }
        order.push(key);
        for (w, ctx) in &entry.includes_written {
            match self.resolve_written_ref(w, ctx) {
                Some(m) => self.collect_qualified(m, order, seen, ok),
                None => {
                    *ok = false;
                    return;
                }
            }
            if !*ok {
                return;
            }
        }
        let sup = match &entry.superclass_written {
            Some((w, ctx)) => match self.resolve_written_ref(w, ctx) {
                Some(s) => Some(s),
                None => {
                    *ok = false;
                    return;
                }
            },
            None => self.qualified_default_superclass(key, entry),
        };
        if let Some(s) = sup {
            // The reference's `build_instance` raises when a superclass
            // resolves to a MODULE (`class D < Mod` is not a class) — the
            // whole definition collapses to `Dynamic[Top]` there. Issue #168
            // made this reachable: a synthesized `module` namespace stub can
            // sit where a project `< X` points.
            if self.qualified.get(s).is_some_and(|e| e.is_module) {
                *ok = false;
                return;
            }
            self.collect_qualified(s, order, seen, ok);
        }
    }

    /// The singleton (superclass-only) chain of a qualified entry, mirroring
    /// the walk of [`Self::singleton_method_return_inner`], with the same
    /// stop-at-first-unresolvable prefix discipline as
    /// [`Self::qualified_ancestors_prefix`].
    fn qualified_singleton_chain_prefix(&self, qname: &str) -> Vec<&'static str> {
        let mut chain: Vec<&'static str> = Vec::new();
        let Some((&start, _)) = self.qualified.get_key_value(qname) else {
            return chain;
        };
        let mut seen: HashSet<&'static str> = HashSet::new();
        let mut cur = Some(start);
        while let Some(key) = cur {
            let Some(entry) = self.qualified.get(key) else {
                break;
            };
            if entry.singleton_unbuildable || !seen.insert(key) {
                break;
            }
            chain.push(key);
            cur = match &entry.superclass_written {
                Some((w, ctx)) => match self.resolve_written_ref(w, ctx) {
                    Some(s) => Some(s),
                    None => break,
                },
                None => self.qualified_default_superclass(key, entry),
            };
            // `build_singleton` fails the same way on a module superclass —
            // see `collect_qualified`.
            if let Some(s) = cur {
                if self.qualified.get(s).is_some_and(|e| e.is_module) {
                    break;
                }
            }
        }
        chain
    }

    /// First-definer-wins method lookup over a QUALIFIED chain, resolving
    /// instance `alias`es exactly like [`Self::lookup_on_chain_depth`] but over
    /// the qualified registry, and additionally reporting WHICH entry defined
    /// the method (its member contexts resolve the return leaf).
    fn qualified_lookup_on_chain(
        &self,
        chain: &[&'static str],
        method: &str,
        depth: usize,
    ) -> Option<(&'static str, StoredMethodDef)> {
        for &anc in chain {
            if let Some(entry) = self.qualified.get(anc) {
                if let Some(&def) = entry.methods.get(method) {
                    return Some((anc, def));
                }
            }
        }
        if depth >= 16 {
            return None;
        }
        for &anc in chain {
            if let Some(entry) = self.qualified.get(anc) {
                if let Some(&old) = entry.aliases.get(method) {
                    if let Some(found) = self.qualified_lookup_on_chain(chain, old, depth + 1) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    /// The qualified twin of the [`Self::method_return`] /
    /// [`Self::method_return_nilable`] core: `(resolved return key, nilable)`.
    /// [`SELF_RETURN`] resolves against the QUALIFIED receiver (`-> self` on
    /// `Digest::Base#reset` queried as `Digest::SHA256` yields
    /// `Digest::SHA256`); any other leaf resolves through the DEFINER's member
    /// contexts.
    fn qualified_method_return_core(
        &self,
        qname: &str,
        method: &str,
    ) -> Option<(&'static str, bool)> {
        let chain = self.qualified_ancestors_prefix(qname);
        let (definer, (ret, _, nilable)) = self.qualified_lookup_on_chain(&chain, method, 0)?;
        let ret = ret?;
        if ret == SELF_RETURN {
            return self.qualified.get_key_value(qname).map(|(&k, _)| (k, nilable));
        }
        self.resolve_member_type_ref(definer, ret).map(|r| (r, nilable))
    }

    /// The qualified twin of [`Self::method_tuple_return`]: first definer wins
    /// over the qualified chain; shapes pass through only when every element
    /// name is self-resolving (see [`Self::tuple_shapes_self_resolving`]).
    fn qualified_method_tuple_return(
        &self,
        qname: &str,
        method: &str,
    ) -> Option<&[RbsReturnShape]> {
        let chain = self.qualified_ancestors_prefix(qname);
        for anc in chain {
            if let Some(entry) = self.qualified.get(anc) {
                if entry.methods.contains_key(method) {
                    let shapes = entry.tuple_returns.get(method).map(|v| v.as_slice())?;
                    return self.tuple_shapes_self_resolving(anc, shapes).then_some(shapes);
                }
            }
        }
        None
    }

    /// The qualified twin of [`Self::method_return_is_void`].
    fn qualified_method_return_is_void(&self, qname: &str, method: &str) -> bool {
        let chain = self.qualified_ancestors_prefix(qname);
        for anc in chain {
            if let Some(entry) = self.qualified.get(anc) {
                if entry.methods.contains_key(method) {
                    return entry.void_methods.contains(method);
                }
            }
        }
        false
    }

    /// The qualified twin of [`Self::method_return_with_block`], riding
    /// [`Self::qualified_block_return_on_chain`]; a `self` block return yields
    /// the QUALIFIED receiver key.
    fn qualified_method_return_with_block(
        &self,
        qname: &str,
        method: &str,
    ) -> Option<&'static str> {
        let chain = self.qualified_ancestors_prefix(qname);
        let (definer, ret) = self.qualified_block_return_on_chain(&chain, method, 0)?;
        if ret == SELF_RETURN {
            self.qualified.get_key_value(qname).map(|(&k, _)| k)
        } else {
            self.resolve_member_type_ref(definer, ret)
        }
    }

    /// The qualified twin of [`Self::lookup_block_return_on_chain`], reporting
    /// the defining entry alongside the stored block-return name.
    fn qualified_block_return_on_chain(
        &self,
        chain: &[&'static str],
        method: &str,
        depth: usize,
    ) -> Option<(&'static str, &'static str)> {
        for &anc in chain {
            if let Some(entry) = self.qualified.get(anc) {
                if let Some(&ret) = entry.block_returns.get(method) {
                    return Some((anc, ret));
                }
            }
        }
        if depth >= 16 {
            return None;
        }
        for &anc in chain {
            if let Some(entry) = self.qualified.get(anc) {
                if let Some(&old) = entry.aliases.get(method) {
                    if let Some(found) = self.qualified_block_return_on_chain(chain, old, depth + 1)
                    {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    /// The qualified twin of [`Self::singleton_method_return_inner`]: walk the
    /// qualified singleton chain; the first entry defining the method wins. An
    /// `-> instance` return late-binds to the QUALIFIED queried class
    /// (`Digest::Class.file: -> instance` called as `Digest::SHA256.file`
    /// yields `Digest::SHA256`); a concrete leaf resolves through the
    /// DEFINER's member contexts; a singleton alias resolves through its
    /// target on the QUERIED class (instance-binding preserved).
    fn qualified_singleton_method_return(
        &self,
        qname: &str,
        method: &str,
        depth: usize,
    ) -> Option<&'static str> {
        if depth >= 16 {
            return None;
        }
        let (&self_key, _) = self.qualified.get_key_value(qname)?;
        let chain = self.qualified_singleton_chain_prefix(qname);
        for &anc in &chain {
            let entry = self.qualified.get(anc)?;
            if let Some(&(ret, _, is_instance)) = entry.singleton_methods.get(method) {
                return if is_instance {
                    Some(self_key)
                } else {
                    self.resolve_member_type_ref(anc, ret?)
                };
            }
            if let Some(&target) = entry.singleton_aliases.get(method) {
                return self.qualified_singleton_method_return(qname, target, depth + 1);
            }
        }
        None
    }

    /// The qualified twin of [`Self::singleton_method_tuple_return`].
    fn qualified_singleton_tuple_return(
        &self,
        qname: &str,
        method: &str,
    ) -> Option<&[RbsReturnShape]> {
        let chain = self.qualified_singleton_chain_prefix(qname);
        for anc in chain {
            if let Some(entry) = self.qualified.get(anc) {
                if entry.singleton_methods.contains_key(method) {
                    let shapes = entry.singleton_tuple_returns.get(method).map(|v| v.as_slice())?;
                    return self.tuple_shapes_self_resolving(anc, shapes).then_some(shapes);
                }
            }
        }
        None
    }

    /// The qualified twin of [`Self::singleton_method_is_void`].
    fn qualified_singleton_is_void(&self, qname: &str, method: &str) -> bool {
        let chain = self.qualified_singleton_chain_prefix(qname);
        for anc in chain {
            if let Some(entry) = self.qualified.get(anc) {
                if entry.singleton_methods.contains_key(method) {
                    return entry.void_singleton_methods.contains(method);
                }
            }
        }
        false
    }

    // -- Issue #168: project-signature receiver dispatch --------------------

    /// Whether `name` is a type the missing-referenced-type pass SYNTHESIZED
    /// (module/class stubs only — the reference's `synthesized_type_names`,
    /// `names_synthesized_in` over `class_decls`). A receiver typed by one is
    /// `Dynamic[top]` in the reference (`try_synthesized_stub_type`), so every
    /// diagnostic gate must decline it — a synthesized class is empty, but
    /// "empty" is never "proven-absent".
    pub fn is_synthesized_stub(&self, name: &str) -> bool {
        self.synthesized_type_names.contains(name)
    }

    /// The module self-type half of [`Self::project_sig_chain_ok`]: whether
    /// the WRITTEN name resolves to a definition the reference's
    /// `build_instance`/`build_interface` accepts. Resolution follows
    /// `TypeNameResolver` — the (already `use`-applied) spelling's head binds
    /// innermost→outer→root, then each tail segment under the bound
    /// namespace, every segment binding on `has_type_name? ||
    /// aliased_name?` — i.e. `type_names`, which holds both sets. The bound
    /// name is buildable iff it is a class/module (`qualified` ⇒
    /// `define_instance`) or an interface (a `_` leaf ⇒ `define_interface`);
    /// a type alias or missing name fails the reference's build identically.
    ///
    /// The two reference paths the port cannot reproduce stay declines —
    /// failing the gate only ever loses coverage, it can never admit a name
    /// the reference rejected:
    ///
    /// * a bound CLASS/MODULE-ALIAS segment (capitalized leaf in
    ///   `type_names` but not `qualified`) normalizes through an `old_name`
    ///   target the index never records — the reference may land on a
    ///   buildable decl, but the port cannot know; and
    /// * a bound non-namespace segment mid-path (an interface or type
    ///   alias) dead-ends the tail, so the whole written name stays
    ///   unresolved — the reference keeps it relative and `absolute!` reads
    ///   it `::`-rooted, so the ROOT spelling is retried, never the
    ///   intermediate binding.
    fn self_type_buildable(&self, w: &str, ctx: &[&'static str]) -> bool {
        fn leaf_of(n: &str) -> &str {
            n.rsplit("::").next().unwrap_or(n)
        }
        // The reference's `define_instance`/`define_interface` split: a
        // class/module name is buildable iff `class_decls` holds it
        // (`qualified`); a `_`-leaf bound in `type_names` is an interface
        // decl (nothing else may carry `_`); any other bound leaf — a
        // lowercase type alias or a capitalized class/module alias — is not
        // a buildable definition for a self-type.
        let buildable = |k: &str| {
            self.qualified.contains_key(k) || (leaf_of(k).starts_with('_') && self.type_names.contains(k))
        };
        let alias_hop = |k: &str| {
            !self.qualified.contains_key(k)
                && leaf_of(k).chars().next().is_some_and(|c| c.is_uppercase())
        };
        // An unresolvable relative spelling survives written-relative and
        // `absolute!` roots it — the `::w` retry.
        let root = |w: &str| self.type_names.get(w).is_some_and(|&k| buildable(k));

        if let Some(abs) = w.strip_prefix("::") {
            // Absolute names resolve context-free; a failure leaves the
            // name untouched and `absolute!` is a no-op — no retry.
            let mut segs = abs.split("::").peekable();
            let mut cur: Option<&'static str> = None;
            while let Some(seg) = segs.next() {
                let cand = match cur {
                    Some(c) => format!("{c}::{seg}"),
                    None => seg.to_string(),
                };
                match self.type_names.get(cand.as_str()) {
                    Some(&k) if self.qualified.contains_key(k) => cur = Some(k),
                    Some(&k) if alias_hop(k) => return false,
                    Some(&k) => return segs.peek().is_none() && buildable(k),
                    None => return false,
                }
            }
            return cur.is_some_and(|k| self.qualified.contains_key(k));
        }

        let mut segs = w.split("::").peekable();
        let Some(head) = segs.next() else { return false };
        let mut bound: Option<&'static str> = None;
        for cand in ctx
            .iter()
            .rev()
            .map(|s| format!("{s}::{head}"))
            .chain(std::iter::once(head.to_string()))
        {
            if let Some(&k) = self.type_names.get(cand.as_str()) {
                bound = Some(k);
                break;
            }
        }
        let Some(mut cur) = bound else { return root(w) };
        if alias_hop(cur) {
            return false;
        }
        if !self.qualified.contains_key(cur) {
            return if segs.peek().is_none() {
                buildable(cur)
            } else {
                root(w)
            };
        }
        while let Some(seg) = segs.next() {
            let cand = format!("{cur}::{seg}");
            match self.type_names.get(cand.as_str()) {
                Some(&k) if self.qualified.contains_key(k) => cur = k,
                Some(&k) if alias_hop(k) => return false,
                Some(&k) => {
                    return if segs.peek().is_none() {
                        buildable(k)
                    } else {
                        root(w)
                    };
                }
                None => return root(w),
            }
        }
        buildable(cur)
    }

    /// Whether a class/module name the SOURCE INDEX minted could have its
    /// instance definition built by the reference's `build_instance` — the
    /// chain-completeness gate the RBS return arm rides before trusting a
    /// signature's method table:
    ///
    /// * the qualified ancestor chain must be COMPLETE (an unresolvable
    ///   superclass/include/prepend — or a module where a superclass belongs —
    ///   fails the build wholesale in the reference: the definition is `nil`,
    ///   `Dynamic[top]` everywhere); and
    /// * every module self-type on the chain must resolve to a DECLARED name
    ///   (a `module M : Missing` the stub pass deliberately did NOT
    ///   synthesize — `unresolved_referenced_types` never checks self-type
    ///   NAMES — still fails `build_instance`).
    ///
    /// `initialize`/singleton-side names are deliberately NOT part of the
    /// gate (the reference builds those lazily / skips them the same way).
    pub fn project_sig_chain_ok(&self, qname: &str) -> bool {
        let (chain, complete) = self.qualified_ancestors(qname);
        if !complete {
            return false;
        }
        for anc in chain {
            let Some(entry) = self.qualified.get(anc) else {
                return false;
            };
            for (w, ctx) in &entry.self_types_written {
                if !self.self_type_buildable(w, ctx) {
                    return false;
                }
            }
        }
        true
    }

    /// Whether `class_name` takes the QUALIFIED return path — the issue-#168
    /// member names a project signature stores (`::`-anchored or
    /// context-relative) only resolve through the member-context resolver.
    /// Project-sig names always do; a qualified-only name (absent from
    /// `classes`) does (the ADR-0042 Slice-5 routing); every other name keeps
    /// the legacy short path — byte-identical bundled behaviour.
    fn prefers_qualified_return(&self, class_name: &str) -> bool {
        self.qualified_project_sig_classes.contains(class_name)
            || (!self.classes.contains_key(class_name) && self.qualified.contains_key(class_name))
    }

    /// The `(return class, nilable)` of `class_name#method` for a receiver the
    /// source index typed — [`Self::method_return_nilable`] with the
    /// qualified-preferred routing of [`Self::prefers_qualified_return`]. The
    /// issue-#168 `Foo::Impl` return — a `use`-mapped member name stored
    /// `"::Foo::Impl"` — resolves only through the member contexts this path
    /// reaches.
    pub fn receiver_method_return(
        &self,
        class_name: &str,
        method: &str,
    ) -> Option<(&'static str, bool)> {
        if self.prefers_qualified_return(class_name) {
            self.qualified_method_return_core(class_name, method)
        } else {
            self.method_return_nilable(class_name, method)
        }
    }

    /// The TUPLE twin of [`Self::receiver_method_return`], with project-side
    /// shapes RESOLVED element-by-element through the definer's member
    /// contexts — a `use`-mapped or context-relative element name mints the
    /// class resolution arrives at, and an element resolution cannot pin (a
    /// stub, an interface) degrades to `Unknown` rather than sinking the whole
    /// tuple, exactly as the reference's translator yields `Dynamic[top]` for
    /// that slot.
    pub fn receiver_method_tuple_return(
        &self,
        class_name: &str,
        method: &str,
    ) -> Option<Vec<RbsReturnShape>> {
        if !self.prefers_qualified_return(class_name) {
            return self
                .method_tuple_return(class_name, method)
                .map(<[RbsReturnShape]>::to_vec);
        }
        let chain = self.qualified_ancestors_prefix(class_name);
        for anc in chain {
            let Some(entry) = self.qualified.get(anc) else {
                continue;
            };
            if entry.methods.contains_key(method) {
                let shapes = entry.tuple_returns.get(method)?;
                return Some(self.resolve_return_shapes(anc, shapes));
            }
        }
        None
    }

    /// Resolve each `Class` element of a stored tuple shape list through
    /// `definer`'s member contexts; an element that does not resolve becomes
    /// `Unknown` (the reference's `Dynamic[top]` degrade for that slot).
    fn resolve_return_shapes(&self, definer: &str, shapes: &[RbsReturnShape]) -> Vec<RbsReturnShape> {
        shapes
            .iter()
            .map(|s| match s {
                RbsReturnShape::Class(name) => self
                    .resolve_member_type_ref(definer, name)
                    .map(RbsReturnShape::Class)
                    .unwrap_or(RbsReturnShape::Unknown),
                RbsReturnShape::Tuple(inner) => {
                    RbsReturnShape::Tuple(self.resolve_return_shapes(definer, inner))
                }
                RbsReturnShape::Unknown => RbsReturnShape::Unknown,
            })
            .collect()
    }

    /// The singleton (class-method) twin of [`Self::receiver_method_return`]:
    /// `def self.m` on a project-signature class resolves through the same
    /// qualified-preferred path.
    pub fn receiver_singleton_method_return(
        &self,
        class_name: &str,
        method: &str,
    ) -> Option<&'static str> {
        if self.prefers_qualified_return(class_name) {
            self.qualified_singleton_method_return(class_name, method, 0)
        } else {
            self.singleton_method_return(class_name, method)
        }
    }

    /// The singleton-tuple twin of [`Self::receiver_method_tuple_return`].
    pub fn receiver_singleton_tuple_return(
        &self,
        class_name: &str,
        method: &str,
    ) -> Option<Vec<RbsReturnShape>> {
        if !self.prefers_qualified_return(class_name) {
            return self
                .singleton_method_tuple_return(class_name, method)
                .map(<[RbsReturnShape]>::to_vec);
        }
        let chain = self.qualified_singleton_chain_prefix(class_name);
        for anc in chain {
            let Some(entry) = self.qualified.get(anc) else {
                continue;
            };
            if entry.singleton_methods.contains_key(method) {
                let shapes = entry.singleton_tuple_returns.get(method)?;
                return Some(self.resolve_return_shapes(anc, shapes));
            }
        }
        None
    }

    /// Issue #168: the qualified (and short) class names project `sig/`
    /// introduced — the set the source registry pre-registers so an RBS
    /// return naming one can mint its `Nominal`. Project-sig provenance is
    /// what the dispatch gates (`is_qualified_project_sig_class`) key on, so
    /// synthesized stubs are deliberately NOT here.
    pub fn project_sig_declared_names(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = self
            .project_sig_classes
            .iter()
            .chain(&self.qualified_project_sig_classes)
            .copied()
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Issue #168: the SYNTHESIZED (module/class) stub names — registered for
    /// `Nominal` identity but never flagged declaration-only: a stub receiver
    /// is `Dynamic[top]` in the reference (`try_synthesized_stub_type`), so
    /// the undefined-method gates must stay silent on it.
    pub fn synthesized_stub_names(&self) -> Vec<&'static str> {
        self.synthesized_type_names.iter().copied().collect()
    }

    /// Resolve a method's arity envelope over the ancestor chain (first defining
    /// ancestor wins), resolving through `alias` definitions. `None` if the
    /// method is unknown on the chain — OR if it is known but its envelope is
    /// UNMODELLED (see [`ArityEnvelope`]: a required keyword or trailing
    /// positional in any overload). The two collapse to the same answer on
    /// purpose: both mean "do not arity-check this call", which is exactly what
    /// the reference's `compute_arity_envelope` returning nil means.
    pub fn method_arity(&self, class_name: &str, method: &str) -> Option<Arity> {
        let (chain, _) = self.ancestors(class_name);
        self.lookup_on_chain(&chain, method).and_then(|(_, arity, _)| arity)
    }

    // -- ATM shared-substrate accessors (Slice 1, retention only) --------------
    //
    // These expose the per-overload param shapes + type-alias / interface tables
    // the ingestion now retains. They are wired into NO consumer yet — Slice 2
    // builds the argument-compatibility walk on top and Slice 3 the rule. The
    // slice is output-inert by contract (a ZERO diagnostic-diff gate), so these
    // exist solely to be unit-tested and consumed later.

    /// The per-overload positional-parameter shapes of `class_name#method`,
    /// resolved over the flattened ancestor chain (first defining ancestor wins)
    /// through instance `alias`es — the ATM-substrate twin of [`Self::method_arity`],
    /// but per-overload and per-parameter rather than a merged envelope. `None`
    /// when the method is unknown on the chain (or carries no retained overloads,
    /// which for a real method definition cannot happen — every instance def
    /// records at least one). Each entry is one RBS overload in declaration order.
    pub fn method_overloads(&self, class_name: &str, method: &str) -> Option<&[OverloadSignature]> {
        let (chain, _) = self.ancestors(class_name);
        self.lookup_overloads_on_chain(&chain, method, 0)
    }

    /// The per-overload positional-parameter shapes of the CLASS METHOD
    /// `class_name.method` (`CGI.parse`, `Base64.decode64`), resolved down the
    /// singleton superclass chain — the class-method twin of
    /// [`Self::method_overloads`]. `None` when no ancestor on the singleton chain
    /// records overloads for `method`. Consumed by `call.argument-type-mismatch`
    /// on a `Type::Singleton` receiver.
    pub fn singleton_method_overloads(
        &self,
        class_name: &str,
        method: &str,
    ) -> Option<&[OverloadSignature]> {
        // The singleton class inherits down the SUPERCLASS chain (not the
        // include/ancestor chain). Gather it, then take the first ancestor whose
        // own singleton-overload table records `method`.
        let mut cur = Some(class_name);
        let mut seen: HashSet<&str> = HashSet::new();
        while let Some(name) = cur {
            let Some((&key, entry)) = self.classes.get_key_value(name) else {
                break;
            };
            if !seen.insert(key) {
                break; // cycle guard
            }
            if let Some(ov) = entry.singleton_method_overloads.get(method) {
                return Some(ov.as_slice());
            }
            cur = entry.superclass;
        }
        None
    }

    /// The right-hand side of a `type` alias, one level deep, or `None` if the
    /// alias is unknown. The RHS is RAW: an alias reference INSIDE it stays an
    /// [`RetainedParamType::Alias`] leaf (not expanded). Bounded expansion with a
    /// cycle cap is Slice 2's job — because the RHS is stored one level deep,
    /// ingestion itself can never recurse, so a self- or mutually-referential
    /// alias (`type a = a`) is retained without any risk of a build-time loop.
    pub fn resolve_type_alias(&self, name: &str) -> Option<&RetainedParamType> {
        self.type_alias_defs.get(name.strip_prefix("::").unwrap_or(name))
    }

    /// The declared type-parameter names of a `type` alias (`type range[T] =
    /// ...` ⇒ `["T"]`), or `None` if the alias is unknown. A generic alias USE
    /// (`range[int]`) substitutes these positionally with its own type
    /// arguments — the reference `expand_alias2(name, args)` semantics — which
    /// the translated-describe label applies before rendering the expansion.
    pub fn type_alias_params(&self, name: &str) -> Option<&[&'static str]> {
        self.type_alias_params
            .get(name.strip_prefix("::").unwrap_or(name))
            .map(|v| v.as_slice())
    }

    /// The declared method names of an `interface`, in declaration order, or
    /// `None` if the interface is unknown.
    pub fn interface_methods(&self, name: &str) -> Option<&[&'static str]> {
        self.interface_method_names
            .get(name.strip_prefix("::").unwrap_or(name))
            .map(|v| v.as_slice())
    }

    /// Find `method`'s retained per-overload shapes on the flattened ancestor
    /// `chain`, resolving instance `alias`es exactly like [`Self::lookup_on_chain`]
    /// (bounded against a pathological alias cycle). The first ancestor that
    /// records overloads for `method` directly wins.
    fn lookup_overloads_on_chain(
        &self,
        chain: &[&'static str],
        method: &str,
        depth: usize,
    ) -> Option<&[OverloadSignature]> {
        for anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if let Some(ov) = entry.method_overloads.get(method) {
                    return Some(ov.as_slice());
                }
            }
        }
        if depth >= 16 {
            return None;
        }
        for anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if let Some(&old) = entry.aliases.get(method) {
                    if let Some(found) = self.lookup_overloads_on_chain(chain, old, depth + 1) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    // -- ATM shared-substrate acceptance walk (Slice 2) ------------------------
    //
    // The two argument-compatibility predicates the `call.argument-type-mismatch`
    // rule (Slice 3, not yet wired) decides each parameter with. Both are
    // faithful ports of the reference (`check_rules.rb:2085-2103` /
    // `2157-2213`) with one substitution: the reference hands a translated
    // `ClassInstance` to its acceptance engine (`Inference::Acceptance.accepts`,
    // gradual — refutes only on a proven rejection), where rigor-rs decides the
    // same question with [`Self::class_ordering`] (a `Disjoint` verdict IS the
    // proven rejection; `Superclass`/`Unknown`/`Equal`/`Subclass` all admit).
    // Conservative-TRUE throughout: any shape we cannot decide admits, so the
    // rule never fires on uncertainty. Read by NO consumer in this slice.

    /// Depth cap on bounded `Alias` expansion (defense in depth — Slice 1 stores
    /// each alias RHS one level deep, so ingestion itself cannot loop, but a
    /// mutually-referential alias chain (`type a = b`, `type b = a`) could spin
    /// here without a guard). At exhaustion we admit (conservative true).
    const ALIAS_EXPANSION_CAP: usize = 8;

    /// Does this RBS parameter type admit a `nil` argument? A faithful port of
    /// the reference `rbs_type_admits_nil?` (`check_rules.rb:2085-2103`):
    /// conservative-TRUE by default, so the caller (the nil channel) fires only
    /// on a param that PROVABLY rejects nil.
    ///
    /// - `ClassInstance(name)` ⇒ nil is admitted iff `name` is in the closed
    ///   `NIL_COMPATIBLE` set (`NilClass`/`Object`/`BasicObject`/`Kernel`); a
    ///   concrete class like `String` rejects nil (returns `false`).
    /// - `Alias` ⇒ bounded expansion; an unresolvable alias admits.
    /// - `Interface` ⇒ admitted iff every required method exists on `NilClass`
    ///   (so `_ToStr`/`_ToInt` reject — `NilClass` has no `to_str`/`to_int` —
    ///   while a hypothetical `_ToS` admits, `NilClass#to_s` exists); an
    ///   unresolvable / empty interface admits.
    /// - `Union` ⇒ ANY member admitting admits.
    /// - `Optional` ⇒ `T?` always admits (it explicitly includes nil).
    /// - `Variable` / `Tuple` / `Other` ⇒ every remaining shape (the
    ///   reference's `else`: bases incl. `nil`/`bool`/`void`/`self`/`top`/
    ///   `untyped`, type variables, literals, tuples, records, procs,
    ///   intersections) admits conservatively.
    pub fn param_admits_nil(&self, t: &RetainedParamType) -> bool {
        self.param_admits_nil_depth(t, 0)
    }

    fn param_admits_nil_depth(&self, t: &RetainedParamType, depth: usize) -> bool {
        match t {
            RetainedParamType::ClassInstance(name, _) => {
                let bare = name.strip_prefix("::").unwrap_or(name);
                NIL_COMPATIBLE_CLASS_NAMES.contains(&bare)
            }
            RetainedParamType::Alias(name, _) => {
                if depth >= Self::ALIAS_EXPANSION_CAP {
                    return true;
                }
                match self.resolve_type_alias(name) {
                    // `resolve_type_alias` borrows `self`; clone the small RHS
                    // tag so the recursive `&self` call is not aliased.
                    Some(rhs) => self.param_admits_nil_depth(&rhs.clone(), depth + 1),
                    None => true,
                }
            }
            RetainedParamType::Interface(name, _) => self.interface_admits_nil(name),
            RetainedParamType::Union(members) => {
                members.iter().any(|m| self.param_admits_nil_depth(m, depth))
            }
            RetainedParamType::Optional(_)
            | RetainedParamType::Variable(_)
            | RetainedParamType::Tuple(_)
            | RetainedParamType::Other(_) => true,
        }
    }

    /// An interface parameter admits nil iff `NilClass` implements every method
    /// it requires (the reference `interface_admits_nil?`). An unknown or empty
    /// interface admits (conservative). Uses [`Self::class_has_method`] on
    /// `NilClass`, whose zero-false-positive contract already assumes-present on
    /// an incomplete chain — so absence is witnessed only when `NilClass`'s chain
    /// is fully loaded and genuinely lacks the method.
    fn interface_admits_nil(&self, name: &str) -> bool {
        match self.interface_methods(name) {
            Some(methods) if !methods.is_empty() => {
                methods.iter().all(|m| self.class_has_method("NilClass", m))
            }
            // Unknown or empty interface admits conservatively.
            _ => true,
        }
    }

    /// Does this RBS parameter type accept a (non-nil) argument of class
    /// `arg_class`? A faithful port of the reference `rbs_type_accepts_arg?`
    /// (`check_rules.rb:2157-2213`): conservative-TRUE by default, so the caller
    /// (the non-nil channel) fires only on a param that PROVABLY rejects the
    /// argument's class.
    ///
    /// - `ClassInstance(name)` ⇒ decided by [`Self::class_ordering`]`(arg_class,
    ///   name)`: `Equal`/`Subclass` (the arg is the param class or a descendant)
    ///   accept; `Disjoint` (provably unrelated) is the sole rejection (`false`);
    ///   `Superclass` (the arg is broader — a runtime value MIGHT be the param
    ///   class) and `Unknown` (either class unloaded) admit conservatively.
    /// - `Alias` ⇒ bounded expansion; an unresolvable alias accepts.
    /// - `Interface` ⇒ accepted iff `arg_class` implements every required method
    ///   (mirror of [`Self::interface_admits_nil`], asking the arg class); an
    ///   unresolvable / empty interface, or an arg class not RBS-known, accepts.
    /// - `Union` ⇒ ANY member accepting accepts.
    /// - `Optional` / `Variable` / `Tuple` / `Other` ⇒ accept conservatively
    ///   (the reference `else`).
    pub fn param_accepts_arg_class(&self, t: &RetainedParamType, arg_class: &str) -> bool {
        self.param_accepts_arg_class_depth(t, arg_class, 0)
    }

    fn param_accepts_arg_class_depth(
        &self,
        t: &RetainedParamType,
        arg_class: &str,
        depth: usize,
    ) -> bool {
        match t {
            RetainedParamType::ClassInstance(name, _) => {
                matches!(
                    self.class_ordering(arg_class, name),
                    ClassOrdering::Equal
                        | ClassOrdering::Subclass
                        | ClassOrdering::Superclass
                        | ClassOrdering::Unknown
                )
            }
            RetainedParamType::Alias(name, _) => {
                if depth >= Self::ALIAS_EXPANSION_CAP {
                    return true;
                }
                match self.resolve_type_alias(name) {
                    Some(rhs) => {
                        self.param_accepts_arg_class_depth(&rhs.clone(), arg_class, depth + 1)
                    }
                    None => true,
                }
            }
            RetainedParamType::Interface(name, _) => self.interface_accepts_arg(name, arg_class),
            RetainedParamType::Union(members) => members
                .iter()
                .any(|m| self.param_accepts_arg_class_depth(m, arg_class, depth)),
            RetainedParamType::Optional(_)
            | RetainedParamType::Variable(_)
            | RetainedParamType::Tuple(_)
            | RetainedParamType::Other(_) => true,
        }
    }

    /// An interface parameter accepts `arg_class` iff that class implements every
    /// method the interface requires (the reference `interface_accepts_arg?` /
    /// `arg_class_has_method?`). Conservative on the unknown side: an unresolvable
    /// / empty interface admits, and an arg class NOT RBS-known admits (the class
    /// MIGHT implement the conversion via metaprogramming — the reference returns
    /// true when the class definition is nil). Only a KNOWN arg class that
    /// provably lacks a required method rejects.
    fn interface_accepts_arg(&self, name: &str, arg_class: &str) -> bool {
        match self.interface_methods(name) {
            Some(methods) if !methods.is_empty() => {
                // An arg class not RBS-known might implement the conversion via
                // metaprogramming (the reference returns true on a nil definition).
                if !self.knows_class(arg_class) {
                    return true;
                }
                methods.iter().all(|m| self.class_has_method(arg_class, m))
            }
            // Unknown or empty interface admits conservatively.
            _ => true,
        }
    }

    // -- Sig-gen-only precise declared-return accessors (ADR-14 slice 10) ------
    //
    // These are NOT diagnostic predicates. `class_has_method` /
    // `class_has_singleton_method` deliberately "assume present" on an incomplete
    // ancestor chain (a diagnostic must never witness false absence), which
    // conflates *not declared* with *declared, return unresolvable*. sig-gen's
    // generation-time classification needs those apart: NotDeclared ⇒ emit
    // `# [new]`, Declared(unresolvable) ⇒ silently DROP. The three-valued
    // `Option<Option<&str>>` encoding carries that distinction, and these
    // accessors NEVER assume-present — an incomplete chain with the method absent
    // yields `Some(None)` (the conservative DROP), never `None`.

    /// Whether the flattened ancestor chain of `class` is fully loaded (every
    /// referenced ancestor is in the RBS set). **Sig-gen only.**
    pub fn chain_complete(&self, class: &str) -> bool {
        self.ancestors(class).1
    }

    /// **Sig-gen only — NOT a diagnostic predicate.** Precise three-valued
    /// declared INSTANCE-return lookup over the ancestor chain:
    /// - `None` ⇒ the method is not declared anywhere on a COMPLETE chain
    ///   (⇒ sig-gen emits `# [new]`);
    /// - `Some(None)` ⇒ declared (or the chain is incomplete, so a declaration
    ///   may exist upstream) but the return is not a single bare concrete class
    ///   (⇒ sig-gen DROPs, conservatively);
    /// - `Some(Some(c))` ⇒ declared, resolvable return class `c`.
    ///
    /// The return-class resolution is exactly [`Self::method_return`]'s (a single
    /// bare concrete `ClassInstanceType` across all overloads, else `None`).
    pub fn declared_instance_return(&self, class: &str, method: &str) -> Option<Option<&'static str>> {
        if !self.classes.contains_key(class) {
            return None;
        }
        let (chain, complete) = self.ancestors(class);
        match self.lookup_on_chain(&chain, method) {
            Some((ret, _, _)) => Some(self.resolve_call_site_return(ret, class)),
            None if complete => None,
            None => Some(None),
        }
    }

    /// **Sig-gen only — NOT a diagnostic predicate.** The singleton counterpart
    /// of [`Self::declared_instance_return`], over the same surface
    /// [`Self::class_has_singleton_method`] checks: own `def self.x` up the
    /// superclass chain, every `extend`ed module's INSTANCE methods, and the
    /// INSTANCE methods of the five base classes (`Class`/`Module`/`Object`/
    /// `Kernel`/`BasicObject`) the class object is itself an instance of. A
    /// singleton ALIAS resolves as `Some(None)` (declared, return unresolved ⇒
    /// DROP) rather than being missed.
    pub fn declared_singleton_return(&self, class: &str, method: &str) -> Option<Option<&'static str>> {
        if !self.classes.contains_key(class) {
            return None;
        }
        // (a) own singleton methods up the superclass chain + extends' instance.
        let (ret_a, found_a, complete_a) = self.singleton_return_lookup(class, method);
        if found_a {
            return Some(ret_a);
        }
        // (b) the class object's own ancestry (it is a `Class`): the instance
        //     surface of the five base classes.
        let (ret_b, found_b, bases_loaded) = self.singleton_bases_return(method);
        if found_b {
            return Some(ret_b);
        }
        // Not found anywhere: a precise NotDeclared only when the whole surface
        // is known; otherwise the conservative DROP.
        if complete_a && bases_loaded {
            None
        } else {
            Some(None)
        }
    }

    /// Walk `class_name`'s singleton superclass chain resolving the return of the
    /// first `def self.x` / extended-module-instance / singleton-alias match.
    /// Returns `(return, found, complete)`; `complete` is `false` when a
    /// referenced superclass or extended module is not loaded. An alias match
    /// yields `(None, true, _)` (declared, unresolved ⇒ DROP).
    fn singleton_return_lookup(
        &self,
        class_name: &str,
        method: &str,
    ) -> (Option<&'static str>, bool, bool) {
        let mut chain: Vec<&'static str> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut complete = true;
        let mut cur = Some(class_name);
        while let Some(name) = cur {
            let Some((&key, entry)) = self.classes.get_key_value(name) else {
                complete = false;
                break;
            };
            if !seen.insert(key) {
                break;
            }
            chain.push(key);
            for &module in &entry.extends {
                if !self.classes.contains_key(module) {
                    complete = false;
                }
            }
            cur = entry.superclass;
        }
        // (1) direct `def self.x` on any class in the chain.
        for &anc in &chain {
            if let Some(entry) = self.classes.get(anc) {
                if let Some(&(ret, _, _)) = entry.singleton_methods.get(method) {
                    return (ret, true, complete);
                }
            }
        }
        // (2) an `extend`ed module's INSTANCE method's return.
        for &anc in &chain {
            if let Some(entry) = self.classes.get(anc) {
                for &module in &entry.extends {
                    if self.classes.contains_key(module) {
                        let (mod_chain, _) = self.ancestors(module);
                        if let Some((ret, _, _)) = self.lookup_on_chain(&mod_chain, method) {
                            // A call-site `instance` return means "an instance
                            // of the receiver" — through an `extend`, the
                            // receiver is the class OBJECT, which this flat slot
                            // cannot spell. Decline to Dynamic rather than
                            // guess.
                            return (drop_call_site_return(ret), true, complete);
                        }
                    }
                }
            }
        }
        // (3) a singleton alias ⇒ declared, but the return is not resolved here
        //     ⇒ DROP (safer than mis-emitting `# [new]`).
        for &anc in &chain {
            if let Some(entry) = self.classes.get(anc) {
                if entry.singleton_aliases.contains_key(method) {
                    return (None, true, complete);
                }
            }
        }
        (None, false, complete)
    }

    /// The INSTANCE-method return of `method` on the class object's own ancestry
    /// (the five base classes), plus whether all five are loaded — the
    /// return-resolving twin of [`Self::singleton_bases_lookup`].
    fn singleton_bases_return(&self, method: &str) -> (Option<&'static str>, bool, bool) {
        const BASES: [&str; 5] = ["Class", "Module", "Object", "Kernel", "BasicObject"];
        let mut loaded = true;
        for base in BASES {
            if !self.classes.contains_key(base) {
                loaded = false;
                continue;
            }
            let (chain, _) = self.ancestors(base);
            if let Some((ret, _, _)) = self.lookup_on_chain(&chain, method) {
                // Same as the `extend` path: the receiver here is a class
                // object, so a call-site `instance` return declines.
                return (drop_call_site_return(ret), true, loaded);
            }
        }
        (None, false, loaded)
    }

    /// Find `method`'s `(return, arity)` on the flattened ancestor chain,
    /// resolving instance `alias`es. The first ancestor that defines `method`
    /// directly wins; otherwise, if some ancestor aliases `method -> old`, the
    /// lookup re-runs on the **same chain** for `old` (which may itself be an
    /// alias or live on a different ancestor — `String#size -> length`, both on
    /// `String`; an inherited alias resolves to an inherited target too).
    fn lookup_on_chain(
        &self,
        chain: &[&'static str],
        method: &str,
    ) -> Option<(Option<&'static str>, ArityEnvelope, bool)> {
        self.lookup_on_chain_depth(chain, method, 0)
    }

    fn lookup_on_chain_depth(
        &self,
        chain: &[&'static str],
        method: &str,
        depth: usize,
    ) -> Option<(Option<&'static str>, ArityEnvelope, bool)> {
        // A direct definition anywhere on the chain wins.
        for anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if let Some(&def) = entry.methods.get(method) {
                    return Some(def);
                }
            }
        }
        // Else: follow the first alias for `method` found on the chain. Bound
        // the recursion to defend against a pathological alias cycle in RBS.
        if depth >= 16 {
            return None;
        }
        for anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if let Some(&old) = entry.aliases.get(method) {
                    if let Some(found) = self.lookup_on_chain_depth(chain, old, depth + 1) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    /// Compute the flattened ancestor chain for `class_name`: the class itself,
    /// then its included modules, then its superclass's chain, recursively.
    /// Returns `(chain, complete)` where `complete` is `false` if any ancestor
    /// name referenced along the way is NOT in the loaded set (so absence must
    /// not be witnessed).
    fn ancestors(&self, class_name: &str) -> (Vec<&'static str>, bool) {
        let mut order: Vec<&'static str> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut complete = true;
        self.collect(class_name, &mut order, &mut seen, &mut complete);
        (order, complete)
    }

    fn collect(
        &self,
        name: &str,
        order: &mut Vec<&'static str>,
        seen: &mut HashSet<&'static str>,
        complete: &mut bool,
    ) {
        let Some((&key, entry)) = self.classes.get_key_value(name) else {
            // Referenced ancestor not loaded ⇒ chain incomplete.
            *complete = false;
            return;
        };
        if !seen.insert(key) {
            return;
        }
        // A class whose INSTANCE definition the reference cannot build has NO
        // known instance surface (its tables were emptied), so a chain passing
        // through it is incomplete — otherwise the empty entry would read as
        // "definitively has nothing" and witness absence both on the class itself
        // and on anything inheriting from or including it. This walk is the
        // instance-side chain, so only the instance flag applies.
        if entry.instance_unbuildable {
            *complete = false;
        }
        // PREPENDED modules sit AHEAD of the class in Ruby's MRO, so they are
        // pushed before it — a `prepend`ed definition wins first-definer-wins
        // over the class's own. (`seen` already holds `key`, so a module that
        // prepends its own prepender terminates.)
        for pre in &entry.prepends {
            self.collect(pre, order, seen, complete);
        }
        order.push(key);
        // Included modules sit between the class and its superclass in Ruby's
        // method resolution order; for *existence* the order doesn't matter.
        for inc in &entry.includes {
            self.collect(inc, order, seen, complete);
        }
        if let Some(sup) = entry.superclass {
            self.collect(sup, order, seen, complete);
        }
    }

    /// Hardcoded fallback used when the core RBS directory is unavailable. The
    /// chains here mirror the real ancestry (Object/BasicObject/Kernel/…) so the
    /// conservative gate behaves the same, just over fewer methods.
    fn stub() -> Self {
        let mut classes: HashMap<&'static str, ClassEntry> = HashMap::new();

        let mut put = |name: &'static str,
                       superclass: Option<&'static str>,
                       includes: Vec<&'static str>,
                       methods: &[(&'static str, Option<&'static str>, Arity)]| {
            let mut m: HashMap<&'static str, (Option<&'static str>, ArityEnvelope, bool)> =
                HashMap::new();
            for (n, ret, ar) in methods {
                // The stub never models nilable returns (no `?` shapes here) ⇒
                // `false` keeps the fallback conservative: nothing mints
                // `T | nil`, so `possible-nil-receiver` stays silent under it.
                // Every stub entry is arity-ELIGIBLE (the stub declares no
                // keyword parameters at all), so the envelope is always `Some`.
                m.insert(n, (*ret, Some(*ar), false));
            }
            classes.insert(
                name,
                ClassEntry {
                    methods: m,
                    // The stub declares no `-> void` returns.
                    void_methods: HashSet::new(),
                    void_singleton_methods: HashSet::new(),
                    // The stub declares everything public.
                    private_methods: HashSet::new(),
                    // The stub declares no attribute members.
                    attr_methods: HashSet::new(),
                    singleton_attr_methods: HashSet::new(),
                    // The stub models no per-overload param shapes (the ATM
                    // substrate needs the real embedded RBS); empty keeps the
                    // `method_overloads` accessor inert under the fallback.
                    method_overloads: HashMap::new(),
                    // The stub declares no tuple returns either, so the
                    // structured return descriptor is inert under the fallback.
                    tuple_returns: HashMap::new(),
                    singleton_tuple_returns: HashMap::new(),
                    // The stub doesn't model block-form returns (no block-call
                    // result typing under the fallback); empty keeps it
                    // conservative ⇒ a block-bearing call stays Dynamic/silent.
                    block_returns: HashMap::new(),
                    // Likewise no divergent block/block-free overload pairs, so
                    // the block-free return slots stay empty (⇒ the flat return
                    // is the only answer under the fallback).
                    block_free_returns: HashMap::new(),
                    singleton_block_free_returns: HashMap::new(),
                    // The stub lists `size` directly alongside `length`, so it
                    // needs no alias table; real RBS uses `alias size length`.
                    aliases: HashMap::new(),
                    // The stub doesn't model singleton methods (no class-method
                    // typo detection under the fallback); the surface gate stays
                    // conservative because the five base classes' singleton
                    // surface is incomplete here ⇒ always silent.
                    singleton_methods: HashMap::new(),
                    singleton_method_overloads: HashMap::new(),
                    overloading_method_overloads: Vec::new(),
                    overloading_singleton_overloads: Vec::new(),
                    singleton_aliases: HashMap::new(),
                    superclass,
                    // The stub carries no written references or member contexts
                    // (the qualified return-lookup path needs the real embedded
                    // RBS); empty keeps that path inert under the fallback.
                    superclass_written: None,
                    // The stub records no member-level type references (the
                    // missing-type stub pass needs the real embedded RBS).
                    referenced_type_names: Vec::new(),
                    includes_written: Vec::new(),
                    // The stub models no `prepend` directives.
                    prepends: Vec::new(),
                    prepends_written: Vec::new(),
                    // The stub declares no module self types.
                    self_types_written: Vec::new(),
                    member_ctxs: Vec::new(),
                    // The stub does not distinguish modules from classes; the
                    // raise-non-exception module gate needs the real embedded RBS
                    // (Exception is absent under the stub, so the rule is silent
                    // regardless). `false` keeps it inert.
                    is_module: false,
                    includes,
                    // The stub models no `extend` directives (no class-method
                    // surface under the fallback); empty keeps it conservative.
                    extends: Vec::new(),
                    // The stub carries none of the `UNBUILDABLE_DEFINITIONS`
                    // classes, so the flags are inert under the fallback.
                    instance_unbuildable: false,
                    singleton_unbuildable: false,
                },
            );
        };

        put("BasicObject", None, vec![], &[("==", Some("TrueClass"), (1, Some(1)))]);
        put(
            "Kernel",
            None,
            vec![],
            &[
                ("class", None, (0, Some(0))),
                ("frozen?", Some("TrueClass"), (0, Some(0))),
                ("tap", None, (0, Some(0))),
                ("inspect", Some("String"), (0, Some(0))),
                ("is_a?", Some("TrueClass"), (1, Some(1))),
                ("nil?", Some("FalseClass"), (0, Some(0))),
                ("to_s", Some("String"), (0, Some(0))),
            ],
        );
        put("Object", Some("BasicObject"), vec!["Kernel"], &[]);
        put(
            "Comparable",
            None,
            vec![],
            &[
                ("<", Some("TrueClass"), (1, Some(1))),
                (">", Some("TrueClass"), (1, Some(1))),
                ("<=", Some("TrueClass"), (1, Some(1))),
                (">=", Some("TrueClass"), (1, Some(1))),
            ],
        );
        put(
            "Numeric",
            Some("Object"),
            vec!["Comparable"],
            &[
                ("+", None, (1, Some(1))),
                ("-", None, (1, Some(1))),
                ("*", None, (1, Some(1))),
                ("abs", None, (0, Some(0))),
                ("zero?", Some("TrueClass"), (0, Some(0))),
            ],
        );
        put(
            "Integer",
            Some("Numeric"),
            vec![],
            &[
                ("+", Some("Integer"), (1, Some(1))),
                ("-", Some("Integer"), (1, Some(1))),
                ("*", Some("Integer"), (1, Some(1))),
                ("abs", Some("Integer"), (0, Some(0))),
                ("succ", Some("Integer"), (0, Some(0))),
                ("pred", Some("Integer"), (0, Some(0))),
                ("to_s", Some("String"), (0, Some(1))),
                ("even?", Some("TrueClass"), (0, Some(0))),
                ("odd?", Some("TrueClass"), (0, Some(0))),
                ("times", None, (0, Some(0))),
            ],
        );
        put(
            "Float",
            Some("Numeric"),
            vec![],
            &[
                ("+", Some("Float"), (1, Some(1))),
                ("-", Some("Float"), (1, Some(1))),
                ("*", Some("Float"), (1, Some(1))),
                ("abs", Some("Float"), (0, Some(0))),
                ("round", Some("Integer"), (0, Some(1))),
                ("ceil", Some("Integer"), (0, Some(1))),
                ("floor", Some("Integer"), (0, Some(1))),
                ("to_i", Some("Integer"), (0, Some(0))),
                ("to_s", Some("String"), (0, Some(0))),
                ("nan?", Some("TrueClass"), (0, Some(0))),
            ],
        );
        put(
            "String",
            Some("Object"),
            vec!["Comparable"],
            &[
                ("length", Some("Integer"), (0, Some(0))),
                ("size", Some("Integer"), (0, Some(0))),
                ("upcase", Some("String"), (0, Some(0))),
                ("downcase", Some("String"), (0, Some(0))),
                ("capitalize", Some("String"), (0, Some(0))),
                ("reverse", Some("String"), (0, Some(0))),
                ("strip", Some("String"), (0, Some(0))),
                ("chomp", Some("String"), (0, Some(1))),
                ("to_s", Some("String"), (0, Some(0))),
                ("to_str", Some("String"), (0, Some(0))),
                ("to_sym", Some("Symbol"), (0, Some(0))),
                ("to_i", Some("Integer"), (0, Some(1))),
                ("to_f", Some("Float"), (0, Some(0))),
                ("index", Some("Integer"), (1, Some(2))),
                ("gsub", Some("String"), (1, Some(2))),
                ("sub", Some("String"), (1, Some(2))),
                ("split", Some("Array"), (0, Some(2))),
                ("include?", Some("TrueClass"), (1, Some(1))),
                ("start_with?", Some("TrueClass"), (0, None)),
                ("end_with?", Some("TrueClass"), (0, None)),
                ("empty?", Some("TrueClass"), (0, Some(0))),
                ("rjust", Some("String"), (1, Some(2))),
                ("ljust", Some("String"), (1, Some(2))),
                ("center", Some("String"), (1, Some(2))),
                ("+", Some("String"), (1, Some(1))),
                ("*", Some("String"), (1, Some(1))),
            ],
        );
        put(
            "Symbol",
            Some("Object"),
            vec!["Comparable"],
            &[
                ("to_s", Some("String"), (0, Some(0))),
                ("to_sym", Some("Symbol"), (0, Some(0))),
                ("to_proc", None, (0, Some(0))),
                ("length", Some("Integer"), (0, Some(0))),
                ("size", Some("Integer"), (0, Some(0))),
                ("upcase", Some("Symbol"), (0, Some(0))),
                ("downcase", Some("Symbol"), (0, Some(0))),
            ],
        );
        put(
            "Enumerable",
            None,
            vec![],
            &[
                ("map", Some("Array"), (0, Some(0))),
                ("each", None, (0, Some(0))),
                ("select", Some("Array"), (0, Some(0))),
                ("reject", Some("Array"), (0, Some(0))),
                ("include?", Some("TrueClass"), (1, Some(1))),
                ("to_a", Some("Array"), (0, Some(0))),
                ("first", None, (0, Some(1))),
            ],
        );
        put(
            "Array",
            Some("Object"),
            vec!["Enumerable"],
            &[
                ("length", Some("Integer"), (0, Some(0))),
                ("size", Some("Integer"), (0, Some(0))),
                ("push", None, (0, None)),
                ("pop", None, (0, Some(1))),
                ("shift", None, (0, Some(1))),
                ("unshift", None, (0, None)),
                ("reverse", Some("Array"), (0, Some(0))),
                ("sort", Some("Array"), (0, Some(0))),
                ("join", Some("String"), (0, Some(1))),
                ("empty?", Some("TrueClass"), (0, Some(0))),
                ("+", Some("Array"), (1, Some(1))),
                ("<<", Some("Array"), (1, Some(1))),
            ],
        );
        put(
            "Hash",
            Some("Object"),
            vec!["Enumerable"],
            &[
                ("length", Some("Integer"), (0, Some(0))),
                ("size", Some("Integer"), (0, Some(0))),
                ("keys", Some("Array"), (0, Some(0))),
                ("values", Some("Array"), (0, Some(0))),
                ("fetch", None, (1, Some(2))),
                ("store", None, (2, Some(2))),
                ("merge", Some("Hash"), (0, None)),
                ("empty?", Some("TrueClass"), (0, Some(0))),
                ("key?", Some("TrueClass"), (1, Some(1))),
                ("to_h", Some("Hash"), (0, Some(0))),
            ],
        );
        put(
            "NilClass",
            Some("Object"),
            vec![],
            &[
                ("to_s", Some("String"), (0, Some(0))),
                ("to_a", Some("Array"), (0, Some(0))),
                ("to_h", Some("Hash"), (0, Some(0))),
                ("to_i", Some("Integer"), (0, Some(0))),
                ("nil?", Some("TrueClass"), (0, Some(0))),
                ("inspect", Some("String"), (0, Some(0))),
            ],
        );
        put(
            "TrueClass",
            Some("Object"),
            vec![],
            &[
                ("to_s", Some("String"), (0, Some(0))),
                ("&", Some("TrueClass"), (1, Some(1))),
                ("|", Some("TrueClass"), (1, Some(1))),
                ("^", Some("TrueClass"), (1, Some(1))),
            ],
        );
        put(
            "FalseClass",
            Some("Object"),
            vec![],
            &[
                ("to_s", Some("String"), (0, Some(0))),
                ("&", Some("FalseClass"), (1, Some(1))),
                ("|", Some("TrueClass"), (1, Some(1))),
                ("^", Some("TrueClass"), (1, Some(1))),
            ],
        );

        // Every curated stub name is a genuine top-level core class, so the
        // whole key set is the top-level set (defect 2: `knows_toplevel_class`
        // of a curated class is `true`, of an unknown name is `false`).
        let toplevel_classes: HashSet<&'static str> = classes.keys().copied().collect();
        Self {
            source: RbsSource::Stub,
            classes,
            toplevel_classes,
            project_sig_classes: HashSet::new(),
            qualified_project_sig_classes: HashSet::new(),
            // The stub models no type aliases or interfaces (the ATM substrate
            // needs the real embedded RBS); empty keeps the accessors inert.
            type_alias_defs: HashMap::new(),
            type_alias_params: HashMap::new(),
            interface_method_names: HashMap::new(),
            // ADR-0042 Slice 1: the stub models no qualified registry either —
            // empty keeps the new accessors conservatively inert (never
            // falsely `true`), same as the other ATM-substrate maps above.
            qualified: HashMap::new(),
            short_to_qualified: HashMap::new(),
            // The stub carries no RBS object-constant declarations.
            object_constants: HashMap::new(),
            // The stub models no wider known-type set and synthesizes no
            // missing-type stubs.
            type_names: HashSet::new(),
            synthesized_type_names: HashSet::new(),
            conformance: conformance::ConformanceData::default(),
        }
    }

    /// The signature source this data was built from (embedded / override /
    /// stub) — surfaced by `rigor doctor` (audit-R1).
    pub fn source(&self) -> &RbsSource {
        &self.source
    }

    /// Whether `class_name` was INTRODUCED by project-`sig/` ingestion (ADR-0033)
    /// — declared in the project's own signatures and not already carried by a
    /// bundled (core/stdlib/plugin) RBS. The dispatch rules use this to witness an
    /// `X.new` instance-method typo on a project-authored class while staying
    /// lenient on a bundled stdlib/gem class. Always `false` when no `sig/` was
    /// ingested.
    pub fn is_project_sig_class(&self, class_name: &str) -> bool {
        self.project_sig_classes.contains(class_name)
    }

    /// ADR-0042 Slice 4: whether the QUALIFIED name `qname` (`Outer::Inner`)
    /// was INTRODUCED by project `sig/` ingestion — the qualified twin of
    /// [`Self::is_project_sig_class`], so a nested project-sig class's `.new`
    /// typo witnesses through the qualified path.
    pub fn is_qualified_project_sig_class(&self, qname: &str) -> bool {
        self.qualified_project_sig_classes.contains(qname)
    }

    /// How many distinct classes the loaded RBS surface registered. A coarse
    /// coverage signal for `rigor doctor`.
    pub fn class_count(&self) -> usize {
        self.classes.len()
    }
}

/// What [`Builder::finish`] hands back: the per-class map, the genuine-top-level
/// name set, and the two ATM-substrate global tables (type aliases + interface
/// method names). A named alias keeps the `finish` signature readable
/// (`clippy::type_complexity`).
type BuiltData = (
    HashMap<&'static str, ClassEntry>,
    HashSet<&'static str>,
    HashMap<&'static str, RetainedParamType>,
    HashMap<&'static str, Vec<&'static str>>,
    HashMap<&'static str, Vec<&'static str>>,
    HashMap<&'static str, ClassEntry>,
    HashMap<&'static str, Vec<&'static str>>,
    HashMap<&'static str, &'static str>,
    HashSet<&'static str>,
    HashSet<&'static str>,
);

/// Accumulates parsed RBS declarations into per-class entries before flattening.
#[derive(Default)]
struct Builder {
    /// Issue #129: the `conforms-to` side walk (see [`conformance`]).
    conformance: conformance::ConformanceBuilder,
    classes: HashMap<&'static str, ClassEntry>,
    /// Short names declared at GENUINE top level (empty namespace) in at least
    /// one declaration. Threaded out via [`Self::finish`] into
    /// [`CoreData::toplevel_classes`] (defect 2).
    toplevel_classes: HashSet<&'static str>,
    /// Short names whose superclass has been claimed by a GENUINE top-level
    /// declaration (`!nested && is_toplevel_name`). Because classes are keyed by
    /// SHORT name, a namespaced/nested class (`Psych::Exception < ::RuntimeError`)
    /// otherwise collapses onto a same-short-named top-level class (`Exception`)
    /// and its `< RuntimeError` wins first-write — a superclass CYCLE
    /// (`Exception → RuntimeError → StandardError → Exception`) that makes
    /// `class_ordering` return a spurious `Subclass` in BOTH directions. A
    /// top-level declaration's superclass (even an implicit `Object`, recorded as
    /// `None` here and defaulted in [`Self::finish`]) is authoritative for its
    /// short name; once claimed, a nested twin can no longer overwrite it. This
    /// mirrors the reference's namespace-aware RBS environment without giving up
    /// the deliberate short-name collapse used for method-existence leniency.
    super_claimed: HashSet<&'static str>,
    /// ATM substrate (Slice 1): global `type` alias defs, folded from every
    /// top-level AND nested `type X = ...` declaration. First write wins.
    type_alias_defs: HashMap<&'static str, RetainedParamType>,
    /// ATM substrate: global `type` alias declared type-parameter names
    /// (`type range[T] = ...` ⇒ `"range" → ["T"]`), parallel to
    /// `type_alias_defs` — the substitution names for a generic alias use site.
    type_alias_params: HashMap<&'static str, Vec<&'static str>>,
    /// ATM substrate (Slice 1): global `interface` method-name sets, folded from
    /// every top-level AND nested `interface _X ... end` declaration. First
    /// write wins.
    interface_method_names: HashMap<&'static str, Vec<&'static str>>,
    /// ADR-0042 Slice 1: the NEW qualified-key registry, keyed by
    /// [`qualified_name`] instead of the short leaf. Populated ALONGSIDE
    /// `classes` (every class/module ingest writes to BOTH); PURELY ADDITIVE —
    /// nothing reads this yet except the new `CoreData` accessors, and nothing
    /// existing writes to or reads from it. A qualified key never collides
    /// (each lexical nesting path is unique), so this is a simple union merge
    /// with no `super_claimed`/authoritative cycle-avoidance needed.
    qualified: HashMap<&'static str, ClassEntry>,
    /// ADR-0042 Slice 1: leaf (short) name -> the qualified keys that share it,
    /// in first-seen order (deduplicated). Lets [`CoreData::resolve_short_unambiguous`]
    /// tell an unambiguous short name (exactly one qualified key) from an
    /// ambiguous one (2+, e.g. `ERB::Util` and `CGI::Util` both share the leaf
    /// `"Util"`).
    short_to_qualified: HashMap<&'static str, Vec<&'static str>>,
    /// Collection-shape stage 2b: see [`CoreData::object_constants`].
    object_constants: HashMap<&'static str, &'static str>,
    /// Issue #168: the qualified name of EVERY declaration the env provides —
    /// classes, modules, interfaces, type aliases and class/module aliases —
    /// the `UseMap::Table.known_types` / `TypeNameResolver#all_names`
    /// analogue. Bundled and plugin files populate it through the normal
    /// ingest; project files contribute through the `ingest_project_dirs`
    /// pre-pass because `use Foo::*` expands against the WHOLE loaded set
    /// (the reference builds the table once over every source before
    /// resolving any of them). Synthesized missing-type stubs (the
    /// `stub_missing_referenced_types` pass) join the same set.
    known_type_names: HashSet<&'static str>,
    /// Issue #168: qualified names the missing-referenced-type stub pass
    /// SYNTHESIZED (a subset of `known_type_names` once the pass runs). The
    /// reference keeps exactly this set (`synthesized_type_names`) and treats
    /// a receiver typed by one as `Dynamic[top]` — the dispatch gates need it
    /// to keep a stub from ever witnessing a diagnostic.
    synthesized_type_names: HashSet<&'static str>,
}

impl Builder {
    /// Parse one RBS source and fold its top-level class/module declarations in.
    fn ingest(&mut self, code: &str) {
        let Ok(sig) = parse(code) else {
            return;
        };
        self.ingest_sig(code, &sig, None);
    }

    /// Fold one ALREADY-PARSED signature's top-level declarations in. `ctx` is
    /// the per-file resolution context a PROJECT signature ingests under
    /// (issue #168 — `use` imports and `# resolve-type-names: false`); `None`
    /// is the bundled/plugin path, byte-identical to the previous behaviour.
    fn ingest_sig(&mut self, code: &str, sig: &SignatureNode<'_>, ctx: Option<&FileSigCtx>) {
        self.conformance.walk(code, sig.directives(), sig.declarations());
        for decl in sig.declarations().iter() {
            // `false` = top-level (file-level) declaration: only these may enter
            // the `toplevel_classes` set. `code` is threaded so the ATM substrate
            // can slice verbatim written forms for `RetainedParamType::Other`.
            match decl {
                Node::Class(c) => self.ingest_class(&c, false, &[], code, ctx),
                Node::Module(m) => self.ingest_module(&m, false, &[], code, ctx),
                // Collection-shape stage 2b: a TOP-LEVEL `ENV: …` object
                // constant. The nested dispatch (`collect_members`) deliberately
                // does NOT mirror this — see `CoreData::object_constants`.
                Node::Constant(c) => self.ingest_object_constant(&c, ctx),
                Node::TypeAlias(ta) => self.ingest_type_alias(&ta, code, &[], ctx),
                Node::Interface(i) => self.ingest_interface(&i, &[]),
                Node::ClassAlias(ca) => {
                    // Not modelled as a dispatch surface — but its NEW name is a
                    // known type for `use` maps and the missing-type scan, just
                    // as `class_alias_decls` feeds `known_types` in the reference.
                    self.known_type_names
                        .insert(qualified_name(&[], &ca.new_name()));
                }
                Node::ModuleAlias(ma) => {
                    self.known_type_names
                        .insert(qualified_name(&[], &ma.new_name()));
                }
                _ => {}
            }
        }
    }

    /// Collection-shape stage 2b: record a TOP-LEVEL RBS object-constant
    /// declaration (`ENV: RBS::Unnamed::ENVClass`) as `name -> declared class`.
    ///
    /// Only a bare `ClassInstanceType` right-hand side is recorded — a generic
    /// application keeps just its constructor name (`ARGV: Array[String]` ⇒
    /// `Array`, which is exactly the erasure every other return slot performs).
    /// A literal / optional / union / interface type (`CROSS_COMPILING: true?`)
    /// is skipped, so the map only ever carries a name the class tables can
    /// resolve. First write wins.
    fn ingest_object_constant(&mut self, c: &ruby_rbs::node::ConstantNode, ctx: Option<&FileSigCtx>) {
        let Some(name) = type_name_str(&c.name()) else {
            return;
        };
        // A namespaced declaration name (`Foo::BAR: ...` written at file level)
        // is not a bare constant read at a use site; skip it rather than key the
        // map on a name the typer's `ConstantRead` arm cannot produce.
        if name.contains("::") {
            return;
        }
        let Node::ClassInstanceType(ci) = c.type_() else {
            return;
        };
        let Some(class) = member_name(ctx, &ci.name()) else {
            return;
        };
        // The value feeds `method_return`-family lookups, which key on registry
        // names with no `::` root marker.
        let class = class.strip_prefix("::").unwrap_or(class);
        self.object_constants.entry(name).or_insert(class);
    }

    /// Fold one `type X = ...` alias into the global map (ATM substrate). First
    /// write wins on reopen. The RHS is retained one level deep (aliases inside
    /// are kept as `Alias(..)` leaves, not expanded — Slice 2 owns expansion).
    fn ingest_type_alias(
        &mut self,
        ta: &TypeAliasNode,
        code: &str,
        enclosing: &[&'static str],
        ctx: Option<&FileSigCtx>,
    ) {
        let Some(name) = type_name_str(&ta.name()) else {
            return;
        };
        self.known_type_names.insert(qualified_name(enclosing, &ta.name()));
        // The alias's declared type params (`type range[T] = Range[T] | _Range[T]`)
        // are retained so a USE site `range[int]` can substitute positionally —
        // the reference `expand_alias2(name, args)` semantics — when a label
        // renders the expansion.
        let params: Vec<&'static str> = ta
            .type_params()
            .iter()
            .filter_map(|tp| match tp {
                Node::TypeParam(p) => {
                    let name = p.name();
                    (!name.as_str().is_empty()).then(|| intern(name.as_str()))
                }
                _ => None,
            })
            .collect();
        let rhs = retained_param_type(&ta.type_(), code, ctx);
        self.type_alias_defs.entry(name).or_insert(rhs);
        self.type_alias_params.entry(name).or_insert(params);
    }

    /// Fold one `interface _X ... end` into the global map (ATM substrate),
    /// recording its declared instance-method names in declaration order. First
    /// write wins on reopen.
    fn ingest_interface(&mut self, i: &InterfaceNode, enclosing: &[&'static str]) {
        let Some(name) = type_name_str(&i.name()) else {
            return;
        };
        self.known_type_names.insert(qualified_name(enclosing, &i.name()));
        let mut names: Vec<&'static str> = Vec::new();
        for member in i.members().iter() {
            if let Node::MethodDefinition(md) = member {
                let mname = intern(md.name().as_str());
                if !names.contains(&mname) {
                    names.push(mname);
                }
            }
        }
        self.interface_method_names.entry(name).or_insert(names);
    }

    fn ingest_class(
        &mut self,
        c: &ClassNode,
        nested: bool,
        enclosing: &[&'static str],
        code: &str,
        ctx: Option<&FileSigCtx>,
    ) {
        let tn = c.name();
        let Some(name) = type_name_str(&tn) else {
            return;
        };
        // A genuine top-level decl (`class Time`) is a FILE-LEVEL declaration with
        // an EMPTY namespace. A LEXICALLY NESTED decl (`class Group` written
        // inside `class PrettyPrint`) ALSO has an empty namespace on its own node
        // (nesting is lexical, not embedded in the inner TypeName), so the
        // namespace check alone is insufficient — we must additionally know the
        // decl is file-level (`!nested`). Without this, `PrettyPrint::Group`,
        // `Benchmark::Report`, `Etc::Group` etc. would leak into the top-level set
        // and be wrongly singleton-witnessable (false positives on a project model
        // named `Group`/`Report`). Record only file-level, empty-namespace names.
        let authoritative = !nested && is_toplevel_name(&tn);
        if authoritative {
            self.toplevel_classes.insert(name);
        }
        let superclass = c
            .super_class()
            .and_then(|s| type_name_str(&s.name()));
        // ADR-0042 Slice 5: the superclass reference as WRITTEN, resolved (at
        // lookup time) in the OUTER lexical context — the enclosing chain
        // WITHOUT this class (the reference resolves a super clause in
        // `outer_context`). Issue #168: a project file stores the name as the
        // reference's `resolve_type_names` left it — `use`-mapped /
        // `::`-anchored via `written_ref_ctx`.
        let superclass_written = c
            .super_class()
            .and_then(|s| written_ref_ctx(ctx, &s.name()))
            .map(|w| (w, enclosing.to_vec()));
        // Issue #168: the superclass's type ARGUMENTS are member-level
        // references (`class D < Goo[Missing]`); the super NAME itself fails
        // the definition build instead of reaching the stub pass.
        let mut super_arg_names = Vec::new();
        if let Some(sc) = c.super_class() {
            for arg in sc.args().iter() {
                collect_type_node_names(&arg, ctx, &mut super_arg_names);
            }
        }
        let mut entry = ClassEntry {
            superclass,
            superclass_written,
            ..Default::default()
        };
        // ADR-0042 Slice 1: this decl's own qualified key, and the enclosing
        // context a NESTED decl within its members will qualify against.
        let qual = qualified_name(enclosing, &tn);
        self.known_type_names.insert(qual);
        let child_enclosing: Vec<&'static str> =
            enclosing.iter().copied().chain(std::iter::once(qual)).collect();
        // ADR-0042 Slice 5: members below are ingested under the INNER lexical
        // context (this class included) — record it for member-level type-ref
        // resolution on the qualified path.
        entry.member_ctxs.push(child_enclosing.clone());
        Self::record_ref_names(&mut entry, super_arg_names);
        self.collect_members(c.members().iter(), &mut entry, &child_enclosing, code, ctx);
        self.merge_qualified(qual, entry.clone());
        self.short_to_qualified_push(name, qual);
        self.merge(name, entry, authoritative);
    }

    fn ingest_module(
        &mut self,
        m: &ModuleNode,
        nested: bool,
        enclosing: &[&'static str],
        code: &str,
        ctx: Option<&FileSigCtx>,
    ) {
        let tn = m.name();
        let Some(name) = type_name_str(&tn) else {
            return;
        };
        let authoritative = !nested && is_toplevel_name(&tn);
        if authoritative {
            self.toplevel_classes.insert(name);
        }
        let mut entry = ClassEntry {
            is_module: true,
            ..Default::default()
        };
        // ADR-0042: a module's self-type clause (`module PPMethods :
        // _PPMethodsRequired`) resolves in the OUTER lexical context, exactly
        // like a class's `< X` super clause. Issue #168: `use`-mapped /
        // `::`-anchored forms under a project ctx; the self-type's ARGS are
        // member-level references for the stub pass.
        let mut self_arg_names = Vec::new();
        for st in m.self_types().iter() {
            if let Node::ModuleSelf(ms) = st {
                if let Some(w) = written_ref_ctx(ctx, &ms.name()) {
                    let pair = (w, enclosing.to_vec());
                    if !entry.self_types_written.contains(&pair) {
                        entry.self_types_written.push(pair);
                    }
                }
                for arg in ms.args().iter() {
                    collect_type_node_names(&arg, ctx, &mut self_arg_names);
                }
            }
        }
        let qual = qualified_name(enclosing, &tn);
        self.known_type_names.insert(qual);
        let child_enclosing: Vec<&'static str> =
            enclosing.iter().copied().chain(std::iter::once(qual)).collect();
        // ADR-0042 Slice 5: see `ingest_class` — the inner member context.
        entry.member_ctxs.push(child_enclosing.clone());
        Self::record_ref_names(&mut entry, self_arg_names);
        self.collect_members(m.members().iter(), &mut entry, &child_enclosing, code, ctx);
        self.merge_qualified(qual, entry.clone());
        self.short_to_qualified_push(name, qual);
        self.merge(name, entry, authoritative);
    }

    /// Record one RBS attribute member's generated method name(s) on `entry`.
    /// `reader`/`writer` say which halves the member declares; `kind` routes
    /// them to the instance or the singleton set.
    fn record_attr(
        entry: &mut ClassEntry,
        kind: AttributeKind,
        name: &'static str,
        reader: bool,
        writer: bool,
    ) {
        let set = match kind {
            AttributeKind::Singleton => &mut entry.singleton_attr_methods,
            _ => &mut entry.attr_methods,
        };
        if reader {
            set.insert(name);
        }
        if writer {
            set.insert(intern(&format!("{name}=")));
        }
    }

    /// Issue #168: record the collected member-level type references on
    /// `entry`, deduplicated — see [`ClassEntry::referenced_type_names`].
    fn record_ref_names(entry: &mut ClassEntry, names: impl IntoIterator<Item = &'static str>) {
        for n in names {
            if !entry.referenced_type_names.contains(&n) {
                entry.referenced_type_names.push(n);
            }
        }
    }

    /// ADR-0042 Slice 1: record `qual` under `short`'s qualified-key list,
    /// deduplicated (a reopen ingests the same qualified key more than once).
    fn short_to_qualified_push(&mut self, short: &'static str, qual: &'static str) {
        let list = self.short_to_qualified.entry(short).or_default();
        if !list.contains(&qual) {
            list.push(qual);
        }
    }

    /// Fold method definitions and `include` directives from a member list into
    /// `entry`. Every instance method is recorded regardless of visibility (the
    /// existence check is about dispatch, and a private method still dispatches);
    /// visibility is recorded ASIDE in `private_methods` for the completion
    /// surface, which is the only consumer that must hide it.
    fn collect_members<'a>(
        &mut self,
        members: impl Iterator<Item = Node<'a>>,
        entry: &mut ClassEntry,
        enclosing: &[&'static str],
        code: &str,
        ctx: Option<&FileSigCtx>,
    ) {
        // A bare `private` / `public` member is a SECTION modifier: it applies to
        // every subsequent `def` in this body that does not carry its own
        // visibility. Body-local by construction — a nested class/module recurses
        // through `ingest_class`/`ingest_module`, which start a fresh loop.
        let mut section_private = false;
        for member in members {
            match member {
                Node::MethodDefinition(md) => {
                    let mname = intern(md.name().as_str());
                    let (ret, arity, nilable, ret_instance, ret_self, ret_void) =
                        method_signature(&md, ctx);
                    let block_ret = block_overload_return(&md, ctx);
                    // Collection-shape stage 2a/2c: the return the BLOCK-FREE
                    // overloads alone agree on, recorded only when a block
                    // overload also exists (otherwise it can add nothing the
                    // flat slot does not already carry).
                    let block_free_ret = block_free_overload_return(&md, ctx);
                    // The structured TUPLE return the flat `ret` slot above
                    // collapses to `None` (Slice 2 of the MultiWrite substrate).
                    let tuple_ret = tuple_return(&md, ctx);
                    // An explicit per-`def` visibility overrides the section
                    // modifier in force; `Unspecified` inherits it.
                    let is_private = match md.visibility() {
                        MethodDefinitionVisibility::Private => true,
                        MethodDefinitionVisibility::Public => false,
                        MethodDefinitionVisibility::Unspecified => section_private,
                    };
                    let kind = md.kind();
                    // Issue #168 (`collect_member_references`): an instance-side
                    // method's signature types feed the missing-referenced-type
                    // stub pass — `initialize` and `def self.x` are excluded
                    // (`validate_type_params` never reaches them), `def self?.x`
                    // contributes because its instance side exists.
                    if !matches!(kind, MethodDefinitionKind::Singleton) && mname != "initialize" {
                        let mut names = Vec::new();
                        for ov in md.overloads().iter() {
                            if let Node::MethodDefinitionOverload(ovn) = ov {
                                if let Node::MethodType(mt) = ovn.method_type() {
                                    collect_method_type_names(&mt, ctx, &mut names);
                                }
                            }
                        }
                        Self::record_ref_names(entry, names);
                    }
                    // `def self.x` ⇒ Singleton; `def self?.x` ⇒ SingletonInstance
                    // (BOTH a class method AND an instance method); a plain
                    // `def x` ⇒ Instance. Record into the matching map(s).
                    if matches!(
                        kind,
                        MethodDefinitionKind::Instance
                            | MethodDefinitionKind::SingletonInstance
                    ) {
                        if ret_void && !entry.methods.contains_key(mname) {
                            entry.void_methods.insert(mname);
                        }
                        // Visibility rides the same first-write-wins gate as
                        // `void_methods`: it belongs to the definition being
                        // recorded, so a later reopen must not restate it.
                        if is_private && !entry.methods.contains_key(mname) {
                            entry.private_methods.insert(mname);
                        }
                        if let Some(shapes) = tuple_ret.clone() {
                            entry.tuple_returns.entry(mname).or_insert(shapes);
                        }
                        // `-> instance` on an INSTANCE method means "an instance
                        // of the receiver's class" (rbs 4.1 rewrote several core
                        // returns this way, e.g. `Hash#compact: () -> ::Hash[K,
                        // V]` became `() -> instance`). The declaring class is
                        // the wrong answer under inheritance, so it rides the
                        // call-site sentinel like a `self` block return. The
                        // singleton path keeps its own `ret_instance` flag.
                        let instance_ret = if ret.is_none() && (ret_instance || ret_self) {
                            Some(SELF_RETURN)
                        } else {
                            ret
                        };
                        entry.methods.entry(mname).or_insert((instance_ret, arity, nilable));
                        if let Some(br) = block_ret {
                            entry.block_returns.entry(mname).or_insert(br);
                        }
                        if let Some(bfr) = block_free_ret {
                            entry.block_free_returns.entry(mname).or_insert(bfr);
                        }
                        // ATM substrate (Slice 1): retain the per-overload,
                        // per-parameter shapes the merged arity path discards.
                        // First write wins on reopen — EXCEPT an OVERLOADING
                        // reopen (`def +: (BigDecimal) -> BigDecimal | ...`,
                        // RBS's trailing `...`), whose own overloads are kept
                        // aside and PREPENDED onto the base definition at the
                        // global merge (RBS overloading semantics).
                        if md.overloading() {
                            entry
                                .overloading_method_overloads
                                .push((mname, method_overloads(&md, code, ctx)));
                        } else {
                            entry
                                .method_overloads
                                .entry(mname)
                                .or_insert_with(|| method_overloads(&md, code, ctx));
                        }
                    }
                    if matches!(
                        kind,
                        MethodDefinitionKind::Singleton
                            | MethodDefinitionKind::SingletonInstance
                    ) {
                        if ret_void && !entry.singleton_methods.contains_key(mname) {
                            entry.void_singleton_methods.insert(mname);
                        }
                        if let Some(shapes) = tuple_ret.clone() {
                            entry.singleton_tuple_returns.entry(mname).or_insert(shapes);
                        }
                        entry.singleton_methods.entry(mname).or_insert((ret, arity, ret_instance));
                        if let Some(bfr) = block_free_ret {
                            entry.singleton_block_free_returns.entry(mname).or_insert(bfr);
                        }
                        // ATM substrate: retain the class-method per-overload
                        // shapes so `call.argument-type-mismatch` can check a
                        // `CGI.parse(...)` class-method call site. Overloading
                        // reopens go aside, mirroring the instance path.
                        if md.overloading() {
                            entry
                                .overloading_singleton_overloads
                                .push((mname, method_overloads(&md, code, ctx)));
                        } else {
                            entry
                                .singleton_method_overloads
                                .entry(mname)
                                .or_insert_with(|| method_overloads(&md, code, ctx));
                        }
                    }
                }
                // ATTRIBUTE members (`attr_reader host: String?`). Existence
                // only — see `ClassEntry::attr_methods` for why they are not
                // folded into `methods`. A reader contributes `x`, a writer
                // `x=`, an accessor both; `kind()` splits instance from
                // singleton (`attr_reader self.x`).
                Node::AttrReader(a) => {
                    // Issue #168: a non-singleton attribute's TYPE is a
                    // member-level reference (`kind == :singleton` stands it
                    // down, exactly like `def self.x`).
                    if a.kind() != AttributeKind::Singleton {
                        let mut names = Vec::new();
                        collect_type_node_names(&a.type_(), ctx, &mut names);
                        Self::record_ref_names(entry, names);
                    }
                    let name = intern(a.name().as_str());
                    Self::record_attr(entry, a.kind(), name, true, false);
                }
                Node::AttrWriter(a) => {
                    if a.kind() != AttributeKind::Singleton {
                        let mut names = Vec::new();
                        collect_type_node_names(&a.type_(), ctx, &mut names);
                        Self::record_ref_names(entry, names);
                    }
                    let name = intern(a.name().as_str());
                    Self::record_attr(entry, a.kind(), name, false, true);
                }
                Node::AttrAccessor(a) => {
                    if a.kind() != AttributeKind::Singleton {
                        let mut names = Vec::new();
                        collect_type_node_names(&a.type_(), ctx, &mut names);
                        Self::record_ref_names(entry, names);
                    }
                    let name = intern(a.name().as_str());
                    Self::record_attr(entry, a.kind(), name, true, true);
                }
                Node::Include(inc) => {
                    // Issue #168: the type ARGUMENTS are member references;
                    // the mixin's own name fails the definition build instead
                    // of being stubbed (see `referenced_type_names`).
                    let mut names = Vec::new();
                    for arg in inc.args().iter() {
                        collect_type_node_names(&arg, ctx, &mut names);
                    }
                    Self::record_ref_names(entry, names);
                    if let Some(modname) = type_name_str(&inc.name()) {
                        if !entry.includes.contains(&modname) {
                            entry.includes.push(modname);
                        }
                    }
                    // ADR-0042 Slice 5: the include reference as WRITTEN plus
                    // its INNER lexical context (`enclosing` here already
                    // includes the declaring class — it is the
                    // `child_enclosing` the ingest passed down). Issue #168:
                    // `use`-mapped / `::`-anchored under a project ctx.
                    if let Some(w) = written_ref_ctx(ctx, &inc.name()) {
                        let pair = (w, enclosing.to_vec());
                        if !entry.includes_written.contains(&pair) {
                            entry.includes_written.push(pair);
                        }
                    }
                }
                Node::Prepend(pre) => {
                    // `prepend M` inserts M AHEAD of this class in the MRO. For
                    // method EXISTENCE that is the same contribution as an
                    // `include`; the walks keep the ordering distinction so a
                    // first-definer-wins return lookup stays faithful.
                    let mut names = Vec::new();
                    for arg in pre.args().iter() {
                        collect_type_node_names(&arg, ctx, &mut names);
                    }
                    Self::record_ref_names(entry, names);
                    if let Some(modname) = type_name_str(&pre.name()) {
                        if !entry.prepends.contains(&modname) {
                            entry.prepends.push(modname);
                        }
                    }
                    if let Some(w) = written_ref_ctx(ctx, &pre.name()) {
                        let pair = (w, enclosing.to_vec());
                        if !entry.prepends_written.contains(&pair) {
                            entry.prepends_written.push(pair);
                        }
                    }
                }
                Node::Extend(ext) => {
                    // `extend M` folds M's INSTANCE methods into this class
                    // object's SINGLETON surface (e.g. `SecureRandom extend
                    // Random::Formatter` ⇒ `SecureRandom.hex`). Record the
                    // module name; the singleton lookup resolves it conservatively
                    // (an unknown extended module ⇒ surface incomplete ⇒ silent).
                    let mut names = Vec::new();
                    for arg in ext.args().iter() {
                        collect_type_node_names(&arg, ctx, &mut names);
                    }
                    Self::record_ref_names(entry, names);
                    if let Some(modname) = type_name_str(&ext.name()) {
                        if !entry.extends.contains(&modname) {
                            entry.extends.push(modname);
                        }
                    }
                }
                Node::Alias(a) => {
                    // `alias new old` aliases a method to another. An INSTANCE
                    // alias (`alias size length`) feeds instance dispatch; a
                    // SINGLETON alias (`alias self.pwd self.getwd`) feeds the
                    // class-object surface. Record each into its own map.
                    let new_name = intern(a.new_name().as_str());
                    let old_name = intern(a.old_name().as_str());
                    match a.kind() {
                        AliasKind::Instance => {
                            entry.aliases.entry(new_name).or_insert(old_name);
                        }
                        AliasKind::Singleton => {
                            entry.singleton_aliases.entry(new_name).or_insert(old_name);
                        }
                    }
                }
                // Bare section modifiers. They carry no members of their own —
                // they flip the default visibility for what follows in this body.
                Node::Private(_) => section_private = true,
                Node::Public(_) => section_private = false,
                // A NESTED class/module declaration (e.g. `module PP; module
                // ObjectMixin; end; end`) must be registered too, by its simple
                // name — otherwise an `include` that references it leaves the
                // ancestor chain "incomplete", and the conservative gate would
                // stop witnessing absence for EVERY class whose chain passes
                // through the reopened owner (e.g. `Object include PP::ObjectMixin`
                // ⇒ all typo detection silently disabled). Registering nested
                // types by simple name keeps chains complete. (Simple-name
                // collisions only ever ADD methods, never witness false absence.)
                Node::Class(inner) => self.ingest_class(&inner, true, enclosing, code, ctx),
                Node::Module(inner) => self.ingest_module(&inner, true, enclosing, code, ctx),
                // A NESTED `type X = ...` / `interface _X ... end` folds into the
                // SAME global maps as a top-level one (ATM substrate), keyed by
                // simple name — consistent with how nested classes/modules are
                // registered by simple name above.
                Node::TypeAlias(ta) => self.ingest_type_alias(&ta, code, enclosing, ctx),
                Node::Interface(i) => self.ingest_interface(&i, enclosing),
                _ => {}
            }
        }
    }

    /// Merge an entry into the map (the same class can be reopened across files,
    /// though core mostly isn't). Methods/includes union; an explicit superclass
    /// wins over none.
    ///
    /// `authoritative` is `true` for a GENUINE top-level declaration (empty
    /// namespace, file-level). Such a declaration owns the short name's superclass
    /// identity: the FIRST authoritative write wins and, once made, blocks a
    /// nested/namespaced same-short-name twin from overwriting it — preventing the
    /// short-name-collapse superclass cycles (see [`Builder::super_claimed`]). A
    /// non-authoritative (nested) entry may only fill a still-empty, unclaimed
    /// slot. Methods / includes / singletons still union unconditionally (the
    /// method-existence surface is deliberately the collapsed union).
    fn merge(&mut self, name: &'static str, entry: ClassEntry, authoritative: bool) {
        let claimed = self.super_claimed.contains(name);
        let slot = self.classes.entry(name).or_default();
        if authoritative {
            // The first top-level declaration's superclass (even implicit `Object`,
            // recorded `None` and defaulted in `finish`) is authoritative and may
            // overwrite a value a nested twin set earlier.
            if !claimed {
                slot.superclass = entry.superclass;
            }
        } else if slot.superclass.is_none() && !claimed {
            slot.superclass = entry.superclass;
        }
        slot.is_module |= entry.is_module;
        for (k, v) in entry.methods {
            let newly = !slot.methods.contains_key(k);
            slot.methods.entry(k).or_insert(v);
            if newly && entry.void_methods.contains(k) {
                slot.void_methods.insert(k);
            }
            if newly && entry.private_methods.contains(k) {
                slot.private_methods.insert(k);
            }
        }
        // Tuple returns ride the same first-write-wins reopen discipline as
        // `methods` (they are the same declaration's return, split across two
        // maps).
        for (k, v) in entry.tuple_returns {
            slot.tuple_returns.entry(k).or_insert(v);
        }
        for (k, v) in entry.singleton_tuple_returns {
            slot.singleton_tuple_returns.entry(k).or_insert(v);
        }
        for (k, v) in entry.method_overloads {
            slot.method_overloads.entry(k).or_insert(v);
        }
        // Overloading reopens (`def m: ... | ...`): PREPEND the reopen's own
        // overloads onto the base definition's list (RBS semantics — the
        // reference renders `Integer#+` as `BigDecimal | Integer | Float |
        // Rational | Complex`, bigdecimal's overload first). The vendored load
        // order is core-then-stdlib, so the base is already in the slot; a
        // base-less overloading reopen (no prior definition anywhere) is
        // inserted as-is (degraded, same as the old first-write behavior).
        for (k, v) in entry.overloading_method_overloads {
            match slot.method_overloads.entry(k) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    let base = std::mem::take(e.get_mut());
                    let mut merged = v;
                    merged.extend(base);
                    *e.get_mut() = merged;
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(v);
                }
            }
        }
        for (k, v) in entry.block_returns {
            slot.block_returns.entry(k).or_insert(v);
        }
        for (k, v) in entry.block_free_returns {
            slot.block_free_returns.entry(k).or_insert(v);
        }
        for (k, v) in entry.singleton_block_free_returns {
            slot.singleton_block_free_returns.entry(k).or_insert(v);
        }
        for (k, v) in entry.singleton_methods {
            let newly = !slot.singleton_methods.contains_key(k);
            slot.singleton_methods.entry(k).or_insert(v);
            if newly && entry.void_singleton_methods.contains(k) {
                slot.void_singleton_methods.insert(k);
            }
        }
        for (k, v) in entry.singleton_method_overloads {
            slot.singleton_method_overloads.entry(k).or_insert(v);
        }
        for (k, v) in entry.overloading_singleton_overloads {
            match slot.singleton_method_overloads.entry(k) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    let base = std::mem::take(e.get_mut());
                    let mut merged = v;
                    merged.extend(base);
                    *e.get_mut() = merged;
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(v);
                }
            }
        }
        for (new_name, old_name) in entry.aliases {
            slot.aliases.entry(new_name).or_insert(old_name);
        }
        for (new_name, old_name) in entry.singleton_aliases {
            slot.singleton_aliases.entry(new_name).or_insert(old_name);
        }
        // Attribute members are pure existence, so a reopen simply unions them.
        slot.attr_methods.extend(entry.attr_methods);
        slot.singleton_attr_methods.extend(entry.singleton_attr_methods);
        for name in entry.referenced_type_names {
            if !slot.referenced_type_names.contains(&name) {
                slot.referenced_type_names.push(name);
            }
        }
        for pre in entry.prepends {
            if !slot.prepends.contains(&pre) {
                slot.prepends.push(pre);
            }
        }
        for inc in entry.includes {
            if !slot.includes.contains(&inc) {
                slot.includes.push(inc);
            }
        }
        for ext in entry.extends {
            if !slot.extends.contains(&ext) {
                slot.extends.push(ext);
            }
        }
        // Record the authoritative superclass claim AFTER the `slot` borrow of
        // `self.classes` is done (disjoint field).
        if authoritative {
            self.super_claimed.insert(name);
        }
    }

    /// ADR-0042 Slice 1: union-merge `entry` into `self.qualified` under
    /// `qual`. A simple twin of the union half of [`Self::merge`] — no
    /// `authoritative`/`super_claimed` cycle-avoidance is needed because a
    /// qualified key never collides across distinct classes (each lexical
    /// nesting path is unique), so there is no short-name-collapse superclass
    /// cycle to guard against here.
    fn merge_qualified(&mut self, qual: &'static str, entry: ClassEntry) {
        let slot = self.qualified.entry(qual).or_default();
        if slot.superclass.is_none() {
            slot.superclass = entry.superclass;
            // ADR-0042 Slice 5: the written reference travels WITH the leaf
            // slot (both come from the same `< X` clause), so the two stay
            // first-write-paired.
            slot.superclass_written = entry.superclass_written;
        }
        // ADR-0042 Slice 5: written include refs and member contexts union
        // across reopens, deduped (a reopen re-ingests the same pair).
        for pair in entry.includes_written {
            if !slot.includes_written.contains(&pair) {
                slot.includes_written.push(pair);
            }
        }
        for pair in entry.prepends_written {
            if !slot.prepends_written.contains(&pair) {
                slot.prepends_written.push(pair);
            }
        }
        for pair in entry.self_types_written {
            if !slot.self_types_written.contains(&pair) {
                slot.self_types_written.push(pair);
            }
        }
        for ctx in entry.member_ctxs {
            if !slot.member_ctxs.contains(&ctx) {
                slot.member_ctxs.push(ctx);
            }
        }
        // Issue #168: member-level type references union across reopens,
        // deduped — the missing-referenced-type scan reads them.
        for name in entry.referenced_type_names {
            if !slot.referenced_type_names.contains(&name) {
                slot.referenced_type_names.push(name);
            }
        }
        slot.is_module |= entry.is_module;
        for (k, v) in entry.methods {
            let newly = !slot.methods.contains_key(k);
            slot.methods.entry(k).or_insert(v);
            if newly && entry.void_methods.contains(k) {
                slot.void_methods.insert(k);
            }
            if newly && entry.private_methods.contains(k) {
                slot.private_methods.insert(k);
            }
        }
        // Tuple returns ride the same first-write-wins reopen discipline as
        // `methods` (they are the same declaration's return, split across two
        // maps).
        for (k, v) in entry.tuple_returns {
            slot.tuple_returns.entry(k).or_insert(v);
        }
        for (k, v) in entry.singleton_tuple_returns {
            slot.singleton_tuple_returns.entry(k).or_insert(v);
        }
        for (k, v) in entry.method_overloads {
            slot.method_overloads.entry(k).or_insert(v);
        }
        for (k, v) in entry.overloading_method_overloads {
            match slot.method_overloads.entry(k) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    let base = std::mem::take(e.get_mut());
                    let mut merged = v;
                    merged.extend(base);
                    *e.get_mut() = merged;
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(v);
                }
            }
        }
        for (k, v) in entry.block_returns {
            slot.block_returns.entry(k).or_insert(v);
        }
        for (k, v) in entry.block_free_returns {
            slot.block_free_returns.entry(k).or_insert(v);
        }
        for (k, v) in entry.singleton_block_free_returns {
            slot.singleton_block_free_returns.entry(k).or_insert(v);
        }
        for (k, v) in entry.singleton_methods {
            let newly = !slot.singleton_methods.contains_key(k);
            slot.singleton_methods.entry(k).or_insert(v);
            if newly && entry.void_singleton_methods.contains(k) {
                slot.void_singleton_methods.insert(k);
            }
        }
        for (k, v) in entry.singleton_method_overloads {
            slot.singleton_method_overloads.entry(k).or_insert(v);
        }
        for (k, v) in entry.overloading_singleton_overloads {
            match slot.singleton_method_overloads.entry(k) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    let base = std::mem::take(e.get_mut());
                    let mut merged = v;
                    merged.extend(base);
                    *e.get_mut() = merged;
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(v);
                }
            }
        }
        for (new_name, old_name) in entry.aliases {
            slot.aliases.entry(new_name).or_insert(old_name);
        }
        for (new_name, old_name) in entry.singleton_aliases {
            slot.singleton_aliases.entry(new_name).or_insert(old_name);
        }
        // Attribute members are pure existence, so a reopen simply unions them.
        slot.attr_methods.extend(entry.attr_methods);
        slot.singleton_attr_methods.extend(entry.singleton_attr_methods);
        for pre in entry.prepends {
            if !slot.prepends.contains(&pre) {
                slot.prepends.push(pre);
            }
        }
        for inc in entry.includes {
            if !slot.includes.contains(&inc) {
                slot.includes.push(inc);
            }
        }
        for ext in entry.extends {
            if !slot.extends.contains(&ext) {
                slot.extends.push(ext);
            }
        }
    }

    /// Finish: apply implicit-`Object` superclass defaulting (every class except
    /// `BasicObject` and modules implicitly inherits `Object` when no `< X` was
    /// given) and return the class map plus the set of names declared at genuine
    /// top level (defect 2).
    fn finish(mut self) -> BuiltData {
        let object = intern("Object");
        let basic = intern("BasicObject");
        // A module has no superclass; only *classes* default to Object. We can't
        // perfectly distinguish here, but the curated set's modules (Kernel,
        // Comparable, Enumerable) all legitimately have no class-superclass, and
        // giving a module an Object super would only *add* methods to its chain
        // (never falsely witness absence) — yet to stay precise we skip the
        // known modules.
        let modules: HashSet<&'static str> = ["Kernel", "Comparable", "Enumerable"]
            .into_iter()
            .map(intern)
            .collect();
        for (&name, entry) in self.classes.iter_mut() {
            if entry.superclass.is_none() && name != basic && !modules.contains(name) {
                entry.superclass = Some(object);
            }
        }
        (
            self.classes,
            self.toplevel_classes,
            self.type_alias_defs,
            self.type_alias_params,
            self.interface_method_names,
            self.qualified,
            self.short_to_qualified,
            self.object_constants,
            self.known_type_names,
            self.synthesized_type_names,
        )
    }

    /// RBS `TypeNameResolver#resolve`-style lookup of a stored member name in
    /// ONE lexical context over the full known-type set (classes, modules,
    /// interfaces, aliases — `all_names`): innermost scope outward, then the
    /// root spelling. `Some(_)` ⇒ the name is DECLARED (no stub needed); the
    /// returned key is the resolved qualified name. Unlike the class-only
    /// [`CoreData::resolve_written_ref`], interfaces/aliases count as hits —
    /// `all_names` does not discriminate, and the missing-type scan must not
    /// stub a declared interface.
    fn resolve_type_name_rbs(&self, name: &str, ctx: &[&'static str]) -> Option<&'static str> {
        if let Some(abs) = name.strip_prefix("::") {
            return self.known_type_names.get(abs).copied();
        }
        for scope in ctx.iter().rev() {
            let cand = format!("{scope}::{name}");
            if let Some(&k) = self.known_type_names.get(cand.as_str()) {
                return Some(k);
            }
        }
        self.known_type_names.get(name).copied()
    }
}

/// Parse every `*.rbs` file under `dir` (recursively) and fold its declarations
/// into `builder`. Per-file isolation (ADR-0016 never-crash): a read or parse
/// failure on one file is skipped; the rest still load. Subdirectories are
/// walked because both `core/` (e.g. `core/io/*.rbs`, `core/rubygems/*.rbs`) and
/// some stdlib libs (`stdlib/<lib>/0/<sub>/*.rbs`) nest their signatures.
fn ingest_rbs_dir(builder: &mut Builder, dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            ingest_rbs_dir(builder, &path);
        } else if path.extension().is_some_and(|e| e == "rbs") {
            if let Ok(code) = std::fs::read_to_string(&path) {
                ingest_rbs_source(builder, &path.to_string_lossy(), &code);
            }
        }
    }
}

/// Ingest the project's own signature files: every `signature_paths:` dir and
/// every rbs-collection gem dir, exactly as the reference's
/// `RbsLoader.project_sig_files` + `add_project_signatures` collect them —
/// `Dir.glob(dir/**/*.rbs)` per existing directory, `File.expand_path`'d into
/// a SET (a file reached twice loads once), added in SORTED order.
fn ingest_project_dirs(builder: &mut Builder, sig_dirs: &[PathBuf], collection_dirs: &[PathBuf]) {
    let mut files: std::collections::BTreeMap<String, conformance::Phase> =
        std::collections::BTreeMap::new();
    for (dirs, phase) in [
        (collection_dirs, conformance::Phase::Collection),
        (sig_dirs, conformance::Phase::Project),
    ] {
        for dir in dirs {
            if !dir.is_dir() {
                continue;
            }
            let mut found = Vec::new();
            if !glob_rbs(dir, &mut found) {
                // A name the port cannot read as UTF-8: Ruby's glob sees it.
                builder.conformance.block();
            }
            for f in found {
                // A file both a `signature_paths:` dir and a collection reach
                // counts as the project's. A path that is not UTF-8 (a
                // component ABOVE the sig dir, on Linux) would be keyed and
                // re-read lossily here: stand the scan down instead.
                let abs = expand_path(&f);
                let Some(abs) = abs.to_str() else {
                    builder.conformance.block();
                    continue;
                };
                files.insert(abs.to_string(), phase);
            }
        }
    }
    // Issue #168 — the project load runs in the reference's order:
    //
    //   1. PARSE every file and collect every declared name FIRST. RBS loads
    //      all project declarations into the env (and the `UseMap::Table`
    //      children snapshot) before resolving any of them, so `use Foo::*`
    //      sees a decl in ANY project file, not just files ingested so far.
    //   2. RESOLVE + ingest each file under its own per-file context
    //      (`use` map, `resolve-type-names: false`) — source-local, exactly
    //      like `Environment#resolve_type_names` iterating sources.
    //
    // Pass 1: read + parse, collecting the qualified name of every decl.
    let mut parsed_files: Vec<(
        String,
        conformance::Phase,
        &'static str,
        SignatureNode<'static>,
    )> = Vec::new();
    for (abs, phase) in files {
        let Ok(code) = std::fs::read_to_string(&abs) else {
            // A directory named `*.rbs` or a dangling link is skipped upstream
            // too; a real file the port cannot read (not UTF-8) is one it
            // cannot prove it reads as the reference does.
            if std::fs::metadata(&abs).is_ok_and(|m| m.is_file()) {
                builder.conformance.block();
            }
            continue;
        };
        // A NUL byte crashes the reference's run; `resolve-type-names` is a
        // magic comment rbs reads Ruby-side (the port's parser never sees it).
        if code.contains('\0') || code.contains("resolve-type-names") {
            builder.conformance.block();
        }
        // Interned so the parsed `SignatureNode<'static>` can travel alongside
        // the text into the deferred pass-2 ingest.
        let code: &'static str = intern(&code);
        match parse(code) {
            Ok(sig) => {
                // Every declaration name contributes to `known_type_names`
                // — the set `use Foo::*` expands against (and the missing-type
                // scan's declared set). `ingest_sig` re-records them on the
                // real ingest path; the pre-pass exists because the wildcard
                // children table is computed BEFORE the first file folds in.
                collect_project_decl_names(sig.declarations(), builder);
                parsed_files.push((abs, phase, code, sig));
            }
            Err(_) => {
                // A file the parser rejects is never dropped silently — the
                // same rule the ingest loop below applies, raised early here
                // because this file never reaches it.
                builder.conformance.block();
            }
        }
    }

    // `UseMap::Table#compute_children` — `Foo` → the leaf→child map of every
    // known type DIRECTLY under `Foo` — over the whole known set (bundled +
    // project), computed once before any file resolves.
    let children = use_map_children(builder);

    // Pass 2: ingest each file under its per-file resolution context.
    for (abs, phase, code, sig) in parsed_files {
        let key = intern(&abs);
        let origin = match phase {
            conformance::Phase::Collection => conformance::Origin::Collection(key),
            conformance::Phase::Project => conformance::Origin::Project(key),
        };
        builder.conformance.set_origin(Some(origin));
        let walked = builder.conformance.walks();
        let ctx = file_sig_ctx(&sig, code, &children);
        if ctx.bad_wildcard {
            // `use X::*` with no children under `X`: the reference's
            // `children.fetch` raises `KeyError` and the run dies with an
            // internal-analyzer-error row — a crash shape the port cannot
            // reproduce (its message embeds a Ruby object address) and must
            // not approximate with its own findings.
            builder.conformance.block();
        }
        builder.ingest_sig(code, &sig, Some(&ctx));
        // Not walked = the port's parser rejected it: never drop it silently.
        if builder.conformance.walks() == walked {
            builder.conformance.block();
        }
        builder.conformance.set_origin(None);
    }
}

/// Record the qualified name of every declaration in `decls` (nested members
/// included) into `builder.known_type_names` — the project-name pre-pass the
/// `use Foo::*` expansion table needs before any file resolves.
fn collect_project_decl_names<'a>(
    decls: ruby_rbs::node::NodeList<'a>,
    builder: &mut Builder,
) {
    collect_decl_names_inner(decls.iter(), &[], builder);
}

fn collect_decl_names_inner<'a>(
    decls: impl Iterator<Item = Node<'a>>,
    enclosing: &[&'static str],
    builder: &mut Builder,
) {
    for decl in decls {
        match decl {
            Node::Class(c) => {
                let qual = qualified_name(enclosing, &c.name());
                builder.known_type_names.insert(qual);
                let mut inner = enclosing.to_vec();
                inner.push(qual);
                collect_decl_names_inner(c.members().iter(), &inner, builder);
            }
            Node::Module(m) => {
                let qual = qualified_name(enclosing, &m.name());
                builder.known_type_names.insert(qual);
                let mut inner = enclosing.to_vec();
                inner.push(qual);
                collect_decl_names_inner(m.members().iter(), &inner, builder);
            }
            Node::Interface(i) => {
                builder
                    .known_type_names
                    .insert(qualified_name(enclosing, &i.name()));
            }
            Node::TypeAlias(ta) => {
                builder
                    .known_type_names
                    .insert(qualified_name(enclosing, &ta.name()));
            }
            Node::ClassAlias(ca) => {
                builder
                    .known_type_names
                    .insert(qualified_name(enclosing, &ca.new_name()));
            }
            Node::ModuleAlias(ma) => {
                builder
                    .known_type_names
                    .insert(qualified_name(enclosing, &ma.new_name()));
            }
            _ => {}
        }
    }
}

/// `UseMap::Table#compute_children` over the builder's known-type set —
/// `namespace` → the qualified names directly beneath it (`"Foo"` →
/// `["Foo::Bar", "Foo::Impl"]`, one `::` level deep only), which a
/// `use Foo::*` clause expands through.
fn use_map_children(builder: &Builder) -> HashMap<&'static str, Vec<&'static str>> {
    let mut children: HashMap<&'static str, Vec<&'static str>> = HashMap::new();
    for &name in &builder.known_type_names {
        if let Some(pos) = name.rfind("::") {
            children.entry(&name[..pos]).or_default().push(name);
        }
    }
    for list in children.values_mut() {
        list.sort_unstable();
    }
    children
}

/// Issue #168 (`collect_type_references` over `MethodType#each_type`): every
/// name-bearing type reachable inside `node`, stored the way
/// `resolve_type_names` left it (`use`-mapped / `::`-anchored / root-only via
/// [`member_name`]). Only `ClassInstance` / `Interface` / `Alias` names are
/// candidates (the three kinds `VarianceCalculator#type` raises for); every
/// other node is traversed for the types nested inside it.
fn collect_method_type_names(
    mt: &ruby_rbs::node::MethodTypeNode,
    ctx: Option<&FileSigCtx>,
    out: &mut Vec<&'static str>,
) {
    collect_type_node_names(&mt.type_(), ctx, out);
    if let Some(b) = mt.block() {
        collect_type_node_names(&b.type_(), ctx, out);
        if let Some(st) = b.self_type() {
            collect_type_node_names(&st, ctx, out);
        }
    }
}

fn collect_type_node_names(node: &Node, ctx: Option<&FileSigCtx>, out: &mut Vec<&'static str>) {
    match node {
        Node::ClassInstanceType(ci) => {
            if let Some(n) = member_name(ctx, &ci.name()) {
                out.push(n);
            }
            for a in ci.args().iter() {
                collect_type_node_names(&a, ctx, out);
            }
        }
        Node::InterfaceType(i) => {
            if let Some(n) = member_name(ctx, &i.name()) {
                out.push(n);
            }
            for a in i.args().iter() {
                collect_type_node_names(&a, ctx, out);
            }
        }
        Node::AliasType(a) => {
            if let Some(n) = member_name(ctx, &a.name()) {
                out.push(n);
            }
            for arg in a.args().iter() {
                collect_type_node_names(&arg, ctx, out);
            }
        }
        Node::UnionType(u) => {
            for t in u.types().iter() {
                collect_type_node_names(&t, ctx, out);
            }
        }
        Node::IntersectionType(u) => {
            for t in u.types().iter() {
                collect_type_node_names(&t, ctx, out);
            }
        }
        Node::OptionalType(o) => collect_type_node_names(&o.type_(), ctx, out),
        Node::TupleType(t) => {
            for e in t.types().iter() {
                collect_type_node_names(&e, ctx, out);
            }
        }
        Node::RecordType(r) => {
            for (_k, f) in r.all_fields().iter() {
                match f {
                    Node::RecordFieldType(rf) => collect_type_node_names(&rf.type_(), ctx, out),
                    other => collect_type_node_names(&other, ctx, out),
                }
            }
        }
        Node::ProcType(p) => {
            collect_type_node_names(&p.type_(), ctx, out);
            if let Some(b) = p.block() {
                collect_type_node_names(&b.type_(), ctx, out);
                if let Some(st) = b.self_type() {
                    collect_type_node_names(&st, ctx, out);
                }
            }
            if let Some(st) = p.self_type() {
                collect_type_node_names(&st, ctx, out);
            }
        }
        Node::FunctionType(ft) => collect_function_type_names(ft, ctx, out),
        Node::MethodType(mt) => collect_method_type_names(mt, ctx, out),
        Node::BlockType(b) => {
            collect_type_node_names(&b.type_(), ctx, out);
            if let Some(st) = b.self_type() {
                collect_type_node_names(&st, ctx, out);
            }
        }
        _ => {}
    }
}

fn collect_function_type_names(
    ft: &ruby_rbs::node::FunctionTypeNode,
    ctx: Option<&FileSigCtx>,
    out: &mut Vec<&'static str>,
) {
    for p in ft
        .required_positionals()
        .iter()
        .chain(ft.optional_positionals().iter())
        .chain(ft.trailing_positionals().iter())
    {
        match p {
            Node::FunctionParam(fp) => collect_type_node_names(&fp.type_(), ctx, out),
            other => collect_type_node_names(&other, ctx, out),
        }
    }
    for p in [
        ft.rest_positionals(),
        ft.rest_keywords(),
    ]
    .into_iter()
    .flatten()
    {
        match p {
            Node::FunctionParam(fp) => collect_type_node_names(&fp.type_(), ctx, out),
            other => collect_type_node_names(&other, ctx, out),
        }
    }
    for (_k, v) in ft
        .required_keywords()
        .iter()
        .chain(ft.optional_keywords().iter())
    {
        match v {
            Node::FunctionParam(fp) => collect_type_node_names(&fp.type_(), ctx, out),
            other => collect_type_node_names(&other, ctx, out),
        }
    }
    collect_type_node_names(&ft.return_type(), ctx, out);
}

/// Issue #168 (`synthesize_missing_namespaces` in the reference): a project
/// decl `class Foo::Bar` whose enclosing `Foo` was never declared makes
/// `DefinitionBuilder#build_instance` raise `NoTypeFoundError` there — the
/// reference synthesizes an empty `module Foo` so the classes still build.
/// Every proper `::` prefix of every qualified class/module key must itself
/// be a qualified (class/module) decl; a missing one gets an empty module
/// stub. Runs BEFORE the referenced-type stub pass, as the reference orders
/// them — the stubs it lands are then "already declared" there.
fn synthesize_missing_namespaces(builder: &mut Builder) {
    let mut missing: Vec<&str> = Vec::new();
    for name in builder.qualified.keys() {
        let mut idx = name.len();
        while let Some(pos) = name[..idx].rfind("::") {
            let prefix = &name[..pos];
            if !builder.qualified.contains_key(prefix) && !missing.contains(&prefix) {
                missing.push(prefix);
            }
            idx = pos;
        }
    }
    // Shallowest-first, matching the reference's depth sort — a nested stub's
    // own prefix is synthesized by the time it lands.
    missing.sort_by_key(|n| n.matches("::").count());
    for prefix in missing {
        let key = intern(prefix);
        let entry = ClassEntry {
            is_module: true,
            ..Default::default()
        };
        builder.merge_qualified(key, entry);
        builder.known_type_names.insert(key);
        builder.synthesized_type_names.insert(key);
    }
}

/// Issue #168 (`stub_missing_referenced_types` in the reference): find the
/// type names PROJECT declarations reference but no declaration provides, and
/// synthesize the same empty declarations the reference appends — namespaces
/// as `module`, `_x` as `interface`, lowercase as `type x = untyped`,
/// everything else as `class` — iterating to a fixpoint because a fresh stub
/// can itself resolve a name the previous pass could not. `project_qualified`
/// is the class/module-keyed provenance set — the scan is bounded to
/// project-declared entries exactly like `project_entry?` bounds the
/// reference's walk.
fn synthesize_missing_referenced_types(
    builder: &mut Builder,
    project_qualified: &HashSet<&'static str>,
) {
    // The reference's `synthesize_missing_namespaces` runs FIRST: enclosing
    // namespaces of declared qualified names get `module` stubs before any
    // member-level reference is considered.
    synthesize_missing_namespaces(builder);

    let mut previous: Option<BTreeSet<String>> = None;
    for _ in 0..5 {
        // `unresolved_referenced_types`: the references no declaration
        // provides, per project-declared qualified entry.
        let mut missing: BTreeSet<String> = BTreeSet::new();
        for &q in project_qualified {
            let Some(entry) = builder.qualified.get(q) else {
                continue;
            };
            for &name in &entry.referenced_type_names {
                // `declared_reference?` — `env.type_name?(normalize(name))`.
                // Resolves in EVERY recorded member context (a name declared
                // under any of the entry's decl scopes is declared — `any`
                // keeps the stub set a strict subset of the reference's, so
                // the pass never over-synthesizes); failing that, the
                // reference falls back to the name `absolute!`'d at the root.
                let resolved = entry
                    .member_ctxs
                    .iter()
                    .any(|ctx| builder.resolve_type_name_rbs(name, ctx).is_some());
                if resolved {
                    continue;
                }
                let cand = name.strip_prefix("::").unwrap_or(name);
                if !builder.known_type_names.contains(cand) {
                    missing.insert(cand.to_string());
                }
            }
        }
        if missing.is_empty() || previous.as_ref() == Some(&missing) {
            break;
        }
        // `append_stub_declarations`: enclosing `::` prefixes of a missing
        // name are stubbed with it, minus anything already declared.
        let mut names = missing.clone();
        for m in &missing {
            let mut idx = m.len();
            while let Some(pos) = m[..idx].rfind("::") {
                names.insert(m[..pos].to_string());
                idx = pos;
            }
        }
        names.retain(|n| !builder.known_type_names.contains(n.as_str()));
        if names.is_empty() {
            break;
        }
        let mut synthesized = false;
        for name in &names {
            synthesized |= synthesize_stub(builder, name, &names);
        }
        if !synthesized {
            break;
        }
        previous = Some(missing);
    }
}

/// `stub_declaration_for`: the declaration kind a stubbed name's leaf syntax
/// requires — `module` for a name another stub nests under, `interface` for a
/// `_`-leaf, `type x = untyped` for a lowercase leaf, `class` otherwise.
/// `names` is the whole stub set (for the namespace check). Returns whether a
/// stub landed.
///
/// Only the `module` / `class` arms join `synthesized_type_names` — exactly
/// the reference's `names_synthesized_in` (class/module `class_decls` only):
/// the `interface` and `type` stubs read as `untyped` through every consumer
/// anyway (`RbsTypeTranslator` maps an interface to `Dynamic[Top]`).
fn synthesize_stub(builder: &mut Builder, name: &str, names: &BTreeSet<String>) -> bool {
    let is_namespace = names
        .iter()
        .any(|other| other != name && other.starts_with(&format!("{name}::")));
    let leaf = name.rsplit("::").next().unwrap_or(name);
    if is_namespace {
        let key = intern(name);
        let entry = ClassEntry {
            is_module: true,
            ..Default::default()
        };
        builder.merge_qualified(key, entry);
        builder.known_type_names.insert(key);
        builder.synthesized_type_names.insert(key);
        true
    } else if leaf.starts_with('_') {
        // The `interface` stub — a methodless interface the
        // `qualified_self_type_provides` walk must know exists-but-is-empty
        // (it answers "provides nothing" rather than "unknown ⇒ assume
        // provided"). Not a class decl ⇒ stays out of `synthesized_type_names`.
        builder.interface_method_names.entry(intern(leaf)).or_default();
        builder.known_type_names.insert(intern(name));
        true
    } else if leaf.chars().next().is_some_and(|c| c.is_lowercase()) {
        // `type x = untyped` — the acceptance walk treats `Other` as
        // admit-everything, matching the `Dynamic[Top]` an invented alias
        // reads as in the reference.
        builder
            .type_alias_defs
            .entry(intern(leaf))
            .or_insert_with(|| RetainedParamType::Other("untyped".to_string()));
        builder.known_type_names.insert(intern(name));
        true
    } else {
        let key = intern(name);
        builder.merge_qualified(key, ClassEntry::default());
        builder.known_type_names.insert(key);
        builder.synthesized_type_names.insert(key);
        true
    }
}

/// Ruby's `Dir.glob("<dir>/**/*.rbs")` (no `FNM_DOTMATCH`): a name starting
/// with `.` never matches and a dot-directory is never entered; `**` does not
/// descend into a symlinked directory; `*.rbs` matches any entry by NAME (a
/// symlink, a dangling link, even a directory — the caller's read skips the
/// unreadable ones). On a case-insensitive volume Ruby folds case, so
/// `b.RBS` matches there and only there.
///
/// Returns `false` when an entry's name is not UTF-8 (skipped here, seen by
/// Ruby), so the caller can stand the `conforms-to` scan down.
fn glob_rbs(dir: &std::path::Path, out: &mut Vec<PathBuf>) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return true;
    };
    let mut clean = true;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            clean = false;
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if rbs_name_matches(dir, name) {
            out.push(path.clone());
        }
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            clean &= glob_rbs(&path, out);
        }
    }
    clean
}

/// Whether `*.rbs` matches the entry `name` in `dir` (see [`glob_rbs`]).
fn rbs_name_matches(dir: &std::path::Path, name: &str) -> bool {
    let n = name.len();
    if n <= 4 || !name.is_char_boundary(n - 4) {
        return false;
    }
    let (stem, ext) = name.split_at(n - 4);
    if ext == ".rbs" {
        return true;
    }
    if !ext.eq_ignore_ascii_case(".rbs") {
        return false;
    }
    // Case-folded only on a case-insensitive volume: the lower-cased spelling
    // then names the very same directory entry.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let a = std::fs::symlink_metadata(dir.join(name));
        let b = std::fs::symlink_metadata(dir.join(format!("{stem}.rbs")));
        matches!((a, b), (Ok(a), Ok(b)) if a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (dir, stem);
        cfg!(windows)
    }
}

/// Ruby's `File.expand_path`: absolute, with `.` and `..` folded lexically
/// (symlinks are NOT resolved, as there).
fn expand_path(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = std::path::PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Fold one RBS source's declarations into `builder`. The single per-file ingest
/// point shared by BOTH the filesystem path ([`ingest_rbs_dir`], the
/// `RIGOR_RBS_CORE_DIR` override) and the embedded path ([`ingest_embedded`]):
/// both feed the SAME bytes to the SAME [`Builder::ingest`] / `ruby-rbs` parser,
/// which is what makes the embedded default byte-identical to the runtime path.
/// `name` is informational only (the path or embedded key) — the parser keys off
/// `contents` alone, so it never affects the resulting index.
fn ingest_rbs_source(builder: &mut Builder, _name: &str, contents: &str) {
    builder.ingest(contents);
}

/// Ingest the build-time-embedded vendored RBS set ([`EMBEDDED_RBS`]) — the
/// default (no `RIGOR_RBS_CORE_DIR`) load path. Each `(relative-path, contents)`
/// entry is fed to the SAME [`ingest_rbs_source`] the filesystem path uses, so
/// the index is identical to ingesting the vendored tree from disk. The embedded
/// set is the whole `core/` ⊕ the `DEFAULT_LIBRARIES` stdlib closure already
/// resolved at vendoring time, so no `manifest.yaml` walk is needed here.
fn ingest_embedded(builder: &mut Builder) {
    // Upstream rbs (`core/`, `stdlib/`) first, the rigor-owned overlay LAST —
    // the reference's own load order (`rbs_loader.rb` adds `vendored_gem_sigs/`
    // then `core_overlay/` after the upstream set) so an upstream declaration
    // always wins on conflict. The sorted embed order would otherwise interleave
    // `overlay/` between `core/` and `stdlib/`.
    for (name, contents) in EMBEDDED_RBS.iter().filter(|(n, _)| !is_overlay(n)) {
        ingest_rbs_source(builder, name, contents);
    }
    for (name, contents) in EMBEDDED_RBS.iter().filter(|(n, _)| is_overlay(n)) {
        ingest_rbs_source(builder, name, contents);
    }
}

/// Whether an embedded entry belongs to the rigor-owned overlay tree
/// (`vendor/rbs/overlay/…`) rather than the upstream rbs gem's `core`/`stdlib`.
fn is_overlay(rel_path: &str) -> bool {
    rel_path.starts_with("overlay/")
}

/// Parse the `dependencies:` list out of an RBS stdlib `manifest.yaml`, returning
/// the dependency lib names. Hand-rolled (no YAML crate): the manifests are a
/// trivial, fixed shape — a `dependencies:` key followed by `- name: <lib>`
/// items. A missing/garbled manifest yields no deps (never panics).
fn manifest_deps(path: &std::path::Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut deps = Vec::new();
    let mut in_deps = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        // Leave the `dependencies:` block when a new unindented top-level key
        // appears (a line that isn't a list item and isn't indented).
        if in_deps && !line.starts_with(' ') && !line.starts_with('-') {
            in_deps = false;
        }
        if trimmed == "dependencies:" {
            in_deps = true;
            continue;
        }
        if in_deps {
            // Item shape: `- name: psych` (quotes optional).
            if let Some(rest) = trimmed.strip_prefix("- name:") {
                let name = rest.trim().trim_matches(|c| c == '"' || c == '\'');
                if !name.is_empty() {
                    deps.push(name.to_string());
                }
            }
        }
    }
    deps
}

/// Resolve the **last** path component of a `TypeNameNode`'s namespace+name into
/// a `&'static str` (interned). RBS class refs are usually bare (`Object`,
/// `Comparable`), but a namespaced ref (`::Foo::Bar`) resolves to `Bar`.
fn type_name_str(tn: &ruby_rbs::node::TypeNameNode) -> Option<&'static str> {
    let sym = tn.name();
    let s = sym.as_str();
    if s.is_empty() {
        None
    } else {
        Some(intern(s))
    }
}

/// Whether a `TypeNameNode` names a GENUINE top-level declaration — i.e. its
/// namespace is empty. `class Time` ⇒ empty namespace ⇒ `true`; `class
/// Process::Status` ⇒ namespace path `[Process]` ⇒ `false`. Used to refuse
/// treating a namespaced stdlib class registered by its short key as a known
/// top-level class (defect 2).
fn is_toplevel_name(tn: &ruby_rbs::node::TypeNameNode) -> bool {
    tn.namespace().path().iter().next().is_none()
}

/// ADR-0042 Slice 1: the FULLY QUALIFIED name for a class/module decl — the
/// lexical `enclosing` prefix (from lexically-nesting `class`/`module`
/// bodies), then the `TypeNameNode`'s OWN namespace path (`Process::Status`
/// written at top level has namespace path `[Process]`), then its leaf
/// (`type_name_str`), all joined by `"::"`. A genuine top-level decl with no
/// enclosing and no own namespace (`class Time`) returns just `"Time"` — the
/// SAME string as the existing short key, by design (Slice 2's resolution
/// seam). Interned so repeated qualifications of the same name are the same
/// pointer, mirroring `intern`'s use elsewhere.
///
/// `enclosing` is a CHAIN OF LEXICAL SCOPES, innermost LAST, and every element
/// is ALREADY a full path (`["Bundler", "Bundler::Source"]`) — that shape is
/// what `resolve_written_ref`/`resolve_leaf_unique` need (each scope is used as
/// `"{scope}::{ref}"`). So the prefix of this decl is the INNERMOST scope
/// alone, not the join of the whole chain: joining them produced
/// `Bundler::Bundler::Source::Git` for every decl at lexical depth ≥ 3
/// (2026-08-08 probes note §2d.2; depth ≤ 2 was unaffected because a
/// one-element chain joins to itself).
fn qualified_name(enclosing: &[&'static str], tn: &ruby_rbs::node::TypeNameNode) -> &'static str {
    let ns = tn.namespace();
    // An ABSOLUTE declaration name is ROOTED, not nested: `module ::Kernel`
    // written inside `module Gem` reopens the top-level `Kernel`, not a
    // `Gem::Kernel`. Prefixing it with the lexical scope minted a phantom
    // qualified entry that (a) hid the overlay's `def self?.gem` from every
    // qualified ancestor walk that passes through the real `Kernel` — the 22
    // `gem` holes of the 2026-09-09 diff — and (b) made the short leaf
    // `"Kernel"` ambiguous in `short_to_qualified` (two qualified keys), so
    // `resolve_short_unambiguous("Kernel")` declined. Dropping the prefix on
    // `absolute()` is the whole fix; a relative name is unaffected.
    let mut parts: Vec<String> = if ns.absolute() {
        Vec::new()
    } else {
        enclosing.last().map(|s| vec![(*s).to_string()]).unwrap_or_default()
    };
    for seg in ns.path().iter() {
        if let Node::Symbol(sym) = seg {
            parts.push(sym.as_str().to_string());
        }
    }
    if let Some(leaf) = type_name_str(tn) {
        parts.push(leaf.to_string());
    }
    intern(&parts.join("::"))
}

/// Extract `(return class, arity envelope)` from a method definition by reading
/// its overloads' function types. The return class is resolved only when it is a
/// plain `ClassInstanceType` (a concrete class), or `self`/`instance` mapped to
/// the receiver class by the caller (we record `self` returns as the receiver's
/// own name when known). A union (`bool`), generic, `void`, etc. ⇒ `None`.
///
/// An `Optional` return (`String?`) is unwrapped to its inner
/// `ClassInstanceType` (the class) PLUS a `nilable: true` bit — so a nilable
/// return is preserved, not discarded (it previously fell through to `None` ⇒
/// Dynamic, losing the optionality). The nilable bit obeys the SAME
/// all-overloads-agree discipline as the class: across overloads we adopt the
/// `(class, nilable)` pair only if every resolvable overload agrees on BOTH;
/// any disagreement ⇒ `None` (never guess, never invent nil — being
/// conservative here only loses recall in `possible-nil-receiver`, never an FP).
fn method_signature(
    md: &ruby_rbs::node::MethodDefinitionNode,
    ctx: Option<&FileSigCtx>,
) -> (Option<&'static str>, ArityEnvelope, bool, bool, bool, bool) {
    let mut min: Option<usize> = None;
    let mut max: Option<usize> = Some(0);
    let mut variadic = false;
    // The reference's `arity_eligible?`: ONE ineligible overload disqualifies the
    // whole method, so this latches and is never cleared.
    let mut arity_ineligible = false;
    // The agreed `(class, nilable)` pair across overloads. `ret` carries the
    // class (as before); `ret_nilable` carries the matching nil bit. They move
    // together so a disagreement on EITHER collapses the return to `None`.
    let mut ret: Option<&'static str> = None;
    let mut ret_nilable = false;
    let mut ret_instance = false;
    let mut ret_self = false;
    let mut ret_void = false;
    let mut ret_seen = false;

    for overload in md.overloads().iter() {
        let Node::MethodDefinitionOverload(ov) = overload else {
            continue;
        };
        let Node::MethodType(mt) = ov.method_type() else {
            continue;
        };
        let Node::FunctionType(ft) = mt.type_() else {
            // `(?) -> untyped` parses as an `UntypedFunctionType`, which exposes
            // no per-arity accessors. The reference's `arity_eligible?` calls
            // that ineligible ("an untyped function has no static arity to
            // enforce") and so disqualifies the whole method; skipping the
            // overload silently — as this arm used to — would instead leave the
            // envelope of the REMAINING overloads standing and arity-check
            // against it.
            arity_ineligible = true;
            continue;
        };

        let required = ft.required_positionals().iter().count();
        let optional = ft.optional_positionals().iter().count();
        let has_rest = ft.rest_positionals().is_some();

        // `arity_eligible?` (`analysis/check_rules.rb`) — a REQUIRED keyword
        // must be passed at the call site but is invisible to a positional
        // count, and a trailing positional (`(Integer, *String, Integer)`)
        // makes the envelope non-contiguous. Either one and the reference
        // computes no envelope at all. `optional_keywords` / `rest_keywords`
        // deliberately do NOT disqualify: they cannot change what a caller
        // must pass positionally.
        if ft.required_keywords().iter().next().is_some()
            || ft.trailing_positionals().iter().next().is_some()
        {
            arity_ineligible = true;
        }

        min = Some(min.map_or(required, |m| m.min(required)));
        if has_rest {
            variadic = true;
        } else {
            let hi = required + optional;
            max = max.map(|m| m.max(hi));
        }

        // Return type: resolve a concrete ClassInstanceType, OR an `Optional`
        // wrapping one (`String?` ⇒ class `String`, nilable), OR the late-bound
        // `instance` base (tracked as a flag, NOT a class — `Time.now: () ->
        // instance` means "an instance of the receiver", which only the LOOKUP
        // knows). Across overloads, only adopt a return if ALL resolvable
        // overloads agree on class, nil bit AND instance-ness; any
        // disagreement ⇒ leave None (never guess).
        let (this_ret, this_nilable, this_instance, this_self, this_void) = match ft.return_type() {
            Node::ClassInstanceType(ci) => (member_name(ctx, &ci.name()), false, false, false, false),
            // `-> instance` (M2-GO slice 4): the receiver-class instance.
            Node::InstanceType(_) => (None, false, true, false, false),
            // `-> self` — the RECEIVER itself (`String#force_encoding`,
            // `Array#push`, `Object#freeze`). Tracked apart from `instance`
            // because the two differ on the SINGLETON path: `def self.x: ()
            // -> self` returns the class OBJECT, which the flat return slot
            // cannot spell, while `-> instance` there means an instance.
            Node::SelfType(_) => (None, false, false, true, false),
            // `-> void` (ADR-100): tracked as a flag for
            // `static.value-use.void`; the merged return stays None (the
            // engine's Dynamic recovery), so every existing consumer is
            // unchanged.
            Node::VoidType(_) => (None, false, false, false, true),
            // `String?` lowers to `OptionalType(ClassInstanceType String)`.
            // Recurse into the inner type; a nested optional/union/generic
            // inside the optional is not a single concrete class ⇒ None.
            Node::OptionalType(opt) => match opt.type_() {
                Node::ClassInstanceType(ci) => (member_name(ctx, &ci.name()), true, false, false, false),
                Node::InstanceType(_) => (None, true, true, false, false),
                _ => (None, false, false, false, false),
            },
            _ => (None, false, false, false, false),
        };
        if !ret_seen {
            ret = this_ret;
            ret_nilable = this_nilable;
            ret_instance = this_instance;
            ret_self = this_self;
            ret_void = this_void;
            ret_seen = true;
        } else if ret != this_ret
            || ret_nilable != this_nilable
            || ret_instance != this_instance
            || ret_self != this_self
            || ret_void != this_void
        {
            // Disagreement on class, nilability, instance-ness or void-ness ⇒
            // drop the return entirely (and with it every flag), the
            // conservative choice.
            ret = None;
            ret_nilable = false;
            ret_instance = false;
            ret_self = false;
            ret_void = false;
        }
    }

    let arity = if arity_ineligible {
        None
    } else {
        Some((min.unwrap_or(0), if variadic { None } else { max }))
    };
    // `ret_nilable` is only meaningful when `ret` is Some; callers read it via
    // `method_return_nilable`, which gates on `ret` being present.
    // `ret_instance` / `ret_self` are call-site-late: they say "the receiver",
    // which only the LOOKUP knows. The instance insert path turns either into
    // the `SELF_RETURN` sentinel; the singleton path reads `ret_instance` alone
    // (a singleton `-> self` is the class OBJECT, unspellable here ⇒ declines).
    // When either is set, `ret` is None, so no other consumer changes.
    (ret, arity, ret_nilable, ret_instance, ret_self, ret_void)
}

/// The TUPLE return of a method definition, as an ordered element-shape list —
/// the structured companion of [`method_signature`]'s flat return slot, which
/// collapses a tuple to `None`.
///
/// `Some(elements)` only when EVERY overload's return type is a tuple AND all
/// overloads agree on the element shapes — the same all-overloads-agree
/// discipline the flat return path applies (never guess across a divergent
/// signature). A non-tuple return, a mixed set, or no overload at all ⇒ `None`,
/// which leaves the existing flat path (and its `Dynamic` recovery) untouched.
///
/// An `Optional` tuple (`-> [String, String]?`) is deliberately NOT unwrapped:
/// the reference translates it to a `Tuple | nil` union, which this descriptor
/// cannot carry, and inventing the non-nil half would be unsound.
fn tuple_return(
    md: &ruby_rbs::node::MethodDefinitionNode,
    ctx: Option<&FileSigCtx>,
) -> Option<Vec<RbsReturnShape>> {
    let mut agreed: Option<Vec<RbsReturnShape>> = None;
    let mut seen = false;
    for overload in md.overloads().iter() {
        let Node::MethodDefinitionOverload(ov) = overload else {
            continue;
        };
        let Node::MethodType(mt) = ov.method_type() else {
            continue;
        };
        let Node::FunctionType(ft) = mt.type_() else {
            continue;
        };
        let this = match ft.return_type() {
            Node::TupleType(t) => Some(t.types().iter().map(|e| return_shape(&e, ctx)).collect()),
            _ => None,
        };
        if !seen {
            agreed = this;
            seen = true;
        } else if agreed != this {
            return None;
        }
    }
    agreed
}

/// Translate one RBS type node into an [`RbsReturnShape`] — the rigor-rs
/// analogue of the reference's `RbsTypeTranslator.translate`
/// (`rbs_type_translator.rb:63`), restricted to the two shapes the descriptor
/// models. Everything else degrades to [`RbsReturnShape::Unknown`]
/// (`Dynamic[top]`), exactly as the reference's translator falls back to
/// `Type::Combinator.untyped` for a shape with no handler.
fn return_shape(node: &Node, ctx: Option<&FileSigCtx>) -> RbsReturnShape {
    match node {
        Node::ClassInstanceType(ci) => match member_name_shape(ctx, &ci.name()) {
            Some(name) => RbsReturnShape::Class(name),
            None => RbsReturnShape::Unknown,
        },
        Node::TupleType(t) => {
            RbsReturnShape::Tuple(t.types().iter().map(|e| return_shape(&e, ctx)).collect())
        }
        _ => RbsReturnShape::Unknown,
    }
}

/// The element class name a tuple return RECORDS: the bundled path keeps
/// [`written_type_name`]'s resolved-looking spelling (no `::` marker — the
/// shapes mint verbatim), while a project ctx records the
/// post-`resolve_type_names` name (`::`-anchored) for the lookup-time member
/// resolver to pin.
fn member_name_shape(
    ctx: Option<&FileSigCtx>,
    tn: &ruby_rbs::node::TypeNameNode,
) -> Option<&'static str> {
    match ctx {
        None => written_type_name(tn),
        Some(c) => project_member_name(c, tn),
    }
}

/// The name of a type REFERENCE as written: the `TypeNameNode`'s namespace path
/// joined to its leaf (`Process::Status`), the same spelling
/// [`qualified_name`] builds for a DECLARATION — so an element class name can be
/// looked up in the ADR-0042 qualified registry. A bare reference (`Integer`)
/// yields just the leaf, matching the short-key map. Leading `::` is dropped
/// (the registry keys carry no root marker), mirroring [`qualified_name`].
fn written_type_name(tn: &ruby_rbs::node::TypeNameNode) -> Option<&'static str> {
    let leaf = type_name_str(tn)?;
    let mut parts: Vec<String> = Vec::new();
    for seg in tn.namespace().path().iter() {
        if let Node::Symbol(sym) = seg {
            parts.push(sym.as_str().to_string());
        }
    }
    if parts.is_empty() {
        return Some(leaf);
    }
    parts.push(leaf.to_string());
    Some(intern(&parts.join("::")))
}

/// ADR-0042 Slice 5: the name of a type REFERENCE as written, KEEPING the
/// leading `"::"` of an absolute reference (`include ::Digest::Instance` ⇒
/// `"::Digest::Instance"`; `< Digest::Class` ⇒ `"Digest::Class"`; a bare
/// `< Base` ⇒ `"Base"`). Unlike [`written_type_name`] (which drops the root
/// marker to match the registry key spelling), the absolute bit is load-bearing
/// here: the qualified resolver must NOT walk an absolute reference through the
/// enclosing lexical scopes.
fn written_ref(tn: &ruby_rbs::node::TypeNameNode) -> Option<&'static str> {
    let leaf = type_name_str(tn)?;
    let ns = tn.namespace();
    let mut s = String::new();
    if ns.absolute() {
        s.push_str("::");
    }
    for seg in ns.path().iter() {
        if let Node::Symbol(sym) = seg {
            s.push_str(sym.as_str());
            s.push_str("::");
        }
    }
    if s.is_empty() {
        return Some(leaf);
    }
    s.push_str(leaf);
    Some(intern(&s))
}

/// Issue #168: the per-FILE resolution context a PROJECT signature is ingested
/// under — the port's analogue of `RBS::Environment#resolve_signature` running
/// per source file (`Environment#resolve_type_names` resolves each source's
/// declarations independently). Two knobs:
///
/// * `use_map` — the file's `use` clauses as `UseMap::build_map` produces
///   them: `use Foo::Impl` ⇒ `"Impl" -> "Foo::Impl"`, `use Foo::Impl as B` ⇒
///   `"B" -> "Foo::Impl"`, `use Foo::*` ⇒ every known type `Foo::X`
///   contributes `"X" -> "Foo::X"` (the expansion table is the env-wide
///   known-type set, exactly as `UseMap::Table`). Targets are stored WITHOUT
///   the leading `::`; hits re-anchor them (`UseMap` targets are absolute by
///   construction).
/// * `root_only` — the `# resolve-type-names: false` magic comment: the
///   file's type names pass through unresolved and the reference's
///   `DefinitionBuilder` `absolute!`s them — i.e. they become ROOT-relative
///   as written. The `use` map is skipped in that file too: the directive
///   disables `resolve_signature` wholesale.
///
/// Both are per-file by construction — one file's directives never reach a
/// sibling's resolution.
#[derive(Default)]
struct FileSigCtx {
    /// `# resolve-type-names: false` was the file's leading comment.
    root_only: bool,
    /// bare-name → qualified target (no `::` marker), per `UseMap::build_map`.
    use_map: HashMap<&'static str, &'static str>,
    /// A `use X::*` wildcard whose namespace has no `children` entry — the
    /// reference's `children.fetch` raises `KeyError` there and the whole run
    /// dies with an internal-analyzer-error row (its message embeds a Ruby
    /// object address, so even the row's text is not reproducible). The ingest
    /// flags it so `ingest_project_dirs` can stand the conformance scan down.
    bad_wildcard: bool,
}

/// The reference recognises `# resolve-type-names:` only as the file's LEADING
/// content — `RBS::Parser.magic_comment` anchors `\A` at `buf.content` offset 0
/// with `#\s*resolve-type-names\s*:\s+(true|false)$` — so a blank first line, a
/// different leading comment, or any later line carrying the text does NOT
/// disable resolution there. Mirror that shape exactly.
fn resolve_type_names_disabled(code: &str) -> bool {
    let line = code.lines().next().unwrap_or("");
    let Some(rest) = line.strip_prefix('#') else {
        return false;
    };
    let Some(rest) = rest.trim_start().strip_prefix("resolve-type-names") else {
        return false;
    };
    let Some(rest) = rest.trim_start().strip_prefix(':') else {
        return false;
    };
    // `\s+` after the colon is required; the value must be the whole line tail.
    if !rest.starts_with([' ', '\t']) {
        return false;
    }
    rest.trim_start() == "false"
}

/// Build a file's [`FileSigCtx`]: the magic comment, plus `UseMap::build_map`
/// over the signature's `use` directives. `children` is the env-wide
/// namespace-children table (`UseMap::Table#compute_children` — `Foo` → every
/// known type directly under `Foo`), which the ingest's project pre-pass
/// fills before any file resolves so a wildcard sees declarations from EVERY
/// project file, not just those already ingested.
fn file_sig_ctx(
    sig: &SignatureNode<'_>,
    code: &str,
    children: &HashMap<&'static str, Vec<&'static str>>,
) -> FileSigCtx {
    let mut ctx = FileSigCtx {
        root_only: resolve_type_names_disabled(code),
        ..Default::default()
    };
    for directive in sig.directives().iter() {
        let Node::Use(use_) = directive else {
            continue;
        };
        for clause in use_.clauses().iter() {
            match clause {
                Node::UseSingleClause(sc) => {
                    // `use Foo::Impl [as B]` — the map key is the NEW name
                    // when `as` is present, else the type's own leaf
                    // (`@map[clause.type_name.name]` in `build_map`). The
                    // target `absolute!`s; the map stores it without `::`.
                    let Some(target) = written_ref(&sc.type_name()) else {
                        continue;
                    };
                    let target = intern(target.strip_prefix("::").unwrap_or(target));
                    let key = match sc.new_name() {
                        Some(new_name) => intern(new_name.as_str()),
                        None => match type_name_str(&sc.type_name()) {
                            Some(leaf) => leaf,
                            None => continue,
                        },
                    };
                    ctx.use_map.insert(key, target);
                }
                Node::UseWildcardClause(wc) => {
                    // `use Foo::*` — `table.children.fetch(ns.absolute!).each
                    // { @map[child.name] = child }`: every known type directly
                    // under `Foo` maps its leaf to the full path. `fetch`
                    // RAISES when the namespace has no children entry — `use
                    // Nowhere::*`, and `use EmptyMod::*` alike — and the
                    // reference's whole run dies there. The port maps nothing
                    // and flags it (`bad_wildcard`), which is the unverifiable
                    // signal `ingest_project_dirs` stands the scan down on.
                    if let Some(ns) = namespace_path_str(&wc.namespace()) {
                        match children.get(ns) {
                            Some(list) => {
                                for &child in list {
                                    if let Some(leaf) = child.rsplit("::").next() {
                                        ctx.use_map.insert(intern(leaf), child);
                                    }
                                }
                            }
                            None => ctx.bad_wildcard = true,
                        }
                    }
                }
                _ => {}
            }
        }
    }
    ctx
}

/// A `NamespaceNode` (the `Foo` of `use Foo::*`) as a qualified string, root
/// marker dropped (`ns.absolute!` in the reference produces the same
/// `Foo`/`::Foo`-canonical key the `children` table is built on).
fn namespace_path_str(ns: &ruby_rbs::node::NamespaceNode) -> Option<&'static str> {
    let mut parts: Vec<String> = Vec::new();
    for seg in ns.path().iter() {
        if let Node::Symbol(sym) = seg {
            parts.push(sym.as_str().to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(intern(&parts.join("::")))
    }
}

/// The type name a PROJECT signature's member records, resolved the way the
/// reference's `resolve_type_names` leaves it before the build — see
/// [`FileSigCtx`]. `None` (`ctx` absent ⇒ bundled/plugin path) falls back to
/// [`type_name_str`]'s flat leaf, byte-identical to the previous behaviour.
fn member_name(ctx: Option<&FileSigCtx>, tn: &ruby_rbs::node::TypeNameNode) -> Option<&'static str> {
    match ctx {
        None => type_name_str(tn),
        Some(c) => project_member_name(c, tn),
    }
}

/// [`written_ref`] under a project ctx — the `_written` header references
/// (superclass / include / prepend / module self-type) record the
/// post-`resolve_type_names` name so the qualified resolver sees the same
/// spelling the reference's `DefinitionBuilder` consumed.
fn written_ref_ctx(
    ctx: Option<&FileSigCtx>,
    tn: &ruby_rbs::node::TypeNameNode,
) -> Option<&'static str> {
    match ctx {
        None => written_ref(tn),
        Some(c) => project_member_name(c, tn),
    }
}

/// `resolve_type_names` for one member reference — `UseMap::resolve?` +
/// `absolute_type_name`:
///
/// * `::Foo::Bar` passes through untouched (an absolute name never looks up
///   the `use` map — `resolve?` returns early);
/// * `root_only` (`resolve-type-names: false`) ⇒ the name `absolute!`s to the
///   root spelling: `"::" + written`;
/// * else the `use` map: a BARE `Baz` looks up `Baz`; a `Foo::Bar` looks up
///   its HEAD `Foo` only (`resolve?` maps `hd`, keeping the tail). A hit
///   re-anchors `"::" + target [:: + tail]`; a miss keeps the written path —
///   the `resolver.resolve || type_name` fallback, which the builder then
///   resolves lexically / `absolute!`s at lookup time.
fn project_member_name(
    ctx: &FileSigCtx,
    tn: &ruby_rbs::node::TypeNameNode,
) -> Option<&'static str> {
    let written = written_ref(tn)?;
    if written.starts_with("::") {
        return Some(written);
    }
    if ctx.root_only {
        return Some(intern(&format!("::{written}")));
    }
    let (head, rest) = match written.split_once("::") {
        Some((h, r)) => (h, Some(r)),
        None => (written, None),
    };
    match ctx.use_map.get(head) {
        Some(&target) => match rest {
            Some(tail) => Some(intern(&format!("::{target}::{tail}"))),
            None => Some(intern(&format!("::{target}"))),
        },
        None => Some(written),
    }
}

/// Retain the per-overload positional-parameter shapes of a method definition —
/// the ATM substrate (Slice 1). Unlike [`method_signature`], which collapses all
/// overloads into a single `(min, max)` arity envelope, this keeps every overload
/// as its own [`OverloadSignature`], with required/optional positionals carried as
/// one-level [`RetainedParamType`] tags plus presence flags for the shapes a later
/// argument check treats coarsely (rest / keywords / trailing). One entry per RBS
/// overload, in declaration order. `code` is the RBS source the definition was
/// parsed from, used to slice verbatim written forms for `RetainedParamType::Other`.
fn method_overloads(
    md: &ruby_rbs::node::MethodDefinitionNode,
    code: &str,
    ctx: Option<&FileSigCtx>,
) -> Vec<OverloadSignature> {
    let mut out: Vec<OverloadSignature> = Vec::new();
    for overload in md.overloads().iter() {
        let Node::MethodDefinitionOverload(ov) = overload else {
            continue;
        };
        let Node::MethodType(mt) = ov.method_type() else {
            continue;
        };
        let Node::FunctionType(ft) = mt.type_() else {
            continue;
        };
        // A method type may bind its own type parameters, and rbs 4.1 started
        // using BOUNDED ones in core signatures (`def fetch: … | [I < _ToInt,
        // T] (I index) { (I index) -> T } -> (E | T)`). The bounds ride the
        // signature in `type_param_bounds` and the params keep their raw
        // `Variable` leaves: the reference substitutes a bound ONLY when
        // collecting the multi-overload parameter set (`resolve_param_bounds`),
        // while the single-overload channel walks the raw `param.type` — where
        // a bare variable admits `nil` and is never faithfully checkable, so it
        // declines there.
        let bounds = method_type_param_bounds(&mt, code, ctx);
        let required_positionals = ft
            .required_positionals()
            .iter()
            .map(|p| param_node_type(&p, code, ctx))
            .collect();
        let optional_positionals = ft
            .optional_positionals()
            .iter()
            .map(|p| param_node_type(&p, code, ctx))
            .collect();
        let required_positional_names = ft
            .required_positionals()
            .iter()
            .map(|p| param_node_name(&p))
            .collect();
        let optional_positional_names = ft
            .optional_positionals()
            .iter()
            .map(|p| param_node_name(&p))
            .collect();
        out.push(OverloadSignature {
            required_positionals,
            optional_positionals,
            required_positional_names,
            optional_positional_names,
            has_rest_positionals: ft.rest_positionals().is_some(),
            has_required_keywords: ft.required_keywords().iter().next().is_some(),
            has_optional_keywords: ft.optional_keywords().iter().next().is_some(),
            has_rest_keywords: ft.rest_keywords().is_some(),
            has_trailing_positionals: ft.trailing_positionals().iter().next().is_some(),
            block_required: mt.block().is_some_and(|b| b.required()),
            type_param_bounds: bounds,
            return_form: normalized_written_form(&ft.return_type(), code, out.len()),
        });
    }
    out
}

/// [`node_written_form`] normalised for EQUALITY comparison: whitespace runs
/// collapse to one space and the ends are trimmed, so a return type wrapped
/// across lines in the RBS source compares equal to the same type written on
/// one line. An unavailable slice becomes a per-overload sentinel so it never
/// compares equal to another overload's return (the conservative direction:
/// "these overloads disagree" only ever makes a consumer answer LESS).
fn normalized_written_form(node: &Node, code: &str, ordinal: usize) -> String {
    let raw = node_written_form(node, code);
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        format!("\0unresolved{ordinal}")
    } else {
        normalized
    }
}

/// The upper bounds a method type declares for its own type parameters, as
/// `(variable name, bound)` pairs — `[I < _ToInt, T]` yields one entry for `I`
/// and none for `T`. A `Vec` rather than a map: a method type binds a handful of
/// parameters at most, so a linear scan beats hashing. Unbounded parameters are
/// omitted entirely, so a lookup miss means "genuinely unconstrained".
fn method_type_param_bounds(
    mt: &ruby_rbs::node::MethodTypeNode,
    code: &str,
    ctx: Option<&FileSigCtx>,
) -> Vec<(&'static str, RetainedParamType)> {
    mt.type_params()
        .iter()
        .filter_map(|tp| {
            let Node::TypeParam(p) = tp else {
                return None;
            };
            let bound = p.upper_bound()?;
            let sym = p.name();
            let name = sym.as_str();
            if name.is_empty() {
                return None;
            }
            // The bound is resolved with NO bounds in scope: a bound that
            // itself mentions a sibling variable is not a shape rbs core uses,
            // and resolving it would need fixpoint ordering for no gain.
            Some((intern(name), retained_param_type(&bound, code, ctx)))
        })
        .collect()
}

/// Resolve a positional-parameter node (`RBS::Types::Function::Param`, whose
/// `.type_()` is the parameter's type) into a one-level [`RetainedParamType`].
fn param_node_type(param: &Node, code: &str, ctx: Option<&FileSigCtx>) -> RetainedParamType {
    match param {
        Node::FunctionParam(fp) => retained_param_type(&fp.type_(), code, ctx),
        // Defensive: a positional that isn't a FunctionParam node (shouldn't
        // occur) is retained verbatim as an `Other` leaf.
        other => RetainedParamType::Other(node_written_form(other, code)),
    }
}

/// The declared name of a positional parameter (`str` in `(String str)`), or
/// `None` when the RBS omits it. Feeds the reference's ``parameter `str' of ``
/// message prefix on a single-overload argument-type mismatch.
fn param_node_name(param: &Node) -> Option<&'static str> {
    match param {
        Node::FunctionParam(fp) => fp.name().map(|s| intern(s.as_str())),
        _ => None,
    }
}

/// Lower one RBS type node to a [`RetainedParamType`] tag (ATM substrate). The
/// named kinds — class instance, `type` alias, `interface — and the structural
/// wrappers `Union` / `Optional` / `Tuple` are recognised and recurse into
/// their type arguments / members; a bare `Variable` keeps its name (method
/// bounds are applied by consumers, not here); every other shape collapses to
/// [`RetainedParamType::Other`] carrying the verbatim written form sliced from
/// `code`.
fn retained_param_type(
    node: &Node,
    code: &str,
    ctx: Option<&FileSigCtx>,
) -> RetainedParamType {
    match node {
        Node::VariableType(v) => {
            let name = v.name();
            if name.as_str().is_empty() {
                RetainedParamType::Other(node_written_form(node, code))
            } else {
                RetainedParamType::Variable(intern(name.as_str()))
            }
        }
        Node::ClassInstanceType(ci) => match retained_name(ctx, &ci.name()) {
            Some(name) => RetainedParamType::ClassInstance(
                name,
                ci.args()
                    .iter()
                    .map(|a| retained_param_type(&a, code, ctx))
                    .collect(),
            ),
            None => RetainedParamType::Other(node_written_form(node, code)),
        },
        Node::AliasType(a) => match retained_name(ctx, &a.name()) {
            Some(name) => RetainedParamType::Alias(
                name,
                a.args()
                    .iter()
                    .map(|a| retained_param_type(&a, code, ctx))
                    .collect(),
            ),
            None => RetainedParamType::Other(node_written_form(node, code)),
        },
        Node::InterfaceType(i) => match retained_name(ctx, &i.name()) {
            Some(name) => RetainedParamType::Interface(
                name,
                i.args()
                    .iter()
                    .map(|a| retained_param_type(&a, code, ctx))
                    .collect(),
            ),
            None => RetainedParamType::Other(node_written_form(node, code)),
        },
        Node::UnionType(u) => RetainedParamType::Union(
            u.types()
                .iter()
                .map(|t| retained_param_type(&t, code, ctx))
                .collect(),
        ),
        Node::OptionalType(o) => RetainedParamType::Optional(Box::new(retained_param_type(
            &o.type_(),
            code,
            ctx,
        ))),
        Node::TupleType(t) => RetainedParamType::Tuple(
            t.types()
                .iter()
                .map(|e| retained_param_type(&e, code, ctx))
                .collect(),
        ),
        other => RetainedParamType::Other(node_written_form(other, code)),
    }
}

/// A retained-parameter leaf name under a project ctx: the post-
/// `resolve_type_names` spelling WITHOUT the `::` marker (the ATM consumers
/// compare registry spellings, which carry no root marker).
fn retained_name(ctx: Option<&FileSigCtx>, tn: &ruby_rbs::node::TypeNameNode) -> Option<&'static str> {
    member_name(ctx, tn).map(|n| n.strip_prefix("::").unwrap_or(n))
}

/// The verbatim written form of a type node, sliced from the RBS source `code`
/// by the node's byte range. Falls back to an empty string if the range is out
/// of bounds or not on a UTF-8 boundary (never panics) — the `Other` leaf is a
/// label hint, so a degraded slice is acceptable and never load-bearing here.
fn node_written_form(node: &Node, code: &str) -> String {
    let range = node.location();
    let start = range.start().max(0) as usize;
    let end = range.end().max(0) as usize;
    if start <= end && end <= code.len() {
        code.get(start..end).unwrap_or("").to_string()
    } else {
        String::new()
    }
}

/// The RETURN class of the method's **block-bearing overload** — the overload
/// the reference picks when a block is supplied at the call site
/// (`OverloadSelector` with `block_required: true`). We scan the overloads for
/// one declaring a `block:` clause (`MethodTypeNode::block()`), and resolve ITS
/// function return type:
///
/// - a concrete `ClassInstanceType` (`Hash#filter { } -> ::Hash[K,V]`,
///   `Enumerable#map { } -> ::Array[U]`) ⇒ that class name;
/// - a `self` return (`Array#each { } -> self`, `Kernel#tap { } -> self`) ⇒
///   the [`SELF_RETURN`] sentinel, resolved to the receiver at lookup time.
///
/// Returns `None` (⇒ block form not modeled ⇒ caller stays Dynamic, zero-FP)
/// when no overload has a block, or when the block overload's return is a
/// union (`bool`), bare generic variable, `void`, nilable, or anything else we
/// can't pin to a single concrete class. When MULTIPLE block overloads exist
/// we require them to AGREE on the return (any disagreement ⇒ `None`), matching
/// the conservative discipline of [`method_signature`].
fn block_overload_return(
    md: &ruby_rbs::node::MethodDefinitionNode,
    ctx: Option<&FileSigCtx>,
) -> Option<&'static str> {
    let mut found: Option<Option<&'static str>> = None;
    for overload in md.overloads().iter() {
        let Node::MethodDefinitionOverload(ov) = overload else {
            continue;
        };
        let Node::MethodType(mt) = ov.method_type() else {
            continue;
        };
        // Only the block-bearing overload(s) participate.
        if mt.block().is_none() {
            continue;
        }
        let Node::FunctionType(ft) = mt.type_() else {
            continue;
        };
        let this_ret = match ft.return_type() {
            Node::ClassInstanceType(ci) => member_name(ctx, &ci.name()),
            // A `self` block return (each/tap) ⇒ the receiver's own type.
            Node::SelfType(_) => Some(SELF_RETURN),
            _ => None,
        };
        match found {
            None => found = Some(this_ret),
            Some(prev) if prev != this_ret => return None,
            _ => {}
        }
    }
    found.flatten()
}

/// The RETURN class agreed by the method's **block-free** overloads — the
/// overloads the reference's `OverloadSelector` considers at a call site with NO
/// block (`block_required: false`). The complement of
/// [`block_overload_return`], and the fix for the collection-shape stage-2
/// chain roots: `String#split` and `Dir.glob` both declare a block overload
/// whose return DIVERGES from the block-free one (`self` / `nil` vs
/// `Array[String]`), so [`method_signature`]'s all-overloads-agree collapse
/// drops the return to `None` and a block-free `Dir.glob(...).sort` goes
/// untyped even though the reference types it `Array[String]`.
///
/// Returns `Some(class)` ONLY when
///
/// - the method declares AT LEAST ONE block-bearing overload (otherwise the
///   block-free set IS the full set and this slot can add nothing the flat
///   return already carries — keeping the map empty there makes the delta
///   provably scoped to the divergent-block-overload methods);
/// - at least one block-free overload exists; and
/// - EVERY block-free overload returns the SAME bare concrete
///   `ClassInstanceType`.
///
/// Anything else — a `self`/`instance`/`void`/nilable/union/generic block-free
/// return, or a disagreement between two block-free overloads — yields `None`,
/// i.e. the existing Dynamic decline (zero-FP, the same conservative discipline
/// as [`method_signature`] and [`block_overload_return`]).
fn block_free_overload_return(
    md: &ruby_rbs::node::MethodDefinitionNode,
    ctx: Option<&FileSigCtx>,
) -> Option<&'static str> {
    let mut has_block_overload = false;
    let mut agreed: Option<&'static str> = None;
    let mut seen_block_free = false;
    for overload in md.overloads().iter() {
        let Node::MethodDefinitionOverload(ov) = overload else {
            continue;
        };
        let Node::MethodType(mt) = ov.method_type() else {
            continue;
        };
        if mt.block().is_some() {
            has_block_overload = true;
            continue;
        }
        let Node::FunctionType(ft) = mt.type_() else {
            // An untyped function type (`(?) -> untyped`) is not a concrete
            // class: decline the whole method.
            return None;
        };
        let this_ret = match ft.return_type() {
            Node::ClassInstanceType(ci) => member_name(ctx, &ci.name()),
            _ => None,
        };
        let this_ret = this_ret?;
        match agreed {
            None if !seen_block_free => agreed = Some(this_ret),
            Some(prev) if prev == this_ret => {}
            _ => return None,
        }
        seen_block_free = true;
    }
    if !has_block_overload || !seen_block_free {
        return None;
    }
    agreed
}

/// Intern a `&str` to a `&'static str` by leaking, deduplicated through a
/// process-global set so equal names share one allocation. The core class/method
/// vocabulary is small and bounded, so the leak is negligible and one-time.
fn intern(s: &str) -> &'static str {
    use std::sync::Mutex;
    use std::sync::OnceLock;
    static POOL: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let pool = POOL.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = pool.lock().unwrap();
    if let Some(&existing) = guard.get(s) {
        return existing;
    }
    let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
    guard.insert(leaked);
    leaked
}

#[cfg(test)]
mod embedded_tests;

/// ADR-0042 Slice 1: the new qualified-key registry, alongside-additive to the
/// short-key `classes` map. Guarded (`if idx.knows_class("ERB") { ... }`, mirroring
/// `rbs_class_new_types_to_rbs_instance`'s `knows_class("Pathname")` guard) so
/// these don't false-fail if the embedded/override RBS isn't loadable in the
/// sandbox and `CoreData::load()` falls back to the stub (empty qualified maps).
#[cfg(test)]
mod qualified_registry_tests;

/// S1 (2026-08-08): `qualified_class_has_method` must never witness the absence
/// of a method the reference finds. Two measured causes, both fixed here:
/// inherited methods reached only through the AS-WRITTEN ancestor chain, and
/// methods generated by RBS ATTRIBUTE members (which ingestion dropped
/// entirely). Each row below is a probe from
/// `docs/notes/20260808-qualified-witnessing-probes.md` §7.
#[cfg(test)]
mod qualified_ancestor_soundness_tests;

#[cfg(test)]
mod qualified_singleton_witness_tests;

#[cfg(test)]
mod qualified_instance_method_tests;

#[cfg(test)]
mod qualified_return_lookup_tests;

#[cfg(test)]
mod qualified_project_sig_tests;

/// Issue #168: project `sig/` `use` directives, the `resolve-type-names`
/// magic comment, and missing-referenced-type stubs — the acceptance rows a
/// hand-built project probes.
#[cfg(test)]
mod use_directive_tests;

#[cfg(test)]
mod collection_shape_stage2_tests;
