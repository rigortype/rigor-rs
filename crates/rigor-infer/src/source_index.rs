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
use rigor_parse::{LoweredAst, Node};
use rigor_types::ClassId;

/// The first [`ClassId`] handed out by the per-run registry. Chosen well above
/// the fixed core-class id space (`CORE_CLASSES`, currently 9 entries) so a
/// registered instance's nominal id can never be mistaken for a core class by
/// `CoreIndex::class_name_for_id`. A million-id gap is ample headroom.
pub const SOURCE_CLASS_BASE: u32 = 1_000_000;

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
