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

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use ruby_rbs::node::{
    parse, AliasKind, ClassNode, InterfaceNode, MethodDefinitionKind, ModuleNode,
    Node, TypeAliasNode,
};

// The build-time-embedded RBS signature set: `EMBEDDED_RBS: &[(&str, &str)]`,
// one `(relative-path, file-contents)` entry per vendored `.rbs`, in
// deterministic sorted-by-path order (see `build.rs`). The ingest is
// order-independent for class membership, so any total order yields the same
// index; sorted is chosen for reproducibility.
include!(concat!(env!("OUT_DIR"), "/embedded_rbs.rs"));

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
/// build — `RBS::DefinitionBuilder#build_instance` / `#build_singleton` raise, so
/// `RbsLoader#instance_definition` / `#singleton_definition` fail-soft to `nil`
/// and the dispatcher degrades EVERY call on them to `Dynamic[Top]`. The oracle
/// is therefore silent on any method of these classes — real or misspelled — and
/// witnessing absence over rigor-rs's (buildable) copy of the same signatures is
/// a false positive by ADR-0002. Measured, not theorised: `BigMath.frobnicate(1)`
/// and `Bundler.frobnicate` both fired here while the pinned oracle stayed
/// silent (`docs/notes/20260731-bigmath-ingestion-asymmetry.md`).
///
/// The failures are all SOURCE COLLISIONS inside the reference's own load set,
/// and every one of them involves a signature source rigor-rs deliberately does
/// NOT vendor, which is why rigor-rs's index builds cleanly instead:
///
/// - `BigMath` — `DEFAULT_LIBRARIES` lists BOTH `bigdecimal` and
///   `bigdecimal-math`. `RBS::EnvironmentLoader` resolves `bigdecimal` to the
///   INSTALLED GEM's `sig/` (a gem's own signatures win over `rbs`'s `stdlib/`
///   copy), and that directory ships `big_math.rbs` — the same `module BigMath`
///   that `rbs`'s `stdlib/bigdecimal-math/0/big_math.rbs` declares. Two sources,
///   same methods ⇒ `DuplicatedMethodDefinitionError`. rigor-rs vendors only the
///   `rbs` stdlib copy (`vendor/rbs/PROVENANCE.md`), so it has exactly one.
/// - `Bundler*` / `Gem::*` — the `rbs` GEM's own `sig/shims/{bundler,rubygems}.rbs`
///   (reached because `rbs` is in `DEFAULT_LIBRARIES`) against the reference's
///   `data/vendored_gem_sigs/{bundler,rubygems}/`. rigor-rs vendors the latter as
///   `overlay/vendored_gem_sigs/` but NOT the `rbs` gem's `sig/`, so again one
///   source only.
/// - `Gem::SourceList` (`NoTypeFoundError`) and `Nokogiri::CSS::Parser`
///   (`NoSuperclassFoundError: Racc::Parser`) — dangling references inside
///   `vendored_gem_sigs` itself.
///
/// **Why a table and not a derivation.** rigor-rs cannot compute this set from
/// its own tree: the colliding declaration is precisely the one it does not
/// carry. Mirroring the reference's real load set instead (vendoring the
/// `bigdecimal` and `rbs` gems' `sig/`) would drag those gems' whole class
/// surface into `knows_class` — the failure mode `PROVENANCE.md` records for the
/// `prism` supplement (8 fresh FPs). So the set is DATA, on the same footing as
/// the vendored signatures themselves, and it is regenerated from the pinned
/// oracle by `harness/unbuildable_classes.rb` (`--check` verifies this list
/// against the reference and is part of the pin-bump ritual). If upstream fixes a
/// collision, regeneration drops the entry and rigor-rs starts witnessing again —
/// the table converges rather than freezing a gap.
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
const UNBUILDABLE_DEFINITIONS: &[(&str, bool, bool)] = &[
    ("BigMath", true, true),
    ("Bundler", false, true),
    ("Bundler::Definition", true, true),
    ("Bundler::Dependency", true, true),
    ("Bundler::LazySpecification", true, true),
    ("Bundler::LockfileParser", true, true),
    ("Gem::Dependency", true, true),
    ("Gem::DependencyInstaller", true, true),
    ("Gem::Requirement", false, true),
    ("Gem::SourceList", true, true),
    ("Gem::Specification", true, true),
    ("Nokogiri::CSS::Parser", true, true),
];

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
            entry.void_methods.clear();
            entry.aliases.clear();
        }
        if singleton {
            entry.singleton_unbuildable = true;
            entry.singleton_methods.clear();
            entry.singleton_tuple_returns.clear();
            entry.singleton_method_overloads.clear();
            entry.overloading_singleton_overloads.clear();
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
/// labels (Slice 3) WITHOUT retaining the full type AST: a `ClassInstanceType`
/// drops its type arguments (`Array[Integer]` ⇒ `ClassInstance("Array")`), and
/// only the two genuinely-structural wrappers — `Union` and `Optional` — recurse
/// into their members (they carry no meaning as an opaque leaf). Everything else
/// that isn't one of the four named kinds collapses to [`Other`](Self::Other),
/// whose `String` is the exact WRITTEN form of the type (sliced from the RBS
/// source) so a diagnostic can quote it verbatim later.
///
/// The interned names ride `&'static str` (the file-wide interning discipline);
/// the `Other` leaf is an owned `String` because its vocabulary is unbounded.
/// This type is RETAINED but read by NO consumer in Slice 1 — the accessors
/// ([`CoreData::method_overloads`], [`CoreData::resolve_type_alias`],
/// [`CoreData::interface_methods`]) exist and are unit-tested, but nothing wires
/// them into a rule yet (the slice is output-inert by contract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetainedParamType {
    /// A concrete class instance, type arguments dropped: `Integer`, `String`,
    /// `Array[Integer]` ⇒ `ClassInstance("Array")`.
    ClassInstance(&'static str),
    /// A reference to a `type` alias (RBS lowercase alias, e.g. the `string` in
    /// `(string) -> void`). NOT expanded here — Slice 2 owns bounded expansion
    /// via [`CoreData::resolve_type_alias`].
    Alias(&'static str),
    /// A reference to an `interface` (RBS `_`-prefixed, e.g. `_ToStr`). The
    /// required method-name set is retained separately in
    /// [`CoreData::interface_methods`].
    Interface(&'static str),
    /// A union `A | B | ...` — each member retained one level deep.
    Union(Vec<RetainedParamType>),
    /// An optional `T?` — the inner type retained one level deep.
    Optional(Box<RetainedParamType>),
    /// Any other type shape (base types `bool`/`nil`/`untyped`/`void`/`self`,
    /// literals, tuples, records, procs, singletons, type variables, …). The
    /// `String` is the verbatim written form sliced from the RBS source.
    Other(String),
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
    methods: HashMap<&'static str, (Option<&'static str>, Arity, bool)>,
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
    /// Singleton (class-level) methods `def self.x` (and the singleton half of
    /// `def self?.x`). Keyed by name -> `(resolved return class, arity envelope)`.
    /// The singleton class inherits down the SUPERCLASS chain, so resolving a
    /// class method walks these maps up `superclass`. The return-class slot
    /// mirrors the instance `methods` table's resolution discipline (a single
    /// bare concrete `ClassInstanceType` ⇒ `Some(name)`, else `None`) and is read
    /// ONLY by the sig-gen-only [`Self::declared_singleton_return`]; the existence
    /// check ([`Self::class_has_singleton_method`]) uses just the key set.
    singleton_methods: HashMap<&'static str, (Option<&'static str>, Arity, bool)>,
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
    /// `true` when this name was declared as a `module` (not a `class`) in RBS —
    /// the analogue of the reference's `Environment#rbs_module?`. Read ONLY by
    /// `call.raise-non-exception`'s instance path (a value typed as a module
    /// includer could be an Exception at runtime, so it must stay silent). Set on
    /// the module ingest, OR-merged across reopens.
    is_module: bool,
    /// Included module names (in source order).
    includes: Vec<&'static str>,
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
    /// `"string" → Union([ClassInstance("String"), Interface("_ToStr")])`). The
    /// RHS is stored RAW (aliases inside it are NOT expanded); bounded expansion
    /// with a cycle cap is Slice 2's job. Read only via
    /// [`Self::resolve_type_alias`]; no rule wires it yet.
    type_alias_defs: HashMap<&'static str, RetainedParamType>,
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
                ingest_rbs_source(&mut builder, name, contents);
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
        for dir in sig_dirs {
            ingest_rbs_dir(&mut builder, dir);
        }
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

        let (
            mut classes,
            toplevel_classes,
            type_alias_defs,
            interface_method_names,
            mut qualified,
            short_to_qualified,
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
                interface_method_names,
                qualified,
                short_to_qualified,
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
                    if self.lookup_on_chain(&chain, method).is_some() {
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
        // Not found across the chain. Only witness absence if the chain is
        // fully loaded; otherwise assume present (zero false positive).
        !complete
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
        for inc in &entry.includes {
            self.collect(inc, &mut order, &mut seen, &mut complete);
        }
        let sup = entry.superclass.or({
            if !entry.is_module && qname != "BasicObject" {
                Some("Object")
            } else {
                None
            }
        });
        if let Some(s) = sup {
            self.collect(s, &mut order, &mut seen, &mut complete);
        }
        if self.lookup_on_chain(&order, method).is_some() {
            return true;
        }
        // Absent across the leaf + its resolvable ancestry: witness only when
        // the chain is fully loaded.
        !complete
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
    pub fn is_module(&self, name: &str) -> bool {
        self.classes.get(name).is_some_and(|e| e.is_module)
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

    /// Enumerate every INSTANCE method name callable on `class_name` — its own
    /// methods plus those inherited over the flattened ancestor chain (superclass
    /// and included modules), plus instance `alias` names. Sorted and deduped.
    /// Empty when the class is unknown. Used by LSP completion (§12); unlike the
    /// diagnostic predicates this is advisory, so it enumerates the full known
    /// surface without a completeness gate.
    pub fn instance_method_names(&self, class_name: &str) -> Vec<&'static str> {
        if !self.classes.contains_key(class_name) {
            return Vec::new();
        }
        let (chain, _complete) = self.ancestors(class_name);
        let mut set: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
        for &anc in &chain {
            if let Some(entry) = self.classes.get(anc) {
                set.extend(entry.methods.keys().copied());
                set.extend(entry.aliases.keys().copied());
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
        // (1) A direct singleton method on any class in the chain.
        for &anc in chain {
            if let Some(entry) = self.classes.get(anc) {
                if entry.singleton_methods.contains_key(method) {
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
                        if self.lookup_on_chain(&mod_chain, method).is_some() {
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
        self.singleton_method_return_inner(class_name, method, 0)
    }

    /// Whether `class_name#method`'s author-declared RBS return is `void`
    /// (ADR-100, consumed by `static.value-use.void`). Resolved at the FIRST
    /// ancestor that defines the method — the same first-definer-wins walk
    /// `method_return` rides — so an override that redeclares a non-void
    /// return correctly reads non-void. Aliases are not chased (under-emit).
    pub fn method_return_is_void(&self, class_name: &str, method: &str) -> bool {
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

    /// Resolve a method's arity envelope over the ancestor chain (first defining
    /// ancestor wins), resolving through `alias` definitions. `None` if the
    /// method is unknown on the chain.
    pub fn method_arity(&self, class_name: &str, method: &str) -> Option<Arity> {
        let (chain, _) = self.ancestors(class_name);
        self.lookup_on_chain(&chain, method).map(|(_, arity, _)| arity)
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
    /// - `Other` ⇒ every remaining shape (the reference's `else`: bases incl.
    ///   `nil`/`bool`/`void`/`self`/`top`/`untyped`, type variables, literals,
    ///   tuples, records, procs, intersections) admits conservatively.
    pub fn param_admits_nil(&self, t: &RetainedParamType) -> bool {
        self.param_admits_nil_depth(t, 0)
    }

    fn param_admits_nil_depth(&self, t: &RetainedParamType, depth: usize) -> bool {
        match t {
            RetainedParamType::ClassInstance(name) => {
                let bare = name.strip_prefix("::").unwrap_or(name);
                NIL_COMPATIBLE_CLASS_NAMES.contains(&bare)
            }
            RetainedParamType::Alias(name) => {
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
            RetainedParamType::Interface(name) => self.interface_admits_nil(name),
            RetainedParamType::Union(members) => {
                members.iter().any(|m| self.param_admits_nil_depth(m, depth))
            }
            RetainedParamType::Optional(_) => true,
            RetainedParamType::Other(_) => true,
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
    /// - `Optional` / `Other` ⇒ accept conservatively (the reference `else`).
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
            RetainedParamType::ClassInstance(name) => {
                matches!(
                    self.class_ordering(arg_class, name),
                    ClassOrdering::Equal
                        | ClassOrdering::Subclass
                        | ClassOrdering::Superclass
                        | ClassOrdering::Unknown
                )
            }
            RetainedParamType::Alias(name) => {
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
            RetainedParamType::Interface(name) => self.interface_accepts_arg(name, arg_class),
            RetainedParamType::Union(members) => members
                .iter()
                .any(|m| self.param_accepts_arg_class_depth(m, arg_class, depth)),
            RetainedParamType::Optional(_) => true,
            RetainedParamType::Other(_) => true,
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
    ) -> Option<(Option<&'static str>, Arity, bool)> {
        self.lookup_on_chain_depth(chain, method, 0)
    }

    fn lookup_on_chain_depth(
        &self,
        chain: &[&'static str],
        method: &str,
        depth: usize,
    ) -> Option<(Option<&'static str>, Arity, bool)> {
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
            let mut m: HashMap<&'static str, (Option<&'static str>, Arity, bool)> = HashMap::new();
            for (n, ret, ar) in methods {
                // The stub never models nilable returns (no `?` shapes here) ⇒
                // `false` keeps the fallback conservative: nothing mints
                // `T | nil`, so `possible-nil-receiver` stays silent under it.
                m.insert(n, (*ret, *ar, false));
            }
            classes.insert(
                name,
                ClassEntry {
                    methods: m,
                    // The stub declares no `-> void` returns.
                    void_methods: HashSet::new(),
                    void_singleton_methods: HashSet::new(),
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
            interface_method_names: HashMap::new(),
            // ADR-0042 Slice 1: the stub models no qualified registry either —
            // empty keeps the new accessors conservatively inert (never
            // falsely `true`), same as the other ATM-substrate maps above.
            qualified: HashMap::new(),
            short_to_qualified: HashMap::new(),
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
    HashMap<&'static str, ClassEntry>,
    HashMap<&'static str, Vec<&'static str>>,
);

/// Accumulates parsed RBS declarations into per-class entries before flattening.
#[derive(Default)]
struct Builder {
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
}

impl Builder {
    /// Parse one RBS source and fold its top-level class/module declarations in.
    fn ingest(&mut self, code: &str) {
        let Ok(sig) = parse(code) else {
            return;
        };
        for decl in sig.declarations().iter() {
            // `false` = top-level (file-level) declaration: only these may enter
            // the `toplevel_classes` set. `code` is threaded so the ATM substrate
            // can slice verbatim written forms for `RetainedParamType::Other`.
            match decl {
                Node::Class(c) => self.ingest_class(&c, false, &[], code),
                Node::Module(m) => self.ingest_module(&m, false, &[], code),
                Node::TypeAlias(ta) => self.ingest_type_alias(&ta, code),
                Node::Interface(i) => self.ingest_interface(&i),
                _ => {}
            }
        }
    }

    /// Fold one `type X = ...` alias into the global map (ATM substrate). First
    /// write wins on reopen. The RHS is retained one level deep (aliases inside
    /// are kept as `Alias(..)` leaves, not expanded — Slice 2 owns expansion).
    fn ingest_type_alias(&mut self, ta: &TypeAliasNode, code: &str) {
        let Some(name) = type_name_str(&ta.name()) else {
            return;
        };
        let rhs = retained_param_type(&ta.type_(), code, &[]);
        self.type_alias_defs.entry(name).or_insert(rhs);
    }

    /// Fold one `interface _X ... end` into the global map (ATM substrate),
    /// recording its declared instance-method names in declaration order. First
    /// write wins on reopen.
    fn ingest_interface(&mut self, i: &InterfaceNode) {
        let Some(name) = type_name_str(&i.name()) else {
            return;
        };
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

    fn ingest_class(&mut self, c: &ClassNode, nested: bool, enclosing: &[&'static str], code: &str) {
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
        let mut entry = ClassEntry {
            superclass,
            ..Default::default()
        };
        // ADR-0042 Slice 1: this decl's own qualified key, and the enclosing
        // context a NESTED decl within its members will qualify against.
        let qual = qualified_name(enclosing, &tn);
        let child_enclosing: Vec<&'static str> =
            enclosing.iter().copied().chain(std::iter::once(qual)).collect();
        self.collect_members(c.members().iter(), &mut entry, &child_enclosing, code);
        self.merge_qualified(qual, entry.clone());
        self.short_to_qualified_push(name, qual);
        self.merge(name, entry, authoritative);
    }

    fn ingest_module(&mut self, m: &ModuleNode, nested: bool, enclosing: &[&'static str], code: &str) {
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
        let qual = qualified_name(enclosing, &tn);
        let child_enclosing: Vec<&'static str> =
            enclosing.iter().copied().chain(std::iter::once(qual)).collect();
        self.collect_members(m.members().iter(), &mut entry, &child_enclosing, code);
        self.merge_qualified(qual, entry.clone());
        self.short_to_qualified_push(name, qual);
        self.merge(name, entry, authoritative);
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
    /// `entry`. Only instance, public methods are recorded (the existence check
    /// is about instance dispatch; private/singleton are out of scope here).
    fn collect_members<'a>(
        &mut self,
        members: impl Iterator<Item = Node<'a>>,
        entry: &mut ClassEntry,
        enclosing: &[&'static str],
        code: &str,
    ) {
        for member in members {
            match member {
                Node::MethodDefinition(md) => {
                    let mname = intern(md.name().as_str());
                    let (ret, arity, nilable, ret_instance, ret_self, ret_void) =
                        method_signature(&md);
                    let block_ret = block_overload_return(&md);
                    // The structured TUPLE return the flat `ret` slot above
                    // collapses to `None` (Slice 2 of the MultiWrite substrate).
                    let tuple_ret = tuple_return(&md);
                    let kind = md.kind();
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
                                .push((mname, method_overloads(&md, code)));
                        } else {
                            entry
                                .method_overloads
                                .entry(mname)
                                .or_insert_with(|| method_overloads(&md, code));
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
                        // ATM substrate: retain the class-method per-overload
                        // shapes so `call.argument-type-mismatch` can check a
                        // `CGI.parse(...)` class-method call site. Overloading
                        // reopens go aside, mirroring the instance path.
                        if md.overloading() {
                            entry
                                .overloading_singleton_overloads
                                .push((mname, method_overloads(&md, code)));
                        } else {
                            entry
                                .singleton_method_overloads
                                .entry(mname)
                                .or_insert_with(|| method_overloads(&md, code));
                        }
                    }
                }
                Node::Include(inc) => {
                    if let Some(modname) = type_name_str(&inc.name()) {
                        if !entry.includes.contains(&modname) {
                            entry.includes.push(modname);
                        }
                    }
                }
                Node::Extend(ext) => {
                    // `extend M` folds M's INSTANCE methods into this class
                    // object's SINGLETON surface (e.g. `SecureRandom extend
                    // Random::Formatter` ⇒ `SecureRandom.hex`). Record the
                    // module name; the singleton lookup resolves it conservatively
                    // (an unknown extended module ⇒ surface incomplete ⇒ silent).
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
                // A NESTED class/module declaration (e.g. `module PP; module
                // ObjectMixin; end; end`) must be registered too, by its simple
                // name — otherwise an `include` that references it leaves the
                // ancestor chain "incomplete", and the conservative gate would
                // stop witnessing absence for EVERY class whose chain passes
                // through the reopened owner (e.g. `Object include PP::ObjectMixin`
                // ⇒ all typo detection silently disabled). Registering nested
                // types by simple name keeps chains complete. (Simple-name
                // collisions only ever ADD methods, never witness false absence.)
                Node::Class(inner) => self.ingest_class(&inner, true, enclosing, code),
                Node::Module(inner) => self.ingest_module(&inner, true, enclosing, code),
                // A NESTED `type X = ...` / `interface _X ... end` folds into the
                // SAME global maps as a top-level one (ATM substrate), keyed by
                // simple name — consistent with how nested classes/modules are
                // registered by simple name above.
                Node::TypeAlias(ta) => self.ingest_type_alias(&ta, code),
                Node::Interface(i) => self.ingest_interface(&i),
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
        }
        slot.is_module |= entry.is_module;
        for (k, v) in entry.methods {
            let newly = !slot.methods.contains_key(k);
            slot.methods.entry(k).or_insert(v);
            if newly && entry.void_methods.contains(k) {
                slot.void_methods.insert(k);
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
            self.interface_method_names,
            self.qualified,
            self.short_to_qualified,
        )
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
fn qualified_name(enclosing: &[&'static str], tn: &ruby_rbs::node::TypeNameNode) -> &'static str {
    let mut parts: Vec<String> = enclosing.iter().map(|s| (*s).to_string()).collect();
    for seg in tn.namespace().path().iter() {
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
) -> (Option<&'static str>, Arity, bool, bool, bool, bool) {
    let mut min: Option<usize> = None;
    let mut max: Option<usize> = Some(0);
    let mut variadic = false;
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
            continue;
        };

        let required = ft.required_positionals().iter().count();
        let optional = ft.optional_positionals().iter().count();
        let has_rest = ft.rest_positionals().is_some();

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
            Node::ClassInstanceType(ci) => (type_name_str(&ci.name()), false, false, false, false),
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
                Node::ClassInstanceType(ci) => (type_name_str(&ci.name()), true, false, false, false),
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

    let arity = (min.unwrap_or(0), if variadic { None } else { max });
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
fn tuple_return(md: &ruby_rbs::node::MethodDefinitionNode) -> Option<Vec<RbsReturnShape>> {
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
            Node::TupleType(t) => Some(t.types().iter().map(|e| return_shape(&e)).collect()),
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
fn return_shape(node: &Node) -> RbsReturnShape {
    match node {
        Node::ClassInstanceType(ci) => match written_type_name(&ci.name()) {
            Some(name) => RbsReturnShape::Class(name),
            None => RbsReturnShape::Unknown,
        },
        Node::TupleType(t) => {
            RbsReturnShape::Tuple(t.types().iter().map(|e| return_shape(&e)).collect())
        }
        _ => RbsReturnShape::Unknown,
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
        // T] (I index) { (I index) -> T } -> (E | T)`). A bare variable is an
        // opaque `Other` leaf, which the acceptance walk admits — so the whole
        // overload would accept any argument and silence the mismatch every
        // other overload reports. The declared upper bound IS the constraint
        // (the reference renders exactly it: `expected int | _ToInt`), so a
        // bounded variable resolves to its bound. An UNbounded variable stays
        // opaque — genuinely unconstrained, genuinely admits everything.
        let bounds = method_type_param_bounds(&mt, code);
        let required_positionals = ft
            .required_positionals()
            .iter()
            .map(|p| param_node_type(&p, code, &bounds))
            .collect();
        let optional_positionals = ft
            .optional_positionals()
            .iter()
            .map(|p| param_node_type(&p, code, &bounds))
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
        });
    }
    out
}

/// The upper bounds a method type declares for its own type parameters, as
/// `(variable name, bound)` pairs — `[I < _ToInt, T]` yields one entry for `I`
/// and none for `T`. A `Vec` rather than a map: a method type binds a handful of
/// parameters at most, so a linear scan beats hashing.
type TypeParamBounds = [(&'static str, RetainedParamType)];

/// Collect the bounded type parameters of one method type (see the call site in
/// [`method_overloads`]). Unbounded parameters are omitted entirely, so a lookup
/// miss means "genuinely unconstrained".
fn method_type_param_bounds(
    mt: &ruby_rbs::node::MethodTypeNode,
    code: &str,
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
            Some((intern(name), retained_param_type(&bound, code, &[])))
        })
        .collect()
}

/// Resolve a positional-parameter node (`RBS::Types::Function::Param`, whose
/// `.type_()` is the parameter's type) into a one-level [`RetainedParamType`].
fn param_node_type(param: &Node, code: &str, bounds: &TypeParamBounds) -> RetainedParamType {
    match param {
        Node::FunctionParam(fp) => retained_param_type(&fp.type_(), code, bounds),
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

/// Lower one RBS type node to a one-level [`RetainedParamType`] tag (ATM
/// substrate). The four named kinds — class instance, `type` alias, `interface`,
/// and the two structural wrappers `Union` / `Optional` — are recognised; every
/// other shape collapses to [`RetainedParamType::Other`] carrying the verbatim
/// written form sliced from `code`. Only `Union`/`Optional` recurse (they are
/// meaningless as opaque leaves); a `ClassInstance` drops its type arguments.
///
/// `bounds` carries the enclosing method type's bounded type parameters: a
/// variable listed there resolves to its upper bound instead of collapsing to an
/// (admit-everything) `Other` leaf.
fn retained_param_type(node: &Node, code: &str, bounds: &TypeParamBounds) -> RetainedParamType {
    match node {
        Node::VariableType(v) => {
            let name = v.name();
            match bounds.iter().find(|(n, _)| *n == name.as_str()) {
                Some((_, bound)) => bound.clone(),
                None => RetainedParamType::Other(node_written_form(node, code)),
            }
        }
        Node::ClassInstanceType(ci) => match type_name_str(&ci.name()) {
            Some(name) => RetainedParamType::ClassInstance(name),
            None => RetainedParamType::Other(node_written_form(node, code)),
        },
        Node::AliasType(a) => match type_name_str(&a.name()) {
            Some(name) => RetainedParamType::Alias(name),
            None => RetainedParamType::Other(node_written_form(node, code)),
        },
        Node::InterfaceType(i) => match type_name_str(&i.name()) {
            Some(name) => RetainedParamType::Interface(name),
            None => RetainedParamType::Other(node_written_form(node, code)),
        },
        Node::UnionType(u) => RetainedParamType::Union(
            u.types().iter().map(|t| retained_param_type(&t, code, bounds)).collect(),
        ),
        Node::OptionalType(o) => RetainedParamType::Optional(Box::new(retained_param_type(
            &o.type_(),
            code,
            bounds,
        ))),
        other => RetainedParamType::Other(node_written_form(other, code)),
    }
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
fn block_overload_return(md: &ruby_rbs::node::MethodDefinitionNode) -> Option<&'static str> {
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
            Node::ClassInstanceType(ci) => type_name_str(&ci.name()),
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
mod embedded_tests {
    use super::*;

    /// The build-time-embedded set is present and carries the core classes.
    #[test]
    fn embedded_rbs_non_empty_with_core_files() {
        assert!(!EMBEDDED_RBS.is_empty(), "EMBEDDED_RBS must not be empty");
        let has = |needle: &str| EMBEDDED_RBS.iter().any(|(p, _)| p.ends_with(needle));
        assert!(has("core/array.rbs"), "missing core/array.rbs");
        assert!(has("core/string.rbs"), "missing core/string.rbs");
        // A stdlib closure member.
        assert!(
            EMBEDDED_RBS.iter().any(|(p, _)| p.contains("stdlib/pathname/")),
            "missing stdlib/pathname"
        );
        // Entries are non-empty file contents.
        assert!(EMBEDDED_RBS.iter().all(|(_, c)| !c.is_empty()));
    }

    /// `CoreData::load()` with `RIGOR_RBS_CORE_DIR` UNSET ingests the embedded
    /// set (NOT a stub): it knows the core roots and stdlib-closure classes, and
    /// resolves instance methods with the same parity the runtime path gives.
    ///
    /// NB: this asserts on the global `load()` and so must not clobber the env
    /// var (other code/tests share the process). It relies on the harness/CI
    /// running with the var unset; if it happens to be set, the override path is
    /// equally valid for these assertions (same signatures), so we don't gate.
    #[test]
    fn embedded_load_is_non_stub_and_method_parity() {
        let idx = CoreData::load();
        // Core classes from the embedded core/ tree.
        assert!(idx.knows_class("String"));
        assert!(idx.knows_class("Array"));
        assert!(idx.knows_class("Hash"));
        // Stdlib-closure classes (proves the stdlib set embedded, not just core
        // / the small stub): `Pathname` (pathname) and `Set` (in core builtin).
        assert!(idx.knows_class("Pathname"));
        assert!(idx.knows_class("Set"));
        // Method existence parity for a known method, and absence for a typo.
        assert!(idx.class_has_method("String", "upcase"));
        assert!(!idx.class_has_method("String", "lenght"));
    }

    /// The rigor-owned `overlay/` tree is embedded and ingested — the
    /// reference's `data/vendored_gem_sigs/` and `data/core_overlay/`, which it
    /// loads in EVERY run. Without them rigor-rs's surface is strictly weaker
    /// than the oracle's and reports methods the oracle resolves — measured on
    /// rigor-survey as `::DidYouMean.formatter`.
    #[test]
    fn embedded_overlay_is_loaded() {
        assert!(
            EMBEDDED_RBS.iter().any(|(p, _)| p.starts_with("overlay/")),
            "overlay tree not embedded"
        );
        let idx = CoreData::load();
        // `data/vendored_gem_sigs/did_you_mean/did_you_mean_extras.rbs` — the
        // upstream rbs `stdlib/did_you_mean` declares neither.
        assert!(idx.class_has_singleton_method("DidYouMean", "formatter"));
        assert!(idx.class_has_singleton_method("DidYouMean", "correct_error"));
        // A genuine typo on the same module still witnesses absent.
        assert!(!idx.class_has_singleton_method("DidYouMean", "totally_bogus_name"));
        // `prism` is deliberately NOT copied: its file supplements the prism
        // gem's own sig/, which this tree does not vendor, so loading it alone
        // would declare `module Prism` WITHOUT `Prism.parse` and witness a false
        // absence there. See vendor/rbs/PROVENANCE.md.
        assert!(
            !EMBEDDED_RBS.iter().any(|(p, _)| p.contains("overlay/vendored_gem_sigs/prism")),
            "prism supplement must stay out of the overlay"
        );
    }

    /// Step 1 (nilable-RBS-return): an `Optional` return (`String?`) is
    /// preserved as `(class, nilable=true)`; a plain return is `(class, false)`;
    /// and overloads that DISAGREE on nilability collapse to `None` (never
    /// invent nil). `byteslice` is uniformly `-> String?`; `upcase` is plainly
    /// `-> String`; `try_convert`'s overloads mix `String` and `String?`.
    #[test]
    fn nilable_return_preserved_and_conservative() {
        let idx = CoreData::load();
        // Nilable return: `String#byteslice : (...) -> String?` ⇒ (String, true).
        assert_eq!(
            idx.method_return_nilable("String", "byteslice"),
            Some(("String", true)),
            "byteslice's String? must surface nilable=true"
        );
        // Plain return: `String#upcase : () -> String` ⇒ (String, false).
        assert_eq!(
            idx.method_return_nilable("String", "upcase"),
            Some(("String", false)),
            "upcase's plain String must surface nilable=false"
        );
        // Disagreeing overloads (String vs String?) ⇒ conservative None.
        assert_eq!(
            idx.method_return_nilable("String", "try_convert"),
            None,
            "overloads disagreeing on nilability must collapse to None"
        );
        // The existing non-nil accessors are unchanged by the new bit.
        assert_eq!(idx.method_return("String", "upcase"), Some("String"));
        assert_eq!(idx.method_return("String", "byteslice"), Some("String"));
    }

    /// rbs 4.1 rewrote several core returns from a spelled-out generic to the
    /// late-bound `instance` (`Hash#compact: () -> ::Hash[K, V]` became `() ->
    /// instance`). On an INSTANCE method that means "an instance of the
    /// receiver's class", so the return must still resolve to a concrete class —
    /// otherwise a chained call (`h.compact.presence`) types Dynamic and every
    /// rule that rides the receiver type goes silent.
    #[test]
    fn instance_return_resolves_to_the_receiver_class() {
        let idx = CoreData::load();
        assert_eq!(
            idx.method_return("Hash", "compact"),
            Some("Hash"),
            "`-> instance` on Hash#compact must resolve to the receiver class"
        );
        // The sentinel must never escape as a class name, on any accessor.
        assert_eq!(idx.method_return_nilable("Hash", "compact"), Some(("Hash", false)));
        assert_eq!(idx.declared_instance_return("Hash", "compact"), Some(Some("Hash")));
        // A receiver the index does not model declines to None (⇒ Dynamic).
        assert_eq!(idx.method_return("NoSuchClassZzz", "compact"), None);
    }

    /// `-> self` on an INSTANCE method is the receiver itself — the spelling
    /// core uses for the mutating and re-tagging families. It was tracked only
    /// on the singleton path, so an instance method's `-> self` collapsed to
    /// `Dynamic` and broke every chain running through it (measured: gitlab-foss
    /// `[error.message].concat(error.backtrace).join("\n").truncate(N)`).
    #[test]
    fn self_return_on_an_instance_method_resolves_to_the_receiver() {
        let idx = CoreData::load();
        assert_eq!(idx.method_return("Array", "concat"), Some("Array"));
        assert_eq!(idx.method_return("Array", "push"), Some("Array"));
        assert_eq!(idx.method_return("String", "force_encoding"), Some("String"));
        // Resolved against the RECEIVER, so a subclass keeps its own type
        // rather than widening to the class that declared the method.
        assert_eq!(idx.method_return("Symbol", "freeze"), Some("Symbol"));
        // A singleton `-> self` is the class OBJECT, which this flat slot
        // cannot spell — it must keep declining instead of being folded like
        // the instance case.
        assert_eq!(idx.singleton_method_return("Struct", "new"), None);
    }

    /// rbs 4.1 also started using BOUNDED method type parameters in core
    /// signatures (`Array#fetch`'s block overload spells its index `[I < _ToInt,
    /// T] (I index)`). A bare variable is an opaque `Other` leaf that admits
    /// every argument, which would silence the mismatch the other overloads
    /// report — so a bounded variable must resolve to its declared bound.
    #[test]
    fn bounded_method_type_param_resolves_to_its_upper_bound() {
        let idx = CoreData::load();
        let overloads = idx.method_overloads("Array", "fetch").expect("Array#fetch");
        let params: Vec<&RetainedParamType> = overloads
            .iter()
            .filter_map(|o| o.required_positionals.first())
            .collect();
        assert!(
            params.iter().any(|p| matches!(p, RetainedParamType::Interface("_ToInt"))),
            "the bounded `I < _ToInt` index param must carry its bound: {params:?}"
        );
        assert!(
            !params.iter().any(|p| matches!(p, RetainedParamType::Other(s) if s == "I")),
            "no overload may keep the bare variable as an admit-everything leaf: {params:?}"
        );
    }

    /// ADR-0033: an empty `sig_dirs` is byte-identical to `load_with_plugins`
    /// (the gating contract) — the no-`sig/` path must be unchanged.
    #[test]
    fn empty_project_sig_is_unchanged() {
        let base = CoreData::load_with_plugins(&[]);
        let with_sig = CoreData::load_for_project(&[], &[]);
        assert_eq!(base.class_count(), with_sig.class_count());
        // A named-but-absent dir is inert too (ingestion skips a non-directory).
        let absent = CoreData::load_for_project(
            &[],
            &[std::path::PathBuf::from("this-dir-does-not-exist-xyzzy")],
        );
        assert_eq!(base.class_count(), absent.class_count());
    }

    /// ADR-14 slice 10: the sig-gen-only precise declared-return accessors are
    /// three-valued and never "assume present". `Object#hash → Integer` resolves
    /// through the ancestor chain (instance AND — via the class object's own
    /// ancestry — singleton), an absent method on a complete chain is `None`
    /// (NotDeclared), and an unresolvable-return method is `Some(None)`.
    #[test]
    fn sig_gen_declared_return_accessors_are_three_valued() {
        let idx = CoreData::load();
        // Instance: declared, concrete return.
        assert_eq!(idx.declared_instance_return("String", "upcase"), Some(Some("String")));
        assert_eq!(idx.declared_instance_return("Object", "hash"), Some(Some("Integer")));
        // Instance: declared, but the return is not a single bare concrete class
        // (`Integer#times` returns an Enumerator/self union) ⇒ Some(None).
        assert_eq!(idx.declared_instance_return("Integer", "times"), Some(None));
        // Instance: not declared on a fully-loaded chain ⇒ None (NotDeclared).
        assert_eq!(idx.declared_instance_return("String", "definitely_absent_zzz"), None);
        // An unknown class ⇒ None (the SigEnv gates on presence first).
        assert_eq!(idx.declared_instance_return("NoSuchClassZzz", "foo"), None);

        // Singleton: the class object inherits `Object#hash` (Integer) through
        // its `Class`/`Module`/`Object` ancestry.
        assert_eq!(idx.declared_singleton_return("String", "hash"), Some(Some("Integer")));
        // Singleton: an absent class method on a complete surface ⇒ None.
        assert_eq!(idx.declared_singleton_return("String", "definitely_absent_zzz"), None);

        // Chain completeness: a fully-loaded core class is complete.
        assert!(idx.chain_complete("String"));
        assert!(!idx.chain_complete("NoSuchClassZzz"));
    }

    /// ADR-0033: a project `sig/` dir's classes join the known set (so the
    /// dispatch rules can witness them) and their methods resolve, while a typo
    /// on such a class is witnessed-absent — exactly the reference's
    /// `rbs_class_known?` behaviour. Uses a real temp dir since ingestion is
    /// filesystem-driven (`ingest_rbs_dir`).
    #[test]
    fn project_sig_widens_known_classes() {
        let base = std::env::temp_dir()
            .join(format!("rigor-sig-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("temp sig dir");
        std::fs::write(
            base.join("widget.rbs"),
            "class Widget\n  def spin: () -> Integer\nend\n",
        )
        .expect("write sig");

        let data = CoreData::load_for_project(&[], std::slice::from_ref(&base));
        assert!(data.knows_class("Widget"), "project class joins knows_class");
        assert!(data.knows_toplevel_class("Widget"), "declared at top level");
        assert!(data.class_has_method("Widget", "spin"), "declared method resolves");
        // A typo is witnessed-absent (the whole ancestor chain — Widget + Object
        // — is loaded, so absence is decidable), the coverage this leg unlocks.
        assert!(!data.class_has_method("Widget", "spni"));
        // Core classes are unaffected by the project ingest.
        assert!(data.knows_class("String"));
        assert!(data.class_has_method("String", "upcase"));

        let _ = std::fs::remove_dir_all(&base);
    }

    // -- ATM shared-substrate (Slice 1) retention tests ------------------------

    /// Per-overload retention on a real multi-overload core method: the vendored
    /// core `Integer#+` has FOUR overloads (`Integer`/`Float`/`Rational`/
    /// `Complex`), and the stdlib `bigdecimal` reopen (`def +: (BigDecimal) ->
    /// BigDecimal | ...` — an OVERLOADING def whose trailing `...` appends the
    /// existing overloads) PREPENDS a fifth. Each has a single required
    /// positional whose type is the concrete numeric class; the reference's
    /// multi-overload label renders them in exactly this order (`5 + nil` ⇒
    /// `expected BigDecimal | Integer | Float | Rational | Complex`). The merged
    /// arity path (unchanged) still sees one `(1, Some(1))` envelope.
    #[test]
    fn atm_per_overload_retention_integer_plus() {
        let idx = CoreData::load();
        let ov = idx
            .method_overloads("Integer", "+")
            .expect("Integer#+ has retained overloads");
        assert_eq!(ov.len(), 5, "Integer#+ has five overloads (bigdecimal reopen + core four)");
        let names: Vec<&RetainedParamType> = ov
            .iter()
            .map(|o| {
                assert_eq!(o.required_positionals.len(), 1, "one required positional");
                assert!(o.optional_positionals.is_empty());
                assert!(!o.has_rest_positionals);
                assert!(!o.has_required_keywords);
                assert!(!o.has_optional_keywords);
                assert!(!o.has_rest_keywords);
                assert!(!o.has_trailing_positionals);
                &o.required_positionals[0]
            })
            .collect();
        assert_eq!(
            names,
            vec![
                &RetainedParamType::ClassInstance("BigDecimal"),
                &RetainedParamType::ClassInstance("Integer"),
                &RetainedParamType::ClassInstance("Float"),
                &RetainedParamType::ClassInstance("Rational"),
                &RetainedParamType::ClassInstance("Complex"),
            ],
            "the overloading reopen's operand comes first, then the core four"
        );
        // The merged arity envelope is untouched (additive-retention contract).
        assert_eq!(idx.method_arity("Integer", "+"), Some((1, Some(1))));
        // Param-NAME retention (message prefix substrate): `String#+` declares
        // `(string other_string)`, so the name rides alongside the type.
        let plus = idx.method_overloads("String", "+").expect("String#+ retained");
        assert_eq!(plus.len(), 1);
        assert_eq!(plus[0].required_positional_names, vec![Some("other_string")]);
    }

    /// Type-alias retention against the ACTUAL vendored RBS: `builtin.rbs`
    /// declares `type string = String | _ToStr`, so `resolve_type_alias("string")`
    /// is a two-arm union of the concrete `String` class and the `_ToStr`
    /// interface — retained RAW (the interface is a leaf, not expanded).
    #[test]
    fn atm_type_alias_retention_string() {
        let idx = CoreData::load();
        let rhs = idx
            .resolve_type_alias("string")
            .expect("`type string` is retained");
        assert_eq!(
            rhs,
            &RetainedParamType::Union(vec![
                RetainedParamType::ClassInstance("String"),
                RetainedParamType::Interface("_ToStr"),
            ]),
            "type string = String | _ToStr"
        );
        // A leading `::` on the query is tolerated.
        assert!(idx.resolve_type_alias("::string").is_some());
    }

    /// Interface method-name retention against the ACTUAL vendored RBS:
    /// `interface _ToStr; def to_str: () -> String; end` ⇒ `["to_str"]`.
    #[test]
    fn atm_interface_method_names_to_str() {
        let idx = CoreData::load();
        assert_eq!(
            idx.interface_methods("_ToStr"),
            Some(["to_str"].as_slice()),
            "_ToStr requires exactly to_str"
        );
        // A richer interface (`_Each` requires `each`) is retained too.
        let each = idx.interface_methods("_Each").expect("_Each retained");
        assert!(each.contains(&"each"), "_Each requires each");
    }

    /// Keyword / rest / optional / trailing presence flags are set from the real
    /// RBS. `String#gsub` has an overload with optional positionals and one with
    /// a block; `Hash#merge` takes a rest positional. We assert the flags rather
    /// than pin exact overload indices (which vary with the vendored RBS).
    #[test]
    fn atm_presence_flags_from_real_rbs() {
        let idx = CoreData::load();
        // `String#*` : (int) -> String — a single required positional, no rest.
        let star = idx.method_overloads("String", "*").expect("String#* overloads");
        assert!(star.iter().all(|o| !o.has_rest_positionals));
        // `Array#push` / `Array#concat` take rest positionals (`*T`).
        let push = idx.method_overloads("Array", "push").expect("Array#push overloads");
        assert!(
            push.iter().any(|o| o.has_rest_positionals),
            "Array#push declares a rest positional"
        );
        // Optional positional retention: `String#chomp : (?string) -> String`.
        let chomp = idx.method_overloads("String", "chomp").expect("String#chomp overloads");
        assert!(
            chomp.iter().any(|o| !o.optional_positionals.is_empty()),
            "String#chomp has an optional positional overload"
        );
    }

    /// Overloads resolve over the ancestor chain AND through instance aliases,
    /// exactly like the merged lookups. `Integer` inherits nothing for `+` (own
    /// method); test an inherited case: `Integer#succ` is own, but `String#size`
    /// is `alias size length` ⇒ overloads resolve via the alias target.
    #[test]
    fn atm_overloads_resolve_via_alias_and_chain() {
        let idx = CoreData::load();
        // `String#size` is `alias size length`; overloads resolve to `length`'s.
        let size = idx.method_overloads("String", "size");
        let length = idx.method_overloads("String", "length");
        assert!(size.is_some(), "aliased size resolves to length's overloads");
        assert_eq!(size, length, "size and length share the overload set");
        // Unknown method ⇒ None.
        assert!(idx.method_overloads("String", "definitely_absent_zzz").is_none());
        // Unknown class ⇒ None.
        assert!(idx.method_overloads("NoSuchClassZzz", "foo").is_none());
    }

    /// Class-method (singleton) overload retention — the ATM substrate for
    /// `CGI.parse(...)` / `Base64.decode64(...)`. `CGI.parse` is a plain
    /// `def self.parse: (String query) -> ...`, so it lives ONLY in the singleton
    /// overload table (the instance `method_overloads` does not carry it).
    #[test]
    fn atm_singleton_method_overloads_retained() {
        let idx = CoreData::load();
        // Only assert when the stdlib RBS is actually loaded (a stub build has no
        // CGI); this keeps the test meaningful without failing a Ruby-free CI.
        if idx.knows_class("CGI") {
            let parse = idx
                .singleton_method_overloads("CGI", "parse")
                .expect("CGI.parse singleton overloads retained");
            assert!(
                parse
                    .iter()
                    .any(|o| o.required_positionals.len() == 1
                        && matches!(o.required_positionals[0], RetainedParamType::ClassInstance("String"))),
                "CGI.parse takes a single String positional: {parse:?}"
            );
            // A plain `def self.parse` is NOT an instance method.
            assert!(
                idx.method_overloads("CGI", "parse").is_none(),
                "CGI#parse is not an instance method"
            );
        }
        // Unknown singleton method / class ⇒ None.
        assert!(idx.singleton_method_overloads("Array", "definitely_absent_zzz").is_none());
        assert!(idx.singleton_method_overloads("NoSuchClassZzz", "foo").is_none());
    }

    /// Alias / interface cycle guard: a self-referential `type` alias
    /// (`type loop_t = loop_t`) and a mutual pair are ingested WITHOUT any
    /// build-time loop (the RHS is retained one level deep), and their raw tags
    /// come back as `Alias(..)` leaves. Uses a project `sig/` dir since ingestion
    /// is filesystem/parser-driven.
    #[test]
    fn atm_alias_cycle_guard_and_nested_retention() {
        let base = std::env::temp_dir()
            .join(format!("rigor-atm-cycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("temp sig dir");
        std::fs::write(
            base.join("cyclic.rbs"),
            "type loop_t = loop_t\n\
             type a_t = b_t\n\
             type b_t = a_t\n\
             interface _Spinner\n  def spin: () -> Integer\n  def spin: () -> Integer\nend\n",
        )
        .expect("write sig");

        let data = CoreData::load_for_project(&[], std::slice::from_ref(&base));
        // No hang at ingestion; the self-cycle is retained as a raw Alias leaf.
        assert_eq!(
            data.resolve_type_alias("loop_t"),
            Some(&RetainedParamType::Alias("loop_t")),
            "self-referential alias retained one level deep, no loop"
        );
        assert_eq!(
            data.resolve_type_alias("a_t"),
            Some(&RetainedParamType::Alias("b_t")),
            "mutual alias retained raw (Slice 2 owns expansion)"
        );
        // Interface with a duplicate method decl dedups to one name.
        assert_eq!(
            data.interface_methods("_Spinner"),
            Some(["spin"].as_slice()),
            "duplicate interface method decls dedup"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The retention is ADDITIVE: an empty `sig_dirs` load still has the same
    /// class count, and the substrate accessors are populated (non-degenerate)
    /// on the embedded set — a coarse guard that ingestion actually ran.
    #[test]
    fn atm_substrate_is_additive_and_populated() {
        let idx = CoreData::load();
        // The type-alias and interface tables are non-empty on the real RBS.
        assert!(idx.resolve_type_alias("string").is_some());
        assert!(idx.interface_methods("_ToStr").is_some());
        // A stdlib alias is present too (`type Pathname::glob_pattern` etc. vary;
        // assert the core `boolish` alias which builtin.rbs declares).
        assert!(
            idx.resolve_type_alias("boolish").is_some(),
            "core `type boolish` alias retained"
        );
    }

    // -- ATM Slice 2: acceptance-walk predicates ------------------------------

    /// `ClassInstance` nil-admittance: the closed `NIL_COMPATIBLE` set admits,
    /// every other concrete class rejects. `Object` (a universal nil ancestor)
    /// admits; `String` does not — the load-bearing case (`"a" + nil` fires).
    #[test]
    fn atm_param_admits_nil_class_instance() {
        let idx = CoreData::load();
        assert!(idx.param_admits_nil(&RetainedParamType::ClassInstance("Object")));
        assert!(idx.param_admits_nil(&RetainedParamType::ClassInstance("BasicObject")));
        assert!(idx.param_admits_nil(&RetainedParamType::ClassInstance("Kernel")));
        assert!(idx.param_admits_nil(&RetainedParamType::ClassInstance("NilClass")));
        // A leading `::` is tolerated on the name.
        assert!(idx.param_admits_nil(&RetainedParamType::ClassInstance("::Object")));
        // Concrete classes reject nil.
        assert!(!idx.param_admits_nil(&RetainedParamType::ClassInstance("String")));
        assert!(!idx.param_admits_nil(&RetainedParamType::ClassInstance("Integer")));
    }

    /// `Other` forms all admit nil (the reference `else`: `nil`/`untyped`/`self`/
    /// `bool`/literals/type variables …), and `Optional` always admits.
    #[test]
    fn atm_param_admits_nil_other_and_optional() {
        let idx = CoreData::load();
        for form in ["nil", "untyped", "self", "bool", "top", "void", "1", "\"x\"", ":sym", "T"] {
            assert!(
                idx.param_admits_nil(&RetainedParamType::Other(form.to_string())),
                "Other({form:?}) admits nil conservatively"
            );
        }
        assert!(idx.param_admits_nil(&RetainedParamType::Optional(Box::new(
            RetainedParamType::ClassInstance("String")
        ))));
    }

    /// `Union` admits nil iff ANY member does. `String | Integer` rejects (both
    /// concrete non-nil); `String | Object` admits (via `Object`).
    #[test]
    fn atm_param_admits_nil_union() {
        let idx = CoreData::load();
        assert!(!idx.param_admits_nil(&RetainedParamType::Union(vec![
            RetainedParamType::ClassInstance("String"),
            RetainedParamType::ClassInstance("Integer"),
        ])));
        assert!(idx.param_admits_nil(&RetainedParamType::Union(vec![
            RetainedParamType::ClassInstance("String"),
            RetainedParamType::ClassInstance("Object"),
        ])));
    }

    /// The `string` / `int` interface-aliases reject nil: `type string = String
    /// | _ToStr`, and `NilClass` implements neither `to_str` nor `to_int`, so
    /// both arms reject. This is the semantic that makes `"a" + nil` fire
    /// (proven live against the oracle). A hypothetical `_ToS`-shaped interface
    /// admits, since `NilClass#to_s` exists.
    #[test]
    fn atm_param_admits_nil_string_int_aliases() {
        let idx = CoreData::load();
        assert!(
            !idx.param_admits_nil(&RetainedParamType::Alias("string")),
            "`string` (String | _ToStr) rejects nil — NilClass lacks to_str"
        );
        assert!(
            !idx.param_admits_nil(&RetainedParamType::Alias("int")),
            "`int` (Integer | _ToInt) rejects nil — NilClass lacks to_int"
        );
        // NilClass HAS to_s, so a `_ToS` interface param admits nil directly.
        assert!(
            idx.param_admits_nil(&RetainedParamType::Interface("_ToS")),
            "_ToS admits nil — NilClass#to_s exists"
        );
        // An unknown interface admits conservatively.
        assert!(idx.param_admits_nil(&RetainedParamType::Interface("_NoSuchIfaceZzz")));
    }

    /// `ClassInstance` argument acceptance via `class_ordering`: Equal / Subclass
    /// / Superclass / Unknown accept, only a provable `Disjoint` rejects.
    #[test]
    fn atm_param_accepts_arg_class_instance() {
        let idx = CoreData::load();
        // Equal.
        assert!(idx.param_accepts_arg_class(&RetainedParamType::ClassInstance("String"), "String"));
        // Subclass: ArgumentError <: Exception.
        assert!(idx.param_accepts_arg_class(
            &RetainedParamType::ClassInstance("Exception"),
            "ArgumentError"
        ));
        // Superclass: arg Numeric is broader than param Integer — a runtime value
        // MIGHT be an Integer, so admit (never a provable reject).
        assert!(idx.param_accepts_arg_class(
            &RetainedParamType::ClassInstance("Integer"),
            "Numeric"
        ));
        // Unknown: an unloaded param class cannot be refuted.
        assert!(idx.param_accepts_arg_class(
            &RetainedParamType::ClassInstance("NoSuchClassZzz"),
            "String"
        ));
        // Disjoint: the sole rejection.
        assert!(!idx.param_accepts_arg_class(&RetainedParamType::ClassInstance("String"), "Symbol"));
        assert!(!idx.param_accepts_arg_class(
            &RetainedParamType::ClassInstance("Integer"),
            "String"
        ));
    }

    /// The `string` / `int` aliases accept their concrete arm directly and, via
    /// the interface walk, decline to reject a class that implements the
    /// conversion: `int` accepts `Float` because `Float` (over `Numeric`) has
    /// `to_int`. `string` rejects `Symbol` (no `to_str`).
    #[test]
    fn atm_param_accepts_arg_string_int_aliases() {
        let idx = CoreData::load();
        // `string` accepts String (concrete arm, Equal).
        assert!(idx.param_accepts_arg_class(&RetainedParamType::Alias("string"), "String"));
        // `int` accepts Integer (concrete arm) AND Float (via _ToInt: Float has
        // Numeric#to_int) — the interface walk declining to reject a coercible.
        assert!(idx.param_accepts_arg_class(&RetainedParamType::Alias("int"), "Integer"));
        assert!(
            idx.param_accepts_arg_class(&RetainedParamType::Alias("int"), "Float"),
            "`int` accepts Float — Float implements to_int via Numeric"
        );
        // `string` rejects Symbol: Disjoint from String AND no to_str.
        assert!(
            !idx.param_accepts_arg_class(&RetainedParamType::Alias("string"), "Symbol"),
            "`string` rejects Symbol — not a String and no to_str"
        );
    }

    /// Union / Optional / Other acceptance: Union accepts iff any member does;
    /// Optional and Other admit conservatively.
    #[test]
    fn atm_param_accepts_arg_union_optional_other() {
        let idx = CoreData::load();
        // Union: Symbol accepted by the Symbol arm though rejected by String.
        assert!(idx.param_accepts_arg_class(
            &RetainedParamType::Union(vec![
                RetainedParamType::ClassInstance("String"),
                RetainedParamType::ClassInstance("Symbol"),
            ]),
            "Symbol"
        ));
        // Union of two disjoint concretes rejects a third disjoint arg.
        assert!(!idx.param_accepts_arg_class(
            &RetainedParamType::Union(vec![
                RetainedParamType::ClassInstance("String"),
                RetainedParamType::ClassInstance("Symbol"),
            ]),
            "Integer"
        ));
        // Optional / Other admit unconditionally.
        assert!(idx.param_accepts_arg_class(
            &RetainedParamType::Optional(Box::new(RetainedParamType::ClassInstance("String"))),
            "Integer"
        ));
        assert!(idx.param_accepts_arg_class(&RetainedParamType::Other("untyped".to_string()), "Integer"));
    }

    /// Interface acceptance is conservative on the unknown side: an arg class not
    /// RBS-known admits (it MIGHT implement the conversion via metaprogramming),
    /// only a KNOWN class provably lacking a required method rejects.
    #[test]
    fn atm_interface_accepts_arg_unknown_side() {
        let idx = CoreData::load();
        // Unknown arg class → admit.
        assert!(idx.param_accepts_arg_class(
            &RetainedParamType::Interface("_ToStr"),
            "NoSuchClassZzz"
        ));
        // Unknown interface → admit.
        assert!(idx.param_accepts_arg_class(
            &RetainedParamType::Interface("_NoSuchIfaceZzz"),
            "Symbol"
        ));
        // Known arg class lacking the required method → reject.
        assert!(
            !idx.param_accepts_arg_class(&RetainedParamType::Interface("_ToStr"), "Symbol"),
            "_ToStr rejects Symbol — no to_str"
        );
    }

    /// Bounded alias expansion terminates on a cycle (returns conservative true
    /// at the depth cap) and resolves a finite chain to its leaf. Uses a project
    /// `sig/` dir since alias ingestion is parser-driven.
    #[test]
    fn atm_acceptance_alias_depth_cap_and_chain() {
        let base = std::env::temp_dir().join(format!("rigor-atm-s2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("temp sig dir");
        std::fs::write(
            base.join("chains.rbs"),
            "type a_t = b_t\n\
             type b_t = a_t\n\
             type int_t = Integer\n\
             type obj_t = Object\n",
        )
        .expect("write sig");
        let data = CoreData::load_for_project(&[], std::slice::from_ref(&base));

        // Cyclic alias: no hang, admits at the cap (conservative true).
        assert!(
            data.param_admits_nil(&RetainedParamType::Alias("a_t")),
            "cyclic alias terminates and admits at the depth cap"
        );
        assert!(data.param_accepts_arg_class(&RetainedParamType::Alias("a_t"), "String"));

        // Finite chain resolves to its leaf class.
        assert!(
            !data.param_admits_nil(&RetainedParamType::Alias("int_t")),
            "`type int_t = Integer` inherits Integer's nil rejection"
        );
        assert!(
            data.param_admits_nil(&RetainedParamType::Alias("obj_t")),
            "`type obj_t = Object` inherits Object's nil admittance"
        );
        assert!(data.param_accepts_arg_class(&RetainedParamType::Alias("int_t"), "Integer"));
        assert!(!data.param_accepts_arg_class(&RetainedParamType::Alias("int_t"), "String"));

        let _ = std::fs::remove_dir_all(&base);
    }
}

/// ADR-0042 Slice 1: the new qualified-key registry, alongside-additive to the
/// short-key `classes` map. Guarded (`if idx.knows_class("ERB") { ... }`, mirroring
/// `rbs_class_new_types_to_rbs_instance`'s `knows_class("Pathname")` guard) so
/// these don't false-fail if the embedded/override RBS isn't loadable in the
/// sandbox and `CoreData::load()` falls back to the stub (empty qualified maps).
#[cfg(test)]
mod qualified_registry_tests {
    use super::*;

    /// `ERB::Util` is registered under its full qualified key, and BOTH its
    /// `self?.html_escape` singleton and instance halves are visible via the
    /// new own-entry-only accessors (vendored
    /// `stdlib/erb/0/erb.rbs`: `module ERB; module Util; def self?.html_escape`).
    #[test]
    fn erb_util_qualified_singleton_instance_both_true() {
        let idx = CoreData::load();
        if !idx.knows_class("ERB") {
            return; // stub fallback: qualified registry is empty, nothing to assert.
        }
        assert!(idx.knows_qualified_class("ERB::Util"));
        assert!(idx.qualified_declares_instance("ERB::Util", "html_escape"));
        assert!(idx.qualified_declares_singleton("ERB::Util", "html_escape"));
        assert!(!idx.qualified_declares_singleton("ERB::Util", "no_such_method"));
        assert!(!idx.qualified_declares_instance("ERB::Util", "no_such_method"));
    }

    /// The short-key MERGE collision (`ERB::Util` and `CGI::Util` both collapse
    /// onto the shared short key `"Util"` in `classes`) is now SPLIT in the
    /// qualified registry: both are known, DISTINCT entries — an ERB::Util-only
    /// instance method (`html_escape`) is absent from `CGI::Util`, and a
    /// CGI::Util-only instance method (`pretty`, vendored
    /// `stdlib/cgi/0/core.rbs`: `module CGI; module Util; def pretty`) is absent
    /// from `ERB::Util`.
    #[test]
    fn erb_util_and_cgi_util_are_distinct_qualified_entries() {
        let idx = CoreData::load();
        if !idx.knows_class("ERB") || !idx.knows_class("CGI") {
            return;
        }
        assert!(idx.knows_qualified_class("ERB::Util"));
        assert!(idx.knows_qualified_class("CGI::Util"));
        // ERB::Util-only method is not on CGI::Util.
        assert!(idx.qualified_declares_instance("ERB::Util", "html_escape"));
        assert!(!idx.qualified_declares_instance("CGI::Util", "html_escape"));
        // CGI::Util-only method is not on ERB::Util.
        assert!(idx.qualified_declares_instance("CGI::Util", "pretty"));
        assert!(!idx.qualified_declares_instance("ERB::Util", "pretty"));
    }

    /// `resolve_short_unambiguous` collapses to `None` for an AMBIGUOUS short
    /// name (`"Util"` is shared by `ERB::Util` and `CGI::Util`), resolves a
    /// genuinely-unique nested short name (`"DefMethod"`, only
    /// `ERB::DefMethod` in the vendored set) to its single qualified key, and
    /// is `None` for an unknown name.
    #[test]
    fn resolve_short_unambiguous_collapses_ambiguity() {
        let idx = CoreData::load();
        if !idx.knows_class("ERB") || !idx.knows_class("CGI") {
            return;
        }
        assert_eq!(idx.resolve_short_unambiguous("Util"), None);
        assert_eq!(
            idx.resolve_short_unambiguous("DefMethod"),
            Some("ERB::DefMethod")
        );
        assert_eq!(idx.resolve_short_unambiguous("NoSuchNameZZZ"), None);
    }

    /// A genuine top-level class round-trips: its qualified key equals its
    /// short key (no enclosing, no own namespace).
    #[test]
    fn toplevel_class_qualified_equals_short() {
        let idx = CoreData::load();
        if !idx.knows_class("Time") {
            return;
        }
        assert!(idx.knows_qualified_class("Time"));
    }

    /// Regression guard: the EXISTING short-key `knows_class` API is
    /// UNCHANGED by this slice — `"Util"` is still known there too (the
    /// short-key map still holds the merged, collapsed-union composite
    /// exactly as before). This documents that Slice 1 is purely additive.
    #[test]
    fn short_key_map_unchanged_still_knows_util() {
        let idx = CoreData::load();
        if !idx.knows_class("ERB") {
            return;
        }
        assert!(idx.knows_class("Util"));
    }
}

#[cfg(test)]
mod qualified_singleton_witness_tests {
    use super::*;

    /// ADR-0042 Slice 2: `class_has_singleton_method` transparently resolves a
    /// QUALIFIED namespaced receiver via the qualified registry, with the full
    /// base-object surface, so a real method is present and a typo is ABSENT —
    /// and ERB::Util / CGI::Util stay method-disjoint (no short-key merge).
    #[test]
    fn qualified_singleton_witness_split_and_ancestry() {
        let idx = CoreData::load();
        if !idx.knows_qualified_class("ERB::Util") {
            return; // stub fallback (no vendored rbs) — nothing to assert.
        }
        // Real method present (ERB::Util declares `self?.html_escape`).
        assert!(idx.class_has_singleton_method("ERB::Util", "html_escape"));
        // Genuine typo ABSENT (own surface complete + base surface known).
        assert!(!idx.class_has_singleton_method("ERB::Util", "no_such_method"));
        // MERGE-collision split: CGI::Util-only `pretty` is absent on ERB::Util
        // and vice-versa, despite the shared short key "Util".
        assert!(!idx.class_has_singleton_method("ERB::Util", "pretty"));
        assert!(!idx.class_has_singleton_method("CGI::Util", "html_escape"));
        // A base-object method (`name`, from Module) is present on any class obj.
        assert!(idx.class_has_singleton_method("ERB::Util", "name"));
        // A singleton ALIAS resolves (`alias self.h self.html_escape`) — the
        // measured rails FP. Present, not witnessed absent.
        assert!(idx.class_has_singleton_method("ERB::Util", "h"));
        // An unknown qualified name stays silent (assume-present).
        assert!(idx.class_has_singleton_method("No::Such", "whatever"));
        // Measure-first scope: a qualified CLASS (not module) stays SILENT even
        // for a genuine typo — its inherited class-method chain is not walked in
        // this slice (the measured dependabot `Gem::Specification` FP). Use a
        // qualified class known to exist; `Gem::Specification` is heavily
        // reopened in the vendored rbs.
        if idx.knows_qualified_class("Gem::Specification") {
            assert!(idx.class_has_singleton_method("Gem::Specification", "no_such_zzz"));
        }
    }
}

#[cfg(test)]
mod qualified_instance_method_tests {
    use super::*;

    /// ADR-0042 Slice 3: `qualified_class_has_method` resolves the leaf's own
    /// instance surface + ancestry (Object/Kernel/BasicObject via the short
    /// chain) + instance aliases; a genuine typo on a top-level class is
    /// witnessed ABSENT and a real method (incl. an inherited one) present.
    #[test]
    fn qualified_instance_own_ancestry_and_alias() {
        let idx = CoreData::load();
        if !idx.knows_qualified_class("String") {
            return; // stub fallback.
        }
        // Own method present; inherited (Object#frozen?) present; typo absent.
        assert!(idx.qualified_class_has_method("String", "upcase"));
        assert!(idx.qualified_class_has_method("String", "frozen?"));
        assert!(!idx.qualified_class_has_method("String", "no_such_zzz"));
        // An instance alias resolves (`String#size` aliases `length`).
        assert!(idx.qualified_class_has_method("String", "size"));
        // Unknown qualified name ⇒ silent (assume-present).
        assert!(idx.qualified_class_has_method("No::Such", "whatever"));
    }
}

#[cfg(test)]
mod qualified_project_sig_tests {
    use super::*;

    /// ADR-0042 Slice 4: a NESTED project-sig class is tracked by its QUALIFIED
    /// name, so `Outer::Inner.new.spni` witnesses through the qualified path.
    #[test]
    fn qualified_nested_project_sig_provenance_and_witness() {
        let dir = std::env::temp_dir().join("rigor_qual_projsig_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("n.rbs"),
            "module Outer\n  class Inner\n    def spin: () -> Integer\n  end\nend\n",
        )
        .unwrap();
        let idx = CoreData::load_for_project(&[], std::slice::from_ref(&dir));
        // Introduced-by-sig, tracked qualified.
        assert!(idx.is_qualified_project_sig_class("Outer::Inner"));
        assert!(idx.knows_qualified_class("Outer::Inner"));
        // Instance witness over the isolated qualified surface: valid present,
        // typo absent.
        assert!(idx.qualified_class_has_method("Outer::Inner", "spin"));
        assert!(!idx.qualified_class_has_method("Outer::Inner", "spni"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
