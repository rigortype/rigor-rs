//! `%a{rigor:v1:conforms-to _Interface}` — the reference's
//! `Rigor::RbsExtended::ConformanceChecker` (issue #129, ADR-0044).
//!
//! A class/module declaration in the project's own RBS may assert that it
//! satisfies a named structural interface. The reference checks every such
//! directive once per run and reports two rows, both positioned at the
//! ANNOTATION in the `.rbs` (there is no Ruby `def` a missing member could be
//! reported at):
//!
//! - `rbs_extended.unsatisfied-conformance` — the class does not provide one or
//!   more members the interface requires (tier A, presence);
//! - `dynamic.rbs-extended.unresolved` — the interface name resolves nowhere.
//!
//! Tier B of the reference (signature subtyping of a PROVIDED member) is not
//! ported; see the ADR for why.
//!
//! This module is a self-contained side walk over the same parsed declarations
//! [`super::Builder::ingest`] folds into the class tables. It keeps its OWN
//! tables rather than extending the shared ones, because the questions it must
//! answer are different: not "which methods does `String` have", but "would the
//! reference's RBS environment even LOAD this file, BUILD this class's
//! definition, and BUILD this interface's definition". Every one of those that
//! the reference answers "no" silences the row there, so every one rigor-rs
//! cannot answer "yes" to with certainty silences it here (the zero-FP bar).
//! The two failure families, both oracle-measured at `e59b7b89`:
//!
//! 1. **File quarantine.** `add_project_signatures` inserts each `sig/` file
//!    transactionally and DROPS the whole file on any
//!    `RBS::DuplicatedDeclarationError` — a class/module kind clash, a
//!    redeclared interface, type alias, constant (top-level or nested) or
//!    global — so every annotation in that file vanishes.
//! 2. **Definition build failure.** `ConformanceChecker` skips a class whose
//!    instance definition cannot be built: a method (def / attr / alias name)
//!    declared twice across the class's declarations — core reopens included —
//!    an alias to nothing, a superclass mismatch, a reopen with different type
//!    parameters, or ANY such failure on an ancestor. An interface whose definition cannot be built (a member
//!    redefined through an include, an unresolvable include) makes the
//!    reference report "not loaded"; the port stays silent there instead.

use std::collections::{HashMap, HashSet};

use ruby_rbs::node::{
    parse, AliasKind, AttributeKind, ClassNode, InterfaceNode, MethodDefinitionKind, ModuleNode,
    Node, NodeList, TypeNameNode, TypeParamVariance,
};

use super::{intern, qualified_name, type_name_str, CoreData};

/// The reference's shipped capability-role catalogue
/// (`data/capability_roles/capability_roles.rbs`, #976), vendored byte-for-byte
/// from the pinned submodule. See `vendor/capability_roles/PROVENANCE.md`.
pub(super) const CAPABILITY_ROLES_RBS: &str =
    include_str!("../../vendor/capability_roles/capability_roles.rbs");

/// Rule id of the missing-member row (authored `:warning`).
pub const UNSATISFIED_CONFORMANCE: &str = "rbs_extended.unsatisfied-conformance";
/// Rule id of the unresolved-interface row (authored `:warning`, #928).
pub const RBS_EXTENDED_UNRESOLVED: &str = "dynamic.rbs-extended.unresolved";

/// What a `conforms-to` scan found for one directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConformanceKind {
    /// The class provably lacks these required members (interface order).
    Unsatisfied { missing: Vec<&'static str> },
    /// No loaded interface answers to the name.
    Unresolved,
}

/// One `conforms-to` row, positioned at its annotation in a project `.rbs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceFinding {
    pub kind: ConformanceKind,
    /// The annotated class/module, fully qualified, no leading `::`.
    pub class_name: &'static str,
    /// The interface as written in the directive, leading `::` stripped.
    pub interface_name: String,
    /// Absolute path of the `.rbs` carrying the annotation.
    pub file: &'static str,
    /// Byte offset of the annotation (`%a{…}`) in that file.
    pub start_offset: usize,
    pub end_offset: usize,
}

impl ConformanceFinding {
    /// The rule id this finding surfaces as.
    #[must_use]
    pub fn rule_id(&self) -> &'static str {
        match self.kind {
            ConformanceKind::Unsatisfied { .. } => UNSATISFIED_CONFORMANCE,
            ConformanceKind::Unresolved => RBS_EXTENDED_UNRESOLVED,
        }
    }

    /// The reference's message, byte for byte
    /// (`DiagnosticAggregator#build_{unsatisfied,unresolved}_conformance_diagnostic`).
    #[must_use]
    pub fn message(&self) -> String {
        let (c, i) = (self.class_name, &self.interface_name);
        match &self.kind {
            ConformanceKind::Unresolved => format!(
                "`{c}` declares `conforms-to {i}` but interface `{i}` is not loaded. Check for a \
                 typo or add the `sig`/library that declares it to the RBS load path."
            ),
            ConformanceKind::Unsatisfied { missing } => {
                let noun = if missing.len() == 1 {
                    "required method".to_string()
                } else {
                    format!("{} required methods", missing.len())
                };
                let list: Vec<String> = missing.iter().map(|m| format!("`#{m}`")).collect();
                format!(
                    "`{c}` declares `conforms-to {i}` but does not provide {noun}: {}. Implement \
                     the missing method(s) or remove the directive.",
                    list.join(", ")
                )
            }
        }
    }
}

/// Extract the interface name from a `rigor:v1:conforms-to <Interface>`
/// annotation string, leading `::` stripped — the reference's
/// `RbsExtended.parse_conforms_to_annotation`:
/// `\Arigor:v1:conforms-to\s+(?<interface>(?:::)?(?:[A-Z]\w*::)*_[A-Za-z]\w*)\s*\z`.
#[must_use]
pub fn parse_conforms_to(annotation: &str) -> Option<&str> {
    // Ruby's `\s` is ASCII `[ \t\r\n\f\v]`; `\w` is ASCII `[A-Za-z0-9_]`.
    let is_space = |c: char| matches!(c, ' ' | '\t' | '\r' | '\n' | '\x0c' | '\x0b');
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let rest = annotation.strip_prefix("rigor:v1:conforms-to")?;
    if !rest.starts_with(is_space) {
        return None;
    }
    let name = rest.trim_start_matches(is_space).trim_end_matches(is_space);
    let bare = name.strip_prefix("::").unwrap_or(name);
    let mut segs: Vec<&str> = bare.split("::").collect();
    let leaf = segs.pop()?;
    for seg in segs {
        let mut cs = seg.chars();
        if !cs.next().is_some_and(|c| c.is_ascii_uppercase()) || !cs.all(is_word) {
            return None;
        }
    }
    let mut cs = leaf.chars();
    if cs.next() != Some('_')
        || !cs.next().is_some_and(|c| c.is_ascii_alphabetic())
        || !cs.all(is_word)
    {
        return None;
    }
    Some(bare)
}

/// Kinds sharing RBS's constant namespace, for the duplicate-declaration check.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConstKind {
    Class,
    Module,
    Constant,
    ClassAlias,
}

/// One member of an interface body, in declaration order.
#[derive(Clone, Debug)]
enum IfaceMember {
    Def(&'static str),
    Alias {
        new: &'static str,
        old: &'static str,
    },
}

#[derive(Clone, Debug)]
struct IfaceDecl {
    /// Lexical scopes the interface was declared in (innermost last).
    ctx: Vec<&'static str>,
    /// `include`s as written: `(name, absolute)`.
    includes: Vec<(&'static str, bool)>,
    members: Vec<IfaceMember>,
}

#[derive(Clone, Debug)]
struct IfaceSlot {
    decl: IfaceDecl,
    /// Declared more than once, or in a file the reference may have dropped:
    /// which body (if any) the reference loaded is unknowable.
    ambiguous: bool,
    /// Declared by a project signature file (`None` = bundled / catalogue).
    file: Option<&'static str>,
}

/// First-declaration order of a class: bundled declarations first in ingest
/// order, then project ones by `(sorted file path, offset)` — the reference
/// loads `sig/` files sorted, so `env.class_decls` iterates in that order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Seen {
    Bundled(usize),
    Project(&'static str, usize),
}

#[derive(Clone, Debug)]
struct AnnotationRecord {
    class: &'static str,
    text: String,
    file: &'static str,
    start: usize,
    end: usize,
}

/// Builder-side accumulator (lives inside [`super::Builder`]).
#[derive(Default)]
pub(super) struct ConformanceBuilder {
    /// The project signature file currently being ingested; `None` for bundled
    /// RBS (embedded core/stdlib/overlay, plugins, the override dir).
    current_file: Option<&'static str>,
    seq: usize,
    const_ns: HashMap<&'static str, (ConstKind, Option<&'static str>)>,
    type_aliases: HashMap<&'static str, Option<&'static str>>,
    globals: HashMap<&'static str, Option<&'static str>>,
    interfaces: HashMap<&'static str, IfaceSlot>,
    suspect_files: HashSet<&'static str>,
    file_classes: HashMap<&'static str, Vec<&'static str>>,
    instance_members: HashMap<&'static str, HashSet<&'static str>>,
    def_conflicts: HashSet<&'static str>,
    explicit_super: HashMap<&'static str, String>,
    /// The first declaration's type-parameter shape per class/module, across
    /// ALL sources (see [`param_shape`]).
    type_params: HashMap<&'static str, ParamShape>,
    first_seen: HashMap<&'static str, Seen>,
    annotations: Vec<AnnotationRecord>,
}

/// The frozen tables [`CoreData::conformance_findings`] reads.
#[derive(Default)]
pub(super) struct ConformanceData {
    interfaces: HashMap<&'static str, IfaceSlot>,
    /// Classes the reference cannot build or may not have loaded whole.
    unbuildable: HashSet<&'static str>,
    /// Annotations in the reference's `env.class_decls` order, with those in a
    /// possibly-quarantined file already dropped.
    annotations: Vec<AnnotationRecord>,
}

/// The type-parameter shape of one class/module declaration, as
/// `RBS::Environment::{Class,Module}Entry#validate_type_params` compares it
/// after renaming: `(variance, unchecked)` per parameter. `None` when a
/// parameter carries a bound or a default, whose equality the port does not
/// model: any second declaration then counts as a mismatch (silent).
type ParamShape = Option<Vec<(u8, bool)>>;

fn param_shape(params: NodeList<'_>) -> ParamShape {
    let mut out = Vec::new();
    for p in params.iter() {
        let Node::TypeParam(tp) = p else {
            return None;
        };
        if tp.upper_bound().is_some() || tp.lower_bound().is_some() || tp.default_type().is_some() {
            return None;
        }
        let variance = match tp.variance() {
            TypeParamVariance::Invariant => 0,
            TypeParamVariance::Covariant => 1,
            TypeParamVariance::Contravariant => 2,
        };
        out.push((variance, tp.unchecked()));
    }
    Some(out)
}

/// Written `TypeNameNode` → (`A::B::_C` without leading `::`, absolute?).
fn written_name(tn: &TypeNameNode) -> Option<(&'static str, bool)> {
    let ns = tn.namespace();
    let mut parts: Vec<String> = Vec::new();
    for seg in ns.path().iter() {
        if let Node::Symbol(sym) = seg {
            parts.push(sym.as_str().to_string());
        }
    }
    parts.push(type_name_str(tn)?.to_string());
    Some((intern(&parts.join("::")), ns.absolute()))
}

impl ConformanceBuilder {
    /// Enter (`Some(path)`) or leave (`None`) a PROJECT signature file.
    pub(super) fn set_project_file(&mut self, file: Option<&'static str>) {
        self.current_file = file;
    }

    /// Walk one parsed source's declarations.
    pub(super) fn walk(&mut self, decls: NodeList<'_>) {
        self.walk_decls(decls, &[]);
    }

    fn walk_decls(&mut self, decls: NodeList<'_>, enclosing: &[&'static str]) {
        for decl in decls.iter() {
            match decl {
                Node::Class(c) => self.class_decl(&c, enclosing),
                Node::Module(m) => self.module_decl(&m, enclosing),
                Node::Interface(i) => self.interface_decl(&i, enclosing),
                Node::Constant(c) => {
                    let q = qualified_name(enclosing, &c.name());
                    self.declare_const(q, ConstKind::Constant);
                }
                Node::ClassAlias(a) => {
                    let q = qualified_name(enclosing, &a.new_name());
                    self.declare_const(q, ConstKind::ClassAlias);
                }
                Node::ModuleAlias(a) => {
                    let q = qualified_name(enclosing, &a.new_name());
                    self.declare_const(q, ConstKind::ClassAlias);
                }
                Node::TypeAlias(ta) => {
                    let q = qualified_name(enclosing, &ta.name());
                    let file = self.current_file;
                    if let Some(prev) = self.type_aliases.insert(q, file) {
                        self.collide(prev);
                    }
                }
                Node::Global(g) => {
                    let name = intern(g.name().as_str());
                    let file = self.current_file;
                    if let Some(prev) = self.globals.insert(name, file) {
                        self.collide(prev);
                    }
                }
                _ => {}
            }
        }
    }

    /// A duplicate declaration: the reference drops the LATER project file; the
    /// port cannot trust its own file order, so both project files turn
    /// suspect (a bundled first declarer stays loaded, as it does upstream).
    fn collide(&mut self, prev: Option<&'static str>) {
        if let Some(f) = self.current_file {
            self.suspect_files.insert(f);
        }
        if let Some(f) = prev {
            self.suspect_files.insert(f);
        }
    }

    fn declare_const(&mut self, q: &'static str, kind: ConstKind) {
        let file = self.current_file;
        match self.const_ns.get(q).copied() {
            None => {
                self.const_ns.insert(q, (kind, file));
            }
            Some((k, _)) if k == kind && matches!(kind, ConstKind::Class | ConstKind::Module) => {
                // A reopen — fine.
            }
            Some((_, prev)) => self.collide(prev),
        }
    }

    fn see_class(&mut self, q: &'static str, start: usize) {
        let seen = match self.current_file {
            Some(f) => Seen::Project(f, start),
            None => {
                self.seq += 1;
                Seen::Bundled(self.seq)
            }
        };
        self.first_seen
            .entry(q)
            .and_modify(|s| {
                if seen < *s {
                    *s = seen;
                }
            })
            .or_insert(seen);
        if let Some(f) = self.current_file {
            self.file_classes.entry(f).or_default().push(q);
        }
    }

    /// `GenericParameterMismatchError`: every declaration of a class/module
    /// must repeat the first one's type parameters (up to renaming), so a
    /// project `class Set` reopening core's `class Set[unchecked out A]` makes
    /// `Set` unbuildable, and with it every class that has it as an ancestor
    /// (oracle-measured: `Enumerable`, and through it `Hash` / `Struct`
    /// subclasses, `File`, `StringIO`).
    fn see_type_params(&mut self, q: &'static str, params: NodeList<'_>) {
        let shape = param_shape(params);
        match self.type_params.get(q) {
            None => {
                self.type_params.insert(q, shape);
            }
            Some(first) => {
                if first.is_none() || shape.is_none() || *first != shape {
                    self.def_conflicts.insert(q);
                }
            }
        }
    }

    fn record_annotations(&mut self, q: &'static str, annotations: NodeList<'_>) {
        let Some(file) = self.current_file else {
            return;
        };
        for a in annotations.iter() {
            if let Node::Annotation(an) = a {
                let loc = an.location();
                self.annotations.push(AnnotationRecord {
                    class: q,
                    text: an.string().as_str().to_string(),
                    file,
                    start: usize::try_from(loc.start()).unwrap_or(0),
                    end: usize::try_from(loc.end()).unwrap_or(0),
                });
            }
        }
    }

    fn class_decl(&mut self, c: &ClassNode, enclosing: &[&'static str]) {
        let q = qualified_name(enclosing, &c.name());
        self.declare_const(q, ConstKind::Class);
        self.see_class(q, usize::try_from(c.location().start()).unwrap_or(0));
        self.see_type_params(q, c.type_params());
        if let Some(sup) = c.super_class().and_then(|s| written_name(&s.name())) {
            let text = format!("{}{}", if sup.1 { "::" } else { "" }, sup.0);
            match self.explicit_super.get(q) {
                Some(prev) if *prev != text => {
                    self.def_conflicts.insert(q);
                }
                Some(_) => {}
                None => {
                    self.explicit_super.insert(q, text);
                }
            }
        }
        self.record_annotations(q, c.annotations());
        let child: Vec<&'static str> = enclosing.iter().copied().chain([q]).collect();
        self.members(q, c.members(), &child);
    }

    fn module_decl(&mut self, m: &ModuleNode, enclosing: &[&'static str]) {
        let q = qualified_name(enclosing, &m.name());
        self.declare_const(q, ConstKind::Module);
        self.see_class(q, usize::try_from(m.location().start()).unwrap_or(0));
        self.see_type_params(q, m.type_params());
        self.record_annotations(q, m.annotations());
        let child: Vec<&'static str> = enclosing.iter().copied().chain([q]).collect();
        self.members(q, m.members(), &child);
    }

    /// Instance-side member names of one class body, for the duplicate-member
    /// build failure, plus the nested declarations (walked as declarations).
    fn members(&mut self, q: &'static str, members: NodeList<'_>, child: &[&'static str]) {
        let mut defined: Vec<&'static str> = Vec::new();
        for member in members.iter() {
            match member {
                Node::MethodDefinition(md) => {
                    if !md.overloading()
                        && matches!(
                            md.kind(),
                            MethodDefinitionKind::Instance
                                | MethodDefinitionKind::SingletonInstance
                        )
                    {
                        defined.push(intern(md.name().as_str()));
                    }
                }
                Node::AttrReader(a) if matches!(a.kind(), AttributeKind::Instance) => {
                    defined.push(intern(a.name().as_str()));
                }
                Node::AttrWriter(a) if matches!(a.kind(), AttributeKind::Instance) => {
                    defined.push(intern(&format!("{}=", a.name().as_str())));
                }
                Node::AttrAccessor(a) if matches!(a.kind(), AttributeKind::Instance) => {
                    defined.push(intern(a.name().as_str()));
                    defined.push(intern(&format!("{}=", a.name().as_str())));
                }
                Node::Alias(a) if matches!(a.kind(), AliasKind::Instance) => {
                    defined.push(intern(a.new_name().as_str()));
                }
                Node::Class(inner) => self.class_decl(&inner, child),
                Node::Module(inner) => self.module_decl(&inner, child),
                Node::Interface(i) => self.interface_decl(&i, child),
                Node::Constant(c) => {
                    let cq = qualified_name(child, &c.name());
                    self.declare_const(cq, ConstKind::Constant);
                }
                Node::ClassAlias(a) => {
                    let cq = qualified_name(child, &a.new_name());
                    self.declare_const(cq, ConstKind::ClassAlias);
                }
                Node::ModuleAlias(a) => {
                    let cq = qualified_name(child, &a.new_name());
                    self.declare_const(cq, ConstKind::ClassAlias);
                }
                Node::TypeAlias(ta) => {
                    let aq = qualified_name(child, &ta.name());
                    let file = self.current_file;
                    if let Some(prev) = self.type_aliases.insert(aq, file) {
                        self.collide(prev);
                    }
                }
                _ => {}
            }
        }
        let set = self.instance_members.entry(q).or_default();
        let mut conflict = false;
        for name in defined {
            conflict |= !set.insert(name);
        }
        if conflict {
            self.def_conflicts.insert(q);
        }
    }

    fn interface_decl(&mut self, i: &InterfaceNode, enclosing: &[&'static str]) {
        let q = qualified_name(enclosing, &i.name());
        let mut decl = IfaceDecl {
            ctx: enclosing.to_vec(),
            includes: Vec::new(),
            members: Vec::new(),
        };
        for member in i.members().iter() {
            match member {
                Node::MethodDefinition(md) => {
                    decl.members
                        .push(IfaceMember::Def(intern(md.name().as_str())));
                }
                Node::Alias(a) => decl.members.push(IfaceMember::Alias {
                    new: intern(a.new_name().as_str()),
                    old: intern(a.old_name().as_str()),
                }),
                Node::Include(inc) => {
                    if let Some(w) = written_name(&inc.name()) {
                        decl.includes.push(w);
                    }
                }
                _ => {}
            }
        }
        let file = self.current_file;
        match self.interfaces.get_mut(q) {
            Some(slot) => {
                slot.ambiguous = true;
                let prev = slot.file;
                self.collide(prev);
            }
            None => {
                self.interfaces.insert(
                    q,
                    IfaceSlot {
                        decl,
                        ambiguous: false,
                        file,
                    },
                );
            }
        }
    }

    /// Load the vendored capability-role catalogue AFTER the project's own
    /// signatures, one INTERFACE at a time, skipping any name already declared
    /// — the reference's `add_capability_role_signatures` (#928/#976). The
    /// granularity is the declaration, not the file: a project that declares
    /// its own `_Closable` keeps it and still gets `_ClosableStream`.
    pub(super) fn ingest_capability_roles(&mut self) {
        let Ok(sig) = parse(CAPABILITY_ROLES_RBS) else {
            return;
        };
        self.current_file = None;
        for decl in sig.declarations().iter() {
            if let Node::Interface(i) = decl {
                let q = qualified_name(&[], &i.name());
                if !self.interfaces.contains_key(q) {
                    self.interface_decl(&i, &[]);
                }
            }
        }
    }

    pub(super) fn finish(mut self) -> ConformanceData {
        // Interfaces declared in a possibly-dropped file: unknowable body.
        for slot in self.interfaces.values_mut() {
            if slot.file.is_some_and(|f| self.suspect_files.contains(f)) {
                slot.ambiguous = true;
            }
        }
        let mut unbuildable = self.def_conflicts;
        for f in &self.suspect_files {
            if let Some(classes) = self.file_classes.get(f) {
                unbuildable.extend(classes.iter().copied());
            }
        }
        let suspect = self.suspect_files;
        let first_seen = self.first_seen;
        let mut annotations: Vec<AnnotationRecord> = self
            .annotations
            .into_iter()
            .filter(|a| !suspect.contains(a.file))
            .collect();
        annotations.sort_by(|a, b| {
            let ra = first_seen.get(a.class);
            let rb = first_seen.get(b.class);
            (ra, a.file, a.start).cmp(&(rb, b.file, b.start))
        });
        ConformanceData {
            interfaces: self.interfaces,
            unbuildable,
            annotations,
        }
    }
}

/// Union of presence answers: present if any part has it, absent only when
/// every part is decided.
fn any_present(parts: impl IntoIterator<Item = Option<bool>>) -> Option<bool> {
    let mut unknown = false;
    for p in parts {
        match p {
            Some(true) => return Some(true),
            Some(false) => {}
            None => unknown = true,
        }
    }
    if unknown {
        None
    } else {
        Some(false)
    }
}

/// Resolve a written reference `name` against lexical scopes `ctx` (innermost
/// last), longest prefix first, then top level.
fn resolve_in_ctx(
    interfaces: &HashMap<&'static str, IfaceSlot>,
    name: &str,
    absolute: bool,
    ctx: &[&'static str],
) -> Option<&'static str> {
    if !absolute {
        for scope in ctx.iter().rev() {
            let cand = format!("{scope}::{name}");
            if let Some((k, _)) = interfaces.get_key_value(cand.as_str()) {
                return Some(k);
            }
        }
    }
    interfaces.get_key_value(name).map(|(k, _)| *k)
}

impl ConformanceData {
    /// The interface's required members in `RBS::DefinitionBuilder#build_interface`
    /// order (oracle-pinned with rbs 4.2.0): the includes' members first, LAST
    /// include first, each recursively; then the own members in declaration
    /// order, an alias placing its (own) target before itself. `None` wherever
    /// that build would raise or the body is unknowable — a member defined
    /// twice (own or through an include), an alias to nothing, an
    /// unresolvable or ambiguous include — so the caller stays silent.
    fn required(&self, q: &'static str, depth: usize) -> Option<Vec<&'static str>> {
        if depth > 16 {
            return None;
        }
        let slot = self.interfaces.get(q)?;
        if slot.ambiguous {
            return None;
        }
        let decl = &slot.decl;
        let mut out: Vec<&'static str> = Vec::new();
        for (name, absolute) in decl.includes.iter().rev() {
            let target = resolve_in_ctx(&self.interfaces, name, *absolute, &decl.ctx)?;
            for m in self.required(target, depth + 1)? {
                if out.contains(&m) {
                    return None;
                }
                out.push(m);
            }
        }
        let inherited = out.len();
        let own_defs: Vec<&'static str> = decl
            .members
            .iter()
            .filter_map(|m| match m {
                IfaceMember::Def(n) => Some(*n),
                IfaceMember::Alias { .. } => None,
            })
            .collect();
        let mut seen_defs: HashSet<&'static str> = HashSet::new();
        for d in &own_defs {
            if !seen_defs.insert(d) || out[..inherited].contains(d) {
                return None;
            }
        }
        for m in &decl.members {
            match m {
                IfaceMember::Def(n) => {
                    if !out.contains(n) {
                        out.push(n);
                    }
                }
                IfaceMember::Alias { new, old } => {
                    if !out.contains(old) {
                        if own_defs.contains(old) {
                            out.push(old);
                        } else {
                            return None;
                        }
                    }
                    if out.contains(new) {
                        return None;
                    }
                    out.push(new);
                }
            }
        }
        Some(out)
    }
}

impl CoreData {
    /// Scan every `rigor:v1:conforms-to` directive in the project's signature
    /// files and return the rows the reference emits for them, in its order.
    /// FP-safe by construction: every case where the reference's environment
    /// may not load, build or resolve what rigor-rs sees comes back silent.
    #[must_use]
    pub fn conformance_findings(&self) -> Vec<ConformanceFinding> {
        let data = &self.conformance;
        let mut out = Vec::new();
        for ann in &data.annotations {
            let Some(iname) = parse_conforms_to(&ann.text) else {
                continue;
            };
            // `ConformanceChecker#candidate_interface_names`: the class's
            // namespace prefixes, longest first, then the bare name.
            let parts: Vec<&str> = ann.class.split("::").collect();
            let mut candidates: Vec<String> = (1..=parts.len())
                .rev()
                .map(|n| format!("{}::{iname}", parts[..n].join("::")))
                .collect();
            candidates.push(iname.to_string());
            let resolved = candidates
                .iter()
                .find_map(|c| data.interfaces.get_key_value(c.as_str()).map(|(k, _)| *k));
            let finding = |kind| ConformanceFinding {
                kind,
                class_name: ann.class,
                interface_name: iname.to_string(),
                file: ann.file,
                start_offset: ann.start,
                end_offset: ann.end,
            };
            let Some(iface) = resolved else {
                out.push(finding(ConformanceKind::Unresolved));
                continue;
            };
            let Some(required) = data.required(iface, 0) else {
                continue;
            };
            if !self.conformance_class_buildable(ann.class) {
                continue;
            }
            // Every member must be DECIDED: one the port cannot place on the
            // reference's surface either way silences the whole row, because a
            // row with a shorter or longer list is as wrong as a spurious one.
            let mut missing: Vec<&'static str> = Vec::new();
            let mut decided = true;
            for m in required {
                match self.rbs_instance_has(ann.class, m, 0) {
                    Some(true) => {}
                    Some(false) => missing.push(m),
                    None => {
                        decided = false;
                        break;
                    }
                }
            }
            if decided && !missing.is_empty() {
                out.push(finding(ConformanceKind::Unsatisfied { missing }));
            }
        }
        out
    }

    /// Whether the reference can build `class`'s instance definition as far as
    /// rigor-rs can tell: neither it nor any ancestor (the implicit
    /// `Object`/`Kernel`/`BasicObject` included) is known to fail, and no alias
    /// on the chain points at nothing. An incomplete chain is left to
    /// [`Self::qualified_class_has_method`], which reads it as "present".
    fn conformance_class_buildable(&self, class: &'static str) -> bool {
        let data = &self.conformance;
        let (chain, _complete) = self.qualified_ancestors(class);
        let implicit = ["Object", "Kernel", "BasicObject"];
        if chain
            .iter()
            .copied()
            .chain(implicit)
            .any(|c| data.unbuildable.contains(c))
        {
            return false;
        }
        for anc in &chain {
            if let Some(entry) = self.qualified.get(anc) {
                for old in entry.aliases.values() {
                    if self.rbs_instance_has(anc, old, 0) != Some(true) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Whether `class`'s RBS instance definition — what
    /// `RBS::DefinitionBuilder#build_instance` returns and the reference's
    /// `ConformanceChecker` reads as `provided` — has `method`. `None` when
    /// the port cannot tell (an unresolvable reference, an unbuildable or
    /// unknown entry): the caller then stays silent.
    ///
    /// Deliberately NOT [`Self::qualified_class_has_method`]: that helper
    /// answers "present" whenever unsure, which is the safe side for an
    /// undefined-method witness but not here, where a member wrongly counted
    /// present shortens the reported list. It also gives a module `Object`'s
    /// whole chain, while RBS gives a module only its self types' OWN
    /// surfaces (default `Object`: `Object` plus `Kernel`, not `BasicObject`,
    /// oracle-measured: `#==` and `#!` are reported missing on a module).
    fn rbs_instance_has(&self, class: &str, method: &str, depth: usize) -> Option<bool> {
        if depth > 32 {
            return None;
        }
        let (&key, entry) = self.qualified.get_key_value(class)?;
        if entry.instance_unbuildable {
            return None;
        }
        let mut parts: Vec<Option<bool>> = Vec::new();
        if entry.is_module {
            // `build_instance`: each self type's `define_instance` (an
            // interface self type contributes its members), default `Object`.
            if entry.self_types_written.is_empty() {
                parts.push(self.rbs_define_instance_has("Object", method, depth + 1));
            }
            for (w, ctx) in &entry.self_types_written {
                parts.push(self.rbs_ref_has(w, ctx, method, depth + 1));
            }
        } else {
            // `build_instance`: the superclass's whole definition first.
            match &entry.superclass_written {
                Some((w, ctx)) => parts.push(
                    self.resolve_written_ref(w, ctx)
                        .and_then(|s| self.rbs_instance_has(s, method, depth + 1)),
                ),
                None if key == "BasicObject" => {}
                None => parts.push(self.rbs_instance_has("Object", method, depth + 1)),
            }
        }
        parts.push(self.rbs_define_instance_has(key, method, depth + 1));
        any_present(parts)
    }

    /// `RBS::DefinitionBuilder#define_instance`: the entry's own members, then
    /// its included and prepended modules' (and interfaces'), recursively.
    /// No superclass, and no self types (those only resolve aliases there).
    fn rbs_define_instance_has(&self, class: &str, method: &str, depth: usize) -> Option<bool> {
        if depth > 32 {
            return None;
        }
        let entry = self.qualified.get(class)?;
        if entry.instance_unbuildable {
            return None;
        }
        if entry.methods.contains_key(method)
            || entry.attr_methods.contains(method)
            || entry.aliases.contains_key(method)
        {
            return Some(true);
        }
        any_present(
            entry
                .includes_written
                .iter()
                .chain(&entry.prepends_written)
                .map(|(w, ctx)| self.rbs_ref_has(w, ctx, method, depth + 1)),
        )
    }

    /// A written `include` / `prepend` / self-type reference: an interface
    /// contributes its (built) members, a module its `define_instance`.
    fn rbs_ref_has(
        &self,
        written: &str,
        ctx: &[&'static str],
        method: &str,
        depth: usize,
    ) -> Option<bool> {
        let (name, absolute) = match written.strip_prefix("::") {
            Some(rest) => (rest, true),
            None => (written, false),
        };
        if name.rsplit("::").next().is_some_and(|leaf| leaf.starts_with('_')) {
            let data = &self.conformance;
            let iface = resolve_in_ctx(&data.interfaces, name, absolute, ctx)?;
            return Some(data.required(iface, 0)?.contains(&method));
        }
        let target = self.resolve_written_ref(written, ctx)?;
        self.rbs_define_instance_has(target, method, depth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the index over a throwaway project `sig/` holding `files`.
    fn project(files: &[(&str, &str)]) -> (CoreData, std::path::PathBuf) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rigor-conformance-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let sig = dir.join("sig");
        std::fs::create_dir_all(&sig).unwrap();
        for (name, body) in files {
            std::fs::write(sig.join(name), body).unwrap();
        }
        (CoreData::load_for_project(&[], &[sig]), dir)
    }

    fn rows(files: &[(&str, &str)]) -> Vec<(String, &'static str, String)> {
        let (data, dir) = project(files);
        let out = data
            .conformance_findings()
            .into_iter()
            .map(|f| {
                let file = std::path::Path::new(f.file)
                    .file_name()
                    .unwrap()
                    .to_string_lossy();
                (
                    format!("{file}@{}", f.start_offset),
                    f.rule_id(),
                    f.message(),
                )
            })
            .collect();
        std::fs::remove_dir_all(dir).ok();
        out
    }

    /// The build-failure detector must not flag the bundled ancestors every
    /// class inherits — that would silence the rule for everything.
    #[test]
    fn bundled_ancestors_are_buildable() {
        let data = CoreData::load();
        for name in [
            "BasicObject",
            "Object",
            "Kernel",
            "Comparable",
            "Enumerable",
            "IO",
            "String",
        ] {
            assert!(
                !data.conformance.unbuildable.contains(name),
                "{name} flagged unbuildable"
            );
        }
        for role in [
            "_Closable",
            "_RewindableStream",
            "_ClosableStream",
            "_FileDescriptorBacked",
            "_Callable",
        ] {
            assert!(
                data.conformance.interfaces.contains_key(role),
                "{role} not loaded"
            );
        }
    }

    /// Issue #129 acceptance: the catalogue loads per DECLARATION — a project
    /// `_Closable` wins (and its extra member is required) while the shipped
    /// `_ClosableStream` still loads; a conforming class and an unannotated one
    /// stay silent; an unknown name is the unresolved row.
    #[test]
    fn per_declaration_catalogue_and_controls() {
        let got = rows(&[(
            "a.rbs",
            "interface _Closable\n  def close: () -> void\n  def shut_hard: () -> void\nend\n\
             %a{rigor:v1:conforms-to _Closable}\nclass P\n  def close: () -> void\nend\n\
             %a{rigor:v1:conforms-to _ClosableStream}\nclass Ok\n  def close: () -> void\n  def closed?: () -> bool\nend\n\
             class Bare\nend\n\
             %a{rigor:v1:conforms-to _NoSuchRoleZzz}\nclass U\nend\n",
        )]);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].1, UNSATISFIED_CONFORMANCE);
        assert!(got[0].2.starts_with("`P` declares `conforms-to _Closable` but does not provide required method: `#shut_hard`."));
        assert_eq!(got[1].1, RBS_EXTENDED_UNRESOLVED);
    }

    /// A file the reference quarantines (any duplicate declaration) loses every
    /// annotation; a class whose definition cannot be built is skipped — both
    /// oracle-measured silences.
    #[test]
    fn quarantine_and_build_failures_are_silent() {
        let got = rows(&[
            ("a.rbs", "RUBY_VERSION: String\n%a{rigor:v1:conforms-to _Closable}\nclass Q\nend\n"),
            ("b.rbs", "%a{rigor:v1:conforms-to _Closable}\nclass D\n  def x: () -> void\n  def x: () -> void\nend\n"),
            ("c.rbs", "%a{rigor:v1:conforms-to _Closable}\nclass String\n  def upcase: () -> String\nend\n"),
            ("d.rbs", "class Base\n  attr_reader z: Integer\n  def z: () -> Integer\nend\n%a{rigor:v1:conforms-to _Closable}\nclass Sub < Base\nend\n"),
            ("e.rbs", "%a{rigor:v1:conforms-to _Closable}\nclass Al\n  alias r nosuch\nend\n"),
            ("f.rbs", "%a{rigor:v1:conforms-to _Closable}\nclass Fires\nend\n"),
        ]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].2.starts_with("`Fires` declares"));
    }

    /// Oracle-measured on Ruby 4.0 at `e59b7b89` (PR #150 audit): a module's
    /// surface is its self types' OWN (default `Object`, which brings `Kernel`
    /// but not `BasicObject`), so `#==` / `#!` are missing there; a generic
    /// class reopened without its parameters fails to build, silencing it and
    /// every class below it.
    #[test]
    fn module_surface_and_generic_mismatch() {
        let got = rows(&[(
            "a.rbs",
            "interface _Eq\n  def ==: (untyped) -> bool\n  def !: () -> bool\n  def inspect: () -> String\nend\n\
             %a{rigor:v1:conforms-to _Eq}\nmodule ModEq\nend\n\
             %a{rigor:v1:conforms-to _Eq}\nclass ClsEq\nend\n\
             class Set\nend\n\
             %a{rigor:v1:conforms-to _Closable}\nclass SetSub < Set[Integer]\nend\n\
             %a{rigor:v1:conforms-to _Closable}\nmodule Enumerable\nend\n\
             %a{rigor:v1:conforms-to _Closable}\nclass HashSub < Hash[Integer, Integer]\nend\n",
        )]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0].2.starts_with("`ModEq` declares `conforms-to _Eq` but does not provide 2 required methods: `#==`, `#!`."),
            "{got:?}"
        );
        // Renamed parameters are the same parameters: the reopen still builds.
        let got = rows(&[(
            "a.rbs",
            "%a{rigor:v1:conforms-to _Closable}\nclass Array[unchecked out T]\nend\n",
        )]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].2.starts_with("`Array` declares `conforms-to _Closable`"), "{got:?}");
    }

    /// The reference collects project signature files into a set of expanded
    /// paths: a file reached through two `signature_paths:` entries loads once
    /// (it is neither reported twice nor a duplicate declaration).
    #[test]
    fn a_file_reached_twice_loads_once() {
        let (_, dir) = project(&[(
            "a.rbs",
            "%a{rigor:v1:conforms-to _Closable}\nclass Twice\n  def x: () -> void\nend\n",
        )]);
        let sig = dir.join("sig");
        let data = CoreData::load_for_project(&[], &[sig.clone(), dir.join(".").join("sig")]);
        let got = data.conformance_findings();
        std::fs::remove_dir_all(dir).ok();
        assert_eq!(got.len(), 1, "{:?}", got.iter().map(ConformanceFinding::message).collect::<Vec<_>>());
    }

    /// `RBS::DefinitionBuilder#build_interface` order: includes first (last
    /// include first), then own members with an alias's target before it.
    #[test]
    fn required_member_order_matches_rbs() {
        let got = rows(&[(
            "a.rbs",
            "interface _Z\n  def zm: () -> void\nend\ninterface _A\n  def am: () -> void\nend\n\
             interface _F\n  alias bb aa\n  include _Z\n  include _A\n  def aa: () -> void\n  def cc: () -> void\nend\n\
             %a{rigor:v1:conforms-to _F}\nclass C\nend\n",
        )]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0]
                .2
                .contains("5 required methods: `#am`, `#zm`, `#aa`, `#bb`, `#cc`."),
            "{got:?}"
        );
    }

    /// The vendored catalogue is byte-identical to the pinned reference's
    /// (skipped when the submodule is not checked out).
    #[test]
    fn vendored_catalogue_matches_the_pin() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../reference/rigor/data/capability_roles");
        let Ok(pinned) = std::fs::read_to_string(dir.join("capability_roles.rbs")) else {
            return;
        };
        assert_eq!(
            pinned, CAPABILITY_ROLES_RBS,
            "re-sync vendor/capability_roles/"
        );
        let files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(files.len(), 1, "a new catalogue file upstream: {files:?}");
    }

    #[test]
    fn parses_the_directive_like_the_reference_regex() {
        assert_eq!(
            parse_conforms_to("rigor:v1:conforms-to _Closable"),
            Some("_Closable")
        );
        assert_eq!(
            parse_conforms_to("rigor:v1:conforms-to ::_Closable"),
            Some("_Closable")
        );
        assert_eq!(
            parse_conforms_to("rigor:v1:conforms-to   _Callable  "),
            Some("_Callable")
        );
        assert_eq!(
            parse_conforms_to("rigor:v1:conforms-to Foo::Bar::_X1"),
            Some("Foo::Bar::_X1")
        );
        assert_eq!(parse_conforms_to("rigor:v1:conforms-to _1x"), None);
        assert_eq!(parse_conforms_to("rigor:v1:conforms-to Closable"), None);
        assert_eq!(parse_conforms_to("rigor:v1:conforms-to foo::_X"), None);
        assert_eq!(parse_conforms_to("rigor:v1:conforms-to_X"), None);
        assert_eq!(parse_conforms_to(" rigor:v1:conforms-to _X"), None);
        assert_eq!(parse_conforms_to("rigor:v1:conforms-to _X _Y"), None);
        assert_eq!(parse_conforms_to("rigor:v1:return: Integer"), None);
    }

    #[test]
    fn messages_match_the_reference() {
        let f = ConformanceFinding {
            kind: ConformanceKind::Unsatisfied {
                missing: vec!["closed?"],
            },
            class_name: "Gate",
            interface_name: "_ClosableStream".into(),
            file: "/x/sig/a.rbs",
            start_offset: 0,
            end_offset: 40,
        };
        assert_eq!(
            f.message(),
            "`Gate` declares `conforms-to _ClosableStream` but does not provide required method: \
             `#closed?`. Implement the missing method(s) or remove the directive."
        );
        let two = ConformanceFinding {
            kind: ConformanceKind::Unsatisfied {
                missing: vec!["zeta", "alpha"],
            },
            ..f.clone()
        };
        assert!(two
            .message()
            .contains("does not provide 2 required methods: `#zeta`, `#alpha`."));
        let u = ConformanceFinding {
            kind: ConformanceKind::Unresolved,
            ..f
        };
        assert_eq!(u.rule_id(), RBS_EXTENDED_UNRESOLVED);
        assert_eq!(
            u.message(),
            "`Gate` declares `conforms-to _ClosableStream` but interface `_ClosableStream` is not \
             loaded. Check for a typo or add the `sig`/library that declares it to the RBS load path."
        );
    }
}
