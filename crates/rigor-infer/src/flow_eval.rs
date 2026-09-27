//! Top-level flow evaluation: the straight-line env builders
//! ([`Typer::build_toplevel_env`], [`Typer::build_toplevel_check_env`]) and
//! the branch-joined local-constant propagation behind
//! [`Typer::always_truthy_snapshots`] (ADR-0022), which
//! `flow.always-truthy-condition` reads.

use std::collections::HashMap;

use rigor_parse::{LoweredAst, Node, NodeId, StatementsKind};
use rigor_types::{Interner, Type, TypeId};

use crate::{
    collect_flow_writes, indexed_flow_writes, join_flow_envs, multi_target_binder, qualify_self,
    toplevel_rebinds, widen_flow_writes, DefKind, TypeEnv, Typer,
};

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
        let mut env = TypeEnv::new();
        let body = match ast.get(ast.root()) {
            Node::Program { body, .. } => body.clone(),
            _ => return env,
        };
        let rebinds = toplevel_rebinds(ast);
        for stmt in body {
            self.bind_check_statement(ast, stmt, &mut env, &rebinds, interner);
        }
        env
    }

    /// One statement of [`Self::build_toplevel_check_env`]: a direct write binds
    /// as [`Self::bind_statement`] does, after widening the rebinds nested in
    /// its value (`x = xs.each { |e| w = e }`); any other statement widens every
    /// rebind inside it.
    fn bind_check_statement(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        env: &mut TypeEnv,
        rebinds: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
    ) {
        match ast.get(id) {
            Node::LocalVariableWrite { value, .. } | Node::MultiWrite { value, .. } => {
                let vspan = ast.get(*value).span();
                widen_flow_writes(rebinds, vspan, env, interner);
                self.bind_statement(ast, id, env, interner);
            }
            // Only a real statement sequence is straight-line code. A recovery
            // carrier (a `rescue` modifier, `super(…)`, …) runs its writes
            // conditionally or out of order, so it widens; an inert one
            // (`defined?`, `END`, `BEGIN`) has no writes in `rebinds` and so
            // changes nothing (rigor-rs#153).
            Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
                for s in body.clone() {
                    self.bind_check_statement(ast, s, env, rebinds, interner);
                }
            }
            other => widen_flow_writes(rebinds, other.span(), env, interner),
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
        let body = match ast.get(ast.root()) {
            Node::Program { body, .. } => body.clone(),
            _ => return out,
        };
        let mut env = TypeEnv::new();
        self.flow_eval_scope(ast, &body, &mut env, false, None, DefKind::Instance, &writes, interner, &mut out);
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
        interner: &mut Interner,
        out: &mut HashMap<NodeId, TypeId>,
    ) {
        for &s in stmts {
            self.flow_eval_stmt(ast, s, env, in_loop_or_block, self_qual, self_kind, writes, interner, out);
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
        interner: &mut Interner,
        out: &mut HashMap<NodeId, TypeId>,
    ) {
        match ast.get(id) {
            // A recovery / inert carrier is not straight-line code: it goes to
            // the widening arm below (rigor-rs#153). An inert carrier's writes
            // are not in `writes`, so it leaves `env` as the reference does.
            Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
                let body = body.clone();
                self.flow_eval_scope(ast, &body, env, in_loop_or_block, self_qual, self_kind, writes, interner, out);
            }
            Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                // A value expression may itself write OTHER locals (`x = (y = 5)`)
                // or capture-write via a block — widen those first, then bind.
                let vspan = ast.get(value).span();
                widen_flow_writes(writes, vspan, env, interner);
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
                env.insert(name, ty);
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
                widen_flow_writes(writes, vspan, env, interner);
                let rhs = self.type_of(ast, value, env, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    env.insert(name, ty);
                }
            }
            Node::LocalVariableOpWrite { name, .. } => {
                // `x += 1` / `x ||= 5` reads-then-writes; the result is not a
                // tracked constant in this slice — widen.
                let name = name.clone();
                let u = interner.untyped();
                env.insert(name, u);
            }
            Node::If { predicate, then_body, else_body, .. } => {
                let (predicate, then_body, else_body) =
                    (*predicate, then_body.clone(), else_body.clone());
                if !in_loop_or_block {
                    let pty = self.flow_predicate_type(
                        ast, predicate, env, self_qual, self_kind, interner,
                    );
                    out.insert(id, pty);
                }
                // Independently evaluate each branch from the dominating env, then
                // join: a binding survives only if both branches agree exactly.
                let mut then_env = env.clone();
                self.flow_eval_scope(
                    ast, &then_body, &mut then_env, in_loop_or_block, self_qual, self_kind, writes, interner, out,
                );
                let mut else_env = env.clone();
                self.flow_eval_scope(
                    ast, &else_body, &mut else_env, in_loop_or_block, self_qual, self_kind, writes, interner, out,
                );
                *env = join_flow_envs(&then_env, &else_env, interner);
                // A predicate may contain a write (`if (x = f)`); widen post-join.
                let pspan = ast.get(predicate).span();
                widen_flow_writes(writes, pspan, env, interner);
            }
            Node::Definition { body, singleton_name, .. } => {
                // Independent scope: fresh local env, inherited suppression flag.
                // The self KIND flips to singleton inside a `def self.x` (so an
                // implicit-self call there resolves against the owner's singleton
                // table); the enclosing class QUALIFIED name is unchanged.
                let (body, kind) = (
                    body.clone(),
                    if singleton_name.is_some() { DefKind::Singleton } else { DefKind::Instance },
                );
                let mut fresh = TypeEnv::new();
                self.flow_eval_scope(
                    ast, &body, &mut fresh, in_loop_or_block, self_qual, kind, writes, interner, out,
                );
            }
            Node::ClassDef { body, name, .. } | Node::ModuleDef { body, name, .. } => {
                // Independent scope: fresh local env, inherited suppression flag.
                // Extend the lexical self-qualified name so a nested class/module's
                // implicit-self calls resolve against the right owner; a body-level
                // call defaults to instance kind until a `def self.x` flips it.
                let (body, child_qual) = (body.clone(), qualify_self(self_qual, name));
                let mut fresh = TypeEnv::new();
                self.flow_eval_scope(
                    ast, &body, &mut fresh, in_loop_or_block, Some(&child_qual), DefKind::Instance, writes, interner, out,
                );
            }
            // Loop / case / begin-rescue / logical / call(+block) / any other node:
            // widen every local written in the span, do not descend for snapshots.
            other => {
                widen_flow_writes(writes, other.span(), env, interner);
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
        if let Node::Call { receiver: None, method, block_body, .. } = ast.get(predicate) {
            if block_body.is_empty() {
                let method = method.clone();
                if let Some(q) = self_qual {
                    if let Some(scalar) = self.source.implicit_self_literal(q, self_kind, &method) {
                        return interner.intern(Type::Constant(scalar));
                    }
                }
            }
        }
        self.type_of(ast, predicate, env, interner)
    }

    /// Bind a single statement into `env` if it is a local write; recurse
    /// through a `Statements` wrapper. Other statements have no binding effect.
    fn bind_statement(&self, ast: &LoweredAst, id: NodeId, env: &mut TypeEnv, interner: &mut Interner) {
        match ast.get(id) {
            Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                let ty = self.type_of(ast, value, env, interner);
                env.insert(name, ty);
            }
            Node::MultiWrite { targets, value, .. } => {
                let (targets, value) = (targets.clone(), *value);
                let rhs = self.type_of(ast, value, env, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    env.insert(name, ty);
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
            _ => {}
        }
    }
}
