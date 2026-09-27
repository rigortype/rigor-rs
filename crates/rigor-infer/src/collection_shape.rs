//! Collection-shape receiver survival: the `coll_flow_*` walker behind
//! [`Typer::collection_shape_snapshots`], which threads a per-branch-joined
//! `TypeEnv` of `Tuple` / `HashShape` / `Nominal[Array|Hash]` carriers through
//! in-place mutators so the rules layer can witness a call on a bare-local
//! Array or Hash receiver.

use std::collections::HashMap;

use rigor_parse::{LoweredAst, Node, NodeId, StatementsKind};
use rigor_types::{Interner, Type, TypeId};

use crate::{
    collect_flow_writes, collect_rebind_writes, indexed_flow_writes, multi_target_binder,
    rebound_within, span_hull, widen_flow_writes, TypeEnv, Typer, ARRAY_MUTATORS, HASH_MUTATORS,
};

impl<'i> Typer<'i> {
    // -----------------------------------------------------------------------
    // Collection-shape receiver survival (spec
    // docs/notes/20260807-collection-shape-slice-spec.md, STAGE 1).
    //
    // Self-contained region: a parallel walker (`coll_flow_*`) rather than an
    // extension of `class_flow_*`, so this slice's env discipline (a threaded
    // `TypeEnv` joined per branch) stays independent of the narrowing pass's
    // fact env, and neither can regress the other.
    // -----------------------------------------------------------------------

    /// Compute the per-call-node collection-shape snapshot map: `call node id ->
    /// "Array" | "Hash"` for every call whose receiver is a bare local whose
    /// threaded binding is a collection carrier — a literal `Tuple`/`HashShape`
    /// seed, a `Nominal[Array|Hash]` minted by an in-place mutator that KEPT the
    /// nominal (`MutationWidening::widen_tuple`/`widen_hash_shape`,
    /// `reference/rigor/lib/rigor/inference/mutation_widening.rb:251,265`), or a
    /// `Nominal[Array|Hash]` an already-FP-gated tier fold produced (`missing =
    /// KEYS.filter { … }`). The rules layer's `check_collection_call` fires
    /// `call.undefined-method` from this map — and ONLY that rule (the class
    /// narrowing slice's pitfall 7: no wrong-arity / ATM wiring).
    ///
    /// ## FP-safety envelope (every decline load-bearing; §6 of the spec)
    ///
    /// - **Seeds only from literals or existing folds.** A mutator on a Dynamic
    ///   carrier never MINTS a binding (`widen_for_mutator` returns nil for a
    ///   non-shape carrier — probes m08/m13), which is also what keeps us out of
    ///   the reference's runtime-wrong `[]=`-on-a-String rows (bucket E, probe
    ///   c12).
    /// - **Per-shape mutator tables**, not their union: a Hash-only mutator on a
    ///   `Tuple` is a no-op, exactly as the reference's `case type` dispatch
    ///   (`mutation_widening.rb:209`).
    /// - **Branch join is identical-`TypeId` only** ([`join_flow_envs`]). This is
    ///   the load-bearing decline: a branch-contained mutation on a not-yet-
    ///   widened seed leaves `Tuple[] | Array[…]` after the reference's
    ///   `Scope#join` (`scope.rb:680`) and `receiver_descriptor` has NO
    ///   `Type::Union` arm (`rbs_dispatch.rb:200`), so the reference is SILENT
    ///   (probes m18/m20) — we widen to untyped and never model the union.
    ///   Straight-line widening BEFORE the construct makes both edges agree and
    ///   the site fires (m01/m19).
    /// - **Block bodies** mirror `widen_after_block` (`:144`): the block's
    ///   binding REPLACES the outer one, but only when it is a kept nominal —
    ///   any other outcome (a rebind, m15; a block-internal branch join) widens
    ///   to untyped. Descent only from statement position (the block-narrowing
    ///   position rule, docs/notes/20260807-block-narrowing-position-rule.md);
    ///   an expression-position block widens its contained writes instead.
    /// - **Unmodeled constructs decline**: `while`/`until` (the reference DOES
    ///   fire, probe m10 — deliberate coverage loss, its `break`/`next` join
    ///   edges are unprobed), `begin`/`rescue`, op-writes (m16), logicals,
    ///   safe-nav receivers, and every expression kind without an arm below
    ///   widen every write their span contains and bind nothing.
    /// - **Ivar carriers are never typed** (probe m09 fires in the reference —
    ///   bucket B's own future slice).
    ///
    /// [`join_flow_envs`]: crate::join_flow_envs
    pub fn collection_shape_snapshots(
        &self,
        ast: &LoweredAst,
        interner: &mut Interner,
    ) -> HashMap<NodeId, &'static str> {
        let mut out = HashMap::new();
        let body = match ast.get(ast.root()) {
            Node::Program { body, .. } => body.clone(),
            _ => return out,
        };
        let mut writes = collect_flow_writes(ast);
        writes.extend(indexed_flow_writes(ast, self.source));
        let rebinds = collect_rebind_writes(ast);
        let ctx = CollCtx { writes: &writes, rebinds: &rebinds };
        let mut tenv = TypeEnv::new();
        self.coll_flow_scope(ast, &body, &mut tenv, &ctx, interner, &mut out, true);
        out
    }

    /// Thread `tenv` through a scope's statements in source order.
    #[allow(clippy::too_many_arguments)]
    fn coll_flow_scope(
        &self,
        ast: &LoweredAst,
        stmts: &[NodeId],
        tenv: &mut TypeEnv,
        ctx: &CollCtx<'_>,
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
        stmt_position: bool,
    ) {
        for &s in stmts {
            self.coll_flow_stmt(ast, s, tenv, ctx, interner, out, stmt_position);
        }
    }

    /// Apply one statement's effect on `tenv` and record collection-typed uses.
    #[allow(clippy::too_many_arguments)]
    fn coll_flow_stmt(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        ctx: &CollCtx<'_>,
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
        stmt_position: bool,
    ) {
        match ast.get(id) {
            // An inert carrier leaves the reference's scope unchanged
            // (rigor-rs#153): a write in it does not bind, but the uses in it
            // are still recorded, as before.
            Node::Statements { body, kind: StatementsKind::Inert, .. } => {
                for s in body.clone() {
                    match ast.get(s) {
                        Node::LocalVariableWrite { value, .. }
                        | Node::LocalVariableOpWrite { value, .. }
                        | Node::MultiWrite { value, .. } => {
                            let value = *value;
                            self.coll_flow_expr(ast, value, tenv, ctx, interner, out, stmt_position);
                        }
                        _ => self.coll_flow_stmt(ast, s, tenv, ctx, interner, out, stmt_position),
                    }
                }
            }
            Node::Statements { body, kind, span } => {
                let (body, kind, span) = (body.clone(), *kind, *span);
                self.coll_flow_scope(ast, &body, tenv, ctx, interner, out, stmt_position);
                // A recovery carrier's writes may not run, or not in this order:
                // the uses in it are recorded as before, then its writes widen.
                if kind == StatementsKind::Recovered {
                    widen_flow_writes(ctx.writes, span, tenv, interner);
                }
            }
            Node::LocalVariableWrite { .. }
            | Node::MultiWrite { .. }
            | Node::LocalVariableOpWrite { .. }
            | Node::Call { .. } => {
                self.coll_flow_expr(ast, id, tenv, ctx, interner, out, stmt_position);
            }
            // `return E` evaluates its operands in the current bindings; the
            // operand is EXPRESSION position (no block/`case` effect leaks out).
            Node::Return { values, .. } => {
                let values = values.clone();
                for v in values {
                    self.coll_flow_expr(ast, v, tenv, ctx, interner, out, false);
                }
            }
            Node::If { .. } => {
                self.coll_flow_if(ast, id, tenv, ctx, interner, out, stmt_position);
            }
            Node::Case { .. } => {
                self.coll_flow_case(ast, id, tenv, ctx, interner, out, stmt_position);
            }
            Node::Definition { body, .. }
            | Node::ClassDef { body, .. }
            | Node::ModuleDef { body, .. } => {
                // Independent local scope: fresh env, no effect on the enclosing one.
                let body = body.clone();
                let mut t = TypeEnv::new();
                self.coll_flow_scope(ast, &body, &mut t, ctx, interner, out, true);
            }
            // Unmodeled statement (`while`/`until`, `begin`/`rescue`, ivar
            // writes, …): widen every local it writes and do NOT descend.
            other => {
                let span = other.span();
                widen_flow_writes(ctx.writes, span, tenv, interner);
            }
        }
    }

    /// Evaluate an expression: record collection-typed receiver uses, thread
    /// rebinds, apply the keep-nominal mutator widening, and widen conservatively
    /// for everything unmodeled.
    #[allow(clippy::too_many_arguments)]
    fn coll_flow_expr(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        ctx: &CollCtx<'_>,
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
        stmt_position: bool,
    ) {
        match ast.get(id) {
            Node::Call { receiver, method, args, block_body, safe_nav, span, .. } => {
                let receiver = *receiver;
                let method = method.clone();
                let args = args.clone();
                let block_body = block_body.clone();
                let safe_nav = *safe_nav;
                let call_span = *span;
                // The receiver evaluates first (a nested `a.b` in `a.b.c`) —
                // EXPRESSION position, so no block/`case` under it establishes
                // anything.
                if let Some(r) = receiver {
                    self.coll_flow_expr(ast, r, tenv, ctx, interner, out, false);
                }
                // The bare-local receiver name, if any. Safe-nav dispatch is out
                // of the envelope on both the recording and the widening side.
                let local = match (receiver, safe_nav) {
                    (Some(r), false) => match ast.get(r) {
                        Node::LocalVariableRead { name, .. } => Some(name.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                // Record the use BEFORE any of the call's own effects — the
                // receiver read happens first (`output << 'a'` reads the seed).
                if let Some(name) = &local {
                    if let Some(cls) = tenv.get(name).and_then(|&ty| self.coll_carrier(interner, ty))
                    {
                        out.insert(id, cls);
                    }
                }
                // The mutator's effect is decided by the PRE-call carrier
                // (`widen_for_mutator`, `mutation_widening.rb:209`).
                let widened = match local.as_ref().and_then(|name| {
                    let ty = *tenv.get(name)?;
                    self.coll_widen_for_mutator(interner, ty, &method)
                        .map(|c| (name.clone(), c, ty))
                }) {
                    None => None,
                    // `Inference::MutationRejoin` (`1ad7351e`, #580, `v0.3.9`):
                    // the later store grows the carrier's value side instead of
                    // being swallowed by the already-widened nominal. Read off
                    // the PRE-call carrier, like the widening itself.
                    Some((name, cls, pre_ty)) => {
                        let mut added: Vec<TypeId> = Vec::new();
                        for &a in Typer::coll_store_value_args(&method, &args) {
                            for m in self.coll_store_value_classes(ast, a, tenv, interner) {
                                if !added.contains(&m) {
                                    added.push(m);
                                }
                            }
                        }
                        // Canonical order: the member set is compared only by the
                        // interned `TypeId` two branch edges end up with, so the
                        // order stores happened in must not separate them.
                        let grown = |base: &[TypeId]| {
                            let mut members = base.to_vec();
                            for &m in &added {
                                if !members.contains(&m) {
                                    members.push(m);
                                }
                            }
                            members.sort_unstable();
                            members
                        };
                        match interner.get(pre_ty) {
                            // `widen_union` (mutation_widening.rb:316) widens
                            // EACH arm and `Combinator.union` re-joins, deduping
                            // only structurally identical arms — the edges
                            // converge iff every arm grows to the same set.
                            // Collapsing to one merged carrier fired where the
                            // oracle kept the union (probe r2).
                            Type::Union(arms) => {
                                let arms = arms.clone();
                                let mut minted = Vec::with_capacity(arms.len());
                                for arm in arms {
                                    let members =
                                        grown(&Typer::coll_value_members(interner, arm));
                                    if let Some(t) =
                                        self.coll_nominal_with(interner, cls, &members)
                                    {
                                        minted.push(t);
                                    }
                                }
                                minted.sort_unstable();
                                minted.dedup();
                                match minted.as_slice() {
                                    [] => None,
                                    [only] => Some((name, *only)),
                                    _ => Some((name, interner.intern(Type::Union(minted)))),
                                }
                            }
                            _ => self
                                .coll_nominal_with(
                                    interner,
                                    cls,
                                    &grown(&Typer::coll_value_members(interner, pre_ty)),
                                )
                                .map(|ty| (name, ty)),
                        }
                    }
                };
                // Arguments are EXPRESSION position.
                for a in &args {
                    self.coll_flow_expr(ast, *a, tenv, ctx, interner, out, false);
                }
                if block_body.is_empty() {
                    // Widen every write the call span contains (the receiver-side
                    // mutator entry itself, and argument-position mutations via
                    // `indexed_flow_writes`); the modeled mutator effect is
                    // re-applied below.
                    widen_flow_writes(ctx.writes, call_span, tenv, interner);
                } else if stmt_position {
                    // The block body evaluates in a CHILD env seeded from the
                    // outer one (uses inside the block are recorded there) …
                    let pre = tenv.clone();
                    let mut btenv = tenv.clone();
                    self.coll_flow_scope(ast, &block_body, &mut btenv, ctx, interner, out, true);
                    // … then every write the call span contains widens (a block
                    // REBIND of a captured local is visible outside and kills the
                    // carrier — probe m15) …
                    widen_flow_writes(ctx.writes, call_span, tenv, interner);
                    // … and finally `widen_after_block` (`mutation_widening.rb:144`)
                    // re-applies the mutations. That routine is a SYNTACTIC walk
                    // of the block body against the OUTER scope, NOT a join of the
                    // block's evaluated scope: its own doc names `arr.push(x) if
                    // cond` as a case it catches, so a branch-contained mutation
                    // inside a block still widens the outer binding (which is why
                    // the gitlab jira-tracker / ddl-lock rows fire in the
                    // reference). We mirror it exactly, off the PRE-call carriers.
                    for (name, cls) in self.coll_block_mutations(ast, &block_body, &pre, interner) {
                        if !rebound_within(ctx.rebinds, call_span, &name) {
                            if let Some(ty) = self.coll_nominal(interner, cls) {
                                tenv.insert(name, ty);
                            }
                        }
                    }
                } else {
                    // Expression-position block: no descent (the position rule),
                    // and every contained write widens.
                    widen_flow_writes(ctx.writes, call_span, tenv, interner);
                }
                // Keep-nominal widening — unless something inside the call
                // REBOUND the same local (`output << (output = x)`).
                if let Some((name, ty)) = widened {
                    if !rebound_within(ctx.rebinds, call_span, &name) {
                        tenv.insert(name, ty);
                    }
                }
            }
            // `&&`/`||` — the RHS may not execute, so its effects are unmodeled:
            // evaluate both sides on a THROWAWAY env (uses are still recorded
            // against the pre-logical bindings) and widen every contained write.
            Node::Logical { left, right, span, .. } => {
                let (left, right, lspan) = (*left, *right, *span);
                let mut scratch = tenv.clone();
                self.coll_flow_expr(ast, left, &mut scratch, ctx, interner, out, false);
                self.coll_flow_expr(ast, right, &mut scratch, ctx, interner, out, false);
                widen_flow_writes(ctx.writes, lspan, tenv, interner);
            }
            Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                // The RHS reads the PRE-write binding; an assignment RHS keeps
                // the statement's own position (the narrowing pass's probe p1).
                self.coll_flow_expr(ast, value, tenv, ctx, interner, out, stmt_position);
                let vty = self.type_of(ast, value, tenv, interner);
                tenv.insert(name, vty);
            }
            Node::MultiWrite { targets, value, .. } => {
                let (targets, value) = (targets.clone(), *value);
                self.coll_flow_expr(ast, value, tenv, ctx, interner, out, stmt_position);
                let rhs = self.type_of(ast, value, tenv, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    tenv.insert(name, ty);
                }
            }
            // Op-writes (`output += [1]`) are unmodeled — the reference folds
            // `Tuple + Tuple` and keeps the literal shape (probe m16); mirroring
            // that fold is coverage, not FP-safety, so the target widens.
            Node::LocalVariableOpWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                self.coll_flow_expr(ast, value, tenv, ctx, interner, out, false);
                let u = interner.untyped();
                tenv.insert(name, u);
            }
            Node::If { .. } => {
                self.coll_flow_if(ast, id, tenv, ctx, interner, out, false);
            }
            Node::Case { .. } => {
                self.coll_flow_case(ast, id, tenv, ctx, interner, out, stmt_position);
            }
            // Every other expression kind is unmodeled: bind nothing, and widen
            // every write its span contains (a mutation buried in an array
            // literal, an interpolation, a pattern, …).
            other => {
                let span = other.span();
                widen_flow_writes(ctx.writes, span, tenv, interner);
            }
        }
    }

    /// Thread one `if`/`unless`/ternary. The two edges run on clones of the
    /// pre-conditional env and are joined by IDENTICAL `TypeId` only
    /// ([`join_flow_envs`]) — the mirror of the reference's `Scope#join` union
    /// plus `receiver_descriptor`'s missing `Type::Union` arm (probes m18/m20
    /// silent, m01/m19 fire).
    ///
    /// [`join_flow_envs`]: crate::join_flow_envs
    #[allow(clippy::too_many_arguments)]
    fn coll_flow_if(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        ctx: &CollCtx<'_>,
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
        stmt_position: bool,
    ) {
        let Node::If { predicate, then_body, else_body, is_unless, .. } = ast.get(id) else {
            return;
        };
        let (predicate, is_unless) = (*predicate, *is_unless);
        let (then_body, else_body) = (then_body.clone(), else_body.clone());
        // The predicate evaluates first, in the pre-conditional bindings.
        self.coll_flow_expr(ast, predicate, tenv, ctx, interner, out, false);
        let (truthy, falsey) =
            if is_unless { (&else_body, &then_body) } else { (&then_body, &else_body) };
        let mut t = tenv.clone();
        self.coll_flow_scope(ast, truthy, &mut t, ctx, interner, out, stmt_position);
        let mut f = tenv.clone();
        self.coll_flow_scope(ast, falsey, &mut f, ctx, interner, out, stmt_position);
        *tenv = self.coll_join_envs(&t, &f, interner);
    }

    /// Thread one `case`/`when`. Every clause body runs on a clone of the
    /// pre-`case` env and all of them are joined together WITH the pre-`case` env
    /// (the implicit no-match path) — so a clause-contained mutation on a
    /// not-yet-widened seed widens to untyped (m18), while a pre-widened nominal
    /// survives every arm (m19). A `case`/`in` pattern carrier is unmodeled: the
    /// whole construct's writes widen and nothing is threaded.
    #[allow(clippy::too_many_arguments)]
    fn coll_flow_case(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        ctx: &CollCtx<'_>,
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
        stmt_position: bool,
    ) {
        let Node::Case { predicate, branches, else_body, span } = ast.get(id) else {
            return;
        };
        let (predicate, case_span) = (*predicate, *span);
        let (branches, else_body) = (branches.clone(), else_body.clone());
        if let Some(p) = predicate {
            self.coll_flow_expr(ast, p, tenv, ctx, interner, out, false);
        }
        // The no-match path: the pre-`case` bindings, unchanged.
        let mut acc = tenv.clone();
        for br in branches {
            let Node::When { conditions, body, .. } = ast.get(br) else {
                // `case`/`in` pattern carrier — unmodeled, no descent.
                widen_flow_writes(ctx.writes, case_span, tenv, interner);
                return;
            };
            let (conditions, body) = (conditions.clone(), body.clone());
            let mut t = tenv.clone();
            for &cond in &conditions {
                self.coll_flow_expr(ast, cond, &mut t, ctx, interner, out, false);
            }
            self.coll_flow_scope(ast, &body, &mut t, ctx, interner, out, stmt_position);
            acc = self.coll_join_envs(&acc, &t, interner);
        }
        let mut e = tenv.clone();
        self.coll_flow_scope(ast, &else_body, &mut e, ctx, interner, out, stmt_position);
        *tenv = self.coll_join_envs(&acc, &e, interner);
    }

    /// The collection class a receiver carrier projects to, or `None`. Mirrors
    /// the reference's `receiver_descriptor` (`rbs_dispatch.rb:209-212`): a
    /// `Tuple` dispatches as `Array` and a `HashShape` as `Hash`, and a kept
    /// `Nominal[Array|Hash]` (mutator-widened or tier-folded) dispatches as
    /// itself. Every other carrier — including a `Union` — declines.
    fn coll_carrier(&self, interner: &Interner, ty: TypeId) -> Option<&'static str> {
        match interner.get(ty) {
            Type::Tuple(_) => Some("Array"),
            Type::HashShape(_) => Some("Hash"),
            Type::Nominal { .. } => self.coll_nominal_carrier(interner, ty),
            _ => None,
        }
    }

    /// As [`Typer::coll_carrier`], restricted to a genuine `Nominal[Array|Hash]`
    /// (what a block body may propagate outwards).
    fn coll_nominal_carrier(&self, interner: &Interner, ty: TypeId) -> Option<&'static str> {
        let Type::Nominal { class, .. } = interner.get(ty) else {
            return None;
        };
        ["Array", "Hash"].into_iter().find(|name| self.index.class_id(name) == Some(*class))
    }

    /// The reference's `MutationWidening::widen_for_mutator`
    /// (`reference/rigor/lib/rigor/inference/mutation_widening.rb:209`) as this
    /// pass needs it: the collection class the receiver local carries AFTER the
    /// mutation, or `None` when nothing survives.
    ///
    /// - `Tuple` + an ARRAY mutator ⇒ `"Array"` (`widen_tuple:251` — the nominal
    ///   is KEPT, which is the whole slice); `HashShape` + a HASH mutator ⇒
    ///   `"Hash"` (`widen_hash_shape:265`).
    /// - An already-widened `Nominal[Array|Hash]` under a mutator of ITS OWN
    ///   table has "no precision to lose" — the reference leaves the scope
    ///   untouched, so we re-assert the same nominal (the caller widens the whole
    ///   call span first, and this is what survives it: probes m01/m03/m17, where
    ///   the SECOND and later mutations run on an already-nominal carrier).
    /// - Everything else declines: the tables are per-shape and NOT their union
    ///   (a Hash-only mutator on a Tuple is a no-op), and a non-shape carrier
    ///   never MINTS a binding (probes m08/m13, and the bucket-E `[]=`-on-a-
    ///   String rows we must not mirror).
    fn coll_widen_for_mutator(
        &self,
        interner: &Interner,
        ty: TypeId,
        method: &str,
    ) -> Option<&'static str> {
        match interner.get(ty) {
            Type::Tuple(_) if ARRAY_MUTATORS.contains(&method) => Some("Array"),
            Type::HashShape(_) if HASH_MUTATORS.contains(&method) => Some("Hash"),
            Type::Nominal { .. } => match self.coll_nominal_carrier(interner, ty)? {
                "Array" if ARRAY_MUTATORS.contains(&method) => Some("Array"),
                "Hash" if HASH_MUTATORS.contains(&method) => Some("Hash"),
                _ => None,
            },
            // A union [`Typer::coll_join_envs`] minted — every member a carrier
            // of the SAME collection class. A store widens EACH arm and the
            // union re-joins only where the arms converge (`widen_union`,
            // mutation_widening.rb:316; `MutationRejoin` grows an
            // already-widened carrier — upstream's `joinable_receiver?` had to
            // stop skipping an already-nominal receiver for exactly this
            // reason). The union is not a Dynamic carrier, so minting here does
            // not breach this pass's "never mint from Dynamic" envelope: every
            // member was minted from a literal seed already.
            Type::Union(members) => {
                let members = members.clone();
                let mut carrier: Option<&'static str> = None;
                for m in members {
                    let c = self.coll_nominal_carrier(interner, m)?;
                    match carrier {
                        None => carrier = Some(c),
                        Some(prev) if prev == c => {}
                        Some(_) => return None,
                    }
                }
                match carrier? {
                    "Array" if ARRAY_MUTATORS.contains(&method) => Some("Array"),
                    "Hash" if HASH_MUTATORS.contains(&method) => Some("Hash"),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The reference's `walk_for_outer_mutations` (`mutation_widening.rb:153`)
    /// over one block body: every `local.<mutator>(…)` call in the body's SPAN —
    /// nested blocks included, exactly as the reference recurses — applied in
    /// source order against the carriers `pre` held at the call. Returns the
    /// final `local -> "Array"|"Hash"` widenings.
    ///
    /// This is a SYNTACTIC walk on purpose. The reference does not join the
    /// block's evaluated scope here; it rewrites the outer scope directly, so a
    /// branch-contained mutation inside a block (`arr.push(x) if cond`, named in
    /// its own doc comment) still widens the outer binding — unlike the
    /// branch-contained mutation of probes m18/m20, which lives in the METHOD
    /// body and does go through `Scope#join`.
    ///
    /// The one piece we cannot mirror is Prism's `depth` capture check
    /// (`widen_for_outer_receiver:175`): the lowered AST has no block-parameter
    /// list, so a block param SHADOWING an outer local (`xs.each { |output| … }`)
    /// widens the outer binding here where the reference leaves it alone. That is
    /// FP-neutral by construction — the widening only ever rewrites `Tuple` to
    /// `Nominal[Array]` (or `HashShape` to `Nominal[Hash]`), and both project to
    /// the SAME dispatch class (`receiver_descriptor:209`), so no use site can
    /// change its recorded class because of it.
    fn coll_block_mutations(
        &self,
        ast: &LoweredAst,
        block_body: &[NodeId],
        pre: &TypeEnv,
        interner: &mut Interner,
    ) -> Vec<(String, &'static str)> {
        let Some(body_span) = span_hull(ast, block_body) else {
            return Vec::new();
        };
        let mut carriers: HashMap<String, TypeId> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        let mut hits: Vec<(rigor_parse::Span, String, String)> = ast
            .iter()
            .filter_map(|(_, n)| match n {
                Node::Call { receiver: Some(r), method, safe_nav: false, span, .. }
                    if body_span.0 <= span.0 && span.1 <= body_span.1 =>
                {
                    match ast.get(*r) {
                        Node::LocalVariableRead { name, .. } => {
                            Some((*span, name.clone(), method.clone()))
                        }
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect();
        hits.sort_by_key(|(s, _, _)| *s);
        for (_, name, method) in hits {
            let Some(&cur) = carriers.get(&name).or_else(|| pre.get(&name)) else {
                continue;
            };
            let Some(cls) = self.coll_widen_for_mutator(interner, cur, &method) else {
                continue;
            };
            let Some(ty) = self.coll_nominal(interner, cls) else {
                continue;
            };
            if carriers.insert(name.clone(), ty).is_none() {
                order.push(name);
            }
        }
        order
            .into_iter()
            .filter_map(|name| {
                let ty = *carriers.get(&name)?;
                Some((name, self.coll_nominal_carrier(interner, ty)?))
            })
            .collect()
    }

    /// The bare `Nominal[C]` carrier for `"Array"` / `"Hash"`. Elements are NOT
    /// tracked (`args: vec![]`): undefined-method witnessing is a class-only
    /// lookup, and the reference's own widening fixes an empty seed's elements
    /// at `untyped` on the first mutation anyway (`widen_tuple`, `:251`).
    fn coll_nominal(&self, interner: &mut Interner, class_name: &str) -> Option<TypeId> {
        let class = self.index.class_id(class_name)?;
        Some(interner.intern(Type::Nominal { class, args: vec![] }))
    }

    /// The same carrier, tagged with the accumulated set of erased STORE VALUE
    /// classes — upstream's `Inference::MutationRejoin` (`1ad7351e`, #580,
    /// `v0.3.9`) as this pass needs it.
    ///
    /// Before `v0.3.9` mutation widening was a one-way door: the first content
    /// mutation replaced the literal shape with a bare nominal and every later
    /// store was invisible, so both edges of a branch carried the SAME
    /// `Nominal[Hash]` and the join kept it. The re-join grows the carrier's
    /// value side with each later store, so two edges that stored DIFFERENT
    /// value classes no longer carry the same instantiation — the reference
    /// joins them into a `Type::Union`, and `receiver_descriptor` has no union
    /// arm, so the receiver stops witnessing entirely.
    ///
    /// This pass models exactly that difference and nothing else: the member set
    /// rides in the nominal's `args`, so [`join_flow_envs`]'s identical-`TypeId`
    /// test — this slice's standing decline — does the union for us. The members
    /// themselves are never read; only their EQUALITY is. A store whose value
    /// class this pass cannot name contributes nothing, which is the same thing
    /// the reference's own `Dynamic[top]` seed does (it is in every instantiation
    /// and so never separates two of them).
    ///
    /// The member set is an APPROXIMATION of the reference's model: the
    /// reference carries REAL element types and joins them (`Tuple[1]` and
    /// `Tuple[2]` are different members; `String` and `Dynamic[String]` are too),
    /// while this pass compares ERASED class sets for equality. Two values of
    /// the same erased class therefore agree here where the reference may still
    /// separate them — a residual that can witness where the oracle is silent,
    /// and the reason a faithful port of the content-join model is its own arc.
    ///
    /// [`join_flow_envs`]: crate::join_flow_envs
    fn coll_nominal_with(
        &self,
        interner: &mut Interner,
        class_name: &str,
        members: &[TypeId],
    ) -> Option<TypeId> {
        let class = self.index.class_id(class_name)?;
        let args = match members.len() {
            0 => vec![],
            1 => vec![members[0]],
            _ => vec![interner.intern(Type::Union(members.to_vec()))],
        };
        Some(interner.intern(Type::Nominal { class, args }))
    }

    /// The store-value classes a carrier has accumulated so far, in canonical
    /// order (the `args` of [`Typer::coll_nominal_with`], unwrapped).
    ///
    /// A free function rather than a method: it reads nothing but the interner,
    /// and clippy 1.88 — the version CI pins — flags a `self` that only the
    /// recursive call uses (`only_used_in_recursion`).
    fn coll_value_members(interner: &Interner, ty: TypeId) -> Vec<TypeId> {
        let args = match interner.get(ty) {
            Type::Nominal { args, .. } => args,
            // Defensive: the store path handles a `Nominal[C] | Nominal[C]`
            // union arm-by-arm before reaching here (`widen_union`), so this
            // arm only merges member sets if a union arrives some other way.
            Type::Union(members) => {
                let mut out: Vec<TypeId> = Vec::new();
                for &m in members {
                    for v in Typer::coll_value_members(interner, m) {
                        if !out.contains(&v) {
                            out.push(v);
                        }
                    }
                }
                out.sort_unstable();
                return out;
            }
            _ => return Vec::new(),
        };
        match args.first() {
            None => Vec::new(),
            Some(&a) => match interner.get(a) {
                Type::Union(ms) => ms.clone(),
                _ => vec![a],
            },
        }
    }

    /// The erased classes one stored value contributes to the carrier's value
    /// side — the erased class of the value expression's TYPED answer, not a
    /// syntactic read of its shape (issue #128). The reference names the store
    /// by typing the argument; the syntactic classifier this replaced named
    /// strictly less, and the gap was symmetric: `h['a'] = 1.to_s` lost a row
    /// (the reference's two edges agreed where ours could not) and
    /// `h['b'] = 'y'.to_i` false-positived (our two edges agreed where the
    /// reference's did not).
    ///
    /// The env the value is typed against is the pass's OWN threaded `tenv` —
    /// the same sparse env the local-assignment arm already calls `type_of`
    /// with — never the rules layer's `ScopedEnv` (top-level only, and empty
    /// inside a `def` body, which is where mutation code lives). A sparse env
    /// is safe: an unbound local types `Dynamic[top]`, which contributes
    /// nothing — the same thing the reference's own `Dynamic[top]` seed does
    /// (it is in every instantiation and so never separates two of them).
    fn coll_store_value_classes(
        &self,
        ast: &LoweredAst,
        node: NodeId,
        tenv: &TypeEnv,
        interner: &mut Interner,
    ) -> Vec<TypeId> {
        let ty = self.stmt_value_type(ast, node, tenv, interner);
        let mut out = Vec::new();
        self.coll_erased_store_member(interner, ty, &mut out);
        out
    }

    /// Pushes the erased member classes of one typed store value onto `out`.
    ///
    /// The answer is ERASED before it becomes a member: the member set is
    /// compared by interned `TypeId` equality and never read, so a precise type
    /// (`Constant["a"]` for `'a'`) would separate two edges the reference joins.
    /// Members stay bare class nominals with no args.
    ///
    /// It is a SET rather than one class because a union value contributes every
    /// member the reference's join puts in the element set: `out << (flag ? 'a'
    /// : 1)` types `String | Integer`, both go in, and a later `String` store
    /// then adds nothing (row r15). A `Dynamic` member — `Dynamic[top]` or any
    /// faceted dynamic — names nothing: the erased class of a dynamic is
    /// `untyped`, not a class.
    fn coll_erased_store_member(&self, interner: &mut Interner, ty: TypeId, out: &mut Vec<TypeId>) {
        match interner.get(ty) {
            Type::Union(members) => {
                let members = members.clone();
                for m in members {
                    self.coll_erased_store_member(interner, m, out);
                }
            }
            // Erasing through the base (or an intersection's first member)
            // matches `erase_to_rbs_named`; the store path's own normalizer
            // (`widen_value_pinned`) would keep these wrappers whole, so this
            // is the merge-direction residual — unreachable via `type_of`
            // today since no expression types to them here.
            Type::Refined { base, .. } | Type::Difference { base, .. } => {
                let base = *base;
                self.coll_erased_store_member(interner, base, out);
            }
            Type::Intersection(members) => {
                if let Some(&m) = members.first() {
                    self.coll_erased_store_member(interner, m, out);
                }
            }
            // The erased class of a nominal is itself minus the args — interned
            // straight off the `ClassId`, so a PROJECT-class nominal names too
            // (`CoreIndex::class_name_of` can only spell the nine CORE_CLASSES).
            Type::Nominal { class, .. } | Type::DataInstance { class, .. } => {
                let class = *class;
                Self::push_store_member(interner, out, Type::Nominal { class, args: vec![] });
            }
            // A class object (`Time`, `Array`) contributes its singleton
            // carrier — already erased-level, and injective, so it separates
            // exactly the edges the reference's `singleton(C)` member does.
            Type::Singleton(class) => {
                let class = *class;
                Self::push_store_member(interner, out, Type::Singleton(class));
            }
            // `Constants`, `Tuple`, `HashShape`, `IntegerRange`: the index's
            // type→class-name erasure (`"a"` -> `String`, `[1]` -> `Array`,
            // `1..3` -> `Integer`). `Dynamic`/`Top`/anything else names
            // nothing.
            _ => {
                if let Some(name) = self.index.class_name_of(interner, ty) {
                    if let Some(class) = self.index.class_id(name) {
                        Self::push_store_member(
                            interner,
                            out,
                            Type::Nominal { class, args: vec![] },
                        );
                    }
                }
            }
        }
    }

    /// Interns `member` and pushes it unless already present — member sets are
    /// sets, so two arms of a union erasing to the same class contribute once.
    fn push_store_member(interner: &mut Interner, out: &mut Vec<TypeId>, member: Type) {
        let m = interner.intern(member);
        if !out.contains(&m) {
            out.push(m);
        }
    }

    /// [`join_flow_envs`] for this pass: two carriers of the SAME collection
    /// class that differ only in the store-value members they accumulated join
    /// to a `Type::Union` of the two, which is what the reference's `Scope#join`
    /// leaves behind once `MutationRejoin` lets the edges diverge.
    ///
    /// Keeping the union rather than collapsing to untyped matters in BOTH
    /// directions. A use site sees no witness either way ([`Typer::coll_carrier`]
    /// has no union arm, exactly as `receiver_descriptor` has none). But a later
    /// STORE re-joins the union into one carrier, and mastodon's
    /// `application_helper.rb:180` needs that: a branch push makes the edges
    /// diverge, the unconditional push after it re-joins them, and the reference
    /// fires on the `compact_blank` that follows. Collapsing to untyped loses
    /// that row, because a mutator on a Dynamic carrier never mints.
    ///
    /// Every other disagreement — a shape against a nominal, two different
    /// collection classes, a non-carrier — still widens to untyped, which is this
    /// pass's standing decline (probes m18/m20).
    ///
    /// [`join_flow_envs`]: crate::join_flow_envs
    fn coll_join_envs(&self, a: &TypeEnv, b: &TypeEnv, interner: &mut Interner) -> TypeEnv {
        let u = interner.untyped();
        let mut out = TypeEnv::with_capacity(a.len());
        for (k, av) in a {
            let v = match b.get(k) {
                Some(bv) if bv == av => *av,
                Some(bv) => self.coll_union_carrier(interner, *av, *bv).unwrap_or(u),
                None => u,
            };
            out.insert(k.clone(), v);
        }
        for k in b.keys() {
            if !a.contains_key(k) {
                out.insert(k.clone(), u);
            }
        }
        out
    }

    /// The union of two collection carriers of the same class, or `None` when
    /// they are not both that. Members are flattened and sorted so the result is
    /// a function of the SET — two edges that reach the same set must intern to
    /// the same `TypeId` or the next join separates them for no reason.
    fn coll_union_carrier(
        &self,
        interner: &mut Interner,
        a: TypeId,
        b: TypeId,
    ) -> Option<TypeId> {
        let ca = self.coll_union_class(interner, a)?;
        let cb = self.coll_union_class(interner, b)?;
        if ca != cb {
            return None;
        }
        let mut members: Vec<TypeId> = Vec::new();
        for side in [a, b] {
            match interner.get(side) {
                Type::Union(ms) => {
                    for &m in ms {
                        if !members.contains(&m) {
                            members.push(m);
                        }
                    }
                }
                _ => {
                    if !members.contains(&side) {
                        members.push(side);
                    }
                }
            }
        }
        members.sort_unstable();
        match members.len() {
            0 => None,
            1 => Some(members[0]),
            _ => Some(interner.intern(Type::Union(members))),
        }
    }

    /// The single collection class a carrier — or a union of carriers — stands
    /// for, or `None` when it is neither.
    fn coll_union_class(&self, interner: &Interner, ty: TypeId) -> Option<&'static str> {
        match interner.get(ty) {
            Type::Nominal { .. } => self.coll_nominal_carrier(interner, ty),
            Type::Union(members) => {
                let mut cls: Option<&'static str> = None;
                for &m in members {
                    let c = self.coll_nominal_carrier(interner, m)?;
                    match cls {
                        None => cls = Some(c),
                        Some(prev) if prev == c => {}
                        Some(_) => return None,
                    }
                }
                cls
            }
            _ => None,
        }
    }

    /// The argument positions of `method` that STORE a value into the receiver —
    /// the only ones `MutationRejoin` grows the value side from. Every other
    /// mutator (`delete`, `clear`, `sort!`, …) leaves the member set alone, and
    /// the reference's remaining content adders (`concat`/`insert`/`fill`/
    /// `replace`) stay out of scope for this pass. A Hash `[]=`/`store` also
    /// joins the KEY's type into the reference's K side (`join_added_pairs`) —
    /// this pass's flat member set is value-side only, a documented residual.
    fn coll_store_value_args<'a>(method: &str, args: &'a [NodeId]) -> &'a [NodeId] {
        match method {
            "[]=" | "store" => args.last().map(std::slice::from_ref).unwrap_or(&[]),
            "<<" | "push" | "append" | "unshift" | "prepend" => args,
            _ => &[],
        }
    }
}

/// The per-program write tables the collection-shape walker threads (bundled to
/// keep the recursive walkers' signatures under the clippy argument limit).
struct CollCtx<'a> {
    /// [`collect_flow_writes`] + [`indexed_flow_writes`] — rebinds AND in-place
    /// mutations, the conservative widening set.
    writes: &'a [(rigor_parse::Span, String)],
    /// REBINDS only (no mutator entries) — used to veto the keep-nominal
    /// widening when the call that mutates also reassigns the same local.
    rebinds: &'a [(rigor_parse::Span, String)],
}
