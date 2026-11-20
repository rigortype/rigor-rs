//! The decline-side reach questions a refinement activation asks —
//! rigor-rs#446's port of the reference's `RefinedDispatch::InEffect` +
//! `OwnMethod` + `in_effect_modules` (`inference/refined_dispatch*.rb`,
//! `inference/in_effect_refinements.rb`):
//!
//! * [`SourceIndex::expand_using`] — `activated_modules`: the module set a
//!   `using`/`refined` of `name` puts in effect — `name`'s transitive
//!   include/prepend closure over project-known modules, with the
//!   reference's two unreadable outcomes (`unknown_refinement`, `opaque`).
//! * [`SourceIndex::refining_modules`] — which project's modules hold a
//!   refinement row for `method` on a class the receiver's lookup reaches.
//! * [`SourceIndex::own_method_answers`] — `RefinedDispatch::OwnMethod`'s
//!   proven-`true` answer: the receiver's own ancestry answers `method`
//!   before any in-effect refinement reaches it. Every place the port
//!   cannot prove it, this answers `false` — the reference's decline.

use std::collections::{HashMap, HashSet, VecDeque};

use rigor_index::CoreIndex;

use super::resolution_chain::ChainEntry;
use super::SourceIndex;

/// The expansion outcome [`SourceIndex::expand_using`] reports — the three
/// `activated_modules` answers (`in_effect_refinements.rb`).
pub enum UsingExpansion {
    /// The expanded in-effect module set (always non-empty: `module`
    /// itself is first).
    Modules(Vec<String>),
    /// The chain could not be fully read — the activation enters as
    /// `unknown_refinement` (a wildcard mixin marker or a truncated
    /// closure). Declines the redefinition rules outright, and the
    /// call-name rules wherever [`SourceIndex::refining_modules`] is
    /// non-empty.
    Unknown,
    /// A member of the closure names a module the project does not
    /// declare and core/stdlib does not know — the reference's
    /// `opaque_module?`: declines every refinement-declined rule inside
    /// the region.
    Opaque,
}

/// `activated_modules`' closure cap — the reference's `CHAIN_LIMIT`.
const USING_CHAIN_LIMIT: usize = 50;

impl SourceIndex {
    /// `activated_modules(scope, name)`: the modules `using name` (or
    /// `expr.refined(name)`) puts in effect — `name`'s include/prepend
    /// closure over project entries. A project-declared chain the walk
    /// cannot fully read answers [`UsingExpansion::Unknown`]; a member
    /// naming a module neither the project nor core/stdlib knows answers
    /// [`UsingExpansion::Opaque`].
    pub fn expand_using(&self, core: &CoreIndex, module: &str) -> UsingExpansion {
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        out.push(module.to_string());
        seen.insert(module.to_string());
        queue.push_back(module.to_string());
        let mut opaque = false;
        while let Some(m) = queue.pop_front() {
            if out.len() > USING_CHAIN_LIMIT {
                return UsingExpansion::Unknown;
            }
            let Some(entry) = self.override_classes.get(&m) else {
                continue;
            };
            for raw in &entry.includes {
                // A wildcard mixin marker (`include *x`) makes the chain
                // unreadable — `chain.wildcard_mixin?`.
                if raw == "*" || raw == "?" {
                    return UsingExpansion::Unknown;
                }
                if let Some(q) = self.resolve_override_ancestor(&m, raw) {
                    if seen.insert(q.clone()) {
                        queue.push_back(q.clone());
                        out.push(q);
                    }
                    continue;
                }
                // An external spelling: a candidate the project declares,
                // or core/stdlib knows, is dropped from the set; anything
                // else is a module the walk cannot name — opaque.
                if !external_mixin_known(self, core, &m, raw) {
                    opaque = true;
                }
            }
        }
        if opaque {
            UsingExpansion::Opaque
        } else {
            UsingExpansion::Modules(out)
        }
    }

    /// Whether a `*`-charged targets-wildcard refinement row exists
    /// anywhere in the project — `{*:{*:[*]}}`, which makes every
    /// in-effect module opaque (`opaque_module?` reads `dig(*,*)`).
    pub fn refinement_global_wildcard(&self) -> bool {
        self.refinement_wildcard
    }

    /// The modules a refinement activation must consult —
    /// `RefinedDispatch.refining_modules`: every module whose refinement
    /// table records `method` (or a names-wildcard row) for a target the
    /// receiver's lookup reaches. `singleton` asks for the class-object
    /// side (`ClassObject.reach`).
    pub fn refining_modules(
        &self,
        core: &CoreIndex,
        class: &str,
        method: &str,
        singleton: bool,
    ) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (module, entry) in &self.override_classes {
            for row in &entry.refines {
                if !(row.names_wildcard || row.names.iter().any(|n| n == method)) {
                    continue;
                }
                let reaches = match &row.target {
                    // The class-wildcard key reaches every receiver.
                    None => true,
                    Some(raw) if singleton => {
                        self.class_object_target_reaches(core, raw, module, class)
                    }
                    Some(raw) => {
                        let target = self
                            .resolve_declared_constant_at(&split_qualified_owner(module), raw)
                            .unwrap_or_else(|| raw.clone());
                        target == class
                            || self
                                .level_names(core, class)
                                .map(|levels| levels.iter().any(|l| *l == target))
                                .unwrap_or(false)
                    }
                };
                if reaches && !out.iter().any(|m| m == module) {
                    out.push(module.clone());
                }
            }
        }
        out
    }

    /// `ClassObject.reach` for a `refine` target's written spelling — the
    /// `refined_class_object_method` target test, shared so the
    /// refinement-driver and the flow path agree.
    fn class_object_target_reaches(
        &self,
        core: &CoreIndex,
        target: &str,
        module: &str,
        class: &str,
    ) -> bool {
        if ["Object", "Module", "Kernel", "BasicObject"].contains(&target) {
            return true;
        }
        if target == "Class" {
            // `refine(Class)` reaches class objects, not module objects.
            return !(core.is_module(class) || core.is_qualified_module(class));
        }
        // A module the class object may extend; an unknown constant may
        // be one.
        let known_not_module = match self
            .resolve_declared_constant_at(&split_qualified_owner(module), target)
        {
            Some(q) => self.project_declares_module(&q) == Some(false),
            None => core.knows_class(target) || core.knows_qualified_class(target),
        };
        !known_not_module
    }

    /// The class's lookup LEVELS — `ChainWalk::levels` / `ClassObject`'s:
    /// project ancestry where the project declares the class, core
    /// `ancestor_names` otherwise. `None` when the chain cannot be fully
    /// read.
    pub(crate) fn level_names(&self, core: &CoreIndex, class: &str) -> Option<Vec<String>> {
        if self.override_classes.contains_key(class) {
            let chain = self.instance_chain(class)?;
            let mut names: Vec<String> = Vec::new();
            for entry in chain.entries {
                match entry {
                    ChainEntry::Project(name) => names.push(name),
                    ChainEntry::External { raw, from } => {
                        let name = self
                            .resolve_override_ancestor(&from, &raw)
                            .unwrap_or_else(|| raw.clone());
                        if !names.iter().any(|n| *n == name) {
                            names.push(name.clone());
                        }
                        // An external's own ancestors come with it —
                        // `refine(Object)` reaches every class.
                        if let Some(ancestors) = core.ancestor_names(&name) {
                            for a in ancestors {
                                if !names.iter().any(|n| n == a) {
                                    names.push(a.to_string());
                                }
                            }
                        }
                    }
                }
            }
            Some(names)
        } else {
            core.ancestor_names(class)
                .map(|ancestors| ancestors.iter().map(|a| a.to_string()).collect())
        }
    }

    /// `RefinedDispatch::OwnMethod`'s proven `answers?` — `true` only when
    /// the receiver's own lookup provably answers `method` before any
    /// in-effect refinement reaches it. `raw_targets` are the refined
    /// classes the in-effect rows name — both the written spellings and
    /// their resolved forms.
    ///
    /// `false` — the caller's decline — on the reference's every
    /// conservative outcome: no in-effect row names the method; a
    /// wildcard/unreadable key reaches; a refined level sits at or before
    /// the first definer; a raw module off the receiver's levels may be
    /// `extend`ed in ahead; a raw class off the levels cannot be placed;
    /// a project subclass's own level is refined or unreadable; or the
    /// port's chain cannot prove the first definer.
    pub fn own_method_answers(
        &self,
        core: &CoreIndex,
        class: &str,
        method: &str,
        raw_targets: &[String],
    ) -> bool {
        if raw_targets.is_empty() {
            return true;
        }
        let targets: Vec<String> = raw_targets
            .iter()
            .map(|t| {
                self.resolve_declared_constant_at(&[], t)
                    .unwrap_or_else(|| t.clone())
            })
            .collect();

        let Some(levels) = self.level_names(core, class) else {
            return false;
        };

        // The proven first definer of `method` through `class`'s chain —
        // `None` is `defining_level`'s nil PLUS every case the port cannot
        // prove, so the checks below stay decline-side only.
        let definer = self.settled_definer(core, class, method, false, |n| {
            self.override_classes.get(n).is_some_and(|e| {
                e.methods.contains(method) || e.method_visibilities.contains_key(method)
            })
        });
        match &definer {
            Some(d) => {
                let Some(pos) = levels.iter().position(|l| l == d) else {
                    return false;
                };
                // A refined level at or before the first proven definer
                // takes the dispatch.
                if levels[..pos].iter().any(|l| targets.contains(l)) {
                    return false;
                }
            }
            None => {
                // No proven definer: the own level — the class and its
                // prepended mixins — must not itself be refined.
                let own_level_end = levels
                    .iter()
                    .position(|l| l == class)
                    .map(|p| p + 1)
                    .unwrap_or(1);
                if levels[..own_level_end.min(levels.len())]
                    .iter()
                    .any(|l| targets.contains(l))
                {
                    return false;
                }
            }
        }

        // `refined_off_levels?` — a raw target off the receiver's levels:
        // a module may be `extend`ed in ahead (decline); a class cannot
        // reach it unless `placed_away?` fails — the receiver's class
        // chain did not reach `Object` (project chain), or the target's
        // own ancestors hold `class`.
        for target in &targets {
            if levels.iter().any(|l| l == target) {
                continue;
            }
            if matches!(
                core.class_ordering(target, class),
                rigor_index::ClassOrdering::Subclass
            ) {
                // The target sits under the receiver — it cannot reach.
                continue;
            }
            if target_is_moduleish(self, core, target) {
                return false;
            }
            if self.override_classes.contains_key(class)
                && levels.last().map(|l| l.as_str()) != Some("Object")
            {
                return false;
            }
            match core.ancestor_names(target) {
                Some(ancestors) if ancestors.iter().any(|a| *a == class) => {
                    return false;
                }
                None if !core.knows_class(target) && !core.knows_qualified_class(target) => {
                    return false;
                }
                _ => {}
            }
        }

        // `subclass_refined?` — a project subclass's own level may sit on
        // a refined target or be unreadable while a module is in effect.
        if self.subclass_refined(core, class, raw_targets, &targets) {
            return false;
        }
        true
    }

    /// `subclass_refined?`: a project subclass of `class` whose own level
    /// — the class itself and its direct mixins — is a raw in-effect
    /// target, or whose level cannot be read while a module target is.
    fn subclass_refined(
        &self,
        core: &CoreIndex,
        class: &str,
        raw_targets: &[String],
        resolved: &[String],
    ) -> bool {
        for sub in self.project_subclass_names(class) {
            let mut own_level: Vec<String> = vec![sub.clone()];
            if let Some(entry) = self.override_classes.get(&sub) {
                for raw in &entry.includes {
                    own_level.push(
                        self.resolve_override_ancestor(&sub, raw)
                            .unwrap_or_else(|| raw.clone()),
                    );
                }
            }
            if own_level
                .iter()
                .any(|n| raw_targets.contains(n) || resolved.contains(n))
            {
                return true;
            }
            if self.instance_chain(&sub).is_none()
                && raw_targets.iter().any(|t| target_is_moduleish(self, core, t))
            {
                return true;
            }
        }
        false
    }

    /// The project subclass names of `class` — the same children map
    /// `project_subclass_may_define` walks.
    fn project_subclass_names(&self, class: &str) -> Vec<String> {
        let mut children: HashMap<String, Vec<&str>> = HashMap::new();
        for (child, entry) in &self.override_classes {
            let Some(raw) = entry.superclass.as_deref() else {
                continue;
            };
            let raw = raw.strip_prefix("::").unwrap_or(raw);
            let segments: Vec<&str> = child.split("::").collect();
            let parent = (0..segments.len())
                .rev()
                .map(|i| {
                    if i == 0 {
                        raw.to_string()
                    } else {
                        format!("{}::{}", segments[..i].join("::"), raw)
                    }
                })
                .find(|c| self.known_user(c))
                .unwrap_or_else(|| raw.to_string());
            children.entry(parent).or_default().push(child.as_str());
        }
        let mut out: Vec<String> = Vec::new();
        let mut queue: Vec<&str> = children.get(class).cloned().unwrap_or_default();
        let mut seen: HashSet<&str> = HashSet::new();
        while let Some(sub) = queue.pop() {
            if !seen.insert(sub) {
                continue;
            }
            out.push(sub.to_string());
            queue.extend(children.get(sub).into_iter().flatten().copied());
        }
        out
    }
}

/// The lexical candidates `raw` could name from inside `of` — each
/// enclosing segment joined, longest first, then the bare name
/// (`constant_receiver_candidates`).
fn lexical_candidates(of: &str, raw: &str) -> Vec<String> {
    let segments: Vec<&str> = of.split("::").collect();
    let mut out = Vec::new();
    for i in (0..segments.len()).rev() {
        out.push(format!("{}::{}", segments[..=i].join("::"), raw));
    }
    out.push(raw.to_string());
    out
}

/// `!modul?`/`opaque` — whether an `include`'s unresolvable spelling names
/// something the project or core/stdlib knows (then it is dropped from
/// the expansion), or nothing (then the expansion is opaque).
fn external_mixin_known(idx: &SourceIndex, core: &CoreIndex, of: &str, raw: &str) -> bool {
    lexical_candidates(of, raw).iter().any(|candidate| {
        idx.declares_constant_at(&[], candidate)
            || core.knows_qualified_class(candidate)
            || core.knows_class(candidate)
            || core.is_module(candidate)
            || core.is_qualified_module(candidate)
    })
}

/// The owner-module's scope segments — `refine(X)` inside `module A::M`
/// resolves `X` against `A::M` first.
fn split_qualified_owner(module: &str) -> Vec<String> {
    module.split("::").map(str::to_string).collect()
}

/// `modul?` — `target` names a module per the project or the frozen core.
/// An unreadable spelling may be a module (`extend` may put it ahead), so
/// it counts — the decline direction.
fn target_is_moduleish(idx: &SourceIndex, core: &CoreIndex, target: &str) -> bool {
    match idx.project_declares_module(target) {
        Some(v) => return v,
        None => {}
    }
    if core.is_module(target) || core.is_qualified_module(target) {
        return true;
    }
    !(core.knows_class(target) || core.knows_qualified_class(target))
}
