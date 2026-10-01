//! Per-run **SourceIndex** (ADR-0023 tier-4 in-source typing): the class
//! structure harvested from the lowered AST so `X.new` can be typed as an
//! instance of a project-defined class, and a typo'd method on that instance can
//! be witnessed absent — but ONLY when the receiver's entire superclass chain is
//! known (the zero-false-positive keystone).
//!
//! ## What it holds
//!
//! For every [`Node::ClassDef`]/[`Node::ModuleDef`] in the AST it records the
//! class's **own** instance methods (a reopened class unions methods across its
//! definitions) and its written **superclass** name. Separately it acts as a
//! per-run **instance-class registry**: a name<->[`ClassId`] bijection in a high
//! id range that carries the identity of any class we type an instance of — both
//! source classes and RBS-known classes outside the tiny core nominal surface
//! (e.g. `Pathname`). The registry is needed because `Type::Nominal` only carries
//! a `ClassId`, and the core `CoreIndex` only round-trips ids for `CORE_CLASSES`.
//!
//! ## Class identity carried through the type system
//!
//! A typed instance flows as `Type::Nominal { class: ClassId }` where the
//! `ClassId` is allocated by THIS index in a high range (`>= SOURCE_CLASS_BASE`)
//! that never collides with the core-class ids (which live in `0..CORE_CLASSES`).
//! The index owns the name for that id, so a chained call's receiver resolves
//! back to its class name and the rules layer can decide method existence.
//!
//! ## The conservative gate (do NOT weaken)
//!
//! Method existence over a SOURCE class consults the union of: the class's own
//! methods, the methods of each source superclass up the chain, AND — when the
//! chain reaches an RBS-known class — that class's RBS ancestor chain. Absence is
//! witnessed (the undefined-method rule may fire) ONLY when the receiver's ENTIRE
//! chain is known: every source superclass resolves to a known source/RBS class,
//! terminating in a fully-loaded RBS root (Object/BasicObject). If ANY ancestor
//! is unknown (e.g. `class User < ApplicationRecord` where ApplicationRecord is
//! neither in source nor RBS — the Rails/ActiveRecord metaprogramming case), the
//! chain is INCOMPLETE ⇒ assume present ⇒ stay silent. This is what keeps real
//! Rails models false-positive-free. For an RBS-only instance class (e.g.
//! `Pathname`) existence defers entirely to RBS's own conservative gate.
//!
//! [`ClassId`]: rigor_types::ClassId
//! [`Node::ClassDef`]: rigor_parse::Node::ClassDef
//! [`Node::ModuleDef`]: rigor_parse::Node::ModuleDef

mod method_returns;
mod registry;
mod constants;
mod literal_fold;
mod def_attribution;
mod override_index;
mod harvest;

use std::collections::{HashMap, HashSet};

use rigor_parse::{FileKey, Visibility};
use rigor_types::{Scalar, ShapeKey};
pub(crate) use method_returns::*;
pub(crate) use constants::*;
pub(crate) use literal_fold::*;
pub(crate) use def_attribution::*;
pub use override_index::{lexical_scopes, method_body_spans};
pub(crate) use override_index::*;

/// C5 (const-literal harvest): an owned, interner-INDEPENDENT representation of a
/// fully-literal constant RHS, so a `CONST = <literal>` value can be recorded
/// project-wide once and re-interned against each analyzed file's own
/// [`Interner`] at the `ConstantRead` use site (interners are per-file). Mirrors
/// exactly the carriers the Typer builds for the same inline literal so the
/// resulting diagnostic renders identically — a scalar → `Constant`, an array →
/// `Tuple`, a static-keyed hash → `HashShape`, a range → `Nominal[Range]`.
///
/// [`Interner`]: rigor_types::Interner
#[derive(Clone, Debug, PartialEq)]
pub enum ConstLit {
    /// A value-pinned scalar (`42`, `"hi"`, `:sym`, `1.5`, `true`, `nil`).
    Scalar(Scalar),
    /// A per-position array shape (`[:a, :b]`) — every element fully literal.
    Tuple(Vec<ConstLit>),
    /// A per-key hash shape (`{ t: 10 }`) — every key a static scalar, every
    /// value fully literal, last-wins on a duplicate key (mirroring the Typer).
    Hash(Vec<(ShapeKey, ConstLit)>),
    /// A range literal (`1..1024`). Types to `Nominal[Range]` so method
    /// witnessing resolves against Range's RBS (SOUND — `IntegerRange` would
    /// erase to `Integer` and false-positive on real Range methods).
    Range,
    /// Slice B: a container LITERAL whose elements are not all literal — a
    /// lambda value, a `ConstantRead`, a splat, a dynamic key, an interpolated
    /// string, a call. Types to a BARE `Nominal[Array]` / `Nominal[Hash]` with
    /// `args: []`, **never with element types**.
    ///
    /// Two facts make this the FP-safe carrier, both probed
    /// (`docs/notes/20260808-partial-constant-harvest-probes.md`):
    ///
    /// * The reference never declines such a constant — it types the hole
    ///   (`->(_){…}` as `Proc`) and keeps a full `HashShape`/`Tuple`. So it
    ///   dispatches at the direct receiver, and the undefined-method lookup is
    ///   class-only: `Nominal[Hash]` and `HashShape{c: Proc}` both resolve
    ///   against `Hash`. Same witnessing set, different rendering.
    /// * A bare nominal is projection-INERT in rigor-rs:
    ///   `fold_tuple_projection` / `fold_hash_shape_projection` match
    ///   `Type::Tuple` / `Type::HashShape` only, and an argument-less generic
    ///   resolves nothing in the RBS tier. Every `[]`/`fetch`/`keys`/`values`/
    ///   block-param projection goes silent while the reference fires — the
    ///   divergence is strictly UNDER-emission.
    ///
    /// Element typing is deliberately out of scope: probe z2 shows the
    /// reference leaves even a harvested-constant element `Dynamic[top]`, so an
    /// element-typed harvest would out-precise the oracle at exactly the
    /// projection sites it declines — an FP generator.
    BareArray,
    /// The `Hash` twin of [`ConstLit::BareArray`].
    BareHash,
    /// Upstream #540 (`fc3b8b42`) — a literal shape the FILE ITSELF mutates.
    ///
    /// `LN_SUPPORTED = [true]` with a sibling method writing `LN_SUPPORTED[0] =
    /// false` is only as good as the file's own mutations: reads must not fold
    /// through a shape the program has already outgrown. The reference wraps
    /// such an accumulator entry in `Type::Combinator.dynamic(literal)`; the
    /// port carries the wrapper on the harvested VALUE and
    /// `Typer::intern_const_lit` turns it into `Type::Dynamic(inner)`, so a read
    /// stays honest (the class is still there for dispatch) without licensing
    /// any negative rule.
    ///
    /// Same-file scope only, and never nested: an entry is wrapped at most once,
    /// mirroring the reference's `next if existing.is_a?(Type::Dynamic)`.
    Widened(Box<ConstLit>),
}

/// The first [`ClassId`] handed out by the per-run registry. Chosen well above
/// the fixed core-class id space (`CORE_CLASSES`, currently 9 entries) so a
/// registered instance's nominal id can never be mistaken for a core class by
/// `CoreIndex::class_name_for_id`. A million-id gap is ample headroom.
///
/// [`ClassId`]: rigor_types::ClassId
pub const SOURCE_CLASS_BASE: u32 = 1_000_000;

/// ADR-35 slice 1: the visited-node cap on the override-visibility ancestor
/// walk ([`SourceIndex::nearest_ancestor_defining`]). Matches the reference's
/// `OVERRIDE_ANCESTOR_WALK_LIMIT`. Past it the walk declines (a missed witness,
/// never a false positive) rather than risk a runaway on a pathological graph.
pub const OVERRIDE_ANCESTOR_WALK_LIMIT: usize = 100;

/// The method KIND an interprocedural literal-tail fold is keyed on: an ordinary
/// instance `def` vs a singleton `def self.x` (`module_function` / `class << self`
/// out of scope). The two live in SEPARATE tables — a `Foo.read_only?` singleton
/// call never resolves an instance `read_only?` and vice versa (reference
/// `discovered_def_nodes` vs `discovered_singleton_def_nodes`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DefKind {
    Instance,
    Singleton,
}

/// One (re)definition site of a method whose interprocedural literal-tail return
/// we may fold: the CAPTURED tail expression ([`FoldTail`]) and whether the body
/// contains any explicit `return` (a decline gate — we read only the tail).
/// Collected per `(qualified owner, method, kind)` so reopens are joined (all
/// sites must agree on the folded literal, else decline).
///
/// **Issue #113 — this used to carry `ast_idx: usize`, a POSITION IN THE MERGED
/// SLICE.** That made a [`Harvest`] meaningful only for the exact
/// `&[(Harvest, &LoweredAst)]` the merge was handed — the last member of the
/// identity-hazard family #102/#103 retired. The site now borrows the mini-tree
/// the harvest already owns, so the fold needs no `&[&LoweredAst]` at all and a
/// harvest is self-contained for Pass 4b.
#[derive(Clone, Copy)]
pub(crate) struct FoldSite<'a> {
    tail: &'a FoldTail,
    has_explicit_return: bool,
}

/// The merged def table Pass 4b folds: `(qualified owner, method, kind) -> every
/// (re)definition site`, borrowing each site's mini-tree from its [`Harvest`].
type FoldDefs<'a> = HashMap<(String, String, DefKind), Vec<FoldSite<'a>>>;

/// Per-class structure harvested from source: own instance methods + superclass.
#[derive(Default, Clone)]
struct SourceClass {
    /// Instance method names defined directly in the class body, unioned across
    /// every (re)definition of the class.
    methods: HashSet<String>,
    /// The written superclass name (last path component), if any. `None` means
    /// no `< X` clause was written ⇒ the implicit super is `Object` (a fully
    /// loaded RBS root), so a no-super source class HAS a complete chain.
    superclass: Option<String>,
}

/// ADR-35 slice 1: per-class override data keyed by FULLY LEXICALLY-QUALIFIED
/// name (`IssuableFinder::Params`, not the collapsed `Params` the bare
/// [`SourceClass`] map uses). Lexical qualification is the zero-FP keystone for
/// `def.override-visibility-reduced`: distinct namespaced classes/modules that
/// share a last component (`Groups::Params`, `Integrations::Params`,
/// `IssuableFinder::Params`) must NOT merge into one ancestor — collapsing them
/// invented phantom overrides (the gitlab-foss FP cluster). The ancestor walk
/// resolves `include` / `superclass` names against the subclass's lexical
/// nesting and matches ONLY a precisely-qualified project class.
#[derive(Default, Clone)]
struct OverrideClass {
    /// Fully-qualified superclass NAME as WRITTEN (`< Foo::Bar` keeps `Foo::Bar`;
    /// `< Bar` keeps `Bar`), resolved against lexical nesting at walk time.
    superclass: Option<String>,
    /// `include` / `prepend` names as WRITTEN, in source order.
    includes: Vec<String>,
    /// `extend` names as WRITTEN, in source order — the reference folds each
    /// into the class's OWN singleton surface, so they join the singleton
    /// ancestor walk (never the instance one).
    extends: Vec<String>,
    /// The discovered instance-method VISIBILITY table. First-write-wins on
    /// reopen (mirrors the reference accumulator's stable cross-file view).
    method_visibilities: HashMap<String, Visibility>,
    /// Instance-method names defined directly (any visibility) — the existence
    /// set the walk stops on. Mirrors `SourceClass::methods` but lexically keyed.
    methods: HashSet<String>,
    /// Whether the FIRST declaration of this qualified name was a `module`.
    /// First-write-wins like `superclass` (Ruby raises on a class/module
    /// mismatch, so a reopen can never legally disagree). Read ONLY by
    /// [`SourceIndex::namespace_children`] — the completion-side kind icon —
    /// never by a rule, a predicate, or the typer.
    is_module: bool,
}

/// One harvested constant value: `(defining namespace segments, the
/// [`FileKey`] of the ASSIGNING file, the value)`. The file key is slice A's
/// per-file consumption gate; see [`SourceIndex::literal_constant`].
///
/// **The middle field is a PATH-derived key (issue #102), not a counter.** It
/// used to be `LoweredAst::file_id`, an in-memory process-global counter — which
/// made "the same file" mean "the same `lower()` call", so a RE-lowering of the
/// same bytes mis-gated: the LSP's hover lowered the buffer afresh against a
/// cached index built from the worker's lowering and the fold declined
/// (`FOO : 5` → `FOO : Dynamic[top]`), and a persisted harvest would have had to
/// re-stamp the field or silently answer at the wrong file (#92 §5). A
/// [`FileKey::Path`] is stable across lowerings, threads and processes, so
/// neither hazard survives. It is still stamped at MERGE time from the paired
/// `&LoweredAst` rather than carried inside a [`Harvest`] — the harvest stays a
/// pure function of `(AST, CoreIndex)` — but a future persisted harvest may now
/// carry the key verbatim.
type HarvestedConst = (Vec<String>, FileKey, ConstLit);

/// One source class/module (re)definition, harvested from a single file's AST in
/// `ast.iter()` order. Replayed through [`SourceIndex::add_source`] at merge, so
/// the slice order IS the first-`Some`-wins superclass order and the registration
/// (⇒ [`ClassId`]) order.
///
/// [`ClassId`]: rigor_types::ClassId
struct HarvestedClass {
    name: String,
    superclass: Option<String>,
    methods: Vec<String>,
}

/// One lexically-qualified override-class (re)definition, harvested from a single
/// file's AST in `collect_override_classes` walk order. Replayed through
/// [`SourceIndex::ingest_override_class`] at merge: the slice order IS the
/// first-write-wins visibility order and the `includes` append order, and BOTH
/// reach diagnostics (issue #92 §3.2) — the merge must never sort it.
pub(crate) struct HarvestedOverrideClass {
    qualified: String,
    superclass: Option<String>,
    methods: Vec<String>,
    method_visibilities: Vec<(String, Visibility)>,
    includes: Vec<String>,
    /// `extend`-ed module names as WRITTEN — see [`OverrideClass::extends`].
    extends: Vec<String>,
    /// Whether the declaration was a `module` (vs a `class`). Recorded ONLY so
    /// [`SourceIndex::namespace_children`] can render the same class-vs-module
    /// kind [`CoreIndex::namespace_children`] does for the RBS surface; no rule
    /// and no typing path reads it, so it cannot move a diagnostic.
    ///
    /// [`CoreIndex::namespace_children`]: rigor_index::CoreIndex::namespace_children
    is_module: bool,
}

/// One QUALIFIED constant name written by a single file, in walk order, with the
/// value of that file's FIRST write and how many times THAT FILE writes the name.
///
/// The count (not a bool) is what makes the per-file harvest reproduce C5's
/// project-wide single-assignment gate exactly: today's `lit_multi` is tripped by
/// a second write anywhere — including a second write inside the SAME file — so
/// the merge declines iff Σ counts ≥ 2.
pub(crate) struct HarvestedConstWrite {
    qualified: String,
    namespace: Vec<String>,
    /// The harvested value of this file's FIRST write (`None` ⇒ not fully
    /// literal). A repeat write never re-harvests, exactly as before.
    lit: Option<ConstLit>,
    writes: usize,
}

/// One project `def` site whose interprocedural literal-tail return may fold,
/// harvested from a single file in walk order.
///
/// **Issue #113: `tail` is an OWNED mini-tree, not a `NodeId` into this file's
/// AST.** It carries exactly what the pre-capture `fold_expr` could have reached
/// from this def's tail and nothing else, so the merge's Pass 4b needs no AST and
/// no slice position (see [`FoldTail`] and [`FoldSite`]).
pub(crate) struct HarvestedFoldDef {
    owner: String,
    method: String,
    kind: DefKind,
    tail: FoldTail,
    has_explicit_return: bool,
}

/// One file's slice of the def-attribution tables — the reference's per-file
/// `file_methods` overlay plus the `Object`-owner names a bare call resolves.
/// [`SourceIndex`] keeps these index-aligned with the merge's `files` order
/// and the rules pass the analyzed file's position in.
#[derive(Clone, Debug, Default)]
pub struct FileDefs {
    /// Instance-kind method names this file's own defs and macros bind on
    /// `Object` — the per-file half of `call.unresolved-toplevel`'s
    /// `Object`-reopen suppression. (`source_declared_method?` reads
    /// `Object` through the per-file overlay too, so `Object.class_eval
    /// { def m }` resolves `m` ONLY in the file that declares it.)
    pub toplevel: HashSet<String>,
    /// Qualified owner -> instance-kind method names this file declares
    /// (defs AND call-introduced) — the reference's per-file
    /// `file_methods` deep-merged over the def-stripped cross-file seed.
    pub methods: HashMap<String, HashSet<String>>,
}

/// **Issue #92** — ONE FILE's contribution to a project [`SourceIndex`], computed
/// from that file's AST and the FROZEN [`CoreIndex`] alone. A harvest never reads
/// another file's state, so [`SourceIndex::harvest`] is embarrassingly parallel
/// (the CLI runs it inside stage 1's rayon closure, beside parse+lower); the
/// cross-file joins all live in the serial, deterministic
/// [`SourceIndex::merge`].
///
/// ```text
/// build_project(asts, core) ≡ merge(asts.map(|a| (harvest(a, core), a)), core)
/// ```
///
/// bit-identical, which the `probes_s92` equivalence tests pin field by field.
///
/// The fields split by MERGE DISCIPLINE, and the split is load-bearing:
///
/// * **Pure unions** (`toplevel_defs`, `discovered_methods`, `mutated_params`,
///   `constant_write_bare_names`) — commutative + idempotent, mergeable in any
///   order.
/// * **Ordered replay** (`source_classes`, `override_classes`,
///   `rbs_constant_names`, `constant_writes`, `fold_defs`) — first-write-wins /
///   append semantics that REACH DIAGNOSTICS. The merge replays them in the
///   caller's file order (`expand_check_paths_excluding`: each argument's recursive
///   expansion sorted, arguments concatenated in argument order) and **must
///   never sort**: `rigor check a.rb b.rb` and `rigor check b.rb a.rb` are
///   legitimately different runs today (issue #92 §3.2/§3.5).
///
/// Nothing derived from OTHER files is in here — the literal-constant gates, the
/// declaration-only set, the tier-4b returns, the definers inversion and the
/// interprocedural fold are all computed by the merge.
///
/// [`CoreIndex`]: rigor_index::CoreIndex
#[derive(Default)]
pub struct Harvest {
    // --- pure unions (order-free) ------------------------------------------
    /// Pass 1c: this file's toplevel `def` names ⇒ `toplevel_defs`.
    toplevel_defs: HashSet<String>,
    /// Pass 1d, cross-file half: CALL-INTRODUCED instance-method names per
    /// qualified owner (`define_method`/`attr_*`/module-attr/`alias_method`)
    /// ⇒ `discovered_methods` after the merge's `subtract_def_methods`.
    macro_methods: HashMap<String, HashSet<String>>,
    /// Pass 1d, subtraction set: instance-kind names carrying a `def` node
    /// (incl. `alias`-of-def names) ⇒ the names the merge removes from the
    /// cross-file `discovered_methods` (`subtract_def_methods`: a cross-file
    /// `def` is the ADR-17 monkey-patch case the check surfaces).
    def_names: HashMap<String, HashSet<String>>,
    /// Pass 1d, per-file half: this file's own instance-kind existence
    /// overlay (defs AND macros) plus its `Object`-owner slice — replays
    /// index-aligned into [`SourceIndex::file_defs`].
    file_defs: FileDefs,
    /// Pass 1d: EVERY method name this file defines in any `def` form —
    /// instance, `def self.x`, and receiver-bearing `def obj.x` — flattened
    /// with no owner. ⇒ `defined_method_names`.
    defined_method_names: HashSet<String>,
    /// Pass 1e: method name -> mutated positional param indices ⇒ per-key union.
    mutated_params: HashMap<String, HashSet<usize>>,
    /// Stage 2b: the BARE name of every constant this file writes ⇒
    /// `project_constant_write_names` (today's set is the bare names of every
    /// qualified write key, i.e. exactly this union).
    constant_write_bare_names: HashSet<String>,

    // --- ordered replay ----------------------------------------------------
    /// Pass 1, in `ast.iter()` order.
    source_classes: Vec<HarvestedClass>,
    /// Pass 1b, in lexical walk order.
    override_classes: Vec<HarvestedOverrideClass>,
    /// Pass 2: every `ConstantRead` name the FROZEN core knows, in first-
    /// occurrence order, deduplicated (`register` is idempotent — see
    /// [`SourceIndex::register`] — so dropping repeats cannot move an id).
    ///
    /// Pre-filtering against `core` here is legal because the `CoreIndex` is
    /// frozen before any harvest runs (ADR-0028), and today's extra
    /// `!classes.contains_key(name)` term is a no-op: Pass 1 has already
    /// registered every source class by the time Pass 2 runs (issue #92 §2.1).
    rbs_constant_names: Vec<String>,
    /// C5a, in walk order — one entry per distinct qualified name.
    constant_writes: Vec<HarvestedConstWrite>,
    /// Pass 4a, in walk order.
    fold_defs: Vec<HarvestedFoldDef>,
}

/// The per-run source-class index + instance-class registry. Built once per file.
#[derive(Default)]
pub struct SourceIndex {
    /// `class name -> source structure` (only for in-source class/module defs).
    classes: HashMap<String, SourceClass>,
    /// Dense list of registered class names in id order; the slice index +
    /// [`SOURCE_CLASS_BASE`] IS the class's [`ClassId`] (reversible). Holds both
    /// source classes and registered RBS-only instance classes.
    ///
    /// [`ClassId`]: rigor_types::ClassId
    names: Vec<String>,
    /// Fast name -> registry position lookup.
    name_to_id: HashMap<String, u32>,
    /// MultiWrite substrate Slice 2: names registered ONLY because an RBS TUPLE
    /// return names them as an element (Pass 2b) — i.e. classes the analyzed
    /// SOURCE never mentions and no source file declares, reachable only THROUGH
    /// a declaration (`Process::Status` via `Process.wait2`). Read by the rules'
    /// qualified-witness gate; see [`Self::is_declaration_only_class`] for why
    /// the distinction is load-bearing.
    declaration_only_classes: HashSet<String>,
    /// ADR-0023 tier-4b: `(class NAME, method NAME) -> inferred CORE class NAME`
    /// (e.g. `("User", "full_name") -> "String"`). Populated in a Pass 3 of
    /// [`build_project`] for direct instance methods whose RETURN (tail)
    /// expression types — under an EMPTY env — to a concrete core/RBS class.
    /// Keyed by NAME (cross-file safe); the value is a core class NAME re-interned
    /// at the call site via [`CoreIndex::class_id`]. A method that fails ANY gate
    /// has NO entry ⇒ the call types Dynamic (silent).
    ///
    /// [`CoreIndex::class_id`]: rigor_index::CoreIndex::class_id
    method_returns: HashMap<(String, String), String>,
    /// ADR-0023 tier-4b call-site PARAMETER BINDING: `(class NAME, method NAME)
    /// -> ParamBoundReturn`. This is the param-DEPENDENT companion to
    /// `method_returns` (which is param-INDEPENDENT). A method qualifies when its
    /// tail is a bare positional-param read, or a no-arg core-method CHAIN whose
    /// root receiver is a bare positional-param read (`def up(x); x.upcase; end`).
    /// The descriptor defers the param's type to the call site: it records WHICH
    /// positional param the chain roots at, and the chain of no-arg core methods
    /// to apply. The call site binds the ARGUMENT's type and re-derives the core
    /// return (see [`SourceIndex::param_bound_return`] + the tier-4b call hook).
    /// Kept SEPARATE from `method_returns`: the param-independent map always wins
    /// when present (it needs no args), and a method may have at most one of the
    /// two (a tail is either param-rooted or not). Same cross-file NAME keying and
    /// the same reopen-disagreement decline apply.
    param_bound_returns: HashMap<(String, String), ParamBoundReturn>,
    /// ADR-35 slice 1: the lexically-qualified override index for
    /// `def.override-visibility-reduced` (see [`OverrideClass`]). Keyed by FULL
    /// qualified name to avoid the last-component name-collision merge.
    override_classes: HashMap<String, OverrideClass>,
    /// PROJECT-WIDE toplevel method names, for `call.unresolved-toplevel` (ref
    /// ADR-34). A name is here iff SOME analyzed file declares it OUTSIDE any
    /// class/module — a toplevel `def foo` (Object private method), or an
    /// in-source reopen of `Object`/`Kernel`/`BasicObject`. The reference resolves
    /// a toplevel call against toplevel defs PROJECT-WIDE in a directory run (a
    /// `def` in file A satisfies a call in file B that `require`s it), so the rule
    /// suppresses on this cross-file set — matching the reference's project-mode
    /// resolution and staying zero-FP on the multi-file corpus.
    toplevel_defs: HashSet<String>,
    /// ADR-0038 interprocedural literal-tail fold: `(qualified owner, method,
    /// kind) -> folded scalar literal`. Populated in Pass 4 of [`build_project`]
    /// for a project method whose whole return provably joins to ONE scalar
    /// `Constant` (`Gitlab::Database.read_only? -> false`, `read_write? =
    /// !read_only? -> true`). The value already has the overridable-method
    /// degrade applied (a `Constant` here is never re-opened by a related
    /// subclass/includer override), so a hit types a `Type::Constant` directly.
    /// A method that fails any fold gate has NO entry ⇒ the call stays Dynamic
    /// (silent). Keyed by NAME (cross-file safe). SEPARATE from `method_returns`
    /// (which widens to Nominal and drops the value pin).
    literal_returns: HashMap<(String, String, DefKind), Scalar>,
    /// ADR-0038 interprocedural literal-tail fold: the inverted `(method, kind)
    /// -> [qualified owners that define it]` index over the project's own `def`
    /// bodies. Drives the overridable-method degrade gate (a value-pinned base
    /// return is unsound to adopt when a RELATED subclass/includer redefines the
    /// method) and the implicit-self ancestor resolution. Mirrors the reference's
    /// `method_definers_index`.
    definers: HashMap<(String, DefKind), Vec<String>>,
    /// C1 (constant-shadow gate): constant names the project defines AT TOPLEVEL
    /// (their fully-qualified name has no `::`). A bare read of such a name is
    /// shadowed by the project definition EVERYWHERE (Ruby: a toplevel constant is
    /// always reachable), so the singleton gate stays suppressed — preserving the
    /// pre-C1 blanket behavior for Rails models (`Group`/`Report`).
    toplevel_constants: HashSet<String>,
    /// C5 (const-literal harvest): `bare CONST NAME -> [(defining namespace,
    /// fully-literal value)]`, for a constant assigned EXACTLY ONCE at its
    /// QUALIFIED name, whose RHS is fully literal, and whose name does NOT also
    /// name a class/module. Consulted by the `ConstantRead` arm BEFORE the
    /// singleton gate — but LEXICALLY, exactly like the C1 shadow gate: the value
    /// applies only at a use site the defining namespace is visible from (Ruby's
    /// lexical constant lookup). This is load-bearing: a concern's
    /// `DAYS_TO_EXPIRE = 7` in `module Expirable` must NOT fold in an including
    /// `class Key` where it is not lexically visible (the reference resolves it
    /// lexically too, so folding it there manufactures an `Integer#days` FP).
    /// PER-FILE consumption (slice A, 2026-08-08): each entry also carries the
    /// [`rigor_parse::FileKey`] of the file that ASSIGNED the constant, and
    /// [`Self::literal_constant`] only answers a use site in that same file.
    /// The reference's in-source constant-VALUE table is rebuilt per
    /// file (`ScopeIndexer#build_in_source_constants` walks one file's root and
    /// nothing cross-file feeds it), so a cross-file fold is an emission the
    /// oracle never makes — probed: a fully-literal `TOPL = [1, 2].freeze` in
    /// `a.rb` read from `b.rb` is reference-silent even with a
    /// `require_relative`, while rigor-rs fired. The HARVEST stays project-wide
    /// (the single-assignment gate must still see every file).
    literal_constants: HashMap<String, Vec<HarvestedConst>>,
    /// Collection-shape stage 2e: the SAME harvested values as
    /// `literal_constants`, keyed instead by the constant's FULLY-QUALIFIED name
    /// (`"Gitlab::Ci::Reports::CodequalityReports::SEVERITY_PRIORITIES"`).
    /// Entries pass exactly the same gates (single project-wide assignment,
    /// fully-literal RHS, no class/module name collision), so the two maps
    /// always carry the same constants — this one just answers a
    /// `::A::B::C::CONST` path read, which arrives as one `ConstantRead` whose
    /// `name` is the whole path and therefore misses the bare-name map.
    /// Backs [`Self::qualified_literal_constant`].
    qualified_literal_constants: HashMap<String, HarvestedConst>,
    /// Collection-shape stage 2b: the BARE name of every `CONST = …` write the
    /// project makes in a class/module/program body — INCLUDING the ones C5
    /// declines to harvest (non-literal RHS, multiply assigned). The C1 shadow
    /// tables are built from class/module DEFINITIONS only, so they do not see a
    /// plain constant assignment; the RBS-object-constant arm needs to, because
    /// `ENV = Object.new` in the project makes the core `ENV: ENVClass`
    /// declaration the wrong surface entirely (probed: the reference resolves
    /// the project value and reports a different diagnostic; typing it as
    /// `ENVClass` produced an oracle FP).
    ///
    /// Scope-INDEPENDENT on purpose: the RBS object constants are 18 well-known
    /// globals, and a project that names one anywhere is reason enough to
    /// decline. Backs [`Self::project_writes_constant`].
    project_constant_write_names: HashSet<String>,
    /// C1 (constant-shadow gate): for a constant the project defines NESTED, the
    /// containing-namespace segment vectors keyed by the constant's last segment
    /// (`module Gitlab; module Database; module Partitioning; module Time` keys
    /// `"Time" -> [["Gitlab","Database","Partitioning"]]`). A bare read of `Time`
    /// is shadowed ONLY at a use site whose lexical prefix has one of these
    /// namespaces as an initial segment run — Ruby's `Module.nesting` lexical
    /// lookup, matching the reference's `lexical_constant_candidates`. Elsewhere
    /// the read RELAXES so the core-RBS singleton is witnessed (the C1 fix).
    nested_constant_namespaces: HashMap<String, Vec<Vec<String>>>,
    /// PROJECT-WIDE `qualified class/module name -> instance-method names the
    /// project itself declares on it`, harvested from EVERY receiver-less `def`
    /// lexically inside the body — including one nested in a block or a
    /// conditional (`rake_extension("ext") { def ext; end }`, `if
    /// defined?(X); def call; end; else; def call; end; end`).
    ///
    /// This is the port of the reference's `Scope#discovered_method?` gate, which
    /// `undefined_method_diagnostic` consults BEFORE it reaches the RBS surface:
    /// a project reopening of a CORE class contributes methods RBS cannot know
    /// about, and witnessing their absence against RBS alone is a false positive
    /// (rigor-survey `rake-13.4.2/lib/rake/ext/string.rb` — `class String` gains
    /// `#ext` / `#pathmap_explode` and both were reported undefined).
    ///
    /// Deliberately SEPARATE from `classes[..].methods` (direct children only,
    /// which mirrors the reference's `direct_method_names` and feeds the source
    /// chain walk and tier-4b harvest): this map is a pure SILENCER — it is only
    /// ever read to suppress, never to witness absence — so widening it to nested
    /// defs cannot manufacture a diagnostic.
    ///
    /// Two layers, mirroring the reference (`finalize_def_index` +
    /// `seed_discovered_methods`): this map holds the CROSS-FILE seed —
    /// call-introduced names minus every name a project `def` declares
    /// (`subtract_def_methods`: a cross-file `def` is the ADR-17
    /// monkey-patch case the check surfaces) — while [`Self::file_defs`]
    /// carries each file's own overlay (defs and macros), which
    /// [`Self::project_declares_method`] consults for the analyzed file.
    /// Singleton-kind names (`def self.x`, `class <<`, `instance_eval` defs)
    /// are filed NEITHER place (the reference stamps them `:singleton`; the
    /// port's consumers only ever ask `:instance`).
    discovered_methods: HashMap<String, HashSet<String>>,
    /// The per-file def-attribution overlay, index-aligned with the `files`
    /// order [`Self::merge`] receives (and `asts` order for
    /// [`Self::build_project`]). The rules pass the analyzed file's position
    /// as `file` into [`Self::is_toplevel_def`] /
    /// [`Self::project_declares_method`].
    file_defs: Vec<FileDefs>,
    /// Analyzed file [`rigor_parse::FileKey`] → its index in
    /// [`Self::file_defs`], built alongside the vector in [`Self::merge`].
    file_index: HashMap<FileKey, usize>,
    /// PROJECT-WIDE `method name -> the POSITIONAL PARAMETER INDICES its body
    /// mutates in place`. A parameter is "mutated" when the body calls a
    /// [`crate::MUTATOR_METHODS`] method on a bare read of it (`def fill(a); a <<
    /// 1; end` records `fill -> {0}`).
    ///
    /// The caller-side half of `MutationWidening`: the reference widens a
    /// value-pinned local passed as an ARGUMENT to a method that mutates the
    /// matching parameter, and only then. Probed against the oracle: `def
    /// m(x, a); a << 1; end` widens `m(5, xs)` but NOT `m(xs, 5)`; a mutator on
    /// a DIFFERENT local inside the callee widens nothing; an unresolved callee
    /// widens nothing. Without it, `xs = []; fill xs; if xs.length == 1` folded
    /// to a constant and fired `flow.always-truthy-condition` (rigor-survey
    /// `rspec-core-3.13.6/lib/rspec/core/world.rb:179`, where
    /// `announce_inclusion_filter` shovels into the array it is handed).
    ///
    /// Keyed by NAME alone, cross-file, with no owner resolution: widening only
    /// FORGETS a fact, so over-widening costs coverage and can never add a
    /// diagnostic. Only defs whose parameter list is plain-positional
    /// (`MethodBody::params`) contribute — a splat/kwarg signature has no stable
    /// index-to-name map, so it records nothing.
    mutated_params: HashMap<String, HashSet<usize>>,
    /// rigor-rs#140: every method name the project defines ANYWHERE, in any
    /// `def` form (instance, `def self.x`, `def obj.x`), flattened with no
    /// owner — the port of the reference's `project_defines_anywhere?`
    /// (block_call_timing.rb), which reads the union of the discovered-method
    /// tables on BOTH sides. Deliberately coarse, exactly like the reference:
    /// a `class C; def self.raise` shadows nothing at the `raise` call site,
    /// yet still disables the non-returning-call proof — resolving the site's
    /// `self` ancestry buys nothing for names this rare. Read only to DECLINE
    /// (keep the pre-proof answer), never to witness.
    defined_method_names: HashSet<String>,
}

/// ADR-0023 tier-4b call-site param-binding descriptor (see
/// [`SourceIndex::param_bound_returns`]). The method's tail is the
/// `chain.len() == 0` bare read of positional param `param_index`, or that param
/// read followed by the no-arg core-method `chain` (`x.upcase.strip` ->
/// `param_index = <x>, chain = ["upcase", "strip"]`). The call site types the
/// ARGUMENT at `param_index`, then walks the chain through the core return table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamBoundReturn {
    /// The positional index of the param the tail's root receiver reads.
    pub param_index: usize,
    /// No-arg core methods applied to the param, in source order (possibly empty
    /// for a bare passthrough `def full(x); x; end`).
    pub chain: Vec<String>,
}

#[cfg(test)]
mod tests;

// ===========================================================================
// EQUIVALENCE HARNESS — issue #92 (SourceIndex harvest/merge decomposition).
//
// Promoted from the throwaway probe module the pass inventory
// (`docs/notes/20260825-s92-buildproject-pass-inventory.md`) was written from.
// It renders a per-FIELD fingerprint of a built `SourceIndex` — all 17 fields —
// so two build paths, or two file orders, can be compared field by field.
//
// It carries three things now:
//
//   * `build_project_legacy` — the PRE-#92 body, verbatim, with its own copies
//     of the three walkers. It is the oracle: `merge(harvests)` must fingerprint
//     identically to it on every corpus below. Nothing but the tests calls it.
//   * the five MINIMAL coupling/order examples from the probe, pinned as
//     assertions (they are the cases a wrong merge order or a missing barrier
//     would break, and the real-corpus sweep cannot contain them).
//   * the original probes, kept because each one documents a live channel.
//
// `docs/notes/20260825-s92-harvest-merge-impl.md` is the impl write-up.
// ===========================================================================
#[cfg(test)]
mod probes_s92;

// ===========================================================================
// EQUIVALENCE HARNESS — issue #94 (per-candidate ancestor closure).
//
// Pass 4b's overridable degrade gate used to run one ancestor BFS per
// `(candidate, owner)` PAIR (`related_to_owner`); it now runs one per CANDIDATE
// and answers each pair from the resulting closure. That is a pure performance
// change, so the whole of its correctness is one claim:
//
//     related_to_owner(c, o)  ==  ancestor_closure(c).contains(o)      ∀ c, o
//
// The pre-#94 walk is kept verbatim under `#[cfg(test)]` (the #92
// `build_project_legacy` pattern) and IS the oracle here. The four tests grade
// the claim on (1) the probe corpora's override-graph shapes, (2) randomized
// synthetic hierarchies (reopens, includes, cycles, lexical nesting), and
// (3, 4) both halves of the `OVERRIDE_ANCESTOR_WALK_LIMIT` boundary — the node
// that overflows the cap, and the queue abandoned behind it. No real corpus
// reaches the cap at all (0 hits in 12 runs,
// `docs/notes/20260825-s94-pass4b-cost-probe.md` §2), so those two synthetic
// tests are its ONLY coverage — and the fan-out one is what catches a change in
// BFS order, which below the cap is invisible.
//
// `docs/notes/20260825-s94-ancestor-closure-impl.md` is the impl write-up.
// ===========================================================================
#[cfg(test)]
mod probes_s94;
