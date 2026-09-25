//! The allow-list: when is the reference's `RBS::DefinitionBuilder` build
//! PROVABLY successful, and what does it then return? (ADR-0044, PR #150's
//! second review round.)
//!
//! `ConformanceChecker#check_one` resolves the interface (`build_interface`)
//! and then builds the class (`build_instance`); either raising an RBS error
//! makes it fail soft, so a row the port emits over a build the reference
//! could not finish is a false positive. The first implementation tried to
//! enumerate the failures and missed seven families. This module inverts that:
//! it walks the build CLOSURE — everything `build_instance(C)` touches — and
//! answers `true` only when every member of it is on an explicit allow-list:
//!
//! - `build_instance(X)`: `X` itself, its superclass's whole build, a module's
//!   self types (`define_instance` of each, plus the `build_instance` /
//!   `build_interface` `define_instance` reads for alias targets), then
//!   `define_instance(X)`;
//! - `define_instance(Y)`: `Y`'s self types' builds, its included modules'
//!   `define_instance` (recursively), its included interfaces with their
//!   ancestors, its prepended modules, and its own members;
//! - `build_interface(I)`: `I`'s interface ancestors and their own members.
//!
//! Every entity is checked for what makes the reference raise: a name that
//! does not resolve (or may resolve differently there: a load-set divergence,
//! a stub, a synthesized namespace, a class alias), a mixin of the wrong kind
//! or arity, inconsistent type parameters (`GenericParameterMismatchError`,
//! which also poisons every build whose `validate_type_params` names the
//! class), variance annotations, bounds and defaults on a project class,
//! duplicated members, aliases and overloads whose target the build cannot
//! prove present, duplicated interface members, and instance variables the
//! build would insert twice for one declarer. Bundled-only entities are
//! trusted — the pinned oracle builds all of them, and
//! `harness/conformance_load_set.rb` lists every one whose surface differs.
//!
//! The member SETS are computed by the same walk (`full_set` / `define_set`),
//! so presence is exact wherever the build is provable.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::load_set::LOAD_SET_DIVERGENT;
use super::{
    parse_conforms_to, ClassDecl, ConformanceData, ConformanceFinding, ConformanceKind,
    HeaderRef, IfaceMember, Member, Origin, Written,
};

type Names = Rc<HashSet<&'static str>>;

/// What a name is in the reference's environment, as far as the port knows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ex {
    Class,
    Iface,
    TypeAlias,
    No,
    /// It may exist there (or be something else): never resolve through it.
    Unknown,
}

/// `RBS::Resolver::TypeNameResolver#resolve`, three-valued.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Res {
    Found(&'static str),
    NotFound,
    Unknown,
}

#[derive(Clone, Copy, Debug)]
enum SelfType {
    Module(&'static str),
    Iface(&'static str),
}

/// `AncestorBuilder#one_instance_ancestors`, resolved and checked.
#[derive(Default)]
struct OneAncestors {
    superclass: Option<&'static str>,
    self_types: Vec<SelfType>,
    includes: Vec<&'static str>,
    ifaces: Vec<&'static str>,
    prepends: Vec<&'static str>,
    /// Class names inside the ancestors' type arguments (`validate_type_params`
    /// reads their type parameters through `in_inherit`).
    arg_names: Vec<&'static str>,
}

/// An interface's own member, after `MethodBuilder`'s tsort.
#[derive(Clone, Copy, Debug)]
enum OwnKind {
    Def,
    /// `def x: ... | ...` with no non-overloading `x` in the same body.
    OverloadOnly,
    Alias(&'static str),
}

pub(super) struct Checker<'a> {
    d: &'a ConformanceData,
    /// Ignore the load-set list (the generator's dump of the port's view).
    raw: bool,
    divergent: HashSet<&'static str>,
    /// Classes whose declarations disagree on type parameters.
    generic_bad: HashSet<&'static str>,
    generic_bad_leaves: HashSet<&'static str>,
    entity: RefCell<HashMap<&'static str, bool>>,
    one: RefCell<HashMap<&'static str, Option<Rc<OneAncestors>>>>,
    build: RefCell<HashMap<&'static str, bool>>,
    define: RefCell<HashMap<(&'static str, bool), bool>>,
    /// The builds in progress: `(name, true)` for `build_instance`,
    /// `(name, false)` for `define_instance`. A re-entry is an ancestor cycle
    /// (`RecursiveAncestorError`).
    stack: RefCell<Vec<(&'static str, bool)>>,
    full: RefCell<HashMap<&'static str, Option<Names>>>,
    defset: RefCell<HashMap<&'static str, Option<Names>>>,
    set_stack: RefCell<Vec<(&'static str, bool)>>,
    iface_anc: RefCell<HashMap<&'static str, Option<Rc<Vec<&'static str>>>>>,
    iface_stack: RefCell<Vec<&'static str>>,
    required: RefCell<HashMap<&'static str, Option<Rc<Vec<&'static str>>>>>,
}

fn is_class_leaf(name: &str) -> bool {
    name.rsplit("::")
        .next()
        .is_some_and(|l| l.starts_with(|c: char| c.is_ascii_uppercase()))
}

fn is_iface_leaf(name: &str) -> bool {
    name.rsplit("::").next().is_some_and(|l| l.starts_with('_'))
}

fn leaf(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// The `(min, max)` type-argument count a declaration's parameters accept
/// (`InvalidTypeApplicationError.check!`).
fn arity_of(params: &[super::Param]) -> (usize, usize) {
    (params.iter().filter(|p| !p.default).count(), params.len())
}

impl<'a> Checker<'a> {
    pub(super) fn new(d: &'a ConformanceData, raw: bool) -> Self {
        let mut generic_bad = HashSet::new();
        for (&name, decls) in &d.classes {
            // Bundled-only entries are the oracle's own (and it builds them);
            // anything else must repeat the first declaration's parameters
            // exactly, a bound or a default counting as a mismatch.
            if decls.len() < 2 || decls.iter().all(|x| x.origin == Origin::Bundled) {
                continue;
            }
            let first = &decls[0].params;
            let same = decls.iter().all(|x| {
                x.params.len() == first.len()
                    && x.params.iter().zip(first).all(|(a, b)| {
                        a.variance == b.variance
                            && a.unchecked == b.unchecked
                            && !a.bounded
                            && !a.default
                            && !b.bounded
                            && !b.default
                    })
            });
            if !same {
                generic_bad.insert(name);
            }
        }
        let generic_bad_leaves = generic_bad.iter().map(|n| leaf(n)).collect();
        Checker {
            d,
            raw,
            divergent: LOAD_SET_DIVERGENT.iter().copied().collect(),
            generic_bad,
            generic_bad_leaves,
            entity: RefCell::default(),
            one: RefCell::default(),
            build: RefCell::default(),
            define: RefCell::default(),
            stack: RefCell::default(),
            full: RefCell::default(),
            defset: RefCell::default(),
            set_stack: RefCell::default(),
            iface_anc: RefCell::default(),
            iface_stack: RefCell::default(),
            required: RefCell::default(),
        }
    }

    fn suspect(&self, o: Origin) -> bool {
        o.key().is_some_and(|k| self.d.suspect.contains(k))
    }

    /// What `n` is in the reference's environment, with its interned key.
    fn lookup(&self, n: &str) -> (Ex, &'static str) {
        if !self.raw && self.divergent.contains(n) {
            return (Ex::Unknown, "");
        }
        if self.d.unknown_names.contains(n) {
            return (Ex::Unknown, "");
        }
        if let Some((&k, decls)) = self.d.classes.get_key_value(n) {
            if decls.iter().all(|x| self.suspect(x.origin)) {
                return (Ex::Unknown, "");
            }
            return (Ex::Class, k);
        }
        if let Some((&k, slot)) = self.d.interfaces.get_key_value(n) {
            if slot.ambiguous {
                return (Ex::Unknown, "");
            }
            return (Ex::Iface, k);
        }
        if let Some((&k, &o)) = self.d.type_aliases.get_key_value(n) {
            if self.suspect(o) {
                return (Ex::Unknown, "");
            }
            return (Ex::TypeAlias, k);
        }
        if self.d.class_aliases.contains(n) || self.d.maybe_synthetic.contains(n) {
            return (Ex::Unknown, "");
        }
        (Ex::No, "")
    }

    fn lookup_res(&self, n: &str) -> Res {
        match self.lookup(n) {
            (Ex::No, _) => Res::NotFound,
            (Ex::Unknown, _) => Res::Unknown,
            (_, k) => Res::Found(k),
        }
    }

    /// `TypeNameResolver#resolve` for a written name in lexical `ctx`
    /// (enclosing qualified names, outermost first).
    fn resolve(&self, w: Written, ctx: &[&'static str]) -> Res {
        if w.absolute {
            match self.lookup_res(w.path) {
                Res::NotFound => {}
                other => return other,
            }
        }
        if is_class_leaf(w.path) {
            return self.resolve_namespace(w.path, w.absolute, ctx);
        }
        match w.path.rsplit_once("::") {
            // `resolve_type_name`: innermost scope outward, then the root.
            None => {
                for scope in ctx.iter().rev() {
                    match self.lookup_res(&format!("{scope}::{}", w.path)) {
                        Res::NotFound => {}
                        other => return other,
                    }
                }
                self.lookup_res(w.path)
            }
            Some((ns, name)) => match self.resolve_namespace(ns, w.absolute, ctx) {
                Res::Found(n) => self.lookup_res(&format!("{n}::{name}")),
                other => other,
            },
        }
    }

    /// `resolve_namespace0`: the HEAD segment is looked up innermost scope
    /// outward (then at the root), and every later segment must exist under
    /// the head found — there is no fallback to an outer scope's `A::B`.
    fn resolve_namespace(&self, path: &str, absolute: bool, ctx: &[&'static str]) -> Res {
        let mut segs = path.split("::");
        let Some(head) = segs.next() else {
            return Res::NotFound;
        };
        let mut cur = None;
        if !absolute {
            for scope in ctx.iter().rev() {
                match self.lookup_res(&format!("{scope}::{head}")) {
                    Res::NotFound => {}
                    Res::Found(k) => {
                        cur = Some(k);
                        break;
                    }
                    Res::Unknown => return Res::Unknown,
                }
            }
        }
        let mut cur = match cur {
            Some(k) => k,
            None => match self.lookup_res(head) {
                Res::Found(k) => k,
                other => return other,
            },
        };
        for seg in segs {
            match self.lookup_res(&format!("{cur}::{seg}")) {
                Res::Found(k) => cur = k,
                other => return other,
            }
        }
        Res::Found(cur)
    }

    fn is_module(&self, n: &str) -> Option<bool> {
        self.d.classes.get(n)?.first().map(|x| x.is_module)
    }

    /// A class's parameters are its PRIMARY declaration's: the first with a
    /// superclass, else the first (`ClassEntry#primary_decl`).
    fn class_arity(&self, n: &str) -> Option<(usize, usize)> {
        let decls = self.d.classes.get(n)?;
        let primary = decls.iter().find(|x| x.superclass.is_some()).or(decls.first())?;
        Some(arity_of(&primary.params))
    }

    fn iface_arity(&self, n: &str) -> Option<(usize, usize)> {
        Some(arity_of(&self.d.interfaces.get(n)?.decl.params))
    }

    /// A header's type arguments: the count the target accepts, and every
    /// name inside them present (`validate_type_presence`). Class names met
    /// are appended to `names`.
    fn header_args_ok(
        &self,
        h: &HeaderRef,
        ctx: &[&'static str],
        arity: Option<(usize, usize)>,
        names: &mut Vec<&'static str>,
    ) -> bool {
        let Some((min, max)) = arity else {
            return false;
        };
        if h.nargs < min || h.nargs > max || !h.args_ok {
            return false;
        }
        for w in &h.arg_refs {
            match self.resolve(*w, ctx) {
                Res::Found(k) => names.push(k),
                _ => return false,
            }
        }
        true
    }

    fn inner_ctx(x: &'static str, decl: &ClassDecl) -> Vec<&'static str> {
        decl.outer.iter().copied().chain([x]).collect()
    }

    fn one(&self, x: &'static str) -> Option<Rc<OneAncestors>> {
        if let Some(v) = self.one.borrow().get(x) {
            return v.clone();
        }
        let v = self.compute_one(x).map(Rc::new);
        self.one.borrow_mut().insert(x, v.clone());
        v
    }

    fn compute_one(&self, x: &'static str) -> Option<OneAncestors> {
        let decls = self.d.classes.get(x)?;
        let mut one = OneAncestors::default();
        if decls[0].is_module {
            let mut seen: HashMap<&'static str, &'static str> = HashMap::new();
            for d in decls {
                let ctx = Self::inner_ctx(x, d);
                for h in &d.self_types {
                    let Res::Found(s) = self.resolve(h.name, &ctx) else {
                        return None;
                    };
                    let st = if self.d.interfaces.contains_key(s) {
                        if !self.header_args_ok(h, &ctx, self.iface_arity(s), &mut one.arg_names) {
                            return None;
                        }
                        SelfType::Iface(s)
                    } else {
                        self.is_module(s)?;
                        if !self.header_args_ok(h, &ctx, self.class_arity(s), &mut one.arg_names) {
                            return None;
                        }
                        SelfType::Module(s)
                    };
                    // `ModuleEntry#self_types` is `uniq`'d (name and
                    // arguments); a repeat spelled otherwise is not provably
                    // the same self type.
                    match seen.get(&s) {
                        Some(&text) if text == h.args_text && !text.contains('\0') => continue,
                        Some(_) => return None,
                        None => {
                            seen.insert(s, h.args_text);
                        }
                    }
                    one.self_types.push(st);
                }
            }
            if one.self_types.is_empty() {
                match self.lookup("Object") {
                    (Ex::Class, k) if self.is_module(k) == Some(false) => {
                        one.self_types.push(SelfType::Module(k));
                    }
                    _ => return None,
                }
            }
        } else {
            match decls.iter().find(|d| d.superclass.is_some()) {
                Some(d) => {
                    let h = d.superclass.as_ref()?;
                    let Res::Found(s) = self.resolve(h.name, &d.outer) else {
                        return None;
                    };
                    // `InheritModuleError` / a self-inheritance cycle.
                    if self.is_module(s)? || s == x {
                        return None;
                    }
                    if !self.header_args_ok(h, &d.outer, self.class_arity(s), &mut one.arg_names) {
                        return None;
                    }
                    one.superclass = Some(s);
                }
                None if x == "BasicObject" => {}
                None => match self.lookup("Object") {
                    (Ex::Class, k) if self.is_module(k) == Some(false) => one.superclass = Some(k),
                    _ => return None,
                },
            }
        }
        for d in decls {
            let ctx = Self::inner_ctx(x, d);
            for m in &d.members {
                let (h, prepend) = match m {
                    Member::Include(h) => (h, false),
                    Member::Prepend(h) => (h, true),
                    _ => continue,
                };
                let Res::Found(t) = self.resolve(h.name, &ctx) else {
                    return None;
                };
                if is_iface_leaf(h.name.path) {
                    // `prepend` of an interface raises a non-RBS error upstream.
                    if prepend || !self.d.interfaces.contains_key(t) {
                        return None;
                    }
                    if !self.header_args_ok(h, &ctx, self.iface_arity(t), &mut one.arg_names) {
                        return None;
                    }
                    one.ifaces.push(t);
                } else {
                    // `MixinClassError` for a class, `NoMixinFoundError` else.
                    if self.is_module(t) != Some(true) {
                        return None;
                    }
                    if !self.header_args_ok(h, &ctx, self.class_arity(t), &mut one.arg_names) {
                        return None;
                    }
                    if prepend {
                        one.prepends.push(t);
                    } else {
                        one.includes.push(t);
                    }
                }
            }
        }
        Some(one)
    }

    /// Context-independent checks on one class/module entry.
    fn entity_ok(&self, x: &'static str) -> bool {
        if let Some(&v) = self.entity.borrow().get(x) {
            return v;
        }
        let v = self.compute_entity_ok(x);
        self.entity.borrow_mut().insert(x, v);
        v
    }

    fn compute_entity_ok(&self, x: &'static str) -> bool {
        let Some(decls) = self.d.classes.get(x) else {
            return false;
        };
        if self.lookup(x).0 != Ex::Class {
            return false;
        }
        if decls.iter().any(|d| self.suspect(d.origin)) {
            return false;
        }
        if super::super::UNBUILDABLE_DEFINITIONS.iter().any(|&(n, inst, _)| inst && n == x) {
            return false;
        }
        if self.generic_bad.contains(x) {
            return false;
        }
        if decls.iter().any(|d| d.is_module != decls[0].is_module) {
            return false;
        }
        // `ensure_namespace!`: every enclosing namespace must exist.
        let segs: Vec<&str> = x.split("::").collect();
        for i in 1..segs.len() {
            if self.lookup(&segs[..i].join("::")).0 != Ex::Class {
                return false;
            }
        }
        if self.one(x).is_none() {
            return false;
        }
        // Bundled / plugin-only entries are trusted.
        !decls.iter().any(|d| d.origin.projectish()) || self.project_entity_ok(decls)
    }

    /// `DefinitionBuilder#validate_type_params(x)`, run by `build_instance(x)`
    /// only (not for a module merely included): every name its methods' types
    /// and its ancestors' type arguments mention must exist, and none may be a
    /// class whose declarations disagree on type parameters (reading such a
    /// class's `type_params` raises `GenericParameterMismatchError`). A
    /// bundled declaration's names are compared by leaf, conservatively.
    fn validated_ok(&self, x: &'static str) -> bool {
        let (Some(one), Some(decls)) = (self.one(x), self.d.classes.get(x)) else {
            return false;
        };
        if one.arg_names.iter().any(|n| self.generic_bad.contains(n)) {
            return false;
        }
        for d in decls {
            if d.origin.projectish() {
                let ctx = Self::inner_ctx(x, d);
                for w in &d.vrefs {
                    match self.resolve(*w, &ctx) {
                        Res::Found(k) if !self.generic_bad.contains(k) => {}
                        _ => return false,
                    }
                }
            } else if !self.generic_bad.is_empty()
                && d.vrefs.iter().any(|w| self.generic_bad_leaves.contains(w.leaf()))
            {
                return false;
            }
        }
        true
    }

    fn project_entity_ok(&self, decls: &[ClassDecl]) -> bool {
        for d in decls {
            if d.unsupported {
                return false;
            }
            if let Origin::Project(f) | Origin::Collection(f) = d.origin {
                if self.d.directive_files.contains(f) {
                    return false;
                }
            }
        }
        // `validate_type_params`: only invariant or `unchecked` parameters,
        // without bounds or defaults, are provably compatible.
        if decls[0]
            .params
            .iter()
            .any(|p| (p.variance != 0 && !p.unchecked) || p.bounded || p.default)
        {
            return false;
        }
        // `validate_super_class!`: several superclass clauses must agree.
        let supers: Vec<&ClassDecl> = decls.iter().filter(|d| d.superclass.is_some()).collect();
        if supers.len() > 1 {
            let first = supers[0].superclass.as_ref().map(|h| (h.name, h.nargs));
            if supers.iter().any(|d| {
                d.superclass.as_ref().map(|h| (h.name, h.nargs)) != first
                    || d.outer != supers[0].outer
                    || d.superclass.as_ref().is_some_and(|h| h.nargs != 0)
            }) {
                return false;
            }
        }
        // `MethodBuilder::Methods#validate!`: one original per name, across
        // every declaration (core reopens included); no alias cycle.
        let mut originals: HashSet<&'static str> = HashSet::new();
        let mut aliases: HashMap<&'static str, &'static str> = HashMap::new();
        for d in decls {
            for m in &d.members {
                let name = match m {
                    Member::Def { name, overloading: false } | Member::Attr(name) => *name,
                    Member::Alias { new, old } => {
                        aliases.insert(new, old);
                        new
                    }
                    _ => continue,
                };
                if !originals.insert(name) {
                    return false;
                }
            }
        }
        for &start in aliases.keys() {
            let mut cur = start;
            let mut steps = 0;
            while let Some(&next) = aliases.get(cur) {
                steps += 1;
                if next == start || steps > aliases.len() {
                    return false;
                }
                cur = next;
            }
        }
        true
    }

    /// `DefinitionBuilder#build_instance(x)` provably succeeds.
    pub(super) fn build_ok(&self, x: &'static str) -> bool {
        if let Some(&v) = self.build.borrow().get(x) {
            return v;
        }
        if self.stack.borrow().contains(&(x, true)) {
            return false;
        }
        self.stack.borrow_mut().push((x, true));
        let v = self.compute_build_ok(x);
        self.stack.borrow_mut().pop();
        self.build.borrow_mut().insert(x, v);
        v
    }

    fn compute_build_ok(&self, x: &'static str) -> bool {
        if !self.entity_ok(x) {
            return false;
        }
        let Some(one) = self.one(x) else {
            return false;
        };
        if let Some(s) = one.superclass {
            if !self.build_ok(s) {
                return false;
            }
        }
        if self.is_module(x) == Some(true) {
            for st in &one.self_types {
                let ok = match *st {
                    SelfType::Module(s) => self.define_ok(s, true) && self.build_ok(s),
                    SelfType::Iface(i) => self.required(i).is_some(),
                };
                if !ok {
                    return false;
                }
            }
        }
        self.validated_ok(x)
            && self.define_ok(x, false)
            && self.ivars_ok(x)
            && self.full_set(x).is_some()
    }

    /// `DefinitionBuilder#define_instance(definition, y)` provably succeeds.
    /// `foreign`: `y` is defined into ANOTHER entry's definition (an included
    /// or prepended module, a module's self type), whose methods the port does
    /// not count on for `y`'s aliases and overloads.
    fn define_ok(&self, y: &'static str, foreign: bool) -> bool {
        if let Some(&v) = self.define.borrow().get(&(y, foreign)) {
            return v;
        }
        if self.stack.borrow().contains(&(y, false)) {
            return false;
        }
        self.stack.borrow_mut().push((y, false));
        let v = self.compute_define_ok(y, foreign);
        self.stack.borrow_mut().pop();
        self.define.borrow_mut().insert((y, foreign), v);
        v
    }

    fn compute_define_ok(&self, y: &'static str, foreign: bool) -> bool {
        if !self.entity_ok(y) {
            return false;
        }
        let (Some(one), Some(decls)) = (self.one(y), self.d.classes.get(y)) else {
            return false;
        };
        let is_module = decls[0].is_module;
        // `self_type_methods`: each self type's WHOLE build.
        let mut self_type_methods: HashSet<&'static str> = HashSet::new();
        if is_module {
            for st in &one.self_types {
                let set = match *st {
                    SelfType::Module(s) => {
                        if !self.build_ok(s) {
                            return false;
                        }
                        self.full_set(s)
                    }
                    SelfType::Iface(i) => {
                        if self.required(i).is_none() {
                            return false;
                        }
                        self.iface_all(i)
                    }
                };
                let Some(set) = set else {
                    return false;
                };
                self_type_methods.extend(set.iter().copied());
            }
        }
        for &m in one.includes.iter().chain(&one.prepends) {
            if !self.define_ok(m, true) {
                return false;
            }
        }
        let Some(iface_names) = self.iface_import(&one.ifaces) else {
            return false;
        };
        // Methods already in the definition when `y`'s own are imported:
        // included modules (defined first), and on its OWN build a class's
        // superclass or a module's self types.
        let mut before: HashSet<&'static str> = iface_names.clone();
        for &m in &one.includes {
            let Some(set) = self.define_set(m) else {
                return false;
            };
            before.extend(set.iter().copied());
        }
        if !foreign {
            if let Some(s) = one.superclass {
                let Some(set) = self.full_set(s) else {
                    return false;
                };
                before.extend(set.iter().copied());
            }
            if is_module {
                before.extend(self_type_methods.iter().copied());
            }
        }
        let mut own_orig: HashSet<&'static str> = HashSet::new();
        let mut own_all: HashSet<&'static str> = HashSet::new();
        for d in decls {
            for m in &d.members {
                match m {
                    Member::Def { name, overloading: false } | Member::Attr(name) => {
                        own_orig.insert(name);
                        own_all.insert(name);
                    }
                    Member::Def { name, overloading: true } => {
                        own_all.insert(name);
                    }
                    Member::Alias { new, .. } => {
                        own_all.insert(new);
                    }
                    _ => {}
                }
            }
        }
        // `DuplicatedMethodDefinitionError`: an own def over an included
        // interface's member.
        if own_orig.iter().any(|n| iface_names.contains(n)) {
            return false;
        }
        for d in decls {
            for m in &d.members {
                match m {
                    // `InvalidOverloadMethodError` unless something defines it.
                    Member::Def { name, overloading: true } => {
                        if !own_orig.contains(name) && !before.contains(name) {
                            return false;
                        }
                    }
                    // `UnknownMethodAliasError` unless the target is present.
                    Member::Alias { old, .. } => {
                        if !own_all.contains(old)
                            && !before.contains(old)
                            && !self_type_methods.contains(old)
                        {
                            return false;
                        }
                    }
                    _ => {}
                }
            }
        }
        true
    }

    /// `interface_methods` keys its hash by `Definition::Ancestor::Instance`,
    /// whose equality is name and ARGUMENTS (not the include that reached it):
    /// an interface met twice (a diamond, or included twice) is imported once,
    /// at its first position. `Some(true)` = import `a` now, `Some(false)` =
    /// already imported, `None` = met again with type parameters, whose
    /// arguments the port does not compare.
    fn first_import(&self, a: &'static str, imported: &mut HashSet<&'static str>) -> Option<bool> {
        if imported.insert(a) {
            return Some(true);
        }
        match self.iface_arity(a) {
            Some((_, 0)) => Some(false),
            _ => None,
        }
    }

    /// `import_methods` of the interfaces a class/module includes: each with
    /// its ancestors, in order. Their members must be pairwise distinct
    /// (`DuplicatedInterfaceMethodDefinitionError`), an alias must target a
    /// member imported before it, and an overloading member is refused.
    fn iface_import(&self, ifaces: &[&'static str]) -> Option<HashSet<&'static str>> {
        let mut seen: HashSet<&'static str> = HashSet::new();
        let mut imported: HashSet<&'static str> = HashSet::new();
        for &j in ifaces {
            let anc = self.iface_ancestors(j)?;
            for &a in anc.iter() {
                if !self.first_import(a, &mut imported)? {
                    continue;
                }
                if !self.iface_entity_ok(a) {
                    return None;
                }
                for (n, k) in self.iface_own(a)? {
                    match k {
                        OwnKind::OverloadOnly => return None,
                        OwnKind::Def => {
                            if !seen.insert(n) {
                                return None;
                            }
                        }
                        OwnKind::Alias(old) => {
                            if !seen.contains(old) || !seen.insert(n) {
                                return None;
                            }
                        }
                    }
                }
            }
        }
        Some(seen)
    }

    /// No instance variable is inserted twice by one declarer into one
    /// definition (`InstanceVariableDuplicationError`). The reference raises
    /// only when the FIRST two non-attribute insertions share a declarer; the
    /// port asks for all of them to be distinct, which cannot be wrong.
    fn ivars_ok(&self, x: &'static str) -> bool {
        let mut events: Vec<(&'static str, &'static str)> = Vec::new();
        if !self.ivar_events(x, true, &mut events, 0) {
            return false;
        }
        let mut seen: HashSet<(&'static str, &'static str)> = HashSet::new();
        events.into_iter().all(|e| seen.insert(e))
    }

    fn ivar_events(
        &self,
        x: &'static str,
        full: bool,
        out: &mut Vec<(&'static str, &'static str)>,
        depth: usize,
    ) -> bool {
        if depth > 64 {
            return false;
        }
        let (Some(one), Some(decls)) = (self.one(x), self.d.classes.get(x)) else {
            return false;
        };
        if full {
            if let Some(s) = one.superclass {
                if !self.ivar_events(s, true, out, depth + 1) {
                    return false;
                }
            }
            if decls[0].is_module {
                for st in &one.self_types {
                    if let SelfType::Module(s) = *st {
                        if !self.ivar_events(s, false, out, depth + 1) {
                            return false;
                        }
                    }
                }
            }
        }
        for &m in one.includes.iter().chain(&one.prepends) {
            if !self.ivar_events(m, false, out, depth + 1) {
                return false;
            }
        }
        for d in decls {
            for m in &d.members {
                if let Member::Ivar(name) = m {
                    out.push((x, name));
                }
            }
        }
        true
    }

    /// The method names `build_instance(x)` returns.
    pub(super) fn full_set(&self, x: &'static str) -> Option<Names> {
        if let Some(v) = self.full.borrow().get(x) {
            return v.clone();
        }
        if self.set_stack.borrow().contains(&(x, true)) {
            return None;
        }
        self.set_stack.borrow_mut().push((x, true));
        let v = self.compute_full_set(x);
        self.set_stack.borrow_mut().pop();
        self.full.borrow_mut().insert(x, v.clone());
        v
    }

    fn compute_full_set(&self, x: &'static str) -> Option<Names> {
        let one = self.one(x)?;
        let mut s: HashSet<&'static str> = HashSet::new();
        if self.is_module(x)? {
            for st in &one.self_types {
                let part = match *st {
                    SelfType::Module(m) => self.define_set(m)?,
                    SelfType::Iface(i) => self.iface_all(i)?,
                };
                s.extend(part.iter().copied());
            }
        } else if let Some(sup) = one.superclass {
            s.extend(self.full_set(sup)?.iter().copied());
        }
        s.extend(self.define_set(x)?.iter().copied());
        Some(Rc::new(s))
    }

    /// The method names `define_instance(definition, y)` adds.
    fn define_set(&self, y: &'static str) -> Option<Names> {
        if let Some(v) = self.defset.borrow().get(y) {
            return v.clone();
        }
        if self.set_stack.borrow().contains(&(y, false)) {
            return None;
        }
        self.set_stack.borrow_mut().push((y, false));
        let v = self.compute_define_set(y);
        self.set_stack.borrow_mut().pop();
        self.defset.borrow_mut().insert(y, v.clone());
        v
    }

    fn compute_define_set(&self, y: &'static str) -> Option<Names> {
        let one = self.one(y)?;
        let decls = self.d.classes.get(y)?;
        let mut s: HashSet<&'static str> = HashSet::new();
        for d in decls {
            for m in &d.members {
                match m {
                    Member::Def { name, .. } | Member::Attr(name) => {
                        s.insert(name);
                    }
                    Member::Alias { new, .. } => {
                        s.insert(new);
                    }
                    _ => {}
                }
            }
        }
        for &m in one.includes.iter().chain(&one.prepends) {
            s.extend(self.define_set(m)?.iter().copied());
        }
        for &j in &one.ifaces {
            s.extend(self.iface_all(j)?.iter().copied());
        }
        Some(Rc::new(s))
    }

    /// `interface_ancestors(i)`: `[i, ...]`, the LAST include's ancestors
    /// first, each recursively (pre-order).
    fn iface_ancestors(&self, i: &'static str) -> Option<Rc<Vec<&'static str>>> {
        if let Some(v) = self.iface_anc.borrow().get(i) {
            return v.clone();
        }
        if self.iface_stack.borrow().contains(&i) {
            return None;
        }
        self.iface_stack.borrow_mut().push(i);
        let v = self.compute_iface_ancestors(i).map(Rc::new);
        self.iface_stack.borrow_mut().pop();
        self.iface_anc.borrow_mut().insert(i, v.clone());
        v
    }

    fn compute_iface_ancestors(&self, i: &'static str) -> Option<Vec<&'static str>> {
        if self.lookup(i).0 != Ex::Iface {
            return None;
        }
        let decl = &self.d.interfaces.get(i)?.decl;
        let mut rest: Vec<&'static str> = Vec::new();
        for h in &decl.includes {
            // A module name included into an interface is silently ignored
            // upstream; the port does not rely on that.
            if !is_iface_leaf(h.name.path) {
                return None;
            }
            let Res::Found(j) = self.resolve(h.name, &decl.ctx) else {
                return None;
            };
            if !self.d.interfaces.contains_key(j) {
                return None;
            }
            let mut names = Vec::new();
            if !self.header_args_ok(h, &decl.ctx, self.iface_arity(j), &mut names) {
                return None;
            }
            let sub = self.iface_ancestors(j)?;
            let mut next: Vec<&'static str> = sub.iter().copied().collect();
            next.extend(rest);
            rest = next;
        }
        let mut out = vec![i];
        out.extend(rest);
        Some(out)
    }

    /// An interface entry the port can read at all: known, unambiguous,
    /// fully modelled.
    fn iface_entity_ok(&self, i: &'static str) -> bool {
        if self.lookup(i).0 != Ex::Iface {
            return false;
        }
        let Some(slot) = self.d.interfaces.get(i) else {
            return false;
        };
        match slot.origin {
            Origin::Project(f) | Origin::Collection(f) => {
                !slot.decl.unsupported && !self.d.directive_files.contains(f)
            }
            _ => !slot.decl.unsupported || slot.origin == Origin::Bundled,
        }
    }

    /// An interface's own members in `MethodBuilder` order (`Methods#each`: a
    /// tsort over alias targets). `None` on a duplicated original
    /// (`validate!`) or an alias cycle.
    fn iface_own(&self, i: &'static str) -> Option<Vec<(&'static str, OwnKind)>> {
        let decl = &self.d.interfaces.get(i)?.decl;
        let mut order: Vec<&'static str> = Vec::new();
        let mut originals: HashMap<&'static str, usize> = HashMap::new();
        let mut kinds: HashMap<&'static str, OwnKind> = HashMap::new();
        for m in &decl.members {
            let (name, kind, original) = match *m {
                IfaceMember::Def { name, overloading } => {
                    (name, if overloading { OwnKind::OverloadOnly } else { OwnKind::Def }, !overloading)
                }
                IfaceMember::Alias { new, old } => (new, OwnKind::Alias(old), true),
            };
            if !kinds.contains_key(name) {
                order.push(name);
            }
            if original {
                *originals.entry(name).or_default() += 1;
                kinds.insert(name, kind);
            } else {
                kinds.entry(name).or_insert(kind);
            }
        }
        if originals.values().any(|&c| c > 1) {
            return None;
        }
        let mut out: Vec<(&'static str, OwnKind)> = Vec::new();
        let mut state: HashMap<&'static str, bool> = HashMap::new(); // false = visiting
        fn visit(
            n: &'static str,
            kinds: &HashMap<&'static str, OwnKind>,
            state: &mut HashMap<&'static str, bool>,
            out: &mut Vec<(&'static str, OwnKind)>,
        ) -> bool {
            match state.get(n) {
                Some(true) => return true,
                Some(false) => return false,
                None => {}
            }
            state.insert(n, false);
            let kind = kinds[n];
            if let OwnKind::Alias(old) = kind {
                if kinds.contains_key(old) && !visit(old, kinds, state, out) {
                    return false;
                }
            }
            state.insert(n, true);
            out.push((n, kind));
            true
        }
        for n in order {
            if !visit(n, &kinds, &mut state, &mut out) {
                return None;
            }
        }
        Some(out)
    }

    /// The member names of an interface and its ancestors.
    fn iface_all(&self, i: &'static str) -> Option<Names> {
        let anc = self.iface_ancestors(i)?;
        let mut s: HashSet<&'static str> = HashSet::new();
        for &a in anc.iter() {
            for (n, _) in self.iface_own(a)? {
                s.insert(n);
            }
        }
        Some(Rc::new(s))
    }

    /// `DefinitionBuilder#build_interface(i)` provably succeeds: its required
    /// members in the order its `methods` hash holds them. `None` wherever the
    /// build would raise or the port cannot tell.
    pub(super) fn required(&self, i: &'static str) -> Option<Rc<Vec<&'static str>>> {
        if let Some(v) = self.required.borrow().get(i) {
            return v.clone();
        }
        let v = self.compute_required(i).map(Rc::new);
        self.required.borrow_mut().insert(i, v.clone());
        v
    }

    fn compute_required(&self, i: &'static str) -> Option<Vec<&'static str>> {
        if !self.iface_entity_ok(i) {
            return None;
        }
        let slot = self.d.interfaces.get(i)?;
        // `ensure_namespace!`.
        let segs: Vec<&str> = i.split("::").collect();
        for k in 1..segs.len() {
            if self.lookup(&segs[..k].join("::")).0 != Ex::Class {
                return None;
            }
        }
        // `validate_type_params` over the interface's own methods and includes.
        if slot.origin != Origin::Bundled {
            if slot
                .decl
                .params
                .iter()
                .any(|p| (p.variance != 0 && !p.unchecked) || p.bounded || p.default)
            {
                return None;
            }
            for w in &slot.decl.vrefs {
                match self.resolve(*w, &slot.decl.ctx) {
                    Res::Found(k) if !self.generic_bad.contains(k) => {}
                    _ => return None,
                }
            }
        } else if slot.decl.vrefs.iter().any(|w| self.generic_bad_leaves.contains(w.leaf())) {
            return None;
        }
        let anc = self.iface_ancestors(i)?;
        let mut out: Vec<&'static str> = Vec::new();
        let mut seen: HashSet<&'static str> = HashSet::new();
        let mut imported: HashSet<&'static str> = HashSet::new();
        for &a in anc.iter().skip(1) {
            if !self.first_import(a, &mut imported)? {
                continue;
            }
            if !self.iface_entity_ok(a) {
                return None;
            }
            for (n, k) in self.iface_own(a)? {
                match k {
                    OwnKind::OverloadOnly => return None,
                    OwnKind::Def => {}
                    OwnKind::Alias(old) => {
                        if !seen.contains(old) {
                            return None;
                        }
                    }
                }
                if !seen.insert(n) {
                    return None;
                }
                out.push(n);
            }
        }
        for (n, k) in self.iface_own(i)? {
            match k {
                OwnKind::OverloadOnly => {
                    if !seen.contains(n) {
                        return None;
                    }
                    continue;
                }
                OwnKind::Def => {}
                OwnKind::Alias(old) => {
                    if !seen.contains(old) {
                        return None;
                    }
                }
            }
            if !seen.insert(n) {
                return None;
            }
            out.push(n);
        }
        Some(out)
    }
}

/// `ConformanceChecker.scan`, restricted to what the port can prove.
pub(super) fn findings(d: &ConformanceData) -> Vec<ConformanceFinding> {
    // A project `prepend _Iface` makes the reference's build raise a non-RBS
    // error: the whole run dies there, with no rows at all.
    if d.crash_risk {
        return Vec::new();
    }
    let ck = Checker::new(d, false);
    let mut out = Vec::new();
    for ann in &d.annotations {
        let Some(iname) = parse_conforms_to(&ann.text) else {
            continue;
        };
        // A class the reference's default environment also declares (or
        // declares differently) may sit elsewhere in `env.class_decls`.
        if ck.divergent.contains(ann.class) {
            continue;
        }
        // `ConformanceChecker#candidate_interface_names`: the class's
        // namespace prefixes, longest first, then the bare name. The
        // reference takes the first candidate that BUILDS, so a candidate
        // that exists but is not provably buildable silences the row.
        let parts: Vec<&str> = ann.class.split("::").collect();
        let mut candidates: Vec<String> = (1..=parts.len())
            .rev()
            .map(|n| format!("{}::{iname}", parts[..n].join("::")))
            .collect();
        candidates.push(iname.to_string());
        let mut resolved = None;
        let mut silent = false;
        for c in &candidates {
            match ck.lookup(c) {
                (Ex::No, _) => {}
                (Ex::Iface, k) => {
                    match ck.required(k) {
                        Some(req) => resolved = Some(req),
                        None => silent = true,
                    }
                    break;
                }
                _ => {
                    silent = true;
                    break;
                }
            }
        }
        if silent {
            continue;
        }
        let finding = |kind| ConformanceFinding {
            kind,
            class_name: ann.class,
            interface_name: iname.to_string(),
            file: ann.file,
            start_offset: ann.start,
            end_offset: ann.end,
        };
        let Some(required) = resolved else {
            out.push(finding(ConformanceKind::Unresolved));
            continue;
        };
        if !ck.build_ok(ann.class) {
            continue;
        }
        let Some(provided) = ck.full_set(ann.class) else {
            continue;
        };
        let missing: Vec<&'static str> =
            required.iter().copied().filter(|m| !provided.contains(m)).collect();
        if !missing.is_empty() {
            out.push(finding(ConformanceKind::Unsatisfied { missing }));
        }
    }
    out
}

/// The port's view for `harness/conformance_load_set.rb` (see
/// [`super::CoreData::conformance_surface_dump`]).
pub(super) fn dump(d: &ConformanceData) -> String {
    let ck = Checker::new(d, true);
    let mut lines: Vec<String> = Vec::new();
    let mut names: Vec<&'static str> = d.classes.keys().copied().collect();
    names.sort_unstable();
    for n in names {
        let decls = &d.classes[n];
        let (min, max) = ck.class_arity(n).unwrap_or((0, 0));
        let surface = match ck.full_set(n) {
            Some(s) => {
                let mut v: Vec<&str> = s.iter().copied().collect();
                v.sort_unstable();
                v.join(" ")
            }
            None => "?".to_string(),
        };
        lines.push(format!(
            "class\t{n}\t{}\t{min}\t{max}\t{surface}",
            if decls[0].is_module { "module" } else { "class" }
        ));
    }
    let mut names: Vec<&'static str> = d.interfaces.keys().copied().collect();
    names.sort_unstable();
    for n in names {
        let (min, max) = ck.iface_arity(n).unwrap_or((0, 0));
        let members = match ck.required(n) {
            Some(r) => r.join(" "),
            None => "?".to_string(),
        };
        lines.push(format!("iface\t{n}\t{min}\t{max}\t{members}"));
    }
    let mut names: Vec<&'static str> = d.type_aliases.keys().copied().collect();
    names.sort_unstable();
    lines.extend(names.into_iter().map(|n| format!("alias\t{n}")));
    let mut names: Vec<&'static str> = d.class_aliases.iter().copied().collect();
    names.sort_unstable();
    lines.extend(names.into_iter().map(|n| format!("class_alias\t{n}")));
    let mut names: Vec<&'static str> = d.constants.iter().copied().collect();
    names.sort_unstable();
    lines.extend(names.into_iter().map(|n| format!("const\t{n}")));
    let mut names: Vec<&'static str> = d.globals.iter().copied().collect();
    names.sort_unstable();
    lines.extend(names.into_iter().map(|n| format!("global\t{n}")));
    lines.join("\n") + "\n"
}
