//! `SigEnv` — a sig-gen-local, **FQN-keyed** declaration environment built from
//! the project's own `.rbs` files (ADR-14 slice 10, generation-time env
//! classification).
//!
//! ## Why a separate env and not `CoreIndex`
//!
//! The reference gates its classify chain on `env.class_decls.key?(TypeName)` — a
//! **fully-qualified** key over the loaded RBS env — and then resolves the
//! method's declared return through the RBS ancestor chain. rigor-rs's
//! [`CoreIndex`] cannot answer that gate: `rigor-index` keys every class by its
//! **short name** and `Builder::merge` FOLDS distinct same-short-name classes
//! (`M::Foo` and `N::Foo`) into ONE entry, so `knows_class("Rigor::SigGen::ObservedCall")`
//! is always false and a nested class's declared return is unrecoverable. This
//! module therefore keeps its own FQN-keyed table of the PROJECT signatures and
//! delegates only the CORE/stdlib ancestor tail to the three additive,
//! sig-gen-only accessors on [`CoreIndex`] (`declared_instance_return` /
//! `declared_singleton_return` / `chain_complete`). The `check` path is untouched
//! by construction.
//!
//! ## Three-valued lookup (the classify contract)
//!
//! [`Lookup`] is deliberately three-valued, matching the reference's
//! `lookup_existing_method` → `compare_against_declared` split:
//!
//! - [`Lookup::NotDeclared`] ⇒ the class is absent from the env, or the method is
//!   absent from a fully-loaded ancestor chain ⇒ sig-gen emits `# [new]`.
//! - [`Lookup::Declared(None)`](Lookup::Declared) ⇒ declared but the return is not
//!   a single bare concrete class (union / optional / untyped / multi-overload /
//!   generic), OR the ancestor chain is INCOMPLETE (an unknown superclass such as
//!   `< ApplicationRecord` with no Rails sig) ⇒ sig-gen DROPs. The incomplete-chain
//!   DROP is deliberately conservative: the reference, whose env is complete,
//!   might find a declaration, and emitting `# [new]` there would be a
//!   shared-method TAG MISMATCH — the hard-guarantee break. A DROP is a silent
//!   under-emit.
//! - [`Lookup::Declared(Some(c))`](Lookup::Declared) ⇒ declared, resolvable return
//!   class `c` ⇒ sig-gen compares (tighter / equivalent).
//!
//! ## Divergence from the reference (recorded per ADR-79)
//!
//! The reference builds ONE whole-env RBS `Environment`; a malformed project sig
//! makes that build raise, and EVERY candidate then degrades to `new_method` with
//! one stderr warning. rigor-rs isolates parse failures **per file** (ADR-0016):
//! a malformed `.rbs` drops only its own declarations, the rest of the env stands.
//! This is a deliberate divergence (ADR-79 records the analogous
//! vendor-vs-project-`rbs` decision) — an under-emit at worst, never a wrong byte.

use std::collections::{HashMap, HashSet};

use ruby_rbs::node::{AttributeKind, MethodDefinitionKind, Node as RbsNode};

use rigor_index::CoreIndex;

use super::{collect_rbs_files, decl_full_name};

/// The five base classes a class OBJECT is itself an instance of — its singleton
/// surface inherits their INSTANCE methods (so `Foo.hash` resolves to
/// `Object#hash`). Mirrors `CoreData::declared_singleton_return`'s base set.
const SINGLETON_BASES: [&str; 5] = ["Class", "Module", "Object", "Kernel", "BasicObject"];

/// One class/module declaration harvested from the project's `.rbs`.
struct Decl {
    /// `true` for a `class`, `false` for a `module`. Only a class's instance
    /// chain implicitly continues to `Object`; a module's does not (matching
    /// RBS `DefinitionBuilder#build_instance`).
    is_class: bool,
    /// The superclass FQN as resolved against the decl's namespace, or `None`
    /// when none was written.
    superclass: Option<String>,
    /// Included module FQNs (resolved).
    includes: Vec<String>,
    /// `extend`ed module FQNs (resolved) — their INSTANCE methods fold into this
    /// class object's SINGLETON surface.
    extends: Vec<String>,
    /// Instance method/attr name → resolved return class (`None` = declared but
    /// not a single bare concrete class ⇒ DROP).
    instance: HashMap<String, Option<String>>,
    /// Singleton (`def self.x`) name → resolved return class.
    singleton: HashMap<String, Option<String>>,
}

/// The outcome of resolving a `(class, method, kind)` against the project env +
/// core tail. See the module docs for the classify contract.
pub enum Lookup {
    /// Not declared on a complete chain ⇒ emit `# [new]`.
    NotDeclared,
    /// Declared: `Some(c)` ⇒ compare against `c`; `None` ⇒ DROP (unresolvable
    /// return, or incomplete chain).
    Declared(Option<String>),
}

/// The result of a single ancestor-resolution step.
enum Res {
    /// The method was found; the payload is its resolved return class (`None` =
    /// declared but unresolvable).
    Found(Option<String>),
    /// The method was not found; `complete` is whether the traversed sub-chain
    /// was fully loaded.
    Absent { complete: bool },
}

/// A raw, pre-resolution reference name plus the namespace prefix it was written
/// in, so `class Foo < Base` inside `module M` can resolve `Base` to `M::Base`.
struct RawRef {
    written: String,
    prefix: Vec<String>,
}

/// The sig-gen-local FQN-keyed declaration env.
pub struct SigEnv {
    decls: HashMap<String, Decl>,
}

impl SigEnv {
    /// Build from the project's signature directories (`cfg.all_signature_dirs`).
    /// Per-file parse isolation (ADR-0016): an unreadable/unparseable `.rbs`
    /// drops only its own declarations. A SORTED walk gives first-found-wins on a
    /// duplicated decl.
    pub fn build(signature_dirs: &[std::path::PathBuf]) -> Self {
        // Phase 1: harvest every decl with RAW reference names + its prefix.
        let mut decls: HashMap<String, Decl> = HashMap::new();
        let mut raw_supers: HashMap<String, RawRef> = HashMap::new();
        let mut raw_includes: HashMap<String, Vec<RawRef>> = HashMap::new();
        let mut raw_extends: HashMap<String, Vec<RawRef>> = HashMap::new();

        for dir in signature_dirs {
            if !dir.is_dir() {
                continue;
            }
            let mut files: Vec<std::path::PathBuf> = Vec::new();
            collect_rbs_files(dir, &mut files);
            files.sort();
            for f in files {
                let Ok(src) = std::fs::read_to_string(&f) else { continue };
                let Ok(sig) = ruby_rbs::node::parse(&src) else { continue };
                harvest_decls(
                    &src,
                    sig.declarations().iter(),
                    &[],
                    &mut decls,
                    &mut raw_supers,
                    &mut raw_includes,
                    &mut raw_extends,
                );
            }
        }

        // Phase 2: resolve reference names to FQNs now the whole keyset is known.
        let keyset: HashSet<String> = decls.keys().cloned().collect();
        for (fqn, decl) in decls.iter_mut() {
            if let Some(r) = raw_supers.get(fqn) {
                decl.superclass = Some(resolve_ref(&keyset, r));
            }
            if let Some(rs) = raw_includes.get(fqn) {
                decl.includes = rs.iter().map(|r| resolve_ref(&keyset, r)).collect();
            }
            if let Some(rs) = raw_extends.get(fqn) {
                decl.extends = rs.iter().map(|r| resolve_ref(&keyset, r)).collect();
            }
        }

        SigEnv { decls }
    }

    /// Whether `class_fqn` is present in the env (a project decl OR a core
    /// toplevel class the project shadows). The env gate is on the CLASS's
    /// presence — a class absent here makes EVERY method `NotDeclared`
    /// (`# [new]`), ancestor lookup never runs (the reference's
    /// `env.class_decls.key?`).
    fn class_present(&self, core: &CoreIndex, class_fqn: &str) -> bool {
        self.decls.contains_key(class_fqn) || core.knows_toplevel_class(class_fqn)
    }

    /// Resolve `(class_fqn, method)` for a given `kind` against the project env,
    /// delegating the core/stdlib ancestor tail to `core`. See the module docs.
    pub fn lookup(
        &self,
        core: &CoreIndex,
        class_fqn: &str,
        method: &str,
        singleton: bool,
    ) -> Lookup {
        if !self.class_present(core, class_fqn) {
            return Lookup::NotDeclared;
        }
        let res = if singleton {
            self.resolve_singleton(core, class_fqn, method)
        } else {
            self.resolve_instance(core, class_fqn, method, &mut HashSet::new())
        };
        match res {
            Res::Found(ret) => Lookup::Declared(ret),
            Res::Absent { complete: true } => Lookup::NotDeclared,
            Res::Absent { complete: false } => Lookup::Declared(None),
        }
    }

    /// Walk the INSTANCE ancestor chain (own members → includes → superclass),
    /// with a class implicitly continuing to `Object`.
    fn resolve_instance(
        &self,
        core: &CoreIndex,
        name: &str,
        method: &str,
        visited: &mut HashSet<String>,
    ) -> Res {
        if !visited.insert(name.to_string()) {
            return Res::Absent { complete: true }; // cycle guard.
        }
        if let Some(decl) = self.decls.get(name) {
            if let Some(ret) = decl.instance.get(method) {
                return Res::Found(ret.clone());
            }
            let mut complete = true;
            for inc in &decl.includes {
                match self.resolve_instance(core, inc, method, visited) {
                    Res::Found(r) => return Res::Found(r),
                    Res::Absent { complete: c } => complete &= c,
                }
            }
            // A class defaults to `Object`; a module's instance chain stops.
            let sup = decl
                .superclass
                .clone()
                .or_else(|| decl.is_class.then(|| "Object".to_string()));
            if let Some(sup) = sup {
                match self.resolve_instance(core, &sup, method, visited) {
                    Res::Found(r) => return Res::Found(r),
                    Res::Absent { complete: c } => complete &= c,
                }
            }
            Res::Absent { complete }
        } else if core.knows_toplevel_class(name) {
            // A core/stdlib class: precise three-valued core lookup (the ancestor
            // chain of a core class is fully loaded).
            match core.declared_instance_return(name, method) {
                Some(ret) => Res::Found(ret.map(str::to_string)),
                None => Res::Absent { complete: true },
            }
        } else {
            // An ancestor neither in the project env nor a known core toplevel
            // class ⇒ the chain is incomplete ⇒ DROP upstream (conservative).
            Res::Absent { complete: false }
        }
    }

    /// Resolve a SINGLETON method: the project singleton chain (own `def self.x`
    /// and `extend`ed modules' instance methods, up the superclass chain), then
    /// the INSTANCE surface of the five base classes (`Class`/`Module`/…) the
    /// class object is itself an instance of — what makes an inherited
    /// `def self.hash` resolve to `Object#hash`.
    fn resolve_singleton(&self, core: &CoreIndex, class_fqn: &str, method: &str) -> Res {
        let a = self.resolve_singleton_chain(core, class_fqn, method, &mut HashSet::new());
        if let Res::Found(r) = a {
            return Res::Found(r);
        }
        // Base-class instance surface (Class/Module/Object/Kernel/BasicObject).
        for base in SINGLETON_BASES {
            if let Some(ret) = core.declared_instance_return(base, method) {
                return Res::Found(ret.map(str::to_string));
            }
        }
        let a_complete = matches!(a, Res::Absent { complete: true });
        let bases_complete = SINGLETON_BASES.iter().all(|b| core.chain_complete(b));
        Res::Absent { complete: a_complete && bases_complete }
    }

    /// Walk the project singleton superclass chain: own `def self.x` + `extend`ed
    /// modules' instance methods, then the superclass's singleton chain. Does NOT
    /// fold in the base-class surface (its caller does).
    fn resolve_singleton_chain(
        &self,
        core: &CoreIndex,
        name: &str,
        method: &str,
        visited: &mut HashSet<String>,
    ) -> Res {
        if !visited.insert(name.to_string()) {
            return Res::Absent { complete: true };
        }
        if let Some(decl) = self.decls.get(name) {
            if let Some(ret) = decl.singleton.get(method) {
                return Res::Found(ret.clone());
            }
            let mut complete = true;
            for ext in &decl.extends {
                // `extend M` folds M's INSTANCE methods into the singleton surface.
                match self.resolve_instance(core, ext, method, &mut HashSet::new()) {
                    Res::Found(r) => return Res::Found(r),
                    Res::Absent { complete: c } => complete &= c,
                }
            }
            if let Some(sup) = &decl.superclass {
                match self.resolve_singleton_chain(core, sup, method, visited) {
                    Res::Found(r) => return Res::Found(r),
                    Res::Absent { complete: c } => complete &= c,
                }
            }
            Res::Absent { complete }
        } else if core.knows_toplevel_class(name) {
            match core.declared_singleton_return(name, method) {
                Some(ret) => Res::Found(ret.map(str::to_string)),
                None => Res::Absent { complete: true },
            }
        } else {
            Res::Absent { complete: false }
        }
    }
}

/// Resolve a raw reference name against the known FQN keyset by trying, from the
/// most-nested enclosing namespace down to bare, `prefix[0..k]::written`. The
/// first candidate that is a known project FQN wins; otherwise the bare written
/// name (which the resolver then treats as a core class or an incomplete link).
fn resolve_ref(keyset: &HashSet<String>, r: &RawRef) -> String {
    // A written FQN (contains `::`) or leading-`::` absolute name is used as-is
    // when it matches; otherwise try namespace prefixes.
    for k in (0..=r.prefix.len()).rev() {
        let candidate = if k == 0 {
            r.written.clone()
        } else {
            format!("{}::{}", r.prefix[..k].join("::"), r.written)
        };
        if keyset.contains(&candidate) {
            return candidate;
        }
    }
    r.written.clone()
}

/// Harvest class/module declarations (recursively) into `decls`, recording raw
/// reference names for the phase-2 resolution pass.
#[allow(clippy::too_many_arguments)]
fn harvest_decls<'a>(
    source: &str,
    members: impl Iterator<Item = RbsNode<'a>>,
    prefix: &[String],
    decls: &mut HashMap<String, Decl>,
    raw_supers: &mut HashMap<String, RawRef>,
    raw_includes: &mut HashMap<String, Vec<RawRef>>,
    raw_extends: &mut HashMap<String, Vec<RawRef>>,
) {
    for node in members {
        let (is_class, local, members, super_ref): (bool, String, ruby_rbs::node::NodeList<'a>, Option<RawRef>) =
            match &node {
                RbsNode::Class(c) => {
                    let sup = c.super_class().map(|s| RawRef {
                        written: decl_full_name(&s.name()),
                        prefix: prefix.to_vec(),
                    });
                    (true, decl_full_name(&c.name()), c.members(), sup)
                }
                RbsNode::Module(m) => (false, decl_full_name(&m.name()), m.members(), None),
                _ => continue,
            };
        let fqn = if prefix.is_empty() {
            local.clone()
        } else {
            format!("{}::{}", prefix.join("::"), local)
        };

        let mut decl = Decl {
            is_class,
            superclass: None,
            includes: Vec::new(),
            extends: Vec::new(),
            instance: HashMap::new(),
            singleton: HashMap::new(),
        };
        let mut includes: Vec<RawRef> = Vec::new();
        let mut extends: Vec<RawRef> = Vec::new();

        collect_members(source, members.iter(), prefix, &mut decl, &mut includes, &mut extends);

        // First-found-wins on a duplicated decl (sorted walk).
        if !decls.contains_key(&fqn) {
            decls.insert(fqn.clone(), decl);
            if let Some(s) = super_ref {
                raw_supers.insert(fqn.clone(), s);
            }
            if !includes.is_empty() {
                raw_includes.insert(fqn.clone(), includes);
            }
            if !extends.is_empty() {
                raw_extends.insert(fqn.clone(), extends);
            }
        }

        // Recurse into nested decls (a fresh prefix), regardless of dedup so
        // nested decls in a later reopen still register.
        let mut child_prefix = prefix.to_vec();
        child_prefix.push(local);
        harvest_decls(
            source,
            members.iter(),
            &child_prefix,
            decls,
            raw_supers,
            raw_includes,
            raw_extends,
        );
    }
}

/// Fold a class/module's members into `decl` (method/attr returns) and collect
/// raw include/extend reference names.
fn collect_members<'a>(
    _source: &str,
    members: impl Iterator<Item = RbsNode<'a>>,
    prefix: &[String],
    decl: &mut Decl,
    includes: &mut Vec<RawRef>,
    extends: &mut Vec<RawRef>,
) {
    for member in members {
        match member {
            RbsNode::MethodDefinition(md) => {
                let name = md.name().as_str().to_string();
                let ret = resolve_method_return(&md);
                match md.kind() {
                    MethodDefinitionKind::Instance => {
                        decl.instance.entry(name).or_insert(ret);
                    }
                    MethodDefinitionKind::Singleton => {
                        decl.singleton.entry(name).or_insert(ret);
                    }
                    MethodDefinitionKind::SingletonInstance => {
                        // `def self?.x` is BOTH a class and an instance method.
                        decl.instance.entry(name.clone()).or_insert(ret.clone());
                        decl.singleton.entry(name).or_insert(ret);
                    }
                }
            }
            RbsNode::AttrReader(a) => {
                let ret = resolve_type_node_return(&a.type_());
                attr_insert(decl, a.name().as_str(), a.kind(), ret, false);
            }
            RbsNode::AttrWriter(a) => {
                let ret = resolve_type_node_return(&a.type_());
                attr_insert(decl, a.name().as_str(), a.kind(), ret, true);
            }
            RbsNode::AttrAccessor(a) => {
                let ret = resolve_type_node_return(&a.type_());
                attr_insert(decl, a.name().as_str(), a.kind(), ret.clone(), false);
                attr_insert(decl, a.name().as_str(), a.kind(), ret, true);
            }
            RbsNode::Include(inc) => {
                includes.push(RawRef {
                    written: decl_full_name(&inc.name()),
                    prefix: prefix.to_vec(),
                });
            }
            RbsNode::Extend(ext) => {
                extends.push(RawRef {
                    written: decl_full_name(&ext.name()),
                    prefix: prefix.to_vec(),
                });
            }
            _ => {}
        }
    }
}

/// Insert an attr member: `attr_reader`/`attr_accessor` contribute `name`;
/// `attr_writer`/`attr_accessor` contribute `name=` (reference member
/// expansion). A `self.` attr (`AttributeKind::Singleton`) contributes to the
/// singleton surface.
fn attr_insert(decl: &mut Decl, name: &str, kind: AttributeKind, ret: Option<String>, writer: bool) {
    let key = if writer { format!("{name}=") } else { name.to_string() };
    let table = match kind {
        AttributeKind::Singleton => &mut decl.singleton,
        AttributeKind::Instance => &mut decl.instance,
    };
    table.entry(key).or_insert(ret);
}

/// Resolve a method's declared return to `Some(class)` ONLY when it has EXACTLY
/// ONE overload whose return is a bare `ClassInstanceType` with no type args
/// (probed: optional / union / untyped / multi-overload / generic all DROP).
fn resolve_method_return(md: &ruby_rbs::node::MethodDefinitionNode) -> Option<String> {
    let mut overloads = md.overloads().iter();
    let first = overloads.next()?;
    if overloads.next().is_some() {
        return None; // multi-overload ⇒ unresolvable ⇒ DROP.
    }
    let RbsNode::MethodDefinitionOverload(ov) = first else { return None };
    let RbsNode::MethodType(mt) = ov.method_type() else { return None };
    let RbsNode::FunctionType(ft) = mt.type_() else { return None };
    resolve_type_node_return(&ft.return_type())
}

/// A declared type node resolves to `Some(class)` only for a bare
/// `ClassInstanceType` with no type args (`String` ⇒ `Some("String")`;
/// `String?` / `Array[Integer]` / `bool` / `untyped` ⇒ `None`).
fn resolve_type_node_return(node: &RbsNode) -> Option<String> {
    match node {
        RbsNode::ClassInstanceType(ci) => {
            if ci.args().iter().next().is_some() {
                None // has type args ⇒ not bare ⇒ DROP.
            } else {
                let full = decl_full_name(&ci.name());
                if full.is_empty() {
                    None
                } else {
                    Some(full)
                }
            }
        }
        _ => None,
    }
}
