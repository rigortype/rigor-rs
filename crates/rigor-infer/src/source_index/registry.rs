//! The per-run instance-class registry (`ClassId` <-> name) and the method-existence gates
//! `call.undefined-method` and `call.unresolved-toplevel` consult.

use std::collections::HashSet;

use rigor_index::CoreIndex;
use rigor_parse::FileKey;
use rigor_types::{ClassId, Interner};

use super::{SourceIndex, OVERRIDE_ANCESTOR_WALK_LIMIT, SOURCE_CLASS_BASE};

impl SourceIndex {
    /// Whether the PROJECT declares instance method `method` on `class_name`
    /// (qualified) — the reference's `source_declared_method?` reading the
    /// def-stripped cross-file `methods` seed overlaid by THIS file's own
    /// `file_methods` (`seed_discovered_methods` deep-merges the file's raw
    /// table). `file` is the analyzed file's [`rigor_parse::FileKey`]; `None`
    /// — and a key the merge never saw — consults the UNION of every file's
    /// overlay, the pre-overlay "project-wide" answer tests still assert.
    /// See [`Self::discovered_methods`].
    ///
    /// A pure SILENCER: `true` means "do not witness absence here". It is never
    /// consulted to prove a method exists for any positive inference, so a false
    /// `true` costs coverage, never correctness.
    pub fn project_declares_method(
        &self,
        file: Option<&FileKey>,
        class_name: &str,
        method: &str,
    ) -> bool {
        if self
            .discovered_methods
            .get(class_name)
            .is_some_and(|m| m.contains(method))
        {
            return true;
        }
        match file.and_then(|k| self.file_index.get(k)).copied() {
            Some(i) => self.file_defs[i]
                .methods
                .get(class_name)
                .is_some_and(|m| m.contains(method)),
            None => self.file_defs.iter().any(|fd| {
                fd.methods
                    .get(class_name)
                    .is_some_and(|m| m.contains(method))
            }),
        }
    }

    /// Whether SOME project method named `method` mutates its positional
    /// parameter at `index` in place. See [`Self::mutated_params`].
    pub fn method_mutates_param(&self, method: &str, index: usize) -> bool {
        self.mutated_params.get(method).is_some_and(|s| s.contains(&index))
    }

    /// Whether `name` is a toplevel method the analyzed file can call bare —
    /// a toplevel `def` in any file (the reference's project-seeded
    /// `top_level_def_for`), an `Object`-owner call-introduced method
    /// surviving `subtract_def_methods`, OR an `Object`-owner `def`/`macro`
    /// this file declares itself (`file_defs[file].toplevel` — the per-file
    /// half of `source_declared_method?`'s `Object` read). `file` is the
    /// analyzed file's [`rigor_parse::FileKey`]; `None` consults the union
    /// over every file.
    pub fn is_toplevel_def(&self, file: Option<&FileKey>, name: &str) -> bool {
        if self.toplevel_defs.contains(name) {
            return true;
        }
        match file.and_then(|k| self.file_index.get(k)).copied() {
            Some(i) => self.file_defs[i].toplevel.contains(name),
            None => self
                .file_defs
                .iter()
                .any(|fd| fd.toplevel.contains(name)),
        }
    }

    /// rigor-rs#140: whether the project defines a method named `method`
    /// ANYWHERE, in any `def` form — the flattened union of
    /// [`Self::defined_method_names`]. Deliberately coarse, exactly like the
    /// reference's `project_defines_anywhere?` (`block_call_timing.rb`); the
    /// caller ORs it with [`Self::is_toplevel_def`] for the full gate.
    pub fn project_defines_method_name(&self, method: &str) -> bool {
        self.defined_method_names.contains(method)
    }

    /// Register a name in the id registry (idempotent), returning nothing.
    pub(crate) fn register(&mut self, name: &str) {
        if !self.name_to_id.contains_key(name) {
            let id = self.names.len() as u32;
            self.names.push(name.to_string());
            self.name_to_id.insert(name.to_string(), id);
        }
    }

    /// Fold one (re)definition of a source class into the index, also registering
    /// its instance-class id.
    pub(crate) fn add_source(&mut self, name: &str, superclass: Option<String>, methods: &[String]) {
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
    /// ADR-0042 qualified surface. When this landed (`2fe6493`, 2026-07-25) that
    /// surface was knowingly WEAKER than the oracle's for a NAMESPACED *gem*
    /// class: the reference supplements the rbs gem with
    /// `data/vendored_gem_sigs/` (rubygems / cgi / nokogiri / prism / …), which
    /// rigor-rs did not vendor. Probed then: `Gem::Version.new("1.0").segments`
    /// — `segments` is declared ONLY in the reference's `rubygems_extras.rbs`, so
    /// the ORACLE WAS SILENT and an unrestricted arm fired ⇒ a false positive.
    ///
    /// ## That premise EXPIRED on 2026-07-31 — read before removing this
    ///
    /// `800b3a1` vendored `data/vendored_gem_sigs/` under
    /// `crates/rigor-index/vendor/rbs/overlay/` (every gem named above except
    /// `prism`, excluded deliberately — see that tree's `PROVENANCE.md`), so the
    /// sentence above stopped being true six days after it was written. Measured
    /// 2026-09-09 over a 651-name vocabulary per class, with a non-vacuous
    /// control: the port's surface for `Bundler`, `Bundler::Definition`,
    /// `Gem::Specification`, `Gem::Version`, `Psych::DisallowedClass` and
    /// `ENV` is COMPLETE — zero methods witnessed that the reference has — and
    /// `Gem::Version#segments` resolves here and is silent on both engines. It
    /// cannot re-open.
    ///
    /// What still holds the restriction up is a DIFFERENT and smaller fact: 26
    /// `(class, method)` holes where a late overlay reopen (`module Kernel` in
    /// `overlay/rbs_shims/rubygems.rbs`, merged last so upstream wins) is not
    /// carried to a subclass by the qualified ancestor flattening — e.g.
    /// `("Gem::Dependency", "gem")` answers true and `("Bundler::Dependency",
    /// "gem")` false. Removing this restriction before those are fixed fires on
    /// `Bundler::Dependency.new("a","b").gem("x")`, which the oracle is silent
    /// on. Tracked as rigor-rs#123; the 7 gap rows this would then close, their
    /// must-still-fire controls and the counted prize are in
    /// `docs/notes/20260909-declared-unwitnessed-gem-classes.md`.
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
    /// Remove this restriction once rigor-rs#123's ancestor-closure holes are
    /// closed — NOT merely "when rigor-rs vendors the gem-sig extras", which it
    /// has done since 2026-07-31.
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

    /// `discovered_method_through_ancestors?` (upstream `check_rules.rb`
    /// `ancestry_declares_method?`, asked late from
    /// `last_resort_surface_answers?`): whether the instance method `method`
    /// is project-declared on `class_name` OR on any project ancestor —
    /// included / prepended modules first, then the superclass, in
    /// [`Self::override_ancestor_names`]'s MRO order. `file` threads the same
    /// per-file contract as [`Self::project_declares_method`]: a cross-file
    /// `def` on an ancestor stays invisible (the ADR-17 monkey-patch case)
    /// while a cross-file macro (`attr_reader` in `lib/b.rb`) still counts.
    ///
    /// Past [`OVERRIDE_ANCESTOR_WALK_LIMIT`] returns `true`, mirroring the
    /// reference exactly: budget exhaustion is uncertainty, and "not
    /// declared" would hand `call.undefined-method` a fired verdict it has
    /// no evidence for (suppression is the safe side).
    pub fn project_declares_method_through_ancestors(
        &self,
        file: Option<&FileKey>,
        class_name: &str,
        method: &str,
    ) -> bool {
        let mut queue: Vec<String> = vec![class_name.to_string()];
        let mut seen: HashSet<String> = HashSet::new();
        let mut visited = 0usize;
        while !queue.is_empty() {
            let current = queue.remove(0);
            if !seen.insert(current.clone()) {
                continue;
            }
            visited += 1;
            if visited > OVERRIDE_ANCESTOR_WALK_LIMIT {
                return true;
            }
            if self.project_declares_method(file, &current, method) {
                return true;
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
