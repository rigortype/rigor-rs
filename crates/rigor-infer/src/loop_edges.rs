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
enum Edge {
    /// The edge keeps `T` exactly.
    Keep,
    /// The edge is unreachable for `T` (the reference narrows to `Bot`).
    Empty,
    /// Anything else — a narrower, wider or unmodelled answer.
    Other,
}

/// `a` then `b` on the same edge (`&&`'s truthy edge): an unreachable or
/// unmodelled first step decides alone.
fn seq(a: Edge, b: impl FnOnce() -> Edge) -> Edge {
    match a {
        Edge::Keep => b(),
        other => other,
    }
}

/// The join of two edges' scopes. `T ∪ Bot` is `T`.
fn join(a: Edge, b: Edge) -> Edge {
    match (a, b) {
        (Edge::Other, _) | (_, Edge::Other) => Edge::Other,
        (Edge::Keep, _) | (_, Edge::Keep) => Edge::Keep,
        (Edge::Empty, Edge::Empty) => Edge::Empty,
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
        let mut out = Vec::new();
        local_reads(ast, *p, &mut out);
        out.sort();
        out.dedup();
        out
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
        if !reads_local(ast, pred, local) {
            // An operand that never mentions the local leaves it alone on
            // both edges — unless it folds, which makes one edge dead
            // (`… && false`): decline that rather than model it.
            return if self.expr_truthiness(ast, pred, &mut Vec::new()).is_some() {
                Edge::Other
            } else {
                Edge::Keep
            };
        }
        match ast.get(pred) {
            Node::Statements { body, kind: StatementsKind::Sequence, .. } if body.len() == 1 => {
                self.edge_effect(ast, body[0], local, ty, edge, interner)
            }
            Node::LocalVariableRead { name, .. } if name == local => {
                match self.truthiness(interner, ty) {
                    Some(t) if t == edge => Edge::Keep,
                    Some(_) => Edge::Empty,
                    None => Edge::Other,
                }
            }
            Node::Logical { left, right, is_and, .. } => {
                let (l, r) = (*left, *right);
                let eff = |n: NodeId, e: bool| self.edge_effect(ast, n, local, ty, e, interner);
                match (*is_and, edge) {
                    (true, true) => seq(eff(l, true), || eff(r, true)),
                    (true, false) => join(eff(l, false), seq(eff(l, true), || eff(r, false))),
                    (false, true) => join(eff(l, true), seq(eff(l, false), || eff(r, true))),
                    (false, false) => seq(eff(l, false), || eff(r, false)),
                }
            }
            Node::Call { receiver: Some(r), method, args, block_body, safe_nav: false, span, .. }
                if block_body.is_empty() =>
            {
                let (r, span) = (*r, *span);
                let recv_is_local = matches!(
                    ast.get(r),
                    Node::LocalVariableRead { name, .. } if name == local
                );
                match (method.as_str(), args.as_slice()) {
                    ("!", []) => self.edge_effect(ast, r, local, ty, !edge, interner),
                    ("nil?", []) if recv_is_local => match self.nilness(interner, ty) {
                        Some(is_nil) if is_nil == edge => Edge::Keep,
                        Some(_) => Edge::Empty,
                        None => Edge::Other,
                    },
                    ("is_a?" | "kind_of?" | "instance_of?", [c]) if recv_is_local => {
                        self.class_edge(ast, *c, span, method == "instance_of?", ty, edge, interner)
                    }
                    ("===", [arg])
                        if matches!(
                            ast.get(*arg),
                            Node::LocalVariableRead { name, .. } if name == local
                        ) && !reads_local(ast, r, local) =>
                    {
                        self.class_edge(ast, r, span, false, ty, edge, interner)
                    }
                    _ => Edge::Other,
                }
            }
            _ => Edge::Other,
        }
    }

    /// `local.is_a?(C)` / `kind_of?` / `instance_of?` / `C === local` on a
    /// `Constant` or `Nominal` carrier. `Subclass`/`Equal` keeps the carrier on
    /// the truthy edge and collapses it on the falsey one (probes: `until
    /// h.is_a?(Array)` fires `for Array`, `while h.is_a?(Array)` is silent
    /// after the loop); `Disjoint` is the mirror image (`while
    /// h.is_a?(String)` fires after the loop and is silent inside). Every
    /// other ordering — a proper subclass guard that narrows, an unresolvable
    /// class that widens a nominal — declines.
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
            _ => Edge::Other,
        }
    }
}

impl<'i> Typer<'i> {
    /// `Some(true)` when every value of `ty` is truthy, `Some(false)` when
    /// every value is falsey, `None` otherwise (and for `Dynamic`/`Top`).
    fn truthiness(&self, interner: &Interner, ty: TypeId) -> Option<bool> {
        match interner.get(ty) {
            Type::Constant(s) => Some(!matches!(s, Scalar::Nil | Scalar::Bool(false))),
            Type::Tuple(_) | Type::HashShape(_) => Some(true),
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
            Type::Tuple(_) | Type::HashShape(_) => Some(false),
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

/// Every `LocalVariableRead` name under `id`.
fn local_reads(ast: &LoweredAst, id: NodeId, out: &mut Vec<String>) {
    let span = ast.get(id).span();
    for (_, n) in ast.iter() {
        if let Node::LocalVariableRead { name, span: s } = n {
            if span.0 <= s.0 && s.1 <= span.1 {
                out.push(name.clone());
            }
        }
    }
}

/// Whether `id`'s span holds a read of `local`.
fn reads_local(ast: &LoweredAst, id: NodeId, local: &str) -> bool {
    let span = ast.get(id).span();
    ast.iter().any(|(_, n)| {
        matches!(n, Node::LocalVariableRead { name, span: s }
            if name == local && span.0 <= s.0 && s.1 <= span.1)
    })
}

