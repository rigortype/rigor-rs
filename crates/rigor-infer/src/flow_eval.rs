//! Top-level flow evaluation: the straight-line env builders
//! ([`Typer::build_toplevel_env`], [`Typer::build_toplevel_check_env`]) and
//! the branch-joined local-constant propagation behind
//! [`Typer::always_truthy_snapshots`] (ADR-0022), which
//! `flow.always-truthy-condition` reads.

use std::borrow::Cow;
use std::collections::HashMap;

use rigor_parse::{LoweredAst, Node, NodeId, StatementsKind};
use rigor_types::{Interner, Type, TypeId};

use crate::{
    collect_flow_writes, indexed_flow_writes, join_flow_envs, multi_target_binder, qualify_self,
    toplevel_mutations, toplevel_rebinds, widen_flow_writes, DefKind, TypeEnv, Typer,
    ARRAY_MUTATORS, HASH_MUTATORS,
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
                    rebinds: Vec::new(),
                    mutations: Vec::new(),
                }
            }
        };
        let rebinds = toplevel_rebinds(ast);
        let mutations = toplevel_mutations(ast);
        // Boundaries only matter while a recorded effect could postdate a use
        // site; a file with none gets the flat env for every site.
        let record = !(rebinds.is_empty() && mutations.is_empty());
        let mut boundaries = Vec::new();
        for stmt in body {
            if record {
                boundaries.push((stmt, env.clone()));
            }
            self.bind_check_statement(ast, stmt, &mut env, &rebinds, &mutations, interner);
        }
        CheckFlow {
            env,
            boundaries,
            rebinds,
            mutations,
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
    /// Sites inside a barrier scope (literal block body, lambda, `def`,
    /// `class`, `module` body) get the flat env: a closure may run at any
    /// later point, which is the answer `ScopedEnv::at` gave before this
    /// pass. So does a site with no recorded effect at or after its
    /// statement — the replayed env is then byte-identical to the flat one.
    pub fn check_env_at<'f>(
        &self,
        ast: &LoweredAst,
        flow: &'f CheckFlow,
        site: rigor_parse::Span,
        interner: &mut Interner,
    ) -> Cow<'f, TypeEnv> {
        let Some(&(stmt, ref pre)) = flow.boundaries.iter().find(|(id, _)| {
            let s = ast.get(*id).span();
            s.0 <= site.0 && site.1 <= s.1
        }) else {
            return Cow::Borrowed(&flow.env);
        };
        let stmt_span = ast.get(stmt).span();
        let later = flow.rebinds.iter().any(|(w, _)| w.0 >= stmt_span.0)
            || flow.mutations.iter().any(|(w, _, _)| w.0 >= stmt_span.0);
        if !later {
            return Cow::Borrowed(&flow.env);
        }
        let mut env = pre.clone();
        self.entry_descend(ast, stmt, site, true, &mut env, flow, interner);
        Cow::Owned(env)
    }

    /// Replay `id`'s contained effects into `env` in evaluation order until
    /// the node holding `site` is reached. `uncond` says `id`'s own position
    /// evaluates unconditionally — a mutation on an all-unconditional path
    /// mints the widened nominal (the flat env's own answer for a
    /// same-statement mutator call); anything else widens `Dynamic`, the
    /// flat env's conservative decline for a conditional position.
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
                ..
            } => {
                let (predicate, then_body, else_body) =
                    (*predicate, then_body.clone(), else_body.clone());
                let pspan = ast.get(predicate).span();
                if pspan.0 <= site.0 && site.1 <= pspan.1 {
                    self.entry_descend(ast, predicate, site, uncond, env, flow, interner);
                    return;
                }
                self.apply_subtree_effects(ast, predicate, uncond, env, flow, interner);
                let branch: &[NodeId] = if then_body
                    .iter()
                    .any(|&s| within_span(site, ast.get(s).span()))
                {
                    &then_body
                } else if else_body
                    .iter()
                    .any(|&s| within_span(site, ast.get(s).span()))
                {
                    &else_body
                } else {
                    return; // the site is the `if`'s own span, no child holds it
                };
                for &s in branch {
                    let sspan = ast.get(s).span();
                    if sspan.0 <= site.0 && site.0 < sspan.1 {
                        self.entry_descend(ast, s, site, false, env, flow, interner);
                        return;
                    }
                    if sspan.1 <= site.0 {
                        self.apply_subtree_effects(ast, s, false, env, flow, interner);
                    }
                }
            }
            // A `begin`'s main body runs in order unconditionally; a rescue
            // clause, the else arm and `ensure` are conditional or later, so
            // a site inside one takes every contained effect conservatively
            // — the flat env's own decline.
            Node::BeginRescue { main_body, .. } => {
                let main_body = main_body.clone();
                if main_body
                    .iter()
                    .any(|&s| within_span(site, ast.get(s).span()))
                {
                    self.entry_children(ast, id, site, uncond, env, flow, interner);
                } else {
                    self.apply_subtree_effects(ast, id, false, env, flow, interner);
                }
            }
            // Conditional container: nothing inside orders against the site
            // (recovery carrier, loop, case/when), so apply every contained
            // effect conservatively — byte-identical to the flat env.
            Node::Statements { kind, .. } if !matches!(kind, StatementsKind::Sequence) => {
                self.apply_subtree_effects(ast, id, false, env, flow, interner);
            }
            Node::Loop { .. } | Node::Case { .. } | Node::When { .. } => {
                self.apply_subtree_effects(ast, id, false, env, flow, interner);
            }
            // A closure body or class/module body captures the whole env —
            // today's `ScopedEnv::at` answer, kept verbatim.
            Node::Lambda { .. }
            | Node::Definition { .. }
            | Node::ClassDef { .. }
            | Node::ModuleDef { .. } => {
                *env = flow.env.clone();
            }
            _ => self.entry_children(ast, id, site, uncond, env, flow, interner),
        }
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
        env: &mut TypeEnv,
        flow: &CheckFlow,
        interner: &mut Interner,
    ) {
        for (child, edge) in self.flow_children(ast.get(id)) {
            let cspan = ast.get(child).span();
            if cspan.0 <= site.0 && site.0 < cspan.1 {
                if edge == FlowEdge::Barrier {
                    *env = flow.env.clone();
                } else {
                    self.entry_descend(
                        ast,
                        child,
                        site,
                        uncond && edge == FlowEdge::Uncond,
                        env,
                        flow,
                        interner,
                    );
                }
                return;
            }
            if cspan.1 <= site.0 && edge != FlowEdge::Barrier {
                self.apply_subtree_effects(
                    ast,
                    child,
                    uncond && edge == FlowEdge::Uncond,
                    env,
                    flow,
                    interner,
                );
            }
        }
    }

    /// Apply every recorded effect inside `id`'s span to `env`: a rebind
    /// always widens `Dynamic` (the flat env's own envelope); a mutation
    /// mints the unconditional nominal only when it sits on an
    /// all-`Uncond` descent from `id` — a conditional position widens
    /// `Dynamic`, byte-identical to the flat env.
    fn apply_subtree_effects(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        uncond: bool,
        env: &mut TypeEnv,
        flow: &CheckFlow,
        interner: &mut Interner,
    ) {
        let span = ast.get(id).span();
        for (wspan, name) in &flow.rebinds {
            if wspan.0 >= span.0 && wspan.1 <= span.1 {
                let u = interner.untyped();
                env.insert(name.clone(), u);
            }
        }
        for (wspan, name, method) in &flow.mutations {
            if wspan.0 >= span.0 && wspan.1 <= span.1 {
                if uncond && self.path_unconditional(ast, id, *wspan) {
                    let Some(&pre) = env.get(name.as_str()) else {
                        continue;
                    };
                    if let Some(widened) = self.widen_mutated_binding(pre, method, interner) {
                        env.insert(name.clone(), widened);
                    }
                } else {
                    let u = interner.untyped();
                    env.insert(name.clone(), u);
                }
            }
        }
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
            Node::Loop { index_writes, .. } => {
                if index_writes
                    .iter()
                    .any(|(_, s)| s.0 <= wspan.0 && wspan.1 <= s.1)
                {
                    return false;
                }
            }
            Node::BeginRescue { clauses, .. } => {
                if clauses
                    .iter()
                    .any(|c| c.span.0 <= wspan.0 && wspan.1 <= c.span.1)
                {
                    return false;
                }
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
                // A literal block body is a scope barrier (it may run at any
                // later point, so it keeps the flat env); a `&expr`
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
    fn bind_check_statement(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        env: &mut TypeEnv,
        rebinds: &[(rigor_parse::Span, String)],
        mutations: &[(rigor_parse::Span, String, String)],
        interner: &mut Interner,
    ) {
        match ast.get(id) {
            Node::LocalVariableWrite { value, .. } | Node::MultiWrite { value, .. } => {
                let vspan = ast.get(*value).span();
                widen_flow_writes(rebinds, vspan, env, interner);
                self.widen_mutated_locals(ast, mutations, *value, vspan, env, interner);
                self.bind_statement(ast, id, env, interner);
                // An `h[k]` index target stores through `[]=` on the POST-binding
                // scope — `swap, swap[:a] = swap, 1` stores into the object `swap`
                // was just bound to — so the reference widens each receiver AFTER
                // `MultiTargetBinder` applies (`eval_multi_write`, rigor-rs#134).
                // Widening by the TARGET span mints the unconditional carrier
                // (`path_unconditional` reaches it through `target_exprs`),
                // as a straight-line `h[k] = v` gets.
                if let Node::MultiWrite { targets, .. } = ast.get(id) {
                    for (_, tspan) in targets.index_writes() {
                        self.widen_mutated_locals(ast, mutations, id, tspan, env, interner);
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
                    self.bind_check_statement(ast, s, env, rebinds, mutations, interner);
                }
            }
            other => {
                let span = other.span();
                widen_flow_writes(rebinds, span, env, interner);
                self.widen_mutated_locals(ast, mutations, id, span, env, interner);
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
    fn widen_mutated_locals(
        &self,
        ast: &LoweredAst,
        mutations: &[(rigor_parse::Span, String, String)],
        root: NodeId,
        span: rigor_parse::Span,
        env: &mut TypeEnv,
        interner: &mut Interner,
    ) {
        for (wspan, name, method) in mutations {
            if !(span.0 <= wspan.0 && wspan.1 <= span.1) {
                continue;
            }
            let Some(&pre) = env.get(name.as_str()) else {
                continue;
            };
            let ty = if self.path_unconditional(ast, root, *wspan) {
                let Some(widened) = self.widen_mutated_binding(pre, method, interner) else {
                    continue;
                };
                widened
            } else {
                interner.untyped()
            };
            env.insert(name.clone(), ty);
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
    fn widen_mutated_binding(
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
            Type::Union(members) => {
                let members = members.clone();
                let mut out = Vec::with_capacity(members.len());
                let mut changed = false;
                for m in members {
                    match self.widen_mutated_binding(m, method, interner) {
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
                // An `h[k]` index target stores through `[]=` on the
                // POST-binding scope (`h, h[:a] = h, 1` stores into the rebound
                // `h`), so each receiver's locals widen after the bindings —
                // exactly as `h[k] = v` widens them (`eval_multi_write` →
                // `IndexWriteWidening.widen`, rigor-rs#134).
                for (_, tspan) in targets.index_writes() {
                    widen_flow_writes(writes, tspan, env, interner);
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
    ///
    /// A write recovered under a CROSSED block/lambda binds the closure's own
    /// local, not the enclosing one it shadows — `super { |o| o = 1 }` must not
    /// rebind the outer `o` (rigor-rs#137). [`LoweredAst::closure_bound_names`]
    /// carries exactly that bound set on the recovered child.
    fn bind_statement(&self, ast: &LoweredAst, id: NodeId, env: &mut TypeEnv, interner: &mut Interner) {
        match ast.get(id) {
            Node::LocalVariableWrite { name, value, .. } => {
                if ast.closure_bound_names(id).contains(name) {
                    return;
                }
                let (name, value) = (name.clone(), *value);
                let ty = self.type_of(ast, value, env, interner);
                env.insert(name, ty);
            }
            Node::MultiWrite { targets, value, .. } => {
                let (targets, value) = (targets.clone(), *value);
                let bound = ast.closure_bound_names(id);
                let rhs = self.type_of(ast, value, env, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    if bound.contains(&name) {
                        continue;
                    }
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
            _ => {}
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
    /// `(top-level statement, env before it)` in program order. Empty unless
    /// the file has any recorded effect at all.
    boundaries: Vec<(NodeId, TypeEnv)>,
    /// [`toplevel_rebinds`]: every top-level local write `(span, name)`.
    rebinds: Vec<(rigor_parse::Span, String)>,
    /// [`toplevel_mutations`]: every `local.<mutator>` call
    /// `(call span, name, method)`.
    mutations: Vec<(rigor_parse::Span, String, String)>,
}
