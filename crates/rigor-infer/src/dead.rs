//! Provably-dead positions (rigor-rs#368): the spans whose writes the
//! reference never lets bind for a later read, and whose predicate subjects
//! read as `Bot`.
//!
//! Two mechanisms, both from `statement_evaluator.rb`:
//!
//! - **Folded `if`/`unless` arms** — `live_branch_for_if`/`_for_unless`
//!   (`:5005`/`:5012`) discard a branch once `branch_certainty` proves the
//!   predicate. The discarded arm is never evaluated at all: `propagate`
//!   fills every node inside it with the arm's *entry* scope — the predicate's
//!   edge scope — so its own writes are invisible even to reads inside the
//!   arm (`q = nil; if q; s = "y"; s.w; end` is silent on the oracle) and they
//!   never join the continuation (`x = 1; if false; x = "s"; end; x.w` fires
//!   `for 1`). A bare-local predicate subject narrows to `Bot` on the dead
//!   edge (`q = nil; if q; q.w; end` is silent on the oracle); the
//!   narrowing-call shapes (`q == "x"`, `q.is_a?(C)`, `!q`, `a && b`) narrow
//!   the same way or decline — flooring every subject local to `Bot` is
//!   always at or below the reference's edge scope.
//! - **Terminating `rescue` arms** — `collect_rescue_chain_results` joins
//!   only `live_rescues`: an arm `branch_terminates?` drops from the
//!   post-`begin` scope, yet its writes still bind for reads INSIDE the arm
//!   and — probed — inside `ensure` (`begin 1 rescue x = 2; x.w; raise end`
//!   fires `w for 2` on the oracle; the same write leaks into `ensure`).
//!   `branch_unconditionally_exits?` (`:5027`) is `return`, an argument-less
//!   `next`/`break`, an implicit-self `raise`/`throw`/`exit`/`abort`/`fail`
//!   call, a sequence whose tail exits, or an `if`/`unless` whose arms both
//!   exit — plus the literal-tail fold making `raise if helper` exit when
//!   `def helper = true`.
//!
//! The discipline is strict-subset: `dead` only ever withholds a write or a
//! binding, and the certainty tests fold SYNTAX (`expr_scalar`-level pins and
//! literal tails), not env-typed carriers, so a position is only marked dead
//! when the oracle proves it.

use rigor_parse::{JumpKind, LoweredAst, Node, NodeId, Span, StatementsKind};

use rigor_types::Scalar;

use crate::source_index::DefKind;
use crate::Typer;

/// `outer` contains `inner` — half-open byte spans, equal spans count.
fn contains(outer: Span, inner: Span) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}

/// The implicit-self calls `branch_unconditionally_exits?` counts as exits
/// (`EXIT_CALL_NAMES`, `statement_evaluator.rb:5025`).
const EXIT_CALLS: &[&str] = &["raise", "throw", "exit", "abort", "fail"];

/// One `rescue` clause of a real `begin`/`rescue`: whether the arm falls
/// through to the post-`begin` join, and the spans that bound "after the
/// begin" and "inside the ensure body".
#[derive(Debug, Clone)]
struct RescueArm {
    /// The clause's own span (header + body).
    span: Span,
    /// The arm contributes to the continuation only when it falls through —
    /// `false` once `branch_terminates?` drops it from `live_rescues`.
    live: bool,
    /// The owning `begin` node's END byte offset — `use.0 >= end` reads the
    /// post-`begin` scope.
    end: usize,
    /// The ensure body's hull when the `begin` has one — the reference joins
    /// exit scopes *through* ensure, so a live arm's writes reach it.
    ensure: Option<Span>,
}

/// The file's provably-dead positions.
#[derive(Debug, Default, Clone)]
pub(crate) struct DeadPositions {
    /// `(arm-body hull, predicate-subject locals)` per folded `if`/`unless`
    /// arm the reference never evaluates.
    folded: Vec<(Span, Vec<String>)>,
    /// One entry per `rescue` clause of every real `begin`/`rescue`.
    rescue: Vec<RescueArm>,
}

impl DeadPositions {
    /// Whether a write at `write` can bind for a read at `use_` — the
    /// reference's live-branch / live-rescue drop (rigor-rs#368).
    ///
    /// A folded-arm write reaches nothing at all (the arm is never
    /// evaluated). A rescue-arm write reaches a read inside its own arm
    /// unconditionally, and a read after the `begin` or inside `ensure` only
    /// when the arm falls through (`live`); reads elsewhere — a sibling arm,
    /// the `else` clause — keep the write, as the reference evaluates them
    /// from the arm's entry scope.
    pub(crate) fn write_reaches(&self, write: Span, use_: Span) -> bool {
        if self.folded.iter().any(|(d, _)| contains(*d, write)) {
            return false;
        }
        if let Some(arm) = self.rescue.iter().find(|a| contains(a.span, write)) {
            return contains(arm.span, use_)
                || (arm.live && (use_.0 >= arm.end || arm.ensure.is_some_and(|e| contains(e, use_))));
        }
        true
    }

    /// Whether `span` sits inside a folded-dead arm — for questions with no
    /// use site (`definitely_assigns`, the `entry_descend` arm walk).
    pub(crate) fn covers(&self, span: Span) -> bool {
        self.folded.iter().any(|(d, _)| contains(*d, span))
    }

    /// Whether `hull` IS one of the folded-dead arm hulls — `entry_descend`
    /// compares the arm it is about to walk against the recorded set.
    pub(crate) fn arm_dead(&self, hull: Option<Span>) -> bool {
        hull.is_some_and(|h| self.folded.iter().any(|(d, _)| *d == h))
    }

    /// Whether any folded-dead arm exists — `check_env_at`'s boundary pass
    /// only records when one could matter.
    pub(crate) fn has_folded(&self) -> bool {
        !self.folded.is_empty()
    }

    /// Whether any real `rescue` clause exists — its sites need the
    /// boundary replay (the arm's own bindings and `=> e`), not the flat
    /// env (`rescue => e; e.w` fires `w for StandardError` on the oracle).
    pub(crate) fn has_rescue(&self) -> bool {
        !self.rescue.is_empty()
    }

    /// Whether `site` sits inside a `rescue` clause's own span — where the
    /// boundary replay (not the flat env) carries the arm's bindings.
    pub(crate) fn in_rescue(&self, site: Span) -> bool {
        self.rescue.iter().any(|a| contains(a.span, site))
    }

    /// The predicate-subject locals a site inside a folded-dead arm reads as
    /// `Bot` — the reference's `propagate` fill of the dead edge's scope.
    pub(crate) fn subjects_at(&self, site: Span) -> Option<&[String]> {
        self.folded
            .iter()
            .find(|(d, _)| contains(*d, site))
            .map(|(_, subjects)| subjects.as_slice())
    }
}

/// The hull covering a statement list's spans, or `None` for an empty arm.
pub(crate) fn body_hull(ast: &LoweredAst, body: &[NodeId]) -> Option<Span> {
    let lo = body.iter().map(|&s| ast.get(s).span().0).min()?;
    let hi = body.iter().map(|&s| ast.get(s).span().1).max()?;
    Some((lo, hi))
}

impl<'i> Typer<'i> {
    /// Compute the file's [`DeadPositions`], memoized on the [`Typer`] per
    /// `ast` identity. `seen` is the shared `expr_scalar` recursion cap.
    ///
    /// Folding a predicate consults `local_reach`, which itself needs the
    /// dead set — a `dead_positions` → `expr_truthiness` → `local_reach` →
    /// `dead_positions` cycle that would otherwise not converge (the `seen`
    /// cap bounds DEPTH but not the per-predicate branching of the recompute).
    /// A re-entrant call therefore answers with the empty set — "nothing is
    /// dead" is the conservative half, so the walk only ever folds an arm
    /// when the predicate's own reach query cannot recurse back into it.
    pub(crate) fn dead_positions(
        &self,
        ast: &LoweredAst,
        seen: &mut Vec<String>,
    ) -> DeadPositions {
        let key = ast as *const LoweredAst as usize;
        if let Some((cached_key, cached)) = &*self.dead_cache.borrow() {
            if *cached_key == key {
                return cached.clone();
            }
        }
        if self.dead_in_flight.replace(true) {
            return DeadPositions::default();
        }
        let mut dead = DeadPositions::default();
        for (_, n) in ast.iter() {
            match n {
                Node::If {
                    predicate,
                    then_body,
                    else_body,
                    is_unless,
                    ..
                } => {
                    let Some(truthy) = self.expr_truthiness(ast, *predicate, seen) else {
                        continue;
                    };
                    // `if` runs `then` on truthy; `unless` swaps the arms — the
                    // OTHER arm is dead.
                    let dead_body = if truthy != *is_unless {
                        else_body
                    } else {
                        then_body
                    };
                    if let Some(hull) = body_hull(ast, dead_body) {
                        let mut subjects = Vec::new();
                        self.predicate_locals(ast, *predicate, &mut subjects, 4);
                        dead.folded.push((hull, subjects));
                    }
                }
                Node::BeginRescue {
                    clauses,
                    ensure_body,
                    span,
                    ..
                } if !clauses.is_empty() => {
                    let ensure = body_hull(ast, ensure_body);
                    for c in clauses {
                        dead.rescue.push(RescueArm {
                            span: c.span,
                            live: !self.arm_exits(ast, &c.body, seen),
                            end: span.1,
                            ensure,
                        });
                    }
                }
                _ => {}
            }
        }
        self.dead_in_flight.set(false);
        *self.dead_cache.borrow_mut() = Some((key, dead.clone()));
        dead
    }

    /// The three-valued truthiness a predicate PROVES without an env — the
    /// syntax-only half of `branch_certainty` (`statement_evaluator.rb:5048`
    /// → `Narrowing.predicate_certainty`, `narrowing.rb:134`). `Some(true)` =
    /// always truthy, `Some(false)` = always falsey, `None` = live on both
    /// edges (the answer for `Bot` and every unfolded carrier).
    pub(crate) fn expr_truthiness(
        &self,
        ast: &LoweredAst,
        pred: NodeId,
        seen: &mut Vec<String>,
    ) -> Option<bool> {
        let scalar = match ast.get(pred) {
            // A bare-local write's value decides the predicate (`if (q = nil)`).
            Node::LocalVariableWrite { value, .. } => {
                return self.expr_truthiness(ast, *value, seen);
            }
            // `a && b` / `a || b` — `Node::Logical`; the short-circuit folds:
            // `&&` is falsey when the left folds falsey, or the right does
            // after a truthy left; `||` mirrors. An unresolved left keeps the
            // value unknown (`u && nil` is `u | nil`, not falsey).
            Node::Logical {
                left,
                right,
                is_and,
                ..
            } => {
                let (l, r) = (
                    self.expr_truthiness(ast, *left, seen),
                    self.expr_truthiness(ast, *right, seen),
                );
                return if *is_and {
                    match (l, r) {
                        (Some(false), _) | (Some(true), Some(false)) => Some(false),
                        (Some(true), Some(true)) => Some(true),
                        _ => None,
                    }
                } else {
                    match (l, r) {
                        (Some(true), _) | (Some(false), Some(true)) => Some(true),
                        (Some(false), Some(false)) => Some(false),
                        _ => None,
                    }
                };
            }
            // An implicit-self no-arg call folds through the interprocedural
            // literal tail (`def helper = true; … raise if helper`) — the same
            // fold `flow_predicate_type` reaches inside a class, extended to a
            // same-file top-level `def` the project table does not key.
            Node::Call {
                receiver: None,
                method,
                args,
                block_body,
                span,
                ..
            } if args.is_empty() && block_body.is_empty() => {
                let qual = self.enclosing_prefix(*span).join("::");
                self.source
                    .implicit_self_literal(&qual, DefKind::Instance, method)
                    .or_else(|| self.file_def_literal(ast, method, seen))
            }
            _ => self.expr_scalar(ast, pred, seen),
        };
        scalar.map(|s| !matches!(s, Scalar::Nil | Scalar::Bool(false)))
    }

    /// The folded literal a same-file TOP-LEVEL `def name = <literal tail>`
    /// returns — the `expr_scalar` half of `implicit_self_literal` for the
    /// `definer`-keyed table misses (a `def` at file scope, or an
    /// empty-source `Typer` in tests). Definitions nested inside a class,
    /// module or another def are not toplevel.
    pub(crate) fn file_def_literal(
        &self,
        ast: &LoweredAst,
        name: &str,
        seen: &mut Vec<String>,
    ) -> Option<Scalar> {
        let containers: Vec<Span> = ast
            .iter()
            .filter_map(|(_, n)| match n {
                Node::ClassDef { span, .. }
                | Node::ModuleDef { span, .. }
                | Node::Definition { span, .. } => Some(*span),
                _ => None,
            })
            .collect();
        // `degrade_if_overridable`: a toplevel `def` is an `Object` instance
        // method, and every project class descends `Object` — a same-named
        // def anywhere overrides it, so the literal cannot fold.
        if self.source.toplevel_def_overridden(name) {
            return None;
        }
        // The LAST same-named `def` wins a call (Ruby re-definition), so a
        // shadowing second `def` never lets the first one's literal fold.
        ast.iter()
            .filter_map(|(_, n)| {
                let Node::Definition {
                    name: Some(m),
                    body,
                    span,
                    ..
                } = n
                else {
                    return None;
                };
                if m != name || containers.iter().any(|&c| c != *span && contains(c, *span)) {
                    return None;
                }
                body.last().copied()
            })
            .last()
            .and_then(|tail| self.expr_scalar(ast, tail, seen))
    }

    /// The locals a predicate NARROWS on the dead edge, floored to `Bot` for
    /// a site inside the folded arm. Bare-local subjects are the reference's
    /// edge-narrow (`if q` with `q` folded falsey types `q` `Bot` inside the
    /// arm); the comparison/guard receivers (`==`, `!=`, `nil?`, `is_a?`, …)
    /// and `!`/`&&`/`||` compounds narrow those locals the same way or
    /// decline — `Bot` is never above the reference's edge scope.
    pub(crate) fn predicate_locals(
        &self,
        ast: &LoweredAst,
        pred: NodeId,
        out: &mut Vec<String>,
        depth: u32,
    ) {
        if depth == 0 {
            return;
        }
        match ast.get(pred) {
            Node::LocalVariableRead { name, .. } | Node::LocalVariableWrite { name, .. }
                if !out.contains(name) =>
            {
                out.push(name.clone());
            }
            Node::Logical { left, right, .. } => {
                self.predicate_locals(ast, *left, out, depth - 1);
                self.predicate_locals(ast, *right, out, depth - 1);
            }
            Node::Call {
                receiver,
                method,
                args,
                block_body,
                safe_nav,
                ..
            } if block_body.is_empty() && !*safe_nav => match method.as_str() {
                "!" if args.is_empty() => {
                    if let Some(r) = receiver {
                        self.predicate_locals(ast, *r, out, depth - 1);
                    }
                }
                "==" | "!=" | "eql?" | "equal?" | "nil?" | "is_a?" | "kind_of?"
                | "instance_of?" | "===" | "respond_to?" => {
                    if let Some(r) = receiver {
                        self.predicate_locals(ast, *r, out, depth - 1);
                    }
                    if matches!(method.as_str(), "==" | "!=" | "eql?" | "equal?" | "===") {
                        for &a in args {
                            self.predicate_locals(ast, a, out, depth - 1);
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// `branch_terminates?` for an arm body — the sequence's LAST statement
    /// exits (`branch_unconditionally_exits?`, `statement_evaluator.rb:5027`).
    /// The reference's other half — a `Bot`-typed non-exit arm — is
    /// represented where the port can see it: a folded `if`/`unless` tail
    /// whose live arm exits (`raise if helper`) and a write/call whose value
    /// position exits (`q = raise`, `f(raise)`). Anything else stays live —
    /// keeping a dead arm's writes only ever loses diagnostics, it never
    /// invents one.
    pub(crate) fn arm_exits(
        &self,
        ast: &LoweredAst,
        body: &[NodeId],
        seen: &mut Vec<String>,
    ) -> bool {
        body.last().is_some_and(|&l| self.stmt_exits(ast, l, seen))
    }

    /// `branch_unconditionally_exits?` (`statement_evaluator.rb:5027`): `return`,
    /// an argument-less `next`/`break`, an implicit-self `raise`/`throw`/`exit`/
    /// `abort`/`fail` call, a sequence whose last statement exits, an
    /// `if`/`unless` whose live arm exits (folded predicate) or whose BOTH
    /// arms exit, and — the `branch_terminates?`/`Bot` half the port can see —
    /// a write or call whose evaluated value exits.
    pub(crate) fn stmt_exits(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        seen: &mut Vec<String>,
    ) -> bool {
        match ast.get(id) {
            Node::Return { .. } => true,
            // An argument-less `next`/`break`/`redo`/`retry` jumps out — prism
            // lowers the bare keywords to `Other { jump: Some(..) }`; the
            // value-bearing forms ride `StatementsKind::Jump` carriers whose
            // writes stay reachable, so the carrier itself is not an exit here.
            Node::Other {
                jump: Some(JumpKind::Next | JumpKind::Break),
                ..
            } => true,
            Node::Call {
                receiver: None,
                method,
                ..
            } if EXIT_CALLS.contains(&method.as_str()) => true,
            // `next x` / `break x` still exit — the value is the jump's
            // payload, not a fall-through (`NextNode`/`BreakNode` count
            // unconditionally on the oracle). `redo`/`retry` re-enter, so
            // their carriers are not exits.
            Node::Statements {
                kind: StatementsKind::Jump(JumpKind::Next | JumpKind::Break),
                ..
            } => true,
            Node::Statements { body, kind, .. } => {
                // Only a straight-line sequence's tail exits; `Recovered` /
                // `Inert` / `Jump` carriers do not run their tail in order.
                matches!(kind, StatementsKind::Sequence)
                    && self.arm_exits(ast, body, seen)
            }
            Node::If {
                predicate,
                then_body,
                else_body,
                is_unless,
                ..
            } => match self.expr_truthiness(ast, *predicate, seen) {
                // The fold keeps only the live arm — `raise if helper` exits
                // when `helper` folds truthy.
                Some(truthy) => {
                    let live = if truthy != *is_unless {
                        then_body
                    } else {
                        else_body
                    };
                    self.arm_exits(ast, live, seen)
                }
                None => {
                    self.arm_exits(ast, then_body, seen)
                        && self.arm_exits(ast, else_body, seen)
                }
            },
            // A clause-less `begin` is a parenthesised / else-only carrier —
            // its tail is the body's tail.
            Node::BeginRescue { clauses, body, .. } if clauses.is_empty() => {
                self.arm_exits(ast, body, seen)
            }
            // A write whose value position exits types the write `Bot`
            // (`x = raise`); a call with an exit in an evaluated operand the
            // same (`f(raise)`). A receiver call's exit is already covered by
            // `receiver`'s own check.
            Node::LocalVariableWrite { value, .. } => self.stmt_exits(ast, *value, seen),
            Node::Call { receiver, args, .. } => {
                receiver.is_some_and(|r| self.stmt_exits(ast, r, seen))
                    || args.iter().any(|&a| self.stmt_exits(ast, a, seen))
            }
            _ => false,
        }
    }
}
