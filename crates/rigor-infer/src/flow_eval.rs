//! Top-level flow evaluation: the straight-line env builders
//! ([`Typer::build_toplevel_env`], [`Typer::build_toplevel_check_env`]) and
//! the branch-joined local-constant propagation behind
//! [`Typer::always_truthy_snapshots`] (ADR-0022), which
//! `flow.always-truthy-condition` reads.

use std::borrow::Cow;
use std::collections::HashMap;

use rigor_parse::{LoweredAst, Node, NodeId, StatementsKind};
use rigor_types::{Interner, Scalar, ShapeKey, Type, TypeId};

use crate::{
    closure_mutations, collect_flow_writes, indexed_flow_writes, join_flow_envs,
    multi_target_binder, qualify_self, toplevel_mutations, toplevel_rebinds, widen_flow_writes,
    DefKind, TypeEnv, Typer, ARRAY_MUTATORS, HASH_MUTATORS,
};
use crate::dead::DeadOp;
use crate::flow_writes::{
    collect_indexed_flow, drop_indexed_narrowings, indexed_narrowing_key, IndexedFlow,
    MUTATOR_METHODS, STRING_MUTATORS,
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
                    closure_mutations: Vec::new(),
                    indexed: IndexedFlow::default(),
                    dead: std::rc::Rc::new(crate::dead::DeadPositions::default()),
                }
            }
        };
        // rigor-rs#368 — provably-dead positions (computed once for the file;
        // `seen` starts empty — the predicate fold is a fresh walk).
        let dead = self.dead_positions(ast, &mut Vec::new());
        let mut rebinds = toplevel_rebinds(ast);
        let mut mutations = toplevel_mutations(ast);
        let mut closure_muts = closure_mutations(ast);
        // A write or mutation inside a folded-dead arm never lands — it must
        // not widen the flat env either (`x = 1; if false; x = "s"; end; x.w`
        // fires `for 1` on the oracle, not a widened decline). A `rescue`
        // arm's writes keep widening: they are real writes whose reach the
        // position rule in `DeadPositions::write_reaches` decides per-use.
        rebinds.retain(|(s, _)| !dead.covers(*s));
        mutations.retain(|(s, ..)| !dead.covers(*s));
        closure_muts.retain(|(_, w, _)| !dead.covers(*w));
        let indexed = collect_indexed_flow(ast);
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
        let mut boundaries = Vec::new();
        for stmt in body {
            if record {
                boundaries.push((stmt, env.clone()));
            }
            self.bind_check_statement(ast, stmt, &mut env, &rebinds, &mutations, &indexed, &dead, interner);
        }
        CheckFlow {
            env,
            boundaries,
            rebinds,
            mutations,
            closure_mutations: closure_muts,
            indexed,
            dead,
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
        let base: Cow<'f, TypeEnv> = 'env: {
            let Some(&(stmt, ref pre)) = flow.boundaries.iter().find(|(id, _)| {
                let s = ast.get(*id).span();
                s.0 <= site.0 && site.1 <= s.1
            }) else {
                break 'env Cow::Borrowed(&flow.env);
            };
            let stmt_span = ast.get(stmt).span();
            // rigor-rs#368 — a site inside a `rescue` clause also needs the
            // replay even when no write postdates its statement: the clause's
            // own bindings (`rescue => e`) reach only through it.
            let later = flow.rebinds.iter().any(|(w, _)| w.0 >= stmt_span.0)
                || flow.mutations.iter().any(|(w, _, _, _)| w.0 >= stmt_span.0)
                || flow
                    .closure_mutations
                    .iter()
                    .any(|(_, w, _)| w.0 >= stmt_span.0)
                || flow.dead.in_rescue(site);
            if !later {
                break 'env Cow::Borrowed(&flow.env);
            }
            let mut env = pre.clone();
            self.entry_descend(ast, stmt, site, true, &mut env, flow, interner);
            Cow::Owned(env)
        };
        // rigor-rs#368 — a site inside a folded-dead `if`/`unless` arm reads
        // the arm's ENTRY scope: the reference never evaluates the arm, and
        // `propagate` fills its nodes with the predicate's edge scope, whose
        // subject locals narrowed to `Bot` on the dead edge (`q = nil; if q;
        // q.w; end` is silent on the oracle). This also catches a dead-arm
        // site the boundary descent cannot reach (buried under a `Loop` /
        // `Case` — `apply_subtree_effects` is flat there).
        if let Some(subjects) = flow.dead.subjects_at(site) {
            let mut env = base.into_owned();
            for (name, op) in subjects {
                let cur = env
                    .get(name)
                    .copied()
                    .unwrap_or_else(|| interner.untyped());
                let narrowed = self.dead_subject_type(ast, cur, op, interner);
                env.insert(name.clone(), narrowed);
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
                self.apply_subtree_effects(ast, predicate, site, uncond, env, flow, interner);
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
                        self.entry_descend(ast, s, site, false, env, flow, interner);
                        return;
                    }
                    if sspan.1 <= site.0 {
                        self.apply_subtree_effects(ast, s, site, false, env, flow, interner);
                    }
                }
            }
            // A `begin`'s main body runs in order unconditionally; a rescue
            // clause, the else arm and `ensure` are conditional or later, so
            // a site inside one takes every contained effect conservatively
            // — the flat env's own decline.
            Node::BeginRescue {
                main_body, clauses, ..
            } => {
                let (main_body, clauses) = (main_body.clone(), clauses.clone());
                if main_body
                    .iter()
                    .any(|&s| within_span(site, ast.get(s).span()))
                {
                    self.entry_children(ast, id, site, uncond, env, flow, interner);
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
                        // `rescue => e` binds the exception inside the arm —
                        // `bind_rescue_reference` on the oracle
                        // (`statement_evaluator.rb:5100`).
                        if let Some(name) = c.bound_name.clone() {
                            let ty = self.rescue_reference_type(ast, c, interner);
                            env.insert(name, ty);
                        }
                        for &pre_s in c.body.iter().take_while(|&x| *x != s) {
                            self.bind_check_statement(
                                ast, pre_s, env, &flow.rebinds, &flow.mutations, &flow.indexed, &flow.dead, interner,
                            );
                        }
                        self.entry_descend(ast, s, site, false, env, flow, interner);
                        return;
                    }
                }
                self.apply_subtree_effects(ast, id, site, false, env, flow, interner);
            }
            // Conditional container: nothing inside orders against the site
            // (recovery carrier, loop, case/when), so apply every contained
            // effect conservatively — byte-identical to the flat env.
            Node::Statements { kind, .. } if !matches!(kind, StatementsKind::Sequence) => {
                self.apply_subtree_effects(ast, id, site, false, env, flow, interner);
            }
            Node::Loop { .. } | Node::Case { .. } | Node::When { .. } => {
                self.apply_subtree_effects(ast, id, site, false, env, flow, interner);
            }
            // A closure body or class/module body captures the whole env —
            // today's `ScopedEnv::at` answer, kept verbatim. A lambda body
            // is also a deferred scope with its OWN evaluated attribute
            // writes: replay them positionally inside it (rigor-rs#366).
            Node::Lambda { .. } => {
                *env = flow.env.clone();
                self.closure_descend(ast, id, site, env, flow, id);
            }
            Node::Definition { .. } | Node::ClassDef { .. } | Node::ModuleDef { .. } => {
                *env = flow.env.clone();
            }
            _ => self.entry_children(ast, id, site, uncond, env, flow, interner),
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
                Node::ConstantRead { name, .. } => named(name, interner),
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
        env: &mut TypeEnv,
        flow: &CheckFlow,
        interner: &mut Interner,
    ) {
        for (child, edge) in self.flow_children(ast.get(id)) {
            let cspan = ast.get(child).span();
            if cspan.0 <= site.0 && site.0 < cspan.1 {
                if edge == FlowEdge::Barrier {
                    *env = flow.env.clone();
                    // A literal block body's scope is the captured env —
                    // then the body's own evaluated attribute writes drop
                    // their indexed narrowings positionally inside it
                    // (rigor-rs#366). A `def`/`class`/`module` body is a
                    // fresh local scope the flat env already stands for.
                    if matches!(ast.get(id), Node::Call { .. }) {
                        self.closure_descend(ast, id, site, env, flow, id);
                    }
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
                    site,
                    uncond && edge == FlowEdge::Uncond,
                    env,
                    flow,
                    interner,
                );
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
        for (wspan, name) in &flow.rebinds {
            if wspan.0 >= span.0
                && wspan.1 <= span.1
                && flow.dead.write_reaches(*wspan, site)
            {
                let u = interner.untyped();
                env.insert(name.clone(), u);
                drop_indexed_narrowings(env, name);
            }
        }
        for (wspan, name, method, drop_key) in &flow.mutations {
            if wspan.0 >= span.0
                && wspan.1 <= span.1
                && flow.dead.write_reaches(*wspan, site)
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
            {
                self.apply_slot_mutation(ast, id, m, env, interner);
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
        let body_owning = matches!(
            node,
            Node::Call {
                block_span: Some(_),
                ..
            } | Node::Lambda { .. }
        );
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
                        self.apply_closure_mutations(ast, sib, env, flow, sib_owner);
                    }
                }
                if edge == FlowEdge::Barrier {
                    // A nested literal block/lambda: its own writes belong
                    // to ITS body — `id` becomes the owner. Any other
                    // barrier (a `def`/`class`/`module` body) is a fresh
                    // local scope the replay does not enter.
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
    /// drop is always the receiver's whole record set.
    fn apply_closure_mutations(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        env: &mut TypeEnv,
        flow: &CheckFlow,
        owner: NodeId,
    ) {
        let span = ast.get(id).span();
        for (o, wspan, name) in &flow.closure_mutations {
            if *o == owner && wspan.0 >= span.0 && wspan.1 <= span.1 {
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
        self.drop_indexed_mutation(name, method, drop_key, env);
        let evaluated = uncond
            && (self.path_unconditional(ast, root, wspan)
                || (indexed.operand_spans.contains(&wspan)
                    && self.path_operand_evaluated(ast, root, wspan)));
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
        if MUTATOR_METHODS.contains(&method) {
            if let Some(&pre) = env.get(name) {
                if evaluated {
                    if let Some(widened) = self.widen_mutated_binding(pre, method, interner) {
                        env.insert(name.to_string(), widened);
                    }
                } else {
                    env.insert(name.to_string(), interner.untyped());
                }
            }
        }
        if let Some((key, ty)) = stored {
            env.insert(key, ty);
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
        root: NodeId,
        m: &crate::flow_writes::SlotMutation,
        env: &mut TypeEnv,
        interner: &mut Interner,
    ) {
        let lookup = indexed_narrowing_key(&m.name, &m.key);
        let Some(&recorded) = env.get(&lookup) else {
            return;
        };
        // Element mutators live in ordinary expression positions (argument
        // lists, splats), so the lenient operand path — not the strict
        // carrier mint — decides whether the call ran.
        if !self.path_operand_evaluated(ast, root, m.span) {
            env.remove(&lookup);
            return;
        }
        let widened = self
            .widen_mutated_binding(recorded, &m.method, interner)
            .or_else(|| self.string_slot_floor(recorded, &m.method, interner));
        match widened {
            Some(ty) => {
                env.insert(lookup, ty);
            }
            None => {
                env.remove(&lookup);
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
    #[allow(clippy::too_many_arguments)]
    fn bind_check_statement(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        env: &mut TypeEnv,
        rebinds: &[(rigor_parse::Span, String)],
        mutations: &[(rigor_parse::Span, String, String, Option<ShapeKey>)],
        indexed: &IndexedFlow,
        dead: &crate::dead::DeadPositions,
        interner: &mut Interner,
    ) {
        match ast.get(id) {
            Node::LocalVariableWrite { value, .. } | Node::MultiWrite { value, .. } => {
                let vspan = ast.get(*value).span();
                widen_flow_writes(rebinds, vspan, env, interner);
                self.drop_indexed_rebinds(rebinds, vspan, env);
                self.widen_mutated_locals(ast, mutations, indexed, *value, vspan, env, interner);
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
                        self.widen_mutated_locals(ast, mutations, indexed, id, tspan, env, interner);
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
                    self.bind_check_statement(ast, s, env, rebinds, mutations, indexed, dead, interner);
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
                        let live_rebinds: Vec<_> = rebinds
                            .iter()
                            .filter(|(w, _)| dead.write_reaches(*w, (span.1, span.1)))
                            .cloned()
                            .collect();
                        let live_mutations: Vec<_> = mutations
                            .iter()
                            .filter(|(w, ..)| dead.write_reaches(*w, (span.1, span.1)))
                            .cloned()
                            .collect();
                        widen_flow_writes(&live_rebinds, span, env, interner);
                        self.drop_indexed_rebinds(&live_rebinds, span, env);
                        self.widen_mutated_locals(
                            ast, &live_mutations, indexed, id, span, env, interner,
                        );
                        for s in live {
                            self.bind_statement(ast, s, env, interner);
                        }
                    }
                    None => {
                        widen_flow_writes(rebinds, span, env, interner);
                        self.drop_indexed_rebinds(rebinds, span, env);
                        self.widen_mutated_locals(ast, mutations, indexed, id, span, env, interner);
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
                let live_rebinds: Vec<_> = rebinds
                    .iter()
                    .filter(|(w, _)| dead.write_reaches(*w, (span.1, span.1)))
                    .cloned()
                    .collect();
                let live_mutations: Vec<_> = mutations
                    .iter()
                    .filter(|(w, ..)| dead.write_reaches(*w, (span.1, span.1)))
                    .cloned()
                    .collect();
                widen_flow_writes(&live_rebinds, span, env, interner);
                self.drop_indexed_rebinds(&live_rebinds, span, env);
                self.widen_mutated_locals(ast, &live_mutations, indexed, id, span, env, interner);
            }
            other => {
                let span = other.span();
                widen_flow_writes(rebinds, span, env, interner);
                self.drop_indexed_rebinds(rebinds, span, env);
                self.widen_mutated_locals(ast, mutations, indexed, id, span, env, interner);
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
    fn widen_mutated_locals(
        &self,
        ast: &LoweredAst,
        mutations: &[(rigor_parse::Span, String, String, Option<ShapeKey>)],
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
            if span.0 <= m.span.0 && m.span.1 <= span.1 {
                self.apply_slot_mutation(ast, root, m, env, interner);
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
        indexed
            .slot_writes
            .iter()
            .filter(|w| w.span.0 >= span.0 && w.span.1 <= span.1)
            .filter(|w| self.path_operand_evaluated(ast, root, w.span))
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
        self.drop_indexed_rebinds(rebinds, span, env);
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
                    let (_, name, method, drop_key) = &mutations[i];
                    self.drop_indexed_mutation(name, method, drop_key.as_ref(), env);
                }
                Ev::Insert(i) => {
                    env.insert(stored[i].1.clone(), stored[i].2);
                }
                Ev::SlotMut(i) => {
                    self.apply_slot_mutation(ast, root, &indexed.slot_mutations[i], env, interner);
                }
            }
        }
    }

    /// `Scope#type_of=`'s rebind invalidation: a local rebound anywhere in
    /// `span` drops every indexed narrowing rooted at it.
    fn drop_indexed_rebinds(
        &self,
        rebinds: &[(rigor_parse::Span, String)],
        span: rigor_parse::Span,
        env: &mut TypeEnv,
    ) {
        for (wspan, name) in rebinds {
            if wspan.0 >= span.0 && wspan.1 <= span.1 {
                drop_indexed_narrowings(env, name);
            }
        }
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
                env.remove(&indexed_narrowing_key(name, key));
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
        let indexed = collect_indexed_flow(ast);
        // rigor-rs#368 — a write/mutation inside a folded-dead arm never
        // lands; it must not widen this env either.
        let dead = self.dead_positions(ast, &mut Vec::new());
        writes.retain(|(s, _)| !dead.covers(*s));
        rebinds.retain(|(s, _)| !dead.covers(*s));
        mutations.retain(|(s, ..)| !dead.covers(*s));
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
                widen_flow_writes(writes, vspan, env, interner);
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
                let stored =
                    self.stored_slot_writes(ast, indexed, id, vspan, env, interner);
                widen_flow_writes(writes, vspan, env, interner);
                self.land_indexed_stored(
                    ast, indexed, id, vspan, rebinds, mutations, stored, env, interner,
                );
                let rhs = self.type_of(ast, value, env, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    drop_indexed_narrowings(env, &name);
                    env.insert(name, ty);
                }
                // An `h[k]` index target stores through `[]=` on the
                // POST-binding scope (`h, h[:a] = h, 1` stores into the rebound
                // `h`), so each receiver's locals widen after the bindings —
                // exactly as `h[k] = v` widens them (`eval_multi_write` →
                // `IndexWriteWidening.widen`, rigor-rs#134).
                for (_, tspan, _) in targets.index_writes() {
                    widen_flow_writes(writes, tspan, env, interner);
                }
            }
            Node::LocalVariableOpWrite { name, .. } => {
                // `x += 1` / `x ||= 5` reads-then-writes; the result is not a
                // tracked constant in this slice — widen.
                let name = name.clone();
                let u = interner.untyped();
                drop_indexed_narrowings(env, &name);
                env.insert(name, u);
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
                                .get(&name)
                                .copied()
                                .unwrap_or_else(|| interner.untyped());
                            let narrowed = self.dead_subject_type(ast, cur, &op, interner);
                            dead_env.insert(name, narrowed);
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
                widen_flow_writes(writes, pspan, env, interner);
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
                    let mut arm_envs = Vec::with_capacity(clauses.len());
                    for c in &clauses {
                        // The arm's entry scope is the `begin`'s ENTRY scope —
                        // the exception may fire before ANY try statement runs,
                        // so try-body writes never flow in (probed: `timed =
                        // nil; begin; timed = true; rescue; if timed` warns
                        // FALSEY on the oracle, not truthy). A `retry` inside
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
                            widen_flow_writes(rebinds, begin_span, &mut cenv, interner);
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
                    self.flow_eval_scope(
                        ast, &ensure_body, &mut eenv, in_loop_or_block, self_qual, self_kind, writes, rebinds, mutations, indexed, interner, out,
                    );
                }
                let span = ast.get(id).span();
                let stored =
                    self.stored_slot_writes(ast, indexed, id, span, env, interner);
                widen_flow_writes(writes, span, env, interner);
                self.land_indexed_stored(
                    ast, indexed, id, span, rebinds, mutations, stored, env, interner,
                );
            }
            // Loop / case / logical / call(+block) / any other node:
            // widen every local written in the span, do not descend for snapshots.
            other => {
                let span = other.span();
                let stored =
                    self.stored_slot_writes(ast, indexed, id, span, env, interner);
                widen_flow_writes(writes, span, env, interner);
                self.land_indexed_stored(
                    ast, indexed, id, span, rebinds, mutations, stored, env, interner,
                );
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
                    if let Some(scalar) = self.source.implicit_self_literal(q, self_kind, &method) {
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
                    drop_indexed_narrowings(env, &name);
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
            // `recv.attr op= v` — a compound ATTRIBUTE write binds no local
            // itself; nested writes in its operands (`h.default ||= (x = 1)`)
            // bind as they did under the recovered carrier (rigor-rs#343).
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
    /// `(call span, name, method, drop_key)`.
    mutations: Vec<(rigor_parse::Span, String, String, Option<ShapeKey>)>,
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
}
