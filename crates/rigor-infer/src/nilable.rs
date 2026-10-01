//! Nilable receivers (ADR-0038 Slice 1): the `nil_flow_*` walker behind
//! [`Typer::nilable_receiver_snapshots`], which threads a type env and a
//! `local -> non-nil arm` nilability map through the program for the rules
//! layer's `call.possible-nil-receiver`, plus the `Array.new` provenance and
//! nilable-source classification it seeds from.

use std::collections::{HashMap, HashSet};

use rigor_parse::{Compound, LoweredAst, Node, NodeId, StatementsKind};
use rigor_types::{Interner, Scalar, Type, TypeId};

use crate::{
    block_bound_names, collect_flow_writes, drop_indexed_narrowings, indexed_flow_writes,
    is_shape_mutator, local_rebinds, multi_target_binder, rebound_within, widen_flow_writes,
    widen_penv_writes, TypeEnv, Typer,
};

/// The reference's `Array.new(n)` tuple-lift cap (`ARRAY_NEW_TUPLE_LIMIT`,
/// `method_dispatcher.rb`): a constant size `n ≤ 16` lifts to a `Tuple`; a size
/// `> 16` (or a non-constant / zero-arg call) stays `Nominal[Array]`. Ported
/// faithfully (ADR-0039); re-measured on every upstream bump (UPSTREAM.md).
const ARRAY_NEW_TUPLE_LIMIT: i64 = 16;

impl<'i> Typer<'i> {
    // -----------------------------------------------------------------------
    // ADR-0038 Slice 1 — `call.possible-nil-receiver` on the threaded flow-eval
    // -----------------------------------------------------------------------

    /// Compute the per-call-node nil-receiver snapshot map (ADR-0038 Slice 1):
    /// `call node id -> non-nil core arm C` for every bare-local receiver that is
    /// certainly `C | nil` and unguarded at the use. The rules layer's
    /// `check_nil_receiver` fires from this map (applying the method-absent-on-
    /// NilClass / present-on-C gate). This REPLACES the prior `enclosing_def`
    /// span-scan, so a nilable local now witnesses in block / top-level scopes,
    /// not only inside a named `def`.
    ///
    /// It threads two facts straight-line through the program, DESCENDING into
    /// block bodies:
    /// - `tenv` — a TYPE env, INHERITED (cloned) into block bodies so a slice /
    ///   `.new` receiver typed in an OUTER scope (`random_array = Array.new(n){…}`)
    ///   is visible to a source in a NESTED block (`select_subset = random_array[
    ///   0..n]`). Widened precisely (only written locals) on unmodeled constructs.
    /// - `nenv` — a NILABILITY fact map, `local -> non-nil core arm C` (the local
    ///   is currently `C | nil`). It starts EMPTY in every block body.
    ///
    /// ## FP-safety (ADR-0038 §2/§3 decline backstop)
    ///
    /// - **Same-block-body locality.** `nenv` is FRESH per block, so a fact never
    ///   crosses INTO a block. Block parameters are not lowered (so cannot be
    ///   cleared by name); the fresh env makes a param shadowing an outer local
    ///   unable to leak a stale fact — the shadowing FP class is structurally
    ///   impossible.
    /// - **Unmodeled ⇒ clear all.** ANY statement not in the modeled set (control
    ///   flow, multi-assign, ivar write, …) CLEARS ALL `nenv` facts. Multi-assign
    ///   targets are invisible in the lowered arena, so a per-name scan could miss
    ///   a reassignment; the clear-all is the bulletproof choice for the direct
    ///   fire gate.
    /// - **Block descent clears outer facts.** After descending a block, ALL outer
    ///   `nenv` facts are cleared (a block capture may invisibly reassign an outer
    ///   local).
    /// - **Guards clear the fact.** A `.nil?`/`present?`/`blank?`/`presence` call
    ///   or a safe-nav call on the local removes it (narrowed); an `&&`/`||`
    ///   operand context clears all facts (unmodeled narrowing in Slice 1).
    ///
    /// Residual (documented Slice 1 limit): a multi-assign that reassigns a
    /// SOURCE receiver's TYPE leaves `tenv` stale (targets invisible), which could
    /// feed a wrong NEW source. Contrived and survey-absent; closed when
    /// multi-assign is modeled. Every fire is gated by `fp_audit.py` on the survey.
    pub fn nilable_receiver_snapshots(
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
        let rebinds = local_rebinds(ast);
        let mut tenv = TypeEnv::new();
        let mut nenv: HashMap<String, &'static str> = HashMap::new();
        let mut penv: HashSet<String> = HashSet::new();
        self.nil_flow_scope(
            ast, &body, &mut tenv, &mut nenv, &mut penv, &writes, &rebinds, interner, &mut out,
        );
        out
    }

    /// Thread `(tenv, nenv, penv)` through a scope's statements in source order.
    /// `penv` is the `Array.new`-Nominal-provenance set (ADR-0039 §2) — the locals
    /// currently bound to an array the reference keeps `Nominal[Array]` (not a
    /// `Tuple`), the only receivers the array-slice possible-nil source may fire on.
    /// It travels on the tenv side (inherited into blocks; widened by tenv's rules).
    #[allow(clippy::too_many_arguments)]
    fn nil_flow_scope(
        &self,
        ast: &LoweredAst,
        stmts: &[NodeId],
        tenv: &mut TypeEnv,
        nenv: &mut HashMap<String, &'static str>,
        penv: &mut HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        rebinds: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
    ) {
        for &s in stmts {
            self.nil_flow_stmt(ast, s, tenv, nenv, penv, writes, rebinds, interner, out);
        }
    }

    /// Apply one statement's effect on `(tenv, nenv, penv)` and record any nil uses.
    ///
    /// A statement lowered under a CROSSED block/lambda (a recovered child
    /// carrying [`LoweredAst::closure_bound_names`] — `super { |o| … }`) is
    /// processed on scratch envs with the closure's bound names dropped: those
    /// names read the parameter, never the shadowed outer binding, and the
    /// closure's effects do not reach the enclosing scope (rigor-rs#137).
    #[allow(clippy::too_many_arguments)]
    fn nil_flow_stmt(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        nenv: &mut HashMap<String, &'static str>,
        penv: &mut HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        rebinds: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
    ) {
        let bound = ast.closure_bound_names(id);
        if !bound.is_empty() {
            let mut t = tenv.clone();
            let mut n = nenv.clone();
            let mut p = penv.clone();
            for name in bound {
                t.remove(name.as_str());
                n.remove(name.as_str());
                p.remove(name.as_str());
            }
            return self.nil_flow_stmt_inner(
                ast, id, &mut t, &mut n, &mut p, writes, rebinds, interner, out,
            );
        }
        self.nil_flow_stmt_inner(ast, id, tenv, nenv, penv, writes, rebinds, interner, out)
    }

    /// The per-node half of [`Typer::nil_flow_stmt`].
    #[allow(clippy::too_many_arguments)]
    fn nil_flow_stmt_inner(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        nenv: &mut HashMap<String, &'static str>,
        penv: &mut HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        rebinds: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
    ) {
        match ast.get(id) {
            // A carrier is descended so the uses in it are recorded, as before;
            // what differs by kind is the state it leaves behind (rigor-rs#153).
            Node::Statements { body, kind, span } => {
                let (body, kind, span) = (body.clone(), *kind, *span);
                match kind {
                    StatementsKind::Sequence => {
                        self.nil_flow_scope(
                            ast, &body, tenv, nenv, penv, writes, rebinds, interner, out
                        );
                    }
                    // Its writes may not run, or not in this order: widen them
                    // and drop their facts after the descent. A `Jump` carrier
                    // holds the jump's VALUE expressions (`break (x = 1)`), so
                    // it reads exactly like `Recovered` here — the write runs
                    // only when the jump does, which nothing upstream proves.
                    StatementsKind::Recovered | StatementsKind::Jump(_) => {
                        self.nil_flow_scope(
                            ast, &body, tenv, nenv, penv, writes, rebinds, interner, out
                        );
                        widen_flow_writes(writes, span, tenv, interner);
                        widen_penv_writes(writes, span, penv);
                        for (w, name) in writes {
                            if span.0 <= w.0 && w.1 <= span.1 {
                                nenv.remove(name);
                            }
                        }
                    }
                    // Its writes never reach the scope: record the uses in a
                    // written value, bind nothing.
                    StatementsKind::Inert => {
                        for s in body {
                            match ast.get(s) {
                                Node::LocalVariableWrite { value, .. }
                                | Node::LocalVariableOpWrite { value, .. }
                                | Node::MultiWrite { value, .. } => {
                                    let value = *value;
                                    self.nil_flow_expr(
                                        ast, value, tenv, nenv, penv, writes, rebinds, interner, out
                                    );
                                }
                                _ => self.nil_flow_stmt(
                                    ast, s, tenv, nenv, penv, writes, rebinds, interner, out,
                                ),
                            }
                        }
                    }
                }
            }
            Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                // Record uses in the RHS (and descend any block it carries) BEFORE
                // rebinding — a use of a currently-nilable local reads the fact.
                self.nil_flow_expr(ast, value, tenv, nenv, penv, writes, rebinds, interner, out);
                let src = self.nilable_source_class(ast, value, tenv, penv, interner);
                let prov = self.array_new_nominal_provenance(ast, value, tenv, interner);
                let vty = self.type_of(ast, value, tenv, interner);
                tenv.insert(name.clone(), vty);
                // A rebind invalidates the local's indexed-narrowing slot
                // records, exactly as `apply_subtree_effects` drops them for
                // every rebind in a subtree — without this a `s[k] ||= v`
                // record outlives `s = "abc"` and answers a later `s[k]` read
                // for the NEW binding (rigor-rs#352).
                drop_indexed_narrowings(tenv, &name);
                // Rebinding always refreshes the provenance (any non-`Array.new`
                // RHS clears it).
                if prov {
                    penv.insert(name.clone());
                } else {
                    penv.remove(&name);
                }
                match src {
                    Some(c) => {
                        nenv.insert(name, c);
                    }
                    None => {
                        nenv.remove(&name);
                    }
                }
            }
            // `a, b = rhs` — record the RHS uses, then rebind every target to
            // its destructured slot type and DROP the per-name nil / `Array.new`
            // facts. Dropping is the FP-safe direction (a dropped `C | nil` fact
            // can only silence `call.possible-nil-receiver`, never add a
            // firing), and it is what the binder's `soften_optional_slot` says
            // anyway: a destructured slot never carries a manufactured nil.
            Node::MultiWrite { targets, value, .. } => {
                let (targets, value) = (targets.clone(), *value);
                self.nil_flow_expr(ast, value, tenv, nenv, penv, writes, rebinds, interner, out);
                let rhs = self.type_of(ast, value, tenv, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    nenv.remove(&name);
                    penv.remove(&name);
                    tenv.insert(name.clone(), ty);
                    drop_indexed_narrowings(tenv, &name);
                }
                // An `h[k]` index target stores through `[]=` on the
                // post-binding scope — a MUTATION of `h`, not a rebind: widen
                // `tenv` (the stored slot's type is unmodelled) but keep any
                // `nenv`/`penv` fact, exactly as a bare `h[k] = v` call does
                // (rigor-rs#134).
                // rigor-rs#352: the mutation mints the carrier
                // `widen_for_mutator("[]=")` leaves (`Nominal[String]` for a
                // value-pinned `s`), not a blunt Dynamic — a later `x = s[k]`
                // stays a nilable `String | nil` source. The nominal is
                // computed on the PRE-binding so `widen_flow_writes`' Dynamic
                // pass (which still covers any writes nested inside the target
                // expression) cannot shadow it.
                for (name, tspan, key) in targets.index_writes() {
                    // The store also invalidates the slot's `||=` record
                    // (`invalidate_indexed_write`, rigor-rs#342).
                    if let Some(key) = crate::flow_writes::index_target_drop_key(key) {
                        tenv.remove(&crate::indexed_narrowing_key(&name, &key));
                    }
                    let pre = tenv.get(&name).copied();
                    let widened = pre
                        .and_then(|pre| self.widen_mutated_binding(pre, "[]=", interner));
                    widen_flow_writes(writes, tspan, tenv, interner);
                    // The index-target store is a MUTATION, not a rebind:
                    // `widen_mutated_binding` declining (`None` — the binding is
                    // already a `Nominal`/`Dynamic`) keeps the binding it had,
                    // it does not fall back to `widen_flow_writes`' Dynamic.
                    if let Some(pre) = pre {
                        tenv.insert(name, widened.unwrap_or(pre));
                    }
                }
            }
            Node::LocalVariableOpWrite { name, .. } => {
                // `x += …` / `x ||= …` reads-then-writes ⇒ the nil possibility is
                // narrowed/replaced; drop every fact and widen the type. The
                // rebind also drops the local's indexed-narrowing records.
                let name = name.clone();
                nenv.remove(&name);
                penv.remove(&name);
                let u = interner.untyped();
                tenv.insert(name.clone(), u);
                drop_indexed_narrowings(tenv, &name);
            }
            Node::Call { .. } | Node::IndexWrite { .. } | Node::AttrWrite { .. } => {
                self.nil_flow_expr(ast, id, tenv, nenv, penv, writes, rebinds, interner, out);
            }
            Node::Definition { body, .. }
            | Node::ClassDef { body, .. }
            | Node::ModuleDef { body, .. } => {
                // Independent scope: fresh `tenv`/`nenv`/`penv`, no effect on the
                // enclosing scope.
                let body = body.clone();
                let mut t = TypeEnv::new();
                let mut n: HashMap<String, &'static str> = HashMap::new();
                let mut p: HashSet<String> = HashSet::new();
                self.nil_flow_scope(
                    ast, &body, &mut t, &mut n, &mut p, writes, rebinds, interner, out
                );
            }
            // `for s[k] in xs` — each index target stores the element through
            // `[]=` on `s` (`bind_for_index` → `widen_index_target`), and the
            // post-`for` join keeps the widened carrier beside the
            // zero-iteration binding: a value-pinned `s` loses its `Constant`
            // to `Nominal[String]`, so a later `x = s[k]` is still a nilable
            // `String | nil` source (rigor-rs#352). The store also drops the
            // slot's `||=` record (`invalidate_indexed_write`). The pass does
            // not descend the body — every other write inside widens to
            // Dynamic exactly as the `other` arm does.
            Node::Loop { index_writes, .. } => {
                let span = ast.get(id).span();
                let mut widened: Vec<(String, TypeId)> = Vec::new();
                for (name, _, key) in index_writes {
                    if let Some(key) =
                        crate::flow_writes::index_target_drop_key(key.clone())
                    {
                        tenv.remove(&crate::indexed_narrowing_key(name, &key));
                    }
                    if let Some(&pre) = tenv.get(name) {
                        // The store is a MUTATION: a declined widening keeps
                        // the prior binding rather than falling back to
                        // `widen_flow_writes`' Dynamic.
                        widened.push((
                            name.clone(),
                            self.widen_mutated_binding(pre, "[]=", interner)
                                .unwrap_or(pre),
                        ));
                    }
                }
                widen_flow_writes(writes, span, tenv, interner);
                widen_penv_writes(writes, span, penv);
                nenv.clear();
                for (name, w) in widened {
                    // A rebind of the index-target local INSIDE the loop wins
                    // over the `[]=` widening: `widen_flow_writes` already
                    // landed `Dynamic`, and re-inserting the value derived
                    // from the PRE-loop binding would resurrect a dead
                    // binding (`for s[0] in xs; s = "q"; end` — the join types
                    // `s` from the pre-loop binding and the rebound one, so
                    // the nilable `x = s[k]` source is gone; rigor-rs#352
                    // review).
                    if !rebound_within(rebinds, span, &name) {
                        tenv.insert(name, w);
                    }
                }
            }
            // `rescue => s[k]` — the same index-target `[]=` store
            // (`bind_rescue_reference` → `widen_index_target`), with the same
            // widened carrier surviving the post-rescue join (rigor-rs#352).
            Node::BeginRescue { clauses, .. } => {
                let span = ast.get(id).span();
                let mut widened: Vec<(String, TypeId)> = Vec::new();
                for c in clauses {
                    for (name, _, key) in &c.index_writes {
                        if let Some(key) =
                            crate::flow_writes::index_target_drop_key(key.clone())
                        {
                            tenv.remove(&crate::indexed_narrowing_key(name, &key));
                        }
                        if let Some(&pre) = tenv.get(name) {
                            widened.push((
                                name.clone(),
                                self.widen_mutated_binding(pre, "[]=", interner)
                                    .unwrap_or(pre),
                            ));
                        }
                    }
                }
                widen_flow_writes(writes, span, tenv, interner);
                widen_penv_writes(writes, span, penv);
                nenv.clear();
                for (name, w) in widened {
                    // Same rebind-in-span guard as `Node::Loop`: a `s = "q"`
                    // in a clause body or the `ensure` rebinds `s`, so the
                    // pre-rescue `[]=` widening must not reinsert
                    // (`rescue => s[0]; s = "q"` — rigor-rs#352 review).
                    if !rebound_within(rebinds, span, &name) {
                        tenv.insert(name, w);
                    }
                }
            }
            // Any other statement (`if`/`unless`/`while`/`case`/logical/begin/
            // multi-assign/ivar-write/…) is UNMODELED in Slice 1: widen `tenv` and
            // `penv` for the locals it writes, and CLEAR ALL `nenv` facts (decline
            // backstop — no fact survives an unmodeled construct). No descent.
            other => {
                let span = other.span();
                widen_flow_writes(writes, span, tenv, interner);
                widen_penv_writes(writes, span, penv);
                nenv.clear();
            }
        }
    }

    /// Evaluate an expression for nil-receiver USES: record `call -> arm` for a
    /// bare-local receiver in `nenv`, clear the fact on a guard/safe-nav call, and
    /// descend a block body with a FRESH `nenv` + INHERITED `(tenv, penv)`.
    #[allow(clippy::too_many_arguments)]
    fn nil_flow_expr(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        nenv: &mut HashMap<String, &'static str>,
        penv: &mut HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        rebinds: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
    ) {
        match ast.get(id) {
            Node::Call { receiver, method, args, block_body, block_locals, block_params, safe_nav, span, .. } => {
                let receiver = *receiver;
                let method = method.clone();
                let args = args.clone();
                let block_body = block_body.clone();
                let safe_nav = *safe_nav;
                let call_span = *span;
                let bound: Vec<String> =
                    block_bound_names(block_locals, block_params).map(str::to_string).collect();
                // Recurse the receiver first (a nested use like `a.b` in `a.b.c`).
                if let Some(r) = receiver {
                    self.nil_flow_expr(ast, r, tenv, nenv, penv, writes, rebinds, interner, out);
                }
                if let Some(r) = receiver {
                    if let Node::LocalVariableRead { name, .. } = ast.get(r) {
                        let is_guard = matches!(
                            method.as_str(),
                            "nil?" | "present?" | "blank?" | "presence"
                        );
                        // Record the use: currently-nilable bare local, plain (not
                        // safe-nav) call, non-guard method. `check_nil_receiver`
                        // applies the NilClass-absent / arm-present gate.
                        if !safe_nav && !is_guard {
                            if let Some(&arm) = nenv.get(name) {
                                out.insert(id, arm);
                            }
                        }
                        // A guard or safe-nav call on the local narrows nil away
                        // for SUBSEQUENT uses ⇒ drop the fact.
                        if safe_nav || is_guard {
                            nenv.remove(name);
                        }
                        // Issue #352 — the receiver-mutation widening
                        // (`MutationWidening.widen_for_mutator`): `s[k] = v` /
                        // `s << x` rewrites `s` in place, and a value-pinned
                        // `Constant["abc"]` loses its pin to `Nominal[String]`
                        // — the carrier under which `x = s[0]` is a nilable
                        // `String | nil` source (the Constant-receiver keystone
                        // below otherwise declines and `x.upcase` stays
                        // silent). Only the carriers `widen_mutated_binding`
                        // widens change here; a `Nominal`/`Dynamic` binding is
                        // already mutation-safe.
                        if is_shape_mutator(&method) {
                            // `IndexedNarrowing.invalidate_after_call` rides
                            // first: a stable-key `s[k] = v` drops that slot's
                            // `||=` record, every other shape mutator drops
                            // every record rooted at `s`.
                            let drop_key = if method == "[]=" {
                                args.first()
                                    .and_then(|&a| crate::stable_index_key(ast.get(a)))
                            } else {
                                None
                            };
                            self.drop_indexed_mutation(
                                name,
                                &method,
                                drop_key.as_ref(),
                                tenv,
                            );
                            if let Some(&pre) = tenv.get(name) {
                                if let Some(widened) =
                                    self.widen_mutated_binding(pre, &method, interner)
                                {
                                    tenv.insert(name.clone(), widened);
                                }
                            }
                        }
                    }
                }
                for a in &args {
                    self.nil_flow_expr(ast, *a, tenv, nenv, penv, writes, rebinds, interner, out);
                }
                if !block_body.is_empty() {
                    // Same-block locality: descend with a FRESH `nenv`, inheriting
                    // (cloning) `(tenv, penv)` MINUS the names the block binds —
                    // a bound name reads its own parameter/local, never the
                    // shadowed outer local (rigor-rs#137; the port has no
                    // block-entry typing, so the name is simply unbound inside).
                    // Afterwards CLEAR ALL outer `nenv` (a block capture may
                    // invisibly reassign an outer local), and widen `tenv`/`penv`
                    // for locals the block visibly writes (a capture-write must
                    // not leave a stale type/provenance behind).
                    let mut btenv = tenv.clone();
                    let mut bnenv: HashMap<String, &'static str> = HashMap::new();
                    let mut bpenv = penv.clone();
                    for name in &bound {
                        btenv.remove(name);
                        bpenv.remove(name.as_str());
                    }
                    self.nil_flow_scope(
                        ast, &block_body, &mut btenv, &mut bnenv, &mut bpenv, writes, rebinds,
                        interner, out,
                    );
                    nenv.clear();
                    widen_flow_writes(writes, call_span, tenv, interner);
                    widen_penv_writes(writes, call_span, penv);
                }
            }
            Node::Logical { left, right, .. } => {
                // `&&`/`||` — unmodeled narrowing in Slice 1. Clear all facts
                // (decline), then recurse for block/call reachability.
                let (left, right) = (*left, *right);
                nenv.clear();
                self.nil_flow_expr(ast, left, tenv, nenv, penv, writes, rebinds, interner, out);
                self.nil_flow_expr(ast, right, tenv, nenv, penv, writes, rebinds, interner, out);
            }
            // `h[k] ||= v` / `h[k] &&= v` / `h[k] op= v` — a compound index
            // write. The receiver read is recorded exactly as a `[]=`
            // `Call` receiver's: evaluating `h[k]` on a nil `h` raises just
            // the same. The store can only REPLACE `h`'s contents, never
            // leave `h` nil — but narrowing it away is this pass's decline,
            // matching the `Call` arm's keep-the-fact treatment of
            // `h[k] = v`. Operands are EXPRESSION position: evaluating them
            // preserves the uses the old recovered `Statements` carrier
            // descended (rigor-rs#135).
            Node::IndexWrite {
                receiver,
                indices,
                value,
                compound,
                operand,
                ..
            } => {
                let (receiver, indices, value, compound, operand) =
                    (*receiver, indices.clone(), *value, compound.clone(), *operand);
                if let Some(r) = receiver {
                    self.nil_flow_expr(ast, r, tenv, nenv, penv, writes, rebinds, interner, out);
                }
                if let Some(r) = receiver {
                    if let Node::LocalVariableRead { name, .. } = ast.get(r) {
                        if let Some(&arm) = nenv.get(name) {
                            out.insert(id, arm);
                        }
                        // rigor-rs#352: the compound index write stores through
                        // `[]=` — widen `s` the way `eval_index_or_write` does
                        // (`IndexWriteWidening` → `widen_for_mutator`), so a
                        // value-pinned String loses its pin to `Nominal[String]`
                        // and a later `x = s[k]` stays a nilable source. An
                        // `operand` `||=` additionally records the stored-slot
                        // narrowing `s[k] -> narrow_truthy(s[k]) | v`, which is
                        // what `nilable_source_class` then reads for `x = s[k]`
                        // — `s[0] ||= "x"` narrows the read to `String | "x"`,
                        // NON-nilable, so `x.upcase` must not fire. The record
                        // computes on the PRE-widening binding, exactly as
                        // `eval_index_or_write` does.
                        let stored = if operand && matches!(compound, Compound::Or) {
                            let tracked = tenv
                                .get(name)
                                .is_some_and(|&pre| self.fully_tracked_type(pre, interner));
                            if tracked {
                                indices.first().and_then(|&k| {
                                    crate::stable_index_key(ast.get(k)).map(|key| {
                                        let ty = self.index_write_value_type(
                                            ast, r, &indices, value, &compound, tenv,
                                            interner,
                                        );
                                        (crate::indexed_narrowing_key(name, &key), ty)
                                    })
                                })
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        if let Some(&pre) = tenv.get(name) {
                            if let Some(widened) =
                                self.widen_mutated_binding(pre, "[]=", interner)
                            {
                                tenv.insert(name.clone(), widened);
                            }
                        }
                        if let Some((key, stored)) = stored {
                            tenv.insert(key, stored);
                        }
                    }
                }
                for i in &indices {
                    self.nil_flow_expr(ast, *i, tenv, nenv, penv, writes, rebinds, interner, out);
                }
                self.nil_flow_expr(ast, value, tenv, nenv, penv, writes, rebinds, interner, out);
            }
            // `h.attr op= v` — a compound ATTRIBUTE write. It is never a
            // nilable USE site (`eval_attribute_compound_write` types the
            // node without dispatching a checkable `attr`/`attr=` call —
            // probed silent at `e59b7b89`, rigor-rs#343). A bare-local
            // receiver's nilable fact is still CONSUMED: `h.default ||= 0`
            // raises on `nil` (`for 1`, not `for 1 | nil`, after it on the
            // oracle) and `h&.default ||= 0` is the write-through-`&.`
            // the `Call` arm's safe-nav consume already models.
            Node::AttrWrite {
                receiver, value, ..
            } => {
                let (receiver, value) = (*receiver, *value);
                if let Some(r) = receiver {
                    self.nil_flow_expr(ast, r, tenv, nenv, penv, writes, rebinds, interner, out);
                    if let Node::LocalVariableRead { name, .. } = ast.get(r) {
                        nenv.remove(name);
                    }
                }
                self.nil_flow_expr(ast, value, tenv, nenv, penv, writes, rebinds, interner, out);
            }
            _ => {}
        }
    }

    /// Whether `rhs_id` is an `Array.new(...)` the REFERENCE keeps `Nominal[Array]`
    /// (not a `Tuple`) — the FP-safe provenance for the possible-nil array-slice
    /// source (ADR-0039 §2). True iff `Array.new` with ZERO args, or a first arg
    /// that types to `Constant(Int(n))` with `n > ARRAY_NEW_TUPLE_LIMIT`. A small /
    /// non-constant / non-integer size ⇒ false: the reference MIGHT `Tuple` it
    /// (it may fold a constant rigor-rs leaves `Dynamic`), so claiming Nominal
    /// would over-fire. Syntactic on the `Array` constant + a Constant size arg;
    /// never a bare `Nominal[Array]` (which a `.map` result the reference Tuples
    /// also carries).
    fn array_new_nominal_provenance(
        &self,
        ast: &LoweredAst,
        rhs_id: NodeId,
        tenv: &TypeEnv,
        interner: &mut Interner,
    ) -> bool {
        let Node::Call { receiver: Some(recv), method, args, .. } = ast.get(rhs_id) else {
            return false;
        };
        if method != "new" {
            return false;
        }
        let Node::ConstantRead { name, .. } = ast.get(*recv) else {
            return false;
        };
        if name != "Array" {
            return false;
        }
        // Zero-arg `Array.new` ⇒ the reference declines the tuple lift ⇒ Nominal.
        if args.is_empty() {
            return true;
        }
        // Else the FIRST arg must be a Constant integer strictly above the tuple
        // limit (small / non-constant / non-integer size ⇒ decline, FP-safe).
        let first = args[0];
        let fty = self.type_of(ast, first, tenv, interner);
        matches!(interner.get(fty), Type::Constant(Scalar::Int(n)) if *n > ARRAY_NEW_TUPLE_LIMIT)
    }

    /// The non-nil core arm `C` of a nilable SOURCE expression `value`, or `None`
    /// (not a modeled nil source ⇒ the local is treated non-nilable).
    ///
    /// Two sources (both zero-FP by construction):
    /// (a) **String slice** `str[Range]` — the single-`Range`-arg `#[]` form on a
    ///     non-`Constant` `String` receiver. RBS types it `String?`, so the
    ///     non-nil arm is `String`. A `Constant` receiver is declined: the
    ///     reference constant-folds a string LITERAL slice to a concrete non-nil
    ///     value (`"hello"[0..2]` ⇒ `"hel"`), so it never sees `String | nil`;
    ///     rigor-rs types a string literal as `Constant` and declines, matching.
    ///     A `String.new` / interpolated / method-return String is `Nominal` in
    ///     both (unfolded) and fires.
    /// (a2) **Array slice** `arr[Range]` ⇒ `Array?` — but ONLY when the receiver is
    ///     an `Array.new`-Nominal-provenance array (ADR-0039 §2 syntactic
    ///     provenance): a bare local in `penv`, or a direct `Array.new(nominal)`
    ///     call. NEVER a bare `Nominal[Array]` — the reference types array literals
    ///     and `Array.new(n≤16)` (and `.map`/… results) as `Tuple` whose slice is
    ///     non-nil, so firing off the type env would over-fire on those.
    /// (b) **Certain nilable RBS return** on a KNOWN core receiver
    ///     (`String#byteslice -> String?`). A `Constant` receiver is declined for
    ///     the same folding-parity reason — the keystone.
    fn nilable_source_class(
        &self,
        ast: &LoweredAst,
        value_id: NodeId,
        tenv: &TypeEnv,
        penv: &HashSet<String>,
        interner: &mut Interner,
    ) -> Option<&'static str> {
        let Node::Call { receiver: Some(recv), method, args, block_body, .. } = ast.get(value_id)
        else {
            return None;
        };
        if !block_body.is_empty() {
            return None;
        }
        let recv = *recv;
        let method = method.clone();
        let args = args.clone();
        // rigor-rs#352: a `s[k]` read answers the `s[k] ||= v` slot record when
        // one is in scope (`type_call`'s `[]` interception reads the same
        // `indexed_narrowing_key`), so the record's nilability — not
        // `String#[]`'s RBS `String?` — decides `x`'s fact: `s[0] ||= "x"`
        // records `String | "x"` (non-nil), and `x = s[0]` must not mark.
        if method == "[]" && args.len() == 1 {
            if let Node::LocalVariableRead { name, .. } = ast.get(recv) {
                if let Some(key) = crate::stable_index_key(ast.get(args[0])) {
                    if let Some(&recorded) =
                        tenv.get(&crate::indexed_narrowing_key(name, &key))
                    {
                        return self
                            .non_nil_fragment(recorded, interner)
                            .and_then(|frag| self.fragment_class(frag, interner))
                            .filter(|c| self.index.knows_class(c));
                    }
                }
            }
        }
        // (c) `Regexp.last_match` — a CORE SINGLETON returning an optional (P2,
        // 2026-07-17). `Regexp.last_match() -> MatchData?`; `Regexp.last_match(n)`
        // / `(name) -> String?`. The receiver is a `ConstantRead "Regexp"` (both
        // `Regexp` and `::Regexp` lower to this bare name), whose type is a
        // `Singleton` — `class_name_of` below returns `None` for it, so this MUST
        // be matched syntactically here, before the receiver-class resolution. The
        // syntactic name gate mirrors the reference resolving `Regexp.last_match`
        // against core RBS; a project constant coincidentally named `Regexp` is not
        // a realistic hazard. The arm depends only on the ARITY (spec
        // `docs/notes/20260717-p2-optional-local-nil-spec.md`, widened by the
        // compat plan S2): EVERY 1-arity overload returns `String?` —
        // `(Integer) -> String?`, `(Symbol|String name) -> String?` — so the
        // reference resolves a 1-arg call to `String?` even when the arg is
        // non-literal (fixture 65). Arity, not arg shape, decides:
        //   - zero args         ⇒ `MatchData` (deref `#[]` / `#begin` / …),
        //   - one non-splat arg ⇒ `String`    (deref `#gsub` / `#upcase` / …),
        //   - splat / multi arg ⇒ DECLINE (arity unknown / raises — never guess).
        if method == "last_match" {
            if let Node::ConstantRead { name, .. } = ast.get(recv) {
                if name == "Regexp" {
                    return match args.as_slice() {
                        [] => Some("MatchData"),
                        // A splat lowers to `Statements` (receiver-call args) or
                        // `Other` (`...` forwarding) — arity unknown, decline.
                        [only] if !matches!(
                            ast.get(*only),
                            Node::Other { .. } | Node::Statements { .. }
                        ) =>
                        {
                            Some("String")
                        }
                        _ => None,
                    };
                }
            }
        }
        let rty = self.type_of(ast, recv, tenv, interner);
        // Issue #352: a `T | nil` union receiver — `x` bound from an `s[0]`
        // read — dispatches its call on the non-nil fragment
        // (`try_non_nil_receiver_retry`), so the nilable-source test reads the
        // same fragment: `y = x[0]` marks `y` `String | nil` exactly like
        // `x = u[0]` on a `String.new` receiver does.
        let rty = match interner.get(rty) {
            Type::Union(_) => self.non_nil_fragment(rty, interner).unwrap_or(rty),
            _ => rty,
        };
        // Folding-parity keystone (shared by both sources): a `Constant` receiver
        // is folded by the reference to a concrete non-nil value ⇒ decline.
        if matches!(interner.get(rty), Type::Constant(_)) {
            return None;
        }
        let cls = self.index.class_name_of(interner, rty)?;
        if !self.index.knows_class(cls) {
            return None;
        }
        let is_range_slice =
            method == "[]" && args.len() == 1 && matches!(ast.get(args[0]), Node::Range { .. });
        // (a) String slice — `str[Range]` ⇒ `String?`. String only (see doc).
        if is_range_slice && cls == "String" {
            return Some("String");
        }
        // (a2) Array slice — `arr[Range]` ⇒ `Array?`, provenance-gated (§2).
        if is_range_slice && cls == "Array" {
            let provenanced = match ast.get(recv) {
                Node::LocalVariableRead { name, .. } => penv.contains(name),
                _ => self.array_new_nominal_provenance(ast, recv, tenv, interner),
            };
            return provenanced.then_some("Array");
        }
        // (b) certain nilable RBS return.
        match self.index.method_return_nilable(cls, &method) {
            Some((core, true)) if self.index.knows_class(core) => Some(core),
            _ => None,
        }
    }

    /// The core-class name a nilable-source fragment witnesses — a
    /// `non_nil_fragment` result is a union whenever the recorded slot held
    /// more than one non-nil member (`s[k] ||= v` records
    /// `narrow_truthy(s[k]) | v`, e.g. `String | "x"`), and `class_name_of`
    /// alone answers `None` for it. The marked arm only needs ONE member's
    /// class: `check_nil_receiver` fires when the method is absent on
    /// `NilClass` and present on that arm, and every member is a possible
    /// receiver — picking the first member with a known class keeps the fire
    /// for `String | "x" | nil` (`x.upcase` ⇒ `String`) without inventing a
    /// class a member does not carry (rigor-rs#352).
    fn fragment_class(&self, frag: TypeId, interner: &Interner) -> Option<&'static str> {
        if let Some(c) = self.index.class_name_of(interner, frag) {
            return Some(c);
        }
        if let Type::Union(members) = interner.get(frag) {
            for &m in members {
                if let Some(c) = self.index.class_name_of(interner, m) {
                    if self.index.knows_class(c) {
                        return Some(c);
                    }
                }
            }
        }
        None
    }
}
