//! Top-level flow evaluation: the straight-line env builders
//! ([`Typer::build_toplevel_env`], [`Typer::build_toplevel_check_env`]) and
//! the branch-joined local-constant propagation behind
//! [`Typer::always_truthy_snapshots`] (ADR-0022), which
//! `flow.always-truthy-condition` reads.

use std::borrow::Cow;
use std::rc::Rc;
use std::collections::HashMap;

use rigor_parse::{LoweredAst, Node, NodeId, StatementsKind};
use rigor_types::{Interner, Scalar, ShapeKey, Type, TypeId};

use crate::{
    closure_mutations, collect_flow_writes, indexed_flow_writes, join_flow_envs,
    multi_target_binder, qualify_self, toplevel_argument_floor, toplevel_mutations, toplevel_rebinds,
    widen_clause_entry, widen_flow_writes,
    DefKind, TypeEnv, Typer, ARRAY_MUTATORS, HASH_MUTATORS,
};
use crate::dead::DeadOp;
use crate::flow_writes::{
    collect_indexed_flow, drop_indexed_narrowings, indexed_narrowing_key, owns_deferred_body,
    is_content_mutator, IndexedFlow, MUTATOR_METHODS, STRING_MUTATORS,
};

/// What a `while`/`until` loop's tracked locals converge to —
/// [`Typer::loop_continuation`]'s answer, applied by the caller over its
/// post-join / post-widen env.
enum LoopContinuation {
    /// The bounded replay's per-name bindings (rigor-rs#638): insert each.
    Fixpoint(HashMap<String, TypeId>),
    /// The rigor-rs#435 structural verdict — suspects floor to
    /// `Dynamic[top]`, every other tracked name sheds `Constant -> Nominal`
    /// — applied via [`Typer::apply_loop_cap_widen`].
    Structural(Vec<String>, std::collections::HashSet<String>),
}

impl<'i> Typer<'i> {
    /// Walk the top-level statement sequence in source order, binding each
    /// `LocalVariableWrite`'s name to the type of its value expression, and
    /// return the resulting [`TypeEnv`].
    ///
    /// This is the minimal flow needed so a later `s.lenght` can see `s :
    /// Constant["Hello"]`. Nested scopes / reassignment narrowing are out of
    /// scope for the tracer bullet.
    // TODO(spec): real flow-sensitive scoping + narrowing across branches (ADR-0022).
    pub fn build_toplevel_env(&self, ast: &LoweredAst, interner: &mut Interner) -> TypeEnv {
        let mut env = TypeEnv::new();
        let body = match ast.get(ast.root()) {
            Node::Program { body, .. } => body.clone(),
            _ => return env,
        };
        for stmt in body {
            // A program body may wrap statements directly or via a Statements node.
            self.bind_statement(ast, stmt, &mut env, interner);
        }
        env
    }

    /// A def-local env for `sig-gen` return typing (rigor-rs#546): the
    /// straight-line binder replayed over `stmts` on top of `env` — the
    /// method-body analogue of [`Self::build_toplevel_env`], so
    /// `def z; a = 1; a; end` reads `a : Constant[1]`.
    pub fn bind_statements_into(
        &self,
        ast: &LoweredAst,
        env: &TypeEnv,
        stmts: &[NodeId],
        interner: &mut Interner,
    ) -> TypeEnv {
        let mut env = env.clone();
        let mut returns = Vec::new();
        self.replay_statements(ast, &mut env, stmts, interner, &mut returns);
        env
    }

    /// [`bind_statements_into`] plus the type of every `return`'s value at
    /// ITS position in the replay — `(value node, type)` pairs keyed by node
    /// id, so `a = :initial; if c; return a; end; a = 100; a` reads
    /// `(100 | :initial)` not the final-env `100` (rigor-rs#546 review).
    /// Returns under `def` / block bodies are never reached by the replay, so
    /// every recorded return is a real return-site of the body.
    pub fn bind_statements_into_with_returns(
        &self,
        ast: &LoweredAst,
        env: &TypeEnv,
        stmts: &[NodeId],
        interner: &mut Interner,
    ) -> (TypeEnv, Vec<(NodeId, TypeId)>) {
        let mut env = env.clone();
        let mut returns = Vec::new();
        self.replay_statements(ast, &mut env, stmts, interner, &mut returns);
        (env, returns)
    }

    /// A method body for return typing: the env the LAST statement runs in
    /// (the replay of every statement before it — the tail's own writes and
    /// decisions must not already be applied when its value is typed) plus the
    /// type of every `return`'s value at its position, tail construct
    /// included (rigor-rs#546 review 3).
    pub fn bind_body_for_tail(
        &self,
        ast: &LoweredAst,
        env: &TypeEnv,
        stmts: &[NodeId],
        interner: &mut Interner,
    ) -> (TypeEnv, Vec<(NodeId, TypeId)>) {
        let mut env = env.clone();
        let mut returns = Vec::new();
        let split = stmts.len().saturating_sub(1);
        self.replay_statements(ast, &mut env, &stmts[..split], interner, &mut returns);
        // A tail after a statement that always leaves is dead code.
        if stmts[..split].last().is_some_and(|&s| self.always_exits(ast, s)) {
            Self::widen_all(&mut env, interner);
        }
        let before_tail = env.clone();
        self.replay_statements(ast, &mut env, &stmts[split..], interner, &mut returns);
        (before_tail, returns)
    }

    /// The straight-line statement replay behind [`bind_statements_into`]:
    /// `bind_statement` per statement, plus shallow branch JOINS for the
    /// conditional bodies sig-gen return typing reads (`if c; a = 1; end; a`
    /// → `(1 | nil)`, `while c; a = "s"; end` after `a = 1` → `("s" | 1)`).
    /// An arm's binding joins with the name's ENTRY type — `nil` when the
    /// name was unbound at entry, since the arm may not run.
    ///
    /// Everything the replay does not model DECLINES: a local the reference
    /// would re-type there is bound to `untyped`, so a later read is
    /// `Dynamic` — a coverage loss against the reference's flow env, never a
    /// wrong type. The declines (rigor-rs#546 reviews):
    ///
    /// - a statement holding a DECISION (an `if` / `case` / loop predicate,
    ///   `&&` / `||` left, `&.` receiver) that reads a typed local: the
    ///   reference narrows and constant-folds on it ([`Self::replay_opaque`]);
    /// - a local written at a position the replay does not walk — a
    ///   predicate, an argument, an interpolation, a pattern, an `else`
    ///   clause, a `for` index, a `rescue => e` ([`Self::widen_shell`]);
    /// - a named-capture `=~` (unknown bound names);
    /// - a construct arm that may leave the join (`return`, `raise`, a
    ///   possibly-`bot` helper call) — the names it writes
    ///   ([`Self::arm_suspect`]); a loop with `break` / `next` / `redo`;
    ///   `begin` with `else` or `retry`; `case`/`in`;
    /// - a rescue clause / ensure body reads the names the protected body
    ///   writes as `untyped` (the reference's #1837 rule).
    fn replay_statements(
        &self,
        ast: &LoweredAst,
        env: &mut TypeEnv,
        stmts: &[NodeId],
        interner: &mut Interner,
        returns: &mut Vec<(NodeId, TypeId)>,
    ) {
        for (idx, &stmt) in stmts.iter().enumerate() {
            let marks = self.def_local_marks(ast);
            let span = ast.get(stmt).span();
            let guards = self.typed_decision_reads(&marks, env, span, interner);
            let wild = marks.wild_within(span);
            // An unseen call / write in the statement may rewrite an instance
            // variable before a later part of it reads it (`while foo; return
            // @n; end`): the statement runs against declined ivars.
            if marks.ivar_reset_within(span) {
                Self::widen_ivars(env, interner);
            }
            // Capture the pre-loop env: `replay_one` + `apply_def_local_marks`
            // join/widen the body's writes, and the ADR-56 continuation must
            // overlay the *entry* seed afterwards (rigor-rs#638).
            // [`Self::loop_continuation`] picks the verdict — the bounded
            // replay when it is value-exact, the structural criterion
            // otherwise (and always under `def_scope`, where sig-gen's
            // def-local replay keeps the pre-#638 shape).
            let loop_entry = match ast.get(stmt) {
                Node::Loop { predicate, body, kind, .. } => {
                    Some((*predicate, body.clone(), *kind, env.clone()))
                }
                _ => None,
            };
            // A `loop_continuation` pass must model `if` decisions itself:
            // `replay_opaque` widens every typed decision subject and every
            // written name to `Dynamic`, which the exactness gate then reads
            // as an untrusted verdict (rigor-rs#437). `replay_one`'s `if`
            // arm already joins undecided arms and — under `loop_pass` —
            // takes a decided Constant predicate's live arm only. A write
            // whose value IS an `if` (`s = (if d then 2 else w = "x"; 3
            // end)`) is replayed the same way — `type_of` elides a decided
            // `if`'s dead arm and `widen_shell` drops the dead arm's stale
            // writes. Other decision shapes (`x = t && v`, `x = t&.m`, a
            // `case` value) stay opaque — `type_of`'s narrowing for them
            // was never in the fixpoint's trust base — and ivar or
            // class/global decisions stay opaque regardless.
            let modelled = self.loop_pass.get()
                && (matches!(ast.get(stmt), Node::If { .. })
                    || match ast.get(stmt) {
                        Node::LocalVariableWrite { value, .. }
                        | Node::LocalVariableOpWrite { value, .. }
                        | Node::MultiWrite { value, .. } => {
                            matches!(ast.get(*value), Node::If { .. })
                        }
                        _ => false,
                    })
                && !marks.decision_reset_within(span)
                && marks
                    .decision_cg_reads_within(span, self.def_scope.get())
                    .is_empty();
            if (guards.is_empty() || modelled) && !wild {
                self.replay_one(ast, &marks, env, stmt, interner, returns);
            } else {
                self.replay_opaque(ast, &marks, env, stmt, &guards, wild, interner, returns);
            }
            // A mutation or closure write inside the statement leaves the
            // literal carrier a binding holds stale (`r = []; r << 1`,
            // `s = "a"; items.each { |i| s << i }`): the reference widens it,
            // the replay declines it (`def_local_marks`).
            self.apply_def_local_marks(ast, env, span, interner);
            if let Some((predicate, body, kind, entry)) = loop_entry {
                use crate::def_local_marks::ExitKind as K;
                let jumps =
                    marks.has_exit_within(span, &[K::Break, K::Next, K::Redo, K::Retry]);
                let once = matches!(
                    kind,
                    rigor_parse::LoopKind::While { begin_modifier: true }
                        | rigor_parse::LoopKind::Until { begin_modifier: true }
                );
                if !(jumps || once || matches!(kind, rigor_parse::LoopKind::For)) {
                    match self.loop_continuation(ast, &entry, predicate, &body, interner)
                    {
                        Some(LoopContinuation::Fixpoint(bindings)) => {
                            for (n, ty) in bindings {
                                env.insert(Rc::from(n.as_str()), ty);
                            }
                        }
                        Some(LoopContinuation::Structural(tracked, suspect)) => {
                            self.apply_loop_cap_widen(env, &tracked, &suspect, interner);
                        }
                        None => {}
                    }
                }
            }
            // Code after a statement that always leaves (`return`, `raise`,
            // `break`) is unreachable: the reference types it in a scope the
            // replay cannot reproduce, so it reads every local as unknown.
            if idx + 1 < stmts.len() && self.always_exits(ast, stmt) {
                Self::widen_all(env, interner);
            }
        }
    }

    /// Whether `stmt` always leaves its sequence: `return`, a jump, or a
    /// receiver-less `raise` / `exit` / `abort` / `throw` / `fail`.
    pub fn always_exits(&self, ast: &LoweredAst, stmt: NodeId) -> bool {
        match ast.get(stmt) {
            Node::Return { .. } | Node::Other { jump: Some(_), .. } => true,
            Node::Statements { kind: StatementsKind::Jump(_), .. } => true,
            Node::Call { receiver: None, method, .. } => {
                matches!(method.as_str(), "raise" | "fail" | "exit" | "abort" | "throw")
            }
            _ => false,
        }
    }

    /// A statement the replay cannot model — a decision on a typed local, or
    /// an unknown-name binding. It is replayed on a CLONE with the decision's
    /// locals widened (so a `return` inside records its in-position type),
    /// then every local it writes — and the decision's locals — are declined
    /// in the real env.
    #[allow(clippy::too_many_arguments)]
    fn replay_opaque(
        &self,
        ast: &LoweredAst,
        marks: &crate::def_local_marks::DefLocalMarks,
        env: &mut TypeEnv,
        stmt: NodeId,
        guards: &[String],
        wild: bool,
        interner: &mut Interner,
        returns: &mut Vec<(NodeId, TypeId)>,
    ) {
        let span = ast.get(stmt).span();
        let before = returns.len();
        let mut clone = env.clone();
        if wild {
            Self::widen_all(&mut clone, interner);
        } else {
            Self::widen_names(&mut clone, guards, interner);
        }
        self.replay_one(ast, marks, &mut clone, stmt, interner, returns);
        // A `return` whose value holds the decision itself read it typed.
        let untyped = interner.untyped();
        for (value, ty) in returns[before..].iter_mut() {
            let vs = ast.get(*value).span();
            if wild || marks.wild_within(vs) || marks.reads_any_within_decisions(vs, guards) {
                *ty = untyped;
            }
        }
        if wild {
            Self::widen_all(env, interner);
        }
        Self::widen_names(env, guards, interner);
        Self::widen_names(env, &marks.written_within(span), interner);
    }

    /// One statement of the replay (see [`Self::replay_statements`]).
    fn replay_one(
        &self,
        ast: &LoweredAst,
        marks: &crate::def_local_marks::DefLocalMarks,
        env: &mut TypeEnv,
        stmt: NodeId,
        interner: &mut Interner,
        returns: &mut Vec<(NodeId, TypeId)>,
    ) {
        // `return e` mid-body: its value types at the position it runs,
        // not against the body's final env.
        if let Node::Return { values, .. } = ast.get(stmt) {
            if let [v] = values.as_slice() {
                let ty = self.type_of(ast, *v, env, interner);
                returns.push((*v, ty));
            }
        }
        let span = ast.get(stmt).span();
        // A write at a position this replay does not walk is stale.
        self.widen_shell(ast, marks, env, stmt, interner);
        match ast.get(stmt) {
            Node::If { predicate, then_body, else_body, is_unless, .. } => {
                // A literal-only predicate is constant-folded by the
                // reference: the dead arm never runs. A decided one replays
                // the live arm; an undecided one (`if true && false`) is not
                // modelled and declines.
                let pred_ty = self.type_of(ast, *predicate, env, interner);
                let folded = self.predicate_polarity(interner, pred_ty);
                let literal = crate::def_local_marks::literal_like(ast, *predicate);
                if literal && folded.is_none() {
                    self.replay_discard(ast, env, &[then_body, else_body], interner, returns);
                    Self::widen_names(env, &marks.written_within(span), interner);
                } else if literal
                    || (self.loop_pass.get()
                        && folded.is_some()
                        && matches!(interner.get(pred_ty), Type::Constant(_)))
                {
                    // Under `loop_pass` a decided non-literal predicate
                    // takes only its live arm too — the reference's per-pass
                    // `live_branch_for_if` kills the dead edge, so a write
                    // there (`if d … else t = nil end` on a pinned `d`)
                    // never reaches the pass env (rigor-rs#437). The elide
                    // is restricted to value-pinned (`Constant`) predicates:
                    // a `Nominal`/`Tuple` answer could be an optimistic
                    // carrier the reference declines to elide (its
                    // `OptimisticOrigin` channel has no port yet), where
                    // skipping the arm sheds a pin the reference keeps.
                    let (live, dead) =
                        if folded == Some(!*is_unless) { (then_body, else_body) } else { (else_body, then_body) };
                    if crate::def_local_marks::body_span(ast, dead)
                        .is_some_and(|sp| marks.has_exit_within(sp, &[crate::def_local_marks::ExitKind::Return]))
                    {
                        // A `return` in the dead arm: whether the reference
                        // still collects its value is not modelled.
                        self.replay_discard(ast, env, &[then_body, else_body], interner, returns);
                        Self::widen_names(env, &marks.written_within(span), interner);
                    } else {
                        self.replay_statements(ast, env, live, interner, returns);
                    }
                } else {
                    // An empty `else` IS an arm that leaves every name at its
                    // entry binding — `if c; a = 1; end` joins `a` with `nil`.
                    // The predicate also narrows its subject locals per
                    // edge, and the join keeps the narrowed arm types
                    // (`if h.empty?; end; h.frob` — `h` is `Dynamic` after,
                    // not `Array`, rigor-rs#451): a decided predicate —
                    // `[1].empty?` — is the reference's folded `if`, which
                    // narrows nothing, so its arm keeps the pin (`h.frob`
                    // fires `for [1]`).
                    let binds: [Vec<(String, TypeId)>; 2] = if folded.is_some() {
                        [Vec::new(), Vec::new()]
                    } else {
                        [
                            self.predicate_edge_binds(ast, *predicate, env, !*is_unless, interner),
                            self.predicate_edge_binds(ast, *predicate, env, *is_unless, interner),
                        ]
                    };
                    self.join_arm_writes_bound(
                        ast,
                        env,
                        &[(then_body, &[]), (else_body, &[])],
                        &binds,
                        interner,
                        returns,
                    );
                }
            }
            Node::Logical { left, right, .. } if crate::def_local_marks::literal_like(ast, *left) => {
                // `true && (a = 2)`: a literal left decides whether the right
                // runs; the reference folds it. Not modelled.
                self.replay_discard(ast, env, &[std::slice::from_ref(right)], interner, returns);
                Self::widen_names(env, &marks.written_within(span), interner);
            }
            Node::Logical { right, .. } => {
                // `c && a = 1` / `c || a = 1`: the right side runs only
                // when the left's truthiness reaches it — a one-arm join.
                self.join_arm_writes(
                    ast,
                    env,
                    &[(std::slice::from_ref(right), &[]), (&[], &[])],
                    interner,
                    returns,
                );
            }
            // `begin`+`rescue`: the protected body and each clause are
            // alternative paths — `begin; a = 1; rescue; a = 2; end; a`
            // joins `(1 | 2)`, and a name only the protected body binds
            // widens to `nil`. A clause enters from ANY point of the body, so
            // it reads the names the body writes as `untyped`; so does the
            // `ensure`, which also runs on every path.
            Node::BeginRescue { main_body, body, clauses, ensure_body, has_else, .. } => {
                let retry = marks.has_exit_within(span, &[crate::def_local_marks::ExitKind::Retry]);
                if *has_else || retry {
                    // `else` replaces the body's value / `retry` re-enters it:
                    // not modelled — decline everything the begin writes.
                    let mut arms: Vec<&[NodeId]> = vec![body];
                    arms.extend(clauses.iter().map(|c| c.body.as_slice()));
                    self.replay_discard(ast, env, &arms, interner, returns);
                    Self::widen_names(env, &marks.written_within(span), interner);
                } else if clauses.is_empty() {
                    if ensure_body.is_empty() {
                        self.replay_statements(ast, env, body, interner, returns);
                    } else {
                        self.replay_statements(ast, env, main_body, interner, returns);
                        Self::widen_names(env, &marks.changed_within(span), interner);
                        self.replay_statements(ast, env, ensure_body, interner, returns);
                    }
                } else {
                    let stale = crate::def_local_marks::body_span(ast, main_body)
                        .map(|sp| marks.changed_within(sp))
                        .unwrap_or_default();
                    let mut arms: Vec<(&[NodeId], &[String])> = vec![(main_body.as_slice(), &[])];
                    arms.extend(clauses.iter().map(|c| (c.body.as_slice(), stale.as_slice())));
                    // `rescue [T, …] => e` binds `e` inside its clause only
                    // (rigor-rs#548); past the construct `e` joins with the
                    // entry's decline and stays `untyped`.
                    let mut binds: Vec<Vec<(String, TypeId)>> = vec![Vec::new()];
                    binds.extend(clauses.iter().map(|c| self.rescue_binding(ast, c, interner)));
                    self.join_arm_writes_bound(ast, env, &arms, &binds, interner, returns);
                    if !ensure_body.is_empty() {
                        Self::widen_names(env, &marks.changed_within(span), interner);
                        self.replay_statements(ast, env, ensure_body, interner, returns);
                    }
                }
            }
            Node::Case { branches, else_body, .. } => {
                // Each `When` node's body is one arm; a missing `else`
                // reads as a no-op arm via `entry` inside the join.
                // `case`/`in` raises on no match and joins only the actual
                // arms — not modelled: decline what it writes.
                if branches.iter().all(|&b| matches!(ast.get(b), Node::When { .. })) {
                    let mut arms: Vec<(&[NodeId], &[String])> = branches
                        .iter()
                        .map(|&b| match ast.get(b) {
                            Node::When { body, .. } => (body.as_slice(), &[][..]),
                            _ => (&[][..], &[][..]),
                        })
                        .collect();
                    arms.push((else_body.as_slice(), &[]));
                    self.join_arm_writes(ast, env, &arms, interner, returns);
                } else {
                    let mut arms: Vec<&[NodeId]> = branches
                        .iter()
                        .map(|&b| match ast.get(b) {
                            Node::When { body, .. } => body.as_slice(),
                            Node::BeginRescue { body, .. } => body.get(1..).unwrap_or(&[]),
                            _ => &[][..],
                        })
                        .collect();
                    arms.push(else_body.as_slice());
                    self.replay_discard(ast, env, &arms, interner, returns);
                    Self::widen_names(env, &marks.written_within(span), interner);
                }
            }
            Node::Loop { body, kind, .. } => {
                use crate::def_local_marks::ExitKind as K;
                let (body, kind) = (body.clone(), *kind);
                let before = returns.len();
                let changed = marks.changed_within(span);
                // A `begin … end while` body runs at least once; `for` types
                // as its collection; `break` / `next` / `redo` leave the loop
                // from the middle of the body — none is modelled.
                let jumps = marks.has_exit_within(span, &[K::Break, K::Next, K::Redo, K::Retry]);
                let once = matches!(
                    kind,
                    rigor_parse::LoopKind::While { begin_modifier: true }
                        | rigor_parse::LoopKind::Until { begin_modifier: true }
                );
                if jumps || once || matches!(kind, rigor_parse::LoopKind::For) {
                    self.replay_discard(ast, env, &[&body], interner, returns);
                    Self::widen_names(env, &marks.written_within(span), interner);
                } else {
                    // The body runs zero or more times: the empty arm below
                    // is the zero-iteration path (entry env). A name the loop
                    // both reads and writes is carried round to the next
                    // iteration (`a = [a]`): the body must not read the entry
                    // binding of it.
                    let written = marks.written_within(span);
                    let carried: Vec<String> = marks
                        .reads_within(span)
                        .into_iter()
                        .filter(|r| written.iter().any(|w| w == r))
                        .map(str::to_string)
                        .collect();
                    // ADR-56 slice B: the single-pass join (nil-injection +
                    // carried names). The bounded rebind fixpoint overlays
                    // the continuation after `apply_def_local_marks` in
                    // [`Self::replay_statements`] so a loop-body rebind does
                    // not wipe the converged pins (rigor-rs#638).
                    self.join_arm_writes(ast, env, &[(&body, &carried), (&[], &[])], interner, returns);
                }
                // A `return` recorded inside the body typed its value at
                // its source position, but the loop carries later writes
                // (and mutations) of the names it reads round to the next
                // iteration (`while c; return a if x; a = "s"; end`).
                if !changed.is_empty() {
                    let untyped = interner.untyped();
                    for (value, ty) in returns[before..].iter_mut() {
                        if marks.reads_any_within(ast.get(*value).span(), &changed) {
                            *ty = untyped;
                        }
                    }
                }
            }
            Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
                self.replay_statements(ast, env, body, interner, returns);
            }
            // A carrier with no sequence order (`rescue` modifier,
            // `super(…)`, `yield(…)`, `defined?`, `BEGIN` / `END`): its
            // writes were declined by `widen_shell`; nothing binds.
            Node::Statements { .. } => {}
            // `@x = v` binds the sigilled name — `def m; @i = 1; @i; end` reads
            // `1` (rigor-rs#548). A class / global variable write is not
            // tracked: the name stays unbound, so a read declines.
            Node::InstanceVariableWrite { name, value, .. } => {
                if ast.in_blocked_carrier(ast.get(stmt).span()) {
                    return;
                }
                let (name, value) = (name.clone(), *value);
                let ty = self.type_of(ast, value, env, interner);
                env.insert(Rc::from(name.as_str()), ty);
            }
            // `a op= v` rebinds `a` to the fold of its entry binding and `v`:
            // `a = 1; a += 2` ⇒ `3`, `a = nil; a ||= 5` ⇒ `5`. Anything the
            // fold cannot decide exactly (an unbound / `untyped` entry, a
            // non-constant operand, a fold the port lacks) declines the local
            // to `untyped`. Inside a loop / block body the entry binding is
            // already `untyped` when the name is read-and-written there
            // (`Self::replay_one`'s `Loop` and block arms carry it round).
            Node::LocalVariableOpWrite { name, value, op, .. } => {
                if ast.in_blocked_carrier(ast.get(stmt).span()) || ast.closure_bound_names(stmt).contains(name) {
                    return;
                }
                let (name, value, op) = (name.clone(), *value, op.clone());
                let v_ty = self.type_of(ast, value, env, interner);
                let new_ty = match env.get(name.as_str()).copied() {
                    Some(entry) => self.op_write_result(entry, &op, v_ty, interner),
                    None => interner.untyped(),
                };
                drop_indexed_narrowings(env, &name);
                env.insert(Rc::from(name.as_str()), new_ty);
            }
            Node::Call { block_body, block_params, block_locals, block_span, .. } if !block_body.is_empty() => {
                // A block may run and rebind a captured local — its
                // writes are not modelled (block params are unbindable
                // here), so every name the block could rebind widens to
                // `untyped`. Without this `a = :s; loop do; a = 1; break;
                // end; a` kept `:s` (review).
                self.bind_statement(ast, stmt, env, interner);
                let mut block_env = env.clone();
                // A block parameter / block-local SHADOWS the outer local of
                // the same name: the body reads the argument, not the binding
                // (`i = 1; c.each { |i| return i }` returns the argument).
                let shadowed: Vec<String> = block_params
                    .iter()
                    .map(|(n, _)| n.clone())
                    .chain(block_locals.iter().cloned())
                    .collect();
                Self::widen_names(&mut block_env, &shadowed, interner);
                // A block runs zero or more times: a name it both reads and
                // writes (`s += 1`, `a = [a]`) is carried round to the next
                // run, so the body must not read the entry binding of it.
                let bspan = block_span.unwrap_or(span);
                let written = marks.written_within(bspan);
                let carried: Vec<String> = marks
                    .reads_within(bspan)
                    .into_iter()
                    .filter(|r| written.iter().any(|w| w == r))
                    .map(str::to_string)
                    .collect();
                Self::widen_names(&mut block_env, &carried, interner);
                self.replay_statements(ast, &mut block_env, block_body, interner, returns);
                let untyped = interner.untyped();
                for (name, ty) in block_env.iter() {
                    if shadowed.iter().any(|b| b.as_str() == &**name) {
                        continue;
                    }
                    if env.get(name) != Some(ty) {
                        env.insert(name.clone(), untyped);
                    }
                }
            }
            _ => self.bind_statement(ast, stmt, env, interner),
        }
    }

    /// The binding a `rescue` clause gives its `=> e` reference, as
    /// `(name, type)` pairs (empty when the clause binds nothing or the type
    /// is not known exactly): a bare clause binds `StandardError`, a listed
    /// one the union of the listed classes.
    fn rescue_binding(
        &self,
        ast: &LoweredAst,
        clause: &rigor_parse::RescueClause,
        interner: &mut Interner,
    ) -> Vec<(String, TypeId)> {
        let Some(name) = &clause.bound_name else { return Vec::new() };
        let ty = if clause.exceptions.is_empty() {
            self.nominal_or_untyped("StandardError", interner)
        } else {
            self.exception_union(ast, &clause.exceptions, interner)
        };
        if matches!(interner.get(ty), Type::Dynamic(_)) {
            return Vec::new();
        }
        vec![(name.clone(), ty)]
    }

    /// The union of the (non-generic) core exception classes a `rescue T1,
    /// T2` list names. A name the core index does not register, a project
    /// definition that may shadow it, a path (`Errno::ENOENT`), a splat or
    /// any other expression declines the WHOLE binding to `untyped`.
    fn exception_union(
        &self,
        ast: &LoweredAst,
        exceptions: &[NodeId],
        interner: &mut Interner,
    ) -> TypeId {
        let mut members: Vec<TypeId> = Vec::new();
        for &ex in exceptions {
            // A `dynamic_base` path (`k::ArgumentError`) is opaque: the
            // reference types the node through the expression typer
            // (`ErrorInfo.rescue_exception_type`), which answers a
            // non-Singleton for it, so the member is `Dynamic[top]` — the
            // lenient trailing name must NOT bind here (rigor-rs#673).
            let Node::ConstantRead {
                name,
                span,
                dynamic_base: false,
                ..
            } = ast.get(ex)
            else {
                return interner.untyped();
            };
            let Some(class) = self.index.class_id(name) else { return interner.untyped() };
            if self.source.knows_class(name)
                || self.source.constant_shadowed(name, self.enclosing_prefix(*span))
            {
                return interner.untyped();
            }
            let n = interner.intern(Type::Nominal { class, args: Vec::new() });
            if !members.contains(&n) {
                members.push(n);
            }
        }
        match members.len() {
            0 => interner.untyped(),
            1 => members[0],
            _ => members
                .iter()
                .skip(1)
                .fold(members[0], |acc, &m| rigor_types::Algebra::join(interner, acc, m)),
        }
    }

    /// The new binding of `a` after `a <op>= v`, from `a`'s entry binding and
    /// `v`'s type (`untyped` declines).
    fn op_write_result(
        &self,
        entry: TypeId,
        op: &rigor_parse::OpWriteKind,
        v_ty: TypeId,
        interner: &mut Interner,
    ) -> TypeId {
        use rigor_parse::OpWriteKind;
        let untyped = interner.untyped();
        if matches!(interner.get(entry), Type::Dynamic(_)) || matches!(interner.get(v_ty), Type::Dynamic(_)) {
            return untyped;
        }
        match op {
            // `a ||= v` ⇒ `a = a || v`: the truthy part of the entry plus
            // `v` — `a = nil; a ||= 5` ⇒ `5`, `a = 1; a ||= "x"` ⇒
            // `("x" | 1)`.
            OpWriteKind::Or => match self.split_truthiness(entry, interner) {
                Some((Some(truthy), _)) => rigor_types::Algebra::join(interner, truthy, v_ty),
                Some((None, _)) => v_ty,
                None => untyped,
            },
            // `a &&= v` ⇒ `a = a && v`: the falsy part of the entry plus `v`.
            OpWriteKind::And => match self.split_truthiness(entry, interner) {
                Some((_, Some(falsy))) => rigor_types::Algebra::join(interner, falsy, v_ty),
                Some((_, None)) => v_ty,
                None => untyped,
            },
            // `a <binop>= v` ⇒ `a = a.<binop>(v)`: only a literal-by-literal
            // fold is exact; every other dispatch declines.
            OpWriteKind::Operator(opname) => {
                // Memberwise over a union of constants so a loop fixpoint can
                // see `x |= 1` stabilize at `0 | 1` instead of declining the
                // second pass to `Dynamic` (rigor-rs#638) — the port's
                // answer to the reference's `ConstantFolding
                // .try_fold_binary_set`, including its caps
                // (`UNION_FOLD_INPUT_LIMIT` = 32 input members × arg sets,
                // `UNION_FOLD_OUTPUT_LIMIT` = 8 deduped results): past them
                // the reference widens or declines, and so does this. A
                // non-constant member declines the whole write.
                const UNION_FOLD_INPUT_LIMIT: usize = 32;
                const UNION_FOLD_OUTPUT_LIMIT: usize = 8;
                let v_scalar = match interner.get(v_ty) {
                    Type::Constant(a) => a.clone(),
                    _ => return untyped,
                };
                let members: Vec<TypeId> = match interner.get(entry) {
                    Type::Union(ms) => ms.clone(),
                    _ => vec![entry],
                };
                if members.len() > UNION_FOLD_INPUT_LIMIT {
                    return untyped;
                }
                let mut folded: Vec<Scalar> = Vec::new();
                let mut acc: Option<TypeId> = None;
                for m in members {
                    let r = match interner.get(m) {
                        Type::Constant(r) => r.clone(),
                        _ => return untyped,
                    };
                    let Some(sc) = crate::folding::fold(&r, opname, std::slice::from_ref(&v_scalar))
                    else {
                        return untyped;
                    };
                    if !folded.contains(&sc) {
                        if folded.len() >= UNION_FOLD_OUTPUT_LIMIT {
                            return untyped;
                        }
                        folded.push(sc.clone());
                    }
                    let f = interner.intern(Type::Constant(sc));
                    acc = Some(match acc {
                        None => f,
                        Some(a) => rigor_types::Algebra::join(interner, a, f),
                    });
                }
                acc.unwrap_or(untyped)
            }
        }
    }

    /// Split `ty` into its `(truthy, falsy)` parts, each the join of the
    /// members on that side (`None` when empty). `None` when any member's
    /// truthiness is not statically known.
    fn split_truthiness(
        &self,
        ty: TypeId,
        interner: &mut Interner,
    ) -> Option<(Option<TypeId>, Option<TypeId>)> {
        let members: Vec<TypeId> = match interner.get(ty) {
            Type::Union(ms) => ms.clone(),
            _ => vec![ty],
        };
        let (mut truthy, mut falsy): (Vec<TypeId>, Vec<TypeId>) = (Vec::new(), Vec::new());
        for m in members {
            let falsy_member = match interner.get(m) {
                Type::Constant(Scalar::Nil) | Type::Constant(Scalar::Bool(false)) => true,
                Type::Constant(_)
                | Type::Tuple(_)
                | Type::HashShape(_)
                | Type::IntegerRange { .. } => false,
                Type::Nominal { class, .. } => match self.index.class_name_for_id(*class)? {
                    "NilClass" | "FalseClass" => true,
                    "String" | "Integer" | "Float" | "Symbol" | "Array" | "Hash" | "Range" | "Regexp"
                    | "Rational" | "Complex" | "Proc" | "TrueClass" => false,
                    _ => return None,
                },
                _ => return None,
            };
            if falsy_member { falsy.push(m) } else { truthy.push(m) }
        }
        let join = |v: Vec<TypeId>, interner: &mut Interner| -> Option<TypeId> {
            let first = *v.first()?;
            Some(v.iter().skip(1).fold(first, |acc, &m| rigor_types::Algebra::join(interner, acc, m)))
        };
        Some((join(truthy, interner), join(falsy, interner)))
    }

    /// Replay each arm on its own clone of `env` and discard the result —
    /// only to record the `return`s inside (typed at their positions).
    fn replay_discard(
        &self,
        ast: &LoweredAst,
        env: &TypeEnv,
        arms: &[&[NodeId]],
        interner: &mut Interner,
        returns: &mut Vec<(NodeId, TypeId)>,
    ) {
        for arm in arms {
            let mut e = env.clone();
            self.replay_statements(ast, &mut e, arm, interner, returns);
        }
    }

    /// Join the locals rebound inside each `arm` body into `env`: every name an
    /// arm binds gets the union of the type each arm leaves it at — the entry
    /// binding on an arm that does not rebind it, `nil` when it was unbound
    /// both there and at entry (the arm may never have run). Each arm carries
    /// the names its ENTRY reads as `untyped`. An arm that may leave the
    /// construct (`Self::arm_suspect`) has the names it writes declined: the
    /// reference drops a terminating arm from the join.
    fn join_arm_writes(
        &self,
        ast: &LoweredAst,
        env: &mut TypeEnv,
        arms: &[(&[NodeId], &[String])],
        interner: &mut Interner,
        returns: &mut Vec<(NodeId, TypeId)>,
    ) {
        self.join_arm_writes_bound(ast, env, arms, &[], interner, returns);
    }

    /// The `(name, type)` bindings a predicate's `edge` gives its subject
    /// locals — the same edge map `while`/`until` uses
    /// ([`Typer::subject_edge`]): a subject the edge keeps is absent; every
    /// other answer (a widened guard member, a dead edge's `Bot` — the port
    /// has no `non-empty-array` shape to mint, rigor-rs#451) binds
    /// `Dynamic`, which is never an FP: the oracle's joined arm types never
    /// leave the pre-`if` nominal either (`if h.empty?; end; h.frob` is
    /// reference-silent, and `if h.is_a?(Array); else; h.frob`'s
    /// `non-empty-array[top]` is a shape this port declines instead).
    fn predicate_edge_binds(
        &self,
        ast: &LoweredAst,
        pred: NodeId,
        env: &TypeEnv,
        edge: bool,
        interner: &mut Interner,
    ) -> Vec<(String, TypeId)> {
        let mut out = Vec::new();
        for (subject, _read) in self.subjects_within(ast, pred) {
            let crate::loop_edges::Subject::Local(name) = &subject else { continue };
            if out.iter().any(|(n, _)| n == name) {
                continue;
            }
            let ty = env
                .get(name.as_str())
                .copied()
                .unwrap_or_else(|| interner.untyped());
            // An untyped carrier is the guard map's domain — `subject_edge`
            // only speaks for a resolved one (rigor-rs#451).
            if matches!(interner.get(ty), Type::Dynamic(_) | Type::Top) {
                continue;
            }
            let e = self.subject_edge(
                ast,
                pred,
                &subject,
                ty,
                edge,
                false,
                interner,
            );
            if e != crate::loop_edges::Edge::Keep {
                out.push((name.clone(), interner.untyped()));
            }
        }
        out
    }

    /// [`Self::join_arm_writes`] where arm `i` additionally starts with the
    /// `(name, type)` bindings in `binds[i]` (a `rescue => e` clause binds
    /// `e`; a missing entry binds nothing).
    fn join_arm_writes_bound(
        &self,
        ast: &LoweredAst,
        env: &mut TypeEnv,
        arms: &[(&[NodeId], &[String])],
        binds: &[Vec<(String, TypeId)>],
        interner: &mut Interner,
        returns: &mut Vec<(NodeId, TypeId)>,
    ) {
        let entry = env.clone();
        let nil = interner.nil();
        let marks = self.def_local_marks(ast);
        let mut bound: Vec<String> = Vec::new();
        let mut arm_envs: Vec<TypeEnv> = Vec::new();
        let mut written: Vec<String> = Vec::new();
        let mut any_suspect = false;
        for (i, (arm, entry_stale)) in arms.iter().enumerate() {
            let mut e = entry.clone();
            Self::widen_names(&mut e, entry_stale, interner);
            for (name, ty) in binds.get(i).into_iter().flatten() {
                e.insert(Rc::from(name.as_str()), *ty);
            }
            self.replay_statements(ast, &mut e, arm, interner, returns);
            arm_envs.push(e);
            if let Some(sp) = crate::def_local_marks::body_span(ast, arm) {
                written.extend(marks.written_within(sp));
                any_suspect |= self.arm_suspect(ast, arm);
            }
        }
        // The reference drops a terminating arm from the join, so a sibling
        // arm's write is the only one left (`if c; a = 2; else; raise; end`
        // reads `2`): when ANY arm may leave, every name the arms write is
        // declined.
        let suspect = if any_suspect { written } else { Vec::new() };
        // An EMPTY arm contributes the entry env unchanged (the `else_body`
        // of an else-less `if` is `[]`, and the loop body's zero-iteration
        // path is `entry` itself — appended by callers below).
        for ae in &arm_envs {
            for (name, _) in ae.iter() {
                if !bound.iter().any(|b| b.as_str() == &**name) {
                    bound.push(name.to_string());
                }
            }
        }
        for name in bound {
            // Only names an arm actually CHANGED need the join; a name no arm
            // rebinds keeps its entry binding.
            if arm_envs.iter().all(|ae| ae.get(name.as_str()) == entry.get(name.as_str())) {
                continue;
            }
            // An ivar unbound at entry reads its class seed there, not
            // `nil`: the join declines it.
            let absent = if crate::def_local_marks::is_ivar(&name) { interner.untyped() } else { nil };
            let mut members: Vec<TypeId> = Vec::new();
            for ae in &arm_envs {
                let ty = ae.get(name.as_str()).or_else(|| entry.get(name.as_str())).copied().unwrap_or(absent);
                if !members.contains(&ty) {
                    members.push(ty);
                }
            }
            let joined = members.iter().skip(1).fold(members[0], |acc, &m| {
                rigor_types::Algebra::join(interner, acc, m)
            });
            env.insert(Rc::from(name.as_str()), joined);
        }
        Self::widen_names(env, &suspect, interner);
    }

    /// `Type::Combinator.widen_value_pinned` — the widener a capped fixpoint's
    /// last pass applies: `Constant[1]` -> `Integer`, `IntegerRange` ->
    /// `Integer`, a `Refined` to its (recursively widened) base, a union
    /// memberwise. `Constant[nil]` keeps its pin, exactly as the reference's
    /// `type.value.nil? ? type : …` arm does.
    fn widen_value_pinned(&self, ty: TypeId, interner: &mut Interner) -> TypeId {
        match interner.get(ty) {
            Type::Constant(Scalar::Nil) => ty,
            Type::Constant(s) => {
                let class = crate::folding::scalar_class(s);
                self.nominal_or_untyped(class, interner)
            }
            Type::IntegerRange { .. } => self.nominal_or_untyped("Integer", interner),
            Type::Refined { base, .. } => {
                let base = *base;
                self.widen_value_pinned(base, interner)
            }
            Type::Union(members) => {
                let members = members.clone();
                let mut acc: Option<TypeId> = None;
                for &m in &members {
                    let w = self.widen_value_pinned(m, interner);
                    acc = Some(match acc {
                        None => w,
                        Some(a) => rigor_types::Algebra::join(interner, a, w),
                    });
                }
                acc.unwrap_or(ty)
            }
            _ => ty,
        }
    }

    /// ADR-56 slice B — what a `while`/`until` loop's tracked locals
    /// converge to (the reference's `loop_rebind_fixpoint`,
    /// `BodyFixpoint.converge`, CAP = 3). Two verdicts:
    ///
    /// - **Fixpoint (rigor-rs#638).** Bounded passes replay the body via
    ///   [`Self::replay_statements`] — the port's `single_pass` +
    ///   `join_with_nil_injection` — on `env` overlaid with the running
    ///   assumption (pre-existing names only; a body-first local's `nil`
    ///   seed is a 0-iteration join constituent, never a leak — the
    ///   reference's `except(*body_first)`). Per-name stability is `TypeId`
    ///   equality after `Algebra::join`; on the last permitted pass both
    ///   sides are [`Self::widen_value_pinned`] before the join and a
    ///   still-moving name floors to `Dynamic[top]`. Stabilizing self-ops
    ///   (`x |= 1`, `i += 0`) converge in pass 1–2 and never hit the widen,
    ///   so a sibling dead-arm pin (`t = 1` with `if false; t = nil`) stays
    ///   `Constant` — the structural criterion below treated every op-write
    ///   as compounding and over-capped those rows (rigor-rs#638).
    ///   A still-growing shape (`y = [y]`) reaches CAP and sheds every
    ///   tracked pin, exactly as `BodyFixpoint`'s cap pass does.
    ///
    ///   The replay verdict is trusted only when it is **value-exact end to
    ///   end**: every tracked seed and every tracked pass exit free of
    ///   `Dynamic` / `Top` / `Bottom` / `Void` ([`Self::type_has_decline`]),
    ///   and the predicate reading or writing no tracked name (the pass
    ///   does not model `loop_pass_entry`'s edge narrowing). A pass that
    ///   returns `Dynamic` for a tracked name DECLINED — `type_of` missed,
    ///   `replay_opaque` widened a typed-local decision, an op-write union
    ///   hit a non-constant member — and `join(Dynamic, Dynamic) ==
    ///   Dynamic` then reports the name "stable": trusting it reads a loop
    ///   the reference still caps as converged and leaks a sibling pin
    ///   (`x = f(x)`, `i += 1 if i < 3`, `x = (x || 0) + 1` — the FP
    ///   family #638's own remediation anticipated).
    ///
    /// - **Structural (rigor-rs#435).** When the replay is not exact —
    ///   and always under `def_scope`, where sig-gen's def-local replay
    ///   keeps the pre-#638 shape — [`Self::loop_cap_widening`]'s static
    ///   moved/suspect analysis answers instead.
    ///
    /// Returns `None` when the body writes nothing tracked.
    fn loop_continuation(
        &self,
        ast: &LoweredAst,
        env: &TypeEnv,
        predicate: Option<NodeId>,
        body: &[NodeId],
        interner: &mut Interner,
    ) -> Option<LoopContinuation> {
        let marks = self.def_local_marks(ast);
        let body_span = crate::def_local_marks::body_span(ast, body)?;
        let mut names: Vec<String> = Vec::new();
        for name in marks.written_within(body_span) {
            if crate::def_local_marks::is_ivar(&name) {
                if env.contains_key(name.as_str()) {
                    names.push(name);
                }
            } else {
                names.push(name);
            }
        }
        if names.is_empty() {
            return None;
        }
        // Save/restore for a NESTED call: a `loop_continuation` inside this
        // body's replay must hand the flag back to the outer pass, not reset
        // it (design-review N5).
        let prev_loop_pass = self.loop_pass.replace(true);
        macro_rules! structural {
            () => {{
                self.loop_pass.set(prev_loop_pass);
                return self
                    .loop_cap_widening(ast, env, body, interner)
                    .map(|(tracked, suspect)| LoopContinuation::Structural(tracked, suspect));
            }};
        }
        // sig-gen's def-local replay keeps the structural verdict — its
        // body replay must keep declining the loops the reference's
        // def-local pass declines, and the fixpoint would fold them.
        if self.def_scope.get().is_some() {
            structural!();
        }
        // The pass does not model `loop_pass_entry` — the per-pass
        // predicate edge narrowing — so a predicate that reads or writes
        // a tracked name leaves the verdict unobservable to the replay.
        if let Some(p) = predicate {
            let pspan = ast.get(p).span();
            let reads = marks.reads_within(pspan);
            let writes = marks.written_within(pspan);
            if names
                .iter()
                .any(|n| reads.contains(&n.as_str()) || writes.iter().any(|w| w == n))
            {
                structural!();
            }
        }
        // Every tracked seed must be value-exact: a `Dynamic` seed means
        // the pass never observed the name — and where the reference holds
        // a precise seed that MOVES (`nil` → `nil | 2`), an absorbing
        // `Dynamic` would converge a pass earlier and keep pins the
        // reference's cap widen sheds (design-review of #437).
        if names.iter().any(|n| {
            env.get(n.as_str())
                .map(|&t| Self::type_has_decline(interner, t))
                .unwrap_or(false)
        }) {
            structural!();
        }
        let nil = interner.nil();
        // Pre-existing names only: a body-first local's `nil` seed must not
        // leak into the body (reference `loop_pass_entry` /
        // `except(*body_first)`).
        let overlay: Vec<String> = names
            .iter()
            .filter(|n| env.contains_key(n.as_str()))
            .cloned()
            .collect();
        let mut assumption: HashMap<String, TypeId> = names
            .iter()
            .map(|n| (n.clone(), env.get(n.as_str()).copied().unwrap_or(nil)))
            .collect();
        const CAP: usize = 3;
        self.loop_pass.set(true);
        for iteration in 0..CAP {
            let last = iteration + 1 == CAP;
            let mut pass_env = env.clone();
            for n in &overlay {
                pass_env.insert(Rc::from(n.as_str()), assumption[n]);
            }
            let mut returns = Vec::new();
            self.replay_statements(ast, &mut pass_env, body, interner, &mut returns);
            // A non-exact exit is a decline wearing a "stable" verdict —
            // `join(Dynamic, Dynamic) == Dynamic` — so the run falls back
            // to the structural criterion rather than trust the pass.
            if names.iter().any(|n| {
                Self::type_has_decline(
                    interner,
                    pass_env.get(n.as_str()).copied().unwrap_or(assumption[n]),
                )
            }) {
                structural!();
            }
            let mut stable = true;
            for n in &names {
                let exit_ty = pass_env.get(n.as_str()).copied().unwrap_or(assumption[n]);
                if last {
                    let base = self.widen_value_pinned(assumption[n], interner);
                    let exit_w = self.widen_value_pinned(exit_ty, interner);
                    let joined = rigor_types::Algebra::join(interner, base, exit_w);
                    assumption.insert(
                        n.clone(),
                        if joined == base {
                            joined
                        } else {
                            interner.untyped()
                        },
                    );
                } else {
                    let joined = rigor_types::Algebra::join(interner, assumption[n], exit_ty);
                    if joined != assumption[n] {
                        stable = false;
                        assumption.insert(n.clone(), joined);
                    }
                }
            }
            if stable {
                break;
            }
        }
        self.loop_pass.set(prev_loop_pass);
        Some(LoopContinuation::Fixpoint(assumption))
    }

    /// Whether `ty` carries a decline marker a replay verdict must not
    /// trust — `Dynamic` (a `type_of` miss, a `replay_opaque` widen, an
    /// op-write decline), `Top`, `Bottom` or `Void` — anywhere in the
    /// shape. `Constant`, `Nominal`, `Tuple` &co. are value-exact: a
    /// `Tuple` that keeps growing converges only via the cap pass, which
    /// is the verdict the fixpoint exists to observe.
    fn type_has_decline(interner: &Interner, ty: TypeId) -> bool {
        match interner.get(ty) {
            Type::Top | Type::Bottom | Type::Void | Type::Dynamic(_) => true,
            Type::Union(ms) | Type::Intersection(ms) | Type::Tuple(ms) => {
                ms.iter().any(|&m| Self::type_has_decline(interner, m))
            }
            Type::HashShape(ms) => {
                ms.iter().any(|m| Self::type_has_decline(interner, m.value))
            }
            Type::Nominal { args, .. } | Type::App { args, .. } => {
                args.iter().any(|&a| Self::type_has_decline(interner, a))
            }
            Type::Refined { base, .. } | Type::Complement(base) => {
                Self::type_has_decline(interner, *base)
            }
            Type::Difference { base, removed } => {
                Self::type_has_decline(interner, *base)
                    || Self::type_has_decline(interner, *removed)
            }
            Type::DataInstance { members, .. } => {
                members.iter().any(|(_, t)| Self::type_has_decline(interner, *t))
            }
            _ => false,
        }
    }

    /// ADR-56 slice B — the structural convergence criterion for the loop
    /// rebind fixpoint, the port of the reference's pass analysis
    /// (`StatementEvaluator#loop_rebind_fixpoint`'s suspect-name logic,
    /// upstream #1359). Runs when [`Self::loop_continuation`] cannot trust
    /// the replay: every local the body writes textually is TRACKED
    /// (`loop_body_local_writes` + `loop_body_ivar_writes`), a name whose
    /// binding can differ from its seed at any pass is MOVED, and a name
    /// that can still move at pass >= 2 is SUSPECT — the cap trigger.
    /// Returns `(tracked, suspect)` on a cap — the caller widens over its
    /// POST-JOIN env via [`Self::apply_loop_cap_widen`] — `None` when the
    /// body provably converges or is untracked.
    fn loop_cap_widening(
        &self,
        ast: &LoweredAst,
        env: &TypeEnv,
        body: &[NodeId],
        interner: &mut Interner,
    ) -> Option<(Vec<String>, std::collections::HashSet<String>)> {
        use std::collections::HashSet;
        use rigor_parse::OpWriteKind;

        // `loop_body_local_writes` + `loop_body_ivar_writes`: every local the
        // body writes textually (nested def/class/module scopes excluded),
        // plus the ivars bound at entry (the `rebindable_ivar?` gate).
        let marks = self.def_local_marks(ast);
        let body_span = crate::def_local_marks::body_span(ast, body)?;
        let mut names: Vec<String> = Vec::new();
        for name in marks.written_within(body_span) {
            if crate::def_local_marks::is_ivar(&name) {
                if env.contains_key(name.as_str()) {
                    names.push(name);
                }
            } else {
                names.push(name);
            }
        }
        if names.is_empty() {
            return None;
        }
        let tracked: HashSet<&str> = names.iter().map(String::as_str).collect();
        let nested = marks.nested_scopes_within(body_span);
        let inside_nested =
            |span: rigor_parse::Span| nested.iter().any(|s| s.0 <= span.0 && span.1 <= s.1);
        let dead = self.dead_positions(ast, &mut Vec::new());

        // The body's live write sites. `value` is the RHS node — a `None`
        // site is a write the port cannot read the value of (pattern binds,
        // `for` indices), which can never prove convergence.
        struct Site {
            target: String,
            span: rigor_parse::Span,
            value: Option<rigor_parse::Span>,
            value_node: Option<NodeId>,
            compound: bool,
            unmodeled: bool,
            /// A `LocalVariableWrite` / `InstanceVariableWrite` — whose
            /// compound verdict `self_compound` re-derives per operand
            /// (rigor-rs#683). Op-write and masgn sites keep the scan's
            /// `compound`.
            plain: bool,
            /// A `LocalVariableOpWrite` — `x += v` / `x ||= v` / `x &&= v`.
            /// Its RHS is an IMPLICIT self-read, so it can never cut a
            /// later read off the pass-entry binding.
            op: bool,
        }
        let mut sites: Vec<Site> = Vec::new();
        for (_, n) in ast.iter() {
            let span = n.span();
            if !(body_span.0 <= span.0 && span.1 <= body_span.1) || inside_nested(span) {
                continue;
            }
            match n {
                Node::LocalVariableWrite { name, value, span, .. }
                | Node::InstanceVariableWrite { name, value, span, .. } => {
                    if !tracked.contains(name.as_str()) {
                        continue;
                    }
                    let vs = ast.get(*value).span();
                    // A plain self-write's compound verdict is re-derived
                    // per operand post-`live` (`self_compound` below,
                    // rigor-rs#683): `x = x || v` converges, but a self-read
                    // buried in an operand (`x = [1, 2][x] || 0`,
                    // `x = f(x)`) can compound.
                    let compound = marks.reads_within(vs).contains(&name.as_str())
                        && !matches!(ast.get(*value), Node::LocalVariableRead { name: n2, .. } | Node::VariableRead { name: n2, .. } if n2 == name);
                    sites.push(Site {
                        target: name.clone(),
                        span: *span,
                        value: Some(vs),
                        value_node: Some(*value),
                        compound,
                        unmodeled: false,
                        plain: true,
                        op: false,
                    });
                }
                Node::LocalVariableOpWrite { name, value, op, span, .. } => {
                    if !tracked.contains(name.as_str()) {
                        continue;
                    }
                    let vs = ast.get(*value).span();
                    // `x <binop>= v` folds `x.binop(v)` afresh per pass —
                    // compounding. `x ||= v` / `x &&= v` only unions with
                    // the entry binding (converges) unless `v` itself reads
                    // `x` (`x ||= [x]`).
                    let compound = matches!(op, OpWriteKind::Operator(_))
                        || (marks.reads_within(vs).contains(&name.as_str())
                            && !matches!(ast.get(*value), Node::LocalVariableRead { name: n2, .. } if n2 == name));
                    sites.push(Site {
                        target: name.clone(),
                        span: *span,
                        value: Some(vs),
                        value_node: Some(*value),
                        compound,
                        unmodeled: false,
                        plain: false,
                        op: true,
                    });
                }
                Node::MultiWrite { targets, value, span, .. } => {
                    let vs = ast.get(*value).span();
                    // A masgn RHS evaluates before any target binds, so every
                    // read inside it sees the assumption — `a, b = b, a`
                    // swaps the pass-entry bindings.
                    for (name, _) in targets.bound_names() {
                        if !tracked.contains(name.as_str()) {
                            continue;
                        }
                        sites.push(Site {
                            target: name,
                            span: *span,
                            value: Some(vs),
                            value_node: Some(*value),
                            compound: false,
                            unmodeled: false,
                            plain: false,
                            op: false,
                        });
                    }
                }
                Node::UnmodeledWrite { span, names: writes, .. } => {
                    for name in writes {
                        if !tracked.contains(name.as_str()) {
                            continue;
                        }
                        sites.push(Site {
                            target: name.clone(),
                            span: *span,
                            value: None,
                            value_node: None,
                            compound: false,
                            unmodeled: true,
                            plain: false,
                            op: false,
                        });
                    }
                }
                _ => {}
            }
        }
        let live: Vec<&Site> = sites.iter().filter(|s| !dead.covers(s.span)).collect();
        // A name's first live write position in the body: a read of it left
        // of that position sees the pass-entry ASSUMPTION; a later read sees
        // the body's own binding.
        let first_write = |name: &str| -> usize {
            live.iter()
                .filter(|s| s.target == name)
                .map(|s| s.span.0)
                .min()
                .unwrap_or(usize::MAX)
        };
        let nil = interner.nil();
        // rigor-rs#683 — a plain self-write compounds when a self-read feeds
        // its result unboundedly. `x = x || v` only unions with the entry
        // binding, but `x = [1, 2][x] || 0` / `x = x.succ || 0` / `x = f(x)`
        // keep moving until the cap. Walk the value's `&&`/`||` tree: a leaf
        // inherits an x-polarity gate — `x || R` evaluates `R` only while
        // `x` is falsey (bounded), `x && R` while `x` is truthy, a leaf
        // behind a statically-decided operand never (`x = 0 || f(x)`) — and
        // an `&&` left operand contributes only its FALSEY part to the
        // result ({nil,false}: bounded — `x = [x] && 0` converges). A leaf
        // reading the name while it may be truthy compounds, unless the
        // read is not an ASSUMPTION read — a name the body's earlier
        // x-free write cut from the pass-entry binding (`x = 1; x = f(x)`)
        // — or the operand's own type proves the write converged:
        // `x + 0` folds back to the entry binding, `x[0]`'s `Integer`
        // stays inside the entry's widened envelope (the reference's
        // `0[0] -> 0` fold converges), and an unresolved `foo(x)`
        // declines to `Dynamic`, which joins the binding into a stable
        // `Dynamic`-union on the second pass.
        let mut self_compound: HashSet<usize> = HashSet::new();
        for site in &live {
            if !site.plain || !site.compound {
                continue;
            }
            let Some(v) = site.value_node else {
                continue;
            };
            let n = site.target.as_str();
            let entry = env.get(n).copied().unwrap_or(nil);
            // A read at `p` sees the pass-entry binding iff no earlier
            // write of the name produced a DOMINATING x-free binding before
            // it. Dominating = the write is itself a body statement
            // (`x = 0` cuts; `x = 0 if d` does not — the assumption
            // survives the untaken arm). A write that itself reads the
            // name (`x = x`, `x = x || 0`, `x += v`) yields an
            // assumption-DERIVED binding and does not cut either; an
            // op-write's self-read is implicit and never cuts.
            let assumption_read = |p: usize| {
                live.iter()
                    .filter(|s| s.target == n && s.span.1 <= p)
                    .all(|s| {
                        if s.op {
                            return true;
                        }
                        let dominates = body.iter().any(|b| {
                            let bn = ast.get(*b);
                            matches!(
                                bn,
                                Node::LocalVariableWrite { .. }
                                    | Node::InstanceVariableWrite { .. }
                                    | Node::MultiWrite { .. }
                                    | Node::UnmodeledWrite { .. }
                            ) && bn.span() == s.span
                        });
                        if !dominates {
                            return true;
                        }
                        match s.value {
                            Some(sv) => marks.reads_within(sv).contains(&n),
                            // An unmodeled write may or may not carry the
                            // assumption — keep the read (compound is the
                            // FP-safe side).
                            None => true,
                        }
                    })
            };
            // {FALSEY = 0b01, TRUTHY = 0b10}: the x-polarities under which
            // this leaf's read can evaluate unboundedly.
            let mut stack: Vec<(NodeId, u8)> = vec![(v, 0b11)];
            'site: while let Some((id, gate)) = stack.pop() {
                match ast.get(id) {
                    Node::Logical { left, right, is_and, .. } => {
                        let (left, right, is_and) = (*left, *right, *is_and);
                        stack.push((left, if is_and { gate & 0b01 } else { gate }));
                        let cond = if is_and {
                            Self::logical_truthy_cond(ast, left, n)
                        } else {
                            Self::logical_falsey_cond(ast, left, n)
                        };
                        stack.push((right, gate & cond));
                    }
                    node => {
                        // A bare `name` operand contributes the entry
                        // binding itself — the monotone term.
                        if matches!(node, Node::LocalVariableRead { name: n2, .. } | Node::VariableRead { name: n2, .. } if n2 == n) {
                            continue;
                        }
                        // Gated to `name` falsey, or never: the read is
                        // bounded by the {nil,false} set.
                        if gate & 0b10 == 0 {
                            continue;
                        }
                        let has_assumption_read = marks
                            .read_positions_within(node.span())
                            .iter()
                            .any(|(p, r)| *r == n && assumption_read(*p));
                        if !has_assumption_read {
                            continue;
                        }
                        // The operand's type proves the write converged.
                        // A foreign read sees the BODY's binding, not the
                        // entry's — `k = 0` pre-loop then `k = 1` and
                        // `x = x + k` inside must type the leaf as
                        // `x + 1`, so overlay each earlier dominating
                        // write's value type onto the entry env.
                        // `x + 0` folds back to the entry binding (`vt ⊆
                        // entry`; a nominal codomain like `x[0]`'s
                        // `Integer` is NOT enough — `x.bit_length` types
                        // `Integer` too and keeps moving inside it), and a
                        // `Dynamic` operand joins the binding into a
                        // stable union on the next pass — but only for a
                        // decline the reference must share: the name is
                        // unbound at entry (a body-first `x`'s nil seed
                        // declines identically), or the leaf is a
                        // receiverless call nothing resolves — no
                        // toplevel `def`, no Object/Kernel surface, no
                        // project `def` on the enclosing class (`foo(x)`
                        // converges; a defined `g(x)` or `Rational(x)`
                        // compounds). A decline the reference resolves
                        // (`[1,2].fetch(x)`) keeps moving — compound.
                        let mut leaf_env = env.clone();
                        for (pos, r) in marks.read_positions_within(node.span()) {
                            if r == n {
                                continue;
                            }
                            let mut r_ty = None;
                            for s in live
                                .iter()
                                .filter(|s| s.target == r && s.span.1 <= pos)
                            {
                                match s.value_node {
                                    Some(sv) if s.plain => {
                                        let wv = self.type_of(ast, sv, env, interner);
                                        r_ty = Some(match r_ty {
                                            None => wv,
                                            Some(prev) => {
                                                rigor_types::Algebra::join(interner, prev, wv)
                                            }
                                        });
                                    }
                                    _ => {
                                        r_ty = None;
                                        break;
                                    }
                                }
                            }
                            if let Some(wv) = r_ty {
                                leaf_env.insert(Rc::from(r), wv);
                            }
                        }
                        let vt = self.type_of(ast, id, &leaf_env, interner);
                        let vt_decline = Self::type_has_decline(interner, vt);
                        let unresolvable = matches!(node, Node::Call { receiver: None, method, .. } if {
                            let scope = self
                                .lexical_scopes
                                .iter()
                                .filter(|(s, _)| {
                                    s.0 <= node.span().0 && node.span().1 <= s.1
                                })
                                .min_by_key(|(s, _)| s.1 - s.0)
                                .map(|(_, segs)| segs.join("::"));
                            let class_name = scope.as_deref().unwrap_or("Object");
                            let resolves = self
                                .source
                                .is_toplevel_def(self.file_key, method)
                                || self.index.class_has_method("Object", method)
                                || self.source.project_declares_method(
                                    self.file_key,
                                    class_name,
                                    method,
                                );
                            !resolves
                        });
                        let converged = rigor_types::Algebra::join(interner, vt, entry)
                            == entry
                            || (vt_decline
                                && (!env.contains_key(n) || unresolvable));
                        if !converged {
                            self_compound.insert(site.span.0);
                            break 'site;
                        }
                    }
                }
            }
        }

        // `moved`: a name whose binding can differ from its seed at ANY pass
        // — a write whose value types differently than the entry binding,
        // plus the propagated cases below. `suspect`: a name that can still
        // move at pass >= 2 — the cap trigger.
        let mut moved: HashSet<String> = HashSet::new();
        let mut suspect: HashSet<String> = HashSet::new();
        loop {
            let mut grew = false;
            for site in &live {
                let n = site.target.as_str();
                if site.unmodeled {
                    grew |= suspect.insert(n.to_string());
                }
                // For a plain write the per-operand pass is authoritative
                // (its `site.compound` was only the scan's coarse
                // "reads-self" flag); `x <op>=` and masgn keep it.
                if if site.plain {
                    self_compound.contains(&site.span.0)
                } else {
                    site.compound
                } {
                    grew |= suspect.insert(n.to_string());
                }
                if !moved.contains(n) {
                    // A pass-1 mover: the write's value at the entry env is
                    // not the seed binding (`y = 2` for `y = nil` seeds it
                    // `nil | 2`).
                    let differs = match site.value_node {
                        Some(v) => {
                            let vt = self.type_of(ast, v, env, interner);
                            vt != env.get(n).copied().unwrap_or(nil)
                        }
                        None => true,
                    };
                    if differs {
                        moved.insert(n.to_string());
                        grew = true;
                    }
                }
                // Assumption reads: a value read of a moved name that
                // precedes the name's first body write sees the pass-entry
                // binding — it changes again next pass, so the reader moves
                // at pass >= 2 (`s = t` before `t`'s write). Reads of a
                // SUSPECT name move regardless of position.
                if !suspect.contains(n) {
                    if let Some(vs) = site.value {
                        let hits = marks
                            .read_positions_within(vs)
                            .iter()
                            .any(|(pos, r)| {
                                *r != n
                                    && (suspect.contains(*r)
                                        || (moved.contains(*r) && *pos < first_write(r)))
                            });
                        if hits {
                            suspect.insert(n.to_string());
                            grew = true;
                        }
                    }
                }
            }
            // A decision whose read can still move flips an arm's liveness
            // between passes — every name written inside its construct (the
            // smallest node enclosing the predicate span: the `if`, `while`,
            // `&&`/`||`, `case`/`when`) moves at pass >= 2 (`if s then
            // y = 1 end`, o8).
            for d in marks.decision_spans_within(body_span) {
                if dead.covers(d) {
                    continue;
                }
                let hot = marks.read_positions_within(d).iter().any(|(pos, r)| {
                    suspect.contains(*r) || (moved.contains(*r) && *pos < first_write(r))
                });
                if !hot {
                    continue;
                }
                let gate = ast
                    .iter()
                    .map(|(_, n)| n.span())
                    .filter(|s| s.0 <= d.0 && d.1 <= s.1 && *s != d)
                    .min_by_key(|s| s.1 - s.0);
                if let Some(gate) = gate {
                    for site in &live {
                        if gate.0 <= site.span.0 && site.span.1 <= gate.1 {
                            grew |= suspect.insert(site.target.clone());
                        }
                    }
                }
            }
            for n in suspect.iter() {
                moved.insert(n.clone());
            }
            if !grew {
                break;
            }
        }
        if suspect.is_empty() {
            return None;
        }
        Some((names, suspect))
    }

    /// The `x`-polarities under which a `&&`/`||` operand can evaluate
    /// FALSEY, as a bitmask `{FALSEY = 0b01, TRUTHY = 0b10}` over the
    /// tracked name's polarity — the gate [`Self::loop_cap_widening`]
    /// threads into right operands (rigor-rs#683). A bare `name` operand
    /// is falsey exactly when `name` is; a `||` / `&&` composes
    /// distributively (`a || b` falsey iff both are, `a && b` iff either
    /// is); literals carry their static polarity; every other leaf may go
    /// either way (conservative `0b11`).
    fn logical_falsey_cond(ast: &LoweredAst, id: NodeId, name: &str) -> u8 {
        match ast.get(id) {
            Node::LocalVariableRead { name: n2, .. } | Node::VariableRead { name: n2, .. }
                if n2 == name =>
            {
                0b01
            }
            Node::Logical { left, right, is_and, .. } => {
                let (l, r) = (Self::logical_falsey_cond(ast, *left, name), Self::logical_falsey_cond(ast, *right, name));
                if *is_and {
                    l | r
                } else {
                    l & r
                }
            }
            Node::NilLit { .. } | Node::FalseLit { .. } => 0b11,
            Node::StringLit { .. }
            | Node::InterpolatedString { .. }
            | Node::InterpolatedSymbol { .. }
            | Node::IntegerLit { .. }
            | Node::FloatLit { .. }
            | Node::RegexpLit { .. }
            | Node::RationalLit { .. }
            | Node::ImaginaryLit { .. }
            | Node::SymbolLit { .. }
            | Node::TrueLit { .. }
            | Node::ArrayLit { .. }
            | Node::HashLit { .. }
            | Node::Range { .. }
            | Node::Lambda { .. }
            | Node::SelfExpr { .. } => 0,
            _ => 0b11,
        }
    }

    /// [`Self::logical_falsey_cond`]'s dual: the `x`-polarities under
    /// which the operand can evaluate TRUTHY.
    fn logical_truthy_cond(ast: &LoweredAst, id: NodeId, name: &str) -> u8 {
        match ast.get(id) {
            Node::LocalVariableRead { name: n2, .. } | Node::VariableRead { name: n2, .. }
                if n2 == name =>
            {
                0b10
            }
            Node::Logical { left, right, is_and, .. } => {
                let (l, r) = (Self::logical_truthy_cond(ast, *left, name), Self::logical_truthy_cond(ast, *right, name));
                if *is_and {
                    l & r
                } else {
                    l | r
                }
            }
            Node::NilLit { .. } | Node::FalseLit { .. } => 0,
            Node::StringLit { .. }
            | Node::InterpolatedString { .. }
            | Node::InterpolatedSymbol { .. }
            | Node::IntegerLit { .. }
            | Node::FloatLit { .. }
            | Node::RegexpLit { .. }
            | Node::RationalLit { .. }
            | Node::ImaginaryLit { .. }
            | Node::SymbolLit { .. }
            | Node::TrueLit { .. }
            | Node::ArrayLit { .. }
            | Node::HashLit { .. }
            | Node::Range { .. }
            | Node::Lambda { .. }
            | Node::SelfExpr { .. } => 0b11,
            _ => 0b11,
        }
    }

    /// Applies a capped fixpoint's last-pass widen to the caller's POST-JOIN
    /// env: a still-moving (suspect) tracked name floors to `Dynamic[top]`;
    /// every other tracked name sheds `Constant -> Nominal` the reference's
    /// way ([`Self::widen_value_pinned`]). The widen reads the binding the
    /// join produced — a body write to a non-suspect name (`w = 1` under a
    /// `w = nil` seed) must survive, and a name the join does not bind is
    /// left alone, never minted `nil`.
    fn apply_loop_cap_widen(
        &self,
        env: &mut TypeEnv,
        tracked: &[String],
        suspect: &std::collections::HashSet<String>,
        interner: &mut Interner,
    ) {
        let untyped = interner.untyped();
        for n in tracked {
            let Some(&post) = env.get(n.as_str()) else { continue };
            let widened = if suspect.contains(n) {
                untyped
            } else {
                self.widen_value_pinned(post, interner)
            };
            env.insert(Rc::from(n.as_str()), widened);
        }
    }

    /// [`Self::build_toplevel_env`] for the `check` rules: the same straight-line
    /// binder, except that every statement it does not bind WIDENS (to
    /// `Dynamic`) each top-level local rebound inside it, in statement order.
    ///
    /// The flat binder sees only a top-level `x = …` / `a, b = …`; a rebind
    /// nested in an `if`, a loop, a block, a `begin`, or an `x += …` is
    /// invisible to it, so the local kept its FIRST type past the construct.
    /// The reference joins every path that leaves the construct — including a
    /// `next` / `break` path, since upstream #1248 (loops) and #1215 (blocks) —
    /// so `w = String.new; while …; w = i; next; end; w.even?` and
    /// `n = String.new; xs.each { |e| n = e; next }; n.even?` are silent there
    /// while the flat env still fired `for String` (rigor-rs#133). The port has
    /// no flow evaluator behind this env to join into, so it declines instead:
    /// a strict loss of information, which cannot add a diagnostic. A local
    /// the construct does not rebind keeps its type, so the no-rebind controls
    /// still fire.
    ///
    /// A write inside a `def` / `class` / `module` body is a different local
    /// scope and widens nothing. A write in a `->` or block body does widen: a
    /// closure may run and rebind a CAPTURED local. A write to a name the
    /// block/lambda binds itself (a parameter or `;`-declared block-local —
    /// [`Node::Call::block_locals`], rigor-rs#166) shadows the top-level name
    /// and widens nothing either.
    pub fn build_toplevel_check_env(&self, ast: &LoweredAst, interner: &mut Interner) -> TypeEnv {
        self.build_toplevel_check_flow(ast, interner).env
    }

    /// [`Self::build_toplevel_check_env`] plus the boundary/effect data
    /// [`Self::check_env_at`] needs to give a use site the env it was ENTERED
    /// from: the env snapshot before each top-level statement, and the
    /// span-keyed rebind/mutation lists an ordered replay inside the
    /// statement applies (rigor-rs#136, the port of upstream rigor#1310's
    /// per-node scope index).
    pub fn build_toplevel_check_flow(&self, ast: &LoweredAst, interner: &mut Interner) -> CheckFlow {
        let mut env = TypeEnv::new();
        let body = match ast.get(ast.root()) {
            Node::Program { body, .. } => body.clone(),
            _ => {
                return CheckFlow {
                    env,
                    boundaries: Vec::new(),
                    boundary_stmts: Vec::new(),
                    boundary_stride: 1,
                    effects: FlowEffects::new(Vec::new(), Vec::new()),
                    closure_mutations: Vec::new(),
                    indexed: IndexedFlow::default(),
                    dead: std::rc::Rc::new(crate::dead::DeadPositions::default()),
                    loops: Vec::new(),
                    ifs: Vec::new(),
                    record_guards: Vec::new(),
                }
            }
        };
        // rigor-rs#368 — provably-dead positions (computed once for the file;
        // `seen` starts empty — the predicate fold is a fresh walk).
        let dead = self.dead_positions(ast, &mut Vec::new());
        let mut rebinds = toplevel_rebinds(ast);
        // Upstream v0.4.7 (ADR-127): a mutable local a call may hand to a
        // project body is floored after the call — read as a rebind here,
        // which widens it (the receiver typing behind `call.*` reads this
        // env, not the condition walkers' indexed writes).
        rebinds.extend(toplevel_argument_floor(ast, self.source));
        let mut mutations = toplevel_mutations(ast);
        let mut closure_muts = closure_mutations(ast);
        // A write or mutation inside a folded-dead arm never lands — it must
        // not widen the flat env either (`x = 1; if false; x = "s"; end; x.w`
        // fires `for 1` on the oracle, not a widened decline). A `rescue`
        // arm's writes keep widening: they are real writes whose reach the
        // position rule in `DeadPositions::write_reaches` decides per-use.
        // One mutation exception survives: a content-writeback-scan call
        // (`CONTENT_MUTATORS`/index write) inside an iterated body — the
        // reference's textual scan lands it on the post-body scope even
        // from a folded arm (`while c; if false; h[:a] = 9; end; end`
        // still drops `h[:a]` on the oracle — rigor-rs#363 review).
        // `mutation_reaches` decides the per-use reach; a use INSIDE the
        // body keeps the record.
        rebinds.retain(|(s, _)| !dead.covers(*s));
        mutations.retain(|(s, _, m, _)| {
            !dead.covers(*s) || (is_content_mutator(m) && dead.inside_iterated(*s))
        });
        closure_muts.retain(|(_, w, _)| !dead.covers(*w));
        let mut indexed = collect_indexed_flow(ast);
        // Upstream #2677's `SiteCollector` walks the whole program — a
        // copy link inside a folded-dead arm still counts (`if false;
        // g = f; end; g << 2` floors `f` on the oracle), so no
        // dead-position filter applies to the links. A mutation inside
        // one still never ran, so it takes the `mutations` retain.
        indexed.alias_mutations.retain(|am| {
            !dead.covers(am.span)
                || (is_content_mutator(&am.method) && dead.inside_iterated(am.span))
        });
        indexed.retry_drops = self.retry_edge_index_drops(ast, &rebinds, &mutations, &indexed);
        // Boundaries only matter while a recorded effect could postdate a use
        // site, a folded-dead arm could hold one, or a `rescue` clause could
        // (its own bindings + `=> e` reach only through replay — rigor-rs#368);
        // a file with none of these gets the flat env for every site. An
        // in-closure attribute write is an effect only a site inside that
        // body can observe (rigor-rs#366), but it still requires the
        // boundary + replay path to reach the site at all.
        let record = !(rebinds.is_empty() && mutations.is_empty() && closure_muts.is_empty())
            || dead.has_folded()
            || dead.has_rescue();
        // A boundary env per statement is a full clone of a growing map —
        // O(statements × env size) time and memory on a flat file
        // (rigor-rs#424). Checkpoints at `stride` spacing bound the table
        // to ~sqrt(N) envs; `check_env_at` replays the ≤ stride-1
        // statements between a checkpoint and the queried one through the
        // same `bind_check_statement`, so the env it sees is identical.
        let stride = (body.len() as f64).sqrt().ceil().max(1.0) as usize;
        let mut boundaries = Vec::new();
        let mut boundary_stmts = Vec::with_capacity(body.len());
        let effects = FlowEffects::new(rebinds, mutations);
        for (i, &stmt) in body.iter().enumerate() {
            if record {
                boundary_stmts.push((stmt, ast.get(stmt).span()));
                if i % stride == 0 {
                    boundaries.push((i, env.clone()));
                }
            }
            self.bind_check_statement(ast, stmt, &mut env, &effects, &indexed, &dead, interner);
        }
        let record_guards = self.collect_record_guards(ast, &indexed);
        CheckFlow {
            env,
            boundaries,
            boundary_stmts,
            boundary_stride: stride,
            effects,
            closure_mutations: closure_muts,
            indexed,
            dead,
            loops: self.collect_loop_edges(ast),
            ifs: self.collect_if_edges(ast),
            record_guards,
        }
    }

    /// [`CheckFlow::loops`]: every `while`/`until` loop with a predicate
    /// local, its body hull, its innermost `def`, and per local the first
    /// write after the loop (a later binding the overlay must not cover).
    fn collect_loop_edges(&self, ast: &LoweredAst) -> Vec<LoopEdges> {
        let writes = collect_flow_writes(ast);
        let defs = crate::method_body_spans(ast);
        let closures: Vec<rigor_parse::Span> =
            ast.iter().filter_map(|(_, n)| crate::loop_edges::closure_extent(n)).collect();
        let mut out = Vec::new();
        for (id, n) in ast.iter() {
            let Node::Loop { span, .. } = n else { continue };
            let span = *span;
            let names = self.loop_predicate_locals(ast, id);
            if names.is_empty() {
                continue;
            }
            let locals = names
                .into_iter()
                .map(|name| {
                    let next = writes
                        .iter()
                        .filter(|(w, n)| *n == name && w.0 >= span.1)
                        .map(|(w, _)| w.0)
                        .min();
                    (name, next)
                })
                .collect();
            let def = defs
                .iter()
                .filter(|d| d.0 <= span.0 && span.1 <= d.1)
                .min_by_key(|d| d.1 - d.0)
                .copied();
            let closure = closures
                .iter()
                .filter(|c| c.0 <= span.0 && span.1 <= c.1)
                .min_by_key(|c| c.1 - c.0)
                .copied();
            out.push(LoopEdges {
                id,
                span,
                body: Self::loop_body_hull(ast, id),
                def,
                closure,
                locals,
            });
        }
        out
    }

    /// [`CheckFlow::ifs`]: every `if`/`unless` whose predicate reads a
    /// local, each arm's hull, its innermost `def`, and per local the first
    /// write after the `if` (a later binding the overlay must not cover).
    fn collect_if_edges(&self, ast: &LoweredAst) -> Vec<IfEdges> {
        let writes = collect_flow_writes(ast);
        let defs = crate::method_body_spans(ast);
        let closures: Vec<rigor_parse::Span> =
            ast.iter().filter_map(|(_, n)| crate::loop_edges::closure_extent(n)).collect();
        let mut out = Vec::new();
        for (_id, n) in ast.iter() {
            let Node::If { predicate, span, then_body, else_body, is_unless, .. } = n else {
                continue;
            };
            let (predicate, span, is_unless) = (*predicate, *span, *is_unless);
            let names: Vec<String> = self
                .subjects_within(ast, predicate)
                .into_iter()
                .filter_map(|(s, _)| match s {
                    crate::loop_edges::Subject::Local(n) => Some(n),
                    _ => None,
                })
                .collect();
            if names.is_empty() {
                continue;
            }
            let locals = names
                .into_iter()
                .map(|name| {
                    let next = writes
                        .iter()
                        .filter(|(w, n)| *n == name && w.0 >= span.1)
                        .map(|(w, _)| w.0)
                        .min();
                    (name, next)
                })
                .collect();
            let def = defs
                .iter()
                .filter(|d| d.0 <= span.0 && span.1 <= d.1)
                .min_by_key(|d| d.1 - d.0)
                .copied();
            let closure = closures
                .iter()
                .filter(|c| c.0 <= span.0 && span.1 <= c.1)
                .min_by_key(|c| c.1 - c.0)
                .copied();
            out.push(IfEdges {
                predicate,
                span,
                then_hull: crate::dead::body_hull(ast, then_body),
                else_hull: crate::dead::body_hull(ast, else_body),
                is_unless,
                def,
                closure,
                locals,
            });
        }
        out
    }

    /// [`Typer::loop_edge_overlay`] for `if`/`unless` statements
    /// (rigor-rs#451): a site AFTER an `if` whose predicate narrows a local
    /// reads the join of the arms' edge types (`if h.empty?; end; h.frob`
    /// is reference-silent — `h` is `Dynamic` after, not `Array`), and a
    /// site INSIDE an arm reads that edge's narrowing. A decided
    /// predicate — `[1].empty?` — is the reference's folded `if` and
    /// narrows nothing.
    fn if_edge_overlay(
        &self,
        ast: &LoweredAst,
        flow: &CheckFlow,
        site: rigor_parse::Span,
        env: &mut Cow<'_, TypeEnv>,
        interner: &mut Interner,
    ) {
        for i in &flow.ifs {

            let outside = |c: Option<rigor_parse::Span>| {
                c.is_some_and(|c| !(c.0 <= site.0 && site.1 <= c.1))
            };
            if outside(i.def) || outside(i.closure) {
                continue;
            }
            let edge_in_arm = if i.then_hull.is_some_and(|b| b.0 <= site.0 && site.1 <= b.1) {
                Some(!i.is_unless)
            } else if i.else_hull.is_some_and(|b| b.0 <= site.0 && site.1 <= b.1) {
                Some(i.is_unless)
            } else {
                None
            };
            let after = edge_in_arm.is_none() && site.0 >= i.span.1;
            if edge_in_arm.is_none() && !after {
                continue;
            }
            for (name, next) in &i.locals {
                if next.is_some_and(|w| w < site.0) {
                    continue;
                }
                let Some(&ty) = env.get(name.as_str()) else { continue };
                if matches!(interner.get(ty), Type::Dynamic(_) | Type::Top) {
                    continue;
                }
                if self.known_polarity(ast, env, i.predicate, interner).is_some() {
                    continue;
                }
                match edge_in_arm {
                    Some(edge) => {
                        let e = self.subject_edge(
                            ast,
                            i.predicate,
                            &crate::loop_edges::Subject::Local(name.clone()),
                            ty,
                            edge,
                            false,
                            interner,
                        );
                        if e != crate::loop_edges::Edge::Keep {
                            let u = interner.untyped();
                            env.to_mut().insert(Rc::from(name.as_str()), u);
                            drop_indexed_narrowings(env.to_mut(), name);
                        }
                    }
                    None => {
                        let t = self.subject_edge(ast, i.predicate, &crate::loop_edges::Subject::Local(name.clone()), ty, !i.is_unless, false, interner);
                        let f = self.subject_edge(ast, i.predicate, &crate::loop_edges::Subject::Local(name.clone()), ty, i.is_unless, false, interner);
                        use crate::loop_edges::Edge;
                        if t == Edge::Other || f == Edge::Other {
                            let u = interner.untyped();
                            env.to_mut().insert(Rc::from(name.as_str()), u);
                            drop_indexed_narrowings(env.to_mut(), name);
                        }
                    }
                }
            }
        }
    }

    /// Upstream #1359 (`eval_loop`'s `loop_pass_entry` /
    /// `narrow_loop_exit_edge`): a site inside a `while`/`until` body reads
    /// the predicate's loop-entry edge, and a site after the loop reads the
    /// exit edge over the joined scope. No engine here models either edge,
    /// so a predicate local whose edge the reference does not leave
    /// unchanged ([`Typer::loop_edge_keeps`]) is widened at the site — the
    /// decline side. A site after the loop is covered until the local's next
    /// write; a loop inside a `def` or a block / `->` body the site is not in
    /// narrows a different scope.
    fn loop_edge_overlay(
        &self,
        ast: &LoweredAst,
        flow: &CheckFlow,
        site: rigor_parse::Span,
        env: &mut Cow<'_, TypeEnv>,
        interner: &mut Interner,
    ) {
        for l in &flow.loops {
            let outside = |c: Option<rigor_parse::Span>| {
                c.is_some_and(|c| !(c.0 <= site.0 && site.1 <= c.1))
            };
            if outside(l.def) || outside(l.closure) {
                continue;
            }
            let side = if l.body.is_some_and(|b| b.0 <= site.0 && site.1 <= b.1) {
                crate::loop_edges::LoopSide::Body
            } else if site.0 >= l.span.1 {
                crate::loop_edges::LoopSide::Exit
            } else {
                continue;
            };
            for (name, next) in &l.locals {
                if side == crate::loop_edges::LoopSide::Exit && next.is_some_and(|w| w < site.0) {
                    continue;
                }
                let Some(&ty) = env.get(name.as_str()) else { continue };
                if matches!(interner.get(ty), Type::Dynamic(_) | Type::Top) {
                    continue;
                }
                if !self.loop_edge_keeps(ast, l.id, name, ty, side, interner) {
                    let u = interner.untyped();
                    let env = env.to_mut();
                    env.insert(Rc::from(name.as_str()), u);
                    drop_indexed_narrowings(env, name);
                }
            }
        }
    }

    /// The env a use site at `site` was entered from: the pre-statement
    /// boundary env, replayed in evaluation order through the effects that
    /// run before the site. An operand earlier than a same-statement
    /// mutation or rebind therefore still types from the binding it saw —
    /// the reference's `argument_scope` / per-node scope index
    /// (`OperandWalk`, rigor-rs#136) — while a LATER operand still sees the
    /// effect, exactly as the flat env already produced.
    ///
    /// Sites inside a literal block body or lambda read the env at the
    /// closure with the body's own effects applied conservatively
    /// ([`Self::closure_entry`], issue #473). Sites inside a `def`, `class`
    /// or `module` body get the flat env, and so does a site with no
    /// recorded effect at or after its statement — the replayed env is then
    /// byte-identical to the flat one.
    pub fn check_env_at<'f>(
        &self,
        ast: &LoweredAst,
        flow: &'f CheckFlow,
        site: rigor_parse::Span,
        interner: &mut Interner,
    ) -> Cow<'f, TypeEnv> {
        let base: Cow<'f, TypeEnv> = 'env: {
            // The containing statement: `boundary_stmts` is in program
            // (span) order and top-level statements never overlap, so the
            // last one starting at or before the site is the only
            // candidate — a binary search, not the per-statement `find`
            // this used to run (rigor-rs#424).
            let k = flow
                .boundary_stmts
                .partition_point(|(_, s)| s.0 <= site.0);
            let Some(&(stmt, stmt_span)) = k
                .checked_sub(1)
                .and_then(|i| flow.boundary_stmts.get(i))
            else {
                break 'env Cow::Borrowed(&flow.env);
            };
            if !(stmt_span.0 <= site.0 && site.1 <= stmt_span.1) {
                break 'env Cow::Borrowed(&flow.env);
            }
            // rigor-rs#368 — a site inside a `rescue` clause also needs the
            // replay even when no write postdates its statement: the clause's
            // own bindings (`rescue => e`) reach only through it.
            let later = flow.effects.rebinds.iter().any(|(w, _)| w.0 >= stmt_span.0)
                || flow.effects.mutations.iter().any(|(w, _, _, _)| w.0 >= stmt_span.0)
                || flow
                    .closure_mutations
                    .iter()
                    .any(|(_, w, _)| w.0 >= stmt_span.0)
                || flow.dead.in_rescue(site);
            if !later {
                break 'env Cow::Borrowed(&flow.env);
            }
            // The env before `stmt`: the checkpoint at or before its index
            // plus the same `bind_check_statement` replay the flat env ran
            // over the ≤ `boundary_stride` statements in between — the env
            // is byte-identical to the per-statement snapshot this
            // replaced (rigor-rs#424).
            let ci = flow
                .boundaries
                .partition_point(|&(i, _)| i < k)
                .saturating_sub(1);
            let (cp_idx, ref cp_env) = flow.boundaries[ci];
            let mut env = cp_env.clone();
            for &(s, _) in &flow.boundary_stmts[cp_idx..k - 1] {
                self.bind_check_statement(
                    ast, s, &mut env, &flow.effects, &flow.indexed, &flow.dead, interner,
                );
            }
            self.entry_descend(ast, stmt, site, true, false, &mut env, flow, interner);
            Cow::Owned(env)
        };
        // rigor-rs#368 — a site inside a folded-dead `if`/`unless` arm reads
        // the arm's ENTRY scope: the reference never evaluates the arm, and
        // `propagate` fills its nodes with the predicate's edge scope, whose
        // subject locals narrowed to `Bot` on the dead edge (`q = nil; if q;
        // q.w; end` is silent on the oracle). This also catches a dead-arm
        // site the boundary descent cannot reach (buried under a `Loop` /
        // `Case` — `apply_subtree_effects` is flat there).
        let mut base = base;
        self.loop_edge_overlay(ast, flow, site, &mut base, interner);
        self.if_edge_overlay(ast, flow, site, &mut base, interner);
        // rigor-rs#405 — a class guard's rebind drops the records rooted at
        // its local, on both edges and past the join.
        self.record_guard_overlay(&flow.record_guards, site, &mut base, interner);
        if let Some(subjects) = flow.dead.subjects_at(site) {
            let mut env = base.into_owned();
            for (name, op) in subjects {
                let cur = env
                    .get(name.as_str())
                    .copied()
                    .unwrap_or_else(|| interner.untyped());
                let narrowed = self.dead_subject_type(ast, cur, op, interner);
                env.insert(Rc::from(name.as_str()), narrowed);
            }
            return Cow::Owned(env);
        }
        base
    }

    /// Replay `id`'s contained effects into `env` in evaluation order until
    /// the node holding `site` is reached. `uncond` says `id`'s own position
    /// evaluates unconditionally — a mutation on an all-unconditional path
    /// mints the widened nominal (the flat env's own answer for a
    /// same-statement mutator call); anything else widens `Dynamic`, the
    /// flat env's conservative decline for a conditional position.
    /// `preserve_env` says `env` already holds this position's flat scope —
    /// descent under a non-descending container (`loop`/`case`/`when`,
    /// `begin`'s else/ensure; rigor-rs#380) — so the barrier captures that
    /// would overwrite it with `flow.env` (the end-of-FILE env, which
    /// resurrects post-statement rebinds) are skipped.
    // too_many_arguments: the replay context (ast, flow, env, interner) is
    // threaded through each recursive step; bundling into a struct would
    // obscure the recursion.
    #[allow(clippy::too_many_arguments)]
    fn entry_descend(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        site: rigor_parse::Span,
        uncond: bool,
        preserve_env: bool,
        env: &mut TypeEnv,
        flow: &CheckFlow,
        interner: &mut Interner,
    ) {
        match ast.get(id) {
            // `if` evaluates its predicate first, then ONE branch — the
            // sibling branch's writes never reach the site (upstream probe:
            // `if c; b.unshift("s"); else; b.first.upcase; end` still fires
            // on the pre-branch `b`). Sites inside the taken branch replay
            // its earlier statements conditionally.
            Node::If {
                predicate,
                then_body,
                else_body,
                is_unless,
                ..
            } => {
                let (predicate, then_body, else_body, is_unless) = (
                    *predicate,
                    then_body.clone(),
                    else_body.clone(),
                    *is_unless,
                );
                let pspan = ast.get(predicate).span();
                if pspan.0 <= site.0 && site.1 <= pspan.1 {
                    self.entry_descend(ast, predicate, site, uncond, preserve_env, env, flow, interner);
                    return;
                }
                self.apply_subtree_effects(ast, predicate, site, uncond, env, flow, interner);
                let (branch, edge) = if then_body
                    .iter()
                    .any(|&s| within_span(site, ast.get(s).span()))
                {
                    (&then_body[..], !is_unless)
                } else if else_body
                    .iter()
                    .any(|&s| within_span(site, ast.get(s).span()))
                {
                    (&else_body[..], is_unless)
                } else {
                    return; // the site is the `if`'s own span, no child holds it
                };
                // A site inside the arm reads the predicate's edge-narrowed
                // locals — `if h.is_a?(Array); else; h.frob` keeps `h`
                // `Dynamic` inside on the oracle (rigor-rs#451).
                if self
                    .known_polarity(ast, env, predicate, interner)
                    .is_none()
                {
                    for (name, ty) in
                        self.predicate_edge_binds(ast, predicate, env, edge, interner)
                    {
                        env.insert(Rc::from(name.as_str()), ty);
                    }
                }
                // rigor-rs#368 — a folded-dead arm never runs: the site reads
                // the arm's ENTRY scope (the post-predicate env), so neither
                // its own earlier writes nor any nested effect replay. The
                // `check_env_at` post-pass floors the predicate's subject
                // locals to `Bot` on top of this.
                if flow.dead.arm_dead(crate::dead::body_hull(ast, branch)) {
                    return;
                }
                for &s in branch {
                    let sspan = ast.get(s).span();
                    if sspan.0 <= site.0 && site.0 < sspan.1 {
                        self.entry_descend(ast, s, site, false, preserve_env, env, flow, interner);
                        return;
                    }
                    if sspan.1 <= site.0 {
                        // rigor-rs#362 — the arm's own earlier statements
                        // evaluate in order on the oracle (`if c; x = 2;
                        // x.w; return; end` fires `for 2`), so replay them
                        // with real bindings the way the `rescue` clause
                        // arm below does, not the flat untyped subtree
                        // replay.
                        self.bind_check_statement(
                            ast,
                            s,
                            env, &flow.effects,
                            &flow.indexed,
                            &flow.dead,
                            interner,
                        );
                    }
                }
            }
            // A `begin`'s main body runs in order unconditionally; a rescue
            // clause, the else arm and `ensure` are conditional or later, so
            // a site inside one takes every contained effect conservatively
            // — the flat env's own decline.
            Node::BeginRescue {
                main_body, clauses, span, ..
            } => {
                let (main_body, clauses) = (main_body.clone(), clauses.clone());
                // rigor-rs#393 — every site inside a retrying `begin` reads
                // the retry edge's widened entry (`eval_retried_begin`
                // re-evaluates the body and the clauses under it, and the
                // index keeps that last visit): a record rooted at a local
                // the edge rebinds is gone there too.
                drop_retry_edge_records(&flow.indexed, *span, true, env);
                if main_body
                    .iter()
                    .any(|&s| within_span(site, ast.get(s).span()))
                {
                    self.entry_children(ast, id, site, uncond, preserve_env, env, flow, interner);
                    return;
                }
                // rigor-rs#368 — a site inside a `rescue` clause replays the
                // clause's own statements in order with real bindings: the
                // reference evaluates each arm from the entry scope
                // (`collect_rescue_chain_results`), so `x = 2; x.w; raise`
                // inside fires `w for 2`. Statements after the site, and
                // sibling arms, do not reach it.
                for c in &clauses {
                    if let Some(&s) = c
                        .body
                        .iter()
                        .find(|&&s| within_span(site, ast.get(s).span()))
                    {
                        // Upstream #1837 (rigor #1231): the arm reads each
                        // local the protected body writes as `Dynamic[top]`
                        // — before the reference binds, so `rescue => e`
                        // still wins. Under a retry edge the widening holds
                        // for EVERY arm (`rescue_writes` is nil; rigor-rs#468)
                        // plus the retrying clauses' own writes via
                        // `absorb_retry_rebinds`.
                        if let (Some(&f), Some(&l)) = (main_body.first(), main_body.last()) {
                            let region = (ast.get(f).span().0, ast.get(l).span().1);
                            widen_clause_entry(
                                &flow.effects.rebinds, region, &flow.indexed.scope_spans, env, interner,
                            );
                        }
                        for rs in clauses
                            .iter()
                            .filter(|rc| self.clause_retries(ast, rc.span))
                            .map(|rc| rc.span)
                        {
                            widen_flow_writes(flow.effects.rebinds.iter(), rs, env, interner);
                        }
                        // `rescue => e` binds the exception inside the arm —
                        // `bind_rescue_reference` on the oracle
                        // (`statement_evaluator.rb:5100`).
                        if let Some(name) = c.bound_name.clone() {
                            let ty = self.rescue_reference_type(ast, c, interner);
                            env.insert(Rc::from(name.as_str()), ty);
                        }
                        for &pre_s in c.body.iter().take_while(|&x| *x != s) {
                            self.bind_check_statement(
                                ast, pre_s, env, &flow.effects, &flow.indexed, &flow.dead, interner,
                            );
                        }
                        self.entry_descend(ast, s, site, false, preserve_env, env, flow, interner);
                        return;
                    }
                }
                self.apply_subtree_effects(ast, id, site, false, env, flow, interner);
                self.entry_descend_site_child(ast, id, site, env, flow, interner);
            }
            // Conditional container: nothing inside orders against the site
            // (recovery carrier, loop, case/when), so apply every contained
            // effect conservatively — byte-identical to the flat env.
            Node::Statements { kind, .. } if !matches!(kind, StatementsKind::Sequence) => {
                self.apply_subtree_effects(ast, id, site, false, env, flow, interner);
            }
            // …but the flat pass cannot cross a deferred body's barrier: a
            // literal block/lambda nested under a `loop`/`case`/`when`
            // still evaluates its own attribute writes positionally on the
            // oracle, so descend into the child holding the site as well
            // (rigor-rs#380). The non-Sequence `Statements` carriers stay
            // flat — `Recovered`/`Jump`/`Inert` children never evaluate on
            // the reference (`x = (each { w; r }) rescue nil` and
            // `break each { w; r }` both fire there).
            Node::Loop { .. } | Node::Case { .. } => {
                self.apply_subtree_effects(ast, id, site, false, env, flow, interner);
                self.entry_descend_site_child(ast, id, site, env, flow, interner);
            }
            // A `when`/`in` arm descends into its BODY only: the oracle
            // keeps a condition-position block's writes out of the in-body
            // read's scope (`when [1].each { h.default ||= 0; h[:a].m }`
            // fires there), so `conditions` stays flat.
            Node::When { body, .. } => {
                self.apply_subtree_effects(ast, id, site, false, env, flow, interner);
                for &s in body.clone().iter() {
                    let sspan = ast.get(s).span();
                    if sspan.0 <= site.0 && site.0 < sspan.1 {
                        self.entry_descend(ast, s, site, false, preserve_env, env, flow, interner);
                        return;
                    }
                }
            }
            // A closure body reads the env at the closure, with its own
            // rebinds and mutations applied conservatively (issue #473,
            // `closure_entry`), unless the descent came from a
            // non-descending container, whose flat env is already in place
            // (`preserve_env`). A class/module body captures the whole env.
            // A lambda body is also a deferred scope with its OWN evaluated
            // attribute writes: replay them positionally inside it
            // (rigor-rs#366).
            Node::Lambda { .. } => {
                if !preserve_env {
                    self.closure_entry(ast, id, site, env, flow, interner);
                }
                self.closure_descend(ast, id, site, env, flow, id);
            }
            Node::Definition { .. } | Node::ClassDef { .. } | Node::ModuleDef { .. } => {
                if !preserve_env {
                    *env = flow.env.clone();
                }
            }
            _ => self.entry_children(ast, id, site, uncond, preserve_env, env, flow, interner),
        }
    }

    /// `rescue_exception_type` (`statement_evaluator.rb:5115`): the type a
    /// `rescue … => e` reference binds — `StandardError` for a bare `rescue`,
    /// the union of the named classes otherwise; a name the index cannot
    /// resolve contributes `Dynamic[top]` exactly as the reference's does.
    fn rescue_reference_type(
        &self,
        ast: &LoweredAst,
        clause: &rigor_parse::RescueClause,
        interner: &mut Interner,
    ) -> TypeId {
        let named = |name: &str, interner: &mut Interner| {
            self.index
                .class_id(name)
                .or_else(|| self.source.class_id(name))
                .map(|class| interner.intern(Type::Nominal { class, args: vec![] }))
                .unwrap_or_else(|| interner.untyped())
        };
        if clause.exceptions.is_empty() {
            return named("StandardError", interner);
        }
        let mut ty = interner.bottom();
        for &e in &clause.exceptions {
            let member = match ast.get(e) {
                // A `dynamic_base` path types `Dynamic[top]` upstream
                // (`rescue_exception_type` maps a non-Singleton member to
                // untyped), never its lenient trailing name (rigor-rs#673).
                Node::ConstantRead {
                    name,
                    dynamic_base: false,
                    ..
                } => named(name, interner),
                _ => interner.untyped(),
            };
            ty = rigor_types::Algebra::join(interner, ty, member);
        }
        ty
    }

    /// Ordered-children descent ([`Self::flow_children`]): apply each child's
    /// contained effects in evaluation order until the child holding `site`
    /// is entered. A `Barrier` child is a scope boundary — it gets the flat
    /// env, which for a block body is the `ScopedEnv::at` answer from before
    /// this pass.
    // too_many_arguments: same replay context as `entry_descend`; a bundle
    // struct would obscure the recursion.
    #[allow(clippy::too_many_arguments)]
    fn entry_children(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        site: rigor_parse::Span,
        uncond: bool,
        preserve_env: bool,
        env: &mut TypeEnv,
        flow: &CheckFlow,
        interner: &mut Interner,
    ) {
        for (child, edge) in self.flow_children(ast.get(id)) {
            let cspan = ast.get(child).span();
            if cspan.0 <= site.0 && site.0 < cspan.1 {
                if edge == FlowEdge::Barrier {
                    // A literal block body's scope is the env at the call
                    // with the body's own effects applied conservatively
                    // (`closure_entry`, issue #473) — skipped when the
                    // descent came from a non-descending container, whose
                    // flat env already stands (`preserve_env`, rigor-rs#380).
                    // Then the body's own evaluated attribute writes drop
                    // their indexed narrowings positionally inside it
                    // (rigor-rs#366).
                    if !preserve_env {
                        self.closure_entry(ast, id, site, env, flow, interner);
                    }
                    if matches!(ast.get(id), Node::Call { .. }) {
                        self.closure_descend(ast, id, site, env, flow, id);
                    }
                } else {
                    self.entry_descend(
                        ast,
                        child,
                        site,
                        uncond && edge == FlowEdge::Uncond,
                        preserve_env,
                        env,
                        flow,
                        interner,
                    );
                }
                return;
            }
            if cspan.1 <= site.0 && edge != FlowEdge::Barrier {
                // An earlier statement in the same sequential scope ran
                // before `site` on the oracle — replay it with real
                // bindings (`begin;1;rescue;x=2;x.w` and `if c; else;
                // x=2; x.w; end` both fire `for 2`), the way
                // `bind_check_statement` already does for the toplevel
                // env, rather than the flat untyped subtree replay
                // (rigor-rs#362 review).
                self.bind_check_statement(
                    ast,
                    child,
                    env, &flow.effects,
                    &flow.indexed,
                    &flow.dead,
                    interner,
                );
            }
        }
    }

    /// The env a site inside the literal block / lambda `closure` reads:
    /// the env replayed up to the closure (`env`, already in place), with
    /// every rebind and mutation the closure's own subtree holds applied
    /// conservatively — the body may run more than once, so a captured
    /// local it rebinds is widened as the flat env widened it.
    ///
    /// Issue #473: this used to be the end-of-FILE flat env, so a later
    /// top-level write leaked in (`y = "s"; [1].each { y.upcase }; y = 1`
    /// fired `for 1`). The reference types the body from the scope at the
    /// closure (silent there, and `y = 1; [1].each { y.upcase }; y = "s"`
    /// fires `for 1`); a write after the closure never reaches it, even for
    /// a lambda called after the write.
    fn closure_entry(
        &self,
        ast: &LoweredAst,
        closure: NodeId,
        site: rigor_parse::Span,
        env: &mut TypeEnv,
        flow: &CheckFlow,
        interner: &mut Interner,
    ) {
        self.apply_subtree_effects(ast, closure, site, false, env, flow, interner);
    }

    /// After a non-descending container's flat
    /// [`Self::apply_subtree_effects`], descend into the one child holding
    /// `site` so a deferred body nested under it still reaches the
    /// `FlowEdge::Barrier` handling that replays its `closure_mutations`
    /// (rigor-rs#380). No child containing the site — or a descent that
    /// never crosses a barrier — leaves the flat env as it stands.
    fn entry_descend_site_child(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        site: rigor_parse::Span,
        env: &mut TypeEnv,
        flow: &CheckFlow,
        interner: &mut Interner,
    ) {
        for (child, _edge) in self.flow_children(ast.get(id)) {
            let cspan = ast.get(child).span();
            if cspan.0 <= site.0 && site.0 < cspan.1 {
                // `preserve_env`: the container's flat pass already built
                // this position's env; a barrier capture would replace it
                // with the end-of-file `flow.env`.
                self.entry_descend(ast, child, site, false, true, env, flow, interner);
                return;
            }
        }
    }

    /// The value a [`DeadOp`] subject local reads inside a folded-dead arm —
    /// the reference's `propagate` edge scope (`dead.rs` has the shapes). An
    /// unbound local reads `untyped` (the reference's `dynamic_top`); a
    /// carrier member the op cannot decide keeps its env value rather than
    /// invent `Bot` where the oracle does not narrow.
    fn dead_subject_type(
        &self,
        ast: &LoweredAst,
        ty: TypeId,
        op: &DeadOp,
        interner: &mut Interner,
    ) -> TypeId {
        match op {
            // A bare operand keeps the edge-consistent members only —
            // `q = nil; if q; q.w` reads `Bot`, but `a = "x"; …; if a && b`
            // keeps `a` `"x"` (its truthy part is nonempty).
            DeadOp::Bare(edge) => {
                let edge = *edge;
                self.narrow_type_members(
                    ty,
                    |t| match t {
                        Type::Constant(s) => {
                            matches!(s, Scalar::Nil | Scalar::Bool(false)) != edge
                        }
                        // `Dynamic`/`Top` may hold either and keep both
                        // ways (the decline side); `Bottom` stays `Bottom`.
                        Type::Top | Type::Dynamic(_) | Type::Bottom => true,
                        // Every other carrier (`Nominal`, `Singleton`,
                        // shapes, `Void`, …) is non-`nil`/`false`.
                        _ => edge,
                    },
                    interner,
                )
            }
            // `q == p` on the arm's edge: the local keeps iff its pin
            // compares as the edge requires. An unpinned peer declines —
            // the fold that made the arm dead came from a sibling operand.
            DeadOp::Cmp { peer, eq } => {
                let eq = *eq;
                match self.expr_scalar(ast, *peer, &mut Vec::new()) {
                    Some(lit) => self.narrow_type_members(
                        ty,
                        |t| match t {
                            Type::Constant(s) => (*s == lit) == eq,
                            _ => true,
                        },
                        interner,
                    ),
                    None => ty,
                }
            }
            DeadOp::NilQ(want) => {
                let want = *want;
                self.narrow_type_members(
                    ty,
                    |t| match t {
                        Type::Constant(s) => matches!(s, Scalar::Nil) == want,
                        _ => true,
                    },
                    interner,
                )
            }
            DeadOp::Isa {
                class,
                exact,
                holds,
            } => {
                let (class, exact, holds) = (class.as_str(), *exact, *holds);
                self.narrow_type_members(
                    ty,
                    |t| {
                        let own = match t {
                            Type::Constant(s) => Some(crate::folding::scalar_class(s)),
                            Type::Nominal { class: c, .. } => self.index.class_name_for_id(*c),
                            _ => None,
                        };
                        match own {
                            Some(own) => {
                                let is = own == class
                                    || (!exact
                                        && self.index.ancestor_names(own).is_some_and(
                                            |anc| anc.contains(&class),
                                        ));
                                is == holds
                            }
                            // `Dynamic`/shapes/etc — the class test is
                            // undecidable; keep the member.
                            None => true,
                        }
                    },
                    interner,
                )
            }
        }
    }

    /// `keep` a type's members individually — a `Union` narrows memberwise
    /// and collapses to `Bottom` when none survive; a lone carrier keeps or
    /// empties as a whole. Shared by [`Self::dead_subject_type`]'s edge
    /// narrowing.
    fn narrow_type_members(
        &self,
        ty: TypeId,
        keep: impl Fn(&Type) -> bool,
        interner: &mut Interner,
    ) -> TypeId {
        match interner.get(ty) {
            Type::Union(members) => {
                let members = members.clone();
                let kept: Vec<TypeId> = members
                    .into_iter()
                    .filter(|m| keep(interner.get(*m)))
                    .collect();
                match kept.as_slice() {
                    [] => interner.bottom(),
                    [only] => *only,
                    _ => interner.intern(Type::Union(kept)),
                }
            }
            t => {
                if keep(t) {
                    ty
                } else {
                    interner.bottom()
                }
            }
        }
    }

    /// Apply every recorded effect inside `id`'s span to `env`: a rebind
    /// always widens `Dynamic` (the flat env's own envelope); a mutation
    /// mints the unconditional nominal only when it sits on an
    /// all-`Uncond` descent from `id` — a conditional position widens
    /// `Dynamic`, byte-identical to the flat env.
    #[allow(clippy::too_many_arguments)]
    fn apply_subtree_effects(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        site: rigor_parse::Span,
        uncond: bool,
        env: &mut TypeEnv,
        flow: &CheckFlow,
        interner: &mut Interner,
    ) {
        let span = ast.get(id).span();
        for (wspan, name) in &flow.effects.rebinds {
            if wspan.0 >= span.0
                && wspan.1 <= span.1
                && flow.dead.write_reaches(*wspan, site)
            {
                let u = interner.untyped();
                env.insert(Rc::from(name.as_str()), u);
                drop_indexed_narrowings(env, name);
            }
        }
        for (wspan, name, method, drop_key) in &flow.effects.mutations {
            if wspan.0 >= span.0
                && wspan.1 <= span.1
                && flow
                    .dead
                    .mutation_reaches(*wspan, site, is_content_mutator(method))
            {
                self.apply_mutation_effects(
                    ast,
                    &flow.indexed,
                    id,
                    uncond,
                    *wspan,
                    name,
                    method,
                    drop_key.as_ref(),
                    env,
                    interner,
                );
            }
        }
        for m in &flow.indexed.slot_mutations {
            if m.span.0 >= span.0
                && m.span.1 <= span.1
                && flow.dead.write_reaches(m.span, site)
                // A slot mutation inside a block body's `break` argument
                // is a scope effect on a locals-only join — its widening
                // lands nowhere, not even inside the argument
                // (rigor-rs#407).
                && !ast.in_block_break_locals_only_carrier(m.span)
            {
                self.apply_slot_mutation(ast, &flow.indexed, id, m, env, interner);
            }
        }
        // Upstream #2677 — mutations the named tables filter out (a
        // block-local `b << "s"` whose `b` was bound to `a`'s object, a
        // `def`-internal mutator) still reach the shared object: floor
        // every copy-linked local they cover. Toplevel mutations appear
        // here too, so a floor can run twice — `widen_mutated_binding`
        // declines an already-widened carrier, keeping it idempotent.
        for am in &flow.indexed.alias_mutations {
            if am.span.0 >= span.0
                && am.span.1 <= span.1
                && flow.dead.write_reaches(am.span, site)
                && !ast.in_block_break_locals_only_carrier(am.span)
            {
                self.floor_alias_linked(
                    ast,
                    &flow.indexed,
                    &am.name,
                    &am.method,
                    am.span,
                    am.part_only,
                    env,
                    interner,
                );
            }
        }
    }

    /// Replay `owner`'s own [`closure_mutations`](crate::closure_mutations)
    /// into `env` in the deferred body's evaluation order until `site` — the
    /// block-scope `widen_attribute_write` (`x.each { h.default ||= 0;
    /// h[:a].m }` drops `h`'s indexed narrowings for the in-body reads that
    /// follow it) the flat env cannot see, while the outer scope's records
    /// survive (rigor-rs#366). Called at the `FlowEdge::Barrier` boundary —
    /// `id` is the `Node::Call`/`Node::Lambda` whose body holds the site and
    /// IS `owner`; recursion into a nested barrier re-keys `owner` to the
    /// inner body, and a body-owning `id`'s earlier Barrier-edged siblings
    /// (its own body statements) apply under `id` as well — their writes
    /// key there, not to `owner` (rigor-rs#379).
    ///
    /// The ordering is EVALUATION order through `flow_children`, not source
    /// order — `h[:a].m if h.default ||= 0` inside a body evaluates the
    /// predicate first, so the write still lands before the read's scope.
    /// A write in a subtree that completes before the site drops outright:
    /// the reference joins each conditional arm's post-scope and a record
    /// absent on any path is gone. A write in a SIBLING conditional arm —
    /// the other `if`/`case`/`when` arm, a `rescue` clause beside the
    /// site's, a recovery carrier's other children — evaluates on a path
    /// that never reaches the site's scope, so [`Self::same_cond_path`]
    /// declines it.
    fn closure_descend(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        site: rigor_parse::Span,
        env: &mut TypeEnv,
        flow: &CheckFlow,
        owner: NodeId,
    ) {
        let node = ast.get(id);
        let children = self.flow_children(node);
        // Whether `id` itself owns a deferred body — a literal block or
        // lambda — whose writes `closure_mutations` keys to `id`. Its
        // Barrier-edged children are that body's own statements: an
        // earlier one must apply under `id`, not the inherited `owner`
        // (rigor-rs#379 — `[1].each { [2].each { h.a ||= 0; h[:b].m } }`
        // applies the write under the inner `each`, one level late
        // otherwise).
        let body_owning = owns_deferred_body(node);
        for (i, &(child, edge)) in children.iter().enumerate() {
            let cspan = ast.get(child).span();
            if cspan.0 <= site.0 && site.0 < cspan.1 {
                for &(sib, sedge) in &children[..i] {
                    if sedge == FlowEdge::Uncond
                        || self.same_cond_path(node, sib, child)
                    {
                        let sib_owner = if sedge == FlowEdge::Barrier && body_owning {
                            id
                        } else {
                            owner
                        };
                        self.apply_closure_mutations(ast, sib, site, env, flow, sib_owner);
                    }
                }
                if edge == FlowEdge::Barrier {
                    // A nested literal block/lambda: its own writes belong
                    // to ITS body — `id` becomes the owner. Any other
                    // barrier child (`def`/`class`/`module` — a fresh local
                    // scope) is still descended into, but nothing inside it
                    // keys to `id` (its writes key to `None` or an inner
                    // body), so no mutation replays there.
                    if body_owning {
                        self.closure_descend(ast, child, site, env, flow, id);
                    }
                    return;
                }
                self.closure_descend(ast, child, site, env, flow, owner);
                return;
            }
        }
    }

    /// Whether `earlier` and `later` — both `Cond`-edged children of
    /// `parent` — lie on one evaluation path, so `earlier`'s post-scope
    /// reaches `later`'s entry. True except across branch alternatives:
    /// an `if`'s two arms, a `case`'s `when`/`else` arms, a `begin`'s
    /// protected body versus its `rescue` clauses, and a recovery
    /// carrier's children each run exclusively — a write in one never
    /// lands in another's scope. An `ensure` body runs after whichever
    /// path completed, on the joined scope — a record absent on any path
    /// is gone — so every earlier child reaches it.
    fn same_cond_path(&self, parent: &Node, earlier: NodeId, later: NodeId) -> bool {
        match parent {
            Node::If {
                then_body,
                else_body,
                ..
            } => !(then_body.contains(&earlier) && else_body.contains(&later)),
            Node::Case {
                branches,
                else_body,
                ..
            } => {
                // Each `when` arm is its own path, as is the `else` arm;
                // only statements sharing `else_body` stay on one path.
                let earlier_else = else_body.contains(&earlier);
                let later_else = else_body.contains(&later);
                earlier_else && later_else
                    || !(earlier_else
                        || later_else
                        || branches.contains(&earlier)
                        || branches.contains(&later))
            }
            Node::BeginRescue {
                ensure_body,
                clauses,
                ..
            } => {
                if ensure_body.contains(&later) {
                    return true;
                }
                // The protected body and the `else` arm share the
                // normal-completion path; each `rescue` clause is an
                // alternative to them and to each other.
                let clause = |c: NodeId| {
                    clauses
                        .iter()
                        .position(|cl| cl.body.contains(&c))
                };
                clause(earlier) == clause(later)
            }
            // A recovery carrier's (`Recovered`/`Jump`/`Inert`) children are
            // mutually conditional — none's effects reach a sibling.
            Node::Statements { kind, .. } if !matches!(kind, StatementsKind::Sequence) => false,
            // Sequential: a `while`/`for`/`when` predicate or condition
            // before its body, an operand list, `&&`'s left — and every
            // other shape carries `Uncond` children only.
            _ => true,
        }
    }

    /// Every `owner`-keyed attribute write inside `id`'s span drops its
    /// receiver's indexed narrowings — the block-scope sibling of
    /// [`Self::apply_subtree_effects`]' mutation half. The write drops
    /// unconditionally on the post-subtree scope: the reference joins every
    /// arm and a record absent on any path is gone. `method` is not carried
    /// — every collected writer is a shape mutator, never `[]=`, so the
    /// drop is always the receiver's whole record set. A write inside an
    /// evaluated-but-join-dropped position still must reach `site`
    /// positionally (rigor-rs#362 — `each { if d; w; return; end; r }`
    /// keeps `h`'s record for the post-`if` in-body read).
    fn apply_closure_mutations(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        site: rigor_parse::Span,
        env: &mut TypeEnv,
        flow: &CheckFlow,
        owner: NodeId,
    ) {
        let span = ast.get(id).span();
        for (o, wspan, name) in &flow.closure_mutations {
            if *o == owner
                && wspan.0 >= span.0
                && wspan.1 <= span.1
                && flow.dead.write_reaches(*wspan, site)
                // A block body's `break` argument lands on the locals-only
                // break join — its drop applies nowhere (rigor-rs#363),
                // and on a block break EVERY scope effect is dropped
                // (rigor-rs#407), adder widens included.
                && !(ast.in_block_break_locals_only_carrier(*wspan)
                    || (ast.in_locals_only_carrier(*wspan)
                        && ast.iter().any(|(_, n)| {
                            matches!(n, Node::AttrWrite { span: s, .. } if s == wspan)
                        })))
            {
                drop_indexed_narrowings(env, name);
            }
        }
    }

    /// One mutation's env effects — the receiver's binding AND the indexed
    /// narrowings (`rigor-rs#325`). Three stages, in the reference's order:
    ///
    /// 1. `IndexedNarrowing.invalidate_after_call`: a stable-key `local[k]
    ///    = v` `[]=` drops that one slot's record (`drop_key`) — the index
    ///    targets (`h[k], z = …`, `for h[k] in xs`, `rescue => h[k]`) carry
    ///    the same key since `widen_index_target` runs
    ///    `invalidate_indexed_write` on them (rigor-rs#342) — every other
    ///    shape mutator drops every record rooted at the receiver
    ///    (`invalidate_mutator`), and a compound `h[k] op= v` or a
    ///    non-literal / non-local index target — `method == "[]="` with
    ///    `drop_key == None` — drops none (those never reach
    ///    `invalidate_indexed_write`).
    /// 2. `index_write_stored_type` on the env the write was ENTERED with —
    ///    an `operand`-flagged `h[k] ||= v` — computed BEFORE the `[]=`
    ///    widening lands, exactly as `eval_index_or_write` does.
    /// 3. The binding itself: an unconditional mutation mints the widened
    ///    nominal (`widen_mutated_binding`), a conditional one widens to
    ///    `Dynamic`; an `operand` write additionally passes
    ///    `path_operand_evaluated`, so a `puts(*[h[k] ||= v])` mints `h`'s
    ///    nominal carrier instead of `Dynamic` (`IndexWriteWidening` widens
    ///    it unconditionally there).
    // too_many_arguments: shared replay context — a bundle struct would obscure.
    #[allow(clippy::too_many_arguments)]
    fn apply_mutation_effects(
        &self,
        ast: &LoweredAst,
        indexed: &IndexedFlow,
        root: NodeId,
        uncond: bool,
        wspan: rigor_parse::Span,
        name: &str,
        method: &str,
        drop_key: Option<&ShapeKey>,
        env: &mut TypeEnv,
        interner: &mut Interner,
    ) {
        // `break`'s argument scope lands on `join_break_scopes` — a
        // LOCALS-only union — so an indexed-record drop there (a scope
        // effect, not a binding) never propagates, not even to reads
        // inside the argument (rigor-rs#363). The exception is the
        // CONTENT_MUTATORS scan set (`[]=`/`store`/`replace`/`<<` … plus
        // the String-only mutators `upcase!`/`sub!`/…) under an iterated
        // body: `loop_content_writeback` text-scans it and re-joins the
        // receiver binding, so the read declines anyway — the flat env
        // cannot express "record present, read declined", so the port
        // keeps the drop.
        // `break h[:a] = 9` under `while` stays silent; under a bare
        // `for` (no writeback) or any non-scanned mutator — `break
        // h.merge!(…)`, `break a.pop` — the record survives and fires,
        // exactly as the oracle reports.
        if !crate::flow_writes::break_join_suppresses(ast, wspan, method) {
            self.drop_indexed_mutation(name, method, drop_key, env);
        }
        // A `[]=` replayed through a content write-back boundary drops
        // EVERY narrowing rooted at the receiver, whatever `drop_key`
        // says: `content_writeback_block_captures` /
        // `loop_content_writeback` rebind the captured local on those
        // paths, and the rebind invalidates its records — the compound
        // `h[k] op= v` a `drop_key == None` entry otherwise keeps alive
        // is exactly the stale record of rigor-rs#388. A `for` `break`
        // argument is the exception: its scope lands on the locals-only
        // join, so a nested block's captured `[]=` lands nowhere
        // (`for i in xs; break (x.each { h[:a] = 9 }); end` keeps the
        // record — rigor-rs#363 review).
        // rigor-rs#658 — the three `path_*` predicates answer from ONE DFS's
        // marks rather than re-walking the shared path prefix per mutation
        // (a `local.<mutator>` chain went cubic); a `wspan` no node carries
        // falls back to the recursive walk.
        let marks = self.path_marks(ast, root);
        if method == "[]="
            && !ast.in_bare_locals_only_carrier(wspan)
            && marks
                .writeback(wspan)
                .unwrap_or_else(|| self.path_content_writeback(ast, root, wspan))
        {
            drop_indexed_narrowings(env, name);
        }
        let evaluated = uncond
            && (marks
                .uncond(wspan)
                .unwrap_or_else(|| self.path_unconditional(ast, root, wspan))
                || (indexed.operand_spans.contains(&wspan)
                    && marks
                        .operand(wspan)
                        .unwrap_or_else(|| self.path_operand_evaluated(ast, root, wspan))));
        // The `h[k] ||= v` record computes on the PRE-widening env
        // (`eval_index_or_write`: `index_write_stored_type(node, scope)`
        // before `IndexWriteWidening.widen`, then `with_indexed_narrowing`).
        let stored = if evaluated {
            indexed
                .slot_writes
                .iter()
                .find(|w| w.span == wspan)
                .and_then(|w| {
                    self.slot_stored_type(ast, w, env, interner)
                        .map(|ty| (indexed_narrowing_key(&w.name, &w.key), ty))
                })
        } else {
            None
        };
        // The receiver-binding widen is a scope effect too: inside a
        // locals-only `break` argument it lands nowhere for a
        // NON-scanned mutator — `break h.clear` leaves `h` at its
        // literal carrier on the oracle, so `h[:a]` still answers `1`
        // (rigor-rs#363 review) — and under a bare `for` at all. A
        // SCANNED mutator's writeback DOES rejoin the receiver to a
        // different carrier (`Hash[:a | Symbol, 1 | 9]`), so its widen
        // must still land: keeping the `{a: 1}` literal there would let
        // a post-loop `h[:a]` read serve stale `1` evidence the oracle
        // does not emit.
        if MUTATOR_METHODS.contains(&method)
            && !crate::flow_writes::break_join_suppresses(ast, wspan, method)
        {
            if let Some(&pre) = env.get(name) {
                if evaluated {
                    if let Some(widened) = self.widen_mutated_binding(pre, method, interner) {
                        env.insert(Rc::from(name), widened);
                    }
                } else {
                    env.insert(Rc::from(name), interner.untyped());
                }
            }
        }
        self.floor_alias_linked(ast, indexed, name, method, wspan, false, env, interner);
        if let Some((key, ty)) = stored {
            env.insert(Rc::from(key.as_str()), ty);
        }
    }

    /// Upstream #2677 (`AliasMutationFloor.floor_after_call`) — a mutation
    /// through `name` may reach the object another local's copy `x = name`
    /// (or `name = x`) bound, so the copy's literal carrier goes stale the
    /// same way. The floor lands once the copy site may have run: earlier
    /// in the body, or anywhere inside an iterated body the mutation also
    /// sits in (the copy may have run on an earlier pass). A binding the
    /// widening declines keeps its type, exactly as upstream floors only
    /// literal shapes. `part_only` marks a mutation that reached `name`'s
    /// ELEMENT — `f[0] << 2`, never `name`'s own object — so the copy is
    /// floored only when its binding can hold a mutable element
    /// (`mutable_parts`, upstream's `kind == PART` gate): `z = w` where
    /// `w = [1]` keeps `Tuple[1]` under `w[0] << 2` — `1` is immutable —
    /// while `g = f` where `f = [[1]]` floors. The port's slice sees copy
    /// links only — element, ivar, argument and foreign-locals links are
    /// not modelled.
    #[allow(clippy::too_many_arguments)]
    fn floor_alias_linked(
        &self,
        ast: &LoweredAst,
        indexed: &IndexedFlow,
        name: &str,
        method: &str,
        wspan: rigor_parse::Span,
        part_only: bool,
        env: &mut TypeEnv,
        interner: &mut Interner,
    ) {
        // A compound index write (`h[k] ||= v` / `&&=` / `op=`) is not an
        // upstream floor trigger — only `CallNode` shape mutators and the
        // plain `=` store are.
        if indexed.non_floorable.contains(&wspan) {
            return;
        }
        // The reach closes over chained copies (upstream
        // `AliasMutationFloor.reach`): `a = b; b = c; c << x` floors `a`
        // and `b`. Each link's own site decides whether it may have run
        // before this mutation.
        let mut queue: Vec<String> = vec![name.to_string()];
        let mut seen: std::collections::HashSet<String> =
            [name.to_string()].into_iter().collect();
        let mut i = 0;
        while i < queue.len() {
            let cur = queue[i].clone();
            i += 1;
            for (lspan, bound, source) in &indexed.alias_links {
                let other = if *bound == cur {
                    source.as_str()
                } else if *source == cur {
                    bound.as_str()
                } else {
                    continue;
                };
                if seen.contains(other)
                    || !crate::flow_writes::alias_floor_reaches(ast, indexed, *lspan, wspan)
                {
                    continue;
                }
                seen.insert(other.to_string());
                queue.push(other.to_string());
            }
        }
        for other in queue.iter().skip(1) {
            if let Some(&pre) = env.get(other.as_str()) {
                if part_only && !self.mutable_parts(pre, interner) {
                    continue;
                }
                if let Some(widened) = self.widen_mutated_binding(pre, method, interner) {
                    if widened != pre {
                        env.insert(Rc::from(other.as_str()), widened);
                    }
                }
            }
            crate::flow_writes::drop_indexed_narrowings(env, other);
        }
    }

    /// Upstream `AliasMutationFloor.mutable_parts?` — whether `ty`'s
    /// binding can hold a mutable element: any collection member that is a
    /// container (Tuple, HashShape, literal or nominal `String`/`Array`/
    /// `Hash`/`Set`) or an untyped member. A `Tuple[1]` whose elements are
    /// all immutable scalars answers false.
    fn mutable_parts(&self, ty: TypeId, interner: &mut Interner) -> bool {
        self.coll_value_members(interner, ty)
            .iter()
            .any(|&m| self.mutable_member(m, interner))
    }

    fn mutable_member(&self, m: TypeId, interner: &mut Interner) -> bool {
        match interner.get(m) {
            Type::Tuple(_) | Type::HashShape(_) | Type::DataInstance { .. } => true,
            // A String literal is mutable; every other literal scalar
            // (Integer, Symbol, nil, true/false) is not.
            Type::Constant(rigor_types::Scalar::Str(_)) => true,
            Type::Constant(_) | Type::IntegerRange { .. } | Type::Bottom | Type::Void => {
                false
            }
            Type::Nominal { class, .. } => matches!(
                self.index.class_name_for_id(*class),
                Some("Array" | "Hash" | "Set" | "String")
            ),
            Type::Union(members) => {
                let members = members.clone();
                members
                    .iter()
                    .any(|&m| self.mutable_member(m, interner))
            }
            // An untyped or Dynamic member may hold anything — floor it.
            _ => true,
        }
    }

    /// `IndexedNarrowing.widen_mutated_slot` (`indexed_narrowing.rb:166`): a
    /// mutator call whose receiver IS a recorded slot's element — `h[k] <<
    /// x`, `h[k][j] = v`, `(h[k] ||= []) << x` — widens the recorded value
    /// as the mutator widens it, or drops the record when the widening
    /// declines. A call that may not have evaluated drops the record
    /// instead — the reference's join diverges the narrowing away.
    fn apply_slot_mutation(
        &self,
        ast: &LoweredAst,
        indexed: &IndexedFlow,
        root: NodeId,
        m: &crate::flow_writes::SlotMutation,
        env: &mut TypeEnv,
        interner: &mut Interner,
    ) {
        // A slot mutation inside a lexical `break` argument lands on the
        // locals-only break join — its record widening or drop never
        // propagates: `break h[:a] << 2` keeps `h[:a]`'s `"s" | 1`
        // (rigor-rs#363).
        let lookup = indexed_narrowing_key(&m.name, &m.key);
        let recorded = env.get(lookup.as_str()).copied();
        // Upstream #2677: a mutator on `name`'s element mutates the shared
        // object a copy `x = name` also holds — the copy's whole carrier
        // goes stale, not just the touched slot — but only when the copy
        // can hold a mutable element (`part_only`: `w = [1]` under
        // `w[0] << 2` does not stale `z = w`, whose `1`s are immutable).
        // Floored even when `name` itself has no slot record:
        // `f = [[1]]; g = f; f[0] << 2` stales `g` although `f[0]`
        // recorded nothing.
        self.floor_alias_linked(ast, indexed, &m.name, &m.method, m.span, true, env, interner);
        let Some(recorded) = recorded else {
            return;
        };
        // Element mutators live in ordinary expression positions (argument
        // lists, splats), so the lenient operand path — not the strict
        // carrier mint — decides whether the call ran.
        if !self
            .path_marks(ast, root)
            .operand(m.span)
            .unwrap_or_else(|| self.path_operand_evaluated(ast, root, m.span))
        {
            env.remove(lookup.as_str());
            return;
        }
        let widened = self
            .widen_mutated_binding(recorded, &m.method, interner)
            .or_else(|| self.string_slot_floor(recorded, &m.method, interner));
        match widened {
            Some(ty) => {
                env.insert(Rc::from(lookup.as_str()), ty);
            }
            None => {
                env.remove(lookup.as_str());
            }
        }
    }

    /// [`PathMarks`] for `root`, through the small LRU on the typer: one DFS
    /// over `root`'s subtree recording the flag each of the three
    /// `path_*` walks would derive edge-by-edge — the same total ONE deep
    /// walk spends, then shared by every contained mutation (rigor-rs#658:
    /// the walks' shared prefixes made a `v.push(1)` × N chain cubic).
    fn path_marks(&self, ast: &LoweredAst, root: NodeId) -> std::rc::Rc<PathMarks> {
        let key = (ast.uid() as usize, root);
        {
            let mut cache = self.path_marks.borrow_mut();
            if let Some(pos) = cache.iter().position(|&(k, _)| k == key) {
                let entry = cache.remove(pos);
                let marks = entry.1.clone();
                cache.push(entry);
                return marks;
            }
        }
        let mut marks = PathMarks::default();
        // `window` is the intersection of the node's ancestors' spans: a
        // node is marked only when its own span nests inside every
        // ancestor, mirroring the recursive walks, which only descend a
        // child whose span CONTAINS `wspan`. Most Prism nodes nest, but a
        // heredoc's interpolated parts sit in the body while the
        // `InterpolatedString` node's span is the `<<~T` opener — the
        // parts escape, get no mark, and the callers' fallback then
        // reproduces the recursion's off-child answer verbatim
        // (rigor-rs#658 heredoc row: `if … #{v.push(1)} … end` used to
        // drop the mutation where the recursion minted `for Integer`).
        let mut stack = vec![(
            root,
            PathFlags { uncond: true, operand: true, writeback: false },
            ast.get(root).span(),
        )];
        while let Some((id, flags, window)) = stack.pop() {
            let node = ast.get(id);
            let nspan = node.span();
            // Ancestors mark before their descendants exist on the stack,
            // so `or_insert` keeps the OUTERMOST node's flags on a shared
            // span — exactly as the recursive walks stop at the first node
            // whose span is `wspan`. A node whose span escapes an
            // ancestor's gets NO mark — the fallback answers it.
            if nspan.0 >= window.0 && nspan.1 <= window.1 {
                marks.by_span.entry(nspan).or_insert(flags);
            }
            let child_window = (
                window.0.max(nspan.0),
                window.1.min(nspan.1),
            );
            let lenient = matches!(
                node,
                Node::Statements {
                    kind: StatementsKind::Recovered | StatementsKind::Jump(_),
                    ..
                }
            );
            for (child, edge) in self.flow_children(node) {
                // `path_content_writeback`'s edge test, verbatim: a
                // `Barrier`, or a body child of a `while`/`until` loop (a
                // `for` — no `predicate` — joins rather than writing back,
                // and an index-target `for` misreads as `while` without
                // the empty-index gate).
                let writeback = flags.writeback
                    || edge == FlowEdge::Barrier
                    || matches!(
                        node,
                        Node::Loop {
                            predicate: Some(_),
                            index,
                            index_writes,
                            body,
                            ..
                        } if body.contains(&child)
                            && index.is_empty()
                            && index_writes.is_empty()
                    );
                stack.push((
                    child,
                    PathFlags {
                        uncond: flags.uncond && edge == FlowEdge::Uncond,
                        operand: flags.operand && (edge == FlowEdge::Uncond || lenient),
                        writeback,
                    },
                    child_window,
                ));
            }
        }
        let marks = std::rc::Rc::new(marks);
        let mut cache = self.path_marks.borrow_mut();
        if cache.len() >= 8 {
            cache.remove(0);
        }
        cache.push((key, marks.clone()));
        marks
    }

    /// Whether `wspan`'s position inside `id` sits on an all-`Uncond` descent
    /// — `f(b.unshift("s"), …)` evaluates it before the call's dispatch (yes)
    /// while `x && b.unshift("s")` may never reach it (no).
    fn path_unconditional(&self, ast: &LoweredAst, id: NodeId, wspan: rigor_parse::Span) -> bool {
        let node = ast.get(id);
        if node.span() == wspan {
            return true;
        }
        for (child, edge) in self.flow_children(node) {
            let cspan = ast.get(child).span();
            if cspan.0 <= wspan.0 && wspan.1 <= cspan.1 {
                return edge == FlowEdge::Uncond && self.path_unconditional(ast, child, wspan);
            }
        }
        // `wspan` sits inside `id` but inside no linked child — it rides
        // `id`'s own position, UNLESS the position is one the node keeps off
        // its child list and evaluates conditionally (rigor-rs#306):
        // - a `for` index target: `for h[:k] in xs` stores each element
        //   through `[]=` on `h` per iteration (`bind_for_index`), and the
        //   loop may never run — the post-loop scope joins the zero-iteration
        //   binding (`eval_for`'s `join_with_nil_injection`), so the store is
        //   conditional;
        // - a `rescue` clause's header: the exception list and the `=>`
        //   target run only when the clause fires
        //   (`bind_rescue_reference` binds inside the clause's edge). The
        //   clause's own span covers both, and its body statements already
        //   declined through their `Cond` edges above.
        match node {
            Node::Loop { index_writes, .. }
                if index_writes
                    .iter()
                    .any(|(_, s, _)| s.0 <= wspan.0 && wspan.1 <= s.1) =>
            {
                return false;
            }
            Node::BeginRescue { clauses, .. }
                if clauses
                    .iter()
                    .any(|c| c.span.0 <= wspan.0 && wspan.1 <= c.span.1) =>
            {
                return false;
            }
            _ => {}
        }
        true
    }

    /// Whether `wspan`'s position inside `id` descends through a content
    /// write-back boundary — a literal block/lambda body
    /// ([`FlowEdge::Barrier`]) or a `while`/`until` body. The reference
    /// rebinds the captured receiver on those paths
    /// (`content_writeback_block_captures` for the non-escaping-block call,
    /// `loop_content_writeback` for the iterated body), and the rebind
    /// drops every indexed narrowing rooted at it (rigor-rs#388). A
    /// `for` body (`predicate` `None`) joins scopes rather than writing
    /// back — the record an arm's own eval re-recorded survives the join,
    /// the same way an `if`/`case`/`rescue` arm keeps it.
    fn path_content_writeback(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        wspan: rigor_parse::Span,
    ) -> bool {
        let node = ast.get(id);
        if node.span() == wspan {
            return false;
        }
        for (child, edge) in self.flow_children(node) {
            let cspan = ast.get(child).span();
            if cspan.0 <= wspan.0 && wspan.1 <= cspan.1 {
                // A `for` body JOINS its scope rather than writing back
                // (`eval_for` has no `loop_content_writeback` — the arm's
                // own eval re-records the slot), so only `while`/`until`
                // count: `for` lowers its collection into `predicate` too,
                // and a `for` whose target binds no local and stores no
                // index (`for @a in xs`) misreads as `while` — a decline
                // toward silence, never a new fire.
                let writeback = edge == FlowEdge::Barrier
                    || matches!(
                        node,
                        Node::Loop {
                            predicate: Some(_),
                            index,
                            index_writes,
                            body,
                            ..
                        } if body.contains(&child)
                            && index.is_empty()
                            && index_writes.is_empty()
                    );
                return writeback || self.path_content_writeback(ast, child, wspan);
            }
        }
        false
    }

    /// The lenient sibling of [`Self::path_unconditional`] for a recovery-
    /// flagged `operand` `IndexWrite` (rigor-rs#325): the recovery collect
    /// already decided the write evaluates inline — its `Cond` edges ride a
    /// `StatementsKind::Recovered`/`Jump` carrier that flattened an arbitrary
    /// expression shape (a splat argument, a `return` operand, a `p(...)`
    /// argument list), not a real branch. An edge out of such a carrier
    /// counts as `Uncond` here; every other `Cond` — `if` arms, `&&`/`||`
    /// operands, `begin` bodies, loop bodies — still declines, so a
    /// `narrows`-flagged write records only where its stored slot can
    /// actually reach a read.
    fn path_operand_evaluated(&self, ast: &LoweredAst, id: NodeId, span: rigor_parse::Span) -> bool {
        let node = ast.get(id);
        if node.span() == span {
            return true;
        }
        // A `Recovered` / `Jump` carrier flattens evaluated operands behind
        // `Cond` edges — treated as `Uncond` here, never the other kinds
        // (`Inert` carriers hold no flagged writes at all).
        let lenient = matches!(
            node,
            Node::Statements {
                kind: StatementsKind::Recovered | StatementsKind::Jump(_),
                ..
            }
        );
        for (child, edge) in self.flow_children(node) {
            let cspan = ast.get(child).span();
            if cspan.0 <= span.0 && span.1 <= cspan.1 {
                return (edge == FlowEdge::Uncond || lenient)
                    && self.path_operand_evaluated(ast, child, span);
            }
        }
        // Same off-child-position rule `path_unconditional` applies: a `for`
        // index target or rescue header position never evaluates inline.
        match node {
            Node::Loop { index_writes, .. }
                if index_writes
                    .iter()
                    .any(|(_, s, _)| s.0 <= span.0 && span.1 <= s.1) =>
            {
                return false;
            }
            Node::BeginRescue { clauses, .. }
                if clauses
                    .iter()
                    .any(|c| c.span.0 <= span.0 && span.1 <= c.span.1) =>
            {
                return false;
            }
            _ => {}
        }
        true
    }

    /// `node`'s children in evaluation order, each flagged by the certainty
    /// its position evaluates ([`FlowEdge`]). The order feeds both
    /// [`Self::entry_children`] (positional replay to a site) and
    /// [`Self::path_unconditional`] (is an effect's position unconditional).
    // exhaustive: every variant with value children lists them in eval order.
    #[allow(clippy::too_many_lines)]
    fn flow_children(&self, node: &Node) -> Vec<(NodeId, FlowEdge)> {
        match node {
            Node::Statements {
                body,
                kind: StatementsKind::Sequence,
                ..
            } => body.iter().map(|&c| (c, FlowEdge::Uncond)).collect(),
            Node::Statements { body, .. } => {
                body.iter().map(|&c| (c, FlowEdge::Cond)).collect()
            }
            Node::Call {
                receiver,
                args,
                block_body,
                block_span,
                ..
            } => {
                let mut v: Vec<(NodeId, FlowEdge)> =
                    receiver.iter().map(|&c| (c, FlowEdge::Uncond)).collect();
                v.extend(args.iter().map(|&c| (c, FlowEdge::Uncond)));
                // A literal block body is a scope barrier (it may run more
                // than once, so its own effects widen, `closure_entry`); a `&expr`
                // block-pass is an operand evaluated before dispatch — its
                // `block_span` is `None` by construction.
                let edge = if block_span.is_some() {
                    FlowEdge::Barrier
                } else {
                    FlowEdge::Cond
                };
                v.extend(block_body.iter().map(|&c| (c, edge)));
                v
            }
            Node::If {
                predicate,
                then_body,
                else_body,
                ..
            } => {
                let mut v = vec![(*predicate, FlowEdge::Uncond)];
                v.extend(then_body.iter().map(|&c| (c, FlowEdge::Cond)));
                v.extend(else_body.iter().map(|&c| (c, FlowEdge::Cond)));
                v
            }
            Node::Logical { left, right, .. } => {
                vec![(*left, FlowEdge::Uncond), (*right, FlowEdge::Cond)]
            }
            Node::Case {
                predicate,
                branches,
                else_body,
                ..
            } => {
                let mut v: Vec<(NodeId, FlowEdge)> =
                    predicate.iter().map(|&c| (c, FlowEdge::Uncond)).collect();
                v.extend(branches.iter().map(|&c| (c, FlowEdge::Cond)));
                v.extend(else_body.iter().map(|&c| (c, FlowEdge::Cond)));
                v
            }
            Node::When {
                conditions, body, ..
            } => conditions
                .iter()
                .chain(body.iter())
                .map(|&c| (c, FlowEdge::Cond))
                .collect(),
            Node::Loop {
                predicate, body, ..
            } => predicate
                .iter()
                .chain(body.iter())
                .map(|&c| (c, FlowEdge::Cond))
                .collect(),
            Node::BeginRescue {
                body,
                main_body,
                ensure_body,
                clauses,
                ..
            } => {
                // `main_body` is `Cond`, not `Uncond`: a `rescue` exit can
                // leave a mutation inside it unrun, and the joined
                // post-statement scope still sees the pre-mutation binding
                // (upstream probe: `begin; b.unshift("s"); rescue; nil; end;
                // b.frobnicate` is silent on the reference). Sites inside it
                // still replay in order — the flag only widens to `Dynamic`.
                let mut v: Vec<(NodeId, FlowEdge)> =
                    main_body.iter().map(|&c| (c, FlowEdge::Cond)).collect();
                let mut covered: std::collections::HashSet<NodeId> =
                    main_body.iter().copied().collect();
                covered.extend(ensure_body.iter().copied());
                for clause in clauses {
                    covered.extend(clause.body.iter().copied());
                    v.extend(clause.body.iter().map(|&c| (c, FlowEdge::Cond)));
                }
                v.extend(
                    body.iter()
                        .filter(|c| !covered.contains(c))
                        .map(|&c| (c, FlowEdge::Cond)),
                );
                v.extend(ensure_body.iter().map(|&c| (c, FlowEdge::Cond)));
                v
            }
            Node::LocalVariableWrite { value, .. }
            | Node::LocalVariableOpWrite { value, .. }
            | Node::VariableWrite { value, .. }
            | Node::InstanceVariableWrite { value, .. }
            | Node::ConstantWrite { value, .. } => vec![(*value, FlowEdge::Uncond)],
            Node::MultiWrite {
                value,
                target_exprs,
                ..
            } => {
                let mut v = vec![(*value, FlowEdge::Uncond)];
                v.extend(target_exprs.iter().map(|&c| (c, FlowEdge::Uncond)));
                v
            }
            // A compound index write evaluates receiver, index arguments and
            // value in order (`eval_index_or_write` / `eval_index_write`
            // sub_eval each); its `[]=` widening is applied by the mutation
            // pass, not these edges.
            Node::IndexWrite {
                receiver: Some(r),
                indices,
                value,
                ..
            } => std::iter::once(*r)
                .chain(indices.iter().copied())
                .chain(std::iter::once(*value))
                .map(|c| (c, FlowEdge::Uncond))
                .collect(),
            // `recv.attr op= v` — `eval_attribute_compound_write` reads the
            // receiver unconditionally; the RHS is conditional under
            // `||=`/`&&=` (runs only when `recv.attr`'s truthiness requires)
            // and unconditional under `op=`. `Cond` is the safe join for all
            // three (rigor-rs#343).
            Node::AttrWrite {
                receiver, value, ..
            } => receiver
                .iter()
                .map(|&c| (c, FlowEdge::Uncond))
                .chain(std::iter::once((*value, FlowEdge::Cond)))
                .collect(),
            Node::ArrayLit { elements, .. }
            | Node::HashLit { elements, .. }
            | Node::InterpolatedString {
                parts: elements, ..
            }
            | Node::InterpolatedSymbol {
                parts: elements, ..
            }
            | Node::Return {
                values: elements, ..
            } => elements.iter().map(|&c| (c, FlowEdge::Uncond)).collect(),
            Node::Alias {
                new_name, old_name, ..
            } => vec![
                (*new_name, FlowEdge::Uncond),
                (*old_name, FlowEdge::Uncond),
            ],
            // A range evaluates each bound unconditionally in source order —
            // the reference's `OPERAND_CONTAINERS` includes `RangeNode`
            // (`eval_value_container` threads each child into the next), so
            // `(b.unshift("s"))..b.first` widens `b` before `first` types and
            // `b.first..(b.unshift("s"))` does not (rigor-rs#306).
            Node::Range { left, right, .. } => left
                .iter()
                .chain(right.iter())
                .map(|&c| (c, FlowEdge::Uncond))
                .collect(),
            // A class/module header may hold a superclass or `<<` operand;
            // read it under the same barrier as the body.
            Node::Lambda { body, .. }
            | Node::Definition { body, .. }
            | Node::ClassDef { body, .. }
            | Node::ModuleDef { body, .. } => {
                body.iter().map(|&c| (c, FlowEdge::Barrier)).collect()
            }
            _ => vec![],
        }
    }

    /// One statement of [`Self::build_toplevel_check_env`]: a direct write binds
    /// as [`Self::bind_statement`] does, after widening the rebinds and
    /// receiver mutations nested in its value (`x = xs.each { |e| w = e }`,
    /// `y = (a << 1)`); any other statement widens every rebind inside it and
    /// applies the contained `local.<mutator>` calls.
    ///
    /// rigor-rs#393 — a retrying `begin` inside the statement then drops
    /// every indexed narrowing rooted at a local its retry edge rebinds
    /// ([`Self::retry_edge_index_drops`]): the reference re-evaluates the
    /// `begin` under the widened entry, so no record from before it, nor
    /// one its clauses wrote, survives past it.
    #[allow(clippy::too_many_arguments)]
    fn bind_check_statement(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        env: &mut TypeEnv,
        effects: &FlowEffects,
        indexed: &IndexedFlow,
        dead: &crate::dead::DeadPositions,
        interner: &mut Interner,
    ) {
        self.bind_check_statement_effects(ast, id, env, effects, indexed, dead, interner);
        drop_retry_edge_records(indexed, ast.get(id).span(), false, env);
    }

    #[allow(clippy::too_many_arguments)]
    fn bind_check_statement_effects(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        env: &mut TypeEnv,
        effects: &FlowEffects,
        indexed: &IndexedFlow,
        dead: &crate::dead::DeadPositions,
        interner: &mut Interner,
    ) {
        match ast.get(id) {
            Node::LocalVariableWrite { value, .. } | Node::MultiWrite { value, .. } => {
                let vspan = ast.get(*value).span();
                widen_flow_writes(effects.rebinds_within(vspan), vspan, env, interner);
                self.drop_indexed_rebinds(effects.rebinds_within(vspan), vspan, indexed, env);
                self.widen_mutated_locals(
                    ast, effects.mutations_within(vspan), indexed, *value, vspan, env, interner,
                );
                self.bind_statement(ast, id, env, interner);
                // An `h[k]` index target stores through `[]=` on the POST-binding
                // scope — `swap, swap[:a] = swap, 1` stores into the object `swap`
                // was just bound to — so the reference widens each receiver AFTER
                // `MultiTargetBinder` applies (`eval_multi_write`, rigor-rs#134).
                // Widening by the TARGET span mints the unconditional carrier
                // (`path_unconditional` reaches it through `target_exprs`),
                // as a straight-line `h[k] = v` gets.
                if let Node::MultiWrite { targets, .. } = ast.get(id) {
                    for (_, tspan, _) in targets.index_writes() {
                        self.widen_mutated_locals(
                            ast, effects.mutations_within(tspan), indexed, id, tspan, env, interner,
                        );
                    }
                }
            }
            // Only a real statement sequence is straight-line code. A recovery
            // carrier (a `rescue` modifier, `super(…)`, …) runs its writes
            // conditionally or out of order, so it widens; an inert one
            // (`defined?`, `END`, `BEGIN`) has no writes in `rebinds` and so
            // changes nothing (rigor-rs#153).
            Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
                for &s in body {
                    self.bind_check_statement(ast, s, env, effects, indexed, dead, interner);
                }
            }
            // rigor-rs#368 — `live_branch_for_if`: a certainty-folded
            // `if`/`unless` runs ONE arm unconditionally, so its writes must
            // BIND their values, not widen (`x = nil; unless x; x = "s"; end;
            // x.w` fires `for "s"` on the oracle). The folded-dead arm's
            // writes and mutations never reach the continuation.
            Node::If {
                predicate,
                then_body,
                else_body,
                is_unless,
                ..
            } => {
                let span = ast.get(id).span();
                match self.expr_truthiness(ast, *predicate, &mut Vec::new()) {
                    Some(truthy) => {
                        let live = if truthy != *is_unless {
                            then_body.clone()
                        } else {
                            else_body.clone()
                        };
                        // Only the effects INSIDE the `if`'s span are ever
                        // applied — every consumer below keeps the span
                        // check — so the tables' span slice replaces the
                        // whole-table scan (rigor-rs#424).
                        let live_rebinds: Vec<_> = effects
                            .rebinds_within(span)
                            .filter(|(w, _)| dead.write_reaches(*w, (span.1, span.1)))
                            .cloned()
                            .collect();
                        let live_mutations = effects.mutations_within(span).filter(
                            |(w, _, m, _)| {
                                dead.mutation_reaches(*w, (span.1, span.1), is_content_mutator(m))
                            },
                        );
                        widen_flow_writes(live_rebinds.iter(), span, env, interner);
                        self.drop_indexed_rebinds(live_rebinds.iter(), span, indexed, env);
                        self.widen_mutated_locals(
                            ast, live_mutations, indexed, id, span, env, interner,
                        );
                        for s in live {
                            self.bind_statement(ast, s, env, interner);
                        }
                    }
                    None => {
                        // rigor-rs#362 — a non-folded `if`'s terminated arm
                        // still evaluates but its scope dies at the join:
                        // its writes must not widen the post-`if` env.
                        // Same in-span slice as the folded arm above —
                        // `widen_*`/`drop_*`/`arm_write_ty` only ever read
                        // the inside-`span` entries (rigor-rs#424).
                        let live_rebinds: Vec<_> = effects
                            .rebinds_within(span)
                            .filter(|(w, _)| dead.write_reaches(*w, (span.1, span.1)))
                            .cloned()
                            .collect();
                        let live_mutations = effects.mutations_within(span).filter(
                            |(w, _, m, _)| {
                                dead.mutation_reaches(*w, (span.1, span.1), is_content_mutator(m))
                            },
                        );
                        // rigor-rs#443 — the post-`if` env JOINS a conditional
                        // write's value with the entry type, not a blanket
                        // `Dynamic`: `v = nil if p` on `v = nil` keeps `nil`
                        // and `v.zz` fires `for nil` on the oracle; a write
                        // whose value disagrees with the entry (`v = "s"`)
                        // unions into a decline-side type. Each arm
                        // contributes the LAST write of the name inside its
                        // span, or the entry binding when it wrote nothing.
                        // The LAST write of `name` in an arm, typed from its
                        // node kind: a plain `v = e` yields `type_of(e)`, an
                        // `op=` yields `op_write_result(entry, op, e)` (`v +=
                        // 2` on `v = 1` is `3`, unioning to `1 | 3` — the
                        // oracle's own answer), and a `v, w = rhs` yields the
                        // destructure's bound type for `name` (`"a" | 1` on
                        // `v, w = "a", "b"` — rigor-rs#443 review).
                        #[allow(clippy::too_many_arguments)]
                        fn arm_write_ty(
                            this: &Typer<'_>,
                            ast: &LoweredAst,
                            env: &TypeEnv,
                            body: &[NodeId],
                            name: &str,
                            entry: TypeId,
                            interner: &mut Interner,
                        ) -> Option<TypeId> {
                            let idx = this.reach_index(ast);
                            let mut last: Option<(rigor_parse::Span, TypeId)> = None;
                            let mut consider = |wspan: rigor_parse::Span, ty: TypeId| {
                                if last.is_none_or(|(ls, _)| ls.0 < wspan.0) {
                                    last = Some((wspan, ty));
                                }
                            };
                            for s in body {
                                for n in idx.nodes_within(ast, ast.get(*s).span()) {
                                    let wspan = ast.get(n).span();
                                    match ast.get(n) {
                                        Node::LocalVariableWrite { name: n2, value, .. }
                                            if n2 == name =>
                                        {
                                            let ty = this.type_of(ast, *value, env, interner);
                                            consider(wspan, ty);
                                        }
                                        Node::LocalVariableOpWrite { name: n2, value, op, .. }
                                            if n2 == name =>
                                        {
                                            let v_ty = this.type_of(ast, *value, env, interner);
                                            let ty = this.op_write_result(entry, op, v_ty, interner);
                                            consider(wspan, ty);
                                        }
                                        Node::MultiWrite { targets, value, .. } => {
                                            let rhs = this.type_of(ast, *value, env, interner);
                                            for (n2, ty) in crate::multi_target_binder::bind(targets, rhs, interner) {
                                                if n2 == name {
                                                    consider(wspan, ty);
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            last.map(|(_, ty)| ty)
                        }
                        let mut seen_names = Vec::new();
                        for (wspan, name) in &live_rebinds {
                            if !(span.0 <= wspan.0 && wspan.1 <= span.1) {
                                continue;
                            }
                            if seen_names.iter().any(|n| n == name) {
                                continue;
                            }
                            seen_names.push(name.clone());
                            let (truthy_body, falsey_body) = if *is_unless {
                                (else_body, then_body)
                            } else {
                                (then_body, else_body)
                            };
                            let entry_for_write = env.get(name.as_str()).copied().unwrap_or_else(|| interner.untyped());
                            let t_write = arm_write_ty(self, ast, env, truthy_body, name, entry_for_write, interner);
                            let f_write = arm_write_ty(self, ast, env, falsey_body, name, entry_for_write, interner);
                            // A terminating arm contributes the entry type —
                            // its writes never reach the join (rigor-rs#451
                            // review; `dead.write_reaches` above already
                            // drops the write itself, but a name written in
                            // BOTH arms needs the terminated edge floored).
                            let t_term = !truthy_body.is_empty() && crate::class_narrowing::branch_terminates(ast, truthy_body);
                            let f_term = !falsey_body.is_empty() && crate::class_narrowing::branch_terminates(ast, falsey_body);
                            let u = interner.untyped();
                            let entry_ty = env.get(name.as_str()).copied().unwrap_or(u);
                            let t_ty = match (t_term, t_write) {
                                (true, _) => entry_ty,
                                (false, Some(v)) => v,
                                (false, None) => entry_ty,
                            };
                            let f_ty = match (f_term, f_write) {
                                (true, _) => entry_ty,
                                (false, Some(v)) => v,
                                (false, None) => entry_ty,
                            };
                            let j = rigor_types::Algebra::join(interner, t_ty, f_ty);
                            drop_indexed_narrowings(env, name);
                            env.insert(Rc::from(name.as_str()), j);
                        }
                        self.drop_indexed_rebinds(live_rebinds.iter(), span, indexed, env);
                        self.widen_mutated_locals(
                            ast, live_mutations, indexed, id, span, env, interner,
                        );
                    }
                }
            }
            // rigor-rs#368 — `live_rescue_results` (`statement_evaluator.rb:1351`):
            // a rescue arm `branch_terminates?` drops contributes NO scope to
            // the post-`begin` join, so its writes do not widen the
            // continuation either (`begin 1 rescue x = "s" raise end; x.w`
            // fires `for 1` on the oracle — `x` keeps its pre-begin binding).
            Node::BeginRescue { clauses, .. } if !clauses.is_empty() => {
                let span = ast.get(id).span();
                let live_rebinds: Vec<_> = effects
                    .rebinds_within(span)
                    .filter(|(w, _)| dead.write_reaches(*w, (span.1, span.1)))
                    .cloned()
                    .collect();
                let live_mutations = effects.mutations_within(span).filter(|(w, _, m, _)| {
                    dead.mutation_reaches(*w, (span.1, span.1), is_content_mutator(m))
                });
                widen_flow_writes(live_rebinds.iter(), span, env, interner);
                self.drop_indexed_rebinds(live_rebinds.iter(), span, indexed, env);
                self.widen_mutated_locals(ast, live_mutations, indexed, id, span, env, interner);
            }
            Node::Loop { predicate, body, kind, .. } => {
                use crate::def_local_marks::ExitKind as K;
                let span = ast.get(id).span();
                let (predicate, body, kind) = (*predicate, body.clone(), *kind);
                let marks = self.def_local_marks(ast);
                let jumps =
                    marks.has_exit_within(span, &[K::Break, K::Next, K::Redo, K::Retry]);
                let once = matches!(
                    kind,
                    rigor_parse::LoopKind::While { begin_modifier: true }
                        | rigor_parse::LoopKind::Until { begin_modifier: true }
                );
                // The continuation criterion reads the entry env — after
                // `widen_flow_writes` every tracked name is pre-declined and
                // the replay would always answer "converged" — and lands on
                // the post-widen env so a body write to a non-suspect name
                // survives (rigor-rs#638).
                let entry = env.clone();
                let cont = if !(jumps || once || matches!(kind, rigor_parse::LoopKind::For)) {
                    self.loop_continuation(ast, &entry, predicate, &body, interner)
                } else {
                    None
                };
                let live_rebinds: Vec<_> = effects
                    .rebinds_within(span)
                    .filter(|(w, _)| dead.write_reaches(*w, (span.1, span.1)))
                    .cloned()
                    .collect();
                let live_mutations = effects.mutations_within(span).filter(|(w, _, m, _)| {
                    dead.mutation_reaches(*w, (span.1, span.1), is_content_mutator(m))
                });
                widen_flow_writes(live_rebinds.iter(), span, env, interner);
                self.drop_indexed_rebinds(live_rebinds.iter(), span, indexed, env);
                self.widen_mutated_locals(ast, live_mutations, indexed, id, span, env, interner);
                match cont {
                    Some(LoopContinuation::Fixpoint(bindings)) => {
                        for (n, ty) in bindings {
                            env.insert(Rc::from(n.as_str()), ty);
                        }
                    }
                    Some(LoopContinuation::Structural(tracked, suspect)) => {
                        self.apply_loop_cap_widen(env, &tracked, &suspect, interner);
                    }
                    None => {}
                }
            }
            other => {
                let span = other.span();
                // rigor-rs#362 — the `if`/`begin` arms above already filter
                // through `dead.write_reaches`; every other statement kind
                // (`case`, `&&`/`||`, a `while` holding a terminated arm)
                // needs the same drop: a write inside an
                // evaluated-but-join-dropped position must not widen the
                // post-statement scope.
                let live_rebinds: Vec<_> = effects
                    .rebinds_within(span)
                    .filter(|(w, _)| dead.write_reaches(*w, (span.1, span.1)))
                    .cloned()
                    .collect();
                let live_mutations = effects.mutations_within(span).filter(|(w, _, m, _)| {
                    dead.mutation_reaches(*w, (span.1, span.1), is_content_mutator(m))
                });
                widen_flow_writes(live_rebinds.iter(), span, env, interner);
                self.drop_indexed_rebinds(live_rebinds.iter(), span, indexed, env);
                self.widen_mutated_locals(ast, live_mutations, indexed, id, span, env, interner);
            }
        }
    }

    /// Apply the `local.<mutator>(…)` entries inside `span` to `env` — the
    /// flat-env port of the reference's `MutationWidening` (`widen_after_call`
    /// runs on every statement). A mutation on an all-`Uncond` descent from
    /// `root` ([`Self::path_unconditional`]) — the statement's own call, a
    /// write's unconditional operand, a positional argument — definitely
    /// ran, so the binding becomes the widened nominal outright and a later
    /// `a.frobnicate` keeps firing (the reference's sequential rebind,
    /// rigor-rs#136). A mutation in a conditional position (inside an `if`
    /// branch, a `&&` operand, a `rescue` arm — including `begin`'s main
    /// body, whose `rescue` exit can leave it unrun) widens to `Dynamic`
    /// instead, where the flat env cannot reproduce `Scope#join`, handing
    /// the convergence question to the collection-shape pass: two edges that
    /// mint the same carrier join back to it and still fire through
    /// `check_collection_call`'s Dynamic gate, while divergent edges decline
    /// there and stay silent — exactly the reference's union-then-decline
    /// (rigor-rs#139).
    ///
    /// `indexed` threads the rigor-rs#325 side table: a recovery-flagged
    /// `operand` `h[k] ||= v` mints the receiver's nominal where strict
    /// `path_unconditional` declines — the reference's `IndexWriteWidening`
    /// applies it unconditionally at an evaluated operand position — and
    /// records its stored slot (`eval_index_or_write` →
    /// `Scope#with_indexed_narrowing`) through [`Self::apply_mutation_effects`].
    // too_many_arguments: shared replay context — a bundle struct would obscure.
    #[allow(clippy::too_many_arguments)]
    // `mutations` is an ITERATOR, not a slice: the `bind_check_statement`
    // arms that dead-gate the table used to `.cloned().collect()` a filtered
    // Vec per statement — two `String` clones plus a `ShapeKey` clone per
    // entry, O(mutations) per replayed call subtree — the largest single
    // allocation stream of rigor-rs#658's mutator chain.
    fn widen_mutated_locals<'m>(
        &self,
        ast: &LoweredAst,
        mutations: impl Iterator<Item = &'m (rigor_parse::Span, String, String, Option<ShapeKey>)>,
        indexed: &IndexedFlow,
        root: NodeId,
        span: rigor_parse::Span,
        env: &mut TypeEnv,
        interner: &mut Interner,
    ) {
        for (wspan, name, method, drop_key) in mutations {
            if !(span.0 <= wspan.0 && wspan.1 <= span.1) {
                continue;
            }
            self.apply_mutation_effects(
                ast,
                indexed,
                root,
                true,
                *wspan,
                name,
                method,
                drop_key.as_ref(),
                env,
                interner,
            );
        }
        for m in &indexed.slot_mutations {
            // A slot mutation inside a block body's `break` argument
            // widens nothing anywhere — `join_block_break_bindings`
            // drops every scope effect (rigor-rs#407).
            if span.0 <= m.span.0
                && m.span.1 <= span.1
                && !ast.in_block_break_locals_only_carrier(m.span)
            {
                self.apply_slot_mutation(ast, indexed, root, m, env, interner);
            }
        }
        // Upstream #2677 — the unfiltered `alias_mutations` covers the
        // block- and def-internal mutators the named tables drop; a
        // toplevel mutation may floor twice, harmlessly
        // (`widen_mutated_binding` declines an already-widened carrier).
        for am in &indexed.alias_mutations {
            if am.span.0 >= span.0 && am.span.1 <= span.1 {
                self.floor_alias_linked(
                    ast,
                    indexed,
                    &am.name,
                    &am.method,
                    am.span,
                    am.part_only,
                    env,
                    interner,
                );
            }
        }
    }

    /// The `h[k] -> stored` pairs every in-span `operand` `||=` writes —
    /// `eval_index_or_write`'s `with_indexed_narrowing` record — computed on
    /// `env` AS ENTERED: `index_write_stored_type` reads the slot's `current`
    /// and the rvalue before `IndexWriteWidening` touches the receiver, so
    /// this MUST run before the span's rebind/mutation widenings land. A
    /// `None` `slot_stored_type` declines the record (a `Dynamic`/`Top`
    /// receiver — `fully_tracked_receiver_type?`, upstream issue #544).
    #[allow(clippy::too_many_arguments)]
    fn stored_slot_writes(
        &self,
        ast: &LoweredAst,
        indexed: &IndexedFlow,
        root: NodeId,
        span: rigor_parse::Span,
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Vec<(rigor_parse::Span, String, TypeId)> {
        if indexed.slot_writes.is_empty() {
            return Vec::new();
        }
        let marks = self.path_marks(ast, root);
        indexed
            .slot_writes
            .iter()
            .filter(|w| w.span.0 >= span.0 && w.span.1 <= span.1)
            .filter(|w| {
                marks
                    .operand(w.span)
                    .unwrap_or_else(|| self.path_operand_evaluated(ast, root, w.span))
            })
            .filter_map(|w| {
                self.slot_stored_type(ast, w, env, interner)
                    .map(|ty| (w.span, indexed_narrowing_key(&w.name, &w.key), ty))
            })
            .collect()
    }

    /// The second half of one statement's indexed-narrowing effect for the
    /// always-truthy env, whose bindings `widen_flow_writes` already moved:
    /// the `Scope#type_of=` rebind drops, the `invalidate_after_call`
    /// `[]=`/mutator drops, the `with_indexed_narrowing` inserts, and the
    /// `widen_mutated_slot` element-mutator widenings — all in SOURCE order
    /// so `h[:a] ||= 1; h[:a] = 2` leaves the slot dropped while
    /// `h[:a] ||= []; h[:a] << 1` widens the record it just wrote.
    #[allow(clippy::too_many_arguments)]
    fn land_indexed_stored(
        &self,
        ast: &LoweredAst,
        indexed: &IndexedFlow,
        root: NodeId,
        span: rigor_parse::Span,
        rebinds: &[(rigor_parse::Span, String)],
        mutations: &[(rigor_parse::Span, String, String, Option<ShapeKey>)],
        stored: Vec<(rigor_parse::Span, String, TypeId)>,
        env: &mut TypeEnv,
        interner: &mut Interner,
    ) {
        self.drop_indexed_rebinds(rebinds.iter(), span, indexed, env);
        // Merge the three event lists by span start; kind order on a tie is
        // drop < insert < slot-widen (a `h[k] = v` after the `||=` still
        // drops the record the `||=` just wrote).
        enum Ev {
            Drop(usize),
            Insert(usize),
            SlotMut(usize),
        }
        let mut events: Vec<(rigor_parse::Span, Ev)> = Vec::new();
        for (i, (wspan, ..)) in mutations.iter().enumerate() {
            if wspan.0 >= span.0 && wspan.1 <= span.1 {
                events.push((*wspan, Ev::Drop(i)));
            }
        }
        for (i, (wspan, ..)) in stored.iter().enumerate() {
            events.push((*wspan, Ev::Insert(i)));
        }
        for (i, m) in indexed.slot_mutations.iter().enumerate() {
            if m.span.0 >= span.0 && m.span.1 <= span.1 {
                events.push((m.span, Ev::SlotMut(i)));
            }
        }
        events.sort_by_key(|(s, ev)| {
            (
                s.0,
                match ev {
                    Ev::Drop(_) => 0,
                    Ev::Insert(_) => 1,
                    Ev::SlotMut(_) => 2,
                },
            )
        });
        for (_, ev) in events {
            match ev {
                Ev::Drop(i) => {
                    let (wspan, name, method, drop_key) = &mutations[i];
                    // The `apply_mutation_effects` locals-only gate
                    // (rigor-rs#363): a `break` argument's scope lands on
                    // the locals-only break join, so the record drop
                    // applies nowhere — except an iterated-body
                    // content-scan call, whose writeback-joined rebind
                    // declines the read anyway.
                    if !crate::flow_writes::break_join_suppresses(ast, *wspan, method) {
                        self.drop_indexed_mutation(name, method, drop_key.as_ref(), env);
                    }
                }
                Ev::Insert(i) => {
                    env.insert(Rc::from(stored[i].1.as_str()), stored[i].2);
                }
                Ev::SlotMut(i) => {
                    // `join_block_break_bindings` drops the widening
                    // (rigor-rs#407).
                    if !ast.in_block_break_locals_only_carrier(
                        indexed.slot_mutations[i].span,
                    ) {
                        self.apply_slot_mutation(
                            ast,
                            indexed,
                            root,
                            &indexed.slot_mutations[i],
                            env,
                            interner,
                        );
                    }
                }
            }
        }
        // Upstream #2677 — the unfiltered `alias_mutations` covers the
        // block- and def-internal mutators the named tables drop; a
        // toplevel mutation may floor twice, harmlessly
        // (`widen_mutated_binding` declines an already-widened carrier).
        for am in &indexed.alias_mutations {
            if am.span.0 >= span.0 && am.span.1 <= span.1 {
                self.floor_alias_linked(
                    ast,
                    indexed,
                    &am.name,
                    &am.method,
                    am.span,
                    am.part_only,
                    env,
                    interner,
                );
            }
        }
    }

    /// `Scope#type_of=`'s rebind invalidation: a local rebound anywhere in
    /// `span` drops every indexed narrowing rooted at it.
    fn drop_indexed_rebinds<'r>(
        &self,
        rebinds: impl Iterator<Item = &'r (rigor_parse::Span, String)>,
        span: rigor_parse::Span,
        indexed: &IndexedFlow,
        env: &mut TypeEnv,
    ) {
        // A `name\u{1f}key` env entry is only ever minted from an
        // `indexed.slot_writes` record — where there are none the O(env)
        // `retain` inside `drop_indexed_narrowings` cannot find anything,
        // and a flat generated file pays it once per statement
        // (rigor-rs#424).
        if indexed.slot_writes.is_empty() {
            return;
        }
        for (wspan, name) in rebinds {
            if wspan.0 >= span.0 && wspan.1 <= span.1 {
                drop_indexed_narrowings(env, name);
            }
        }
    }

    /// rigor-rs#393 — the [`IndexedFlow::retry_drops`] table: for each
    /// top-level `begin` with a clause holding a `retry` that re-enters it,
    /// every local the protected body or a retrying clause rebinds or
    /// mutates. The reference carries those writes back across the retry
    /// edge (`eval_retried_begin` → `widen_entry_for_retry`: the body's
    /// `raise_scopes` and each `retry`'s scope) and rebinds the entry's
    /// binding when the written one is not accepted, which drops every
    /// indexed narrowing rooted at it (`h[:a] ||= "t"; retry` leaves
    /// `h[:a]` reading `Hash#[]` past the `begin`). Whether the rebind
    /// happens hangs on the widened carrier's acceptance (`h[:b] ||= 1`
    /// keeps the record on the oracle), which the flat env does not model:
    /// any write declines. A `begin` inside a `def`/`class`/`module` body
    /// rebinds that scope's locals, not the top-level ones, so it records
    /// nothing; neither does a write inside such a body.
    pub(crate) fn retry_edge_index_drops(
        &self,
        ast: &LoweredAst,
        rebinds: &[(rigor_parse::Span, String)],
        mutations: &[(rigor_parse::Span, String, String, Option<ShapeKey>)],
        indexed: &IndexedFlow,
    ) -> Vec<(rigor_parse::Span, String)> {
        let in_scope_body =
            |s: rigor_parse::Span| indexed.scope_spans.iter().any(|&b| within_span(s, b));
        let mut out: Vec<(rigor_parse::Span, String)> = Vec::new();
        for (_, n) in ast.iter() {
            let Node::BeginRescue { span, main_body, clauses, .. } = n else {
                continue;
            };
            if clauses.is_empty() || in_scope_body(*span) {
                continue;
            }
            let mut regions: Vec<rigor_parse::Span> = clauses
                .iter()
                .filter(|c| self.clause_retries(ast, c.span))
                .map(|c| c.span)
                .collect();
            if regions.is_empty() {
                continue;
            }
            if let (Some(&f), Some(&l)) = (main_body.first(), main_body.last()) {
                regions.push((ast.get(f).span().0, ast.get(l).span().1));
            }
            let writes = rebinds
                .iter()
                .map(|(w, name)| (*w, name))
                .chain(mutations.iter().map(|(w, name, ..)| (*w, name)))
                .chain(indexed.slot_writes.iter().map(|w| (w.span, &w.name)));
            for (w, name) in writes {
                if regions.iter().any(|&r| within_span(w, r))
                    && !in_scope_body(w)
                    && !out.iter().any(|(b, n)| b == span && n == name)
                {
                    out.push((*span, name.clone()));
                }
            }
        }
        out
    }

    /// `IndexedNarrowing.invalidate_after_call` for ONE mutation entry —
    /// the record-drop half `apply_mutation_effects` also runs.
    pub(crate) fn drop_indexed_mutation(
        &self,
        name: &str,
        method: &str,
        drop_key: Option<&ShapeKey>,
        env: &mut TypeEnv,
    ) {
        match drop_key {
            Some(key) => {
                env.remove(indexed_narrowing_key(name, key).as_str());
            }
            None if method != "[]=" => drop_indexed_narrowings(env, name),
            _ => {}
        }
    }

    /// The binding a `local.<mutator>(…)` call leaves behind — the flat-env
    /// port of the reference's `MutationWidening.widen_for_mutator`: a
    /// literal-shape carrier loses its shape but keeps its class (`Tuple` →
    /// `Nominal[Array]`, `HashShape` → `Nominal[Hash]`), a union widens
    /// memberwise (`widen_union`), and
    /// every other binding is untouched (`None` — a precise `Nominal` has no
    /// shape to lose and a `Dynamic` gains nothing). The flat env tracks no
    /// element evidence, so the nominal's args stay empty — the message reads
    /// `for Array` where the reference says `for Array[Dynamic[top] | …]`; a
    /// message drift only, since the harness keys on `(rule, line, col)`.
    ///
    /// The union arm's per-member mint is the exception: `widen_union` grows
    /// each arm memberwise and `Combinator.union` dedups on STRUCTURAL
    /// equality, so a literal arm keeps its own element evidence —
    /// `widen_tuple` writes `Array[1 | …]`, not bare `Array`. Two DISTINCT
    /// literal arms (`c ? [1] : [2]` then `a.push(3)`) must therefore mint
    /// two distinct carriers or the union collapses to `Nominal[Array]` and
    /// `a.frobnicate` fires where the oracle stays silent (adversarial review
    /// of rigor-rs#309). Member evidence comes from the same
    /// [`Typer::coll_value_members`] the collection-shape pass uses.
    pub(crate) fn widen_mutated_binding(
        &self,
        ty: TypeId,
        method: &str,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        let mint = |name: &str, interner: &mut Interner| {
            self.index
                .class_id(name)
                .map(|class| interner.intern(Type::Nominal { class, args: vec![] }))
        };
        match interner.get(ty) {
            Type::Tuple(_) if ARRAY_MUTATORS.contains(&method) => mint("Array", interner),
            Type::HashShape(_) if HASH_MUTATORS.contains(&method) => mint("Hash", interner),
            // `StringMutation.widen_constant` (string_mutation.rb): a
            // String-valued `Constant` under a String mutator loses its value
            // pin to the bare `String` nominal — `s = "abc"; s[0] = 5` makes
            // `s` a `String` whose `s[k]` reads answer `String | nil` (the
            // RBS `String?`), so a chained `s[0].frobnicate` declines instead
            // of witnessing on the erased bare `String` (rigor-rs#352). Only
            // the String table applies: the pinned value is what the mutation
            // falsifies, and no other Constant carrier has a value an
            // in-place call can change.
            Type::Constant(Scalar::Str(_)) if STRING_MUTATORS.contains(&method) => {
                mint("String", interner)
            }
            // `StringMutation.widen_refined` (string_mutation.rb): a
            // predicate refinement over String (`decimal-int-string`,
            // `lowercase-string`) under a String mutator → its `String`
            // base. ANY in-place mutation can falsify the predicate
            // (`s.clear` leaves a `decimal-int-string` empty, `s << "A"` a
            // `lowercase-string` mixed-case), so none keeps it — the base
            // nominal answers directly.
            Type::Refined { base, .. } if STRING_MUTATORS.contains(&method) => {
                match interner.get(*base) {
                    Type::Nominal { class, .. }
                        if self.index.class_name_for_id(*class) == Some("String") =>
                    {
                        Some(*base)
                    }
                    _ => None,
                }
            }
            // `RefinementMutation.widen` → `string_base`
            // (refinement_mutation.rb / string_mutation.rb): a
            // `non-empty-string` keeps its witness under the
            // EMPTY_PRESERVING mutators (returns `None` — the binding
            // stands); every other String mutator can leave the buffer
            // empty, so it widens to the `String` base.
            Type::Difference { base, removed } if STRING_MUTATORS.contains(&method) => {
                let non_empty_string = matches!(interner.get(*base),
                    Type::Nominal { class, .. }
                        if self.index.class_name_for_id(*class) == Some("String"))
                    && matches!(interner.get(*removed),
                        Type::Constant(Scalar::Str(s)) if s.is_empty());
                if non_empty_string
                    && !crate::flow_writes::STRING_MUTATOR_EMPTY_PRESERVING.contains(&method)
                {
                    Some(*base)
                } else {
                    None
                }
            }
            Type::Union(members) => {
                let members = members.clone();
                let mut out = Vec::with_capacity(members.len());
                let mut changed = false;
                for m in members {
                    let shape_cls = match interner.get(m) {
                        Type::Tuple(_) if ARRAY_MUTATORS.contains(&method) => Some("Array"),
                        Type::HashShape(_) if HASH_MUTATORS.contains(&method) => Some("Hash"),
                        _ => None,
                    };
                    let widened = match shape_cls {
                        Some(cls) => {
                            let members = self.coll_value_members(interner, m);
                            self.coll_nominal_with(interner, cls, &members)
                        }
                        None => self.widen_mutated_binding(m, method, interner),
                    };
                    match widened {
                        Some(w) => {
                            changed = true;
                            out.push(w);
                        }
                        None => out.push(m),
                    }
                }
                if !changed {
                    return None;
                }
                out.sort_unstable();
                out.dedup();
                Some(if out.len() == 1 {
                    out[0]
                } else {
                    interner.intern(Type::Union(out))
                })
            }
            _ => None,
        }
    }

    /// Flow-sensitive local CONSTANT propagation (ADR-0022 first substrate
    /// slice). For every `if`/`unless`/ternary predicate NOT lexically inside a
    /// loop / block, record the [`TypeId`] the predicate folds to under the
    /// branch-joined flow environment that dominates it. The companion rule
    /// `flow.always-truthy-condition` fires only when that recorded type is a
    /// `Type::Constant`, so this query is the zero-FP keystone: it must be a
    /// strict UNDER-approximation of the reference's flow folder (witness set ⊆
    /// reference), achieved by **widening on any doubt**.
    ///
    /// Soundness model (why a constant here can never be a false positive):
    /// - **Straight-line writes** bind the local to the RHS type, exactly as the
    ///   flat env does.
    /// - **`if`/`unless` branches** are evaluated independently and JOINED: a
    ///   local keeps a binding only when both branches agree on the IDENTICAL
    ///   `TypeId`; any disagreement (or a local written in only one branch)
    ///   widens it to `Dynamic`. This is what stops `x = 5; if c; x = f; end;
    ///   if x` from folding `x` to `5` — the flat env's central unsoundness.
    /// - **Loops / blocks / `case` / `begin`-`rescue` / `&&`-`||` / any other
    ///   node** widen EVERY local written anywhere in their span (a loop iterates
    ///   0..n times; a closure may write a captured local; a `case`/`begin` arm
    ///   is conditional) and are NOT descended for predicate snapshots. Skipping
    ///   loop/block predicates matches the reference's own envelope; declining
    ///   the others is an extra conservative miss (never an FP).
    /// - **`def` / `class` / `module` bodies** are independent scopes: they are
    ///   descended with a FRESH local env (Ruby method/class bodies do not see
    ///   the enclosing locals) but INHERIT the loop/block suppression flag, so a
    ///   `def` nested in a block keeps its predicates suppressed (reference parity)
    ///   while a top-level `def`'s predicates are recorded. A nested scope never
    ///   perturbs the enclosing env.
    ///
    /// Writes are collected once (span-keyed) and widening filters that list by
    /// span-containment — orphan-proof, the same discipline as
    /// the dead-assignment collector.
    pub fn always_truthy_snapshots(
        &self,
        ast: &LoweredAst,
        interner: &mut Interner,
    ) -> HashMap<NodeId, TypeId> {
        let mut out = HashMap::new();
        let mut writes = collect_flow_writes(ast);
        writes.extend(indexed_flow_writes(ast, self.source));
        // The indexed-narrowing side table (rigor-rs#325): `rebinds` drops a
        // rebinding name's records, `mutations` carries the `[]=` / mutator
        // drops, `indexed` the `h[k] ||= v` records themselves.
        let mut rebinds = collect_flow_writes(ast);
        rebinds.extend(indexed_flow_writes(ast, self.source));
        let mut mutations = toplevel_mutations(ast);
        let mut indexed = collect_indexed_flow(ast);
        // rigor-rs#368 — a write/mutation inside a folded-dead arm never
        // lands; it must not widen this env either. Same iterated-body
        // writeback exception as the `check_flow` retain above
        // (rigor-rs#363 review).
        let dead = self.dead_positions(ast, &mut Vec::new());
        writes.retain(|(s, _)| !dead.covers(*s));
        rebinds.retain(|(s, _)| !dead.covers(*s));
        mutations.retain(|(s, _, m, _)| {
            !dead.covers(*s) || (is_content_mutator(m) && dead.inside_iterated(*s))
        });
        indexed.alias_mutations.retain(|am| {
            !dead.covers(am.span)
                || (is_content_mutator(&am.method) && dead.inside_iterated(am.span))
        });
        // The copy links keep their dead-arm rows — upstream's
        // `SiteCollector` walks the whole program.
        let body = match ast.get(ast.root()) {
            Node::Program { body, .. } => body.clone(),
            _ => return out,
        };
        let mut env = TypeEnv::new();
        self.flow_eval_scope(ast, &body, &mut env, false, None, DefKind::Instance, &writes, &rebinds, &mutations, &indexed, interner, &mut out);
        out
    }

    /// Thread `env` through a scope's statements in source order. `self_qual` /
    /// `self_kind` carry the enclosing class/module QUALIFIED name + method kind
    /// so an implicit-self predicate call can be resolved for the interprocedural
    /// literal-tail fold (ADR-0038); `None` at the top level (a receiverless call
    /// there has no project self to resolve against).
    #[allow(clippy::too_many_arguments)]
    fn flow_eval_scope(
        &self,
        ast: &LoweredAst,
        stmts: &[NodeId],
        env: &mut TypeEnv,
        in_loop_or_block: bool,
        self_qual: Option<&str>,
        self_kind: DefKind,
        writes: &[(rigor_parse::Span, String)],
        rebinds: &[(rigor_parse::Span, String)],
        mutations: &[(rigor_parse::Span, String, String, Option<ShapeKey>)],
        indexed: &IndexedFlow,
        interner: &mut Interner,
        out: &mut HashMap<NodeId, TypeId>,
    ) {
        for &s in stmts {
            self.flow_eval_stmt(ast, s, env, in_loop_or_block, self_qual, self_kind, writes, rebinds, mutations, indexed, interner, out);
        }
    }

    /// Evaluate one statement's effect on `env`, recording predicate snapshots.
    #[allow(clippy::too_many_arguments)]
    fn flow_eval_stmt(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        env: &mut TypeEnv,
        in_loop_or_block: bool,
        self_qual: Option<&str>,
        self_kind: DefKind,
        writes: &[(rigor_parse::Span, String)],
        rebinds: &[(rigor_parse::Span, String)],
        mutations: &[(rigor_parse::Span, String, String, Option<ShapeKey>)],
        indexed: &IndexedFlow,
        interner: &mut Interner,
        out: &mut HashMap<NodeId, TypeId>,
    ) {
        match ast.get(id) {
            // A recovery / inert carrier is not straight-line code: it goes to
            // the widening arm below (rigor-rs#153). An inert carrier's writes
            // are not in `writes`, so it leaves `env` as the reference does.
            Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
                let body = body.clone();
                self.flow_eval_scope(ast, &body, env, in_loop_or_block, self_qual, self_kind, writes, rebinds, mutations, indexed, interner, out);
            }
            Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                // A value expression may itself write OTHER locals (`x = (y = 5)`)
                // or capture-write via a block — widen those first, then bind.
                let vspan = ast.get(value).span();
                // The `h[k] ||= v` records compute on the env the write is
                // ENTERED with, before the widenings land
                // (`index_write_stored_type` runs on the entry scope).
                let stored =
                    self.stored_slot_writes(ast, indexed, id, vspan, env, interner);
                widen_flow_writes(writes.iter(), vspan, env, interner);
                self.land_indexed_stored(
                    ast, indexed, id, vspan, rebinds, mutations, stored, env, interner,
                );
                // An if-EXPRESSION assigned to a local (`strategies = if
                // Gitlab::Database.read_write?; …`) still carries a predicate the
                // always-truthy rule visits — record its snapshot here (the
                // statement walk only reaches an `if` that is a bare statement).
                // The branch writes are already conservatively widened above, so
                // this only ADDS the predicate snapshot (no env perturbation).
                if !in_loop_or_block {
                    if let Node::If { predicate, .. } = ast.get(value) {
                        let predicate = *predicate;
                        let pty = self
                            .flow_predicate_type(ast, predicate, env, self_qual, self_kind, interner);
                        out.insert(value, pty);
                    }
                }
                let ty = self.type_of(ast, value, env, interner);
                drop_indexed_narrowings(env, &name);
                env.insert(Rc::from(name.as_str()), ty);
            }
            // `a, b = rhs` — destructure the RHS and rebind every target. This
            // is the arm that closes the multi-write flow-write FP: without it
            // an earlier `x = 5` survived the rebind and `if x` folded to a
            // constant.
            Node::MultiWrite { targets, value, .. } => {
                let (targets, value) = (targets.clone(), *value);
                // Same discipline as the single-target arm: the RHS may itself
                // write other locals — widen those first, then bind.
                let vspan = ast.get(value).span();
                let stored =
                    self.stored_slot_writes(ast, indexed, id, vspan, env, interner);
                widen_flow_writes(writes.iter(), vspan, env, interner);
                self.land_indexed_stored(
                    ast, indexed, id, vspan, rebinds, mutations, stored, env, interner,
                );
                let rhs = self.type_of(ast, value, env, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    drop_indexed_narrowings(env, &name);
                    env.insert(Rc::from(name.as_str()), ty);
                }
                // An `h[k]` index target stores through `[]=` on the
                // POST-binding scope (`h, h[:a] = h, 1` stores into the rebound
                // `h`), so each receiver's locals widen after the bindings —
                // exactly as `h[k] = v` widens them (`eval_multi_write` →
                // `IndexWriteWidening.widen`, rigor-rs#134).
                for (_, tspan, _) in targets.index_writes() {
                    widen_flow_writes(writes.iter(), tspan, env, interner);
                }
            }
            Node::LocalVariableOpWrite { name, .. } => {
                // `x += 1` / `x ||= 5` reads-then-writes; the result is not a
                // tracked constant in this slice — widen.
                let name = name.clone();
                let u = interner.untyped();
                drop_indexed_narrowings(env, &name);
                env.insert(Rc::from(name.as_str()), u);
            }
            Node::If { predicate, then_body, else_body, is_unless, .. } => {
                let (predicate, then_body, else_body, is_unless) =
                    (*predicate, then_body.clone(), else_body.clone(), *is_unless);
                let pty = self.flow_predicate_type(
                    ast, predicate, env, self_qual, self_kind, interner,
                );
                if !in_loop_or_block {
                    out.insert(id, pty);
                }
                // A predicate `h[k] ||= v` records its slot on the env the
                // predicate is ENTERED with; the record lands in BOTH branch
                // scopes, so it survives the join the same way the
                // reference's `with_indexed_narrowing` does.
                let if_span = ast.get(id).span();
                let stored =
                    self.stored_slot_writes(ast, indexed, id, if_span, env, interner);
                // rigor-rs#368 — `live_branch_for_if`: a certainty-folded
                // predicate keeps ONE arm — the dead arm's writes never join
                // the continuation. It is still walked into a DISCARDED env
                // so the always-truthy snapshots inside it record the
                // scope-fill the reference's `propagate` produces.
                match self.predicate_certainty(pty, interner) {
                    Some(truthy) => {
                        let (live, dead_arm) = if truthy != is_unless {
                            (&then_body, &else_body)
                        } else {
                            (&else_body, &then_body)
                        };
                        // The reference fills the dead arm's scope from the
                        // dead EDGE — the arm would have run on the opposite
                        // of the folded truthiness, and each predicate-
                        // subject local narrows to the members consistent
                        // with that edge (`q = nil; if q; if q; end` warns
                        // on neither on the oracle — the inner `q` is `Bot`;
                        // `a = "x"; b = nil; if a && b; a.w` fires `w` for
                        // `"x"`, `&&`'s truthy edge keeping `a`).
                        let mut dead_env = env.clone();
                        let mut subjects = Vec::new();
                        Self::predicate_edge_locals(ast, predicate, !truthy, &mut subjects);
                        for (name, op) in subjects {
                            let cur = dead_env
                                .get(name.as_str())
                                .copied()
                                .unwrap_or_else(|| interner.untyped());
                            let narrowed = self.dead_subject_type(ast, cur, &op, interner);
                            dead_env.insert(Rc::from(name.as_str()), narrowed);
                        }
                        self.flow_eval_scope(
                            ast, dead_arm, &mut dead_env, in_loop_or_block, self_qual, self_kind, writes, rebinds, mutations, indexed, interner, out,
                        );
                        let mut live_env = env.clone();
                        self.flow_eval_scope(
                            ast, live, &mut live_env, in_loop_or_block, self_qual, self_kind, writes, rebinds, mutations, indexed, interner, out,
                        );
                        *env = live_env;
                    }
                    None => {
                        // Independently evaluate each branch from the dominating env, then
                        // join: a binding survives only if both branches agree exactly.
                        let mut then_env = env.clone();
                        self.flow_eval_scope(
                            ast, &then_body, &mut then_env, in_loop_or_block, self_qual, self_kind, writes, rebinds, mutations, indexed, interner, out,
                        );
                        let mut else_env = env.clone();
                        self.flow_eval_scope(
                            ast, &else_body, &mut else_env, in_loop_or_block, self_qual, self_kind, writes, rebinds, mutations, indexed, interner, out,
                        );
                        *env = join_flow_envs(&then_env, &else_env, interner);
                    }
                }
                // A predicate may contain a write (`if (x = f)`); widen post-join.
                let pspan = ast.get(predicate).span();
                widen_flow_writes(writes.iter(), pspan, env, interner);
                self.land_indexed_stored(
                    ast, indexed, id, if_span, rebinds, mutations, stored, env, interner,
                );
            }
            Node::Definition { span, body, .. } => {
                // Independent scope: fresh local env, inherited suppression
                // flag. The self KIND + owner mirror
                // `self_type_for_method_body` / `self_type_for_class_body`:
                // `def self.x`, defs under a `class <<` frame, and
                // `class <<` / class / module body statements are
                // singleton-side; a receiver-bearing `def` or an
                // unnameable context keeps no modelled self (rigor-rs#368).
                let (qual, kind) = match self.call_self(ast, *span) {
                    crate::dead::CallSelf::Singleton(q) => (q, DefKind::Singleton),
                    crate::dead::CallSelf::Instance(q) => (q, DefKind::Instance),
                    crate::dead::CallSelf::Unmodelled => (String::new(), DefKind::Instance),
                };
                let body = body.clone();
                let mut fresh = TypeEnv::new();
                self.flow_eval_scope(
                    ast, &body, &mut fresh, in_loop_or_block,
                    if qual.is_empty() { None } else { Some(qual.as_str()) },
                    kind, writes, rebinds, mutations, indexed, interner, out,
                );
            }
            Node::ClassDef { body, name, .. } | Node::ModuleDef { body, name, .. } => {
                // Independent scope: fresh local env, inherited suppression flag.
                // Extend the lexical self-qualified name so a nested class/module's
                // implicit-self calls resolve against the right owner. A
                // class/module body statement's `self` IS the class object —
                // `self_type_for_class_body` → `Singleton[path]` — so the
                // body-level kind is `Singleton` (a `def` re-derives its own
                // kind from its lexical position via `call_self`).
                let (body, child_qual) = (body.clone(), qualify_self(self_qual, name));
                let mut fresh = TypeEnv::new();
                self.flow_eval_scope(
                    ast, &body, &mut fresh, in_loop_or_block, Some(&child_qual), DefKind::Singleton, writes, rebinds, mutations, indexed, interner, out,
                );
            }
            // A `begin`/`rescue` DOES get its bodies evaluated on the
            // reference — `collect_rescue_chain_results` sub-evals each
            // clause — so their predicate snapshots record (that is what
            // makes `raise if helper` warn inside an arm). The continuation
            // join is still the flat decline: every write in the span widens
            // (rigor-rs#368 only repositions which writes REACH a use —
            // `local_reach`'s `dead_positions` — not this widening).
            Node::BeginRescue {
                main_body,
                clauses,
                ensure_body,
                span: begin_span,
                ..
            } => {
                let (main_body, clauses, ensure_body, begin_span) =
                    (main_body.clone(), clauses.clone(), ensure_body.clone(), *begin_span);
                if !in_loop_or_block {
                    let mut scratch = env.clone();
                    self.flow_eval_scope(
                        ast, &main_body, &mut scratch, in_loop_or_block, self_qual, self_kind, writes, rebinds, mutations, indexed, interner, out,
                    );
                    // Upstream #1837 (rigor #1231): with no `retry` aimed at
                    // this `begin`, each arm reads a local the protected body
                    // writes as `Dynamic[top]` (`widen_clause_entry`;
                    // `rescue_writes` is nil under a retry edge, for EVERY arm).
                    let body_region = match (main_body.first(), main_body.last()) {
                        (Some(&f), Some(&l)) => Some((ast.get(f).span().0, ast.get(l).span().1)),
                        _ => None,
                    };
                    // Spans of the retrying clauses — see the sibling
                    // widening below (`absorb_retry_rebinds`).
                    let retry_spans: Vec<(usize, usize)> = clauses
                        .iter()
                        .filter(|c| self.clause_retries(ast, c.span))
                        .map(|c| c.span)
                        .collect();
                    let mut arm_envs = Vec::with_capacity(clauses.len());
                    for c in &clauses {
                        // The arm's entry scope is the `begin`'s ENTRY scope —
                        // the exception may fire before ANY try statement runs,
                        // so try-body writes never flow in — and since v0.4.3
                        // (#1837) each name the body writes reads untyped there
                        // (`timed = nil; begin; timed = true; rescue; if timed`
                        // warned FALSEY up to v0.4.2, silent since). A `retry` inside
                        // the arm re-runs the `begin`, carrying the arm's own
                        // writes back round — widened, since the re-entry order
                        // is not the straight line (`retried = false; begin;
                        // rescue; return if retried; retried = true; retry` is
                        // silent on the oracle). The carrier spans the WHOLE
                        // `begin`, not just the arm: `retry` re-runs the try
                        // body before the clause executes again, so try-body
                        // writes carry round too (`attempts += 1` under
                        // `begin` then `retry if attempts < MAX` —
                        // gitlab-foss `migrator.rb#with_retry`,
                        // `source_internal_user_finder.rb#fetch_ghost_user`).
                        let mut cenv = env.clone();
                        if self.clause_retries(ast, c.span) {
                            widen_flow_writes(rebinds.iter(), begin_span, &mut cenv, interner);
                        }
                        // Under a retry edge the clause-entry widening
                        // (`rescue_writes`) holds for EVERY arm, not just
                        // the retrying one — a sibling can re-enter the
                        // `begin` through the retry too, so `ok = false;
                        // begin; ok = true; rescue A; retry; rescue B; if ok`
                        // stays silent on the oracle (rigor-rs#468).
                        if let Some(region) = body_region {
                            widen_clause_entry(rebinds, region, &indexed.scope_spans, &mut cenv, interner);
                        }
                        // …and a NON-retry sibling additionally reads the
                        // retrying arm's own writes as `Dynamic` — the
                        // reference's `absorb_retry_rebinds` feeds each
                        // `edge.retry_scopes` back into the shared `begin`
                        // env (`ok = false; begin; rescue A; ok = true;
                        // retry; rescue B; if ok` is silent on the oracle).
                        if !self.clause_retries(ast, c.span) {
                            for rs in &retry_spans {
                                widen_flow_writes(rebinds.iter(), *rs, &mut cenv, interner);
                            }
                        }
                        self.flow_eval_scope(
                            ast, &c.body, &mut cenv, in_loop_or_block, self_qual, self_kind, writes, rebinds, mutations, indexed, interner, out,
                        );
                        arm_envs.push(cenv);
                    }
                    // `ensure` runs on EVERY exit path — its entry env is
                    // the oracle's `exit_scope`:
                    // `reduce_scopes_with_nil_injection([primary_scope,
                    // *live_rescues])` (`statement_evaluator.rb:1331`) — the
                    // post-try env joined with every LIVE arm's exit env, so
                    // a write from any one path widens rather than folds
                    // (probed: `r = nil; begin; r = true; rescue; ensure;
                    // if r` is SILENT on the oracle; so are the
                    // rescue-arm-only and unbound variants). A terminating
                    // arm drops from `live_rescues` — its writes never reach
                    // `ensure` (`rescue; q = 1; raise; ensure; Float(q).w` is
                    // silent on the oracle; `q = 1; … rescue; q = 2; raise;
                    // ensure; if q` warns TRUTHY — the post-try `q` survives
                    // alone). With NO live arm (and with no clause at all)
                    // `exit_scope` IS the post-try env — `begin; r = true;
                    // ensure; if r` warns TRUTHY.
                    let mut eenv = {
                        let mut live = clauses
                            .iter()
                            .zip(arm_envs.iter())
                            .filter(|(c, _)| {
                                !self.arm_exits(ast, &c.body, &mut Vec::new())
                            })
                            .map(|(_, e)| e);
                        match live.next() {
                            None => scratch,
                            Some(first) => {
                                let mut j = join_flow_envs(&scratch, first, interner);
                                for cenv in live {
                                    j = join_flow_envs(&j, cenv, interner);
                                }
                                j
                            }
                        }
                    };
                    // Upstream #1837 (rigor #1231 / #1457): the `ensure`
                    // clause reads each local the body, its `else` or a rescue
                    // arm writes as `Dynamic[top]` — the region is everything
                    // ahead of the `ensure` statements (a method-level
                    // `ensure` included: its `begin` is the `def` body). The
                    // env past the `begin` is not this one, so the widening
                    // stays inside the clause.
                    if let Some(&first) = ensure_body.first() {
                        let region = (begin_span.0, ast.get(first).span().0);
                        widen_clause_entry(rebinds, region, &indexed.scope_spans, &mut eenv, interner);
                    }
                    self.flow_eval_scope(
                        ast, &ensure_body, &mut eenv, in_loop_or_block, self_qual, self_kind, writes, rebinds, mutations, indexed, interner, out,
                    );
                }
                let span = ast.get(id).span();
                let stored =
                    self.stored_slot_writes(ast, indexed, id, span, env, interner);
                widen_flow_writes(writes.iter(), span, env, interner);
                self.land_indexed_stored(
                    ast, indexed, id, span, rebinds, mutations, stored, env, interner,
                );
            }
            // A `while`/`until`/`for` loop: the same flat widen as `other`,
            // then the ADR-56 slice-B rebind fixpoint — see
            // [`Self::loop_rebind_fixpoint`]. A `begin … end while`, a `for`,
            // or a body holding `break`/`next`/`redo`/`retry` keeps the plain
            // widen (the replay arm declines the same shapes).
            Node::Loop { predicate, body, kind, .. } => {
                use crate::def_local_marks::ExitKind as K;
                let (predicate, body, kind) = (*predicate, body.clone(), *kind);
                let span = ast.get(id).span();
                let marks = self.def_local_marks(ast);
                let jumps =
                    marks.has_exit_within(span, &[K::Break, K::Next, K::Redo, K::Retry]);
                let once = matches!(
                    kind,
                    rigor_parse::LoopKind::While { begin_modifier: true }
                        | rigor_parse::LoopKind::Until { begin_modifier: true }
                );
                // The continuation reads the entry env — running it after
                // `widen_flow_writes` would overlay already-declined names
                // and always answer "converged". Overlay the continuation
                // AFTER the flat widen so a body write to a non-suspect name
                // survives, and a stabilizing self-op does not shed a
                // sibling pin (rigor-rs#638).
                let cont = if !(jumps || once || matches!(kind, rigor_parse::LoopKind::For)) {
                    self.loop_continuation(ast, env, predicate, &body, interner)
                } else {
                    None
                };
                let stored =
                    self.stored_slot_writes(ast, indexed, id, span, env, interner);
                widen_flow_writes(writes.iter(), span, env, interner);
                self.land_indexed_stored(
                    ast, indexed, id, span, rebinds, mutations, stored, env, interner,
                );
                match cont {
                    Some(LoopContinuation::Fixpoint(bindings)) => {
                        for (n, ty) in bindings {
                            env.insert(Rc::from(n.as_str()), ty);
                        }
                    }
                    Some(LoopContinuation::Structural(tracked, suspect)) => {
                        self.apply_loop_cap_widen(env, &tracked, &suspect, interner);
                    }
                    None => {}
                }
                self.widen_loop_exits_within(ast, span, env, interner);
            }
            // Loop / case / logical / call(+block) / any other node:
            // widen every local written in the span, do not descend for snapshots.
            // A `while`/`until` also widens the predicate locals its exit
            // edge changes (upstream #1359 — `while q; end; if q` is not
            // always-truthy on the reference).
            other => {
                let span = other.span();
                let stored =
                    self.stored_slot_writes(ast, indexed, id, span, env, interner);
                widen_flow_writes(writes.iter(), span, env, interner);
                self.land_indexed_stored(
                    ast, indexed, id, span, rebinds, mutations, stored, env, interner,
                );
                self.widen_loop_exits_within(ast, span, env, interner);
            }
        }
    }

    /// The recorded flow type for an `if`/`unless`/ternary predicate. Tries the
    /// ADR-0038 interprocedural literal-tail fold on an IMPLICIT-SELF predicate
    /// call first (resolved against the enclosing class `self_qual`/`self_kind`) —
    /// this is the one fold that needs the self context `type_of` lacks — then
    /// falls back to the ordinary `type_of` (which itself folds a `Const.method`
    /// predicate via `type_call`'s tier 4c). Producing a `Type::Constant` here is
    /// what makes `flow.always-truthy-condition` fire.
    fn flow_predicate_type(
        &self,
        ast: &LoweredAst,
        predicate: NodeId,
        env: &TypeEnv,
        self_qual: Option<&str>,
        self_kind: DefKind,
        interner: &mut Interner,
    ) -> TypeId {
        // `l && r` / `l || r`: a bare `type_of` declines a `Logical` to
        // `Dynamic`, but the oracle's `scope.evaluate` (`expression_typer.rb:
        // type_of_and_or`) gives the RESULT type — an elided edge contributes
        // its operand (`a = "x"; b = nil; if a && b` → `Constant[nil]`:
        // falsey `"x"` is `Bot` so the result is `b`'s), a live left edge
        // unions its surviving fragment with `r`'s type (`u = ENV["K"]; if u
        // && false` → `u | false` — NOT a Constant, so the oracle stays
        // silent there). Computing the union — not a minted `bool` — is what
        // keeps `check_always_truthy` firing exactly on the oracle's rows.
        // `r` is typed against the UN-narrowed env: the true port of
        // `evaluate` would narrow it by the left edge, a precision this
        // slice leaves to `type_of`'s own `Logical` coverage.
        if let Node::Logical { left, right, is_and, .. } = ast.get(predicate) {
            let lt = self.type_of(ast, *left, env, interner);
            let rt = self.type_of(ast, *right, env, interner);
            // The left edge's certainty: env type first, then the reach-fold
            // (`q = "x"; q.is_a?(Integer) || r` — `type_of` leaves the guard
            // call untyped but `expr_scalar` pins it `false`, so the result
            // IS `r`'s, exactly as the oracle's `eval_with_edges` reads the
            // pinned scope).
            let lc = self
                .predicate_certainty(lt, interner)
                .or_else(|| {
                    self.expr_scalar(ast, *left, &mut Vec::new())
                        .map(|s| !matches!(s, Scalar::Nil | Scalar::Bool(false)))
                });
            let res = match (lc, *is_and) {
                // Left edge is certain: the result is the decided operand —
                // `&&` on always-falsey keeps `l`, on always-truthy is `r`.
                (Some(false), true) | (Some(true), false) => lt,
                (Some(true), true) | (Some(false), false) => rt,
                // Both edges live: `falsey(l) | r` / `truthy(l) | r`.
                (None, true) => {
                    let fragment = self.narrow_falsey(lt, interner);
                    self.union_members(vec![fragment, rt], interner)
                }
                (None, false) => {
                    let fragment = self.narrow_truthy(lt, interner);
                    self.union_members(vec![fragment, rt], interner)
                }
            };
            return res;
        }
        if let Node::Call { receiver: None, method, block_body, .. } = ast.get(predicate) {
            if block_body.is_empty() {
                let method = method.clone();
                if let Some(q) = self_qual {
                    if let Some(scalar) = self.source.implicit_self_literal(self.index, q, self_kind, &method) {
                        return interner.intern(Type::Constant(scalar));
                    }
                }
                // rigor-rs#368 — a same-file TOPLEVEL `def helper = true`
                // folds `helper` the same way (the `definer`-keyed table
                // never sees it — `walk_fold_defs` harvests class/module
                // children only). The overridable degrade is applied inside
                // `file_def_literal`. Only this CALL shape may reach-fold:
                // locals/comparisons take the env answer — a reach-pin past
                // a mutator (`upcased.upcase!`) or a block-carried binding
                // would fold a predicate the oracle does not (fixture 111's
                // `if upcased == "ab"`).
                if let Some(scalar) =
                    self.file_def_literal(ast, &method, ast.get(predicate).span(), &mut Vec::new())
                {
                    return interner.intern(Type::Constant(scalar));
                }
            }
        }
        self.type_of(ast, predicate, env, interner)
    }

    /// Bind a single statement into `env` if it is a local write; recurse
    /// through a `Statements` wrapper. Other statements have no binding effect.
    ///
    /// A write recovered under a CROSSED block/lambda binds the closure's own
    /// local, not the enclosing one it shadows — `super { |o| o = 1 }` must not
    /// rebind the outer `o` (rigor-rs#137). [`LoweredAst::closure_bound_names`]
    /// carries exactly that bound set on the recovered child.
    fn bind_statement(&self, ast: &LoweredAst, id: NodeId, env: &mut TypeEnv, interner: &mut Interner) {
        // A write lowered inside a position whose post-scope the reference
        // DISCARDS (`Recovered::blocked` — a `when`/`in` condition under a
        // rescue modifier, a dead arm, a `super`/`yield` operand under a
        // wrapper) binds nothing: `x = (case v when (q = 1; Integer) then
        // 1 end) rescue nil` leaves `q` unbound and preserves an earlier
        // binding (rigor-rs#357 — the recovered sibling of the `Inert`
        // carrier arm below).
        if ast.in_blocked_carrier(ast.get(id).span()) {
            return;
        }
        match ast.get(id) {
            Node::LocalVariableWrite { name, value, .. } => {
                if ast.closure_bound_names(id).contains(name) {
                    return;
                }
                let (name, value) = (name.clone(), *value);
                let ty = self.type_of(ast, value, env, interner);
                drop_indexed_narrowings(env, &name);
                env.insert(Rc::from(name.as_str()), ty);
            }
            Node::MultiWrite { targets, value, .. } => {
                let (targets, value) = (targets.clone(), *value);
                let bound = ast.closure_bound_names(id);
                let rhs = self.type_of(ast, value, env, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    if bound.contains(&name) {
                        continue;
                    }
                    drop_indexed_narrowings(env, &name);
                    env.insert(Rc::from(name.as_str()), ty);
                }
            }
            // A write under `defined?` / `END` / `BEGIN` / `super` / `yield`
            // never reaches the scope on the reference (rigor-rs#153), so the
            // flat env does not bind it either (`type-of` on the later read
            // says the earlier type, as the reference does). A recovery
            // carrier is still bound as before: this env has no widening.
            Node::Statements { kind: StatementsKind::Inert, .. } => {}
            Node::Statements { body, .. } => {
                for s in body.clone() {
                    self.bind_statement(ast, s, env, interner);
                }
            }
            // `h[k] op= v` — a compound index write binds no LOCAL name, but a
            // write nested in its operands (`h[:a] ||= (y = 5)`) binds as it
            // did under the old recovered carrier this replaces.
            Node::IndexWrite {
                receiver,
                indices,
                value,
                ..
            } => {
                let children: Vec<NodeId> = receiver
                    .iter()
                    .chain(indices.iter())
                    .chain(std::iter::once(value))
                    .copied()
                    .collect();
                for c in children {
                    self.bind_statement(ast, c, env, interner);
                }
            }
            // `recv.attr op= v` — a compound ATTRIBUTE write binds no local
            // itself; nested writes in its operands (`h.default ||= (x = 1)`)
            // bind as they did under the recovered carrier (rigor-rs#343).
            Node::If { predicate, .. } => {
                // rigor-rs#451 — an `if`/`unless` whose predicate narrows a
                // local joins the arms' edge types on the reference, so the
                // post-`if` name is `Dynamic` unless BOTH edges keep the
                // carrier (`if h.empty?; end; h.frob` is silent there; this
                // binder's plain answer, `Array`, is the FP). A decided
                // predicate is the folded `if` the dead-arm machinery owns
                // — it narrows nothing.
                if self.known_polarity(ast, env, *predicate, interner).is_none() {
                    for (subject, _) in self.subjects_within(ast, *predicate) {
                        let crate::loop_edges::Subject::Local(name) = &subject else {
                            continue;
                        };
                        let ty = env
                            .get(name.as_str())
                            .copied()
                            .unwrap_or_else(|| interner.untyped());
                        if matches!(interner.get(ty), Type::Dynamic(_) | Type::Top) {
                            continue;
                        }
                        let t = self.subject_edge(
                            ast,
                            *predicate,
                            &subject,
                            ty,
                            true,
                            false,
                            interner,
                        );
                        let f = self.subject_edge(
                            ast,
                            *predicate,
                            &subject,
                            ty,
                            false,
                            false,
                            interner,
                        );
                        if t == crate::loop_edges::Edge::Other
                            || f == crate::loop_edges::Edge::Other
                        {
                            let u = interner.untyped();
                            env.insert(Rc::from(name.as_str()), u);
                        }
                    }
                }
            }
            Node::AttrWrite {
                receiver, value, ..
            } => {
                let children: Vec<NodeId> = receiver
                    .iter()
                    .chain(std::iter::once(value))
                    .copied()
                    .collect();
                for c in children {
                    self.bind_statement(ast, c, env, interner);
                }
            }
            _ => {}
        }
    }
}

/// rigor-rs#393 — drop every indexed narrowing rooted at a local a retrying
/// `begin`'s edge rebinds ([`IndexedFlow::retry_drops`]): the `begin` at
/// exactly `span` when `exact` (a site inside it), else every one inside
/// `span` (the env past a statement holding it).
fn drop_retry_edge_records(
    indexed: &IndexedFlow,
    span: rigor_parse::Span,
    exact: bool,
    env: &mut TypeEnv,
) {
    for (begin, name) in &indexed.retry_drops {
        let hit = if exact { *begin == span } else { within_span(*begin, span) };
        if hit {
            drop_indexed_narrowings(env, name);
        }
    }
}

/// Half-open span containment.
fn within_span(site: rigor_parse::Span, outer: rigor_parse::Span) -> bool {
    outer.0 <= site.0 && site.1 <= outer.1
}

/// One edge of [`Typer::flow_children`]: how certain the child's own position
/// is to evaluate.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FlowEdge {
    /// Always evaluates in position (a receiver, a positional argument, a
    /// sequence statement, an `if`/`case` predicate, a write's value).
    Uncond,
    /// May not evaluate (a `&&`/`||` right operand, a branch body, a loop
    /// body, a rescue/else/ensure clause, a `case`/`when` arm, a `&expr`
    /// block-pass operand the port cannot prove ran).
    Cond,
    /// A scope that captures the whole env rather than evaluating inline —
    /// a literal block body, a lambda, a `def`/`class`/`module` body.
    Barrier,
}

/// The flags one node carries in [`PathMarks`] — what each `path_*`
/// predicate's edge walk would have derived for the path from the replay
/// root to the node.
#[derive(Clone, Copy)]
struct PathFlags {
    /// [`Typer::path_unconditional`]: every edge on the path `Uncond`.
    uncond: bool,
    /// [`Typer::path_operand_evaluated`]: every edge `Uncond`, or out of a
    /// `Recovered`/`Jump` carrier.
    operand: bool,
    /// [`Typer::path_content_writeback`]: some edge on the path a content
    /// write-back boundary (`Barrier`, or a `while`/`until` body child).
    writeback: bool,
}

/// rigor-rs#658 — one DFS's worth of `path_*` answers for a replay `root`,
/// keyed by node span. The mutator replay asks the three predicates once
/// per contained mutation from the same root; the recursive walks shared
/// their path prefixes, so a `v.push(1)` × N chain went cubic. A `wspan`
/// that is no node's own span (a `for`/multi-write index target, an
/// operand write inside a larger node) has no entry — the caller falls
/// back to the recursive walk, keeping the off-child rules verbatim.
#[derive(Default)]
pub(crate) struct PathMarks {
    by_span: HashMap<rigor_parse::Span, PathFlags>,
}

impl PathMarks {
    /// [`Typer::path_unconditional`]'s answer when `wspan` is some node's
    /// own span; `None` when no node carries it.
    fn uncond(&self, wspan: rigor_parse::Span) -> Option<bool> {
        self.by_span.get(&wspan).map(|f| f.uncond)
    }

    /// [`Typer::path_operand_evaluated`]'s answer, like [`Self::uncond`].
    fn operand(&self, wspan: rigor_parse::Span) -> Option<bool> {
        self.by_span.get(&wspan).map(|f| f.operand)
    }

    /// [`Typer::path_content_writeback`]'s answer, like [`Self::uncond`].
    fn writeback(&self, wspan: rigor_parse::Span) -> Option<bool> {
        self.by_span.get(&wspan).map(|f| f.writeback)
    }
}

/// The flat-env effect tables one statement's replay consults —
/// [`toplevel_rebinds`] and [`toplevel_mutations`] — with a start-sorted
/// position index for span slicing (rigor-rs#424). Every consumer of a
/// statement's effects keeps only the entries INSIDE the statement's own
/// span, so the per-statement work used to be an O(effects) scan to find a
/// handful of in-span candidates — quadratic on a flat generated file
/// (`x_i = 1 if y` × N). `within` slices the sorted index to the span's
/// start range instead: O(log effects + in-span), original order inside.
pub(crate) struct FlowEffects {
    /// [`toplevel_rebinds`]: every top-level local write `(span, name)`.
    pub(crate) rebinds: Vec<(rigor_parse::Span, String)>,
    /// [`toplevel_mutations`]: every `local.<mutator>` call
    /// `(call span, name, method, drop_key)`.
    pub(crate) mutations: Vec<(rigor_parse::Span, String, String, Option<ShapeKey>)>,
    /// Positions into `rebinds`, sorted by span start (stable — the
    /// tables' own order inside an equal start).
    rebind_starts: Vec<u32>,
    /// Positions into `mutations`, same rule.
    mutation_starts: Vec<u32>,
}

impl FlowEffects {
    pub(crate) fn new(
        rebinds: Vec<(rigor_parse::Span, String)>,
        mutations: Vec<(rigor_parse::Span, String, String, Option<ShapeKey>)>,
    ) -> Self {
        let mut rebind_starts: Vec<u32> = (0..rebinds.len() as u32).collect();
        rebind_starts.sort_by_key(|&i| rebinds[i as usize].0 .0);
        let mut mutation_starts: Vec<u32> = (0..mutations.len() as u32).collect();
        mutation_starts.sort_by_key(|&i| mutations[i as usize].0 .0);
        FlowEffects { rebinds, mutations, rebind_starts, mutation_starts }
    }

    /// The positions of `table`'s span-keyed entries whose START falls in
    /// `span`, IN `table` ORDER — the table's arena order is post-order
    /// (children lower before parents), and consumers like
    /// `apply_mutation_effects`/`arm_write_ty` sequence effects by it, so
    /// the slice's indices are sorted back into table order rather than
    /// yielded start-sorted (an outer mutator must not run before the
    /// `h[k] ||= v` nested inside it). The `w.1 <= span.1` half of
    /// containment is still per-candidate (a span never properly crosses
    /// a boundary, but the callers' predicates are kept verbatim).
    fn within<'a, T: 'a>(
        table: &'a [T],
        starts: &'a [u32],
        span: rigor_parse::Span,
        at: impl Fn(&'a T) -> rigor_parse::Span + 'a,
    ) -> impl Iterator<Item = &'a T> {
        let lo = starts.partition_point(|&i| at(&table[i as usize]).0 < span.0);
        let hi = starts.partition_point(|&i| at(&table[i as usize]).0 <= span.1);
        let mut order = starts[lo..hi].to_vec();
        order.sort_unstable();
        order.into_iter().map(move |i| &table[i as usize])
    }

    /// `rebinds` entries starting inside `span` (`w.0 <= span.1` — the end
    /// check is the caller's).
    pub(crate) fn rebinds_within(
        &self,
        span: rigor_parse::Span,
    ) -> impl Iterator<Item = &(rigor_parse::Span, String)> {
        Self::within(&self.rebinds, &self.rebind_starts, span, |r| r.0)
    }

    /// `mutations` entries starting inside `span`, same rule.
    pub(crate) fn mutations_within(
        &self,
        span: rigor_parse::Span,
    ) -> impl Iterator<Item = &(rigor_parse::Span, String, String, Option<ShapeKey>)> {
        Self::within(&self.mutations, &self.mutation_starts, span, |m| m.0)
    }
}

/// The flat check env plus the data [`Typer::check_env_at`] needs to
/// reconstruct the scope a use site was ENTERED from — the port of the
/// reference's per-node scope index (`OperandWalk`, upstream rigor#1310 /
/// rigor-rs#136). `boundaries` records the env before each top-level
/// statement; `rebinds`/`mutations` are the span-keyed effects the ordered
/// replay applies inside it.
pub struct CheckFlow {
    /// The end-of-file flat env — [`Typer::build_toplevel_check_env`]'s
    /// whole answer, and the fallback for sites inside a barrier scope or
    /// outside every recorded statement.
    pub env: TypeEnv,
    /// `(top-level statement index, env before it)` — a CHECKPOINT every
    /// `boundary_stride` statements, not every one: `check_env_at` replays
    /// `bind_check_statement` over the (bounded) gap instead of storing an
    /// env per statement — N full env clones were O(N²) time AND memory on
    /// a flat generated file (rigor-rs#424). Empty unless the file has any
    /// recorded effect at all. Index 0 (the empty env) is always stored
    /// when the table is non-empty.
    boundaries: Vec<(usize, TypeEnv)>,
    /// Every top-level `(statement, span)` in program order — the
    /// `check_env_at` containing-statement lookup (binary search) and the
    /// replay range between a checkpoint and a queried statement.
    boundary_stmts: Vec<(NodeId, rigor_parse::Span)>,
    /// The statements between consecutive `boundaries` entries — the
    /// replay bound.
    boundary_stride: usize,
    /// The flat-env effect tables ([`FlowEffects`]).
    effects: FlowEffects,
    /// [`closure_mutations`]: every `h.attr op= v` a literal block/lambda
    /// body's own scope evaluates — `(body owner node, write span, receiver
    /// name)` — replayed only for a site inside that body (rigor-rs#366).
    closure_mutations: Vec<(NodeId, rigor_parse::Span, String)>,
    /// [`collect_indexed_flow`]: the indexed-narrowing side table
    /// (rigor-rs#325) — `operand` `h[k] ||= v` records, their spans, and
    /// element-mutator calls.
    indexed: IndexedFlow,
    /// rigor-rs#368 — the file's provably-dead positions (folded
    /// `if`/`unless` arms and terminating `rescue` arms); `check_env_at`
    /// floors a dead arm's predicate subjects to `Bot` and `entry_descend`
    /// skips a dead arm's internal effects — the reference never evaluates
    /// the arm (`live_branch_for_if` / `live_rescues`). Shared with the
    /// `Typer`'s `dead_cache` entry — `Rc` so neither clones the set.
    dead: std::rc::Rc<crate::dead::DeadPositions>,
    /// Upstream #1359 — every `while`/`until` loop whose predicate reads a
    /// local, with the data [`Typer::loop_edge_overlay`] needs.
    loops: Vec<LoopEdges>,
    /// Every `if`/`unless` whose predicate reads a local — the per-edge
    /// narrowing an inside-arm or post-`if` site reads (rigor-rs#451).
    ifs: Vec<IfEdges>,
    /// rigor-rs#405 — every class guard whose local holds an indexed
    /// record, for [`Typer::record_guard_overlay`].
    record_guards: Vec<crate::record_guards::RecordGuard>,
}

/// One `while`/`until` loop for [`Typer::loop_edge_overlay`].
struct LoopEdges {
    id: NodeId,
    span: rigor_parse::Span,
    body: Option<rigor_parse::Span>,
    /// The innermost `def` around the loop — a site outside it reads a
    /// different local scope.
    def: Option<rigor_parse::Span>,
    /// The innermost block / `->` body around the loop: its exit edge
    /// narrows that closure's scope only.
    closure: Option<rigor_parse::Span>,
    /// `(predicate local, first write of it after the loop)`.
    locals: Vec<(String, Option<usize>)>,
}

/// One `if`/`unless` for [`Typer::if_edge_overlay`].
struct IfEdges {
    predicate: NodeId,
    span: rigor_parse::Span,
    /// `then_body`'s hull / `else_body`'s hull — a site inside one reads its
    /// edge's narrowed locals.
    then_hull: Option<rigor_parse::Span>,
    else_hull: Option<rigor_parse::Span>,
    is_unless: bool,
    def: Option<rigor_parse::Span>,
    closure: Option<rigor_parse::Span>,
    /// `(predicate local, first write of it inside or after the if)` — a
    /// later binding the overlay must not cover.
    locals: Vec<(String, Option<usize>)>,
}
