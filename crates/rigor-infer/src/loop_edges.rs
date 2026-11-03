//! `while`/`until` loop-entry and loop-exit edges (upstream `900bc64c`,
//! rigor#1359; `eval_loop` in `statement_evaluator.rb`).
//!
//! The reference evaluates a `while`/`until` body once, ENTERING it on the
//! predicate's loop-entry edge (`loop_pass_entry`: truthy for `while`, falsey
//! for `until`; a `begin … end while` body runs once unnarrowed), joins the
//! body's end scope into the post-predicate scope, and narrows the result on
//! the EXIT edge (`narrow_loop_exit_edge`: falsey for `while`, truthy for
//! `until`). The port's flow engines model neither edge — every engine keeps
//! a predicate local's pre-loop binding inside the body and after the loop —
//! so a guard whose edge collapses or widens the local (`until
//! h.is_a?(Unknown)`, `while h.is_a?(Unknown)` joined back in, `while q` on a
//! non-nil `q`) left the port firing where the reference is silent.
//!
//! This module is the shared decision, not a flow engine: for one loop and
//! one predicate local with the carrier an engine holds, does the reference's
//! edge leave that carrier UNCHANGED? Each engine keeps its binding where the
//! answer is yes and widens the local (the decline side) everywhere else. Only
//! a small allow-list of predicate shapes is decided at all; every other
//! predicate that reads the local declines.

use rigor_index::ClassOrdering;
use rigor_parse::{LoopKind, LoweredAst, Node, NodeId, Span, StatementsKind};
use rigor_types::{Interner, Scalar, Type, TypeId};

use crate::Typer;

/// One edge's effect on a local's carrier `T`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Edge {
    /// The edge keeps `T` exactly.
    Keep,
    /// The edge is unreachable for `T` (the reference narrows to `Bot`).
    Empty,
    /// Anything else — a narrower, wider or unmodelled answer.
    Other,
}

/// `a` then `b` on the same edge (`&&`'s truthy edge): an unreachable or
/// unmodelled first step decides alone.
pub(crate) fn seq(a: Edge, b: impl FnOnce() -> Edge) -> Edge {
    match a {
        Edge::Keep => b(),
        other => other,
    }
}

/// The join of two edges' scopes. `T ∪ Bot` is `T`.
pub(crate) fn join(a: Edge, b: Edge) -> Edge {
    match (a, b) {
        (Edge::Other, _) | (_, Edge::Other) => Edge::Other,
        (Edge::Keep, _) | (_, Edge::Keep) => Edge::Keep,
        (Edge::Empty, Edge::Empty) => Edge::Empty,
    }
}

/// What a guard narrows: a local, or a constant read keyed by its spelling
/// (upstream `1be2fd0b` keeps a constant's narrowing in a scope table keyed
/// by spelling — `::File` and `File` are different keys there, probes
/// g3 e/f) and whether it is rooted.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Subject {
    Local(String),
    Const(String, bool),
}

impl Subject {
    /// The subject `id` reads, if it is a bare local or static constant.
    pub(crate) fn of(ast: &LoweredAst, id: NodeId) -> Option<Subject> {
        match ast.get(id) {
            Node::LocalVariableRead { name, .. } => Some(Subject::Local(name.clone())),
            Node::ConstantRead { name, dynamic_base: false, rooted, .. } if !name.is_empty() => {
                let rooted = *rooted || name.starts_with("::");
                Some(Subject::Const(name.trim_start_matches("::").to_string(), rooted))
            }
            _ => None,
        }
    }

    /// Whether `id` is a read of this subject.
    pub(crate) fn is_read(&self, ast: &LoweredAst, id: NodeId) -> bool {
        Subject::of(ast, id).as_ref() == Some(self)
    }
}

/// Which side of a loop a use site reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LoopSide {
    /// Inside the body: the loop-entry edge.
    Body,
    /// After the loop: the exit edge over the joined scope.
    Exit,
}

impl<'i> Typer<'i> {
    /// The predicate-local names of a `while`/`until` loop `id` — every
    /// `LocalVariableRead` inside its predicate. Empty for a `for` loop
    /// (its collection narrows nothing) and for any other node.
    pub(crate) fn loop_predicate_locals(&self, ast: &LoweredAst, id: NodeId) -> Vec<String> {
        let Node::Loop { predicate: Some(p), kind, .. } = ast.get(id) else {
            return Vec::new();
        };
        if matches!(kind, LoopKind::For) {
            return Vec::new();
        }
        let mut out: Vec<String> = self
            .subjects_within(ast, *p)
            .into_iter()
            .filter_map(|(s, _)| match s {
                Subject::Local(n) => Some(n),
                Subject::Const(..) => None,
            })
            .collect();
        out.sort();
        out
    }

    /// [`Typer::widen_loop_exit`] for every `while`/`until` loop inside
    /// `span` (the statement an engine just stepped over without descending,
    /// which may hold a nested loop — `while c; until h.is_a?(X); end; end`).
    pub(crate) fn widen_loop_exits_within(
        &self,
        ast: &LoweredAst,
        span: Span,
        env: &mut crate::TypeEnv,
        interner: &mut Interner,
    ) {
        let idx = self.reach_index(ast);
        let loops: Vec<NodeId> = idx
            .nodes_within(ast, span)
            .filter(|&id| matches!(ast.get(id), Node::Loop { .. }))
            .collect();
        for id in loops {
            self.widen_loop_exit(ast, id, env, interner);
        }
    }

    /// Widen in `env` every predicate local of loop `id` whose binding the
    /// reference's exit edge does not leave unchanged — the post-loop half
    /// of [`Typer::loop_edge_keeps`] for an engine that threads one env
    /// through the statements and does not descend the loop body.
    pub(crate) fn widen_loop_exit(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        env: &mut crate::TypeEnv,
        interner: &mut Interner,
    ) {
        for name in self.loop_predicate_locals(ast, id) {
            let Some(&ty) = env.get(name.as_str()) else { continue };
            if matches!(interner.get(ty), Type::Dynamic(_) | Type::Top) {
                continue;
            }
            if !self.loop_edge_keeps(ast, id, &name, ty, LoopSide::Exit, interner) {
                let u = interner.untyped();
                crate::flow_writes::drop_indexed_narrowings(env, &name);
                env.insert(name, u);
            }
        }
    }

    /// The body hull of loop `id` — the span its body statements cover, or
    /// `None` for an empty body.
    pub(crate) fn loop_body_hull(ast: &LoweredAst, id: NodeId) -> Option<Span> {
        let Node::Loop { body, .. } = ast.get(id) else {
            return None;
        };
        crate::span_hull(ast, body)
    }

    /// Whether the reference keeps `local`'s carrier `ty` UNCHANGED on `side`
    /// of the `while`/`until` loop `id`. `false` is the decline answer: the
    /// caller widens the local there.
    ///
    /// * `Body`: the loop-entry edge must keep `ty` (an unreachable entry —
    ///   `while h.is_a?(String)` on an `Array` — is reference-silent in the
    ///   body, so it declines too). A `begin … end while` body runs before
    ///   the first test and always keeps.
    /// * `Exit`: the entry edge must keep `ty` or be unreachable (so the
    ///   joined body-end scope is `ty ∪ Bot = ty` — a widened entry, `while
    ///   h.is_a?(Unknown)`, poisons the join), and the exit edge must keep
    ///   it.
    ///
    /// The join argument ignores what the body does to the local: a rebind
    /// or mutation inside the loop is the callers' own (already declining)
    /// write machinery.
    pub(crate) fn loop_edge_keeps(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        local: &str,
        ty: TypeId,
        side: LoopSide,
        interner: &Interner,
    ) -> bool {
        let Node::Loop { predicate: Some(p), kind, .. } = ast.get(id) else {
            return true;
        };
        let (entry_truthy, begin_modifier) = match *kind {
            LoopKind::While { begin_modifier } => (true, begin_modifier),
            LoopKind::Until { begin_modifier } => (false, begin_modifier),
            LoopKind::For => return true,
        };
        let p = *p;
        let entry = if begin_modifier {
            Edge::Keep
        } else {
            self.edge_effect(ast, p, local, ty, entry_truthy, interner)
        };
        match side {
            LoopSide::Body => entry == Edge::Keep,
            LoopSide::Exit => {
                entry != Edge::Other
                    && self.edge_effect(ast, p, local, ty, !entry_truthy, interner) == Edge::Keep
            }
        }
    }

    /// `pred`'s effect on `local` (carrier `ty`) on its truthy (`edge`) or
    /// falsey edge — the allow-listed slice of the reference's `Narrowing`
    /// predicate analysis.
    fn edge_effect(
        &self,
        ast: &LoweredAst,
        pred: NodeId,
        local: &str,
        ty: TypeId,
        edge: bool,
        interner: &Interner,
    ) -> Edge {
        self.subject_edge(ast, pred, &Subject::Local(local.to_string()), ty, edge, false, interner)
    }

    /// [`Typer::edge_effect`] over any guard [`Subject`]. With
    /// `respond_only` every atom but `subject.respond_to?(:m)` counts as
    /// keeping the carrier — for a caller whose other engines already model
    /// those atoms.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn subject_edge(
        &self,
        ast: &LoweredAst,
        pred: NodeId,
        subject: &Subject,
        ty: TypeId,
        edge: bool,
        respond_only: bool,
        interner: &Interner,
    ) -> Edge {
        if !self.reads_subject(ast, pred, subject) {
            // An operand that never mentions the subject leaves it alone on
            // both edges — unless it folds, which makes one edge dead
            // (`… && false`): decline that rather than model it.
            return if self.expr_truthiness(ast, pred, &mut Vec::new()).is_some() {
                Edge::Other
            } else {
                Edge::Keep
            };
        }
        let eff = |n: NodeId, e: bool| {
            self.subject_edge(ast, n, subject, ty, e, respond_only, interner)
        };
        let atom = |e: Edge| if respond_only { Edge::Keep } else { e };
        // Any other expression that merely READS the subject: a local may be
        // narrowed by shapes this allow-list does not decide (`h.empty?`), so
        // it declines; a constant is narrowed ONLY by the guard shapes above
        // (`1be2fd0b`), so `Hash === x`, `C.critical = v` or `n < LIMIT` keep
        // it (rspec-mocks `Hash.ruby2_keywords_hash?` under `Hash === x`
        // fires on the oracle).
        let unknown = match subject {
            Subject::Const(..) => Edge::Keep,
            Subject::Local(_) => atom(Edge::Other),
        };
        match ast.get(pred) {
            Node::Statements { body, kind: StatementsKind::Sequence, .. } if body.len() == 1 => {
                eff(body[0], edge)
            }
            _ if subject.is_read(ast, pred) => atom(match self.truthiness(interner, ty) {
                Some(t) if t == edge => Edge::Keep,
                Some(_) => Edge::Empty,
                None => Edge::Other,
            }),
            Node::Logical { left, right, is_and, .. } => {
                let (l, r) = (*left, *right);
                // A constant's narrowing is a scope-table entry: a join with
                // a path that never narrowed it drops the entry, so `Keep`
                // absorbs (`if File.respond_to?(:x) and File.respond_to?(:y)
                // … else File.x` fires on the reference).
                let j = |a: Edge, b: Edge| match subject {
                    Subject::Const(..) if a == Edge::Keep || b == Edge::Keep => Edge::Keep,
                    _ => join(a, b),
                };
                match (*is_and, edge) {
                    (true, true) => seq(eff(l, true), || eff(r, true)),
                    (true, false) => j(eff(l, false), seq(eff(l, true), || eff(r, false))),
                    (false, true) => j(eff(l, true), seq(eff(l, false), || eff(r, true))),
                    (false, false) => seq(eff(l, false), || eff(r, false)),
                }
            }
            Node::Call { receiver: Some(r), method, args, block_body, safe_nav: false, span, .. }
                if block_body.is_empty() =>
            {
                let (r, span) = (*r, *span);
                let recv_is_subject = subject.is_read(ast, r);
                match (method.as_str(), args.as_slice()) {
                    ("!", []) => eff(r, !edge),
                    ("nil?", []) if recv_is_subject => atom(match self.nilness(interner, ty) {
                        Some(is_nil) if is_nil == edge => Edge::Keep,
                        Some(_) => Edge::Empty,
                        None => Edge::Other,
                    }),
                    ("is_a?" | "kind_of?" | "instance_of?", [c]) if recv_is_subject => atom(
                        self.class_edge(ast, *c, span, method == "instance_of?", ty, edge, interner),
                    ),
                    ("===", [arg]) if subject.is_read(ast, *arg) && !self.reads_subject(ast, r, subject) => {
                        atom(self.class_edge(ast, r, span, false, ty, edge, interner))
                    }
                    ("respond_to?", [m]) if recv_is_subject => {
                        let name = match ast.get(*m) {
                            Node::SymbolLit { value, .. } | Node::StringLit { value, .. } => {
                                Some(value.as_str())
                            }
                            _ => None,
                        };
                        match name {
                            Some(m) => self.respond_edge(ty, m, edge, interner),
                            None => Edge::Other,
                        }
                    }
                    _ => unknown,
                }
            }
            _ => unknown,
        }
    }

    /// `subject.respond_to?(:m)` (upstream `1be2fd0b`, rigor#1453): the
    /// truthy edge drops every member whose class RBS knows to lack `m`, and
    /// a carrier with no member left reads `Dynamic[top]`. It keeps the
    /// carrier only when the carrier provably has `m`. The falsey edge keeps
    /// it (probe: `unless s.respond_to?(:upcase); s.frob; end` fires `for
    /// "x"`; `unless File.respond_to?(:makedirs)` fires `for
    /// singleton(File)`).
    fn respond_edge(&self, ty: TypeId, method: &str, edge: bool, interner: &Interner) -> Edge {
        if !edge {
            return Edge::Keep;
        }
        let has = match interner.get(ty) {
            Type::Singleton(class) => self
                .source
                .class_name_for_id(*class)
                .map(|n| self.index.class_has_singleton_method(n, method)),
            Type::Constant(_) | Type::Nominal { .. } | Type::Tuple(_) | Type::HashShape(_) => self
                .index
                .class_name_of(interner, ty)
                .map(|n| self.index.class_has_method(n, method)),
            _ => None,
        };
        if has == Some(true) {
            Edge::Keep
        } else {
            Edge::Other
        }
    }

    /// `local.is_a?(C)` / `kind_of?` / `instance_of?` / `C === local` on a
    /// `Constant` or `Nominal` carrier. `Subclass`/`Equal` keeps the carrier on
    /// the truthy edge and collapses it on the falsey one (probes: `until
    /// h.is_a?(Array)` fires `for Array`, `while h.is_a?(Array)` is silent
    /// after the loop); `Disjoint` is the mirror image (`while
    /// h.is_a?(String)` fires after the loop and is silent inside), and an
    /// unorderable guard keeps the carrier on its falsey edge. Every other
    /// answer — a proper subclass guard that narrows, an unorderable guard's
    /// widening truthy edge — declines.
    #[allow(clippy::too_many_arguments)]
    fn class_edge(
        &self,
        ast: &LoweredAst,
        class_node: NodeId,
        use_span: Span,
        exact: bool,
        ty: TypeId,
        edge: bool,
        interner: &Interner,
    ) -> Edge {
        if !matches!(interner.get(ty), Type::Constant(_) | Type::Nominal { .. }) {
            return Edge::Other;
        }
        let Some((class, true)) = self.guard_constant(ast, class_node, use_span) else {
            return Edge::Other;
        };
        let Some(carrier) = self.index.class_name_of(interner, ty) else {
            return Edge::Other;
        };
        if exact {
            // `instance_of?` collapses on any name mismatch (`exact:` path);
            // an exact match keeps on the truthy edge, while its falsey edge
            // is left undecided.
            return match (carrier == class, edge) {
                (true, true) => Edge::Keep,
                (true, false) => Edge::Other,
                (false, true) => Edge::Empty,
                (false, false) => Edge::Keep,
            };
        }
        match (self.index.class_ordering(carrier, &class), edge) {
            (ClassOrdering::Subclass | ClassOrdering::Equal, true) => Edge::Keep,
            (ClassOrdering::Subclass | ClassOrdering::Equal, false) => Edge::Empty,
            (ClassOrdering::Disjoint, true) => Edge::Empty,
            (ClassOrdering::Disjoint, false) => Edge::Keep,
            // The falsey edge of an unorderable guard keeps the carrier
            // (`begin … end while h.is_a?(Unknown)` fires `for Array` after
            // the loop) — only its truthy edge widens.
            (ClassOrdering::Unknown, false) => Edge::Keep,
            _ => Edge::Other,
        }
    }
}

impl<'i> Typer<'i> {
    /// Whether `id`'s span holds a read of `subject` (span containment, so
    /// a read an unmodelled construct keeps off the child links still
    /// counts — the decline side).
    pub(crate) fn reads_subject(&self, ast: &LoweredAst, id: NodeId, subject: &Subject) -> bool {
        let idx = self.reach_index(ast);
        let span = ast.get(id).span();
        let found = idx.nodes_within(ast, span).any(|n| subject.is_read(ast, n));
        found
    }

    /// Every subject read inside `id`'s span, deduplicated, in start order.
    pub(crate) fn subjects_within(&self, ast: &LoweredAst, id: NodeId) -> Vec<(Subject, NodeId)> {
        let idx = self.reach_index(ast);
        let span = ast.get(id).span();
        let mut out: Vec<(Subject, NodeId)> = Vec::new();
        for n in idx.nodes_within(ast, span) {
            if let Some(s) = Subject::of(ast, n) {
                if !out.iter().any(|(o, _)| *o == s) {
                    out.push((s, n));
                }
            }
        }
        out
    }

    /// `Some(true)` when every value of `ty` is truthy, `Some(false)` when
    /// every value is falsey, `None` otherwise (and for `Dynamic`/`Top`).
    fn truthiness(&self, interner: &Interner, ty: TypeId) -> Option<bool> {
        match interner.get(ty) {
            Type::Constant(s) => Some(!matches!(s, Scalar::Nil | Scalar::Bool(false))),
            Type::Tuple(_) | Type::HashShape(_) | Type::Singleton(_) => Some(true),
            Type::Nominal { .. } => self.nominal_excludes(interner, ty, "FalseClass")
                .then(|| self.nominal_excludes(interner, ty, "NilClass"))
                .and_then(|ok| ok.then_some(true)),
            _ => None,
        }
    }

    /// `Some(true)` when `ty` is exactly `nil`, `Some(false)` when it is
    /// never `nil`, `None` otherwise.
    fn nilness(&self, interner: &Interner, ty: TypeId) -> Option<bool> {
        match interner.get(ty) {
            Type::Constant(s) => Some(matches!(s, Scalar::Nil)),
            Type::Tuple(_) | Type::HashShape(_) | Type::Singleton(_) => Some(false),
            Type::Nominal { .. } => self.nominal_excludes(interner, ty, "NilClass").then_some(false),
            _ => None,
        }
    }

    /// Whether a `Nominal` carrier provably cannot hold an instance of
    /// `other` (`NilClass` / `FalseClass`): its class resolves and is
    /// neither `other` nor one of its ancestors (`Object`, `Kernel`, …).
    fn nominal_excludes(&self, interner: &Interner, ty: TypeId, other: &str) -> bool {
        let Some(name) = self.index.class_name_of(interner, ty) else {
            return false;
        };
        matches!(
            self.index.class_ordering(other, name),
            ClassOrdering::Disjoint | ClassOrdering::Superclass
        )
    }
}
