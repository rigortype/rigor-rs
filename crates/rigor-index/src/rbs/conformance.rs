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
//! [`super::Builder::ingest`] folds into the class tables. It records its OWN
//! model of every declaration (bundled and project alike) because the question
//! it answers is not "which methods does `String` have" but "would the
//! reference's RBS environment LOAD this file, BUILD this class's definition
//! and BUILD this interface's definition". The reference is silent wherever one
//! of those fails, and it fails in more ways than a deny-list can enumerate
//! (the PR #150 review found seven families). So the port answers with an
//! ALLOW-LIST ([`closure`]): a row fires only when every declaration the
//! reference's `RBS::DefinitionBuilder#build_instance` touches is provably
//! buildable, and every name it resolves is provably the name the reference
//! resolves. Anything the port cannot positively verify is silent.
//!
//! The recording below is deliberately exhaustive about SHAPE (every member
//! kind, header, type parameter, directive) and records an `unsupported` bit
//! for anything it does not model, so the allow-list can refuse it.

use std::collections::{HashMap, HashSet};

use ruby_rbs::node::{
    parse, AliasKind, AttributeKind, ClassNode, InterfaceNode, MethodDefinitionKind, ModuleNode,
    Node, NodeList, TypeNameNode, TypeParamVariance,
};

use super::{intern, qualified_name, type_name_str, CoreData};

mod closure;
mod load_set;

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

// ---------------------------------------------------------------------------
// The recorded model
// ---------------------------------------------------------------------------

/// Where a declaration came from. The reference loads these in a different
/// order and under different rules, which is what the allow-list must respect:
/// bundled RBS through `RBS::EnvironmentLoader` (trusted, built by the pinned
/// oracle), project and rbs-collection files one file at a time (quarantined
/// on a duplicate declaration), and a bundled plugin's `sig/` DEFERRED after
/// them (dropped on a generic-arity clash).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Origin {
    /// Embedded core / stdlib / overlay RBS and the capability-role catalogue.
    Bundled,
    /// One bundled plugin's RBS source (`plugin:<id>:<name>`).
    Plugin(&'static str),
    /// A file under the project's `signature_paths:`.
    Project(&'static str),
    /// A file under a discovered rbs-collection gem directory.
    Collection(&'static str),
}

impl Origin {
    /// The quarantine key: which unit the reference drops as a whole.
    fn key(self) -> Option<&'static str> {
        match self {
            Origin::Bundled => None,
            Origin::Plugin(k) | Origin::Project(k) | Origin::Collection(k) => Some(k),
        }
    }

    /// Loaded by the reference as a project signature file.
    pub(super) fn projectish(self) -> bool {
        matches!(self, Origin::Project(_) | Origin::Collection(_))
    }
}

/// Which kind of project directory a signature file was found under.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Phase {
    Project,
    Collection,
}

/// A type name as written: path without a leading `::`, plus the absolute bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Written {
    pub(super) path: &'static str,
    pub(super) absolute: bool,
}

impl Written {
    fn leaf(self) -> &'static str {
        self.path.rsplit("::").next().unwrap_or(self.path)
    }
}

/// A header reference (superclass, self type, mixin): its name plus its type
/// arguments' count and the names inside them.
#[derive(Clone, Debug)]
pub(super) struct HeaderRef {
    pub(super) name: Written,
    pub(super) nargs: usize,
    pub(super) arg_refs: Vec<Written>,
    /// Every argument is a form whose named types the walk could enumerate.
    pub(super) args_ok: bool,
    /// The arguments' source text (`[Elem]`), where the comparison needs it.
    pub(super) args_text: &'static str,
}

/// One declared type parameter: `(variance, unchecked)` plus whether it
/// carries a bound or a default (whose equality the port does not model).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Param {
    pub(super) variance: u8,
    pub(super) unchecked: bool,
    pub(super) bounded: bool,
    pub(super) default: bool,
}

/// One instance-side member of a class/module declaration.
#[derive(Clone, Debug)]
pub(super) enum Member {
    /// `def` / `def self?.`, instance side.
    Def { name: &'static str, overloading: bool },
    /// One method an `attr_*` generates (`x`, `x=`).
    Attr(&'static str),
    Alias { new: &'static str, old: &'static str },
    /// `@x: T` (an `InstanceVariableDuplicationError` candidate).
    Ivar(&'static str),
    Include(HeaderRef),
    Prepend(HeaderRef),
}

#[derive(Debug)]
pub(super) struct ClassDecl {
    pub(super) origin: Origin,
    pub(super) is_module: bool,
    /// Lexical context of the declaration (enclosing qualified names,
    /// outermost first). Members resolve in this plus the class itself.
    pub(super) outer: Vec<&'static str>,
    pub(super) params: Vec<Param>,
    pub(super) superclass: Option<HeaderRef>,
    pub(super) self_types: Vec<HeaderRef>,
    pub(super) members: Vec<Member>,
    /// Named types `RBS::DefinitionBuilder#validate_type_params` walks for this
    /// declaration: its instance methods' (not `initialize`, not overloading
    /// reopens) and attributes' types.
    pub(super) vrefs: Vec<Written>,
    /// A member kind or type form the recording does not model.
    pub(super) unsupported: bool,
}

/// One member of an interface body, in declaration order.
#[derive(Clone, Debug)]
pub(super) enum IfaceMember {
    Def { name: &'static str, overloading: bool },
    Alias { new: &'static str, old: &'static str },
}

#[derive(Debug)]
pub(super) struct IfaceDecl {
    /// Lexical scopes the interface was declared in (innermost last).
    pub(super) ctx: Vec<&'static str>,
    pub(super) params: Vec<Param>,
    pub(super) includes: Vec<HeaderRef>,
    pub(super) members: Vec<IfaceMember>,
    pub(super) vrefs: Vec<Written>,
    pub(super) unsupported: bool,
}

#[derive(Debug)]
pub(super) struct IfaceSlot {
    pub(super) decl: IfaceDecl,
    /// Declared more than once, or in a file the reference may have dropped:
    /// which body (if any) the reference loaded is unknowable.
    pub(super) ambiguous: bool,
    pub(super) origin: Origin,
}

/// Kinds sharing RBS's constant namespace, for the duplicate-declaration check.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConstKind {
    Class,
    Module,
    Constant,
    ClassAlias,
}

/// First-declaration order of a class: bundled (and plugin) declarations first
/// in ingest order, then project ones by `(sorted file path, offset)` — the
/// reference loads `sig/` files sorted, so `env.class_decls` iterates in that
/// order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Seen {
    Bundled(usize),
    Project(&'static str, usize),
}

#[derive(Clone, Debug)]
pub(super) struct AnnotationRecord {
    pub(super) class: &'static str,
    pub(super) text: String,
    pub(super) file: &'static str,
    pub(super) start: usize,
    pub(super) end: usize,
}

/// Builder-side accumulator (lives inside [`super::Builder`]).
#[derive(Default)]
pub(super) struct ConformanceBuilder {
    current: Option<Origin>,
    seq: usize,
    const_ns: HashMap<&'static str, (ConstKind, Origin)>,
    type_aliases: HashMap<&'static str, Origin>,
    globals: HashMap<&'static str, Origin>,
    interfaces: HashMap<&'static str, IfaceSlot>,
    classes: HashMap<&'static str, Vec<ClassDecl>>,
    suspect: HashSet<&'static str>,
    collection_files: HashSet<&'static str>,
    /// Project files carrying a directive (`use`, `resolve-type-names`): the
    /// reference resolves their names through a map the port does not model.
    directive_files: HashSet<&'static str>,
    /// Every type name written in a project file, and its prefixes: the
    /// reference stubs the unresolvable ones (`stub_missing_referenced_types`).
    written: HashSet<&'static str>,
    /// Names each plugin source declares (dropped with it upstream).
    plugin_names: HashMap<&'static str, Vec<&'static str>>,
    /// Names each project / collection file declares.
    file_names: HashMap<&'static str, Vec<&'static str>>,
    /// A project `prepend` of an interface: the reference's build raises a
    /// non-RBS error there and the whole run dies.
    crash_risk: bool,
    /// The source text being walked (for header argument spellings).
    code: String,
    first_seen: HashMap<&'static str, (Seen, Origin)>,
    annotations: Vec<AnnotationRecord>,
}

/// The frozen model [`CoreData::conformance_findings`] reads.
#[derive(Default)]
pub(super) struct ConformanceData {
    pub(super) classes: HashMap<&'static str, Vec<ClassDecl>>,
    pub(super) interfaces: HashMap<&'static str, IfaceSlot>,
    pub(super) type_aliases: HashMap<&'static str, Origin>,
    pub(super) class_aliases: HashSet<&'static str>,
    /// Constant and global names (for the load-set dump only).
    pub(super) constants: HashSet<&'static str>,
    pub(super) globals: HashSet<&'static str>,
    /// Quarantine keys (project / collection files, plugin sources) the
    /// reference may have dropped.
    pub(super) suspect: HashSet<&'static str>,
    pub(super) directive_files: HashSet<&'static str>,
    /// Names whose existence in the reference's environment the port cannot
    /// decide (declared only by a possibly-dropped source).
    pub(super) unknown_names: HashSet<&'static str>,
    /// Names the reference may have synthesized: written-but-maybe-missing
    /// type names (stubs) and undeclared namespace prefixes.
    pub(super) maybe_synthetic: HashSet<&'static str>,
    pub(super) crash_risk: bool,
    /// Annotations in the reference's `env.class_decls` order, restricted to
    /// classes first declared in a (non-quarantined) project file.
    pub(super) annotations: Vec<AnnotationRecord>,
}

/// Header arguments as written, each whole-word occurrence of one of the
/// declaration's type parameters replaced by its position: the comparison
/// `RBS::Environment::ModuleEntry#self_types` makes after `align_params`
/// renames every declaration's parameters to the first one's.
fn canonical_args(text: &str, params: &[String]) -> String {
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        match params.iter().position(|p| p == word) {
            Some(i) => out.push_str(&format!("\u{1}{i}")),
            None => out.push_str(word),
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

fn param_list(params: NodeList<'_>) -> (Vec<Param>, bool) {
    let mut out = Vec::new();
    let mut ok = true;
    for p in params.iter() {
        let Node::TypeParam(tp) = p else {
            ok = false;
            continue;
        };
        let variance = match tp.variance() {
            TypeParamVariance::Invariant => 0,
            TypeParamVariance::Covariant => 1,
            TypeParamVariance::Contravariant => 2,
        };
        out.push(Param {
            variance,
            unchecked: tp.unchecked(),
            bounded: tp.upper_bound().is_some() || tp.lower_bound().is_some(),
            default: tp.default_type().is_some(),
        });
    }
    (out, ok)
}

/// Written `TypeNameNode` → (`A::B::_C` without leading `::`, absolute?).
fn written_name(tn: &TypeNameNode) -> Option<Written> {
    let ns = tn.namespace();
    let mut parts: Vec<String> = Vec::new();
    for seg in ns.path().iter() {
        if let Node::Symbol(sym) = seg {
            parts.push(sym.as_str().to_string());
        }
    }
    parts.push(type_name_str(tn)?.to_string());
    Some(Written { path: intern(&parts.join("::")), absolute: ns.absolute() })
}

/// Collect the named types (`ClassInstance` / `ClassSingleton` / `Interface` /
/// `Alias`) inside a type or method type. `false` when a node kind the walk
/// does not know was met: the carrier is then unverifiable.
fn type_refs(node: &Node<'_>, out: &mut Vec<Written>) -> bool {
    fn all(list: NodeList<'_>, out: &mut Vec<Written>) -> bool {
        list.iter().fold(true, |ok, n| type_refs(&n, out) && ok)
    }
    fn named(tn: &TypeNameNode<'_>, args: NodeList<'_>, out: &mut Vec<Written>) -> bool {
        let ok = match written_name(tn) {
            Some(w) => {
                out.push(w);
                true
            }
            None => false,
        };
        all(args, out) && ok
    }
    match node {
        Node::ClassInstanceType(t) => named(&t.name(), t.args(), out),
        Node::InterfaceType(t) => named(&t.name(), t.args(), out),
        Node::AliasType(t) => named(&t.name(), t.args(), out),
        Node::ClassSingletonType(t) => named(&t.name(), t.args(), out),
        Node::OptionalType(t) => type_refs(&t.type_(), out),
        Node::UnionType(t) => all(t.types(), out),
        Node::IntersectionType(t) => all(t.types(), out),
        Node::TupleType(t) => all(t.types(), out),
        Node::RecordType(t) => t
            .all_fields()
            .iter()
            .fold(true, |ok, (_, v)| type_refs(&v, out) && ok),
        Node::RecordFieldType(f) => type_refs(&f.type_(), out),
        Node::ProcType(t) => {
            let mut ok = type_refs(&t.type_(), out);
            if let Some(b) = t.block() {
                ok &= type_refs(&Node::BlockType(b), out);
            }
            if let Some(s) = t.self_type() {
                ok &= type_refs(&s, out);
            }
            ok
        }
        Node::BlockType(b) => {
            let mut ok = type_refs(&b.type_(), out);
            if let Some(s) = b.self_type() {
                ok &= type_refs(&s, out);
            }
            ok
        }
        Node::FunctionType(f) => {
            let mut ok = all(f.required_positionals(), out);
            ok &= all(f.optional_positionals(), out);
            ok &= all(f.trailing_positionals(), out);
            if let Some(r) = f.rest_positionals() {
                ok &= type_refs(&r, out);
            }
            for (_, v) in f.required_keywords().iter().chain(f.optional_keywords().iter()) {
                ok &= type_refs(&v, out);
            }
            if let Some(r) = f.rest_keywords() {
                ok &= type_refs(&r, out);
            }
            ok & type_refs(&f.return_type(), out)
        }
        Node::FunctionParam(p) => type_refs(&p.type_(), out),
        Node::UntypedFunctionType(u) => type_refs(&u.return_type(), out),
        Node::MethodType(mt) => {
            let mut ok = type_refs(&mt.type_(), out);
            if let Some(b) = mt.block() {
                ok &= type_refs(&Node::BlockType(b), out);
            }
            ok
        }
        Node::AnyType(_)
        | Node::BoolType(_)
        | Node::BottomType(_)
        | Node::NilType(_)
        | Node::SelfType(_)
        | Node::TopType(_)
        | Node::VoidType(_)
        | Node::InstanceType(_)
        | Node::ClassType(_)
        | Node::LiteralType(_)
        | Node::VariableType(_) => true,
        _ => false,
    }
}

/// The named types inside a method definition's overloads.
fn overload_refs(overloads: NodeList<'_>, out: &mut Vec<Written>) -> bool {
    let mut ok = true;
    for o in overloads.iter() {
        match o {
            Node::MethodDefinitionOverload(ov) => ok &= type_refs(&ov.method_type(), out),
            _ => ok = false,
        }
    }
    ok
}

fn header(name: &TypeNameNode<'_>, args: NodeList<'_>) -> Option<HeaderRef> {
    let name = written_name(name)?;
    let mut arg_refs = Vec::new();
    let mut nargs = 0;
    let mut args_ok = true;
    for a in args.iter() {
        nargs += 1;
        args_ok &= type_refs(&a, &mut arg_refs);
    }
    Some(HeaderRef { name, nargs, arg_refs, args_ok, args_text: "" })
}

impl ConformanceBuilder {
    /// Enter a source of the given origin (`None` = bundled).
    pub(super) fn set_origin(&mut self, origin: Option<Origin>) {
        if let Some(Origin::Collection(f)) = origin {
            self.collection_files.insert(f);
        }
        self.current = origin;
    }

    fn origin(&self) -> Origin {
        self.current.unwrap_or(Origin::Bundled)
    }

    /// Walk one parsed source's directives and declarations.
    pub(super) fn walk(&mut self, code: &str, directives: NodeList<'_>, decls: NodeList<'_>) {
        self.code.clear();
        self.code.push_str(code);
        let origin = self.origin();
        if let Origin::Project(f) | Origin::Collection(f) = origin {
            if directives.iter().next().is_some() {
                self.directive_files.insert(f);
            }
        }
        self.walk_decls(decls, &[]);
    }

    /// Remember the names a project file writes (and their prefixes): a name
    /// the reference cannot resolve is stubbed into its environment.
    fn note_written(&mut self, refs: &[Written]) {
        if !self.origin().projectish() {
            return;
        }
        for w in refs {
            let mut acc = String::new();
            for seg in w.path.split("::") {
                if !acc.is_empty() {
                    acc.push_str("::");
                }
                acc.push_str(seg);
                self.written.insert(intern(&acc));
            }
        }
    }

    fn note_type(&mut self, node: &Node<'_>) {
        let mut refs = Vec::new();
        type_refs(node, &mut refs);
        self.note_written(&refs);
    }

    fn walk_decls(&mut self, decls: NodeList<'_>, enclosing: &[&'static str]) {
        for decl in decls.iter() {
            self.nested(&decl, enclosing);
        }
    }

    /// A declaration at file level or nested in a class/module body. Returns
    /// `false` for a node that is not a declaration.
    fn nested(&mut self, decl: &Node<'_>, enclosing: &[&'static str]) -> bool {
        match decl {
            Node::Class(c) => self.class_decl(c, enclosing),
            Node::Module(m) => self.module_decl(m, enclosing),
            Node::Interface(i) => self.interface_decl(i, enclosing),
            Node::Constant(c) => {
                let q = qualified_name(enclosing, &c.name());
                self.declare_const(q, ConstKind::Constant);
                self.note_type(&c.type_());
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
                let origin = self.origin();
                if let Some(prev) = self.type_aliases.insert(q, origin) {
                    self.collide(prev);
                }
                self.note_type(&ta.type_());
                self.note_plugin_name(q);
            }
            Node::Global(g) => {
                let name = intern(g.name().as_str());
                let origin = self.origin();
                if let Some(prev) = self.globals.insert(name, origin) {
                    self.collide(prev);
                }
                self.note_type(&g.type_());
                self.note_plugin_name(name);
            }
            _ => return false,
        }
        true
    }

    /// A duplicate declaration: the reference drops the LATER project file (a
    /// plugin source loads after the project and is the one dropped); the port
    /// cannot trust its own file order, so every non-bundled party turns
    /// suspect (a bundled first declarer stays loaded, as it does upstream).
    fn collide(&mut self, prev: Origin) {
        if let Some(k) = self.origin().key() {
            self.suspect.insert(k);
        }
        if let Some(k) = prev.key() {
            self.suspect.insert(k);
        }
    }

    /// Record a name a non-bundled source declares: a plugin source is
    /// dropped with all of them upstream, and a project file declaring a name
    /// only the reference's environment knows may collide with it there.
    fn note_plugin_name(&mut self, q: &'static str) {
        match self.origin() {
            Origin::Plugin(k) => self.plugin_names.entry(k).or_default().push(q),
            Origin::Project(k) | Origin::Collection(k) => {
                self.file_names.entry(k).or_default().push(q);
            }
            Origin::Bundled => {}
        }
    }

    fn declare_const(&mut self, q: &'static str, kind: ConstKind) {
        let origin = self.origin();
        self.note_plugin_name(q);
        match self.const_ns.get(q).copied() {
            None => {
                self.const_ns.insert(q, (kind, origin));
            }
            Some((k, _)) if k == kind && matches!(kind, ConstKind::Class | ConstKind::Module) => {
                // A reopen — fine.
            }
            Some((_, prev)) => self.collide(prev),
        }
    }

    fn see_class(&mut self, q: &'static str, start: usize) {
        let origin = self.origin();
        let seen = match origin {
            Origin::Project(f) | Origin::Collection(f) => Seen::Project(f, start),
            Origin::Bundled | Origin::Plugin(_) => {
                self.seq += 1;
                Seen::Bundled(self.seq)
            }
        };
        self.first_seen
            .entry(q)
            .and_modify(|s| {
                if seen < s.0 {
                    *s = (seen, origin);
                }
            })
            .or_insert((seen, origin));
    }

    fn record_annotations(&mut self, q: &'static str, annotations: NodeList<'_>) {
        let (Origin::Project(file) | Origin::Collection(file)) = self.origin() else {
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
        self.record_annotations(q, c.annotations());
        let (params, params_ok) = param_list(c.type_params());
        let mut decl = ClassDecl {
            origin: self.origin(),
            is_module: false,
            outer: enclosing.to_vec(),
            params,
            superclass: None,
            self_types: Vec::new(),
            members: Vec::new(),
            vrefs: Vec::new(),
            unsupported: !params_ok,
        };
        if let Some(sup) = c.super_class() {
            match header(&sup.name(), sup.args()) {
                Some(h) => {
                    self.note_written(&h.arg_refs.clone());
                    decl.superclass = Some(h);
                }
                None => decl.unsupported = true,
            }
        }
        let child: Vec<&'static str> = enclosing.iter().copied().chain([q]).collect();
        self.members(&mut decl, c.members(), &child);
        self.classes.entry(q).or_default().push(decl);
    }

    fn module_decl(&mut self, m: &ModuleNode, enclosing: &[&'static str]) {
        let q = qualified_name(enclosing, &m.name());
        self.declare_const(q, ConstKind::Module);
        self.see_class(q, usize::try_from(m.location().start()).unwrap_or(0));
        self.record_annotations(q, m.annotations());
        let (params, params_ok) = param_list(m.type_params());
        let param_names: Vec<String> = m
            .type_params()
            .iter()
            .filter_map(|p| match p {
                Node::TypeParam(tp) => Some(tp.name().as_str().to_string()),
                _ => None,
            })
            .collect();
        let mut decl = ClassDecl {
            origin: self.origin(),
            is_module: true,
            outer: enclosing.to_vec(),
            params,
            superclass: None,
            self_types: Vec::new(),
            members: Vec::new(),
            vrefs: Vec::new(),
            unsupported: !params_ok,
        };
        for st in m.self_types().iter() {
            let h = match &st {
                Node::ModuleSelf(s) => header(&s.name(), s.args()).map(|mut h| {
                    if let Some(r) = s.args_location() {
                        let (a, b) = (r.start(), r.end());
                        let range = usize::try_from(a).unwrap_or(0)..usize::try_from(b).unwrap_or(0);
                        h.args_text = match self.code.get(range) {
                            Some(text) => intern(&canonical_args(text, &param_names)),
                            None => "\u{0}?",
                        };
                    }
                    h
                }),
                _ => None,
            };
            match h {
                Some(h) => {
                    self.note_written(&h.arg_refs.clone());
                    decl.self_types.push(h);
                }
                None => decl.unsupported = true,
            }
        }
        let child: Vec<&'static str> = enclosing.iter().copied().chain([q]).collect();
        self.members(&mut decl, m.members(), &child);
        self.classes.entry(q).or_default().push(decl);
    }

    /// One class/module body: its instance-side members as the model records
    /// them, plus the nested declarations (walked as declarations).
    fn members(&mut self, decl: &mut ClassDecl, members: NodeList<'_>, child: &[&'static str]) {
        for member in members.iter() {
            match &member {
                Node::MethodDefinition(md) => {
                    let mut refs = Vec::new();
                    let ok = overload_refs(md.overloads(), &mut refs);
                    self.note_written(&refs);
                    let name = intern(md.name().as_str());
                    if matches!(
                        md.kind(),
                        MethodDefinitionKind::Instance | MethodDefinitionKind::SingletonInstance
                    ) {
                        let overloading = md.overloading();
                        decl.members.push(Member::Def { name, overloading });
                        // `validate_type_params` walks each method's ORIGINAL
                        // (non-overloading) definition, `initialize` excepted.
                        if !overloading && name != "initialize" {
                            decl.vrefs.extend(refs);
                            decl.unsupported |= !ok;
                        }
                    }
                }
                Node::AttrReader(a) => {
                    let ty = a.type_();
                    if matches!(a.kind(), AttributeKind::Instance) {
                        decl.members.push(Member::Attr(intern(a.name().as_str())));
                        self.attr_type(decl, &ty);
                    } else {
                        self.note_type(&ty);
                    }
                }
                Node::AttrWriter(a) => {
                    let ty = a.type_();
                    if matches!(a.kind(), AttributeKind::Instance) {
                        decl.members.push(Member::Attr(intern(&format!("{}=", a.name().as_str()))));
                        self.attr_type(decl, &ty);
                    } else {
                        self.note_type(&ty);
                    }
                }
                Node::AttrAccessor(a) => {
                    let ty = a.type_();
                    if matches!(a.kind(), AttributeKind::Instance) {
                        decl.members.push(Member::Attr(intern(a.name().as_str())));
                        decl.members.push(Member::Attr(intern(&format!("{}=", a.name().as_str()))));
                        self.attr_type(decl, &ty);
                    } else {
                        self.note_type(&ty);
                    }
                }
                Node::Alias(a) => {
                    if matches!(a.kind(), AliasKind::Instance) {
                        decl.members.push(Member::Alias {
                            new: intern(a.new_name().as_str()),
                            old: intern(a.old_name().as_str()),
                        });
                    }
                }
                Node::InstanceVariable(iv) => {
                    self.note_type(&iv.type_());
                    decl.members.push(Member::Ivar(intern(iv.name().as_str())));
                }
                Node::ClassInstanceVariable(iv) => self.note_type(&iv.type_()),
                Node::ClassVariable(cv) => self.note_type(&cv.type_()),
                Node::Include(inc) => match header(&inc.name(), inc.args()) {
                    Some(h) => {
                        self.note_written(&h.arg_refs.clone());
                        decl.members.push(Member::Include(h));
                    }
                    None => decl.unsupported = true,
                },
                Node::Prepend(p) => match header(&p.name(), p.args()) {
                    Some(h) => {
                        if h.name.leaf().starts_with('_') && self.origin().projectish() {
                            self.crash_risk = true;
                        }
                        self.note_written(&h.arg_refs.clone());
                        decl.members.push(Member::Prepend(h));
                    }
                    None => decl.unsupported = true,
                },
                Node::Extend(e) => {
                    // The singleton side only; the instance build never reads it.
                    if let Some(h) = header(&e.name(), e.args()) {
                        self.note_written(&h.arg_refs);
                    }
                }
                Node::Public(_) | Node::Private(_) => {}
                other => {
                    if !self.nested(other, child) {
                        decl.unsupported = true;
                    }
                }
            }
        }
    }

    fn attr_type(&mut self, decl: &mut ClassDecl, ty: &Node<'_>) {
        let mut refs = Vec::new();
        let ok = type_refs(ty, &mut refs);
        self.note_written(&refs);
        decl.vrefs.extend(refs);
        decl.unsupported |= !ok;
    }

    fn interface_decl(&mut self, i: &InterfaceNode, enclosing: &[&'static str]) {
        let q = qualified_name(enclosing, &i.name());
        self.note_plugin_name(q);
        let (params, params_ok) = param_list(i.type_params());
        let mut decl = IfaceDecl {
            ctx: enclosing.to_vec(),
            params,
            includes: Vec::new(),
            members: Vec::new(),
            vrefs: Vec::new(),
            unsupported: !params_ok,
        };
        for member in i.members().iter() {
            match &member {
                Node::MethodDefinition(md) => {
                    let mut refs = Vec::new();
                    let ok = overload_refs(md.overloads(), &mut refs);
                    self.note_written(&refs);
                    let name = intern(md.name().as_str());
                    let overloading = md.overloading();
                    if !overloading && name != "initialize" {
                        decl.vrefs.extend(refs);
                        decl.unsupported |= !ok;
                    }
                    if !matches!(md.kind(), MethodDefinitionKind::Instance) {
                        decl.unsupported = true;
                    }
                    decl.members.push(IfaceMember::Def { name, overloading });
                }
                Node::Alias(a) => decl.members.push(IfaceMember::Alias {
                    new: intern(a.new_name().as_str()),
                    old: intern(a.old_name().as_str()),
                }),
                Node::Include(inc) => match header(&inc.name(), inc.args()) {
                    Some(h) => {
                        self.note_written(&h.arg_refs.clone());
                        decl.vrefs.extend(h.arg_refs.iter().copied());
                        decl.includes.push(h);
                    }
                    None => decl.unsupported = true,
                },
                _ => decl.unsupported = true,
            }
        }
        let origin = self.origin();
        match self.interfaces.get_mut(q) {
            Some(slot) => {
                slot.ambiguous = true;
                let prev = slot.origin;
                self.collide(prev);
            }
            None => {
                self.interfaces.insert(q, IfaceSlot { decl, ambiguous: false, origin });
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
        self.current = None;
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
        // A plugin source whose class clashes in generic ARITY with another
        // declaration is dropped whole upstream (`add_deferred_signatures`).
        for decls in self.classes.values() {
            let arity = decls.first().map_or(0, |d| d.params.len());
            for d in decls {
                if let Origin::Plugin(k) = d.origin {
                    if d.params.len() != arity {
                        self.suspect.insert(k);
                    }
                }
            }
        }
        // Interfaces declared in a possibly-dropped source: unknowable body.
        for slot in self.interfaces.values_mut() {
            if slot.origin.key().is_some_and(|k| self.suspect.contains(k)) {
                slot.ambiguous = true;
            }
        }
        // A project file declaring a name the reference's environment holds
        // differently (or at all, where the port does not) may collide there
        // (`DuplicatedDeclarationError` against a constant, a class alias, a
        // kind clash): the whole file may be quarantined upstream.
        if !self.file_names.is_empty() {
            let divergent: HashSet<&str> = load_set::LOAD_SET_DIVERGENT.iter().copied().collect();
            for (file, names) in &self.file_names {
                if names.iter().any(|n| divergent.contains(n)) {
                    self.suspect.insert(file);
                }
            }
        }
        let mut unknown_names: HashSet<&'static str> = HashSet::new();
        for (k, names) in &self.plugin_names {
            if self.suspect.contains(k) {
                unknown_names.extend(names.iter().copied());
            }
        }
        let constants: HashSet<&'static str> = self
            .const_ns
            .iter()
            .filter(|(_, (k, _))| *k == ConstKind::Constant)
            .map(|(n, _)| *n)
            .collect();
        let globals: HashSet<&'static str> = self.globals.keys().copied().collect();
        let class_aliases: HashSet<&'static str> = self
            .const_ns
            .iter()
            .filter(|(_, (k, _))| *k == ConstKind::ClassAlias)
            .map(|(n, _)| *n)
            .collect();
        // `synthesize_missing_namespaces`: an undeclared namespace prefix of a
        // declared class becomes a module upstream.
        let mut maybe_synthetic = self.written;
        for name in self.classes.keys() {
            let mut acc = String::new();
            let segs: Vec<&str> = name.split("::").collect();
            for seg in &segs[..segs.len().saturating_sub(1)] {
                if !acc.is_empty() {
                    acc.push_str("::");
                }
                acc.push_str(seg);
                maybe_synthetic.insert(intern(&acc));
            }
        }
        let classes = &self.classes;
        let interfaces = &self.interfaces;
        let type_aliases = &self.type_aliases;
        maybe_synthetic.retain(|n| {
            !classes.contains_key(n) && !interfaces.contains_key(n) && !type_aliases.contains_key(n)
        });
        let suspect = &self.suspect;
        let collection = &self.collection_files;
        let first_seen = &self.first_seen;
        // Order silence (ADR-0044): a class whose first declaration is bundled,
        // a plugin's, an rbs collection's or in a possibly-dropped file sits at
        // a position in `env.class_decls` the port cannot reproduce.
        let mut annotations: Vec<AnnotationRecord> = self
            .annotations
            .into_iter()
            .filter(|a| !suspect.contains(a.file) && !collection.contains(a.file))
            .filter(|a| match first_seen.get(a.class) {
                Some((Seen::Project(f, _), _)) => !suspect.contains(f) && !collection.contains(f),
                _ => false,
            })
            .collect();
        annotations.sort_by(|a, b| {
            let ra = first_seen.get(a.class).map(|s| s.0);
            let rb = first_seen.get(b.class).map(|s| s.0);
            (ra, a.file, a.start).cmp(&(rb, b.file, b.start))
        });
        ConformanceData {
            classes: self.classes,
            interfaces: self.interfaces,
            type_aliases: self.type_aliases,
            class_aliases,
            suspect: self.suspect,
            directive_files: self.directive_files,
            unknown_names,
            maybe_synthetic,
            crash_risk: self.crash_risk,
            constants,
            globals,
            annotations,
        }
    }
}

impl CoreData {
    /// Scan every `rigor:v1:conforms-to` directive in the project's signature
    /// files and return the rows the reference emits for them, in its order.
    /// FP-safe by construction: a row fires only when the reference's build is
    /// provably successful and every name it resolves is provably the port's.
    #[must_use]
    pub fn conformance_findings(&self) -> Vec<ConformanceFinding> {
        closure::findings(&self.conformance)
    }

    /// The port's side of `harness/conformance_load_set.rb` (read through the
    /// ignored test `dump_conformance_surface`): every class, interface,
    /// alias, constant and global name the recorded model holds, with the
    /// surface the port computes, the load-set list NOT applied. One line
    /// each: `class\t<name>\t<module?>\t<min>\t<max>\t<sorted methods | ?>`,
    /// `iface\t<name>\t<min>\t<max>\t<ordered members | ?>`, `alias\t<name>`,
    /// `class_alias\t<name>`, `const\t<name>`, `global\t<name>`.
    #[doc(hidden)]
    #[must_use]
    pub fn conformance_surface_dump(&self) -> String {
        closure::dump(&self.conformance)
    }
}

#[cfg(test)]
mod tests;
