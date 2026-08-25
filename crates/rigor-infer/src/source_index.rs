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

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet, VecDeque};

use rigor_index::CoreIndex;
use rigor_parse::{LoweredAst, MethodBody, Node, NodeId, Visibility};
use rigor_types::{ClassId, Interner, Scalar, ShapeKey};

/// C5 (const-literal harvest): an owned, interner-INDEPENDENT representation of a
/// fully-literal constant RHS, so a `CONST = <literal>` value can be recorded
/// project-wide once and re-interned against each analyzed file's own
/// [`Interner`] at the `ConstantRead` use site (interners are per-file). Mirrors
/// exactly the carriers the Typer builds for the same inline literal so the
/// resulting diagnostic renders identically — a scalar → `Constant`, an array →
/// `Tuple`, a static-keyed hash → `HashShape`, a range → `Nominal[Range]`.
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
}

/// The first [`ClassId`] handed out by the per-run registry. Chosen well above
/// the fixed core-class id space (`CORE_CLASSES`, currently 9 entries) so a
/// registered instance's nominal id can never be mistaken for a core class by
/// `CoreIndex::class_name_for_id`. A million-id gap is ample headroom.
pub const SOURCE_CLASS_BASE: u32 = 1_000_000;

/// ADR-35 slice 1: the visited-node cap on the override-visibility ancestor
/// walk ([`SourceIndex::nearest_ancestor_defining`]). Matches the reference's
/// `OVERRIDE_ANCESTOR_WALK_LIMIT`. Past it the walk declines (a missed witness,
/// never a false positive) rather than risk a runaway on a pathological graph.
pub const OVERRIDE_ANCESTOR_WALK_LIMIT: usize = 100;

/// Interprocedural literal-tail fold: the recursion depth cap on
/// [`SourceIndex::fold_expr`] (bodies calling bodies — `read_write? = !read_only?`).
/// Past it the fold declines (a missed witness, never a false positive). Bodies
/// this deep are vanishingly rare; the cap just backstops a pathological chain the
/// per-key cycle guard would otherwise still terminate but slowly.
const FOLD_DEPTH_CAP: usize = 16;

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
/// we may fold: which analyzed AST holds it, the tail (return) expression node,
/// and whether the body contains any explicit `return` (a decline gate — we read
/// only the tail). Collected per `(qualified owner, method, kind)` so reopens are
/// joined (all sites must agree on the folded literal, else decline).
///
/// `ast_idx` is a POSITION IN THE MERGED SLICE, not a file identity: it is only
/// meaningful for the exact `&[(Harvest, &LoweredAst)]` the merge was handed.
/// That is why [`Harvest`] stores [`HarvestedFoldDef`] instead — file-relative,
/// with the slice position stamped on at merge time (issue #92 §5).
#[derive(Clone, Copy)]
struct FoldSite {
    ast_idx: usize,
    tail: NodeId,
    has_explicit_return: bool,
}

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
    /// The discovered instance-method VISIBILITY table. First-write-wins on
    /// reopen (mirrors the reference accumulator's stable cross-file view).
    method_visibilities: HashMap<String, Visibility>,
    /// Instance-method names defined directly (any visibility) — the existence
    /// set the walk stops on. Mirrors `SourceClass::methods` but lexically keyed.
    methods: HashSet<String>,
}

/// The per-run source-class index + instance-class registry. Built once per file.
/// One harvested constant value: `(defining namespace segments, the
/// `LoweredAst::file_id` of the ASSIGNING file, the value)`. The file id is
/// slice A's per-file consumption gate; see [`SourceIndex::literal_constant`].
///
/// **Persistence hazard (issue #92 §5).** The middle field is a
/// [`rigor_parse::LoweredAst::file_id`] — an in-memory PROCESS-GLOBAL counter,
/// not a content or path key. It is stamped at MERGE time from the paired
/// `&LoweredAst` (never carried inside a [`Harvest`]) precisely because a
/// harvest that outlived its process — a blake3-keyed on-disk cache, an LSP
/// harvest held across a rebuild — would otherwise replay a stale id and make
/// [`SourceIndex::literal_constant`]'s same-file gate answer at the wrong file.
/// Any future persisted harvest MUST re-stamp this field, or the field must
/// become a path/content key first.
type HarvestedConst = (Vec<String>, u64, ConstLit);

/// Issue #94: the per-MERGE memo of transitive project-ancestor closures, keyed
/// by definer-candidate name. One entry answers every later
/// `(candidate, *owner*)` relatedness query of that merge as a set membership
/// test, replacing one BFS per PAIR with one BFS per CANDIDATE (measured reuse:
/// 12.5× at gitlab-foss/lib, 21.5× at mastodon/app —
/// `docs/notes/20260825-s94-pass4b-cost-probe.md` §2).
///
/// It is deliberately a plain local threaded by `&mut` through the Pass-4b call
/// chain, NOT a [`SourceIndex`] field and NOT interior mutability: its soundness
/// rests on `override_classes` being frozen for the whole of
/// [`SourceIndex::compute_literal_returns`], and on nothing outliving that call
/// (the LSP rebuilds the index per dispatch, so a longer-lived cache keyed by
/// class name alone would serve stale answers across keystrokes).
type AncestorClosures = HashMap<String, HashSet<String>>;

/// One source class/module (re)definition, harvested from a single file's AST in
/// `ast.iter()` order. Replayed through [`SourceIndex::add_source`] at merge, so
/// the slice order IS the first-`Some`-wins superclass order and the registration
/// (⇒ [`ClassId`]) order.
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
struct HarvestedOverrideClass {
    qualified: String,
    superclass: Option<String>,
    methods: Vec<String>,
    method_visibilities: Vec<(String, Visibility)>,
    includes: Vec<String>,
}

/// One QUALIFIED constant name written by a single file, in walk order, with the
/// value of that file's FIRST write and how many times THAT FILE writes the name.
///
/// The count (not a bool) is what makes the per-file harvest reproduce C5's
/// project-wide single-assignment gate exactly: today's `lit_multi` is tripped by
/// a second write anywhere — including a second write inside the SAME file — so
/// the merge declines iff Σ counts ≥ 2.
struct HarvestedConstWrite {
    qualified: String,
    namespace: Vec<String>,
    /// The harvested value of this file's FIRST write (`None` ⇒ not fully
    /// literal). A repeat write never re-harvests, exactly as before.
    lit: Option<ConstLit>,
    writes: usize,
}

/// One project `def` site whose interprocedural literal-tail return may fold,
/// harvested from a single file in walk order. Deliberately FILE-RELATIVE: the
/// `tail` [`NodeId`] indexes THIS file's AST, and the merge stamps the slice
/// position on to build a [`FoldSite`]. See [`FoldSite`] for why.
struct HarvestedFoldDef {
    owner: String,
    method: String,
    kind: DefKind,
    tail: NodeId,
    has_explicit_return: bool,
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
///   caller's file order (`expand_check_paths`: each argument's recursive
///   expansion sorted, arguments concatenated in argument order) and **must
///   never sort**: `rigor check a.rb b.rb` and `rigor check b.rb a.rb` are
///   legitimately different runs today (issue #92 §3.2/§3.5).
///
/// Nothing derived from OTHER files is in here — the literal-constant gates, the
/// declaration-only set, the tier-4b returns, the definers inversion and the
/// interprocedural fold are all computed by the merge.
#[derive(Default)]
pub struct Harvest {
    // --- pure unions (order-free) ------------------------------------------
    /// Pass 1c: this file's toplevel `def` names ⇒ `toplevel_defs`.
    toplevel_defs: HashSet<String>,
    /// Pass 1d: qualified owner -> this file's own `def` names ⇒ per-key union.
    discovered_methods: HashMap<String, HashSet<String>>,
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

#[derive(Default)]
pub struct SourceIndex {
    /// `class name -> source structure` (only for in-source class/module defs).
    classes: HashMap<String, SourceClass>,
    /// Dense list of registered class names in id order; the slice index +
    /// [`SOURCE_CLASS_BASE`] IS the class's [`ClassId`] (reversible). Holds both
    /// source classes and registered RBS-only instance classes.
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
    /// [`rigor_parse::LoweredAst::file_id`] of the file that ASSIGNED the
    /// constant, and [`Self::literal_constant`] only answers a use site in that
    /// same file. The reference's in-source constant-VALUE table is rebuilt per
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
    discovered_methods: HashMap<String, HashSet<String>>,
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

impl SourceIndex {
    /// Build from a lowered AST against the core (RBS) index. Collects every
    /// `ClassDef`/`ModuleDef` (source structure) and registers an instance-class
    /// id for every class we may type an instance of: each source class, and
    /// each `X.new` receiver constant whose `X` is RBS-known (so a `Pathname.new`
    /// instance carries identity even though `Pathname` is outside `CORE_CLASSES`).
    pub fn build(ast: &LoweredAst, core: &CoreIndex) -> Self {
        Self::build_project(&[ast], core)
    }

    /// Build a PROJECT-WIDE index from EVERY analyzed file's lowered AST. Class /
    /// module names are harvested from all `asts`, so [`knows_class`] answers
    /// project-wide — this is what lets the rules layer refuse to singleton-type a
    /// bare constant that the project itself defines elsewhere (e.g. a Rails model
    /// `Group`/`Report`), keeping cross-file constant typing false-positive-free.
    ///
    /// Constant registration is also project-wide and generalized: EVERY
    /// `Node::ConstantRead { name }` whose `name` is RBS-known (and not already a
    /// source class) gets a registry id, so `Time`/`Array`/... round-trip via
    /// [`class_id`]/[`class_name_for_id`] for singleton rendering. The original
    /// `X.new` registration is subsumed by this (its receiver is a `ConstantRead`).
    ///
    /// [`knows_class`]: SourceIndex::knows_class
    /// [`class_id`]: SourceIndex::class_id
    /// [`class_name_for_id`]: SourceIndex::class_name_for_id
    pub fn build_project(asts: &[&LoweredAst], core: &CoreIndex) -> Self {
        let files: Vec<(Harvest, &LoweredAst)> =
            asts.iter().map(|ast| (Self::harvest(ast, core), *ast)).collect();
        Self::merge(&files, core)
    }

    /// **Issue #92 — the PARALLEL half.** Everything one file contributes to a
    /// project index that is derivable from `(that file's AST, the FROZEN
    /// [`CoreIndex`])` alone: passes 1, 1b, 1c, 1d, 1e, C5a, 2 and the 4a walk.
    ///
    /// Reads no other file's state and no accumulated [`SourceIndex`] state, so
    /// it is safe to run inside the CLI's stage-1 rayon closure beside
    /// parse+lower. Nothing here decides anything: the ordered fields are
    /// REPLAYED by [`Self::merge`] in the caller's file order, and every
    /// cross-file gate (the constant single-assignment gate, the declaration-only
    /// set, tier-4b returns, the definers inversion, the interprocedural fold)
    /// runs there.
    pub fn harvest(ast: &LoweredAst, core: &CoreIndex) -> Harvest {
        let mut h = Harvest::default();

        // Pass 1: source class/module structure, in `ast.iter()` order (which IS
        // the registration ⇒ ClassId order once the merge replays it).
        for (_, node) in ast.iter() {
            match node {
                Node::ClassDef { name, superclass, methods, .. } => {
                    if name.is_empty() {
                        continue; // un-namable (dynamic constant) ⇒ skip.
                    }
                    h.source_classes.push(HarvestedClass {
                        name: name.clone(),
                        superclass: superclass.clone(),
                        methods: methods.to_vec(),
                    });
                }
                Node::ModuleDef { name, methods, .. } => {
                    if name.is_empty() {
                        continue;
                    }
                    // A module has no super.
                    h.source_classes.push(HarvestedClass {
                        name: name.clone(),
                        superclass: None,
                        methods: methods.to_vec(),
                    });
                }
                _ => {}
            }
        }

        // Pass 1b (ADR-35 slice 1): the LEXICALLY-QUALIFIED override index, by a
        // recursive walk with a nesting stack, so a nested `module Params` is
        // keyed `Outer::Params` (not the collapsed `Params`). This is what keeps
        // the override-visibility rule free of the name-collision false
        // positives. Kept entirely separate from the collapsed `classes` map —
        // no other rule is affected.
        collect_override_classes(ast, ast.root(), &[], &mut h.override_classes);

        // Pass 1c (ADR-34): PROJECT-WIDE toplevel method names for
        // `call.unresolved-toplevel`. A `def` OUTSIDE any class/module body is a
        // toplevel def (an Object private method); an in-source reopen of
        // Object/Kernel/BasicObject injects toplevel-callable methods too. Toplevel
        // detection is span-containment against the file's class/module spans
        // (orphan-proof) — a WITHIN-file comparison, which is why the pass is
        // per-file. The merge unions across files so a `def` in one file resolves
        // a call in another (the reference's project-mode resolution).
        let scope_spans: Vec<rigor_parse::Span> = ast
            .iter()
            .filter_map(|(_, n)| match n {
                Node::ClassDef { span, .. } | Node::ModuleDef { span, .. } => Some(*span),
                _ => None,
            })
            .collect();
        for (_, node) in ast.iter() {
            match node {
                Node::Definition { name: Some(nm), span, .. }
                    if !scope_spans.iter().any(|s| s.0 <= span.0 && span.1 <= s.1) =>
                {
                    h.toplevel_defs.insert(nm.clone());
                }
                // A TOPLEVEL receiver-bearing `def Foo.bar` registers `bar`
                // as a toplevel name too. That is not Ruby's runtime
                // semantics — `bar` is on `Foo`'s singleton — but it is the
                // reference's: `ScopeIndexer#record_def_node` keys a def
                // under `<toplevel>` whenever its lexical prefix is empty,
                // and `def_singleton?` excludes only a `self` receiver and a
                // receiver naming the lexically enclosing class (impossible
                // with an empty prefix). Parity is the contract, so rigor-rs
                // matches it: rigor-survey `io-console-0.8.2`'s `size.rb`
                // calls `default_console_size` from inside `def
                // IO.console_size`, where the reference is silent.
                // `def self.x` at toplevel is NOT registered (the reference
                // excludes it, and so does this arm — a self receiver leaves
                // `receiver_def_name` `None`, see the lowering).
                Node::Definition { receiver_def_name: Some(nm), span, .. }
                    if !scope_spans.iter().any(|s| s.0 <= span.0 && span.1 <= s.1) =>
                {
                    h.toplevel_defs.insert(nm.clone());
                }
                Node::ClassDef { name, methods, .. } | Node::ModuleDef { name, methods, .. }
                    if matches!(name.as_str(), "Object" | "Kernel" | "BasicObject") =>
                {
                    h.toplevel_defs.extend(methods.iter().cloned());
                }
                _ => {}
            }
        }

        // Pass 1d: the project's own instance-method declarations per QUALIFIED
        // class/module (`discovered_methods`), the port of the reference's
        // `Scope#discovered_method?` suppression gate. A def counts for the
        // INNERMOST lexical class/module whose span contains it, so a def in a
        // block or a conditional inside the body still lands on the right class
        // and a def in a nested class does not leak to the outer one.
        let scopes = lexical_scopes(ast);
        for (_, node) in ast.iter() {
            let Node::Definition { name: Some(nm), span, .. } = node else {
                continue;
            };
            let innermost = scopes
                .iter()
                .filter(|(s, _)| s.0 <= span.0 && span.1 <= s.1)
                .min_by_key(|(s, _)| s.1 - s.0);
            if let Some((_, segs)) = innermost {
                h.discovered_methods.entry(segs.join("::")).or_default().insert(nm.clone());
            }
        }

        // Pass 1e: the caller-side half of `MutationWidening` — which positional
        // parameter of each project method the method mutates in place. See
        // `mutated_params`.
        for (_, node) in ast.iter() {
            let Node::Definition { params: Some(names), span, .. } = node else {
                continue;
            };
            if names.is_empty() {
                continue;
            }
            for (_, inner) in ast.iter() {
                let Node::Call { receiver: Some(r), method, span: cspan, .. } = inner else {
                    continue;
                };
                if !(span.0 <= cspan.0 && cspan.1 <= span.1) {
                    continue; // not inside this def.
                }
                if !crate::MUTATOR_METHODS.contains(&method.as_str()) {
                    continue;
                }
                let Node::LocalVariableRead { name: recv_name, .. } = ast.get(*r) else {
                    continue;
                };
                if let Some(i) = names.iter().position(|p| p == recv_name) {
                    // The def's NAME: a receiver-bearing def carries it in
                    // `receiver_def_name` / `singleton_name` instead.
                    for key in def_names(node) {
                        h.mutated_params.entry(key).or_default().insert(i);
                    }
                }
            }
        }

        // C5a: this file's lexically-qualified `CONST = <literal>` writes, in walk
        // order, with a per-file write COUNT per qualified name. The project-wide
        // single-assignment gate is the merge's job (see [`Self::merge`]); the
        // count is what lets it see an INTRA-file duplicate too.
        let mut seen_writes: HashMap<String, usize> = HashMap::new();
        collect_literal_constants(ast, ast.root(), &[], &mut h.constant_writes, &mut seen_writes);
        // Stage 2b: every constant this file ASSIGNS, by bare name — recorded
        // BEFORE the merge's C5 gates drop the non-literal / multiply-assigned
        // ones, because the RBS-object-constant arm must decline on those too.
        for w in &h.constant_writes {
            let bare = w.qualified.rsplit("::").next().unwrap_or(&w.qualified).to_string();
            h.constant_write_bare_names.insert(bare);
        }

        // Pass 2: every `ConstantRead` whose `name` the FROZEN core knows, so the
        // merge can register an instance-class id for it. This lets both
        // `Pathname.new(...)` instances AND bare singleton constants (`Time`,
        // `Array`, ...) carry a registry identity that round-trips for rendering.
        //
        // ADR-0042 Slice 2: a QUALIFIED RBS-known constant read (`ERB::Util`)
        // counts too, so it carries a registry id that round-trips for
        // `Singleton` rendering. `knows_class` (short key) covers top-level and
        // the merged composite; the added `knows_qualified_class` covers a
        // namespaced name the short map lacks.
        //
        // Today's third term — `!idx.classes.contains_key(name)` — is NOT
        // reproduced, and cannot change the result: a source class was already
        // registered by Pass 1, and `register` is idempotent, so the skipped call
        // was a no-op (issue #92 §2.1, pinned by `register_is_idempotent`).
        let mut seen_names: HashSet<&str> = HashSet::new();
        for (_, node) in ast.iter() {
            if let Node::ConstantRead { name, .. } = node {
                if !name.is_empty()
                    && (core.knows_class(name) || core.knows_qualified_class(name))
                    && seen_names.insert(name.as_str())
                {
                    h.rbs_constant_names.push(name.clone());
                }
            }
        }

        // Pass 4a (ADR-0038): every project instance + singleton `def` body by
        // QUALIFIED owner name (the same lexical walk, so `module Gitlab; module
        // Database` keys `Gitlab::Database` — matching a
        // `Gitlab::Database.read_only?` receiver). FILE-RELATIVE: the merge
        // stamps the slice position on to build each `FoldSite`.
        walk_fold_defs(ast, ast.root(), &[], &mut h.fold_defs);

        h
    }

    /// **Issue #92 — the SERIAL half.** Fold per-file [`Harvest`]es into one
    /// project index, then run every genuinely cross-file pass over the complete
    /// state. `files` pairs each harvest with ITS OWN AST, in the caller's file
    /// order.
    ///
    /// ## The order is normative — never sort `files`
    ///
    /// Today's order is `expand_check_paths`' (each directory argument expands to
    /// its recursive `**/*.rb` SORTED, arguments concatenated in ARGUMENT order),
    /// and it reaches diagnostics twice: `method_visibilities` is first-write-wins
    /// and `includes` is an ordered append, so `rigor check a.rb b.rb` and
    /// `rigor check b.rb a.rb` legitimately differ (issue #92 §3.2/§3.5).
    /// Normalising the order here would be a behaviour change, not a cleanup.
    ///
    /// ## Three phases, in this order
    ///
    /// * **M1 — ordered replay.** Each ordered harvest field, replayed pass by
    ///   pass across all files (pass by pass, NOT file by file: `names` is
    ///   appended by Pass 1 and Pass 2 both, so the ClassId order is the pass
    ///   order interleaved with the file order).
    /// * **M2 — barrier aggregates.** Cheap, need the complete replayed state:
    ///   the C1 constant-shadow tables, the C5b literal-constant gates, the Pass
    ///   2b tuple-element registry + declaration-only set.
    /// * **M3 — AST-consuming passes.** Pass 3 (tier-4b returns, typed against
    ///   the complete index) and Pass 4 (the definers inversion + the
    ///   interprocedural literal-tail fold, which resolves calls into OTHER
    ///   files' bodies). These are why the merge still takes the ASTs — issue #92
    ///   §5: harvest-then-evict is NOT unblocked by this decomposition.
    ///
    /// ## Why the harvest is BORROWED (`H: Borrow<Harvest>`)
    ///
    /// The merge only ever READS each harvest, so the parameter is generic over
    /// anything that lends one out: `check` passes the owned `Harvest`es its
    /// stage-1 rayon closure just produced (`H = Harvest`), while the LSP passes
    /// `&Harvest` borrowed from the per-file harvests tier 1 HOLDS across
    /// keystrokes (`Arc<Harvest>`, so a context swap stays a pointer copy). An
    /// owned-only parameter would force the LSP to re-harvest every project file
    /// on every dispatch — which is exactly the cost the held table removes. No
    /// behaviour rides on this: `H` is erased before the first read.
    pub fn merge<H: Borrow<Harvest>>(files: &[(H, &LoweredAst)], core: &CoreIndex) -> Self {
        let mut idx = SourceIndex::default();

        // === M1: ordered replay ============================================

        // Pass 1: source class/module structure, across ALL files in order.
        for (h, _) in files {
            let h = h.borrow();
            for c in &h.source_classes {
                idx.add_source(&c.name, c.superclass.clone(), &c.methods);
            }
        }

        // Pass 1b: the lexically-qualified override index. First-write-wins on
        // superclass + visibility, ordered append-with-dedup on includes — so
        // this replay is exactly today's call sequence.
        for (h, _) in files {
            let h = h.borrow();
            for oc in &h.override_classes {
                idx.ingest_override_class(
                    &oc.qualified,
                    oc.superclass.clone(),
                    &oc.methods,
                    &oc.method_visibilities,
                    &oc.includes,
                );
            }
        }

        // Passes 1c / 1d / 1e + stage 2b's bare-name set: pure unions. Order-free
        // (commutative + idempotent), merged in file order anyway.
        for (h, _) in files {
            let h = h.borrow();
            idx.toplevel_defs.extend(h.toplevel_defs.iter().cloned());
            for (owner, methods) in &h.discovered_methods {
                idx.discovered_methods
                    .entry(owner.clone())
                    .or_default()
                    .extend(methods.iter().cloned());
            }
            for (method, indices) in &h.mutated_params {
                idx.mutated_params
                    .entry(method.clone())
                    .or_default()
                    .extend(indices.iter().copied());
            }
            idx.project_constant_write_names.extend(h.constant_write_bare_names.iter().cloned());
        }

        // Pass 2: register the RBS-known constant reads. Runs AFTER Pass 1's
        // registrations, exactly as before — the two share the `names` vector, so
        // this is the ClassId order.
        for (h, _) in files {
            let h = h.borrow();
            for name in &h.rbs_constant_names {
                idx.register(name);
            }
        }

        // === M2: barrier aggregates ========================================

        // C1: derive the constant-shadow tables from the lexically-qualified
        // override index built above (the same class/module set Ruby's lexical
        // constant lookup sees). A key with no `::` is a TOPLEVEL definition
        // (shadows everywhere); a namespaced key contributes its containing
        // namespace under the constant's last segment (shadows only where
        // lexically visible). Collected keys first to satisfy the borrow checker.
        let qualified_defs: Vec<String> = idx.override_classes.keys().cloned().collect();
        for qualified in &qualified_defs {
            let segs: Vec<&str> = qualified.split("::").collect();
            let Some((name, ns)) = segs.split_last() else { continue };
            if ns.is_empty() {
                idx.toplevel_constants.insert((*name).to_string());
            } else {
                let ns_vec: Vec<String> = ns.iter().map(|s| (*s).to_string()).collect();
                let entry = idx.nested_constant_namespaces.entry((*name).to_string()).or_default();
                if !entry.contains(&ns_vec) {
                    entry.push(ns_vec);
                }
            }
        }

        // C5b: the project-wide constant gates. A QUALIFIED name qualifies iff it
        // is assigned EXACTLY ONCE project-wide, its RHS harvested to a
        // `ConstLit` (fully literal), and its bare name does NOT also name a
        // class/module. Ambiguity (multiple writes to the same qualified name, a
        // non-literal RHS, a class-name collision) declines. The recorded value
        // is keyed by BARE name + DEFINING NAMESPACE so the use-site consults it
        // lexically — a constant only visible in its defining namespace never
        // folds at an unrelated use site (the app/models concern-constant FP).
        //
        // `lit_first` keeps the FIRST write in file-then-walk order and
        // `lit_writes` sums the per-file counts, so a duplicate ACROSS files and
        // a duplicate WITHIN one file decline identically — which is what today's
        // single shared `lit_first`/`lit_multi` pair does.
        //
        // The `file` stamp comes from the paired AST, never from the harvest —
        // see the persistence hazard on `HarvestedConst`.
        let mut lit_first: HashMap<String, (Vec<String>, u64, Option<ConstLit>)> = HashMap::new();
        let mut lit_writes: HashMap<String, usize> = HashMap::new();
        for (h, ast) in files {
            let h = h.borrow();
            for w in &h.constant_writes {
                *lit_writes.entry(w.qualified.clone()).or_insert(0) += w.writes;
                lit_first
                    .entry(w.qualified.clone())
                    .or_insert_with(|| (w.namespace.clone(), ast.file_id(), w.lit.clone()));
            }
        }
        for (qualified, (namespace, file, lit)) in lit_first {
            if lit_writes.get(&qualified).is_some_and(|n| *n >= 2) {
                continue;
            }
            let bare = qualified.rsplit("::").next().unwrap_or(&qualified).to_string();
            // A constant is never a class/module: a name collision (the qualified
            // name names an override class, or the bare name a source class)
            // declines — the singleton / source-class path owns that name.
            if idx.override_classes.contains_key(&qualified) || idx.classes.contains_key(&bare) {
                continue;
            }
            if let Some(l) = lit {
                // Stage 2e: the qualified twin, keyed by the full path so a
                // `::A::B::C::CONST` read resolves. Same entry set, same gates.
                idx.qualified_literal_constants
                    .insert(qualified, (namespace.clone(), file, l.clone()));
                idx.literal_constants.entry(bare).or_default().push((namespace, file, l));
            }
        }

        // Pass 2b (MultiWrite substrate Slice 2): register an id for every class
        // an RBS TUPLE return names as an element. Pass 2 above can only see
        // classes the SOURCE mentions, but a tuple element is reached THROUGH a
        // call — `Process.wait2 : [Integer, Process::Status]` names
        // `Process::Status` in no source file — so without this the element has
        // no registry identity and its `Nominal` cannot be minted (the slot would
        // silently degrade to `Dynamic[top]`).
        //
        // Declaration-driven, not name-driven: the set is whatever the loaded RBS
        // declares (see `CoreIndex::tuple_return_class_names`), so no class name
        // is special-cased here. A name that is already a source class keeps the
        // source registration (the project's own class wins, as everywhere else),
        // and an element the loaded RBS does not model is skipped — an
        // unregistered name simply leaves that slot `Dynamic[top]` (silent).
        //
        // CROSS-FILE by construction: `!name_to_id.contains_key` asks "did NO
        // analyzed file name this class?", which no per-file harvest can answer.
        for name in core.tuple_return_class_names() {
            if !idx.classes.contains_key(name)
                && (core.knows_class(name) || core.knows_qualified_class(name))
            {
                // A name the source ALREADY registered (a class it declares, or
                // a constant it reads) is not declaration-only — see
                // `is_declaration_only_class`.
                if !idx.name_to_id.contains_key(name) {
                    idx.declaration_only_classes.insert(name.to_string());
                }
                idx.register(name);
            }
        }

        // === M3: AST-consuming passes ======================================

        let asts: Vec<&LoweredAst> = files.iter().map(|(_, ast)| *ast).collect();

        // Pass 3 (ADR-0023 tier-4b): infer per-method RETURN types. Runs AFTER the
        // source/registry maps are complete (so a Typer over `&idx` sees every
        // project class), and produces a fresh map that is then assigned — we must
        // NOT mutate `idx.method_returns` while `&idx` is immutably borrowed for
        // typing, so the inference returns a value.
        let (returns, param_bound) = infer_method_returns(&idx, core, &asts);
        idx.method_returns = returns;
        idx.param_bound_returns = param_bound;

        // Pass 4 (ADR-0038): interprocedural literal-tail return folding. Runs
        // AFTER Pass 1b (`override_classes`, the ancestry the degrade + implicit-
        // self resolution walk) and needs no `core`/typing state. Stamps each
        // harvested def site with its file's SLICE POSITION, inverts to a definers
        // index, then folds each method's tail to a scalar literal (resolving
        // nested project calls — into other files' ASTs — and applying the
        // overridable degrade).
        let mut defs: HashMap<(String, String, DefKind), Vec<FoldSite>> = HashMap::new();
        for (ast_idx, (h, _)) in files.iter().enumerate() {
            for d in &h.borrow().fold_defs {
                defs.entry((d.owner.clone(), d.method.clone(), d.kind)).or_default().push(
                    FoldSite {
                        ast_idx,
                        tail: d.tail,
                        has_explicit_return: d.has_explicit_return,
                    },
                );
            }
        }
        idx.definers = invert_definers(&defs);
        idx.literal_returns = idx.compute_literal_returns(&asts, &defs);

        idx
    }

    /// Whether `name` is a PROJECT-WIDE toplevel method (a toplevel `def` in any
    /// analyzed file, or an in-source Object/Kernel/BasicObject reopen method) —
    /// the `call.unresolved-toplevel` cross-file suppression surface.
    /// Whether the PROJECT declares instance method `method` on `class_name`
    /// (qualified) with an in-source `def` — including a def nested in a block or
    /// a conditional inside the class body. See [`Self::discovered_methods`].
    ///
    /// A pure SILENCER: `true` means "do not witness absence here". It is never
    /// consulted to prove a method exists for any positive inference, so a false
    /// `true` costs coverage, never correctness.
    pub fn project_declares_method(&self, class_name: &str, method: &str) -> bool {
        self.discovered_methods.get(class_name).is_some_and(|m| m.contains(method))
    }

    /// Whether SOME project method named `method` mutates its positional
    /// parameter at `index` in place. See [`Self::mutated_params`].
    pub fn method_mutates_param(&self, method: &str, index: usize) -> bool {
        self.mutated_params.get(method).is_some_and(|s| s.contains(&index))
    }

    pub fn is_toplevel_def(&self, name: &str) -> bool {
        self.toplevel_defs.contains(name)
    }

    /// Register a name in the id registry (idempotent), returning nothing.
    fn register(&mut self, name: &str) {
        if !self.name_to_id.contains_key(name) {
            let id = self.names.len() as u32;
            self.names.push(name.to_string());
            self.name_to_id.insert(name.to_string(), id);
        }
    }

    /// Fold one (re)definition of a source class into the index, also registering
    /// its instance-class id.
    fn add_source(&mut self, name: &str, superclass: Option<String>, methods: &[String]) {
        let entry = self.classes.entry(name.to_string()).or_default();
        if entry.superclass.is_none() {
            entry.superclass = superclass;
        }
        for m in methods {
            entry.methods.insert(m.clone());
        }
        self.register(name);
    }

    /// Whether `name` names a class defined in source (has harvested structure).
    pub fn knows_class(&self, name: &str) -> bool {
        self.classes.contains_key(name)
    }

    /// C5: the harvested fully-literal value of constant `name` VISIBLE at a use
    /// site with lexical prefix `use_prefix`, or `None`. A recorded entry applies
    /// iff its defining namespace is an initial segment run of `use_prefix` (Ruby
    /// lexical lookup: toplevel is visible everywhere, a nested constant only
    /// within its namespace); among visible entries the LONGEST-namespace
    /// (innermost) wins. The `ConstantRead` arm consults this BEFORE the
    /// singleton gate and re-interns the value via `Typer::intern_const_lit`.
    ///
    /// `use_file` is the [`rigor_parse::LoweredAst::file_id`] of the file the use
    /// site is in, and an entry only applies when it matches the file that
    /// ASSIGNED the constant. The reference's constant-value table is per-file
    /// (see the field docs); before this gate a same-namespace cross-file read
    /// folded here and was silent in the oracle.
    pub fn literal_constant(
        &self,
        name: &str,
        use_prefix: &[String],
        use_file: u64,
    ) -> Option<&ConstLit> {
        self.literal_constants
            .get(name)?
            .iter()
            .filter(|(_, file, _)| *file == use_file)
            .filter(|(ns, _, _)| ns.len() <= use_prefix.len() && use_prefix[..ns.len()] == ns[..])
            .max_by_key(|(ns, _, _)| ns.len())
            .map(|(_, _, lit)| lit)
    }

    /// Collection-shape stage 2b's negative check: whether SOME harvested
    /// literal constant named `name` is lexically visible at `use_prefix`,
    /// **ignoring which file assigned it**.
    ///
    /// Deliberately file-AGNOSTIC, and deliberately separate from
    /// [`Self::literal_constant`]. That method types a use site, so slice A's
    /// per-file gate belongs there; this one is a DECLINE predicate whose job is
    /// to be maximally conservative, and narrowing it would be the one way slice
    /// A could make the `ENV` arm fire somewhere it previously stayed silent.
    /// (In practice `project_writes_constant` already subsumes it — that set is
    /// built from every constant write before the literal gates — but the
    /// redundancy is the point.)
    pub fn literal_constant_visible_any_file(&self, name: &str, use_prefix: &[String]) -> bool {
        self.literal_constants.get(name).is_some_and(|entries| {
            entries.iter().any(|(ns, _, _)| {
                ns.len() <= use_prefix.len() && use_prefix[..ns.len()] == ns[..]
            })
        })
    }

    /// Collection-shape stage 2b: whether the project ASSIGNS a constant with
    /// this bare name anywhere (see [`Self::project_constant_write_names`]).
    pub fn project_writes_constant(&self, name: &str) -> bool {
        self.project_constant_write_names.contains(name)
    }

    /// Collection-shape stage 2e: the harvested value of a constant named by a
    /// QUALIFIED path (`::A::B::C::CONST` / `A::B::C::CONST` — both lower to one
    /// `ConstantRead` whose `name` is `"A::B::C::CONST"`, the leading `::` is not
    /// preserved). `None` for a bare name (the C5 map owns those).
    ///
    /// Resolution mirrors PR #64's precedent for qualified REFERENCES: the path
    /// is taken AS WRITTEN and tried against the use site's lexical contexts —
    /// every initial segment run of `use_prefix` (innermost first) plus the
    /// top-level reading. **Ambiguity DECLINES**: if two distinct candidate keys
    /// both name a harvested constant, we return `None` rather than guess which
    /// one Ruby's lookup would reach (a strict under-emit; the reference resolves
    /// it precisely, so this can only lose recall, never fire wrongly).
    ///
    /// A resolved key must then pass the SAME lexical-visibility filter
    /// [`Self::literal_constant`] applies to the bare spelling: the constant's
    /// DEFINING namespace has to be an initial segment run of `use_prefix`. This
    /// makes stage 2e a pure SPELLING extension of the already-shipped C5 gate
    /// rather than a wider resolution reach — load-bearing, and measured: without
    /// it, a cross-namespace path (gitlab
    /// `Gitlab::GitalyClient::DiffBlob::ATTRS` read from
    /// `…::DiffBlobsStitcher`) folded here while the reference stayed silent, an
    /// oracle FP on the sweep.
    ///
    /// `use_file` applies the SAME per-file consumption gate as
    /// [`Self::literal_constant`] — a qualified path is only a SPELLING of the
    /// same harvest, so it inherits the same restriction.
    pub fn qualified_literal_constant(
        &self,
        name: &str,
        use_prefix: &[String],
        use_file: u64,
    ) -> Option<&ConstLit> {
        if !name.contains("::") {
            return None;
        }
        let mut hit: Option<(&String, &HarvestedConst)> = None;
        // Candidate keys: `<prefix[..i]>::<name>` for every i (the lexical
        // nesting runs), plus `name` itself (i == 0 yields exactly that).
        for i in 0..=use_prefix.len() {
            let key = if i == 0 {
                name.to_string()
            } else {
                format!("{}::{}", use_prefix[..i].join("::"), name)
            };
            if let Some((k, v)) = self.qualified_literal_constants.get_key_value(&key) {
                match hit {
                    // The same constant reached by two spellings is not an
                    // ambiguity; two DIFFERENT keys are.
                    Some((prev, _)) if prev != k => return None,
                    Some(_) => {}
                    None => hit = Some((k, v)),
                }
            }
        }
        let (_, (ns, file, lit)) = hit?;
        (*file == use_file
            && ns.len() <= use_prefix.len()
            && use_prefix[..ns.len()] == ns[..])
            .then_some(lit)
    }

    /// C1 (constant-shadow gate): whether a BARE read of constant `name` at a use
    /// site with lexical prefix `use_prefix` (the enclosing class/module segment
    /// vector, empty at toplevel) is SHADOWED by a project definition — i.e. the
    /// project name resolves in Ruby's lexical lookup, so the core-RBS singleton
    /// must NOT be witnessed. This REPLACES the pre-C1 bare-name project-wide
    /// `!knows_class(name)` suppression with a lexically precise one, matching the
    /// reference's `lexical_constant_candidates` walk:
    ///
    ///   * a TOPLEVEL project definition shadows everywhere;
    ///   * a NESTED definition `N::name` shadows only where `N` is an initial
    ///     segment run of `use_prefix` (`N` ∈ `Module.nesting` of the use site);
    ///   * a name known as a project class but placed by the qualified walk at
    ///     neither position (def-nested / walk gap) falls back to the pre-C1
    ///     blanket suppression — ambiguity resolves to silent (never an FP).
    ///
    /// FP-safe by construction: the only behavior change vs the old gate is that a
    /// nested-only definition STOPS suppressing at use sites it is not lexically
    /// visible from — a strict relaxation whose every new firing the reference
    /// (which resolves identically-lexically) confirms.
    pub fn constant_shadowed(&self, name: &str, use_prefix: &[String]) -> bool {
        if self.toplevel_constants.contains(name) {
            return true;
        }
        match self.nested_constant_namespaces.get(name) {
            Some(namespaces) => namespaces.iter().any(|ns| {
                ns.len() <= use_prefix.len() && use_prefix[..ns.len()] == ns[..]
            }),
            // Not seen by the qualified walk at all: preserve pre-C1 behavior for
            // any project class the walk did not qualify (def-nested / walk gap).
            None => self.classes.contains_key(name),
        }
    }

    /// Whether the project defines a constant named `name` ANYWHERE (toplevel,
    /// nested, or as a discovered class/module) — the scope-INDEPENDENT
    /// companion to [`Self::constant_shadowed`]. Used by `type_dot_new`'s
    /// stdlib-mint decline: a project-defined name colliding with a loaded-RBS
    /// short key (`Selector = Data.define(...)` vs an RBS `Selector`) keeps its
    /// project mint regardless of the caller's lexical-scope attachment
    /// (callers without `with_lexical_scopes` have an empty prefix, which would
    /// make the lexical predicate miss a nested definition). Conservative
    /// toward KEEPING the mint — the pre-existing behavior.
    pub fn constant_defined_anywhere(&self, name: &str) -> bool {
        self.toplevel_constants.contains(name)
            || self.nested_constant_namespaces.contains_key(name)
            || self.classes.contains_key(name)
    }

    /// The DISCOVERED written superclass (last path component) of a source class,
    /// or `None` when the name is unknown OR is a source class/module WITHOUT a
    /// `class Foo < Bar` superclass (a bare `class Foo`/`module Foo` — the two are
    /// indistinguishable in the collapsed discovery table). This is the rigor-rs
    /// analogue of the reference's `discovered_superclasses` map: a `Some` result
    /// both certifies `name` as a project exception-comparable CLASS and gives
    /// `flow.shadowed-rescue-clause`'s project chain-walk its next parent link.
    pub fn discovered_superclass(&self, name: &str) -> Option<&str> {
        self.classes.get(name).and_then(|c| c.superclass.as_deref())
    }

    /// Whether `name` is registered in the instance-class id space (source class
    /// or registered RBS instance class).
    pub fn is_registered(&self, name: &str) -> bool {
        self.name_to_id.contains_key(name)
    }

    /// MultiWrite substrate Slice 2: whether `name` got its registry id ONLY
    /// from the RBS tuple-element sweep (Pass 2b) — the analyzed source neither
    /// declares the class nor names the constant anywhere, so a value of this
    /// class can ONLY have come from an RBS DECLARATION (`Process.wait2`'s
    /// `[Integer, Process::Status]`).
    ///
    /// ## Why the rules need this (an FP measured, not theorised)
    ///
    /// The rules' qualified-witness arm reports a method as undefined over the
    /// ADR-0042 qualified surface. For a NAMESPACED *gem* class that surface is
    /// knowingly WEAKER than the oracle's: the reference supplements the rbs gem
    /// with `data/vendored_gem_sigs/` (rubygems / cgi / nokogiri / prism / …),
    /// which rigor-rs does not vendor. Probed: `Gem::Version.new("1.0").segments`
    /// — `segments` is declared ONLY in the reference's `rubygems_extras.rbs`, so
    /// the ORACLE IS SILENT and an unrestricted arm fired ⇒ a false positive.
    ///
    /// Restricting the arm to declaration-only classes closes that door
    /// structurally rather than by name: a project that writes `Gem::Version`
    /// registers the constant in Pass 2, so the class is NOT declaration-only and
    /// the witness stays silent (the pre-Slice-2 behaviour — a coverage gap in
    /// the FP-safe direction). Nothing about the value's TYPE changes, so
    /// `sig-gen` / `annotate` keep their (oracle-matching) precision.
    ///
    /// The residual surface is closed and auditable: over the vendored rbs-4.0.3
    /// the only tuple-element class reachable from a TOP-LEVEL receiver — the
    /// only receivers whose tuple return resolves, since the lookup rides the
    /// SHORT-key map, which holds no qualified keys — is `Process::Status`.
    /// Remove this restriction when rigor-rs vendors the reference's gem-sig
    /// extras.
    pub fn is_declaration_only_class(&self, name: &str) -> bool {
        self.declaration_only_classes.contains(name)
    }

    /// The [`ClassId`] for a registered class name. `None` if not registered.
    pub fn class_id(&self, name: &str) -> Option<ClassId> {
        self.name_to_id.get(name).map(|&i| ClassId(SOURCE_CLASS_BASE + i))
    }

    /// Resolve a registry [`ClassId`] back to its class name. `None` if the id is
    /// not in the source range or out of bounds.
    pub fn class_name_for_id(&self, class: ClassId) -> Option<&str> {
        if class.0 < SOURCE_CLASS_BASE {
            return None;
        }
        self.names
            .get((class.0 - SOURCE_CLASS_BASE) as usize)
            .map(|s| s.as_str())
    }

    /// The inferred CORE return-class NAME for a project method `(class,
    /// method)`, if tier-4b inferred one. `None` ⇒ no entry ⇒ the call types
    /// Dynamic (silent). Re-intern at the call site via [`CoreIndex::class_id`].
    ///
    /// [`CoreIndex::class_id`]: rigor_index::CoreIndex::class_id
    pub fn method_return(&self, class: &str, method: &str) -> Option<&str> {
        self.method_returns
            .get(&(class.to_string(), method.to_string()))
            .map(|s| s.as_str())
    }

    /// ADR-0038 interprocedural literal-tail fold — the folded scalar literal a
    /// `Const.method` SINGLETON call yields, or `None` to decline (Dynamic,
    /// silent). `receiver_name` is the receiver constant's dotted name as written
    /// (`Gitlab::Database`, `::Gitlab::Database`); resolution is OWN-CLASS only
    /// (the reference `try_singleton_method_inference` walks no singleton
    /// ancestry) and the returned value already has the overridable degrade
    /// applied. The call site interns the result as a `Type::Constant`.
    pub fn const_singleton_literal(&self, receiver_name: &str, method: &str) -> Option<Scalar> {
        let owner = receiver_name.strip_prefix("::").unwrap_or(receiver_name);
        self.literal_returns
            .get(&(owner.to_string(), method.to_string(), DefKind::Singleton))
            .cloned()
    }

    /// ADR-0038 interprocedural literal-tail fold — the folded scalar literal an
    /// IMPLICIT-SELF call `method` yields inside the enclosing scope `self_qual`
    /// (a qualified class/module name) whose method kind is `self_kind`, or `None`
    /// to decline. A singleton enclosing method (`def self.x`) resolves `method`
    /// against `self_qual`'s OWN singleton table; an instance method resolves it
    /// through `self_qual`'s project ancestry (nearest ancestor defining it), the
    /// same ancestor walk the override-visibility rule uses — so an unrelated
    /// same-name method elsewhere is NOT resolved (the cross-class zero-FP
    /// keystone). The value already has the overridable degrade applied.
    pub fn implicit_self_literal(
        &self,
        self_qual: &str,
        self_kind: DefKind,
        method: &str,
    ) -> Option<Scalar> {
        let (owner, kind) = match self_kind {
            DefKind::Singleton => (self_qual.to_string(), DefKind::Singleton),
            DefKind::Instance => (self.resolve_instance_owner(self_qual, method)?, DefKind::Instance),
        };
        self.literal_returns
            .get(&(owner, method.to_string(), kind))
            .cloned()
    }

    /// The ADR-0023 tier-4b call-site PARAMETER-BINDING descriptor for a project
    /// method `(class, method)`, if its tail roots on a positional param. `None`
    /// ⇒ no param-bound entry ⇒ the call site falls through (Dynamic, silent).
    /// The param-INDEPENDENT [`Self::method_return`] takes precedence at the
    /// call site: it is consulted FIRST and this map only on a miss. That
    /// precedence — not exclusivity — is the contract. A method reopened across
    /// files CAN have an entry in both maps (each def site is dispatched on its
    /// own; issue #92 §8), which is exactly why the order matters.
    /// See [`ParamBoundReturn`].
    pub fn param_bound_return(&self, class: &str, method: &str) -> Option<&ParamBoundReturn> {
        self.param_bound_returns
            .get(&(class.to_string(), method.to_string()))
    }

    /// The SOURCE class name behind a `Nominal { class }` whose `ClassId` is in
    /// the source registry range. `None` for a core-range id or a non-Nominal
    /// carrier. This is the source-side companion to the core
    /// `CoreIndex::class_name_of` (which returns `None` for a source-range id):
    /// the tier-4b call hook uses it to recover the receiver's project-class name
    /// so it can look up that class's inferred method return.
    pub fn class_name_for_id_of(
        &self,
        interner: &Interner,
        ty: rigor_types::TypeId,
    ) -> Option<&str> {
        match interner.get(ty) {
            rigor_types::Type::Nominal { class, .. } => self.class_name_for_id(*class),
            _ => None,
        }
    }

    /// ADR-35 slice 1: the discovered instance-method VISIBILITY of `method` on
    /// the QUALIFIED project class `class` (its OWN table only — not inherited).
    /// `None` when `class` is not in the override index or does not record
    /// `method`.
    pub fn method_visibility(&self, class: &str, method: &str) -> Option<Visibility> {
        self.override_classes
            .get(class)
            .and_then(|c| c.method_visibilities.get(method).copied())
    }

    /// ADR-35 slice 1: the NEAREST project ancestor of the QUALIFIED class
    /// `class` that DEFINES the instance method `method`, paired with that
    /// ancestor's discovered visibility for `method` (`None` when the ancestor
    /// defines the method but its visibility is UNKNOWN — e.g. `private def` /
    /// dynamic form).
    ///
    /// MRO-ordered breadth-first walk over the LEXICALLY-QUALIFIED override index:
    /// included / prepended modules FIRST, then the superclass (Ruby's MRO
    /// ordering). Each ancestor name is resolved against the subclass's lexical
    /// nesting (the reference's `resolve_override_ancestor_name`) and dropped if
    /// it names no PROJECT class (RBS / third-party ancestors are NOT walked —
    /// slice-1 carve-out). Cycle-guarded and capped at
    /// [`OVERRIDE_ANCESTOR_WALK_LIMIT`] visited nodes (returns `None` past the cap
    /// — a missed witness, never an FP).
    ///
    /// An ancestor DEFINES `method` when it appears in that ancestor's own
    /// `methods` set OR its `method_visibilities` table; the walk STOPS at the
    /// first such ancestor.
    ///
    /// ## The zero-FP keystones (do NOT weaken)
    ///
    /// 1. **Lexical qualification.** The index is keyed by FULL qualified name, so
    ///    a nested `module Params` in `IssuableFinder` is `IssuableFinder::Params`
    ///    — it never merges with `Groups::Params`. Collapsing them invented
    ///    phantom ancestors / methods (the gitlab-foss FP cluster).
    /// 2. **Never synthesize Public.** The returned visibility is the ancestor's
    ///    RECORDED entry or `None`. The caller must treat `None` as "cannot prove
    ///    a reduction" and STAY SILENT — never fabricate `Public` from a missing
    ///    entry (the reference's Mastodon 160 → 35 cluster).
    pub fn nearest_ancestor_defining(
        &self,
        class: &str,
        method: &str,
    ) -> Option<(String, Option<Visibility>)> {
        let mut queue: Vec<String> = self.override_ancestor_names(class);
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(class.to_string());
        let mut visited = 0usize;

        while !queue.is_empty() {
            let current = queue.remove(0);
            if !seen.insert(current.clone()) {
                continue;
            }
            visited += 1;
            if visited > OVERRIDE_ANCESTOR_WALK_LIMIT {
                return None; // cap exceeded ⇒ decline (never an FP).
            }
            if let Some(entry) = self.override_classes.get(&current) {
                let defines = entry.methods.contains(method)
                    || entry.method_visibilities.contains_key(method);
                if defines {
                    // Stop at the nearest defining ancestor; its visibility may be
                    // None (unknown) — the caller treats unknown as "cannot prove".
                    return Some((current.clone(), entry.method_visibilities.get(method).copied()));
                }
                // Not defined here ⇒ enqueue this ancestor's own ancestors.
                for next in self.override_ancestor_names(&current) {
                    queue.push(next);
                }
            }
        }
        None
    }

    /// The direct PROJECT ancestors of the QUALIFIED `class`, resolved + ordered:
    /// each `include` / `prepend` (in source order) FIRST, then the `superclass`
    /// — Ruby's MRO ordering. Names that resolve to no project class (RBS /
    /// third-party) are dropped (slice-1 carve-out).
    fn override_ancestor_names(&self, class: &str) -> Vec<String> {
        let Some(entry) = self.override_classes.get(class) else {
            return Vec::new();
        };
        let mut names = Vec::new();
        for inc in &entry.includes {
            if let Some(resolved) = self.resolve_override_ancestor(class, inc) {
                names.push(resolved);
            }
        }
        if let Some(sup) = &entry.superclass {
            if let Some(resolved) = self.resolve_override_ancestor(class, sup) {
                names.push(resolved);
            }
        }
        names
    }

    /// Resolve an as-written ancestor name against the subclass's lexical
    /// nesting, returning the QUALIFIED project class name it names, or `None` if
    /// it names no project class. Mirrors the reference's
    /// `resolve_override_ancestor_name`: try `<prefix>::<raw>` for each enclosing
    /// scope of the subclass, longest-prefix first, falling back to the bare name.
    /// A leading `::` on the raw name is stripped (a top-level absolute path).
    fn resolve_override_ancestor(&self, subclass: &str, raw: &str) -> Option<String> {
        let raw = raw.strip_prefix("::").unwrap_or(raw);
        let segments: Vec<&str> = subclass.split("::").collect();
        // Drop the subclass's own last segment; try its enclosing scopes
        // longest-first, then the top level (bare `raw`).
        for i in (0..segments.len()).rev() {
            let candidate = if i == 0 {
                raw.to_string()
            } else {
                format!("{}::{}", segments[..i].join("::"), raw)
            };
            if self.override_classes.contains_key(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    /// Fold one (re)definition of a QUALIFIED override class into the index.
    fn ingest_override_class(
        &mut self,
        qualified: &str,
        superclass: Option<String>,
        methods: &[String],
        method_visibilities: &[(String, Visibility)],
        includes: &[String],
    ) {
        let entry = self.override_classes.entry(qualified.to_string()).or_default();
        if entry.superclass.is_none() {
            entry.superclass = superclass;
        }
        for m in methods {
            entry.methods.insert(m.clone());
        }
        // First-write-wins per method name (stable cross-file view).
        for (m, vis) in method_visibilities {
            entry.method_visibilities.entry(m.clone()).or_insert(*vis);
        }
        for inc in includes {
            if !entry.includes.contains(inc) {
                entry.includes.push(inc.clone());
            }
        }
    }

    // -----------------------------------------------------------------------
    // ADR-0038 — interprocedural literal-tail return folding
    // -----------------------------------------------------------------------

    /// Compute the `(qualified owner, method, kind) -> folded scalar` table from
    /// the harvested `defs` (which carry each method's tail node), applying the
    /// overridable-method degrade. A per-key memo makes the recursive body-to-body
    /// fold (`read_write? = !read_only?`) linear; a per-resolution `visiting` set
    /// makes a recursive method (`def loopy; loopy; end`) decline rather than spin.
    /// A per-candidate [`AncestorClosures`] memo (#94) makes the degrade gate's
    /// ancestor walk run once per definer-candidate instead of once per
    /// `(candidate, owner)` pair.
    fn compute_literal_returns(
        &self,
        asts: &[&LoweredAst],
        defs: &HashMap<(String, String, DefKind), Vec<FoldSite>>,
    ) -> HashMap<(String, String, DefKind), Scalar> {
        let mut memo: HashMap<(String, String, DefKind), Option<Scalar>> = HashMap::new();
        // Born here, dies here: the closure memo is valid exactly as long as
        // `override_classes` is frozen, which is exactly this call (every reader
        // below takes `&self`, and M1's replay finished before M3 started).
        let mut closures: AncestorClosures = AncestorClosures::new();
        for key in defs.keys() {
            let mut visiting: HashSet<(String, String, DefKind)> = HashSet::new();
            self.resolve_fold_key(key, defs, asts, &mut memo, &mut visiting, &mut closures);
        }
        memo.into_iter().filter_map(|(k, v)| v.map(|s| (k, s))).collect()
    }

    /// Resolve one `(owner, method, kind)`'s folded literal (memoized), applying
    /// the overridable degrade: a value-pinned base return is dropped when a
    /// RELATED subclass/includer redefines the method (else adopting the base's
    /// literal as a flow constant is unsound — the reference `degrade_if_overridable`).
    fn resolve_fold_key(
        &self,
        key: &(String, String, DefKind),
        defs: &HashMap<(String, String, DefKind), Vec<FoldSite>>,
        asts: &[&LoweredAst],
        memo: &mut HashMap<(String, String, DefKind), Option<Scalar>>,
        visiting: &mut HashSet<(String, String, DefKind)>,
        closures: &mut AncestorClosures,
    ) -> Option<Scalar> {
        if let Some(v) = memo.get(key) {
            return v.clone();
        }
        if visiting.contains(key) {
            return None; // cycle (recursive method) ⇒ decline, don't memoize.
        }
        visiting.insert(key.clone());
        let raw = self.fold_key_sites(key, defs, asts, memo, visiting, closures);
        let result = match raw {
            Some(_) if self.overridden_in_project(&key.0, &key.1, key.2, closures) => None,
            other => other,
        };
        visiting.remove(key);
        memo.insert(key.clone(), result.clone());
        result
    }

    /// Fold every (re)definition site of `key` and require they AGREE on one
    /// scalar (a disagreeing reopen declines). Any site with an explicit `return`
    /// declines the whole method (we read only the tail).
    fn fold_key_sites(
        &self,
        key: &(String, String, DefKind),
        defs: &HashMap<(String, String, DefKind), Vec<FoldSite>>,
        asts: &[&LoweredAst],
        memo: &mut HashMap<(String, String, DefKind), Option<Scalar>>,
        visiting: &mut HashSet<(String, String, DefKind)>,
        closures: &mut AncestorClosures,
    ) -> Option<Scalar> {
        let sites = defs.get(key)?;
        let mut acc: Option<Scalar> = None;
        for site in sites {
            if site.has_explicit_return {
                return None;
            }
            let ast = asts[site.ast_idx];
            let s = self
                .fold_expr(ast, site.tail, &key.0, key.2, defs, asts, memo, visiting, closures, 0)?;
            match &acc {
                None => acc = Some(s),
                Some(prev) if *prev != s => return None, // disagreeing reopen.
                _ => {}
            }
        }
        acc
    }

    /// Fold one expression node to a scalar literal, or `None` to decline. Handles
    /// literals, `!expr`, an implicit-self project call (resolved against
    /// `self_qual`/`self_kind`), a `Const.method` singleton call, and a core fold
    /// on a value-pinned receiver + args. A leaf that is anything else (a param /
    /// ivar / non-folding call / branch carrier) declines the whole fold — which
    /// is why an if/case/loop-carrier tail or a param-dependent body never folds.
    #[allow(clippy::too_many_arguments)]
    fn fold_expr(
        &self,
        ast: &LoweredAst,
        node_id: NodeId,
        self_qual: &str,
        self_kind: DefKind,
        defs: &HashMap<(String, String, DefKind), Vec<FoldSite>>,
        asts: &[&LoweredAst],
        memo: &mut HashMap<(String, String, DefKind), Option<Scalar>>,
        visiting: &mut HashSet<(String, String, DefKind)>,
        closures: &mut AncestorClosures,
        depth: usize,
    ) -> Option<Scalar> {
        if depth > FOLD_DEPTH_CAP {
            return None;
        }
        match ast.get(node_id) {
            Node::StringLit { value, .. } => Some(Scalar::Str(value.clone())),
            Node::IntegerLit { value, .. } => Some(Scalar::Int(*value)),
            Node::FloatLit { value, .. } => Some(Scalar::Float(*value)),
            Node::SymbolLit { value, .. } => Some(Scalar::Sym(value.clone())),
            Node::NilLit { .. } => Some(Scalar::Nil),
            Node::TrueLit { .. } => Some(Scalar::Bool(true)),
            Node::FalseLit { .. } => Some(Scalar::Bool(false)),
            // An implicit-self project call (`read_only?`). Args are ignored — the
            // fold is param-INDEPENDENT; if the body reads a param the recursive
            // fold declines on that param leaf. A block form is out of scope.
            Node::Call { receiver: None, method, block_body, .. } if block_body.is_empty() => {
                let method = method.clone();
                let (owner, kind) = match self_kind {
                    DefKind::Singleton => (self_qual.to_string(), DefKind::Singleton),
                    DefKind::Instance => {
                        (self.resolve_instance_owner(self_qual, &method)?, DefKind::Instance)
                    }
                };
                self.resolve_fold_key(&(owner, method, kind), defs, asts, memo, visiting, closures)
            }
            Node::Call { receiver: Some(r), method, args, block_body, .. }
                if block_body.is_empty() =>
            {
                let (r, method, args) = (*r, method.clone(), args.clone());
                // `!expr` — Prism lowers unary not to a receiver-bearing call named
                // `!`. Fold the receiver and invert its Ruby truthiness (this is
                // what turns `read_write? = !read_only?` into `true`).
                if method == "!" && args.is_empty() {
                    let s = self.fold_expr(
                        ast, r, self_qual, self_kind, defs, asts, memo, visiting, closures,
                        depth + 1,
                    )?;
                    return Some(Scalar::Bool(!scalar_truthy(&s)));
                }
                // `Const.method` — an OWN-CLASS singleton project call.
                if args.is_empty() {
                    if let Node::ConstantRead { name, .. } = ast.get(r) {
                        if !name.is_empty() {
                            let owner = name.strip_prefix("::").unwrap_or(name).to_string();
                            return self.resolve_fold_key(
                                &(owner, method, DefKind::Singleton),
                                defs,
                                asts,
                                memo,
                                visiting,
                                closures,
                            );
                        }
                    }
                }
                // A core fold on a value-pinned receiver + args (`1 + 1`, `"x" ==
                // "y"`). Declines unless every part folds.
                let recv = self.fold_expr(
                    ast, r, self_qual, self_kind, defs, asts, memo, visiting, closures, depth + 1,
                )?;
                let mut arg_scalars = Vec::with_capacity(args.len());
                for a in args {
                    arg_scalars.push(self.fold_expr(
                        ast, a, self_qual, self_kind, defs, asts, memo, visiting, closures,
                        depth + 1,
                    )?);
                }
                crate::folding::fold(&recv, &method, &arg_scalars)
            }
            _ => None,
        }
    }

    /// The nearest project ancestor of `qual` (itself first, then its ancestry in
    /// MRO order) that defines instance `method`, or `None`. Mirrors the reference
    /// `resolve_user_def_with_owner`: an unrelated same-name method elsewhere is
    /// never reached, so an implicit-self call resolves ONLY through the enclosing
    /// class's own project chain (the cross-class zero-FP keystone).
    fn resolve_instance_owner(&self, qual: &str, method: &str) -> Option<String> {
        if self.owner_defines(qual, method, DefKind::Instance) {
            return Some(qual.to_string());
        }
        let mut queue: Vec<String> = self.override_ancestor_names(qual);
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(qual.to_string());
        let mut visited = 0usize;
        while !queue.is_empty() {
            let current = queue.remove(0);
            if !seen.insert(current.clone()) {
                continue;
            }
            visited += 1;
            if visited > OVERRIDE_ANCESTOR_WALK_LIMIT {
                return None;
            }
            if self.owner_defines(&current, method, DefKind::Instance) {
                return Some(current);
            }
            for next in self.override_ancestor_names(&current) {
                queue.push(next);
            }
        }
        None
    }

    /// Whether the qualified `owner` has its OWN project `def` of `(method, kind)`.
    fn owner_defines(&self, owner: &str, method: &str, kind: DefKind) -> bool {
        self.definers
            .get(&(method.to_string(), kind))
            .is_some_and(|owners| owners.iter().any(|o| o == owner))
    }

    /// The overridable-method degrade gate (reference `overridden_in_project?`):
    /// true when some project class/module DISTINCT from `owner` redefines
    /// `(method, kind)` AND is RELATED to `owner` (a transitive subclass of an
    /// owner class, or an includer/prepender of an owner module). A same-name
    /// method in an UNRELATED class is not an override — so the two unrelated
    /// `force_pipeline_creation_to_continue?` definers each still fold.
    fn overridden_in_project(
        &self,
        owner: &str,
        method: &str,
        kind: DefKind,
        closures: &mut AncestorClosures,
    ) -> bool {
        let Some(candidates) = self.definers.get(&(method.to_string(), kind)) else {
            return false;
        };
        candidates
            .iter()
            .any(|c| c != owner && self.ancestor_closure(c, closures).contains(owner))
    }

    /// `candidate`'s transitive project-ancestor closure, from the per-merge
    /// [`AncestorClosures`] memo — built on first use, then reused by every later
    /// `(candidate, *)` query. `closure(candidate).contains(owner)` IS the old
    /// per-pair `related_to_owner(candidate, owner)` (#94); the `#[cfg(test)]`
    /// copy of that walk below is the equivalence oracle that pins it.
    fn ancestor_closure<'a>(
        &self,
        candidate: &str,
        closures: &'a mut AncestorClosures,
    ) -> &'a HashSet<String> {
        // `contains_key` + `insert` rather than the `entry` API on purpose: a HIT
        // (12–22× more frequent than a miss) must not pay for a key allocation,
        // and `entry` would force `candidate.to_string()` on every query.
        if !closures.contains_key(candidate) {
            let closure = self.build_ancestor_closure(candidate);
            closures.insert(candidate.to_string(), closure);
        }
        &closures[candidate]
    }

    /// Build `candidate`'s transitive project-ancestor closure: every class name
    /// the pre-#94 per-pair walk could ever have popped off its queue — same MRO
    /// BFS order, same cycle guard, same visited cap — collected instead of
    /// stopping at the first match. So `closure.contains(owner)` answers exactly
    /// what `related_to_owner(candidate, owner)` answered: `candidate` is a
    /// transitive subclass of an owner class, or an includer/prepender of an
    /// owner module.
    ///
    /// ## Cap-boundary fidelity (the one subtle equivalence)
    ///
    /// In the old loop the `current == owner` test ran on POP — BEFORE the
    /// seen-skip AND BEFORE the `visited > OVERRIDE_ANCESTOR_WALK_LIMIT` return.
    /// So the node that OVERFLOWS the cap is still owner-checkable (it can still
    /// answer `true`), while the nodes left behind in the queue never are. This
    /// builder reproduces that boundary exactly: it RECORDS each node at its
    /// first pop, and when the recorded count passes the cap it records the
    /// overflowing node but does NOT expand it and stops — leaving the rest of
    /// the queue out of the closure, exactly as the old walk left them
    /// unreachable. That is also why BFS ORDER is load-bearing here and must stay
    /// byte-identical to the old walk: under the cap, WHICH nodes make it into
    /// the closure depends on the order they were popped in.
    ///
    /// A DUPLICATE pop needs no recording — its first pop already put it in the
    /// set — with ONE exception: `candidate` itself is pre-seeded into `seen`
    /// (the old walk's cycle guard) and so is never recorded by a first pop, yet
    /// a cycle that walks back to it DID make it owner-checkable in the old loop.
    /// That case is recorded explicitly.
    fn build_ancestor_closure(&self, candidate: &str) -> HashSet<String> {
        // `seen` doubles as the closure being built: a node is inserted exactly
        // when the old walk would have owner-checked it for the first time.
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(candidate.to_string());
        let mut candidate_popped = false;
        let mut queue: VecDeque<String> = self.override_ancestor_names(candidate).into();
        let mut visited = 0usize;
        while let Some(current) = queue.pop_front() {
            if seen.contains(&current) {
                if current == candidate {
                    candidate_popped = true;
                }
                continue;
            }
            visited += 1;
            if visited > OVERRIDE_ANCESTOR_WALK_LIMIT {
                // Cap exceeded: this node was popped, so it stays owner-checkable;
                // it is NOT expanded and the queue behind it is abandoned.
                seen.insert(current);
                break;
            }
            // The expansion is moved into the queue and `current` is moved into
            // `seen` — no per-pop `String` re-allocation (the old walk cloned
            // `current` on every pop).
            queue.extend(self.override_ancestor_names(&current));
            seen.insert(current);
        }
        if !candidate_popped {
            // Pre-seeded as the cycle guard, never actually reached ⇒ not part of
            // its own closure.
            seen.remove(candidate);
        }
        seen
    }

    /// The PRE-#94 per-pair walk, verbatim, kept as the equivalence oracle for
    /// [`SourceIndex::build_ancestor_closure`] (the #92 `build_project_legacy`
    /// pattern). Whether `candidate`'s transitive project ancestry reaches
    /// `owner` — i.e. `candidate` is a subclass of an owner class or an includer
    /// of an owner module. Nothing but `probes_s94` calls it.
    #[cfg(test)]
    fn related_to_owner(&self, candidate: &str, owner: &str) -> bool {
        let mut queue: Vec<String> = self.override_ancestor_names(candidate);
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(candidate.to_string());
        let mut visited = 0usize;
        while !queue.is_empty() {
            let current = queue.remove(0);
            if current == owner {
                return true;
            }
            if !seen.insert(current.clone()) {
                continue;
            }
            visited += 1;
            if visited > OVERRIDE_ANCESTOR_WALK_LIMIT {
                return false;
            }
            for next in self.override_ancestor_names(&current) {
                queue.push(next);
            }
        }
        false
    }

    /// Decide whether `class_name` is known to LACK `method`, consulting the
    /// union of source own/inherited methods and — at the RBS boundary — the RBS
    /// ancestor chain, under the conservative completeness gate.
    ///
    /// Returns:
    /// - `true` (method present / chain incomplete ⇒ assume present) when the
    ///   method is found anywhere on the resolvable chain, OR the chain is not
    ///   fully known (some superclass is neither source nor RBS).
    /// - `false` (witnessed absent ⇒ the rule may fire) ONLY when the entire
    ///   chain is known and no member defines the method.
    ///
    /// For a class that is registered but NOT a source class (an RBS-only
    /// instance class like `Pathname`) existence defers entirely to RBS.
    pub fn class_has_method(&self, core: &CoreIndex, class_name: &str, method: &str) -> bool {
        if !self.classes.contains_key(class_name) {
            // Registered RBS-only instance class ⇒ pure RBS resolution.
            if core.knows_class(class_name) {
                return core.class_has_method(class_name, method);
            }
            // Unknown entirely ⇒ assume present (never witness false absence).
            return true;
        }

        // Walk the source chain from `class_name` up. At each step:
        //  - if the source class defines the method directly ⇒ present.
        //  - else follow its superclass: a source super continues the walk; an
        //    RBS-known super defers to RBS; an unknown super ⇒ chain incomplete
        //    ⇒ present (zero-FP keystone).
        let mut current = class_name.to_string();
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            if !seen.insert(current.clone()) {
                return true; // cycle (pathological) ⇒ assume present.
            }
            let Some(entry) = self.classes.get(&current) else {
                return true; // walked off the source map ⇒ assume present.
            };
            if entry.methods.contains(method) {
                return true; // defined directly on this source class.
            }
            match &entry.superclass {
                None => {
                    // Implicit `Object`: defer to RBS over Object's full chain.
                    // RBS `class_has_method` is itself conservative (unknown ⇒
                    // present); witnessing absence here means Object/Kernel/
                    // BasicObject genuinely lack the method.
                    return core.class_has_method("Object", method);
                }
                Some(sup) => {
                    if self.classes.contains_key(sup) {
                        current = sup.clone(); // another source class.
                        continue;
                    }
                    if core.knows_class(sup) {
                        return core.class_has_method(sup, method); // RBS super.
                    }
                    // Neither source nor RBS (e.g. ApplicationRecord) ⇒ INCOMPLETE
                    // ⇒ assume present (the zero-FP keystone for Rails models).
                    return true;
                }
            }
        }
    }
}

/// ADR-0023 tier-4b RETURN inference (the zero-FP minimal slice). For every
/// direct instance method `(class C, method m, body b)` harvested across all
/// `asts`, type `m`'s RETURN (tail) expression under an EMPTY [`TypeEnv`] using a
/// [`Typer`] over `core` + the already-built `idx`, and record `(C, m) -> core
/// class NAME` ONLY when the tail types to a concrete core/RBS class.
///
/// ## Why an EMPTY env is the whole safety argument
///
/// Typing the body under an empty env means any dependence on params / `self` /
/// ivars / branches / OTHER in-source methods naturally yields `Dynamic` (a param
/// read isn't bound, an ivar/self/unknown-constant types Dynamic, an in-source
/// method call resolves to a source Nominal whose core name is `None`), so the
/// concrete-core-class gate declines automatically. The witnessed return set is a
/// strict subset of the reference's body inference.
///
/// ## The gates (any failure ⇒ NO entry; see `check_rules` parity notes)
///
/// 1. Direct instance method — already guaranteed by harvesting (only named,
///    direct `Definition`s are in `method_bodies`; `def self.x` is excluded).
/// 2. Empty/absent body ⇒ decline.
/// 3. `has_explicit_return` (any `return` in the body) ⇒ decline — we read only
///    the tail; an explicit return could carry a different type.
/// 4. The tail is a branch/loop carrier (`If`/`Case`/`Loop`/`Logical`/
///    `BeginRescue`) ⇒ decline — no single concrete return.
/// 5. The tail types (empty env) to anything but a concrete core/RBS class
///    (Dynamic, a source Nominal, or `!knows_class`) ⇒ decline. This single
///    check subsumes param/ivar/self/unknown-constant/in-source-call/
///    non-foldable-call — all already Dynamic under the empty env.
/// 6. Reopen disagreement: the same `(C, m)` inferred twice with DIFFERENT core
///    returns ⇒ remove the entry (decline). Same return twice ⇒ keep.
///
/// ## Pass 3b — call-site PARAMETER BINDING (the param-DEPENDENT companion)
///
/// A method whose tail is a bare positional-PARAM read, or a no-arg core-method
/// CHAIN rooted at one (`def up(x); x.upcase; end`), is param-DEPENDENT, so it
/// yields no entry above (gate 5: a param read is Dynamic under the empty env).
/// We additionally record a [`ParamBoundReturn`] for it so the call site can bind
/// the ARGUMENT's type to the param and re-derive the core return. The extra
/// gates (any failure ⇒ NO param-bound entry, see [`infer_one_param_bound`]):
///   * the method must declare PLAIN POSITIONAL params only (`mb.params ==
///     Some(_)` — splat/post/kwargs/block/optional ⇒ `None` ⇒ decline);
///   * the tail's ROOT receiver must be a bare read of one of those params;
///   * every step of the chain must be a no-arg call (an arg would itself need
///     binding, which we don't model) ⇒ decline otherwise.
///
/// The same gates 2/3/4 (empty body / explicit return / branch tail) and the
/// reopen-disagreement rule apply, tracked independently from the param-
/// independent map.
///
/// ## Which map wins (the two are NOT mutually exclusive)
///
/// Per DEF SITE they are: one tail is either a concrete core class under the
/// empty env or param-rooted, never both, because [`infer_one_param_bound`] is
/// only consulted in the `else` arm below. Per `(class, method)` KEY they are
/// NOT — a method REOPENED across files has two independent sites, each
/// dispatched on its own, so `A#m` can land in both maps at once (issue #92 §8
/// probed exactly that: `def m; "s"; end` in one file, `def m(x); x; end` in
/// another). This is harmless because the call site consults `method_return`
/// FIRST and `param_bound_return` only on a miss (documented at
/// [`SourceIndex::param_bound_returns`]) — but do not lean on an exclusivity
/// that does not hold. The doc claim here used to assert it did.
// type_complexity: the two-map return shape is the real, documented output of this
// pass (param-independent vs param-bound returns); a type alias would only hide it.
#[allow(clippy::type_complexity)]
fn infer_method_returns(
    idx: &SourceIndex,
    core: &CoreIndex,
    asts: &[&LoweredAst],
) -> (
    HashMap<(String, String), String>,
    HashMap<(String, String), ParamBoundReturn>,
) {
    let typer = crate::Typer::with_source(core, idx);
    let empty_env = crate::TypeEnv::new();

    let mut returns: HashMap<(String, String), String> = HashMap::new();
    // Track keys seen with a DISAGREEING reopen so they are never re-added.
    let mut disagreed: HashSet<(String, String)> = HashSet::new();

    // Param-bound (call-site-binding) descriptors, with their own disagreement
    // blacklist (a reopen with a DIFFERENT param-bound shape ⇒ decline).
    let mut param_bound: HashMap<(String, String), ParamBoundReturn> = HashMap::new();
    let mut pb_disagreed: HashSet<(String, String)> = HashSet::new();

    for ast in asts {
        for (_, node) in ast.iter() {
            let (class_name, method_bodies) = match node {
                Node::ClassDef { name, method_bodies, .. } if !name.is_empty() => {
                    (name.as_str(), method_bodies)
                }
                Node::ModuleDef { name, method_bodies, .. } if !name.is_empty() => {
                    (name.as_str(), method_bodies)
                }
                _ => continue,
            };
            for mb in method_bodies {
                let key = (class_name.to_string(), mb.name.clone());
                if let Some(core_name) = infer_one_return(ast, &typer, core, &empty_env, mb) {
                    if disagreed.contains(&key) {
                        continue; // a prior reopen disagreed ⇒ stay declined.
                    }
                    match returns.get(&key) {
                        Some(prev) if prev != &core_name => {
                            // Gate 6: disagreeing reopens ⇒ remove + blacklist.
                            returns.remove(&key);
                            disagreed.insert(key);
                        }
                        _ => {
                            returns.insert(key, core_name);
                        }
                    }
                } else if let Some(pb) = infer_one_param_bound(ast, mb) {
                    // Pass 3b: a param-rooted tail. Same reopen-disagreement rule.
                    if pb_disagreed.contains(&key) {
                        continue;
                    }
                    match param_bound.get(&key) {
                        Some(prev) if prev != &pb => {
                            param_bound.remove(&key);
                            pb_disagreed.insert(key);
                        }
                        _ => {
                            param_bound.insert(key, pb);
                        }
                    }
                }
            }
        }
    }
    (returns, param_bound)
}

/// Run gates 2–5 for one method body and return the inferred CORE class NAME, or
/// `None` to decline. Uses a fresh scratch [`Interner`] per call (the inferred
/// NAME is what we keep; the interned ids are throwaway, re-interned at the call
/// site against the analysis interner).
fn infer_one_return(
    ast: &LoweredAst,
    typer: &crate::Typer<'_>,
    core: &CoreIndex,
    empty_env: &crate::TypeEnv,
    mb: &MethodBody,
) -> Option<String> {
    // Gate 3: any explicit `return` ⇒ decline.
    if mb.has_explicit_return {
        return None;
    }
    // Gate 2: empty/absent body ⇒ decline. The return expression is the LAST
    // direct statement (lowering flattened the Statements wrapper).
    let &ret_id = mb.body.last()?;

    // Gate 4: a branch/loop carrier tail has no single concrete return ⇒ decline.
    if is_branch_carrier(ast.get(ret_id)) {
        return None;
    }

    // Gate 5: type the tail under the EMPTY env; keep ONLY a concrete core/RBS
    // class. A scratch interner is fine — we discard the ids and keep the name.
    let mut scratch = Interner::new();
    let ty = typer.type_of(ast, ret_id, empty_env, &mut scratch);
    let core_name = core.class_name_of(&scratch, ty)?;
    if core.knows_class(core_name) {
        Some(core_name.to_string())
    } else {
        None
    }
}

/// Run the call-site PARAMETER-BINDING gates for one method body and return a
/// [`ParamBoundReturn`] descriptor, or `None` to decline. Called ONLY when the
/// param-independent [`infer_one_return`] already declined (the tail is not a
/// concrete core class under the empty env) — so this never double-records.
///
/// The accepted tail shapes (anything else ⇒ `None`):
///   * a bare positional-param read (`def full(x); x; end`) ⇒
///     `ParamBoundReturn { param_index, chain: [] }`;
///   * a no-arg core-method CHAIN whose ROOT receiver is a bare positional-param
///     read (`def up(x); x.upcase.strip; end`) ⇒ `{ param_index, chain:
///     ["upcase", "strip"] }`.
///
/// Gates (any failure ⇒ `None`; a decline is never a false positive):
///   * `has_explicit_return` ⇒ decline (gate 3 — we read only the tail);
///   * empty body ⇒ decline (gate 2);
///   * `params == None` (splat/post/kwargs/block/optional) ⇒ decline — the
///     call-site positional binder needs a clean 1:1 index mapping;
///   * the tail's root isn't a bare read of a declared positional param ⇒
///     decline (an ivar/self/local-not-a-param/another-param-combination root is
///     not bindable here);
///   * any chain step carries ARGUMENTS ⇒ decline (we bind only the root param;
///     a step arg would itself need binding, which this slice doesn't model);
///   * any chain step carries a BLOCK ⇒ decline (the block-overload return is a
///     separate model; keep this purely the no-arg/no-block core path).
fn infer_one_param_bound(ast: &LoweredAst, mb: &MethodBody) -> Option<ParamBoundReturn> {
    // Gate 3: any explicit `return` ⇒ decline.
    if mb.has_explicit_return {
        return None;
    }
    // Only plain-positional signatures bind (None ⇒ splat/kwargs/etc. ⇒ decline).
    let params = mb.params.as_ref()?;
    // Gate 2: empty/absent body ⇒ decline.
    let &ret_id = mb.body.last()?;

    // Peel the no-arg/no-block core-method chain off the tail, innermost-last:
    // `x.upcase.strip` walks `strip`'s receiver `x.upcase`, then `upcase`'s
    // receiver `x`, collecting method names; the innermost receiver must be a
    // bare param read. We push outer-first then reverse to source (apply) order.
    let mut chain: Vec<String> = Vec::new();
    let mut cursor = ret_id;
    loop {
        match ast.get(cursor) {
            // A bare local read: the chain root. It must name a declared
            // positional param (its index is the binding slot).
            Node::LocalVariableRead { name, .. } => {
                let param_index = params.iter().position(|p| p == name)?;
                chain.reverse(); // collected outer-first ⇒ flip to apply order.
                return Some(ParamBoundReturn { param_index, chain });
            }
            // A call on a receiver: a chain step. It must be a NO-ARG, NO-BLOCK
            // call (an arg/block would need its own binding we don't model).
            Node::Call { receiver: Some(r), method, args, block_body, .. } => {
                if !args.is_empty() || !block_body.is_empty() {
                    return None;
                }
                chain.push(method.clone());
                cursor = *r;
            }
            // Anything else as the root (ivar/self/literal/another carrier) ⇒
            // not a bindable param tail.
            _ => return None,
        }
    }
}

/// ADR-35 slice 1: recursively collect the LEXICALLY-QUALIFIED override classes
/// from `ast`, starting at `node` under the lexical `prefix` (the enclosing
/// class/module name segments). A `ClassDef`/`ModuleDef` appends a
/// [`HarvestedOverrideClass`] keyed by `prefix + name`, then recurses into its
/// body with the extended prefix so a nested class/module is fully qualified.
/// Other nodes recurse over their direct children only enough to reach nested
/// class/module bodies (handled via the explicit body lists below).
///
/// Reads ONE file and accumulates nothing: the first-write-wins semantics for
/// visibilities + superclass and the ordered append-with-dedup for includes are
/// the MERGE's ([`SourceIndex::ingest_override_class`]), replaying `out` in file
/// order. That is why `out` must stay in walk order.
fn collect_override_classes(
    ast: &LoweredAst,
    node: NodeId,
    prefix: &[String],
    out: &mut Vec<HarvestedOverrideClass>,
) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                collect_override_classes(ast, child, prefix, out);
            }
        }
        Node::ClassDef {
            name,
            superclass_path,
            methods,
            method_visibilities,
            includes,
            body,
            ..
        } => {
            if name.is_empty() {
                return;
            }
            let qualified = qualify(prefix, name);
            out.push(HarvestedOverrideClass {
                qualified: qualified.clone(),
                superclass: superclass_path.clone(),
                methods: methods.to_vec(),
                method_visibilities: method_visibilities.to_vec(),
                includes: includes.to_vec(),
            });
            let child_prefix = split_qualified(&qualified);
            for &child in body {
                collect_override_classes(ast, child, &child_prefix, out);
            }
        }
        Node::ModuleDef { name, methods, method_visibilities, includes, body, .. } => {
            if name.is_empty() {
                return;
            }
            let qualified = qualify(prefix, name);
            out.push(HarvestedOverrideClass {
                qualified: qualified.clone(),
                superclass: None,
                methods: methods.to_vec(),
                method_visibilities: method_visibilities.to_vec(),
                includes: includes.to_vec(),
            });
            let child_prefix = split_qualified(&qualified);
            for &child in body {
                collect_override_classes(ast, child, &child_prefix, out);
            }
        }
        // Any other node: a nested class/module only appears as a DIRECT body
        // statement of a class/module/program (mirroring the reference's
        // `record_def_visibility`/qualification, which only qualifies through
        // class/module bodies). We deliberately do NOT descend into method
        // bodies / control flow — a def-nested class is out of slice-1 scope.
        _ => {}
    }
}

/// ADR-35 slice 1: join a lexical `prefix` and a (possibly already-namespaced)
/// declaration `name` into a fully-qualified name. A `name` that is itself a
/// path (`Foo::Bar` declared inside `Outer`) qualifies to `Outer::Foo::Bar`,
/// matching Ruby's lexical constant resolution for the declaration head.
fn qualify(prefix: &[String], name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{}::{}", prefix.join("::"), name)
    }
}

/// Every name a `Definition` node is callable under: its instance name, or the
/// method name of a `def self.x` / `def Recv.x` singleton. Empty for anything
/// else (a `class << X` body).
fn def_names(node: &Node) -> Vec<String> {
    match node {
        Node::Definition { name, singleton_name, receiver_def_name, .. } => {
            [name, singleton_name, receiver_def_name].into_iter().flatten().cloned().collect()
        }
        _ => Vec::new(),
    }
}

/// Split a qualified name into its segment vector (`"A::B" -> ["A", "B"]`), used
/// as the child lexical prefix when recursing into a class/module body.
fn split_qualified(qualified: &str) -> Vec<String> {
    qualified.split("::").map(|s| s.to_string()).collect()
}

/// C1: the per-file lexical class/module SCOPES — each `(span, qualified segment
/// vector)` — so a `ConstantRead`'s use-site lexical prefix can be recovered by
/// span containment (the innermost enclosing scope). Mirrors the qualification
/// walk of [`collect_override_classes`]; computed once per analyzed
/// file and threaded into the [`Typer`] so its `ConstantRead` arm can consult
/// [`SourceIndex::constant_shadowed`] with the correct lexical prefix.
///
/// [`Typer`]: crate::Typer
pub fn lexical_scopes(ast: &LoweredAst) -> Vec<(rigor_parse::Span, Vec<String>)> {
    let mut out = Vec::new();
    collect_lexical_scopes(ast, ast.root(), &[], &mut out);
    out
}

/// The span of every METHOD body in the file (`def x` / `def self.x`), excluding
/// `class << X` bodies (which are class scopes, not method scopes).
///
/// A Ruby method body is an independent LOCAL scope: it never sees the enclosing
/// file's locals. Prism already encodes that for a bare name — `s` inside a `def`
/// lowers to a CALL, not a `LocalVariableRead`, when the def does not bind `s` —
/// so the only reads that survive into a body are its own parameters and writes.
/// The flat top-level env is keyed by NAME alone, though, so a parameter that
/// happens to share a name with a top-level local (`s = 'a'` at file scope,
/// `def go(s)` below it) used to read the top-level local's TYPE. That is what
/// produced the `wrong-arity`/`undefined-method` FPs on rigor-survey
/// `Ruby/data_structures/hash_table/anagram_checker.rb`.
///
/// Callers use these spans to withhold the top-level env from a use site inside
/// a method body. Span-containment (not a structural walk) is orphan-proof — the
/// same discipline as [`lexical_scopes`] and the dead-assignment collector.
pub fn method_body_spans(ast: &LoweredAst) -> Vec<rigor_parse::Span> {
    ast.iter()
        .filter_map(|(_, n)| match n {
            Node::Definition { is_singleton_class: false, span, .. } => Some(*span),
            _ => None,
        })
        .collect()
}

fn collect_lexical_scopes(
    ast: &LoweredAst,
    node: NodeId,
    prefix: &[String],
    out: &mut Vec<(rigor_parse::Span, Vec<String>)>,
) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                collect_lexical_scopes(ast, child, prefix, out);
            }
        }
        Node::ClassDef { name, body, span, .. } | Node::ModuleDef { name, body, span, .. } => {
            if name.is_empty() {
                return; // un-namable (dynamic constant / `class << self`) ⇒ skip.
            }
            let qualified = qualify(prefix, name);
            let segs = split_qualified(&qualified);
            out.push((*span, segs.clone()));
            for &child in body {
                collect_lexical_scopes(ast, child, &segs, out);
            }
        }
        _ => {}
    }
}

/// C5: the static scalar key a hash-key NODE denotes, or `None` when dynamic.
/// Mirrors the Typer's `static_shape_key_of_node` (the reference's
/// `HashShape::ALLOWED_KEY_CLASSES`) so a harvested hash pins the same slots.
fn const_shape_key_of(node: &Node) -> Option<ShapeKey> {
    match node {
        Node::SymbolLit { value, .. } => Some(ShapeKey::Sym(value.clone())),
        Node::StringLit { value, .. } => Some(ShapeKey::Str(value.clone())),
        Node::IntegerLit { value, .. } => Some(ShapeKey::Int(*value)),
        Node::FloatLit { value, .. } => Some(ShapeKey::Float(value.to_bits())),
        Node::TrueLit { .. } => Some(ShapeKey::Bool(true)),
        Node::FalseLit { .. } => Some(ShapeKey::Bool(false)),
        Node::NilLit { .. } => Some(ShapeKey::Nil),
        _ => None,
    }
}

/// C5a: recursively collect ONE FILE's lexically-qualified `CONST = <literal>`
/// writes from `ast` under lexical `prefix`, in walk order. The first write of a
/// qualified name appends `(qualified, defining namespace, harvested value,
/// writes: 1)` to `out`; every repeat bumps that entry's `writes` (and never
/// re-harvests the value — the FIRST write wins, as before). Only
/// class/module/program BODIES are walked (a def-nested constant is out of
/// scope), mirroring the C1 override / fold discovery inclusion rule.
///
/// `seen` maps a qualified name to its position in `out` — a per-file scratch
/// map the caller owns so the recursion stays cheap.
///
/// The project-wide single-assignment gate is NOT here: [`SourceIndex::merge`]
/// sums the counts across files (Σ ≥ 2 ⇒ declined) and takes the first file's
/// value, which is exactly what one shared `first`/`multi` pair used to do.
fn collect_literal_constants(
    ast: &LoweredAst,
    node: NodeId,
    prefix: &[String],
    out: &mut Vec<HarvestedConstWrite>,
    seen: &mut HashMap<String, usize>,
) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                collect_literal_constants(ast, child, prefix, out, seen);
            }
        }
        Node::ClassDef { name, body, .. } | Node::ModuleDef { name, body, .. } => {
            if name.is_empty() {
                return;
            }
            let child_prefix = split_qualified(&qualify(prefix, name));
            for &child in body {
                collect_literal_constants(ast, child, &child_prefix, out, seen);
            }
        }
        Node::ConstantWrite { name, value, .. } => {
            let qualified = qualify(prefix, name);
            match seen.get(&qualified) {
                Some(&at) => out[at].writes += 1,
                None => {
                    seen.insert(qualified.clone(), out.len());
                    out.push(HarvestedConstWrite {
                        qualified,
                        namespace: prefix.to_vec(),
                        lit: const_lit_of(ast, *value),
                        writes: 1,
                    });
                }
            }
        }
        _ => {}
    }
}

/// C5: harvest a `ConstLit` from a constant's RHS `node`, or `None` when the RHS
/// is not FULLY literal (declining the whole constant). Recurses into array /
/// hash elements — any non-literal element declines the entire structure (a
/// splat / dynamic key / non-literal value ⇒ `None`), so a recorded value is
/// always exactly the carrier the Typer builds for the same inline literal.
fn const_lit_of(ast: &LoweredAst, node: NodeId) -> Option<ConstLit> {
    match ast.get(node) {
        Node::IntegerLit { value, .. } => Some(ConstLit::Scalar(Scalar::Int(*value))),
        Node::FloatLit { value, .. } => Some(ConstLit::Scalar(Scalar::Float(*value))),
        Node::StringLit { value, .. } => Some(ConstLit::Scalar(Scalar::Str(value.clone()))),
        Node::SymbolLit { value, .. } => Some(ConstLit::Scalar(Scalar::Sym(value.clone()))),
        Node::TrueLit { .. } => Some(ConstLit::Scalar(Scalar::Bool(true))),
        Node::FalseLit { .. } => Some(ConstLit::Scalar(Scalar::Bool(false))),
        Node::NilLit { .. } => Some(ConstLit::Scalar(Scalar::Nil)),
        Node::ArrayLit { elements, .. } => {
            let mut elems = Vec::with_capacity(elements.len());
            for &e in elements {
                // Slice B: one non-literal element no longer declines the whole
                // constant — it degrades to the projection-inert bare nominal.
                // The reference never declines here either (it types the hole
                // and keeps a Tuple), so this is a spelling of the SAME
                // constant, one precision tier lower.
                match const_lit_of(ast, e) {
                    Some(l) => elems.push(l),
                    None => return Some(ConstLit::BareArray),
                }
            }
            Some(ConstLit::Tuple(elems))
        }
        Node::HashLit { elements, all_assoc, .. } => {
            if !*all_assoc {
                // A `**` splat / non-assoc element. The reference degrades to a
                // widened `Hash[K, V]` (probes p3b/p3b2), never declines.
                return Some(ConstLit::BareHash);
            }
            let mut members: Vec<(ShapeKey, ConstLit)> = Vec::with_capacity(elements.len() / 2);
            let mut i = 0;
            while i + 1 < elements.len() {
                // A dynamic key (probe p3c) or a non-literal value (p1, the
                // lambda-hash shape) degrades the container, not the constant.
                let Some(key) = const_shape_key_of(ast.get(elements[i])) else {
                    return Some(ConstLit::BareHash);
                };
                let Some(value) = const_lit_of(ast, elements[i + 1]) else {
                    return Some(ConstLit::BareHash);
                };
                // Last-wins on a duplicate key (mirrors `hash_shape_or_hash`).
                if let Some(m) = members.iter_mut().find(|m| m.0 == key) {
                    m.1 = value;
                } else {
                    members.push((key, value));
                }
                i += 2;
            }
            Some(ConstLit::Hash(members))
        }
        Node::Range { .. } => Some(ConstLit::Range),
        // `.freeze` is identity on the literal (M2-GO slice 1): the ubiquitous
        // `CONST = %w[...].freeze` / `{...}.freeze` spelling (RuboCop's
        // Style/MutableConstant autocorrect) harvests as the literal underneath.
        // Zero-arg, block-free `freeze` only; recursion makes nested
        // `["a".freeze].freeze` work at any depth. The reference folds the same
        // way (probed: `A = %w[a b].freeze; A.exclude?("c")` fires there).
        Node::Call { receiver: Some(r), method, args, block_body, .. }
            if method == "freeze" && args.is_empty() && block_body.is_empty() =>
        {
            const_lit_of(ast, *r)
        }
        _ => None,
    }
}

/// Ruby truthiness of a folded scalar: only `nil` / `false` are falsey.
fn scalar_truthy(s: &Scalar) -> bool {
    !matches!(s, Scalar::Nil | Scalar::Bool(false))
}

/// ADR-0038 — harvest ONE FILE's project instance + singleton `def` bodies by
/// QUALIFIED owner name (the same lexical walk `collect_override_classes` uses,
/// so `module Gitlab; module Database` keys `Gitlab::Database`), appending each
/// site (tail node + explicit-return flag) to `out` in walk order. Only DIRECT
/// `def` children of a class/module body are harvested — a def nested in a
/// conditional / inner method is out of scope, matching the tier-4b / override
/// discovery inclusion rule.
///
/// `tail` is a [`NodeId`] in THIS file's AST; the merge pairs it with the file's
/// slice position to form a [`FoldSite`].
fn walk_fold_defs(
    ast: &LoweredAst,
    node: NodeId,
    prefix: &[String],
    out: &mut Vec<HarvestedFoldDef>,
) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                walk_fold_defs(ast, child, prefix, out);
            }
        }
        Node::ClassDef { name, body, .. } | Node::ModuleDef { name, body, .. } => {
            if name.is_empty() {
                return;
            }
            let qualified = qualify(prefix, name);
            for &child in body {
                if let Node::Definition {
                    name,
                    singleton_name,
                    body: def_body,
                    has_explicit_return,
                    ..
                } = ast.get(child)
                {
                    let entry = match (name, singleton_name) {
                        (Some(m), _) => Some((m.clone(), DefKind::Instance)),
                        (None, Some(m)) => Some((m.clone(), DefKind::Singleton)),
                        _ => None,
                    };
                    if let Some((method, kind)) = entry {
                        if let Some(&tail) = def_body.last() {
                            out.push(HarvestedFoldDef {
                                owner: qualified.clone(),
                                method,
                                kind,
                                tail,
                                has_explicit_return: *has_explicit_return,
                            });
                        }
                    }
                }
            }
            let child_prefix = split_qualified(&qualified);
            for &child in body {
                walk_fold_defs(ast, child, &child_prefix, out);
            }
        }
        _ => {}
    }
}

/// ADR-0038 — invert the merged def sites into the `(method, kind) -> [qualified
/// owners]` definers index that drives the overridable degrade and implicit-self
/// resolution. Every read of the owner `Vec` is an `.any(…)` (`owner_defines`,
/// `overridden_in_project`), so its order is not semantic — which is just as
/// well: it comes from `HashMap` key iteration and is already unstable between
/// processes on the same input (issue #92 §3.4).
fn invert_definers(
    defs: &HashMap<(String, String, DefKind), Vec<FoldSite>>,
) -> HashMap<(String, DefKind), Vec<String>> {
    let mut definers: HashMap<(String, DefKind), Vec<String>> = HashMap::new();
    for (owner, method, kind) in defs.keys() {
        let owners = definers.entry((method.clone(), *kind)).or_default();
        if !owners.contains(owner) {
            owners.push(owner.clone());
        }
    }
    definers
}

/// Whether a tail node is a branch/loop carrier whose type is not a single
/// concrete class (gate 4). `BeginRescue` also covers a lowered parenthesized
/// expression and an inline `rescue` body — both decline conservatively.
fn is_branch_carrier(node: &Node) -> bool {
    matches!(
        node,
        Node::If { .. }
            | Node::Case { .. }
            | Node::When { .. }
            | Node::Loop { .. }
            | Node::Logical { .. }
            | Node::BeginRescue { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rigor_parse::{lower, parse};

    fn lower_src(src: &[u8]) -> LoweredAst {
        lower(&parse(src))
    }

    /// Build a PROJECT index over one source string.
    fn build_one(src: &[u8], core: &CoreIndex) -> (LoweredAst, SourceIndex) {
        let ast = lower_src(src);
        let idx = SourceIndex::build(&ast, core);
        (ast, idx)
    }

    // --- tier-4b positive: tail types to a concrete core class ---------------

    #[test]
    fn infers_interpolation_return_as_string() {
        // `def full_name; "#{first} #{last}"; end` — the tail is an interpolated
        // String, which always types String ⇒ ("User","full_name") -> "String".
        let core = CoreIndex::new();
        let (_ast, idx) = build_one(
            b"class User\n  def full_name\n    \"#{first} #{last}\"\n  end\nend\n",
            &core,
        );
        assert_eq!(idx.method_return("User", "full_name"), Some("String"));
    }

    #[test]
    fn infers_integer_and_array_literal_returns() {
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class C\n  def n\n    42\n  end\n  def a\n    [1, 2]\n  end\nend\n", &core);
        assert_eq!(idx.method_return("C", "n"), Some("Integer"));
        assert_eq!(idx.method_return("C", "a"), Some("Array"));
    }

    #[test]
    fn infers_core_call_tail_return() {
        // `def shout; "x".upcase; end` — `"x".upcase` folds to a String constant,
        // whose class is String ⇒ "String".
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class C\n  def shout\n    \"x\".upcase\n  end\nend\n", &core);
        assert_eq!(idx.method_return("C", "shout"), Some("String"));
    }

    #[test]
    fn infers_cross_file_return() {
        // A class defined in ast[0] is inferred even though it is `.new`'d in
        // ast[1]; the return map is keyed by NAME, so it is cross-file safe.
        let core = CoreIndex::new();
        let a0 = lower_src(b"class User\n  def full_name\n    \"#{a} #{b}\"\n  end\nend\n");
        let a1 = lower_src(b"u = User.new\nu.full_name.lenght\n");
        let idx = SourceIndex::build_project(&[&a0, &a1], &core);
        assert_eq!(idx.method_return("User", "full_name"), Some("String"));
    }

    // --- tier-4b negative: no entry under the gates --------------------------

    #[test]
    fn param_dependent_body_declines() {
        // `def n(x); x; end` — `x` is an unbound param ⇒ Dynamic ⇒ no entry.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class C\n  def n(x)\n    x\n  end\nend\n", &core);
        assert_eq!(idx.method_return("C", "n"), None);
    }

    #[test]
    fn ivar_body_declines() {
        // `def name; @name; end` — an ivar read types Dynamic ⇒ no entry.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class C\n  def name\n    @name\n  end\nend\n", &core);
        assert_eq!(idx.method_return("C", "name"), None);
    }

    #[test]
    fn explicit_return_declines() {
        // Any explicit `return` ⇒ decline even if the tail would type.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"class C\n  def m\n    return \"e\" if x\n    \"ok\"\n  end\nend\n",
            &core,
        );
        assert_eq!(idx.method_return("C", "m"), None);
    }

    #[test]
    fn conditional_tail_declines() {
        // The tail is an `if` expression (branch carrier) ⇒ decline.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"class C\n  def m\n    if x\n      \"a\"\n    else\n      \"b\"\n    end\n  end\nend\n",
            &core,
        );
        assert_eq!(idx.method_return("C", "m"), None);
    }

    #[test]
    fn in_source_method_call_tail_declines() {
        // `def wrapper; other; end` calling another in-source (implicit-self)
        // method ⇒ Dynamic under the empty env ⇒ decline.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"class C\n  def other\n    \"x\"\n  end\n  def wrapper\n    other\n  end\nend\n",
            &core,
        );
        assert_eq!(idx.method_return("C", "wrapper"), None);
    }

    #[test]
    fn disagreeing_reopened_defs_decline() {
        // `class C; def m; "s"; end; end` reopened with `def m; 1; end` —
        // String vs Integer disagree ⇒ the entry is removed (decline).
        let core = CoreIndex::new();
        let a0 = lower_src(b"class C\n  def m\n    \"s\"\n  end\nend\n");
        let a1 = lower_src(b"class C\n  def m\n    1\n  end\nend\n");
        let idx = SourceIndex::build_project(&[&a0, &a1], &core);
        assert_eq!(idx.method_return("C", "m"), None);
    }

    #[test]
    fn agreeing_reopened_defs_keep() {
        // Same return twice ⇒ keep.
        let core = CoreIndex::new();
        let a0 = lower_src(b"class C\n  def m\n    \"s\"\n  end\nend\n");
        let a1 = lower_src(b"class C\n  def m\n    \"t\"\n  end\nend\n");
        let idx = SourceIndex::build_project(&[&a0, &a1], &core);
        assert_eq!(idx.method_return("C", "m"), Some("String"));
    }

    // --- tier-4b call-site PARAMETER BINDING descriptors ---------------------

    #[test]
    fn passthrough_param_records_bound_return() {
        // `def full(x); x; end` — the tail is a bare read of positional param 0,
        // so it records a param-bound descriptor (index 0, empty chain) and NO
        // param-independent return (the param is Dynamic under the empty env).
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class C\n  def full(x)\n    x\n  end\nend\n", &core);
        assert_eq!(idx.method_return("C", "full"), None);
        assert_eq!(
            idx.param_bound_return("C", "full"),
            Some(&ParamBoundReturn { param_index: 0, chain: vec![] })
        );
    }

    #[test]
    fn second_param_records_correct_index() {
        // `def pick(a, b); b; end` — the tail reads the SECOND positional param,
        // so the descriptor binds index 1.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class C\n  def pick(a, b)\n    b\n  end\nend\n", &core);
        assert_eq!(
            idx.param_bound_return("C", "pick"),
            Some(&ParamBoundReturn { param_index: 1, chain: vec![] })
        );
    }

    #[test]
    fn core_transform_param_records_chain() {
        // `def up(x); x.upcase.strip; end` — a no-arg core chain rooted at param
        // 0 records `{ index: 0, chain: ["upcase", "strip"] }` (apply order).
        let core = CoreIndex::new();
        let (_a, idx) =
            build_one(b"class C\n  def up(x)\n    x.upcase.strip\n  end\nend\n", &core);
        assert_eq!(
            idx.param_bound_return("C", "up"),
            Some(&ParamBoundReturn {
                param_index: 0,
                chain: vec!["upcase".into(), "strip".into()]
            })
        );
    }

    #[test]
    fn splat_param_declines_binding() {
        // `def f(*xs); xs; end` — a splat breaks the positional index map ⇒ no
        // param-bound entry (and `xs` is param-rooted, so no independent entry).
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class C\n  def f(*xs)\n    xs\n  end\nend\n", &core);
        assert_eq!(idx.param_bound_return("C", "f"), None);
        assert_eq!(idx.method_return("C", "f"), None);
    }

    #[test]
    fn kwarg_param_declines_binding() {
        // `def f(x, k:); x; end` — a keyword param ⇒ decline (params == None).
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class C\n  def f(x, k:)\n    x\n  end\nend\n", &core);
        assert_eq!(idx.param_bound_return("C", "f"), None);
    }

    #[test]
    fn default_param_declines_binding() {
        // `def f(x = 1); x; end` — an optional (defaulted) param ⇒ decline.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class C\n  def f(x = 1)\n    x\n  end\nend\n", &core);
        assert_eq!(idx.param_bound_return("C", "f"), None);
    }

    #[test]
    fn block_param_declines_binding() {
        // `def f(x, &blk); x; end` — a block param ⇒ decline.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class C\n  def f(x, &blk)\n    x\n  end\nend\n", &core);
        assert_eq!(idx.param_bound_return("C", "f"), None);
    }

    #[test]
    fn chain_with_args_declines_binding() {
        // `def f(x); x.fetch(0); end` — a chain step that carries an argument is
        // not a no-arg core call ⇒ decline (we bind only the root param).
        let core = CoreIndex::new();
        let (_a, idx) =
            build_one(b"class C\n  def f(x)\n    x.fetch(0)\n  end\nend\n", &core);
        assert_eq!(idx.param_bound_return("C", "f"), None);
    }

    #[test]
    fn non_param_root_tail_declines_binding() {
        // `def f(x); @y.upcase; end` — the chain root is an ivar, not a param ⇒
        // no param-bound entry.
        let core = CoreIndex::new();
        let (_a, idx) =
            build_one(b"class C\n  def f(x)\n    @y.upcase\n  end\nend\n", &core);
        assert_eq!(idx.param_bound_return("C", "f"), None);
    }

    #[test]
    fn explicit_return_declines_param_binding() {
        // An explicit `return` ⇒ decline even for a param-rooted tail.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"class C\n  def f(x)\n    return x if x\n    x\n  end\nend\n",
            &core,
        );
        assert_eq!(idx.param_bound_return("C", "f"), None);
    }

    #[test]
    fn disagreeing_reopened_param_bound_declines() {
        // `def m(x); x; end` reopened with `def m(a, b); b; end` — index 0 vs 1
        // disagree ⇒ the param-bound entry is removed.
        let core = CoreIndex::new();
        let a0 = lower_src(b"class C\n  def m(x)\n    x\n  end\nend\n");
        let a1 = lower_src(b"class C\n  def m(a, b)\n    b\n  end\nend\n");
        let idx = SourceIndex::build_project(&[&a0, &a1], &core);
        assert_eq!(idx.param_bound_return("C", "m"), None);
    }

    // --- ADR-35 slice 1: override-visibility ancestor walk -------------------

    #[test]
    fn method_visibility_reads_own_table() {
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"class C\n  def a\n  end\n  private\n  def b\n  end\nend\n",
            &core,
        );
        assert_eq!(idx.method_visibility("C", "a"), Some(Visibility::Public));
        assert_eq!(idx.method_visibility("C", "b"), Some(Visibility::Private));
        assert_eq!(idx.method_visibility("C", "missing"), None);
    }

    #[test]
    fn nearest_ancestor_walks_superclass() {
        // B < A; A defines `foo` (public). The nearest ancestor of B defining
        // `foo` is A with Public.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"class A\n  def foo\n  end\nend\nclass B < A\n  private\n  def foo\n  end\nend\n",
            &core,
        );
        assert_eq!(
            idx.nearest_ancestor_defining("B", "foo"),
            Some(("A".to_string(), Some(Visibility::Public)))
        );
    }

    #[test]
    fn nearest_ancestor_prefers_included_module_over_superclass() {
        // B includes M and is < A; both define `foo`. MRO ⇒ the included module
        // M is the nearest ancestor.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"module M\n  def foo\n  end\nend\nclass A\n  def foo\n  end\nend\nclass B < A\n  include M\n  def bar\n  end\nend\n",
            &core,
        );
        assert_eq!(
            idx.nearest_ancestor_defining("B", "foo"),
            Some(("M".to_string(), Some(Visibility::Public)))
        );
    }

    #[test]
    fn nearest_ancestor_none_when_no_project_ancestor_defines() {
        // B < A but A does not define `foo` ⇒ no defining ancestor.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"class A\n  def other\n  end\nend\nclass B < A\n  def foo\n  end\nend\n",
            &core,
        );
        assert_eq!(idx.nearest_ancestor_defining("B", "foo"), None);
    }

    #[test]
    fn nearest_ancestor_skips_rbs_third_party_super() {
        // `class B < ApplicationRecord` — the super is not a project source class
        // ⇒ dropped ⇒ no defining ancestor (RBS-ancestor carve-out).
        let core = CoreIndex::new();
        let (_a, idx) =
            build_one(b"class B < ApplicationRecord\n  private\n  def foo\n  end\nend\n", &core);
        assert_eq!(idx.nearest_ancestor_defining("B", "foo"), None);
    }

    #[test]
    fn nearest_ancestor_returns_unknown_visibility_for_methods_only_entry() {
        // The keystone path: an ancestor that DEFINES the method (in `methods`)
        // but has NO visibility-table entry returns `(ancestor, None)` — the rule
        // layer must NOT synthesize Public from this. We construct a methods-only
        // entry directly (the public lowering keeps the two tables in lockstep, so
        // this exercises the data path that the "never synthesize Public" gate
        // guards against).
        let core = CoreIndex::new();
        let mut idx = SourceIndex::build(&lower_src(b"class B < A\n  def foo\n  end\nend\n"), &core);
        // Seed override class `A` with `foo` in `methods` only (no vis entry).
        idx.override_classes.insert(
            "A".to_string(),
            OverrideClass {
                superclass: None,
                includes: Vec::new(),
                method_visibilities: HashMap::new(),
                methods: ["foo".to_string()].into_iter().collect(),
            },
        );
        assert_eq!(
            idx.nearest_ancestor_defining("B", "foo"),
            Some(("A".to_string(), None))
        );
    }

    #[test]
    fn nearest_ancestor_does_not_merge_namespace_collisions() {
        // The gitlab-foss FP root cause: a controller includes `Groups::Params`
        // (which defines `group_params`, not `group`), while a DIFFERENT
        // `IssuableFinder::Params` defines a private `group`. With lexical
        // qualification the include resolves to `Groups::Params` ONLY, so `group`
        // has no project ancestor here ⇒ None (no phantom override).
        let core = CoreIndex::new();
        let groups_params = lower_src(
            b"module Groups\n  module Params\n    def group_params\n    end\n  end\nend\n",
        );
        let finder_params = lower_src(
            b"module IssuableFinder\n  module Params\n    private\n    def group\n    end\n  end\nend\n",
        );
        let controller = lower_src(
            b"module Organizations\n  class GroupsController\n    include Groups::Params\n    private\n    def group\n    end\n  end\nend\n",
        );
        let idx = SourceIndex::build_project(
            &[&groups_params, &finder_params, &controller],
            &core,
        );
        // The controller's `group` has NO project ancestor defining it (the
        // included `Groups::Params` lacks `group`; `IssuableFinder::Params` is not
        // an ancestor) ⇒ silent. This is the precise zero-FP guarantee.
        assert_eq!(
            idx.nearest_ancestor_defining("Organizations::GroupsController", "group"),
            None
        );
    }

    #[test]
    fn nearest_ancestor_resolves_namespaced_include_path() {
        // `include Groups::Params` from a class in a different namespace resolves
        // to the fully-qualified `Groups::Params` (which DOES define the method).
        let core = CoreIndex::new();
        let m = lower_src(b"module Groups\n  module Params\n    def gp\n    end\n  end\nend\n");
        let c = lower_src(
            b"module Organizations\n  class Ctrl\n    include Groups::Params\n    private\n    def gp\n    end\n  end\nend\n",
        );
        let idx = SourceIndex::build_project(&[&m, &c], &core);
        assert_eq!(
            idx.nearest_ancestor_defining("Organizations::Ctrl", "gp"),
            Some(("Groups::Params".to_string(), Some(Visibility::Public)))
        );
    }

    #[test]
    fn nearest_ancestor_cross_file_via_build_project() {
        // Parent A in file 0, subclass B in file 1 — the project build seeds both,
        // so the walk resolves A across files.
        let core = CoreIndex::new();
        let a0 = lower_src(b"class A\n  def foo\n  end\nend\n");
        let a1 = lower_src(b"class B < A\n  private\n  def foo\n  end\nend\n");
        let idx = SourceIndex::build_project(&[&a0, &a1], &core);
        assert_eq!(
            idx.nearest_ancestor_defining("B", "foo"),
            Some(("A".to_string(), Some(Visibility::Public)))
        );
    }

    #[test]
    fn nearest_ancestor_cycle_guarded() {
        // A < B and B < A (pathological cycle) — the walk terminates (None, no
        // panic/loop) when neither defines the method.
        let core = CoreIndex::new();
        let a0 = lower_src(b"class A < B\n  def x\n  end\nend\n");
        let a1 = lower_src(b"class B < A\n  def y\n  end\nend\n");
        let idx = SourceIndex::build_project(&[&a0, &a1], &core);
        assert_eq!(idx.nearest_ancestor_defining("A", "foo"), None);
    }

    #[test]
    fn class_name_for_id_of_recovers_source_name() {
        // A `Nominal` over a source-range id resolves to its class NAME (the
        // companion to the core `class_name_of`, which returns None for it).
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class Point\n  def x\n    1\n  end\nend\n", &core);
        let mut i = Interner::new();
        let class = idx.class_id("Point").expect("Point registered");
        let ty = i.intern(rigor_types::Type::Nominal { class, args: vec![] });
        assert_eq!(idx.class_name_for_id_of(&i, ty), Some("Point"));
        // A Dynamic carrier ⇒ None.
        let u = i.untyped();
        assert_eq!(idx.class_name_for_id_of(&i, u), None);
    }

    // --- ADR-0038 interprocedural literal-tail fold ---------------------------

    /// Build a PROJECT index over N source strings.
    fn build_many(srcs: &[&[u8]], core: &CoreIndex) -> SourceIndex {
        let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower_src(s)).collect();
        let refs: Vec<&LoweredAst> = asts.iter().collect();
        SourceIndex::build_project(&refs, core)
    }

    #[test]
    fn const_singleton_bare_literal_folds() {
        // `module M; def self.ro?; false; end; end` ⇒ `M.ro?` folds to false.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"module M\n  def self.ro?\n    false\n  end\nend\n", &core);
        assert_eq!(idx.const_singleton_literal("M", "ro?"), Some(Scalar::Bool(false)));
    }

    #[test]
    fn const_singleton_class_receiver_folds() {
        // A CLASS (not just a module) singleton call folds too.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class K\n  def self.on?\n    true\n  end\nend\n", &core);
        assert_eq!(idx.const_singleton_literal("K", "on?"), Some(Scalar::Bool(true)));
    }

    #[test]
    fn qualified_const_receiver_folds_stripping_leading_colons() {
        // `module Gitlab; module Database; def self.read_only?; false` keys the
        // fold at the QUALIFIED owner `Gitlab::Database`, matched by the dotted
        // receiver (with or without a leading `::`).
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"module Gitlab\n  module Database\n    def self.read_only?\n      false\n    end\n  end\nend\n",
            &core,
        );
        assert_eq!(
            idx.const_singleton_literal("Gitlab::Database", "read_only?"),
            Some(Scalar::Bool(false))
        );
        assert_eq!(
            idx.const_singleton_literal("::Gitlab::Database", "read_only?"),
            Some(Scalar::Bool(false))
        );
    }

    #[test]
    fn depth_two_bang_of_singleton_call_folds() {
        // `read_write? = !read_only?` — the tail `!read_only?` resolves the
        // OWN-CLASS singleton `read_only?` (false) and inverts it ⇒ true.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"module Gitlab\n  module Database\n    def self.read_only?\n      false\n    end\n    def self.read_write?\n      !read_only?\n    end\n  end\nend\n",
            &core,
        );
        assert_eq!(
            idx.const_singleton_literal("Gitlab::Database", "read_write?"),
            Some(Scalar::Bool(true))
        );
    }

    #[test]
    fn cross_owner_const_call_declines() {
        // `Bar` defines `read_only?`; `Foo` does not. A `Foo.read_only?` fold must
        // DECLINE (own-class resolution — a same-name method elsewhere is never
        // adopted), even though `read_only?` has exactly one project definer.
        let core = CoreIndex::new();
        let idx = build_many(
            &[
                b"class Foo\nend\n",
                b"module Bar\n  def self.read_only?\n    false\n  end\nend\n",
            ],
            &core,
        );
        assert_eq!(idx.const_singleton_literal("Foo", "read_only?"), None);
        assert_eq!(idx.const_singleton_literal("Bar", "read_only?"), Some(Scalar::Bool(false)));
    }

    #[test]
    fn implicit_self_same_class_instance_folds() {
        // `def flag; false; end` resolves an implicit-self `flag` in the SAME
        // class to false.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class Widget\n  def flag\n    false\n  end\nend\n", &core);
        assert_eq!(
            idx.implicit_self_literal("Widget", DefKind::Instance, "flag"),
            Some(Scalar::Bool(false))
        );
    }

    #[test]
    fn implicit_self_inherited_instance_folds() {
        // `class User < Base; Base defines flag` — an implicit-self `flag` in User
        // resolves through the ancestry to Base#flag.
        let core = CoreIndex::new();
        let idx = build_many(
            &[
                b"class Base\n  def flag\n    false\n  end\nend\n",
                b"class User < Base\nend\n",
            ],
            &core,
        );
        assert_eq!(
            idx.implicit_self_literal("User", DefKind::Instance, "flag"),
            Some(Scalar::Bool(false))
        );
    }

    #[test]
    fn implicit_self_included_module_folds() {
        // `class User; include Flaggable; Flaggable defines flag` — resolves
        // through the included module.
        let core = CoreIndex::new();
        let idx = build_many(
            &[
                b"module Flaggable\n  def flag\n    false\n  end\nend\n",
                b"class User\n  include Flaggable\nend\n",
            ],
            &core,
        );
        assert_eq!(
            idx.implicit_self_literal("User", DefKind::Instance, "flag"),
            Some(Scalar::Bool(false))
        );
    }

    #[test]
    fn implicit_self_cross_class_declines() {
        // `Widget` defines `flag`; `User` (unrelated) calls it implicitly. Even
        // with a single project definer, the fold DECLINES — `flag` is not in
        // User's ancestry (the cross-class zero-FP keystone).
        let core = CoreIndex::new();
        let idx = build_many(
            &[
                b"class Widget\n  def flag\n    false\n  end\nend\n",
                b"class User\nend\n",
            ],
            &core,
        );
        assert_eq!(idx.implicit_self_literal("User", DefKind::Instance, "flag"), None);
    }

    #[test]
    fn implicit_self_singleton_kind_folds_own_class() {
        // Inside a `def self.check`, an implicit `read_only?` resolves the OWN
        // singleton table.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"module Gitlab\n  module Database\n    def self.read_only?\n      false\n    end\n  end\nend\n",
            &core,
        );
        assert_eq!(
            idx.implicit_self_literal("Gitlab::Database", DefKind::Singleton, "read_only?"),
            Some(Scalar::Bool(false))
        );
        // The instance table is SEPARATE — no instance `read_only?` exists.
        assert_eq!(
            idx.implicit_self_literal("Gitlab::Database", DefKind::Instance, "read_only?"),
            None
        );
    }

    #[test]
    fn related_subclass_override_degrades_even_when_values_match() {
        // Base#flag = false, Sub < Base overrides flag = false (MATCHING value).
        // The base's literal is the DEFAULT, not what every receiver sees, so it
        // degrades to no-fold (reference `degrade_if_overridable`).
        let core = CoreIndex::new();
        let idx = build_many(
            &[
                b"class Base\n  def flag\n    false\n  end\nend\n",
                b"class Sub < Base\n  def flag\n    false\n  end\nend\n",
            ],
            &core,
        );
        assert_eq!(idx.implicit_self_literal("Base", DefKind::Instance, "flag"), None);
    }

    #[test]
    fn two_unrelated_definers_each_fold() {
        // A and B are UNRELATED modules that each define a singleton `ro? = false`.
        // Neither is an override of the other, so each still folds (the recall the
        // single-definer guard would have lost — the `force_pipeline_creation_to_
        // continue?` pair).
        let core = CoreIndex::new();
        let idx = build_many(
            &[
                b"module A\n  def self.ro?\n    false\n  end\nend\n",
                b"module B\n  def self.ro?\n    false\n  end\nend\n",
            ],
            &core,
        );
        assert_eq!(idx.const_singleton_literal("A", "ro?"), Some(Scalar::Bool(false)));
        assert_eq!(idx.const_singleton_literal("B", "ro?"), Some(Scalar::Bool(false)));
    }

    #[test]
    fn subclass_constant_singleton_declines() {
        // `Sub < Base`, only Base defines singleton `ro?`. A `Sub.ro?` call is an
        // INHERITED singleton — resolution is own-class only, so it declines
        // (reference probe 9: inherited singleton via subclass constant declines).
        let core = CoreIndex::new();
        let idx = build_many(
            &[
                b"class Base\n  def self.ro?\n    false\n  end\nend\n",
                b"class Sub < Base\nend\n",
            ],
            &core,
        );
        assert_eq!(idx.const_singleton_literal("Sub", "ro?"), None);
        assert_eq!(idx.const_singleton_literal("Base", "ro?"), Some(Scalar::Bool(false)));
    }

    #[test]
    fn union_branch_tail_declines() {
        // A method whose tail is an `if`/ternary carrier never folds (a branch
        // carrier has no single scalar leaf in this slice).
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"module M\n  def self.ro?\n    cond ? true : nil\n  end\nend\n",
            &core,
        );
        assert_eq!(idx.const_singleton_literal("M", "ro?"), None);
    }

    #[test]
    fn dynamic_leaf_declines() {
        // A non-literal tail (an unresolved call) declines.
        let core = CoreIndex::new();
        let (_a, idx) =
            build_one(b"module M\n  def self.ro?\n    some_dynamic_thing\n  end\nend\n", &core);
        assert_eq!(idx.const_singleton_literal("M", "ro?"), None);
    }

    #[test]
    fn shape_return_declines() {
        // An array/hash literal tail is not a scalar ⇒ decline.
        let core = CoreIndex::new();
        let (_a, idx) =
            build_one(b"module M\n  def self.ro?\n    [1, 2]\n  end\nend\n", &core);
        assert_eq!(idx.const_singleton_literal("M", "ro?"), None);
    }

    #[test]
    fn explicit_return_declines_fold() {
        // Any explicit `return` in the body declines (we read only the tail).
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"module M\n  def self.ro?\n    return true if x\n    false\n  end\nend\n",
            &core,
        );
        assert_eq!(idx.const_singleton_literal("M", "ro?"), None);
    }

    #[test]
    fn disagreeing_reopen_declines_fold() {
        // The same singleton method reopened with a DIFFERENT literal declines.
        let core = CoreIndex::new();
        let idx = build_many(
            &[
                b"module M\n  def self.ro?\n    false\n  end\nend\n",
                b"module M\n  def self.ro?\n    true\n  end\nend\n",
            ],
            &core,
        );
        assert_eq!(idx.const_singleton_literal("M", "ro?"), None);
    }

    #[test]
    fn recursive_method_declines_fold() {
        // A self-recursive body (`def loopy; loopy; end`) declines via the cycle
        // guard rather than spinning.
        let core = CoreIndex::new();
        let (_a, idx) =
            build_one(b"module M\n  def self.loopy\n    loopy\n  end\nend\n", &core);
        assert_eq!(idx.const_singleton_literal("M", "loopy"), None);
    }

    #[test]
    fn raise_guarded_tail_folds() {
        // A raise-guarded earlier statement leaves the tail literal foldable.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"module M\n  def self.ro?\n    raise \"boom\" if never\n    false\n  end\nend\n",
            &core,
        );
        assert_eq!(idx.const_singleton_literal("M", "ro?"), Some(Scalar::Bool(false)));
    }

    // --- C1: constant-shadow gate --------------------------------------------

    fn seg(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn toplevel_definition_shadows_everywhere() {
        // A toplevel `class Report` suppresses a bare `Report` read at ANY use
        // site (Ruby: a toplevel constant is always reachable).
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class Report\nend\n", &core);
        assert!(idx.constant_shadowed("Report", &[]));
        assert!(idx.constant_shadowed("Report", &seg(&["Foo", "Bar"])));
    }

    #[test]
    fn nested_definition_shadows_only_where_lexically_visible() {
        // `module A; module B; module Time; end; end; end` — a bare `Time` read
        // is shadowed inside `A::B::*` but RELAXES (fires) elsewhere.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"module A\n  module B\n    module Time\n    end\n    class C\n    end\n  end\nend\n",
            &core,
        );
        // Visible: the defining namespace and any scope nested within it.
        assert!(idx.constant_shadowed("Time", &seg(&["A", "B"])));
        assert!(idx.constant_shadowed("Time", &seg(&["A", "B", "C"])));
        // NOT visible: a sibling namespace, an outer scope, or the toplevel.
        assert!(!idx.constant_shadowed("Time", &seg(&["A"])));
        assert!(!idx.constant_shadowed("Time", &seg(&["A", "Z"])));
        assert!(!idx.constant_shadowed("Time", &[]));
        // A different bare name the project never defines is never shadowed.
        assert!(!idx.constant_shadowed("Time", &seg(&["Other"])));
    }

    #[test]
    fn harvests_single_literal_constant_lexically() {
        // `class K; R = 1..1024; A = [:a]; N = 42; end` — each is harvested and
        // visible from within `K`, not from an unrelated scope.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"class K\n  A = [1, 2]\n  N = 42\n  S = \"hi\"\nend\n",
            &core,
        );
        // Visible inside `K`.
        assert_eq!(
            idx.literal_constant("N", &seg(&["K"]), _a.file_id()),
            Some(&ConstLit::Scalar(Scalar::Int(42)))
        );
        assert!(matches!(idx.literal_constant("A", &seg(&["K"]), _a.file_id()), Some(ConstLit::Tuple(_))));
        assert_eq!(
            idx.literal_constant("S", &seg(&["K"]), _a.file_id()),
            Some(&ConstLit::Scalar(Scalar::Str("hi".into())))
        );
        // NOT visible from an unrelated namespace or the toplevel.
        assert_eq!(idx.literal_constant("N", &[], _a.file_id()), None);
        assert_eq!(idx.literal_constant("N", &seg(&["Other"]), _a.file_id()), None);
    }

    #[test]
    fn cross_namespace_constant_not_folded() {
        // `module Expirable; DAYS = 7; end` — `DAYS` is NOT visible from an
        // unrelated `class Consumer` (the app/models concern-constant FP shape).
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"module Expirable\n  DAYS = 7\nend\nclass Consumer\n  D2 = 9\nend\n",
            &core,
        );
        // Visible only within its own namespace.
        assert_eq!(
            idx.literal_constant("DAYS", &seg(&["Expirable"]), _a.file_id()),
            Some(&ConstLit::Scalar(Scalar::Int(7)))
        );
        assert_eq!(idx.literal_constant("DAYS", &seg(&["Consumer"]), _a.file_id()), None);
        assert_eq!(idx.literal_constant("DAYS", &[], _a.file_id()), None);
    }

    #[test]
    fn multiple_assignment_declines_harvest() {
        // A constant written twice (same qualified name) is ambiguous ⇒ declined.
        let core = CoreIndex::new();
        let (_a, idx) =
            build_one(b"class K\n  M = 1\n  M = 2\nend\n", &core);
        assert_eq!(idx.literal_constant("M", &seg(&["K"]), _a.file_id()), None);
    }

    #[test]
    fn class_name_collision_declines_harvest() {
        // `Widget = [1]` where `class Widget` also exists ⇒ declined (a constant
        // is never a class; the class/source path owns that name).
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"class Widget\nend\nWidget = [1]\n",
            &core,
        );
        assert_eq!(idx.literal_constant("Widget", &[], _a.file_id()), None);
    }

    #[test]
    fn range_constant_harvests_as_range() {
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class K\n  R = 1..1024\nend\n", &core);
        assert_eq!(idx.literal_constant("R", &seg(&["K"]), _a.file_id()), Some(&ConstLit::Range));
    }

    // --- slice B: partially-literal containers ⇒ INERT bare nominals ----------

    #[test]
    fn partially_literal_containers_harvest_as_bare_nominals() {
        let core = CoreIndex::new();
        let (a, idx) = build_one(
            b"class K\n  LAM = { c: ->(_x) { 1 } }.freeze\n  DYN = [1, unknown_zzz, 2].freeze\n  \
              SPLAT_H = { a: 1, **unknown_zzz }.freeze\n  DYNKEY = { a: 1, unknown_zzz => 2 }.freeze\n  \
              SPLAT_A = [*unknown_zzz].freeze\n  INTERP = [\"a\", \"b#{1}\"].freeze\n  \
              CHAIN = [1, \"x\".upcase].freeze\nend\n",
            &core,
        );
        let k = seg(&["K"]);
        for name in ["LAM", "SPLAT_H", "DYNKEY"] {
            assert_eq!(
                idx.literal_constant(name, &k, a.file_id()),
                Some(&ConstLit::BareHash),
                "{name} should harvest as a bare Hash nominal"
            );
        }
        for name in ["DYN", "SPLAT_A", "INTERP", "CHAIN"] {
            assert_eq!(
                idx.literal_constant(name, &k, a.file_id()),
                Some(&ConstLit::BareArray),
                "{name} should harvest as a bare Array nominal"
            );
        }
    }

    #[test]
    fn fully_literal_harvest_is_unchanged_by_the_widening() {
        // The value-pinned rendering must not regress: a fully-literal container
        // still harvests as Tuple/Hash, and the EMPTY literals keep their
        // zero-size shapes (`[]` / `{}` fold projections off them).
        let core = CoreIndex::new();
        let (a, idx) = build_one(
            b"class K\n  T = [1, 2].freeze\n  H = { a: 1 }.freeze\n  E = [].freeze\n  \
              EH = {}.freeze\n  R = 1..9\nend\n",
            &core,
        );
        let k = seg(&["K"]);
        assert!(matches!(idx.literal_constant("T", &k, a.file_id()), Some(ConstLit::Tuple(v)) if v.len() == 2));
        assert!(matches!(idx.literal_constant("H", &k, a.file_id()), Some(ConstLit::Hash(m)) if m.len() == 1));
        assert_eq!(idx.literal_constant("E", &k, a.file_id()), Some(&ConstLit::Tuple(vec![])));
        assert_eq!(idx.literal_constant("EH", &k, a.file_id()), Some(&ConstLit::Hash(vec![])));
        assert_eq!(idx.literal_constant("R", &k, a.file_id()), Some(&ConstLit::Range));
    }

    #[test]
    fn a_nested_partial_container_degrades_only_the_inner_level() {
        // `{ a: [->(){}] }` keeps its OUTER shape (the key is static) and puts a
        // bare `Array` in the slot — the reference has `{ a: [Proc] }` there, so
        // `H[:a].zzz` fires in both engines (same class, sharper rendering in
        // the oracle). Verified against the oracle in fixture 92.
        let core = CoreIndex::new();
        let (a, idx) = build_one(b"class K\n  N = { a: [->() { 1 }] }.freeze\nend\n", &core);
        let Some(ConstLit::Hash(members)) = idx.literal_constant("N", &seg(&["K"]), a.file_id())
        else {
            panic!("expected an outer Hash shape");
        };
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].1, ConstLit::BareArray);
    }

    #[test]
    fn non_container_rhs_still_declines() {
        // Slice B widens the CONTAINER arms only. A call chain, a bare constant
        // read, a lambda and a `Class.new` still decline (a chain-valued
        // constant is slice C's question, assessed separately).
        let core = CoreIndex::new();
        let (a, idx) = build_one(
            b"class K\n  CH = %w[a b].map.with_index.to_h.freeze\n  CR = OTHER_ZZZ\n  \
              L = ->(_x) { 1 }\n  C = Class.new(StandardError)\nend\n",
            &core,
        );
        let k = seg(&["K"]);
        for name in ["CH", "CR", "L", "C"] {
            assert_eq!(idx.literal_constant(name, &k, a.file_id()), None, "{name}");
        }
    }

    #[test]
    fn multiple_assignment_still_declines_a_partial_container() {
        // The reference UNIONS duplicate assignments; C5's single-assignment
        // gate is a strict under-emit and slice B keeps it unchanged.
        let core = CoreIndex::new();
        let (a, idx) = build_one(
            b"class K\n  M = { c: ->(_x) { 1 } }\n  M = [1, 2]\nend\n",
            &core,
        );
        assert_eq!(idx.literal_constant("M", &seg(&["K"]), a.file_id()), None);
    }

    // --- slice A: PER-FILE constant-value consumption -------------------------

    #[test]
    fn constant_value_is_consumed_only_in_the_assigning_file() {
        // The reference rebuilds its in-source constant-value table per file
        // (`ScopeIndexer#build_in_source_constants` walks ONE file's root), so a
        // same-namespace read in another file resolves nothing there. Probed:
        // `module M; class C; L = [1, 2].freeze` in `a.rb`, `L.frobnicate_zzz`
        // inside the same `M::C` in `b.rb` — reference silent, rigor-rs fired.
        let core = CoreIndex::new();
        let a0 = lower_src(b"module M\n  class C\n    L = [1, 2].freeze\n  end\nend\n");
        let a1 = lower_src(b"module M\n  class C\n    def go; L; end\n  end\nend\n");
        let idx = SourceIndex::build_project(&[&a0, &a1], &core);
        // Same file as the assignment: still folds (the harvest is unchanged).
        assert!(matches!(
            idx.literal_constant("L", &seg(&["M", "C"]), a0.file_id()),
            Some(ConstLit::Tuple(_))
        ));
        // The USE file did not assign it ⇒ no value, even though the namespace
        // matches exactly.
        assert_eq!(idx.literal_constant("L", &seg(&["M", "C"]), a1.file_id()), None);
    }

    #[test]
    fn qualified_constant_value_is_consumed_only_in_the_assigning_file() {
        // Stage 2e is a pure SPELLING of the same harvest, so it inherits the
        // per-file gate. `M::C::L` read from inside `M::C` in another file.
        let core = CoreIndex::new();
        let a0 = lower_src(b"module M\n  class C\n    L = [1, 2].freeze\n  end\nend\n");
        let a1 = lower_src(b"module M\n  class C\n    def go; M::C::L; end\n  end\nend\n");
        let idx = SourceIndex::build_project(&[&a0, &a1], &core);
        assert!(matches!(
            idx.qualified_literal_constant("M::C::L", &seg(&["M", "C"]), a0.file_id()),
            Some(ConstLit::Tuple(_))
        ));
        assert_eq!(
            idx.qualified_literal_constant("M::C::L", &seg(&["M", "C"]), a1.file_id()),
            None
        );
    }

    #[test]
    fn toplevel_constant_value_is_still_per_file() {
        // Probed with a `require_relative` in place: the reference is silent on
        // BOTH a container and a scalar read cross-file, so the gate is about
        // the FILE, not about namespace visibility or require reachability.
        let core = CoreIndex::new();
        let a0 = lower_src(b"TOPL = [1, 2].freeze\nSCAL = 5\n");
        let a1 = lower_src(b"require_relative \"a\"\nclass K\n  def go; TOPL; SCAL; end\nend\n");
        let idx = SourceIndex::build_project(&[&a0, &a1], &core);
        assert!(idx.literal_constant("TOPL", &seg(&["K"]), a0.file_id()).is_some());
        assert!(idx.literal_constant("SCAL", &seg(&["K"]), a0.file_id()).is_some());
        assert_eq!(idx.literal_constant("TOPL", &seg(&["K"]), a1.file_id()), None);
        assert_eq!(idx.literal_constant("SCAL", &seg(&["K"]), a1.file_id()), None);
    }

    #[test]
    fn env_negative_check_stays_file_agnostic() {
        // Non-goal guard: slice A must not let the stage-2b `ENV` arm start
        // firing where it used to decline. Its decline predicate is
        // `literal_constant_visible_any_file`, which ignores the use file — a
        // project `ENV = { a: 1 }` in ANOTHER file still declines the arm.
        let core = CoreIndex::new();
        let a0 = lower_src(b"ENV = { a: 1 }.freeze\n");
        let a1 = lower_src(b"class K\n  def go; ENV; end\nend\n");
        let idx = SourceIndex::build_project(&[&a0, &a1], &core);
        // The per-file TYPING gate declines in the reading file …
        assert_eq!(idx.literal_constant("ENV", &seg(&["K"]), a1.file_id()), None);
        // … but the 2b decline predicate still sees it from either file.
        assert!(idx.literal_constant_visible_any_file("ENV", &seg(&["K"])));
        assert!(idx.literal_constant_visible_any_file("ENV", &[]));
        // And the coarser project-write set (the arm's other decline) is
        // unaffected by slice A.
        assert!(idx.project_writes_constant("ENV"));
    }

    #[test]
    fn lexical_scopes_records_qualified_spans() {
        // The per-file lexical scope table qualifies nested class/module bodies
        // so a use-site prefix can be recovered by span containment.
        let ast = lower_src(
            b"module A\n  module B\n    class C\n    end\n  end\nend\n",
        );
        let scopes = lexical_scopes(&ast);
        let quals: Vec<Vec<String>> = scopes.iter().map(|(_, q)| q.clone()).collect();
        assert!(quals.contains(&seg(&["A"])));
        assert!(quals.contains(&seg(&["A", "B"])));
        assert!(quals.contains(&seg(&["A", "B", "C"])));
        // Innermost scope has the narrowest span (nested last).
        assert_eq!(scopes.len(), 3);
    }

    #[test]
    fn method_body_spans_are_method_defs_only() {
        // Every `def` body (instance and singleton) contributes a span; a
        // `class << self` body does not (it is a CLASS scope, not a method one).
        let ast = lower_src(
            b"class K\n  def a; 1; end\n  def self.b; 2; end\n  class << self\n    def c; 3; end\n  end\nend\n",
        );
        // `a`, `self.b`, and the `c` inside `class << self` = 3 method defs; the
        // singleton-class body itself is excluded.
        assert_eq!(method_body_spans(&ast).len(), 3);
        assert!(method_body_spans(&lower_src(b"x = 1\n")).is_empty());
    }

    #[test]
    fn project_declares_method_sees_nested_defs() {
        // A reopened CORE class contributes methods RBS cannot know about, and a
        // def nested in a block or a conditional counts — the reference's
        // `Scope#discovered_method?` is keyed by qualified class over every def
        // in the body, not just its direct children.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"class String\n  def direct; 1; end\n  [1].each do\n    def in_block; 2; end\n  end\n  if true\n    def in_if; 3; end\n  end\nend\n",
            &core,
        );
        assert!(idx.project_declares_method("String", "direct"));
        assert!(idx.project_declares_method("String", "in_block"));
        assert!(idx.project_declares_method("String", "in_if"));
        assert!(!idx.project_declares_method("String", "never_defined"));
        // Keyed by the QUALIFIED name — a nested class does not leak outward.
        let (_b, idx2) = build_one(
            b"module Outer\n  class Inner\n    def only_here; 1; end\n  end\nend\n",
            &core,
        );
        assert!(idx2.project_declares_method("Outer::Inner", "only_here"));
        assert!(!idx2.project_declares_method("Outer", "only_here"));
        assert!(!idx2.project_declares_method("Inner", "only_here"));
    }

    #[test]
    fn mutated_params_are_position_aware() {
        // `def m(x, a); a << 1; end` records index 1 ONLY: passing a local at
        // position 0 must not widen it (probed against the oracle).
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"def m(x, a)\n  a << 1\nend\n", &core);
        assert!(idx.method_mutates_param("m", 1));
        assert!(!idx.method_mutates_param("m", 0));
        // A mutator on a DIFFERENT local records nothing.
        let (_b, idx2) = build_one(b"def n(a)\n  b = []\n  b << 1\n  a.size\nend\n", &core);
        assert!(!idx2.method_mutates_param("n", 0));
        // A non-mutating call on the param records nothing.
        let (_c, idx3) = build_one(b"def p(a)\n  a.size\nend\n", &core);
        assert!(!idx3.method_mutates_param("p", 0));
        // An unknown method name is never mutating.
        assert!(!idx.method_mutates_param("totally_unknown", 0));
    }

    #[test]
    fn toplevel_defs_include_receiver_bearing_defs() {
        // The reference keys a def with an EMPTY lexical prefix under its
        // `<toplevel>` table unless the receiver is `self`, so `def IO.foo`
        // resolves a later bare `foo` and `def self.bar` does not.
        let core = CoreIndex::new();
        let (_a, idx) = build_one(
            b"def IO.recv_def; 1; end\ndef self.self_def; 2; end\ndef plain_def; 3; end\n",
            &core,
        );
        assert!(idx.is_toplevel_def("recv_def"));
        assert!(idx.is_toplevel_def("plain_def"));
        assert!(!idx.is_toplevel_def("self_def"));
        // Inside a class body the lexical prefix is non-empty ⇒ not toplevel.
        let (_b, idx2) = build_one(b"class K\n  def self.klass_singleton; 1; end\nend\n", &core);
        assert!(!idx2.is_toplevel_def("klass_singleton"));
    }
}

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
mod probes_s92 {
    use super::*;
    use rigor_parse::{lower, parse};

    /// A per-FIELD rendering of the whole index. `sorted = true` canonicalises
    /// every collection (the SEMANTIC content); `sorted = false` renders the
    /// `Vec` fields in their built order (exposing ORDER leakage).
    fn fingerprint(idx: &SourceIndex, sorted: bool) -> Vec<(&'static str, String)> {
        fn sort_join(mut v: Vec<String>) -> String {
            v.sort();
            v.join(" | ")
        }
        let mut out: Vec<(&'static str, String)> = Vec::new();

        out.push((
            "classes",
            sort_join(
                idx.classes
                    .iter()
                    .map(|(k, c)| {
                        let mut ms: Vec<&str> = c.methods.iter().map(|s| s.as_str()).collect();
                        ms.sort();
                        format!("{k}<{:?}>{{{}}}", c.superclass, ms.join(","))
                    })
                    .collect(),
            ),
        ));
        // `names` IS the ClassId assignment order — never sorted for the
        // ordered fingerprint.
        out.push((
            "names",
            if sorted { sort_join(idx.names.clone()) } else { idx.names.join(" | ") },
        ));
        // `name_to_id` is the reverse half of the registry bijection. Rendered
        // name-sorted with the ID INCLUDED, so it pins the id assignment itself
        // in both modes (the 17th field; the probe module shipped 16).
        out.push((
            "name_to_id",
            sort_join(idx.name_to_id.iter().map(|(n, i)| format!("{n}={i}")).collect()),
        ));
        out.push((
            "declaration_only_classes",
            sort_join(idx.declaration_only_classes.iter().cloned().collect()),
        ));
        out.push((
            "method_returns",
            sort_join(
                idx.method_returns.iter().map(|((c, m), r)| format!("{c}#{m}->{r}")).collect(),
            ),
        ));
        out.push((
            "param_bound_returns",
            sort_join(
                idx.param_bound_returns
                    .iter()
                    .map(|((c, m), p)| format!("{c}#{m}->{p:?}"))
                    .collect(),
            ),
        ));
        out.push((
            "override_classes",
            sort_join(
                idx.override_classes
                    .iter()
                    .map(|(k, c)| {
                        let mut ms: Vec<&str> = c.methods.iter().map(|s| s.as_str()).collect();
                        ms.sort();
                        let mut vis: Vec<String> = c
                            .method_visibilities
                            .iter()
                            .map(|(m, v)| format!("{m}={v:?}"))
                            .collect();
                        vis.sort();
                        // `includes` is ORDER-BEARING (MRO): keep source order
                        // in the ordered fingerprint.
                        let inc = if sorted {
                            sort_join(c.includes.clone())
                        } else {
                            c.includes.join(",")
                        };
                        format!(
                            "{k}<{:?}>inc[{inc}]m{{{}}}vis{{{}}}",
                            c.superclass,
                            ms.join(","),
                            vis.join(",")
                        )
                    })
                    .collect(),
            ),
        ));
        out.push(("toplevel_defs", sort_join(idx.toplevel_defs.iter().cloned().collect())));
        out.push((
            "literal_returns",
            sort_join(
                idx.literal_returns
                    .iter()
                    .map(|((o, m, k), s)| format!("{o}.{m}/{k:?}->{s:?}"))
                    .collect(),
            ),
        ));
        out.push((
            "definers",
            sort_join(
                idx.definers
                    .iter()
                    .map(|((m, k), owners)| {
                        let o = if sorted { sort_join(owners.clone()) } else { owners.join(",") };
                        format!("{m}/{k:?}->[{o}]")
                    })
                    .collect(),
            ),
        ));
        out.push((
            "toplevel_constants",
            sort_join(idx.toplevel_constants.iter().cloned().collect()),
        ));
        out.push((
            "literal_constants",
            sort_join(
                idx.literal_constants
                    .iter()
                    .map(|(k, v)| {
                        let mut es: Vec<String> = v
                            .iter()
                            .map(|(ns, f, l)| format!("{}#{f}={l:?}", ns.join("::")))
                            .collect();
                        if sorted {
                            es.sort();
                        }
                        format!("{k}->[{}]", es.join(","))
                    })
                    .collect(),
            ),
        ));
        out.push((
            "qualified_literal_constants",
            sort_join(
                idx.qualified_literal_constants
                    .iter()
                    .map(|(k, (ns, f, l))| format!("{k}->{}#{f}={l:?}", ns.join("::")))
                    .collect(),
            ),
        ));
        out.push((
            "project_constant_write_names",
            sort_join(idx.project_constant_write_names.iter().cloned().collect()),
        ));
        out.push((
            "nested_constant_namespaces",
            sort_join(
                idx.nested_constant_namespaces
                    .iter()
                    .map(|(k, v)| {
                        let mut nss: Vec<String> = v.iter().map(|ns| ns.join("::")).collect();
                        if sorted {
                            nss.sort();
                        }
                        format!("{k}->[{}]", nss.join(","))
                    })
                    .collect(),
            ),
        ));
        out.push((
            "discovered_methods",
            sort_join(
                idx.discovered_methods
                    .iter()
                    .map(|(k, v)| {
                        let mut ms: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
                        ms.sort();
                        format!("{k}->{{{}}}", ms.join(","))
                    })
                    .collect(),
            ),
        ));
        out.push((
            "mutated_params",
            sort_join(
                idx.mutated_params
                    .iter()
                    .map(|(k, v)| {
                        let mut ix: Vec<usize> = v.iter().copied().collect();
                        ix.sort_unstable();
                        format!("{k}->{ix:?}")
                    })
                    .collect(),
            ),
        ));
        out
    }

    /// Permute the AST REFERENCES, never re-`lower()`: `file_id` comes from a
    /// process-global counter, so re-lowering would inject a spurious diff into
    /// the `literal_constants` / `qualified_literal_constants` fingerprints.
    fn build_perm(
        asts: &[LoweredAst],
        perm: &[usize],
        core: &CoreIndex,
    ) -> Vec<(&'static str, String)> {
        fingerprint(&SourceIndex::build_project(&perm_refs(asts, perm), core), false)
    }

    fn perm_refs<'a>(asts: &'a [LoweredAst], perm: &[usize]) -> Vec<&'a LoweredAst> {
        perm.iter().map(|&i| &asts[i]).collect()
    }

    fn diff(
        a: &[(&'static str, String)],
        b: &[(&'static str, String)],
    ) -> Vec<(&'static str, String, String)> {
        a.iter()
            .zip(b.iter())
            .filter(|((_, x), (_, y))| x != y)
            .map(|((k, x), (_, y))| (*k, x.clone(), y.clone()))
            .collect()
    }

    /// PROBE 1 — permutation sensitivity, field by field. Prints every field
    /// whose built value depends on the ORDER of the `asts` slice, and PINS the
    /// finding: file order may move only the registry ids and the override
    /// index. Anything else moving means the merge acquired an order dependence
    /// the passes never had.
    #[test]
    fn probe_permutation_field_diff() {
        let core = CoreIndex::new();
        let srcs = probe1_sources();
        let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
        let base = build_perm(&asts, &[0, 1, 2, 3], &core);
        let perms: [[usize; 4]; 5] =
            [[1, 0, 2, 3], [3, 2, 1, 0], [2, 3, 0, 1], [0, 2, 1, 3], [3, 0, 1, 2]];
        for p in perms {
            let other = build_perm(&asts, &p, &core);
            let d = diff(&base, &other);
            if !d.is_empty() {
                println!("--- permutation {p:?}: {} field(s) differ", d.len());
                for (field, x, y) in &d {
                    println!("  [{field}]\n    base : {x}\n    perm : {y}");
                }
            } else {
                println!("--- permutation {p:?}: identical");
            }
            // The CANONICAL content: only the two order-bearing fields may move.
            // (`names` sorted is the same SET; it is `name_to_id` that carries
            // the assignment.)
            let canonical = diff(
                &fingerprint(&SourceIndex::build_project(&perm_refs(&asts, &[0, 1, 2, 3]), &core), true),
                &fingerprint(&SourceIndex::build_project(&perm_refs(&asts, &p), &core), true),
            );
            for (field, _, _) in canonical {
                assert!(
                    matches!(field, "name_to_id" | "override_classes"),
                    "permutation {p:?} moved `{field}`, which is not order-bearing"
                );
            }
        }
    }

    /// PROBE 2 — incremental equality: build over ALL files vs ALL-BUT-ONE, and
    /// report which fields change in a way a per-file harvest of the dropped
    /// file could NOT have reconstructed on its own (i.e. file X's recorded
    /// contribution depends on file Y's content).
    #[test]
    fn probe_drop_one_field_diff() {
        let core = CoreIndex::new();
        let srcs: Vec<&[u8]> = vec![
            // f0: the "other" file — reopens, conflicting constant.
            b"class Base\n  def m\n    1\n  end\nend\nSHARED = 1\nmodule Wrap\n  DUP = 1\nend\n",
            // f1: the file under test — its OWN contribution depends on f0.
            b"class Base\n  private\n  def m\n    2\n  end\nend\nmodule Wrap\n  DUP = 2\nend\nclass Sub < Base\n  private\n  def m\n    3\n  end\nend\nSOLO = 7\n",
            // f2: an unrelated third file.
            b"class Solo\n  def q\n    1\n  end\nend\n",
        ];
        let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
        for drop in 0..asts.len() {
            let all: Vec<&LoweredAst> = asts.iter().collect();
            let kept: Vec<&LoweredAst> =
                asts.iter().enumerate().filter(|(i, _)| *i != drop).map(|(_, a)| a).collect();
            let fa = fingerprint(&SourceIndex::build_project(&all, &core), false);
            let fk = fingerprint(&SourceIndex::build_project(&kept, &core), false);
            println!("--- dropping f{drop}");
            for (field, x, y) in diff(&fa, &fk) {
                println!("  [{field}]\n    all  : {x}\n    kept : {y}");
            }
        }
    }

    /// PROBE 3 — is `build_project` over N files equal to the UNION of N
    /// single-file `build_project`s for the "obviously additive" fields? Any
    /// field where it is NOT is a cross-file computation.
    #[test]
    fn probe_union_of_singletons_vs_project() {
        let core = CoreIndex::new();
        let srcs: Vec<&[u8]> = vec![
            b"class A\n  def x\n    \"s\"\n  end\nend\nC1 = 1\n",
            b"class B < A\n  def y\n    A.new\n  end\nend\nC1 = 2\nC2 = 3\n",
            b"class C\n  def z\n    C2\n  end\nend\n",
        ];
        let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
        let all: Vec<&LoweredAst> = asts.iter().collect();
        let project = fingerprint(&SourceIndex::build_project(&all, &core), true);
        let singles: Vec<Vec<(&'static str, String)>> = asts
            .iter()
            .map(|a| fingerprint(&SourceIndex::build_project(&[a], &core), true))
            .collect();
        for (i, (field, joined)) in project.iter().enumerate() {
            let parts: Vec<String> =
                singles.iter().map(|s| s[i].1.clone()).filter(|s| !s.is_empty()).collect();
            println!("[{field}]\n  project : {joined}\n  singles : {}", parts.join("  ||  "));
        }
    }

    // PROBES 4, 5 and 8 were PRINTING probes of the Pass-3 / Pass-4b / both-maps
    // couplings. They are promoted, above, into the asserting tests
    // `coupling_pass3_reads_the_merged_constant_table`,
    // `coupling_pass4b_degrade_is_cross_file` and
    // `method_can_appear_in_both_return_maps` — same inputs, same finding, now a
    // gate instead of a printout.

    /// PROBE 6 — cross-PROCESS instability of the Vec-valued maps. Prints the
    /// built order of `definers` / `literal_constants` /
    /// `nested_constant_namespaces` for a FIXED file order; running the test
    /// twice and diffing the output shows whether the order is a function of
    /// the input at all.
    #[test]
    fn probe_vec_order_stability_across_processes() {
        let core = CoreIndex::new();
        let srcs: Vec<&[u8]> = vec![
            b"module Alpha\n  KEY = 1\n  class Time\n  end\n  def shared; 1; end\nend\n",
            b"module Beta\n  KEY = 2\n  class Time\n  end\n  def shared; 2; end\nend\n",
            b"module Gamma\n  KEY = 3\n  class Time\n  end\n  def shared; 3; end\nend\n",
            b"module Delta\n  KEY = 4\n  class Time\n  end\n  def shared; 4; end\nend\n",
        ];
        let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
        let refs: Vec<&LoweredAst> = asts.iter().collect();
        let idx = SourceIndex::build_project(&refs, &core);
        println!(
            "definers[shared/Instance] = {:?}",
            idx.definers.get(&("shared".to_string(), DefKind::Instance))
        );
        println!(
            "literal_constants[KEY] namespaces = {:?}",
            idx.literal_constants
                .get("KEY")
                .map(|v| v.iter().map(|(ns, _, _)| ns.join("::")).collect::<Vec<_>>())
        );
        println!(
            "nested_constant_namespaces[Time] = {:?}",
            idx.nested_constant_namespaces.get("Time")
        );
        println!("names = {:?}", idx.names);
    }

    /// PROBE 7 — the `names` (ClassId) ORDER LEAK CHANNEL. `Interner::cmp`
    /// canonicalises union members by `ClassId` for `Nominal`/`Singleton`
    /// (`crates/rigor-types/src/interner.rs:135,137`) and `named_union` renders
    /// members in that canonical order (`crates/rigor-types/src/display.rs:446`
    /// — only `nil` floats to the end). So file order → registration order →
    /// ClassId order → rendered union order.
    #[test]
    fn probe_classid_order_reaches_union_rendering() {
        use rigor_types::{Algebra, Type};
        let core = CoreIndex::new();
        let a = lower(&parse(b"class Alpha\nend\n"));
        let b = lower(&parse(b"class Beta\nend\n"));

        let render = |idx: &SourceIndex| {
            let mut i = Interner::new();
            let ca = idx.class_id("Alpha").unwrap();
            let cb = idx.class_id("Beta").unwrap();
            let na = i.intern(Type::Nominal { class: ca, args: Vec::new() });
            let nb = i.intern(Type::Nominal { class: cb, args: Vec::new() });
            let u = Algebra::join(&mut i, na, nb);
            let resolve = |c: rigor_types::ClassId| idx.class_name_for_id(c).map(str::to_string);
            (ca.0, cb.0, rigor_types::describe_named(&i, u, &resolve))
        };

        let (ida, idb, sab) = render(&SourceIndex::build_project(&[&a, &b], &core));
        println!("order [a,b]: Alpha={ida} Beta={idb} union renders as {sab:?}");
        assert!(ida < idb, "ids are handed out in file order");
        assert_eq!(sab, "Alpha | Beta");
        let (ida, idb, sba) = render(&SourceIndex::build_project(&[&b, &a], &core));
        println!("order [b,a]: Alpha={ida} Beta={idb} union renders as {sba:?}");
        assert!(ida > idb);
        assert_eq!(
            sba, "Beta | Alpha",
            "the ClassId channel is LIVE: the merge closes it by assigning ids in \
             file order, not by the rendering being order-free"
        );
    }

    /// PROBE 8 — `infer_method_returns`'s old doc claim that "a method never
    /// appears in BOTH maps" is FALSE per `(class, method)` KEY: a CROSS-FILE
    /// reopen has two independent def sites, each dispatched on its own. Pinned
    /// as an assertion because the corrected doc now states the truth, and the
    /// merge must not re-acquire the old assumption.
    #[test]
    fn method_can_appear_in_both_return_maps() {
        let core = CoreIndex::new();
        let a = lower(&parse(b"class A\n  def m\n    \"s\"\n  end\nend\n"));
        let b = lower(&parse(b"class A\n  def m(x)\n    x\n  end\nend\n"));
        let idx = SourceIndex::build_project(&[&a, &b], &core);
        assert_eq!(idx.method_return("A", "m"), Some("String"));
        assert_eq!(
            idx.param_bound_return("A", "m"),
            Some(&ParamBoundReturn { param_index: 0, chain: Vec::new() }),
            "a cross-file reopen lands in BOTH maps — the call site's \
             method_return-first precedence is what makes that harmless"
        );
    }

    // =======================================================================
    // The PRE-#92 build path, kept alive as the equivalence oracle.
    // =======================================================================

    /// `build_project` exactly as it stood before the harvest/merge split, with
    /// its own copies of the three per-file walkers so the comparison is against
    /// independent code and not a rename. The shared fold primitives
    /// (`add_source`, `ingest_override_class`, `infer_method_returns`,
    /// `compute_literal_returns`) are deliberately the production ones — this
    /// oracle exists to pin the ORCHESTRATION (pass order, barrier placement,
    /// replay order), which is what the decomposition actually moved.
    fn build_project_legacy(asts: &[&LoweredAst], core: &CoreIndex) -> SourceIndex {
        let mut idx = SourceIndex::default();

        // Pass 1.
        for ast in asts {
            for (_, node) in ast.iter() {
                match node {
                    Node::ClassDef { name, superclass, methods, .. } => {
                        if name.is_empty() {
                            continue;
                        }
                        idx.add_source(name, superclass.clone(), methods);
                    }
                    Node::ModuleDef { name, methods, .. } => {
                        if name.is_empty() {
                            continue;
                        }
                        idx.add_source(name, None, methods);
                    }
                    _ => {}
                }
            }
        }

        // Pass 1b.
        for ast in asts {
            legacy_collect_override_classes(&mut idx, ast, ast.root(), &[]);
        }

        // C1.
        let qualified_defs: Vec<String> = idx.override_classes.keys().cloned().collect();
        for qualified in &qualified_defs {
            let segs: Vec<&str> = qualified.split("::").collect();
            let Some((name, ns)) = segs.split_last() else { continue };
            if ns.is_empty() {
                idx.toplevel_constants.insert((*name).to_string());
            } else {
                let ns_vec: Vec<String> = ns.iter().map(|s| (*s).to_string()).collect();
                let entry = idx.nested_constant_namespaces.entry((*name).to_string()).or_default();
                if !entry.contains(&ns_vec) {
                    entry.push(ns_vec);
                }
            }
        }

        // Pass 1c.
        for ast in asts {
            let scope_spans: Vec<rigor_parse::Span> = ast
                .iter()
                .filter_map(|(_, n)| match n {
                    Node::ClassDef { span, .. } | Node::ModuleDef { span, .. } => Some(*span),
                    _ => None,
                })
                .collect();
            for (_, node) in ast.iter() {
                match node {
                    Node::Definition { name: Some(nm), span, .. }
                        if !scope_spans.iter().any(|s| s.0 <= span.0 && span.1 <= s.1) =>
                    {
                        idx.toplevel_defs.insert(nm.clone());
                    }
                    Node::Definition { receiver_def_name: Some(nm), span, .. }
                        if !scope_spans.iter().any(|s| s.0 <= span.0 && span.1 <= s.1) =>
                    {
                        idx.toplevel_defs.insert(nm.clone());
                    }
                    Node::ClassDef { name, methods, .. } | Node::ModuleDef { name, methods, .. }
                        if matches!(name.as_str(), "Object" | "Kernel" | "BasicObject") =>
                    {
                        idx.toplevel_defs.extend(methods.iter().cloned());
                    }
                    _ => {}
                }
            }
        }

        // Pass 1d.
        for ast in asts {
            let scopes = lexical_scopes(ast);
            for (_, node) in ast.iter() {
                let Node::Definition { name: Some(nm), span, .. } = node else {
                    continue;
                };
                let innermost = scopes
                    .iter()
                    .filter(|(s, _)| s.0 <= span.0 && span.1 <= s.1)
                    .min_by_key(|(s, _)| s.1 - s.0);
                if let Some((_, segs)) = innermost {
                    idx.discovered_methods
                        .entry(segs.join("::"))
                        .or_default()
                        .insert(nm.clone());
                }
            }
        }

        // Pass 1e.
        for ast in asts {
            for (_, node) in ast.iter() {
                let Node::Definition { params: Some(names), span, .. } = node else {
                    continue;
                };
                if names.is_empty() {
                    continue;
                }
                for (_, inner) in ast.iter() {
                    let Node::Call { receiver: Some(r), method, span: cspan, .. } = inner else {
                        continue;
                    };
                    if !(span.0 <= cspan.0 && cspan.1 <= span.1) {
                        continue;
                    }
                    if !crate::MUTATOR_METHODS.contains(&method.as_str()) {
                        continue;
                    }
                    let Node::LocalVariableRead { name: recv_name, .. } = ast.get(*r) else {
                        continue;
                    };
                    if let Some(i) = names.iter().position(|p| p == recv_name) {
                        for key in def_names(node) {
                            idx.mutated_params.entry(key).or_default().insert(i);
                        }
                    }
                }
            }
        }

        // C5.
        let mut lit_first: HashMap<String, (Vec<String>, u64, Option<ConstLit>)> = HashMap::new();
        let mut lit_multi: HashSet<String> = HashSet::new();
        for ast in asts {
            legacy_collect_literal_constants(
                ast,
                ast.root(),
                &[],
                ast.file_id(),
                &mut lit_first,
                &mut lit_multi,
            );
        }
        for qualified in lit_first.keys() {
            let bare = qualified.rsplit("::").next().unwrap_or(qualified).to_string();
            idx.project_constant_write_names.insert(bare);
        }
        for (qualified, (namespace, file, lit)) in lit_first {
            if lit_multi.contains(&qualified) {
                continue;
            }
            let bare = qualified.rsplit("::").next().unwrap_or(&qualified).to_string();
            if idx.override_classes.contains_key(&qualified) || idx.classes.contains_key(&bare) {
                continue;
            }
            if let Some(l) = lit {
                idx.qualified_literal_constants
                    .insert(qualified, (namespace.clone(), file, l.clone()));
                idx.literal_constants.entry(bare).or_default().push((namespace, file, l));
            }
        }

        // Pass 2.
        for ast in asts {
            for (_, node) in ast.iter() {
                if let Node::ConstantRead { name, .. } = node {
                    if !name.is_empty()
                        && !idx.classes.contains_key(name)
                        && (core.knows_class(name) || core.knows_qualified_class(name))
                    {
                        idx.register(name);
                    }
                }
            }
        }

        // Pass 2b.
        for name in core.tuple_return_class_names() {
            if !idx.classes.contains_key(name)
                && (core.knows_class(name) || core.knows_qualified_class(name))
            {
                if !idx.name_to_id.contains_key(name) {
                    idx.declaration_only_classes.insert(name.to_string());
                }
                idx.register(name);
            }
        }

        // Pass 3.
        let (returns, param_bound) = infer_method_returns(&idx, core, asts);
        idx.method_returns = returns;
        idx.param_bound_returns = param_bound;

        // Pass 4.
        let (defs, definers) = legacy_collect_fold_defs(asts);
        idx.definers = definers;
        idx.literal_returns = idx.compute_literal_returns(asts, &defs);

        idx
    }

    fn legacy_collect_override_classes(
        idx: &mut SourceIndex,
        ast: &LoweredAst,
        node: NodeId,
        prefix: &[String],
    ) {
        match ast.get(node) {
            Node::Program { body, .. } | Node::Statements { body, .. } => {
                for &child in body {
                    legacy_collect_override_classes(idx, ast, child, prefix);
                }
            }
            Node::ClassDef {
                name,
                superclass_path,
                methods,
                method_visibilities,
                includes,
                body,
                ..
            } => {
                if name.is_empty() {
                    return;
                }
                let qualified = qualify(prefix, name);
                idx.ingest_override_class(
                    &qualified,
                    superclass_path.clone(),
                    methods,
                    method_visibilities,
                    includes,
                );
                let child_prefix = split_qualified(&qualified);
                for &child in body {
                    legacy_collect_override_classes(idx, ast, child, &child_prefix);
                }
            }
            Node::ModuleDef { name, methods, method_visibilities, includes, body, .. } => {
                if name.is_empty() {
                    return;
                }
                let qualified = qualify(prefix, name);
                idx.ingest_override_class(&qualified, None, methods, method_visibilities, includes);
                let child_prefix = split_qualified(&qualified);
                for &child in body {
                    legacy_collect_override_classes(idx, ast, child, &child_prefix);
                }
            }
            _ => {}
        }
    }

    fn legacy_collect_literal_constants(
        ast: &LoweredAst,
        node: NodeId,
        prefix: &[String],
        file: u64,
        first: &mut HashMap<String, (Vec<String>, u64, Option<ConstLit>)>,
        multi: &mut HashSet<String>,
    ) {
        match ast.get(node) {
            Node::Program { body, .. } | Node::Statements { body, .. } => {
                for &child in body {
                    legacy_collect_literal_constants(ast, child, prefix, file, first, multi);
                }
            }
            Node::ClassDef { name, body, .. } | Node::ModuleDef { name, body, .. } => {
                if name.is_empty() {
                    return;
                }
                let child_prefix = split_qualified(&qualify(prefix, name));
                for &child in body {
                    legacy_collect_literal_constants(ast, child, &child_prefix, file, first, multi);
                }
            }
            Node::ConstantWrite { name, value, .. } => {
                let qualified = qualify(prefix, name);
                match first.entry(qualified) {
                    std::collections::hash_map::Entry::Occupied(e) => {
                        multi.insert(e.key().clone());
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert((prefix.to_vec(), file, const_lit_of(ast, *value)));
                    }
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::type_complexity)]
    fn legacy_collect_fold_defs(
        asts: &[&LoweredAst],
    ) -> (
        HashMap<(String, String, DefKind), Vec<FoldSite>>,
        HashMap<(String, DefKind), Vec<String>>,
    ) {
        let mut defs: HashMap<(String, String, DefKind), Vec<FoldSite>> = HashMap::new();
        for (ai, ast) in asts.iter().enumerate() {
            legacy_walk_fold_defs(ai, ast, ast.root(), &[], &mut defs);
        }
        let mut definers: HashMap<(String, DefKind), Vec<String>> = HashMap::new();
        for (owner, method, kind) in defs.keys() {
            let owners = definers.entry((method.clone(), *kind)).or_default();
            if !owners.contains(owner) {
                owners.push(owner.clone());
            }
        }
        (defs, definers)
    }

    fn legacy_walk_fold_defs(
        ast_idx: usize,
        ast: &LoweredAst,
        node: NodeId,
        prefix: &[String],
        defs: &mut HashMap<(String, String, DefKind), Vec<FoldSite>>,
    ) {
        match ast.get(node) {
            Node::Program { body, .. } | Node::Statements { body, .. } => {
                for &child in body {
                    legacy_walk_fold_defs(ast_idx, ast, child, prefix, defs);
                }
            }
            Node::ClassDef { name, body, .. } | Node::ModuleDef { name, body, .. } => {
                if name.is_empty() {
                    return;
                }
                let qualified = qualify(prefix, name);
                for &child in body {
                    if let Node::Definition {
                        name,
                        singleton_name,
                        body: def_body,
                        has_explicit_return,
                        ..
                    } = ast.get(child)
                    {
                        let entry = match (name, singleton_name) {
                            (Some(m), _) => Some((m.clone(), DefKind::Instance)),
                            (None, Some(m)) => Some((m.clone(), DefKind::Singleton)),
                            _ => None,
                        };
                        if let Some((method, kind)) = entry {
                            if let Some(&tail) = def_body.last() {
                                defs.entry((qualified.clone(), method, kind)).or_default().push(
                                    FoldSite {
                                        ast_idx,
                                        tail,
                                        has_explicit_return: *has_explicit_return,
                                    },
                                );
                            }
                        }
                    }
                }
                let child_prefix = split_qualified(&qualified);
                for &child in body {
                    legacy_walk_fold_defs(ast_idx, ast, child, &child_prefix, defs);
                }
            }
            _ => {}
        }
    }

    // =======================================================================
    // INVARIANT 7 — merge(harvests) ≡ the legacy inline path, field by field.
    // =======================================================================

    /// The fields whose ORDER is a function of the input (and therefore MUST
    /// match exactly): the registry bijection and the override index. The
    /// remaining `Vec`-valued fields (`definers`, `literal_constants`,
    /// `nested_constant_namespaces`) come out of `HashMap` iteration and are
    /// already unstable between processes on identical input (§3.4) — comparing
    /// them ordered would pin noise, so they are compared CANONICALISED, which
    /// is the whole content either way.
    const ORDER_BEARING: [&str; 3] = ["names", "name_to_id", "override_classes"];

    /// Assert `SourceIndex::build_project` (harvest + merge) and the pre-#92
    /// inline path agree on every field: canonicalised for content, and
    /// order-exact for the order-bearing fields.
    fn assert_paths_agree(asts: &[&LoweredAst], core: &CoreIndex, label: &str) {
        let new_idx = SourceIndex::build_project(asts, core);
        let old_idx = build_project_legacy(asts, core);

        let (fresh, legacy) = (fingerprint(&new_idx, true), fingerprint(&old_idx, true));
        assert_eq!(fresh.len(), 17, "the fingerprint must cover every field");
        if let Some((field, x, y)) = diff(&fresh, &legacy).into_iter().next() {
            panic!("[{label}] canonical field `{field}` diverged\n  merge  : {x}\n  legacy : {y}");
        }
        let (fresh, legacy) = (fingerprint(&new_idx, false), fingerprint(&old_idx, false));
        let ordered =
            diff(&fresh, &legacy).into_iter().find(|(field, _, _)| ORDER_BEARING.contains(field));
        if let Some((field, x, y)) = ordered {
            panic!("[{label}] ORDER of `{field}` diverged\n  merge  : {x}\n  legacy : {y}");
        }
    }

    /// Every corpus this module builds, under every permutation the probes use:
    /// the merge must reproduce the legacy path bit for bit.
    #[test]
    fn merge_equals_legacy_build_project() {
        let core = CoreIndex::new();
        let corpora: Vec<(&str, Vec<&[u8]>)> = vec![
            ("order-conflicts", probe1_sources()),
            (
                "drop-one",
                vec![
                    b"class Base\n  def m\n    1\n  end\nend\nSHARED = 1\nmodule Wrap\n  DUP = 1\nend\n",
                    b"class Base\n  private\n  def m\n    2\n  end\nend\nmodule Wrap\n  DUP = 2\nend\nclass Sub < Base\n  private\n  def m\n    3\n  end\nend\nSOLO = 7\n",
                    b"class Solo\n  def q\n    1\n  end\nend\n",
                ],
            ),
            (
                "singleton-union",
                vec![
                    b"class A\n  def x\n    \"s\"\n  end\nend\nC1 = 1\n",
                    b"class B < A\n  def y\n    A.new\n  end\nend\nC1 = 2\nC2 = 3\n",
                    b"class C\n  def z\n    C2\n  end\nend\n",
                ],
            ),
            (
                "cross-file-couplings",
                vec![
                    b"MAX = 5\nclass A\n  def m\n    MAX\n  end\nend\n",
                    b"MAX = 6\n",
                    b"class Base\n  def m\n    1\n  end\nend\n",
                    b"class Sub < Base\n  def m\n    2\n  end\nend\n",
                    b"pid, status = Process.wait2\nputs pid\nstatus.nosuchthing\n",
                ],
            ),
            (
                "intra-file-duplicate-writes",
                vec![
                    b"DUP = 1\nDUP = 2\nSOLO = 3\nmodule N\n  INNER = 4\nend\n",
                    b"OTHER = [1, 2].freeze\nPathname.new(\"/\")\n",
                ],
            ),
            (
                "toplevel-and-mutation",
                vec![
                    b"def tl_a; 1; end\nclass Object\n  def injected; 2; end\nend\ndef fill(a)\n  a << 1\nend\n",
                    b"def IO.console_size; 1; end\nmodule Kernel\n  def kern; 3; end\nend\n",
                ],
            ),
        ];
        for (label, srcs) in corpora {
            let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
            let refs: Vec<&LoweredAst> = asts.iter().collect();
            assert_paths_agree(&refs, &core, label);
            // Reverse order too: the ordered replay must track the caller's
            // order in BOTH directions, not just the one that happens to be
            // insertion-order-friendly.
            let rev: Vec<&LoweredAst> = asts.iter().rev().collect();
            assert_paths_agree(&rev, &core, &format!("{label} (reversed)"));
        }
    }

    /// The same equivalence under the probe's five permutations of the
    /// order-conflicting corpus — the shape where file order actually moves a
    /// diagnostic, so the shape a mis-ordered merge would break first.
    #[test]
    fn merge_equals_legacy_under_every_permutation() {
        let core = CoreIndex::new();
        let srcs = probe1_sources();
        let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
        for p in [[0, 1, 2, 3], [1, 0, 2, 3], [3, 2, 1, 0], [2, 3, 0, 1], [0, 2, 1, 3], [3, 0, 1, 2]]
        {
            let refs: Vec<&LoweredAst> = p.iter().map(|&i| &asts[i]).collect();
            assert_paths_agree(&refs, &core, &format!("perm {p:?}"));
        }
    }

    /// The single-file entry point (`SourceIndex::build`) goes through the same
    /// wrapper — every single-file tool (`sig-gen`, `annotate`, `type_of`)
    /// depends on it.
    #[test]
    fn merge_equals_legacy_for_a_single_file() {
        let core = CoreIndex::new();
        let ast = lower(&parse(
            b"module N\n  K = 1\n  class Time\n  end\n  class Foo < Bar\n    include M1\n    private\n    def m; 2; end\n  end\nend\ndef tl; 1; end\n",
        ));
        assert_paths_agree(&[&ast], &core, "single file");
        let built = SourceIndex::build(&ast, &core);
        let wrapped = SourceIndex::build_project(&[&ast], &core);
        assert_eq!(fingerprint(&built, true), fingerprint(&wrapped, true));
    }

    /// `merge` over an EMPTY file list must still produce the Pass-2b registry
    /// (the declaration-only classes are core-driven, not source-driven).
    #[test]
    fn merge_of_no_files_still_runs_the_barrier_passes() {
        let core = CoreIndex::new();
        assert_paths_agree(&[], &core, "empty");
        // Turbofished because an empty literal fixes no `H` (the parameter is
        // generic over anything that lends a `Harvest` out); `check`'s own element
        // type is the owned one, so that is what the empty case must instantiate.
        let idx = SourceIndex::merge::<Harvest>(&[], &core);
        assert!(idx.is_declaration_only_class("Process::Status"));
    }

    // =======================================================================
    // INVARIANTS 1-6 — the five minimal coupling examples, plus the three
    // semantics the merge is built on top of.
    // =======================================================================

    /// INVARIANT 1, example 1 (probe §3.2 i) — `method_visibilities` is
    /// FIRST-WRITE-WINS, so file order decides whether
    /// `def.override-visibility-reduced` fires. `a,b` records `m` public on
    /// `Base` (⇒ `Sub#m` private reduces it ⇒ fires); `b,a` records it private
    /// (⇒ silent). The diagnostic-level twin lives in rigor-rules
    /// (`override_vis_project_order_is_normative`).
    #[test]
    fn order_leak_visibility_first_write_wins() {
        let core = CoreIndex::new();
        let a = lower(&parse(b"class Base\n  def m\n    1\n  end\nend\n"));
        let b = lower(&parse(
            b"class Base\n  private\n  def m\n    2\n  end\nend\n\nclass Sub < Base\n  private\n  def m\n    3\n  end\nend\n",
        ));
        let vis = |idx: &SourceIndex| {
            idx.override_classes.get("Base").and_then(|c| c.method_visibilities.get("m")).copied()
        };
        assert_eq!(vis(&SourceIndex::build_project(&[&a, &b], &core)), Some(Visibility::Public));
        assert_eq!(vis(&SourceIndex::build_project(&[&b, &a], &core)), Some(Visibility::Private));
        // …and the walk that reads it flips with them.
        let ab = SourceIndex::build_project(&[&a, &b], &core);
        let ba = SourceIndex::build_project(&[&b, &a], &core);
        assert_eq!(
            ab.nearest_ancestor_defining("Sub", "m"),
            Some(("Base".to_string(), Some(Visibility::Public)))
        );
        assert_eq!(
            ba.nearest_ancestor_defining("Sub", "m"),
            Some(("Base".to_string(), Some(Visibility::Private)))
        );
    }

    /// INVARIANT 1, example 2 (probe §3.2 ii) — `includes` accumulate in FILE
    /// order and `override_ancestor_names` walks them in that order, so the
    /// MRO's nearest defining ancestor flips with the file order. Idiomatic
    /// Ruby: one class reopened in two files, each adding an `include`.
    #[test]
    fn order_leak_includes_accumulation_order() {
        let core = CoreIndex::new();
        let mods = lower(&parse(
            b"module M1\n  def m; 1; end\nend\nmodule M2\n  private\n  def m; 2; end\nend\n",
        ));
        let a = lower(&parse(b"class Foo\n  include M1\nend\n"));
        let b = lower(&parse(b"class Foo\n  include M2\n  private\n  def m; 3; end\nend\n"));
        let incs = |idx: &SourceIndex| {
            idx.override_classes.get("Foo").map(|c| c.includes.clone()).unwrap_or_default()
        };
        let ab = SourceIndex::build_project(&[&mods, &a, &b], &core);
        let ba = SourceIndex::build_project(&[&mods, &b, &a], &core);
        assert_eq!(incs(&ab), vec!["M1".to_string(), "M2".to_string()]);
        assert_eq!(incs(&ba), vec!["M2".to_string(), "M1".to_string()]);
        // `Foo` defines `m` itself, so the ancestor walk starts at the includes:
        // M1 first ⇒ the public definer is found ⇒ the rule fires; M2 first ⇒ a
        // private definer ⇒ silent.
        assert_eq!(
            ab.nearest_ancestor_defining("Foo", "m"),
            Some(("M1".to_string(), Some(Visibility::Public)))
        );
        assert_eq!(
            ba.nearest_ancestor_defining("Foo", "m"),
            Some(("M2".to_string(), Some(Visibility::Private)))
        );
    }

    /// INVARIANT 2, example 3 (probe §2.2) — Pass 3 is typed against the
    /// COMPLETE index, so a second file's constant write can delete a tier-4b
    /// return for a byte-identical first file. The merge must keep Pass 3 after
    /// the C5 barrier or this silently re-appears as an over-emission.
    #[test]
    fn coupling_pass3_reads_the_merged_constant_table() {
        let core = CoreIndex::new();
        let a = lower(&parse(b"MAX = 5\nclass A\n  def m\n    MAX\n  end\nend\n"));
        let b = lower(&parse(b"MAX = 6\n"));
        assert_eq!(SourceIndex::build_project(&[&a], &core).method_return("A", "m"), Some("Integer"));
        assert_eq!(SourceIndex::build_project(&[&a, &b], &core).method_return("A", "m"), None);
    }

    /// INVARIANT 2, example 4 (probe §2.2) — the Pass-4b overridable degrade is
    /// cross-file: `Base.m`'s folded literal exists alone and VANISHES once a
    /// second file declares a related subclass that redefines `m`.
    #[test]
    fn coupling_pass4b_degrade_is_cross_file() {
        let core = CoreIndex::new();
        let a = lower(&parse(b"class Base\n  def m\n    1\n  end\nend\n"));
        let b = lower(&parse(b"class Sub < Base\n  def m\n    2\n  end\nend\n"));
        let key = ("Base".to_string(), "m".to_string(), DefKind::Instance);
        assert_eq!(
            SourceIndex::build_project(&[&a], &core).literal_returns.get(&key),
            Some(&Scalar::Int(1))
        );
        assert_eq!(SourceIndex::build_project(&[&a, &b], &core).literal_returns.get(&key), None);
    }

    /// INVARIANT 2, example 5 (probe §2.2) — Pass 2b's declaration-only set asks
    /// "did NO analyzed file name this class?", which is unanswerable per file.
    /// `Process::Status` is declaration-only until some file names it.
    #[test]
    fn coupling_pass2b_declaration_only_is_cross_file() {
        let core = CoreIndex::new();
        let a = lower(&parse(b"pid, status = Process.wait2\nputs pid\nstatus.nosuchthing\n"));
        let b = lower(&parse(b"KLASS = Process::Status\nputs KLASS\n"));
        assert!(SourceIndex::build_project(&[&a], &core)
            .is_declaration_only_class("Process::Status"));
        assert!(!SourceIndex::build_project(&[&a, &b], &core)
            .is_declaration_only_class("Process::Status"));
    }

    /// INVARIANT 3 — the constant single-assignment gate counts INTRA-file
    /// duplicates too, which is why `Harvest` carries a per-file write COUNT and
    /// not a "was written" bool. All three shapes must decline identically:
    /// twice in one file, once each in two files, twice in one of two files.
    #[test]
    fn constant_single_assignment_counts_intra_file_duplicates() {
        let core = CoreIndex::new();
        let twice_here = lower(&parse(b"DUP = 1\nDUP = 2\nSOLO = 9\n"));
        let once_here = lower(&parse(b"DUP = 1\nSOLO = 9\n"));
        let once_there = lower(&parse(b"DUP = 2\n"));
        let solo = lower(&parse(b"UNRELATED = 3\n"));

        let harvested = |idx: &SourceIndex, name: &str| idx.literal_constants.contains_key(name);
        // Twice in ONE file ⇒ declined; the single write beside it survives.
        let one = SourceIndex::build_project(&[&twice_here], &core);
        assert!(!harvested(&one, "DUP"));
        assert!(harvested(&one, "SOLO"));
        // Once in each of two files ⇒ declined.
        let split = SourceIndex::build_project(&[&once_here, &once_there], &core);
        assert!(!harvested(&split, "DUP"));
        // Twice in one of two files ⇒ still declined.
        let mixed = SourceIndex::build_project(&[&twice_here, &solo], &core);
        assert!(!harvested(&mixed, "DUP"));
        // …and a lone write in a two-file project still harvests.
        assert!(harvested(&SourceIndex::build_project(&[&once_here, &solo], &core), "DUP"));
    }

    /// INVARIANT 4 — `register` is idempotent. The Pass-2 pre-filter drops
    /// today's `!classes.contains_key(name)` term and the harvest deduplicates
    /// repeated reads; both are no-ops ONLY because a repeat `register` neither
    /// appends a name nor moves an id.
    #[test]
    fn register_is_idempotent() {
        let mut idx = SourceIndex::default();
        idx.register("Alpha");
        idx.register("Beta");
        let (names, ids) = (idx.names.clone(), idx.name_to_id.clone());
        for _ in 0..3 {
            idx.register("Alpha");
            idx.register("Beta");
        }
        assert_eq!(idx.names, names);
        assert_eq!(idx.name_to_id, ids);
        // The same fact end to end: a file that reads `Time` fifty times
        // registers it exactly once, at the same id as reading it once.
        let core = CoreIndex::new();
        let once = lower(&parse(b"Time\n"));
        let many = lower(&parse(b"Time\nTime\nTime\nTime\n"));
        assert_eq!(
            SourceIndex::build_project(&[&once], &core).names,
            SourceIndex::build_project(&[&many], &core).names
        );
    }

    /// INVARIANT 5 — `HarvestedConst`'s file id keeps its PER-FILE consumption
    /// semantics: the stamp is the ASSIGNING file's `LoweredAst::file_id`, and a
    /// use site in another file never folds. The merge stamps it from the paired
    /// AST (never from the harvest), which is the discipline a persisted harvest
    /// would have to keep — see the type's persistence-hazard doc.
    #[test]
    fn harvested_const_file_id_is_the_assigning_file() {
        let core = CoreIndex::new();
        let a = lower(&parse(b"TOPL = 7\n"));
        let b = lower(&parse(b"puts TOPL\n"));
        let idx = SourceIndex::build_project(&[&a, &b], &core);
        let entries = idx.literal_constants.get("TOPL").expect("harvested");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, a.file_id(), "the ASSIGNING file's id, not the reader's");
        assert!(idx.literal_constant("TOPL", &[], a.file_id()).is_some());
        assert!(
            idx.literal_constant("TOPL", &[], b.file_id()).is_none(),
            "a cross-file read must not fold (the oracle is silent there)"
        );
    }

    /// The `Harvest` contract itself: it is a function of ONE file and the
    /// frozen core, so harvesting a file alone or beside others is the same
    /// object — this is what makes the CLI's stage-1 hoist legal.
    #[test]
    fn harvest_is_file_local() {
        let core = CoreIndex::new();
        let a = lower(&parse(b"MAX = 5\nclass A\n  def m\n    MAX\n  end\nend\n"));
        let b = lower(&parse(b"MAX = 6\nclass A\n  include M\nend\n"));
        let solo = SourceIndex::harvest(&a, &core);
        let beside = SourceIndex::harvest(&a, &core);
        let render = |h: &Harvest| {
            (
                h.source_classes.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                h.override_classes.iter().map(|c| c.qualified.clone()).collect::<Vec<_>>(),
                h.constant_writes.iter().map(|w| (w.qualified.clone(), w.writes)).collect::<Vec<_>>(),
                h.rbs_constant_names.clone(),
                h.fold_defs.iter().map(|d| (d.owner.clone(), d.method.clone())).collect::<Vec<_>>(),
            )
        };
        assert_eq!(render(&solo), render(&beside));
        // And merging that harvest with the second file's reproduces the
        // all-at-once build exactly.
        let pairs = vec![(SourceIndex::harvest(&a, &core), &a), (SourceIndex::harvest(&b, &core), &b)];
        assert_eq!(
            fingerprint(&SourceIndex::merge(&pairs, &core), false),
            fingerprint(&SourceIndex::build_project(&[&a, &b], &core), false)
        );
    }

    /// The four files probe 1 permutes: two visibility conflicts, an include
    /// order conflict, constants (toplevel + nested, single + multiply
    /// assigned), a toplevel def and an RBS-known constant read.
    ///
    /// `pub(super)` so the #94 equivalence harness (`probes_s94`) can grade the
    /// ancestor closure on the same override-graph shapes.
    pub(super) fn probe1_sources() -> Vec<&'static [u8]> {
        vec![
            b"class Base\n  def m\n    1\n  end\nend\nmodule M1\n  def shared\n    1\n  end\nend\nTOPC = 1\ndef tl_a; 1; end\nmodule N1\n  K = 1\n  class Time\n  end\nend\n",
            b"class Base\n  include M1\n  private\n  def m\n    2\n  end\nend\n",
            b"module M2\n  private\n  def shared\n    2\n  end\nend\nclass Base\n  include M2\nend\nclass Sub < Base\n  private\n  def shared\n    3\n  end\nend\n",
            b"class Other\n  def name\n    \"x\"\n  end\nend\nOTHERC = [1, 2].freeze\nPathname.new(\"/\")\nmodule N2\n  K = 2\n  class Time\n  end\nend\n",
        ]
    }
}

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
mod probes_s94 {
    use super::*;
    use rigor_parse::{lower, parse};

    /// The `CoreIndex` is passed in, never built per project: it is the most
    /// expensive thing in this module by an order of magnitude (RBS load), and
    /// the graded property does not depend on it.
    fn build(srcs: &[Vec<u8>], core: &CoreIndex) -> SourceIndex {
        let asts: Vec<LoweredAst> = srcs.iter().map(|s| lower(&parse(s))).collect();
        let refs: Vec<&LoweredAst> = asts.iter().collect();
        SourceIndex::build_project(&refs, core)
    }

    fn owned(srcs: Vec<&[u8]>) -> Vec<Vec<u8>> {
        srcs.into_iter().map(|s| s.to_vec()).collect()
    }

    /// Every name the graded pairs are drawn from: every class the override
    /// index knows, plus names it does NOT know (an unrelated owner must stay
    /// `false` through both paths).
    fn universe(idx: &SourceIndex) -> Vec<String> {
        let mut names: Vec<String> = idx.override_classes.keys().cloned().collect();
        names.sort();
        names.push("NoSuchClass".to_string());
        names.push("".to_string());
        names
    }

    /// Grade `closure.contains(owner)` against the pre-#94 walk for EVERY
    /// ordered pair over the universe, through both the memoized entry point
    /// (one shared [`AncestorClosures`] map, so cache hits are exercised) and a
    /// freshly built closure (so a cache hit can never be what makes it agree).
    /// Returns `(pairs graded, pairs that were related)`.
    fn grade(idx: &SourceIndex, label: &str) -> (usize, usize) {
        let names = universe(idx);
        let mut closures = AncestorClosures::new();
        let (mut graded, mut related) = (0usize, 0usize);
        for c in &names {
            let fresh = idx.build_ancestor_closure(c);
            for o in &names {
                let legacy = idx.related_to_owner(c, o);
                let cached = idx.ancestor_closure(c, &mut closures).contains(o);
                assert_eq!(legacy, cached, "{label}: cached closure disagrees at ({c:?}, {o:?})");
                let f = fresh.contains(o);
                assert_eq!(legacy, f, "{label}: fresh closure disagrees at ({c:?}, {o:?})");
                graded += 1;
                if legacy {
                    related += 1;
                }
            }
        }
        (graded, related)
    }

    /// The override-graph shapes the probe corpora actually contain — the four
    /// permuted probe-1 files, the probe-2 reopen trio, and the awkward shapes a
    /// real corpus does contain but those two do not: cycles, a self-referential
    /// class/module, a diamond, and lexical nesting where the SAME short name
    /// resolves to a nested class in one scope and a toplevel one in another.
    #[test]
    fn closure_matches_legacy_on_probe_corpora() {
        let corpora: Vec<(&str, Vec<Vec<u8>>)> = vec![
            ("probe1", owned(super::probes_s92::probe1_sources())),
            (
                "probe2",
                owned(vec![
                    b"class Base\n  def m\n    1\n  end\nend\nSHARED = 1\nmodule Wrap\n  DUP = 1\nend\n",
                    b"class Base\n  private\n  def m\n    2\n  end\nend\nmodule Wrap\n  DUP = 2\nend\nclass Sub < Base\n  private\n  def m\n    3\n  end\nend\nSOLO = 7\n",
                    b"class Solo\n  def q\n    1\n  end\nend\n",
                ]),
            ),
            (
                // A superclass cycle, a self-superclass, and a self-include:
                // the shapes the walk's `seen` guard exists for.
                "cycles",
                owned(vec![
                    b"class A < B\nend\nclass B < A\nend\nclass S < S\nend\n",
                    b"module Loop\n  include Loop\nend\nclass UsesLoop\n  include Loop\nend\n",
                    b"class A\n  include Loop\nend\n",
                ]),
            ),
            (
                // A diamond plus a deep-ish chain, reopened across files so the
                // includes accumulate in source order (MRO-bearing).
                "diamond",
                owned(vec![
                    b"module Top\nend\nmodule Left\n  include Top\nend\nmodule Right\n  include Top\nend\n",
                    b"class Mid\n  include Left\nend\nclass Mid\n  include Right\nend\nclass Leaf < Mid\nend\nclass Leafer < Leaf\n  include Left\nend\n",
                ]),
            ),
            (
                // Lexical nesting: `include Shared` inside `Outer` resolves to
                // `Outer::Shared`, the same text at toplevel resolves to
                // `Shared` — the qualification keystone the walk must preserve.
                "nesting",
                owned(vec![
                    b"module Shared\nend\nmodule Outer\n  module Shared\n  end\n  class Inner\n    include Shared\n  end\n  class Deep < Inner\n  end\nend\nclass Flat\n  include Shared\nend\n",
                    b"module Outer\n  class Inner\n    include Outer::Shared\n  end\nend\nclass Other < Outer::Deep\nend\n",
                ]),
            ),
        ];
        let core = CoreIndex::new();
        let mut total_related = 0usize;
        for (label, srcs) in &corpora {
            let idx = build(srcs, &core);
            let (graded, related) = grade(&idx, label);
            println!("--- {label}: {graded} pairs graded, {related} related");
            total_related += related;
        }
        assert!(
            total_related >= 20,
            "the corpora must EXERCISE relatedness, not agree vacuously ({total_related})"
        );
    }

    /// A deterministic xorshift64 — the randomized hierarchies are reproducible
    /// from the fixed seed, so a failure is replayable.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// A random project: `n` classes and modules spread over 3 files, with
    /// reopens, random `include`s, random superclasses, lexically nested
    /// namespaces, references to names that resolve nowhere, and no acyclicity
    /// guarantee whatsoever (cycles are the point).
    fn random_hierarchy(rng: &mut Rng, n: usize) -> Vec<Vec<u8>> {
        let pool: Vec<String> = (0..n)
            .map(|i| match i % 4 {
                0 => format!("C{i}"),
                1 => format!("M{i}"),
                2 => format!("Ns::C{i}"),
                _ => format!("M{i}"),
            })
            .collect();
        // As-written ancestor names: bare short names (so lexical resolution
        // does the work), fully qualified names, and a name that exists nowhere.
        let refname = |rng: &mut Rng| -> String {
            match rng.below(8) {
                0 => "Absent".to_string(),
                1..=2 => {
                    let p = &pool[rng.below(pool.len())];
                    p.rsplit("::").next().unwrap().to_string()
                }
                _ => pool[rng.below(pool.len())].clone(),
            }
        };
        let mut files: Vec<String> = vec![String::new(), String::new(), String::new()];
        for _ in 0..(n * 2) {
            let target = pool[rng.below(pool.len())].clone();
            let file = rng.below(files.len());
            let (nested, short) = match target.split_once("::") {
                Some((_, s)) => (true, s.to_string()),
                None => (false, target.clone()),
            };
            let is_module = short.starts_with('M');
            let mut body = String::new();
            for _ in 0..rng.below(3) {
                body.push_str(&format!("  include {}\n", refname(rng)));
            }
            let head = if is_module {
                format!("module {short}\n")
            } else if rng.below(3) == 0 {
                format!("class {short}\n")
            } else {
                format!("class {short} < {}\n", refname(rng))
            };
            let decl = format!("{head}{body}end\n");
            if nested {
                files[file].push_str(&format!("module Ns\n{decl}end\n"));
            } else {
                files[file].push_str(&decl);
            }
        }
        files.into_iter().map(|f| f.into_bytes()).collect()
    }

    /// Old-vs-new over randomized hierarchies. Every pair of every generated
    /// project is graded against the pre-#94 walk.
    #[test]
    fn closure_matches_legacy_on_random_hierarchies() {
        let core = CoreIndex::new();
        let mut rng = Rng(0x0094_A9CE_5709_5EED);
        let (mut graded, mut related) = (0usize, 0usize);
        for round in 0..120 {
            let n = 4 + rng.below(14);
            let srcs = random_hierarchy(&mut rng, n);
            let idx = build(&srcs, &core);
            let (g, r) = grade(&idx, &format!("random#{round}"));
            graded += g;
            related += r;
        }
        println!("--- random: {graded} pairs graded, {related} related");
        assert!(
            related >= 200,
            "the generator must produce genuinely related pairs (got {related} of {graded})"
        );
    }

    /// THE CAP TEST — the boundary no corpus reaches. In the pre-#94 walk the
    /// owner check ran on POP, before the `visited > OVERRIDE_ANCESTOR_WALK_LIMIT`
    /// return, so the node that OVERFLOWS the cap still answers `true` while
    /// everything past it answers `false`. Owner placed just inside the cap, AT
    /// the overflow, and one step past it — old and new must agree at all three.
    #[test]
    fn closure_matches_legacy_at_the_walk_cap() {
        // C0 <- C1 <- ... <- C109; the walk starts at C109's ancestors, so C108
        // is visit 1 and C{109-i} is visit i.
        let mut src = String::from("class C0\nend\n");
        for i in 1..110 {
            src.push_str(&format!("class C{i} < C{}\nend\n", i - 1));
        }
        let idx = build(&[src.into_bytes()], &CoreIndex::new());
        let candidate = "C109";
        let limit = OVERRIDE_ANCESTOR_WALK_LIMIT; // 100
        let just_inside = format!("C{}", 109 - limit); // C9  — visit 100
        let at_boundary = format!("C{}", 108 - limit); // C8  — visit 101, overflows
        let just_past = format!("C{}", 107 - limit); // C7  — never popped

        let closure = idx.build_ancestor_closure(candidate);
        assert!(idx.related_to_owner(candidate, &just_inside), "legacy: inside the cap");
        assert!(idx.related_to_owner(candidate, &at_boundary), "legacy: the overflowing node");
        assert!(!idx.related_to_owner(candidate, &just_past), "legacy: past the cap");
        assert!(closure.contains(&just_inside), "closure: inside the cap");
        assert!(closure.contains(&at_boundary), "closure: the overflowing node");
        assert!(!closure.contains(&just_past), "closure: past the cap");
        // The cap really did fire: 100 visited nodes plus the overflowing one.
        assert_eq!(closure.len(), limit + 1);
        grade(&idx, "cap-chain");
    }

    /// The other half of the cap boundary: nodes still QUEUED when the cap fires
    /// were never popped, so they were never owner-checkable. A 200-wide fan-out
    /// leaves 99 modules in the queue behind the overflowing one.
    #[test]
    fn closure_matches_legacy_when_the_cap_abandons_a_queue() {
        let mut src = String::new();
        for i in 0..200 {
            src.push_str(&format!("module M{i}\nend\n"));
        }
        src.push_str("class R\n");
        for i in 0..200 {
            src.push_str(&format!("  include M{i}\n"));
        }
        src.push_str("end\n");
        let idx = build(&[src.into_bytes()], &CoreIndex::new());
        let limit = OVERRIDE_ANCESTOR_WALK_LIMIT; // 100
        let closure = idx.build_ancestor_closure("R");
        // M0..M99 are visits 1..100; M100 overflows the cap but is still popped
        // (⇒ owner-checkable); M101.. never leave the queue.
        assert!(idx.related_to_owner("R", &format!("M{}", limit - 1)));
        assert!(idx.related_to_owner("R", &format!("M{limit}")), "the overflowing node");
        assert!(!idx.related_to_owner("R", &format!("M{}", limit + 1)), "abandoned in the queue");
        assert!(closure.contains(&format!("M{}", limit - 1)));
        assert!(closure.contains(&format!("M{limit}")));
        assert!(!closure.contains(&format!("M{}", limit + 1)));
        assert_eq!(closure.len(), limit + 1);
        grade(&idx, "cap-fanout");
    }
}
