//! ADR-17 — the `pre_eval:` patched-method registry: the port of the
//! reference's `ProjectPatchedMethods` (`project_patched_methods.rb`) and the
//! resolution step of `ProjectPatchedScanner.scan` (`resolve_aliases`,
//! `Visibility.apply`, `Mixins.resolve`) over the items
//! [`rigor_parse::pre_eval::scan_pre_eval`] reads from each configured file.
//!
//! The port does not TYPE a patched call. Every reader asks only whether a
//! patch may answer a call, and declines (stays silent) when one may. The one
//! answer that keeps a diagnostic firing is a patch Ruby refuses: a private
//! one, or a protected one called from the top level, on an explicit receiver
//! (`ProjectPatchedMethods#callable?`), which the reference reports as the
//! undefined method it is.

use std::collections::{HashMap, HashSet};

use rigor_index::CoreIndex;
use rigor_parse::pre_eval::{PatchVisibility, PreEvalItem, PreEvalScanError};

use super::SourceIndex;

/// `(class name, method name, singleton side)`.
type Key = (String, String, bool);

/// How a call reaches the method it names, for `callable?`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatchCall {
    /// No receiver, or `self`: every patch answers (`explicit: false`), and so
    /// does a reader that asks with no call at all (`call_node: nil`).
    Implicit,
    /// An explicit receiver. `toplevel` is true only where the caller's `self`
    /// is known to be `main` (an `Object`): outside every class, module,
    /// method, block and lambda body. Anywhere else a protected patch may be
    /// reachable, so it answers.
    Explicit { toplevel: bool },
}

/// The resolved registry of every `pre_eval:` file the run reads.
#[derive(Debug, Default)]
pub struct PreEvalPatches {
    /// A configured file the port could not read (one nesting deeper than the
    /// analysis bound): the reference reads it, so nothing here can say what
    /// it patches, and every question answers "a patch may answer".
    opaque: bool,
    /// `ProjectPatchedMethods#by_key`, last declaration wins.
    by_key: HashMap<Key, PatchVisibility>,
    /// Every method name in `by_key`.
    names: HashSet<String>,
    /// Each owner's `(prepended, included)` modules, the most recent first.
    mixins_of: HashMap<String, (Vec<String>, Vec<String>)>,
    /// `ProjectPatchedMethods#overrides`: a named visibility call on a method
    /// the class inherits rather than binds.
    overrides: HashSet<Key>,
}

impl PreEvalPatches {
    /// Resolve the scans of the configured files, in configuration order
    /// (`ProjectPatchedScanner.scan`).
    pub fn build(files: Vec<Result<Vec<PreEvalItem>, PreEvalScanError>>) -> Self {
        let mut out = PreEvalPatches::default();
        // `bound` of `resolve_aliases`. The registry's `by_key` is the last
        // binding of each key, which is exactly what `bound` holds once every
        // file has run: a visibility change rewrites the binding in place,
        // and a later file's binding replaces an earlier one.
        let mut bound: HashMap<Key, PatchVisibility> = HashMap::new();
        let mut pending: Vec<(String, String, bool, Vec<String>)> = Vec::new();
        for file in files {
            let items = match file {
                Ok(items) => items,
                Err(PreEvalScanError::Syntax) => continue,
                Err(PreEvalScanError::TooDeep) => {
                    out.opaque = true;
                    continue;
                }
            };
            for item in items {
                match item {
                    PreEvalItem::Mixin { owner, module_name, prepend, prefix } => {
                        pending.push((owner, module_name, prepend, prefix));
                    }
                    PreEvalItem::VisibilityCall { class_name, names, singleton, visibility } => {
                        // `Visibility.apply`.
                        for name in names {
                            let key = (class_name.clone(), name.clone(), singleton);
                            if !bound.contains_key(&key) {
                                out.overrides.insert(key);
                                continue;
                            }
                            if visibility == PatchVisibility::ModuleFunction {
                                bound.insert(key, PatchVisibility::Private);
                                if !singleton {
                                    bound.insert((class_name.clone(), name, true), PatchVisibility::Public);
                                }
                            } else {
                                bound.insert(key, visibility);
                            }
                        }
                    }
                    PreEvalItem::Alias { class_name, new_name, old_name, singleton, .. } => {
                        // `alias_entry`: the old name's visibility when a
                        // patch binds it, else public.
                        let visibility = bound
                            .get(&(class_name.clone(), old_name, singleton))
                            .copied()
                            .unwrap_or(PatchVisibility::Public);
                        bound.insert((class_name, new_name, singleton), visibility);
                    }
                    PreEvalItem::Def { class_name, method, singleton, visibility, guarded, .. } => {
                        let key = (class_name, method, singleton);
                        if guarded && bound.contains_key(&key) {
                            continue;
                        }
                        bound.insert(key, visibility);
                    }
                }
            }
        }
        // `Mixins.resolve`: a module name written inside a body resolves
        // lexically against the names the patch files open or mix into.
        let mut known: HashSet<String> = bound.keys().map(|k| k.0.clone()).collect();
        known.extend(pending.iter().map(|p| p.0.clone()));
        for (owner, module_name, prepend, prefix) in pending {
            let resolved = (1..=prefix.len())
                .rev()
                .map(|depth| format!("{}::{module_name}", prefix[..depth].join("::")))
                .find(|c| known.contains(c))
                .unwrap_or(module_name);
            let edges = out.mixins_of.entry(owner).or_default();
            let list = if prepend { &mut edges.0 } else { &mut edges.1 };
            list.insert(0, resolved);
        }
        out.names = bound.keys().map(|k| k.1.clone()).collect();
        out.by_key = bound;
        out
    }

    /// Whether no configured file patches anything (and none was unreadable).
    pub fn is_empty(&self) -> bool {
        !self.opaque && self.by_key.is_empty()
    }

    /// Whether some patch defines a method named `method`, on any owner and
    /// either side (`by_key.any? { name == method }`). An unreadable file may.
    pub fn defines_name(&self, method: &str) -> bool {
        self.opaque || self.names.contains(method)
    }

    /// Whether a patch defines `method` on one of `owners`' instance side or
    /// singleton side, with no ancestor walk (`ProjectPatchedMethods#lookup`
    /// asked of each owner).
    pub fn defines_on(&self, owners: &[&str], method: &str, singleton: bool) -> bool {
        if self.opaque {
            return true;
        }
        if !self.names.contains(method) {
            return false;
        }
        owners
            .iter()
            .any(|o| self.by_key.contains_key(&(o.to_string(), method.to_string(), singleton)))
    }

    /// `ancestor_keys`: the owners a `singleton` / instance receiver of
    /// `class_name` reaches, with every module the patches mix into an
    /// instance-side owner (transitively). `None` when the class is RBS-known
    /// but its chain is not fully loaded.
    fn ancestor_keys(&self, core: &CoreIndex, class_name: &str, singleton: bool) -> Option<Vec<(String, bool)>> {
        let pairs: Vec<(String, bool)> = if singleton {
            match core.singleton_ancestor_owners(class_name) {
                Some(v) => v.into_iter().map(|(n, s)| (n.to_string(), s)).collect(),
                None if core.knows_class(class_name) => return None,
                None => vec![(class_name.to_string(), true)],
            }
        } else {
            match core.ancestor_names(class_name) {
                Some(v) => {
                    let mut v: Vec<(String, bool)> = v.into_iter().map(|n| (n.to_string(), false)).collect();
                    if !v.iter().any(|(n, _)| n == class_name) {
                        v.insert(0, (class_name.to_string(), false));
                    }
                    v
                }
                None if core.knows_class(class_name) => return None,
                None => vec![(class_name.to_string(), false)],
            }
        };
        if self.mixins_of.is_empty() {
            return Some(pairs);
        }
        // `with_mixins`. The verdict below does not read the walk's order, so
        // the set of owners is all that has to match.
        let mut out = pairs;
        let mut seen: HashSet<String> = out.iter().filter(|p| !p.1).map(|p| p.0.clone()).collect();
        let mut queue: Vec<String> = seen.iter().cloned().collect();
        while let Some(owner) = queue.pop() {
            if let Some((pre, inc)) = self.mixins_of.get(&owner) {
                for m in pre.iter().chain(inc.iter()) {
                    if seen.insert(m.clone()) {
                        out.push((m.clone(), false));
                        queue.push(m.clone());
                    }
                }
            }
        }
        Some(out)
    }

    /// Whether a `pre_eval:` patch may answer `method` on a receiver of
    /// `class_name` (`singleton`: the class object) — the reference's
    /// `source_declared_method?` patch half and
    /// `project_patched_through_ancestors?` together, as
    /// `try_project_patched_method` answers them.
    ///
    /// The reference returns the NEAREST patch on the walk and refuses the
    /// call only when that patch and every farther definer refuse it
    /// (`callable?`'s `farther` read). That is "every patch on the walk
    /// refuses the call", which does not depend on the walk's order, so the
    /// port answers it over the set of owners. A visibility override on the
    /// walk, an incomplete chain or an unreadable file answers "may answer".
    pub fn may_answer(&self, core: &CoreIndex, class_name: &str, method: &str, singleton: bool, call: PatchCall) -> bool {
        if self.opaque {
            return true;
        }
        if !self.names.contains(method) {
            return false;
        }
        let Some(chain) = self.ancestor_keys(core, class_name, singleton) else {
            return true;
        };
        for (owner, side) in &chain {
            let key = (owner.clone(), method.to_string(), *side);
            if self.overrides.contains(&key) {
                return true;
            }
            if let Some(&visibility) = self.by_key.get(&key) {
                if self.callable(core, visibility, owner, call) {
                    return true;
                }
            }
        }
        false
    }

    /// `ProjectPatchedMethods#callable?` for one patch.
    fn callable(&self, core: &CoreIndex, visibility: PatchVisibility, owner: &str, call: PatchCall) -> bool {
        match (call, visibility) {
            (PatchCall::Implicit, _) | (_, PatchVisibility::Public) => true,
            (_, PatchVisibility::Private | PatchVisibility::ModuleFunction) => false,
            (PatchCall::Explicit { toplevel: false }, PatchVisibility::Protected) => true,
            // `self_kind_of?` with `main`'s `Object`.
            (PatchCall::Explicit { toplevel: true }, PatchVisibility::Protected) => {
                match self.ancestor_keys(core, "Object", false) {
                    Some(keys) => keys.iter().any(|(n, s)| !s && n == owner),
                    None => true,
                }
            }
        }
    }
}

impl SourceIndex {
    /// Install the run's `pre_eval:` registry (empty when the project lists
    /// no `pre_eval:` file).
    pub fn set_pre_eval_patches(&mut self, patches: Option<std::sync::Arc<PreEvalPatches>>) {
        self.pre_eval_patches = patches.filter(|p| !p.is_empty());
    }

    /// The run's `pre_eval:` registry, when a configured file patches anything.
    pub fn pre_eval_patches(&self) -> Option<&PreEvalPatches> {
        self.pre_eval_patches.as_deref()
    }

    /// Whether a `pre_eval:` patch may answer `method` on a receiver of
    /// `class_name` ([`PreEvalPatches::may_answer`]). False with no registry.
    pub fn pre_eval_may_answer(
        &self,
        core: &CoreIndex,
        class_name: &str,
        method: &str,
        singleton: bool,
        call: PatchCall,
    ) -> bool {
        self.pre_eval_patches()
            .is_some_and(|p| p.may_answer(core, class_name, method, singleton, call))
    }

    /// Whether a `pre_eval:` patch defines a method named `method` anywhere
    /// ([`PreEvalPatches::defines_name`]). False with no registry.
    pub fn pre_eval_defines_name(&self, method: &str) -> bool {
        self.pre_eval_patches().is_some_and(|p| p.defines_name(method))
    }

    /// Whether a `pre_eval:` patch defines `method` on one of `owners`
    /// ([`PreEvalPatches::defines_on`]). False with no registry.
    pub fn pre_eval_defines_on(&self, owners: &[&str], method: &str, singleton: bool) -> bool {
        self.pre_eval_patches().is_some_and(|p| p.defines_on(owners, method, singleton))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rigor_parse::pre_eval::scan_pre_eval;

    fn patches(src: &str) -> PreEvalPatches {
        PreEvalPatches::build(vec![scan_pre_eval(src.as_bytes())])
    }

    #[test]
    fn last_binding_and_alias_visibility() {
        let p = patches(
            "class Integer\n  def foo = 1\n  private :foo\n  alias_method :foo, :succ\n  def bar = 1\n  alias_method :bar_alias, :bar\n  private :bar_alias\nend\n",
        );
        assert_eq!(p.by_key.get(&("Integer".into(), "foo".into(), false)), Some(&PatchVisibility::Public));
        assert_eq!(p.by_key.get(&("Integer".into(), "bar".into(), false)), Some(&PatchVisibility::Public));
        assert_eq!(p.by_key.get(&("Integer".into(), "bar_alias".into(), false)), Some(&PatchVisibility::Private));
    }

    #[test]
    fn module_function_named_call_binds_a_public_singleton_copy() {
        let p = patches("module Kernel; def mf2 = 1; module_function :mf2; end\n");
        assert_eq!(p.by_key.get(&("Kernel".into(), "mf2".into(), false)), Some(&PatchVisibility::Private));
        assert_eq!(p.by_key.get(&("Kernel".into(), "mf2".into(), true)), Some(&PatchVisibility::Public));
    }

    #[test]
    fn inherited_visibility_call_is_an_override() {
        let p = patches("module Kernel; private def kpv = 1; end\nclass Integer; public :kpv; end\n");
        assert!(p.overrides.contains(&("Integer".into(), "kpv".into(), false)));
    }

    #[test]
    fn guarded_def_does_not_rebind() {
        let p = PreEvalPatches::build(vec![
            scan_pre_eval(b"class String; private def gg = 1; end\n"),
            scan_pre_eval(b"class String; def gg = 1 unless method_defined?(:gg); end\n"),
        ]);
        assert_eq!(p.by_key.get(&("String".into(), "gg".into(), false)), Some(&PatchVisibility::Private));
    }

    #[test]
    fn mixin_names_resolve_lexically() {
        let p = patches("module Ext; module NumExt2; def ne = 1; end; end\nmodule Ext; Integer.include(NumExt2); end\n");
        assert_eq!(p.mixins_of.get("Integer"), Some(&(vec![], vec!["Ext::NumExt2".to_string()])));
    }

    #[test]
    fn syntax_error_file_patches_nothing() {
        let p = patches("class String; def pe1 = 1; end\ndef (\n");
        assert!(p.is_empty());
    }
}
