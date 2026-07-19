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

use std::collections::{HashMap, HashSet};

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
    literal_constants: HashMap<String, Vec<(Vec<String>, ConstLit)>>,
    /// C1 (constant-shadow gate): for a constant the project defines NESTED, the
    /// containing-namespace segment vectors keyed by the constant's last segment
    /// (`module Gitlab; module Database; module Partitioning; module Time` keys
    /// `"Time" -> [["Gitlab","Database","Partitioning"]]`). A bare read of `Time`
    /// is shadowed ONLY at a use site whose lexical prefix has one of these
    /// namespaces as an initial segment run — Ruby's `Module.nesting` lexical
    /// lookup, matching the reference's `lexical_constant_candidates`. Elsewhere
    /// the read RELAXES so the core-RBS singleton is witnessed (the C1 fix).
    nested_constant_namespaces: HashMap<String, Vec<Vec<String>>>,
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
        let mut idx = SourceIndex::default();

        // Pass 1: source class/module structure, harvested across ALL files.
        for ast in asts {
            for (_, node) in ast.iter() {
                match node {
                    Node::ClassDef { name, superclass, methods, .. } => {
                        if name.is_empty() {
                            continue; // un-namable (dynamic constant) ⇒ skip.
                        }
                        idx.add_source(name, superclass.clone(), methods);
                    }
                    Node::ModuleDef { name, methods, .. } => {
                        if name.is_empty() {
                            continue;
                        }
                        idx.add_source(name, None, methods); // a module has no super.
                    }
                    _ => {}
                }
            }
        }

        // Pass 1b (ADR-35 slice 1): build the LEXICALLY-QUALIFIED override index
        // by a recursive walk of each file's AST with a nesting stack, so a
        // nested `module Params` is keyed `Outer::Params` (not the collapsed
        // `Params`). This is what keeps the override-visibility rule free of the
        // name-collision false positives. Kept entirely separate from the
        // collapsed `classes` map above — no other rule is affected.
        for ast in asts {
            idx.collect_override_classes(ast, ast.root(), &[]);
        }

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

        // Pass 1c (ADR-34): PROJECT-WIDE toplevel method names for
        // `call.unresolved-toplevel`. A `def` OUTSIDE any class/module body is a
        // toplevel def (an Object private method); an in-source reopen of
        // Object/Kernel/BasicObject injects toplevel-callable methods too. Toplevel
        // detection is span-containment against the file's class/module spans
        // (orphan-proof). Harvested across ALL files so a `def` in one file
        // resolves a call in another (the reference's project-mode resolution).
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
                    Node::ClassDef { name, methods, .. } | Node::ModuleDef { name, methods, .. }
                        if matches!(name.as_str(), "Object" | "Kernel" | "BasicObject") =>
                    {
                        idx.toplevel_defs.extend(methods.iter().cloned());
                    }
                    _ => {}
                }
            }
        }

        // C5: harvest single-assignment fully-literal `CONST = <literal>` values,
        // LEXICALLY qualified (like the C1 override walk). A QUALIFIED name
        // qualifies iff it is assigned EXACTLY ONCE project-wide, its RHS harvests
        // to a `ConstLit` (fully literal), and its bare name does NOT also name a
        // class/module. Ambiguity (multiple writes to the same qualified name, a
        // non-literal RHS, a class-name collision) declines. The recorded value
        // is keyed by BARE name + DEFINING NAMESPACE so the use-site consults it
        // lexically — a constant only visible in its defining namespace never
        // folds at an unrelated use site (the app/models concern-constant FP).
        let mut lit_first: HashMap<String, (Vec<String>, Option<ConstLit>)> = HashMap::new();
        let mut lit_multi: HashSet<String> = HashSet::new();
        for ast in asts {
            collect_literal_constants(ast, ast.root(), &[], &mut lit_first, &mut lit_multi);
        }
        for (qualified, (namespace, lit)) in lit_first {
            if lit_multi.contains(&qualified) {
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
                idx.literal_constants.entry(bare).or_default().push((namespace, l));
            }
        }

        // Pass 2: register an instance-class id for every `ConstantRead` whose
        // `name` is RBS-known but not a source class (source classes are already
        // registered). This lets both `Pathname.new(...)` instances AND bare
        // singleton constants (`Time`, `Array`, ...) carry a registry identity
        // that round-trips for rendering. Harvested across ALL files.
        for ast in asts {
            for (_, node) in ast.iter() {
                if let Node::ConstantRead { name, .. } = node {
                    // ADR-0042 Slice 2: register a QUALIFIED RBS-known constant
                    // read (`ERB::Util`) too, so it carries a registry id that
                    // round-trips for `Singleton` rendering. `knows_class` (short
                    // key) covers top-level and the merged composite; the added
                    // `knows_qualified_class` covers a namespaced name the short
                    // map lacks.
                    if !name.is_empty()
                        && !idx.classes.contains_key(name)
                        && (core.knows_class(name) || core.knows_qualified_class(name))
                    {
                        idx.register(name);
                    }
                }
            }
        }

        // Pass 3 (ADR-0023 tier-4b): infer per-method RETURN types. Runs AFTER the
        // source/registry maps are complete (so a Typer over `&idx` sees every
        // project class), and produces a fresh map that is then assigned — we must
        // NOT mutate `idx.method_returns` while `&idx` is immutably borrowed for
        // typing, so the inference returns a value.
        let (returns, param_bound) = infer_method_returns(&idx, core, asts);
        idx.method_returns = returns;
        idx.param_bound_returns = param_bound;

        // Pass 4 (ADR-0038): interprocedural literal-tail return folding. Runs
        // AFTER Pass 1b (`override_classes`, the ancestry the degrade + implicit-
        // self resolution walk) and needs no `core`/typing state. Harvests every
        // project instance + singleton `def` body by QUALIFIED owner name (the
        // lexical walk, so `module Gitlab; module Database` keys `Gitlab::
        // Database` — matching a `Gitlab::Database.read_only?` receiver), inverts
        // to a definers index, then folds each method's tail to a scalar literal
        // (resolving nested project calls, applying the overridable degrade).
        let (defs, definers) = collect_fold_defs(asts);
        idx.definers = definers;
        idx.literal_returns = idx.compute_literal_returns(asts, &defs);

        idx
    }

    /// Whether `name` is a PROJECT-WIDE toplevel method (a toplevel `def` in any
    /// analyzed file, or an in-source Object/Kernel/BasicObject reopen method) —
    /// the `call.unresolved-toplevel` cross-file suppression surface.
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
    pub fn literal_constant(&self, name: &str, use_prefix: &[String]) -> Option<&ConstLit> {
        self.literal_constants
            .get(name)?
            .iter()
            .filter(|(ns, _)| ns.len() <= use_prefix.len() && use_prefix[..ns.len()] == ns[..])
            .max_by_key(|(ns, _)| ns.len())
            .map(|(_, lit)| lit)
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
    /// The param-INDEPENDENT [`method_return`] takes precedence at the call site
    /// (a method has at most one of the two). See [`ParamBoundReturn`].
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

    /// ADR-35 slice 1: recursively collect the LEXICALLY-QUALIFIED override
    /// classes from `ast`, starting at `node` under the lexical `prefix` (the
    /// enclosing class/module name segments). A `ClassDef`/`ModuleDef` contributes
    /// an [`OverrideClass`] keyed by `prefix + name`, then recurses into its body
    /// with the extended prefix so a nested class/module is fully qualified. Other
    /// nodes recurse over their direct children only enough to reach nested
    /// class/module bodies (handled via the explicit body lists below).
    ///
    /// First-write-wins on reopen / cross-file for visibilities + superclass
    /// (mirrors the reference's stable accumulator); methods + includes accumulate.
    fn collect_override_classes(&mut self, ast: &LoweredAst, node: NodeId, prefix: &[String]) {
        match ast.get(node) {
            Node::Program { body, .. } | Node::Statements { body, .. } => {
                for &child in body {
                    self.collect_override_classes(ast, child, prefix);
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
                self.ingest_override_class(
                    &qualified,
                    superclass_path.clone(),
                    methods,
                    method_visibilities,
                    includes,
                );
                let child_prefix = split_qualified(&qualified);
                for &child in body {
                    self.collect_override_classes(ast, child, &child_prefix);
                }
            }
            Node::ModuleDef {
                name,
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
                self.ingest_override_class(&qualified, None, methods, method_visibilities, includes);
                let child_prefix = split_qualified(&qualified);
                for &child in body {
                    self.collect_override_classes(ast, child, &child_prefix);
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
    fn compute_literal_returns(
        &self,
        asts: &[&LoweredAst],
        defs: &HashMap<(String, String, DefKind), Vec<FoldSite>>,
    ) -> HashMap<(String, String, DefKind), Scalar> {
        let mut memo: HashMap<(String, String, DefKind), Option<Scalar>> = HashMap::new();
        for key in defs.keys() {
            let mut visiting: HashSet<(String, String, DefKind)> = HashSet::new();
            self.resolve_fold_key(key, defs, asts, &mut memo, &mut visiting);
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
    ) -> Option<Scalar> {
        if let Some(v) = memo.get(key) {
            return v.clone();
        }
        if visiting.contains(key) {
            return None; // cycle (recursive method) ⇒ decline, don't memoize.
        }
        visiting.insert(key.clone());
        let raw = self.fold_key_sites(key, defs, asts, memo, visiting);
        let result = match raw {
            Some(_) if self.overridden_in_project(&key.0, &key.1, key.2) => None,
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
    ) -> Option<Scalar> {
        let sites = defs.get(key)?;
        let mut acc: Option<Scalar> = None;
        for site in sites {
            if site.has_explicit_return {
                return None;
            }
            let ast = asts[site.ast_idx];
            let s = self.fold_expr(ast, site.tail, &key.0, key.2, defs, asts, memo, visiting, 0)?;
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
                self.resolve_fold_key(&(owner, method, kind), defs, asts, memo, visiting)
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
                        ast, r, self_qual, self_kind, defs, asts, memo, visiting, depth + 1,
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
                            );
                        }
                    }
                }
                // A core fold on a value-pinned receiver + args (`1 + 1`, `"x" ==
                // "y"`). Declines unless every part folds.
                let recv = self.fold_expr(
                    ast, r, self_qual, self_kind, defs, asts, memo, visiting, depth + 1,
                )?;
                let mut arg_scalars = Vec::with_capacity(args.len());
                for a in args {
                    arg_scalars.push(self.fold_expr(
                        ast, a, self_qual, self_kind, defs, asts, memo, visiting, depth + 1,
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
    fn overridden_in_project(&self, owner: &str, method: &str, kind: DefKind) -> bool {
        let Some(candidates) = self.definers.get(&(method.to_string(), kind)) else {
            return false;
        };
        candidates
            .iter()
            .any(|c| c != owner && self.related_to_owner(c, owner))
    }

    /// Whether `candidate`'s transitive project ancestry reaches `owner` — i.e.
    /// `candidate` is a subclass of an owner class or an includer of an owner
    /// module. Reuses the same ancestor walk as method resolution.
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
/// independent map. A method never appears in BOTH maps (its tail is either a
/// concrete core class under the empty env, or param-rooted — never both).
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

/// Split a qualified name into its segment vector (`"A::B" -> ["A", "B"]`), used
/// as the child lexical prefix when recursing into a class/module body.
fn split_qualified(qualified: &str) -> Vec<String> {
    qualified.split("::").map(|s| s.to_string()).collect()
}

/// C1: the per-file lexical class/module SCOPES — each `(span, qualified segment
/// vector)` — so a `ConstantRead`'s use-site lexical prefix can be recovered by
/// span containment (the innermost enclosing scope). Mirrors the qualification
/// walk of [`SourceIndex::collect_override_classes`]; computed once per analyzed
/// file and threaded into the [`Typer`] so its `ConstantRead` arm can consult
/// [`SourceIndex::constant_shadowed`] with the correct lexical prefix.
///
/// [`Typer`]: crate::Typer
pub fn lexical_scopes(ast: &LoweredAst) -> Vec<(rigor_parse::Span, Vec<String>)> {
    let mut out = Vec::new();
    collect_lexical_scopes(ast, ast.root(), &[], &mut out);
    out
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

/// C5: recursively collect lexically-qualified `CONST = <literal>` writes from
/// `ast` under lexical `prefix`. Each `ConstantWrite` records its QUALIFIED name
/// (for the single-assignment gate) → `(defining namespace, harvested value)`;
/// a second write to the same qualified name marks it multi (declined). Only
/// class/module/program BODIES are walked (a def-nested constant is out of
/// scope), mirroring the C1 override / fold discovery inclusion rule.
fn collect_literal_constants(
    ast: &LoweredAst,
    node: NodeId,
    prefix: &[String],
    first: &mut HashMap<String, (Vec<String>, Option<ConstLit>)>,
    multi: &mut HashSet<String>,
) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                collect_literal_constants(ast, child, prefix, first, multi);
            }
        }
        Node::ClassDef { name, body, .. } | Node::ModuleDef { name, body, .. } => {
            if name.is_empty() {
                return;
            }
            let child_prefix = split_qualified(&qualify(prefix, name));
            for &child in body {
                collect_literal_constants(ast, child, &child_prefix, first, multi);
            }
        }
        Node::ConstantWrite { name, value, .. } => {
            let qualified = qualify(prefix, name);
            match first.entry(qualified) {
                std::collections::hash_map::Entry::Occupied(e) => {
                    multi.insert(e.key().clone());
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert((prefix.to_vec(), const_lit_of(ast, *value)));
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
                elems.push(const_lit_of(ast, e)?);
            }
            Some(ConstLit::Tuple(elems))
        }
        Node::HashLit { elements, all_assoc, .. } => {
            if !*all_assoc {
                return None; // a `**`splat / non-assoc element ⇒ decline.
            }
            let mut members: Vec<(ShapeKey, ConstLit)> = Vec::with_capacity(elements.len() / 2);
            let mut i = 0;
            while i + 1 < elements.len() {
                let key = const_shape_key_of(ast.get(elements[i]))?;
                let value = const_lit_of(ast, elements[i + 1])?;
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

/// ADR-0038 — harvest every project instance + singleton `def` body by QUALIFIED
/// owner name (the same lexical walk `collect_override_classes` uses, so
/// `module Gitlab; module Database` keys `Gitlab::Database`), returning the
/// per-`(owner, method, kind)` def sites (tail node + explicit-return flag,
/// reopens accumulated) and the inverted `(method, kind) -> [owners]` definers
/// index. Only DIRECT `def` children of a class/module body are harvested — a
/// def nested in a conditional / inner method is out of scope, matching the
/// tier-4b / override discovery inclusion rule.
#[allow(clippy::type_complexity)]
fn collect_fold_defs(
    asts: &[&LoweredAst],
) -> (
    HashMap<(String, String, DefKind), Vec<FoldSite>>,
    HashMap<(String, DefKind), Vec<String>>,
) {
    let mut defs: HashMap<(String, String, DefKind), Vec<FoldSite>> = HashMap::new();
    for (ai, ast) in asts.iter().enumerate() {
        walk_fold_defs(ai, ast, ast.root(), &[], &mut defs);
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

/// Recursive helper for [`collect_fold_defs`]: at each `ClassDef`/`ModuleDef`,
/// harvest its direct `def` children (instance via `name`, singleton via
/// `singleton_name`) keyed by the qualified owner, then recurse into the body for
/// nested classes/modules with the extended lexical prefix.
fn walk_fold_defs(
    ast_idx: usize,
    ast: &LoweredAst,
    node: NodeId,
    prefix: &[String],
    defs: &mut HashMap<(String, String, DefKind), Vec<FoldSite>>,
) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                walk_fold_defs(ast_idx, ast, child, prefix, defs);
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
                walk_fold_defs(ast_idx, ast, child, &child_prefix, defs);
            }
        }
        _ => {}
    }
}

/// Whether a tail node is a branch/loop carrier whose type is not a single
/// concrete class (gate 4). `BeginRescue` also covers a lowered parenthesized
/// expression and an inline `rescue` body — both decline conservatively.
fn is_branch_carrier(node: &Node) -> bool {
    matches!(
        node,
        Node::If { .. }
            | Node::Case { .. }
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
            idx.literal_constant("N", &seg(&["K"])),
            Some(&ConstLit::Scalar(Scalar::Int(42)))
        );
        assert!(matches!(idx.literal_constant("A", &seg(&["K"])), Some(ConstLit::Tuple(_))));
        assert_eq!(
            idx.literal_constant("S", &seg(&["K"])),
            Some(&ConstLit::Scalar(Scalar::Str("hi".into())))
        );
        // NOT visible from an unrelated namespace or the toplevel.
        assert_eq!(idx.literal_constant("N", &[]), None);
        assert_eq!(idx.literal_constant("N", &seg(&["Other"])), None);
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
            idx.literal_constant("DAYS", &seg(&["Expirable"])),
            Some(&ConstLit::Scalar(Scalar::Int(7)))
        );
        assert_eq!(idx.literal_constant("DAYS", &seg(&["Consumer"])), None);
        assert_eq!(idx.literal_constant("DAYS", &[]), None);
    }

    #[test]
    fn multiple_assignment_declines_harvest() {
        // A constant written twice (same qualified name) is ambiguous ⇒ declined.
        let core = CoreIndex::new();
        let (_a, idx) =
            build_one(b"class K\n  M = 1\n  M = 2\nend\n", &core);
        assert_eq!(idx.literal_constant("M", &seg(&["K"])), None);
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
        assert_eq!(idx.literal_constant("Widget", &[]), None);
    }

    #[test]
    fn range_constant_harvests_as_range() {
        let core = CoreIndex::new();
        let (_a, idx) = build_one(b"class K\n  R = 1..1024\nend\n", &core);
        assert_eq!(idx.literal_constant("R", &seg(&["K"])), Some(&ConstLit::Range));
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
}
