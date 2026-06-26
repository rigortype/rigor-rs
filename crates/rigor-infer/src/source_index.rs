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
use rigor_types::{ClassId, Interner};

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

        // Pass 2: register an instance-class id for every `ConstantRead` whose
        // `name` is RBS-known but not a source class (source classes are already
        // registered). This lets both `Pathname.new(...)` instances AND bare
        // singleton constants (`Time`, `Array`, ...) carry a registry identity
        // that round-trips for rendering. Harvested across ALL files.
        for ast in asts {
            for (_, node) in ast.iter() {
                if let Node::ConstantRead { name, .. } = node {
                    if !name.is_empty()
                        && !idx.classes.contains_key(name)
                        && core.knows_class(name)
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

        idx
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
/// The same gates 2/3/4 (empty body / explicit return / branch tail) and the
/// reopen-disagreement rule apply, tracked independently from the param-
/// independent map. A method never appears in BOTH maps (its tail is either a
/// concrete core class under the empty env, or param-rooted — never both).
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
}
