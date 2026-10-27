//! The per-AST tables behind [`Typer::local_reach`] (rigor-rs#422).
//!
//! `local_reach` is re-entered for every write that can reach a read, up to
//! the `seen` cap, and every call used to repeat several whole-arena scans
//! that read nothing but the syntax tree: the scope-shape pass, the
//! [`unrecorded_closure`] boundary walk, [`unevaluated_case_clause_spans`],
//! the write collector (with an `in_inert_carrier` test per node) and, once
//! per candidate write, the `closure_bound_elsewhere` scan. On a generated
//! lexer with 324 writes of one local that was about 310 full scans per call.
//!
//! [`ReachIndex`] holds those answers, built once per `LoweredAst`
//! ([`Typer::reach_index`]) and shared by every reach query on it. It is exact
//! by construction: nothing here reads `seen`, the dead set or any memo, so
//! each table answers exactly what the per-call scan it replaced did. The
//! filters that depend on the READ (`dead.write_reaches`, the closure test
//! and the own-RHS containment) still run per call, in the original order.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use rigor_parse::{LoweredAst, Node, NodeId, Span};

use crate::dead::CallSelf;
use crate::reach::{
    has_non_local_target, proc_like_block, statement_sections, unevaluated_case_clause_spans,
    unrecorded_closure, IvarScope,
};
use crate::Typer;

/// `outer` contains `inner` — half-open byte spans, equal spans count.
fn contains(outer: Span, inner: Span) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}

/// One write of a local, as [`Typer::local_reach`] collects them.
#[derive(Debug, Clone, Copy)]
pub(crate) enum LocalWrite {
    /// `x = v`.
    Plain(NodeId),
    /// `x op= v` — keeps the old value on some path, so it never cuts one off.
    Op(NodeId),
    /// `a, x = v` — the right-hand side.
    Multi(NodeId),
}

/// One node of the scope-shape pass, in arena order — the five node kinds
/// `local_reach`'s first loop matched. Arena order matters: the narrowest
/// `def` and the narrowest binder keep the FIRST of two equal-width spans.
pub(crate) enum ScopeNode {
    /// `Node::Definition` (`singleton_class`: a `class << x` frame).
    Def { span: Span, id: NodeId, singleton_class: bool },
    /// `Node::Lambda` (`->`).
    Lambda { span: Span },
    /// `Node::Loop`.
    Loop { span: Span },
    /// A `Node::BeginRescue` with at least one clause: its span and every
    /// clause's span (a retrying clause makes the `begin` a loop carrier).
    Rescue { span: Span, clauses: Vec<Span> },
    /// A block-bearing `Node::Call`: the block BODY's extent, whether the
    /// block is proc-like, and the names its parameters bind.
    Block { extent: Span, proc_like: bool, params: Vec<String> },
}

/// One node of the write collector that touches a given local, in arena
/// order, with every READ-INDEPENDENT filter already applied (the name, the
/// region, the inert / blocked carriers, the guard shape). What is left per
/// read is [`Typer::local_reach`]'s loop over these.
pub(crate) enum LocalEvent {
    /// A write of the local inside the region. Still per read:
    /// `dead.write_reaches`, the closure test and the own-RHS containment.
    Write { span: Span, value_span: Span, write: LocalWrite },
    /// A `for` index binding the local: its in-region index spans. Refuses
    /// (UNKNOWN) when any of them `write_reaches` the read.
    ForIndex(Vec<Span>),
    /// A `rescue => local` binding: the clause spans that bind it. Refuses
    /// (OPAQUE) when any of them `write_reaches` the read.
    RescueBinding(Vec<Span>),
    /// `local.is_a?(C)` / `kind_of?` / `instance_of?` with a constant class
    /// operand: stepped over under `skip_class_guards`, otherwise refuses.
    ExactGuard,
    /// Any other class guard on the local (a guard call, a `case` subject):
    /// refuses.
    Guard,
}

/// The write collector's cache key: `(region, skip_defs, local name)`.
type EventsKey = (Span, bool, String);

/// [`Typer::reach_index`]'s memo: the index with the `(AST address, arena
/// length)` identity it was built for.
pub(crate) type ReachIndexCache = Option<((usize, usize), Rc<ReachIndex>)>;

/// The per-AST tables. See the module docs.
pub(crate) struct ReachIndex {
    /// [`unevaluated_case_clause_spans`] — computed once instead of per call.
    pub(crate) case_clauses: Vec<Span>,
    /// Every `def` span, in arena order.
    pub(crate) def_spans: Vec<Span>,
    /// Every `->` span, in arena order.
    pub(crate) lambda_spans: Vec<Span>,
    /// The scope-shape nodes, in arena order.
    pub(crate) scopes: Vec<ScopeNode>,
    /// Per bound name, the extents of the closures that bind it — the node
    /// set of `closure_bound_elsewhere` and of the `floored` test:
    /// `Node::Lambda { span, locals }` and `Node::Call { block_span:
    /// Some(_), block_locals }`, in arena order.
    closures: HashMap<String, Vec<Span>>,
    /// Per local name, the nodes the write collector can match for it, in
    /// arena order (see [`LocalEvent`]).
    by_name: HashMap<String, Vec<NodeId>>,
    /// Every node's start offset with its id, sorted — the range index of
    /// [`ReachIndex::nested_section_owner`].
    by_start: Vec<(usize, NodeId)>,
    /// [`ReachIndex::nested_section_owner`] per `(statement, read span)`.
    sections: RefCell<HashMap<(NodeId, Span), Option<NodeId>>>,
    /// Every multi-assignment with an unnameable (ivar, index, attribute,
    /// constant) target, in arena order — the ivar arm refuses on one.
    pub(crate) multi_nonlocal: Vec<Span>,
    /// Per ivar name, its `@x = …` writes `(span, value)`, in arena order.
    ivar_writes: HashMap<String, Vec<(Span, NodeId)>>,
    /// Every `class`/`module` span, in arena order.
    class_spans: Vec<Span>,
    /// Every `def` span with its name, in arena order.
    def_names: Vec<(Span, Option<String>)>,
    /// The names of the `def`s at file scope (no class/module/def around).
    toplevel_defs: HashSet<String>,
    /// Per name, the LAST (by start) file-scope bare `def` of it — the
    /// binding `def_exits` reads.
    last_toplevel_defs: HashMap<String, NodeId>,
    /// [`Typer::call_self`] per call site.
    call_selves: RefCell<HashMap<Span, CallSelf>>,
    /// [`ReachIndex::class_ivar_scope`] per read span.
    ivar_scopes: RefCell<HashMap<Span, Rc<IvarScope>>>,
    /// [`unrecorded_closure`] per read span.
    unrecorded: RefCell<HashMap<Span, Option<(NodeId, Span)>>>,
    /// [`Typer::clause_retries`] per clause span.
    retries: RefCell<HashMap<Span, bool>>,
    /// The collector's events per `(region, skip_defs, local)`.
    events: RefCell<HashMap<EventsKey, Rc<[LocalEvent]>>>,
}

/// The class-guard methods the write collector refuses on.
fn is_guard_method(method: &str) -> bool {
    matches!(method, "is_a?" | "kind_of?" | "instance_of?" | "===")
}

/// The exact `root.is_a?(C)` shape `skip_class_guards` steps over.
fn is_exact_guard_method(method: &str) -> bool {
    matches!(method, "is_a?" | "kind_of?" | "instance_of?")
}

impl ReachIndex {
    /// One pass over the arena (plus [`unevaluated_case_clause_spans`]'s).
    fn build(ast: &LoweredAst) -> ReachIndex {
        let mut def_spans = Vec::new();
        let mut lambda_spans = Vec::new();
        let mut multi_nonlocal = Vec::new();
        let mut ivar_writes: HashMap<String, Vec<(Span, NodeId)>> = HashMap::new();
        let mut class_spans = Vec::new();
        let mut def_names = Vec::new();
        let mut def_ids = Vec::new();
        let mut scopes = Vec::new();
        let mut closures: HashMap<String, Vec<Span>> = HashMap::new();
        let mut by_name: HashMap<String, Vec<NodeId>> = HashMap::new();
        let mut note = |name: &str, id: NodeId| {
            let ids = by_name.entry(name.to_string()).or_default();
            if ids.last() != Some(&id) {
                ids.push(id);
            }
        };
        let local_read = |i: NodeId| match ast.get(i) {
            Node::LocalVariableRead { name, .. } => Some(name.as_str()),
            _ => None,
        };
        for (id, n) in ast.iter() {
            match n {
                Node::Definition { span, name, is_singleton_class, .. } => {
                    def_spans.push(*span);
                    def_names.push((*span, name.clone()));
                    def_ids.push(id);
                    scopes.push(ScopeNode::Def {
                        span: *span,
                        id,
                        singleton_class: *is_singleton_class,
                    });
                }
                Node::ClassDef { span, .. } | Node::ModuleDef { span, .. } => {
                    class_spans.push(*span);
                }
                Node::InstanceVariableWrite { name, value, span, .. } => {
                    ivar_writes.entry(name.clone()).or_default().push((*span, *value));
                }
                Node::Lambda { span, locals, .. } => {
                    lambda_spans.push(*span);
                    scopes.push(ScopeNode::Lambda { span: *span });
                    for l in locals {
                        closures.entry(l.clone()).or_default().push(*span);
                    }
                }
                Node::Loop { span, index, .. } => {
                    scopes.push(ScopeNode::Loop { span: *span });
                    for (name, _) in index {
                        note(name, id);
                    }
                }
                Node::BeginRescue { clauses, span, .. } => {
                    if !clauses.is_empty() {
                        scopes.push(ScopeNode::Rescue {
                            span: *span,
                            clauses: clauses.iter().map(|c| c.span).collect(),
                        });
                    }
                    for c in clauses {
                        if let Some(name) = c.bound_name.as_deref() {
                            note(name, id);
                        }
                    }
                }
                Node::LocalVariableWrite { name, .. } | Node::LocalVariableOpWrite { name, .. } => {
                    note(name, id);
                }
                Node::MultiWrite { targets, span, .. } => {
                    for (name, _) in targets.bound_names() {
                        note(&name, id);
                    }
                    if has_non_local_target(targets) {
                        multi_nonlocal.push(*span);
                    }
                }
                Node::Case { predicate: Some(p), .. } => {
                    if let Some(name) = local_read(*p) {
                        note(name, id);
                    }
                }
                _ => {}
            }
            if let Node::Call {
                receiver,
                method,
                args,
                block_body,
                block_params,
                block_span,
                block_locals,
                ..
            } = n
            {
                if !block_body.is_empty() {
                    let lo = block_body.iter().map(|&b| ast.get(b).span().0).min();
                    let hi = block_body.iter().map(|&b| ast.get(b).span().1).max();
                    if let (Some(lo), Some(hi)) = (lo, hi) {
                        scopes.push(ScopeNode::Block {
                            extent: (lo, hi),
                            proc_like: proc_like_block(ast, *receiver, method),
                            params: block_params.iter().map(|(p, _)| p.clone()).collect(),
                        });
                    }
                }
                if let Some(b) = block_span {
                    for l in block_locals {
                        closures.entry(l.clone()).or_default().push(*b);
                    }
                }
                if is_guard_method(method) {
                    for name in receiver.iter().chain(args.iter()).filter_map(|&i| local_read(i)) {
                        note(name, id);
                    }
                }
            }
        }
        // A `def` is file-scope when no class, module or other `def` span
        // contains it (`toplevel_def_in_file`'s rule).
        let containers: Vec<Span> = class_spans.iter().chain(def_spans.iter()).copied().collect();
        let toplevel_defs: HashSet<String> = def_names
            .iter()
            .filter(|(span, _)| {
                !containers.iter().any(|&c| c != *span && contains(c, *span))
            })
            .filter_map(|(_, name)| name.clone())
            .collect();
        let mut last_toplevel_defs: HashMap<String, NodeId> = HashMap::new();
        for (&(span, ref name), &id) in def_names.iter().zip(&def_ids) {
            let Some(name) = name else { continue };
            if containers.iter().any(|&c| c != span && contains(c, span)) {
                continue;
            }
            let later = last_toplevel_defs
                .get(name)
                .is_none_or(|&prev| ast.get(prev).span().0 <= span.0);
            if later {
                last_toplevel_defs.insert(name.clone(), id);
            }
        }
        let mut by_start: Vec<(usize, NodeId)> =
            ast.iter().map(|(id, n)| (n.span().0, id)).collect();
        by_start.sort_unstable();
        ReachIndex {
            case_clauses: unevaluated_case_clause_spans(ast),
            by_start,
            sections: RefCell::new(HashMap::new()),
            multi_nonlocal,
            ivar_writes,
            class_spans,
            def_names,
            toplevel_defs,
            last_toplevel_defs,
            call_selves: RefCell::new(HashMap::new()),
            ivar_scopes: RefCell::new(HashMap::new()),
            def_spans,
            lambda_spans,
            scopes,
            closures,
            by_name,
            unrecorded: RefCell::new(HashMap::new()),
            retries: RefCell::new(HashMap::new()),
            events: RefCell::new(HashMap::new()),
        }
    }

    /// Whether `span` sits inside a `when` condition / `in` pattern.
    pub(crate) fn in_case_clause(&self, span: Span) -> bool {
        self.case_clauses.iter().any(|&c| contains(c, span))
    }

    /// The extents of the closures that bind `name` (see `closures`).
    pub(crate) fn closures_binding(&self, name: &str) -> &[Span] {
        self.closures.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Whether `name` is bound by a literal block / `->` that contains `span`
    /// but NOT `use_span` — a parameter, `;`-local or body-introduced local
    /// (Prism's `BlockNode#locals`, which a captured outer local never joins).
    /// A write to it rebinds THAT closure's scope only, so it cannot reach a
    /// read outside the block: `xs.each { |x| x = 1 }` leaves a top-level `x`
    /// untouched, and `x` inside one `lambda`'s body is a different variable
    /// from `x` inside a sibling's. The write-collector of
    /// [`Typer::local_reach`] skips such writes, the cross-closure bleed that
    /// otherwise minted a precise value for a name the reference reads as
    /// `Dynamic[top]`.
    pub(crate) fn closure_bound_elsewhere(&self, span: Span, name: &str, use_span: Span) -> bool {
        self.closures_binding(name)
            .iter()
            .any(|&extent| contains(extent, span) && !contains(extent, use_span))
    }

    /// Whether a `def <name>` sits at file scope.
    pub(crate) fn toplevel_def(&self, name: &str) -> bool {
        self.toplevel_defs.contains(name)
    }

    /// The last (by start) file-scope bare `def name`, if any.
    pub(crate) fn last_toplevel_def(&self, name: &str) -> Option<NodeId> {
        self.last_toplevel_defs.get(name).copied()
    }

    /// [`Typer::call_self`] for `site`, memoized.
    pub(crate) fn call_self(&self, typer: &Typer<'_>, ast: &LoweredAst, site: Span) -> CallSelf {
        if let Some(hit) = self.call_selves.borrow().get(&site) {
            return hit.clone();
        }
        let out = typer.call_self_uncached(ast, site);
        self.call_selves.borrow_mut().insert(site, out.clone());
        out
    }

    /// The `@x = …` writes of ivar `name`, `(span, value)` in arena order.
    pub(crate) fn ivar_writes(&self, name: &str) -> &[(Span, NodeId)] {
        self.ivar_writes.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The class/module body that owns an ivar or cvar read at `use_span`, as a
    /// span plus the `def` spans inside it — the port of the reference's
    /// "qualified prefix" keying. The innermost enclosing `ClassDef`/`ModuleDef`
    /// wins, and a class NESTED inside it is a barrier (its ivars belong to its
    /// own class, row i9). With no enclosing class the region is the whole file,
    /// which is where a top-level `def`'s own `@x = …` still binds (row i6)
    /// while a top-level class-body write does not reach it (row t2).
    /// Memoized per read span.
    pub(crate) fn class_ivar_scope(&self, ast: &LoweredAst, use_span: Span) -> Rc<IvarScope> {
        if let Some(hit) = self.ivar_scopes.borrow().get(&use_span) {
            return hit.clone();
        }
        let mut region = ast.get(ast.root()).span();
        for &span in &self.class_spans {
            if contains(span, use_span) && span.1 - span.0 < region.1 - region.0 {
                region = span;
            }
        }
        let barriers: Vec<Span> = self
            .class_spans
            .iter()
            .copied()
            .filter(|&c| contains(region, c) && c != region)
            .collect();
        let defs: Vec<(Span, Option<String>)> = self
            .def_names
            .iter()
            .filter(|(span, _)| contains(region, *span))
            .cloned()
            .collect();
        let out = Rc::new(IvarScope { region, barriers, defs });
        self.ivar_scopes.borrow_mut().insert(use_span, out.clone());
        out
    }

    /// [`unrecorded_closure`] for `use_span`, memoized.
    pub(crate) fn unrecorded_closure(
        &self,
        ast: &LoweredAst,
        use_span: Span,
    ) -> Option<(NodeId, Span)> {
        if let Some(hit) = self.unrecorded.borrow().get(&use_span) {
            return *hit;
        }
        let out = unrecorded_closure(ast, use_span);
        self.unrecorded.borrow_mut().insert(use_span, out);
        out
    }

    /// The node whose statement section holds `use_span` when statement
    /// `stmt` itself has none — `latest_definite_assignment`'s descent
    /// through an expression (`x = items.map { |v| t = 1; Float(t) }`, an
    /// `if` predicate's block). Among the nodes strictly inside `stmt`'s
    /// span that contain the read (outside a `when`/`in` extent and a
    /// blocked carrier), it is the WIDEST one with a section holding the
    /// read, the first in arena order among equal widths; `None` when there
    /// is none, or when any such node is a `->` (its writes never bind, rows
    /// r11/p13).
    ///
    /// That is exactly the arena-order scan it replaces (a later node only
    /// displaced the best one when strictly wider, and a `->` returned
    /// `None` wherever it sat), restricted through `by_start` to the nodes
    /// that can contain the read — every one starts between `stmt`'s start
    /// and the read's — and memoized per `(stmt, use_span)`.
    pub(crate) fn nested_section_owner(
        &self,
        ast: &LoweredAst,
        stmt: NodeId,
        use_span: Span,
    ) -> Option<NodeId> {
        if let Some(hit) = self.sections.borrow().get(&(stmt, use_span)) {
            return *hit;
        }
        let out = self.scan_nested_section_owner(ast, stmt, use_span);
        self.sections.borrow_mut().insert((stmt, use_span), out);
        out
    }

    fn scan_nested_section_owner(
        &self,
        ast: &LoweredAst,
        stmt: NodeId,
        use_span: Span,
    ) -> Option<NodeId> {
        let outer = ast.get(stmt).span();
        let holds = |b: &[NodeId]| b.iter().any(|&s| contains(ast.get(s).span(), use_span));
        let lo = self.by_start.partition_point(|&(s, _)| s < outer.0);
        let hi = self.by_start.partition_point(|&(s, _)| s <= use_span.0);
        let mut best: Option<(usize, NodeId)> = None;
        for &(_, id) in &self.by_start[lo..hi.max(lo)] {
            let n = ast.get(id);
            let sp = n.span();
            if id == stmt
                || !contains(outer, sp)
                || !contains(sp, use_span)
                || self.in_case_clause(sp)
                || ast.in_blocked_carrier(sp)
            {
                continue;
            }
            if matches!(n, Node::Lambda { .. }) {
                return None;
            }
            let width = sp.1 - sp.0;
            let better = best.is_none_or(|(bw, bid)| width > bw || (width == bw && id < bid));
            if better && statement_sections(ast, id).into_iter().any(holds) {
                best = Some((width, id));
            }
        }
        best.map(|(_, id)| id)
    }

    /// [`Typer::clause_retries`] for `clause_span`, memoized.
    pub(crate) fn clause_retries(&self, typer: &Typer<'_>, ast: &LoweredAst, clause_span: Span) -> bool {
        if let Some(hit) = self.retries.borrow().get(&clause_span) {
            return *hit;
        }
        let out = typer.clause_retries(ast, clause_span);
        self.retries.borrow_mut().insert(clause_span, out);
        out
    }

    /// The write collector's events for `root` inside `region` — every node
    /// the per-call loop of [`Typer::local_reach`] used to match, in arena
    /// order, with the read-independent filters applied. `skip_defs` is the
    /// whole-file region's exclusion of `def` bodies.
    pub(crate) fn events(
        &self,
        ast: &LoweredAst,
        region: Span,
        skip_defs: bool,
        root: &str,
    ) -> Rc<[LocalEvent]> {
        let key = (region, skip_defs, root.to_string());
        if let Some(hit) = self.events.borrow().get(&key) {
            return hit.clone();
        }
        let out: Rc<[LocalEvent]> = self.collect_events(ast, region, skip_defs, root).into();
        self.events.borrow_mut().insert(key, out.clone());
        out
    }

    fn collect_events(
        &self,
        ast: &LoweredAst,
        region: Span,
        skip_defs: bool,
        root: &str,
    ) -> Vec<LocalEvent> {
        let in_region = |s: Span| {
            if !contains(region, s) || self.in_case_clause(s) || ast.in_blocked_carrier(s) {
                return false;
            }
            if self.lambda_spans.iter().any(|&l| contains(l, s)) {
                return false;
            }
            !(skip_defs && self.def_spans.iter().any(|&d| contains(d, s)))
        };
        // A guard is a narrowing, not a binding, so the `->` skip does not
        // apply to it — only the region does (see `local_reach`).
        let guards_here = |s: Span| {
            contains(region, s) && !self.in_case_clause(s) && !ast.in_blocked_carrier(s)
        };
        let reads_root = |i: NodeId| {
            matches!(ast.get(i), Node::LocalVariableRead { name, .. } if name == root)
        };
        let mut out = Vec::new();
        let Some(ids) = self.by_name.get(root) else {
            return out;
        };
        for &id in ids {
            let n = ast.get(id);
            // A write under `defined?` / `END` / `BEGIN` never runs in
            // sequence on the reference, so it contributes no value
            // (rigor-rs#153) — and the node is skipped for every arm.
            if ast.in_inert_carrier(n.span()) {
                continue;
            }
            match n {
                Node::LocalVariableWrite { name, value, span, .. } if name == root => {
                    if in_region(*span) {
                        out.push(LocalEvent::Write {
                            span: *span,
                            value_span: ast.get(*value).span(),
                            write: LocalWrite::Plain(*value),
                        });
                    }
                }
                Node::Loop { index, .. } => {
                    let spans: Vec<Span> = index
                        .iter()
                        .filter(|(n, s)| n == root && in_region(*s))
                        .map(|(_, s)| *s)
                        .collect();
                    if !spans.is_empty() {
                        out.push(LocalEvent::ForIndex(spans));
                    }
                }
                Node::LocalVariableOpWrite { name, value, span }
                    if name == root && in_region(*span) =>
                {
                    out.push(LocalEvent::Write {
                        span: *span,
                        value_span: ast.get(*value).span(),
                        write: LocalWrite::Op(*value),
                    });
                }
                Node::MultiWrite { targets, value, span, .. }
                    if in_region(*span)
                        && targets.bound_names().iter().any(|(n, _)| n == root) =>
                {
                    out.push(LocalEvent::Write {
                        span: *span,
                        value_span: ast.get(*value).span(),
                        write: LocalWrite::Multi(*value),
                    });
                }
                Node::BeginRescue { clauses, span, .. } if in_region(*span) => {
                    let spans: Vec<Span> = clauses
                        .iter()
                        .filter(|c| c.bound_name.as_deref() == Some(root))
                        .map(|c| c.span)
                        .collect();
                    if !spans.is_empty() {
                        out.push(LocalEvent::RescueBinding(spans));
                    }
                }
                Node::Call { receiver, method, args, span, .. }
                    if guards_here(*span)
                        && is_guard_method(method)
                        && (receiver.is_some_and(reads_root)
                            || args.iter().copied().any(reads_root)) =>
                {
                    let exact = is_exact_guard_method(method)
                        && receiver.is_some_and(reads_root)
                        && matches!(args.as_slice(), [c] if matches!(ast.get(*c), Node::ConstantRead { .. }));
                    out.push(if exact { LocalEvent::ExactGuard } else { LocalEvent::Guard });
                }
                Node::Case { predicate, span, .. }
                    if guards_here(*span) && predicate.is_some_and(reads_root) =>
                {
                    out.push(LocalEvent::Guard);
                }
                _ => {}
            }
        }
        out
    }
}

impl<'i> Typer<'i> {
    /// The [`ReachIndex`] of `ast`, built on first use and memoized on the
    /// [`Typer`] per `ast` identity (the `dead_cache` keying, plus the arena
    /// length as a guard against a reused address).
    pub(crate) fn reach_index(&self, ast: &LoweredAst) -> Rc<ReachIndex> {
        let key = (ast as *const LoweredAst as usize, ast.len());
        if let Some((cached_key, idx)) = &*self.reach_index.borrow() {
            if *cached_key == key {
                return idx.clone();
            }
        }
        let idx = Rc::new(ReachIndex::build(ast));
        *self.reach_index.borrow_mut() = Some((key, idx.clone()));
        idx
    }
}
