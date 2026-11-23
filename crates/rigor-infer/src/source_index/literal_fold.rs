//! ADR-0038 interprocedural literal-tail fold: the Pass-4a tail capture (issue #113), the Pass-4b
//! fold with its overridable degrade (#94), and its lookups.

use rigor_index::CoreIndex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use rigor_parse::{LoweredAst, Node, NodeId, Span};
use rigor_types::Scalar;

use super::{DefKind, FoldDefs, HarvestedFoldDef, SourceIndex, OVERRIDE_ANCESTOR_WALK_LIMIT};

/// Interprocedural literal-tail fold: the syntactic depth cap the tail capture
/// applies ([`capture_fold_tail`]; bodies calling bodies — `read_write? =
/// !read_only?`). Past it the fold declines (a missed witness, never a false
/// positive). Bodies this deep are vanishingly rare; the cap just backstops a
/// pathological chain the per-key cycle guard would otherwise still terminate but
/// slowly. **Issue #113 moved the check** from the (retired) `fold_expr` walk to
/// the capture; it is now applied exactly once, at harvest time.
pub(crate) const FOLD_DEPTH_CAP: usize = 16;

/// **Issue #113 — the captured tail of one project `def`.** Either a mini-tree
/// [`SourceIndex::fold_tail`] can fold, or `Decline`.
///
/// `SourceIndex::fold_expr` — the pre-capture fold, now retired and kept only as
/// the `probes_s92` oracle — inspected exactly 10 of the 37 [`Node`] variants and declined everything else at its `_ => None` arm,
/// and **every one of those declines is decided by SYNTAX alone**: the shape tag,
/// `args.is_empty()`, `block_body.is_empty()`, `name.is_empty()`. None of them
/// consults merged state. So [`capture_fold_tail`] can make the decision at
/// HARVEST time and the prune is exact by construction — not a subset argument
/// (`docs/notes/20260826-ast-eviction-probe.md` §1.1).
///
/// `Decline` is the overwhelmingly common case (an ivar read, a block-bearing
/// call, an `if`/`case` carrier, a local read — all `_ => None`), and it costs
/// exactly the null niche of the `Box`: a declining def carries 8 bytes and no
/// heap node at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FoldTail {
    /// `fold_expr`'s `_ => None`, decided by syntax at harvest time — including
    /// the [`FOLD_DEPTH_CAP`] cut-off (see [`capture_fold_tail`]).
    Decline,
    Expr(Box<FoldExpr>),
}

/// One node of a captured tail — the 10 shapes the pre-capture `fold_expr` read,
/// with the AST indirection already resolved away.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FoldExpr {
    /// The seven scalar literal shapes (`StringLit` / `IntegerLit` / `FloatLit` /
    /// `SymbolLit` / `NilLit` / `TrueLit` / `FalseLit`), already converted.
    Scalar(Scalar),
    /// A block-free implicit-self call (`read_only?`). Args are deliberately NOT
    /// captured: the fold is param-INDEPENDENT and never looked at them.
    SelfCall { method: String },
    /// A block-free, arg-free call on a non-empty `ConstantRead` receiver
    /// (`Gitlab::Database.read_only?`) — an OWN-CLASS singleton project call.
    /// `owner` already has the `::` prefix stripped, exactly as `fold_expr` did.
    ConstCall { owner: String, method: String },
    /// `!expr` — Prism lowers unary not to a receiver-bearing call named `!` with
    /// no args.
    Not { operand: FoldTail },
    /// Any other block-free receiver-bearing call: a core fold on a value-pinned
    /// receiver + args (`1 + 1`, `"x" == "y"`), applied by [`crate::folding::fold`].
    CoreCall { recv: FoldTail, method: String, args: Vec<FoldTail> },
}

/// Issue #94: the per-MERGE memo of transitive project-ancestor closures, keyed
/// by definer-candidate name. One entry answers every later
/// `(candidate, *owner*)` relatedness query of that merge as a set membership
/// test, replacing one BFS per PAIR with one BFS per CANDIDATE (measured reuse:
/// 12.5× at gitlab-foss/lib, 21.5× at mastodon/app —
/// `docs/notes/20260825-s94-pass4b-cost-probe.md` §2).
///
/// It is deliberately a plain local threaded by `&mut` through the Pass-4b call
/// chain, NOT a [`SourceIndex`] field and NOT interior mutability: its soundness
/// rests on `override_classes` being frozen for the whole of
/// [`SourceIndex::compute_literal_returns`], and on nothing outliving that call
/// (the LSP rebuilds the index per dispatch, so a longer-lived cache keyed by
/// class name alone would serve stale answers across keystrokes).
pub(crate) type AncestorClosures = HashMap<String, HashSet<String>>;

impl SourceIndex {
    /// ADR-0038 interprocedural literal-tail fold — the folded scalar literal a
    /// `Const.method` SINGLETON call yields, or `None` to decline (Dynamic,
    /// silent). `receiver_name` is the receiver constant's dotted name as written
    /// (`Gitlab::Database`, `::Gitlab::Database`); resolution is OWN-CLASS only
    /// (the reference `try_singleton_method_inference` walks no singleton
    /// ancestry) and the returned value already has the overridable degrade
    /// applied. The call site interns the result as a `Type::Constant`.
    pub fn const_singleton_literal(&self, receiver_name: &str, method: &str) -> Option<Scalar> {
        let owner = receiver_name.strip_prefix("::").unwrap_or(receiver_name);
        self.literal_returns
            .get(&(owner.to_string(), method.to_string(), DefKind::Singleton))
            .cloned()
    }

    /// ADR-0038 interprocedural literal-tail fold — the folded scalar literal an
    /// IMPLICIT-SELF call `method` yields inside the enclosing scope `self_qual`
    /// (a qualified class/module name) whose method kind is `self_kind`, or `None`
    /// to decline. A singleton enclosing method (`def self.x`) resolves `method`
    /// against `self_qual`'s OWN singleton table; an instance method resolves it
    /// through `self_qual`'s project ancestry (nearest ancestor defining it), the
    /// same ancestor walk the override-visibility rule uses — so an unrelated
    /// same-name method elsewhere is NOT resolved (the cross-class zero-FP
    /// keystone). The value already has the overridable degrade applied.
    pub fn implicit_self_literal(
        &self,
        core: &CoreIndex,
        self_qual: &str,
        self_kind: DefKind,
        method: &str,
    ) -> Option<Scalar> {
        // A patch on the receiver's walk BETWEEN `self_qual` and the
        // resolved `owner` answers ahead of the def (the reference walks
        // from the receiver, not the definer); the fold key's own check
        // covers only `owner`'s walk.
        if self.pre_eval_may_answer(
            core,
            self_qual,
            method,
            self_kind == DefKind::Singleton,
            crate::PatchCall::Implicit,
        ) {
            return None;
        }
        let (owner, kind) = match self_kind {
            DefKind::Singleton => self.resolve_singleton_slot(self_qual, method)?,
            DefKind::Instance => (self.resolve_instance_owner(core, self_qual, method)?, DefKind::Instance),
        };
        self.literal_returns
            .get(&(owner, method.to_string(), kind))
            .cloned()
    }

    /// A bare call folds a TOPLEVEL `def`'s literal tail across files
    /// (rigor-rs#462): `helpers.rb` `def format(x) = 42` types `script.rb`'s
    /// `format(1)` to `42`, which outranks `Kernel#format`'s `String` return —
    /// the `def_nodes["<toplevel>"]` merge is project-wide on the oracle.
    /// `None` when no toplevel def of the name folds a scalar.
    ///
    /// rigor-rs#491 review F2: this fold does NOT consult the `pre_eval:`
    /// registry — the reference folds a toplevel `def f7(x) = 42` at a bare
    /// `f7(1)` even while a patch binds the name on `Kernel` or `Object`
    /// (probed on both engines: `zz9 for 42` fires), so an on-walk patch must
    /// not decline it.
    pub fn toplevel_def_literal(&self, method: &str, arg_count: usize) -> Option<Scalar> {
        // The call must satisfy SOME def site's arity — `format("%d", 2)`
        // against `def format(x)` keeps `Kernel#format` on the reference.
        let arity_ok = self
            .toplevel_def_arities
            .get(method)
            .is_some_and(|ranges| ranges.iter().any(|(lo, hi)| arg_count >= *lo && arg_count <= *hi));
        if !arity_ok {
            return None;
        }
        self.literal_returns
            .get(&(
                TOPLEVEL_FOLD_OWNER.to_string(),
                method.to_string(),
                DefKind::Instance,
            ))
            .cloned()
    }

    /// The `(owner, kind)` an implicit-self call inside a SINGLETON self
    /// (`def self.x`, `class <<`, a class/module body statement) resolves
    /// `method` against — the reference's `singleton_def_through_ancestors`
    /// (`scope.rb`): the class's own singleton table first, then each
    /// `extend`ed module's INSTANCE surface (ScopeIndexer folds `extend M`
    /// into the extender's own singleton), then the SUPERCLASS chain alone —
    /// an `include`d module's `def self.x` is not callable on the includer.
    fn resolve_singleton_slot(&self, qual: &str, method: &str) -> Option<(String, DefKind)> {
        let mut current = qual.to_string();
        let mut seen: HashSet<String> = HashSet::new();
        let mut visited = 0usize;
        loop {
            if !seen.insert(current.clone()) {
                return None;
            }
            visited += 1;
            if visited > OVERRIDE_ANCESTOR_WALK_LIMIT {
                return None;
            }
            if self.owner_defines(&current, method, DefKind::Singleton) {
                return Some((current, DefKind::Singleton));
            }
            for ext in self.extended_names(&current) {
                if self.owner_defines(&ext, method, DefKind::Instance) {
                    return Some((ext, DefKind::Instance));
                }
            }
            current = self.project_superclass(&current)?;
        }
    }

    // -----------------------------------------------------------------------
    // ADR-0038 — interprocedural literal-tail return folding
    // -----------------------------------------------------------------------

    /// Compute the `(qualified owner, method, kind) -> folded scalar` table from
    /// the harvested `defs` (which carry each method's CAPTURED tail — issue
    /// #113, no AST), applying the overridable-method degrade. A per-key memo
    /// makes the recursive body-to-body fold (`read_write? = !read_only?`)
    /// linear; a per-resolution `visiting` set makes a recursive method
    /// (`def loopy; loopy; end`) decline rather than spin. A per-candidate
    /// [`AncestorClosures`] memo (#94) makes the degrade gate's ancestor walk run
    /// once per definer-candidate instead of once per `(candidate, owner)` pair.
    pub(crate) fn compute_literal_returns(
        &self,
        core: &CoreIndex,
        defs: &FoldDefs<'_>,
    ) -> HashMap<(String, String, DefKind), Scalar> {
        let mut memo: HashMap<(String, String, DefKind), Option<Scalar>> = HashMap::new();
        // Born here, dies here: the closure memo is valid exactly as long as
        // `override_classes` is frozen, which is exactly this call (every reader
        // below takes `&self`, and M1's replay finished before M3 started).
        let mut closures: AncestorClosures = AncestorClosures::new();
        for key in defs.keys() {
            let mut visiting: HashSet<(String, String, DefKind)> = HashSet::new();
            self.resolve_fold_key(core, key, defs, &mut memo, &mut visiting, &mut closures);
        }
        memo.into_iter().filter_map(|(k, v)| v.map(|s| (k, s))).collect()
    }

    /// Resolve one `(owner, method, kind)`'s folded literal (memoized), applying
    /// the overridable degrade: a value-pinned base return is dropped when a
    /// RELATED subclass/includer redefines the method (else adopting the base's
    /// literal as a flow constant is unsound — the reference `degrade_if_overridable`).
    fn resolve_fold_key(
        &self,
        core: &CoreIndex,
        key: &(String, String, DefKind),
        defs: &FoldDefs<'_>,
        memo: &mut HashMap<(String, String, DefKind), Option<Scalar>>,
        visiting: &mut HashSet<(String, String, DefKind)>,
        closures: &mut AncestorClosures,
    ) -> Option<Scalar> {
        if let Some(v) = memo.get(key) {
            return v.clone();
        }
        // ADR-17: a `pre_eval:` patch of the name on the DEFINER's walk may
        // be the method that runs (the reference's patched tier answers
        // `Dynamic` ahead of the project's own `def`), so its body is no
        // evidence. Asked of `key.0` through `lookup_through_ancestors` — a
        // patch of the name on an unrelated class leaves the def to fold.
        // `Implicit`: the fold is call-site-agnostic, so a patch of ANY
        // visibility declines (a coverage loss next to the reference's
        // `callable?`, never a wrong literal).
        if self.pre_eval_may_answer(
            core,
            &key.0,
            &key.1,
            key.2 == DefKind::Singleton,
            crate::PatchCall::Implicit,
        ) {
            return None;
        }
        // rigor-rs#450: a project-`sig/` class whose singleton surface
        // answers the name types the call from its signature — the
        // reference's `RbsDispatch` tier sits above the body-inference
        // tiers, so `def self.one(_x) = nil` under `def self.one: (Integer)
        // -> String` answers `String`, not the folded `nil`. The same
        // precedence the instance path already gives a project-sig receiver
        // (tier 4b's `receiver_method_return` ahead of `method_return`).
        // A signature the reference cannot BUILD (a missing superclass,
        // include or self-type: `project_sig_chain_ok`) answers nothing, so
        // the body still types the call — probed: `class U < Missing` with
        // `def self.u: () -> String` over `def self.u = nil` fires `U.u.upcase`
        // `for nil` on the oracle, as the instance arm's fall-through does.
        if key.2 == DefKind::Singleton
            && core.is_qualified_project_sig_class(&key.0)
            && core.project_sig_chain_ok(&key.0)
            && core.class_has_singleton_method(&key.0, &key.1)
        {
            return None;
        }
        if visiting.contains(key) {
            return None; // cycle (recursive method) ⇒ decline, don't memoize.
        }
        visiting.insert(key.clone());
        let raw = self.fold_key_sites(core, key, defs, memo, visiting, closures);
        let result = match raw {
            Some(_) if self.overridden_in_project(&key.0, &key.1, key.2, closures) => None,
            other => other,
        };
        visiting.remove(key);
        memo.insert(key.clone(), result.clone());
        result
    }

    /// Fold every (re)definition site of `key` and require they AGREE on one
    /// scalar (a disagreeing reopen declines). Any site with an explicit `return`
    /// declines the whole method (we read only the tail).
    fn fold_key_sites(
        &self,
        core: &CoreIndex,
        key: &(String, String, DefKind),
        defs: &FoldDefs<'_>,
        memo: &mut HashMap<(String, String, DefKind), Option<Scalar>>,
        visiting: &mut HashSet<(String, String, DefKind)>,
        closures: &mut AncestorClosures,
    ) -> Option<Scalar> {
        let sites = defs.get(key)?;
        let mut acc: Option<Scalar> = None;
        for site in sites {
            if site.has_explicit_return {
                return None;
            }
            let s = self.fold_tail(core, site.tail, &key.0, key.2, defs, memo, visiting, closures)?;
            match &acc {
                None => acc = Some(s),
                Some(prev) if *prev != s => return None, // disagreeing reopen.
                _ => {}
            }
        }
        acc
    }

    /// Fold one CAPTURED tail ([`FoldTail`]) to a scalar literal, or `None` to
    /// decline. Handles literals, `!expr`, an implicit-self project call (resolved
    /// against `self_qual`/`self_kind`), a `Const.method` singleton call, and a
    /// core fold on a value-pinned receiver + args. A `Decline` — a param / ivar /
    /// non-folding call / branch carrier / anything past [`FOLD_DEPTH_CAP`] —
    /// declines the whole fold, which is why an if/case/loop-carrier tail or a
    /// param-dependent body never folds.
    ///
    /// **Issue #113: this is the post-capture half of the old `fold_expr`.** The
    /// syntactic half moved to [`capture_fold_tail`] at harvest time; what is
    /// left is exactly the part that needs merged state — `resolve_instance_owner`
    /// (the override ancestry), `resolve_fold_key` (the cross-file def table) and
    /// `folding::fold`. Hence no `&LoweredAst`, no `&[&LoweredAst]`, and no
    /// `depth` (the cap is applied by the capture — see there).
    #[allow(clippy::too_many_arguments)]
    fn fold_tail(
        &self,
        core: &CoreIndex,
        tail: &FoldTail,
        self_qual: &str,
        self_kind: DefKind,
        defs: &FoldDefs<'_>,
        memo: &mut HashMap<(String, String, DefKind), Option<Scalar>>,
        visiting: &mut HashSet<(String, String, DefKind)>,
        closures: &mut AncestorClosures,
    ) -> Option<Scalar> {
        let FoldTail::Expr(expr) = tail else {
            return None;
        };
        match &**expr {
            FoldExpr::Scalar(s) => Some(s.clone()),
            // An implicit-self project call (`read_only?`). Args were never read —
            // the fold is param-INDEPENDENT; if the body reads a param the
            // recursive fold declines on that param leaf.
            FoldExpr::SelfCall { method } => {
                // A patch on the receiver's walk between `self_qual` and the
                // resolved owner answers the call ahead of the def, exactly
                // as it does at [`Self::implicit_self_literal`].
                if self.pre_eval_may_answer(
                    core,
                    self_qual,
                    method,
                    self_kind == DefKind::Singleton,
                    crate::PatchCall::Implicit,
                ) {
                    return None;
                }
                let (owner, kind) = match self_kind {
                    DefKind::Singleton => self.resolve_singleton_slot(self_qual, method)?,
                    DefKind::Instance => {
                        (self.resolve_instance_owner(core, self_qual, method)?, DefKind::Instance)
                    }
                };
                self.resolve_fold_key(core, &(owner, method.clone(), kind), defs, memo, visiting, closures)
            }
            // `Const.method` — an OWN-CLASS singleton project call.
            FoldExpr::ConstCall { owner, method } => self.resolve_fold_key(
                core,
                &(owner.clone(), method.clone(), DefKind::Singleton),
                defs,
                memo,
                visiting,
                closures,
            ),
            // `!expr`: the oracle folds `!` only through BOOL_UNARY /
            // NIL_UNARY (`constant_folding.rb:174-175`) — `read_write? =
            // !read_only?` still folds because its operand resolves to a
            // Bool scalar, but `def neg; !"x"; end` must DECLINE: the
            // reference types that tail `bool` NOMINAL (universal dispatch),
            // so `if neg` keeps both arms live and pinning `Constant[false]`
            // here mints a `flow.always-truthy-condition` the oracle never
            // emits. `folding::fold` holds exactly that boundary.
            FoldExpr::Not { operand } => {
                let s =
                    self.fold_tail(core, operand, self_qual, self_kind, defs, memo, visiting, closures)?;
                crate::folding::fold(&s, "!", &[])
            }
            // A core fold on a value-pinned receiver + args (`1 + 1`, `"x" ==
            // "y"`). Declines unless every part folds.
            FoldExpr::CoreCall { recv, method, args } => {
                let recv =
                    self.fold_tail(core, recv, self_qual, self_kind, defs, memo, visiting, closures)?;
                let mut arg_scalars = Vec::with_capacity(args.len());
                for a in args {
                    arg_scalars.push(self.fold_tail(
                        core,
                        a, self_qual, self_kind, defs, memo, visiting, closures,
                    )?);
                }
                crate::folding::fold(&recv, method, &arg_scalars)
            }
        }
    }

    /// The nearest project ancestor of `qual` (itself first, then its ancestry in
    /// MRO order) that defines instance `method`, or `None`. Mirrors the reference
    /// `resolve_user_def_with_owner`: an unrelated same-name method elsewhere is
    /// never reached, so an implicit-self call resolves ONLY through the enclosing
    /// class's own project chain (the cross-class zero-FP keystone).
    pub(crate) fn resolve_instance_owner(
        &self,
        core: &CoreIndex,
        qual: &str,
        method: &str,
    ) -> Option<String> {
        let owner = self.resolve_instance_owner_bfs(qual, method)?;
        // Upstream v0.4.1 (#1594, ADR-119 C2-b1): the typing read answers only
        // where `DefinerResolution` is `Known` on the Ruby-order chain, and
        // names the same definer the breadth-first walk found.
        let settled = self.settled_definer(core, qual, method, false, false, |n| {
            self.owner_defines(n, method, DefKind::Instance)
        })?;
        (settled == owner).then_some(owner)
    }

    /// The breadth-first walk the typing read replaced (kept for the
    /// gate's same-definer check and the legacy-fold probe).
    pub(crate) fn resolve_instance_owner_bfs(&self, qual: &str, method: &str) -> Option<String> {
        if self.owner_defines(qual, method, DefKind::Instance) {
            return Some(qual.to_string());
        }
        let mut queue: Vec<String> = self.override_ancestor_names(qual);
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(qual.to_string());
        let mut visited = 0usize;
        while !queue.is_empty() {
            let current = queue.remove(0);
            if !seen.insert(current.clone()) {
                continue;
            }
            visited += 1;
            if visited > OVERRIDE_ANCESTOR_WALK_LIMIT {
                return None;
            }
            if self.owner_defines(&current, method, DefKind::Instance) {
                return Some(current);
            }
            for next in self.override_ancestor_names(&current) {
                queue.push(next);
            }
        }
        None
    }

    /// Whether the qualified `owner` has its OWN project `def` of `(method, kind)`.
    pub(crate) fn owner_defines(&self, owner: &str, method: &str, kind: DefKind) -> bool {
        self.definers
            .get(&(method.to_string(), kind))
            .is_some_and(|owners| owners.iter().any(|o| o == owner))
    }

    /// The overridable-method degrade gate (reference `overridden_in_project?`):
    /// true when some project class/module DISTINCT from `owner` redefines
    /// `(method, kind)` AND is RELATED to `owner` (a transitive subclass of an
    /// owner class, or an includer/prepender of an owner module). A same-name
    /// method in an UNRELATED class is not an override — so the two unrelated
    /// `force_pipeline_creation_to_continue?` definers each still fold.
    pub(crate) fn overridden_in_project(
        &self,
        owner: &str,
        method: &str,
        kind: DefKind,
        closures: &mut AncestorClosures,
    ) -> bool {
        let Some(candidates) = self.definers.get(&(method.to_string(), kind)) else {
            return false;
        };
        candidates
            .iter()
            .any(|c| c != owner && self.ancestor_closure(c, closures).contains(owner))
    }

    /// `candidate`'s transitive project-ancestor closure, from the per-merge
    /// [`AncestorClosures`] memo — built on first use, then reused by every later
    /// `(candidate, *)` query. `closure(candidate).contains(owner)` IS the old
    /// per-pair `related_to_owner(candidate, owner)` (#94); the `#[cfg(test)]`
    /// copy of that walk below is the equivalence oracle that pins it.
    pub(crate) fn ancestor_closure<'a>(
        &self,
        candidate: &str,
        closures: &'a mut AncestorClosures,
    ) -> &'a HashSet<String> {
        // `contains_key` + `insert` rather than the `entry` API on purpose: a HIT
        // (12–22× more frequent than a miss) must not pay for a key allocation,
        // and `entry` would force `candidate.to_string()` on every query.
        if !closures.contains_key(candidate) {
            let closure = self.build_ancestor_closure(candidate);
            closures.insert(candidate.to_string(), closure);
        }
        &closures[candidate]
    }

    /// Build `candidate`'s transitive project-ancestor closure: every class name
    /// the pre-#94 per-pair walk could ever have popped off its queue — same MRO
    /// BFS order, same cycle guard, same visited cap — collected instead of
    /// stopping at the first match. So `closure.contains(owner)` answers exactly
    /// what `related_to_owner(candidate, owner)` answered: `candidate` is a
    /// transitive subclass of an owner class, or an includer/prepender of an
    /// owner module.
    ///
    /// ## Cap-boundary fidelity (the one subtle equivalence)
    ///
    /// In the old loop the `current == owner` test ran on POP — BEFORE the
    /// seen-skip AND BEFORE the `visited > OVERRIDE_ANCESTOR_WALK_LIMIT` return.
    /// So the node that OVERFLOWS the cap is still owner-checkable (it can still
    /// answer `true`), while the nodes left behind in the queue never are. This
    /// builder reproduces that boundary exactly: it RECORDS each node at its
    /// first pop, and when the recorded count passes the cap it records the
    /// overflowing node but does NOT expand it and stops — leaving the rest of
    /// the queue out of the closure, exactly as the old walk left them
    /// unreachable. That is also why BFS ORDER is load-bearing here and must stay
    /// byte-identical to the old walk: under the cap, WHICH nodes make it into
    /// the closure depends on the order they were popped in.
    ///
    /// A DUPLICATE pop needs no recording — its first pop already put it in the
    /// set — with ONE exception: `candidate` itself is pre-seeded into `seen`
    /// (the old walk's cycle guard) and so is never recorded by a first pop, yet
    /// a cycle that walks back to it DID make it owner-checkable in the old loop.
    /// That case is recorded explicitly.
    pub(crate) fn build_ancestor_closure(&self, candidate: &str) -> HashSet<String> {
        // `seen` doubles as the closure being built: a node is inserted exactly
        // when the old walk would have owner-checked it for the first time.
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(candidate.to_string());
        let mut candidate_popped = false;
        let mut queue: VecDeque<String> = self.override_ancestor_names(candidate).into();
        let mut visited = 0usize;
        while let Some(current) = queue.pop_front() {
            if seen.contains(&current) {
                if current == candidate {
                    candidate_popped = true;
                }
                continue;
            }
            visited += 1;
            if visited > OVERRIDE_ANCESTOR_WALK_LIMIT {
                // Cap exceeded: this node was popped, so it stays owner-checkable;
                // it is NOT expanded and the queue behind it is abandoned.
                seen.insert(current);
                break;
            }
            // The expansion is moved into the queue and `current` is moved into
            // `seen` — no per-pop `String` re-allocation (the old walk cloned
            // `current` on every pop).
            queue.extend(self.override_ancestor_names(&current));
            seen.insert(current);
        }
        if !candidate_popped {
            // Pre-seeded as the cycle guard, never actually reached ⇒ not part of
            // its own closure.
            seen.remove(candidate);
        }
        seen
    }

    /// The PRE-#94 per-pair walk, verbatim, kept as the equivalence oracle for
    /// [`SourceIndex::build_ancestor_closure`] (the #92 `build_project_legacy`
    /// pattern). Whether `candidate`'s transitive project ancestry reaches
    /// `owner` — i.e. `candidate` is a subclass of an owner class or an includer
    /// of an owner module. Nothing but `probes_s94` calls it.
    #[cfg(test)]
    pub(crate) fn related_to_owner(&self, candidate: &str, owner: &str) -> bool {
        let mut queue: Vec<String> = self.override_ancestor_names(candidate);
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(candidate.to_string());
        let mut visited = 0usize;
        while !queue.is_empty() {
            let current = queue.remove(0);
            if current == owner {
                return true;
            }
            if !seen.insert(current.clone()) {
                continue;
            }
            visited += 1;
            if visited > OVERRIDE_ANCESTOR_WALK_LIMIT {
                return false;
            }
            for next in self.override_ancestor_names(&current) {
                queue.push(next);
            }
        }
        false
    }
}

/// ADR-0038 — harvest ONE FILE's project instance + singleton `def` bodies by
/// QUALIFIED owner name (the same lexical walk `collect_override_classes` uses,
/// so `module Gitlab; module Database` keys `Gitlab::Database`), appending each
/// site (tail node + explicit-return flag) to `out` in walk order. Only DIRECT
/// `def` children of a class/module body are harvested — a def nested in a
/// conditional / inner method is out of scope, matching the tier-4b / override
/// discovery inclusion rule.
///
/// Issue #113: each site's tail is CAPTURED here into an owned [`FoldTail`], so
/// nothing downstream needs this file's AST (or its slice position) again.
/// The `def_nodes` key a bare toplevel `def` files under — mirrors the
/// reference's `"<toplevel>"` table entry (rigor-rs#462).
const TOPLEVEL_FOLD_OWNER: &str = "<toplevel>";

/// `[min, max]` positional args `param_shape` accepts — `usize::MAX` for
/// `*rest`; each keyword slot may also arrive as one trailing positional
/// hash, so `keywords` and `**kwrest` widen the upper bound.
fn fold_arity(shape: &rigor_parse::ParamShape) -> (usize, usize) {
    let min = shape.required;
    let max = if shape.has_rest || shape.has_kwrest {
        usize::MAX
    } else {
        shape.required + shape.optional + shape.keywords.len()
    };
    (min, max)
}

/// The WRITTEN-name chain of enclosing `class`/`module` declarations as a
/// persistent list (`module A::V` pushes ONE link `"A::V"`; see
/// `walk_fold_defs`'s comment on the reference's `qualified_prefix`). Pushing
/// and cloning are O(1), so a D-deep nest costs O(D) total — the `Vec<String>`
/// chain's `to_vec` per level cost O(D²) clones and ~1.5 GB at 4,000 nested
/// `class` (rigor-rs#424).
#[derive(Clone, Default)]
pub(crate) struct WrittenChain(Option<Rc<WrittenLink>>);

struct WrittenLink {
    name: String,
    parent: WrittenChain,
}

impl WrittenChain {
    fn push(&self, name: String) -> Self {
        WrittenChain(Some(Rc::new(WrittenLink {
            name,
            parent: self.clone(),
        })))
    }

    /// `self[len - segs.len()..] == segs` on the old `Vec<String>` — compare
    /// the LAST `segs.len()` written names (innermost last), false when the
    /// chain is shorter than `segs`.
    fn tail_is(&self, segs: &[&str]) -> bool {
        let mut cur = &self.0;
        for seg in segs.iter().rev() {
            match cur {
                Some(link) if link.name == *seg => cur = &link.parent.0,
                _ => return false,
            }
        }
        true
    }
}

/// `qualify` on an already-joined parent name — `prefix.join("::")` IS the
/// parent qualified name, so passing it down avoids re-splitting and
/// re-cloning the whole segment vector per level (rigor-rs#424).
fn qualify_str(parent_qual: &str, name: &str) -> String {
    if parent_qual.is_empty() {
        name.to_string()
    } else {
        format!("{parent_qual}::{name}")
    }
}

pub(crate) fn walk_fold_defs(
    ast: &LoweredAst,
    node: NodeId,
    parent_qual: &str,
    written: &WrittenChain,
    out: &mut Vec<HarvestedFoldDef>,
) {
    match ast.get(node) {
        Node::Program { body, .. } => {
            for &child in body {
                // A top-level `class << C` is a direct program statement, so
                // its `def`s file on C's SINGLETON under the same key a
                // `def self.x` inside `class C` uses — missing it let a later
                // singleton reopen go unseen by the disagree-decline merge
                // (`class C; def self.off? = false; end; …; class << C; def
                // off? = true; end` folded `false` where the reference's last
                // definition binds `true`, rigor-rs#386 review).
                if let Node::Definition {
                    is_singleton_class: true,
                    singleton_operand,
                    body: sclass_body,
                    ..
                } = ast.get(child)
                {
                    let sowner = match singleton_operand.map(|op| ast.get(op)) {
                        Some(Node::ConstantRead { name, .. }) if !name.is_empty() => {
                            Some(name.strip_prefix("::").unwrap_or(name).to_string())
                        }
                        _ => None,
                    };
                    if let Some(sowner) = sowner {
                        walk_singleton_fold_defs(ast, sclass_body, &sowner, out);
                    }
                } else if let Node::Definition {
                    name: Some(method),
                    receiver_def_name: None,
                    singleton_name: None,
                    body: def_body,
                    has_explicit_return,
                    param_shape,
                    ..
                } = ast.get(child)
                {
                    // A bare TOPLEVEL `def` — the `<toplevel>` slot the
                    // reference's `def_nodes` files these under, so a call in
                    // ANOTHER file can fold its tail (rigor-rs#462: a
                    // cross-file `def format(x) = 42` types `format(1)` to
                    // `42`, shadowing `Kernel#format` — both engines silent
                    // at top level and inside a `def`).
                    if let Some(&tail) = def_body.last() {
                        out.push(HarvestedFoldDef {
                            owner: TOPLEVEL_FOLD_OWNER.to_string(),
                            method: method.clone(),
                            kind: DefKind::Instance,
                            tail: capture_fold_tail(ast, tail, 0),
                            has_explicit_return: *has_explicit_return,
                            arity: fold_arity(param_shape),
                        });
                    }
                } else {
                    walk_fold_defs(ast, child, parent_qual, written, out);
                }
            }
        }
        Node::Statements { body, .. } => {
            for &child in body {
                walk_fold_defs(ast, child, parent_qual, written, out);
            }
        }
        Node::ClassDef { name, body, rooted, .. } | Node::ModuleDef { name, body, rooted, .. } => {
            if name.is_empty() {
                return;
            }
            let qualified = qualify_str(parent_qual, name);
            // The reference's `qualified_prefix` holds each declaration's
            // WRITTEN name as one element (`module A::V` pushes `"A::V"`,
            // where `module A; module V` pushes two) — `def_receiver_targets_
            // lexical_self?` reads it, so a `def V.x` directly in the body
            // files the singleton only under the former's `"V"`. A `::`-rooted
            // header resets the chain (`declaration_prefix`).
            let child_written = if *rooted {
                WrittenChain::default()
            } else {
                written.clone()
            };
            let child_written = child_written.push(name.clone());
            for &child in body {
                match ast.get(child) {
                    // `class << <operand>` — its `def`s file on the
                    // operand's SINGLETON (`singleton_context_for`):
                    // `self` keeps the enclosing owner, a constant names
                    // itself, anything else names nothing.
                    Node::Definition {
                        is_singleton_class: true,
                        singleton_operand,
                        body: sclass_body,
                        ..
                    } => {
                        let sowner = match singleton_operand.map(|op| ast.get(op)) {
                            Some(Node::SelfExpr { .. }) => Some(qualified.clone()),
                            Some(Node::ConstantRead { name, .. }) if !name.is_empty() => {
                                Some(name.strip_prefix("::").unwrap_or(name).to_string())
                            }
                            _ => None,
                        };
                        if let Some(sowner) = sowner {
                            walk_singleton_fold_defs(ast, sclass_body, &sowner, out);
                        }
                    }
                    Node::Definition {
                        name,
                        singleton_name,
                        receiver_def_name,
                        def_receiver_path,
                        body: def_body,
                        has_explicit_return,
                        param_shape,
                        ..
                    } => {
                        let entry = match (name, singleton_name) {
                            (Some(m), _) => Some((m.clone(), DefKind::Instance)),
                            (None, Some(m)) => Some((m.clone(), DefKind::Singleton)),
                            // `def <path>.x` whose receiver names this class
                            // by the WRITTEN-name chain
                            // (`def_receiver_targets_lexical_self?` — `V`
                            // inside `module A; module V`, not inside
                            // `module A::V`): the reference files it on this
                            // class's singleton, like `def self.x` (#490,
                            // review finding 1).
                            (None, None) => match (receiver_def_name, def_receiver_path) {
                                (Some(m), Some(p))
                                    if receiver_path_targets_written(p, &child_written) =>
                                {
                                    Some((m.clone(), DefKind::Singleton))
                                }
                                _ => None,
                            },
                        };
                        if let Some((method, kind)) = entry {
                            if let Some(&tail) = def_body.last() {
                                out.push(HarvestedFoldDef {
                                    owner: qualified.clone(),
                                    method,
                                    kind,
                                    // Depth 0: `fold_key_sites` always entered
                                    // `fold_expr` at 0, so the cap resets per SITE
                                    // exactly as it did per key.
                                    tail: capture_fold_tail(ast, tail, 0),
                                    has_explicit_return: *has_explicit_return,
                                    arity: fold_arity(param_shape),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
            for &child in body {
                walk_fold_defs(ast, child, &qualified, &child_written, out);
            }
        }
        _ => {}
    }
}

/// Upstream #1936 / v0.4.5 #2045 (`scope_indexer.rb`
/// `supersede_singleton_def_nodes`): a non-`def` definer written on the
/// class's own singleton class replaces the singleton method an earlier
/// `def self.x` installed, so the reference contests the slot and the typing
/// read declines. The definers: `singleton_class.alias_method` /
/// `define_method` / `remove_method` / `undef_method` / `attr_*` (`attr_writer`
/// only `x=`) and its `class_eval` family and `send` (any name),
/// `define_singleton_method`, and in a `class << self` body the receiverless
/// calls of the same names and `alias`. Certain or possible alike — under an
/// `if`, in a block — and a call whose names are not all literal contests
/// every name. A `def` written after the definer wins again, read across the
/// file's bodies of the class (a same-file reopening counts). The superseded
/// singleton sites in `out` (this file's harvest) capture a `Decline` tail.
///
/// Issue #476 — the reference's singleton slot is last-writer-wins
/// (`SingletonDefTable#write`), and more writers fill it than the harvest
/// captures as sites: a `module_function` copy (the bare toggle's later
/// `def x`, or `module_function :x`), a `def` in an `instance_eval` /
/// `instance_exec` block, a `def self.x` in a `class_eval`-family block, a
/// `def C.x`, and a `def self.x` the reference writes as POSSIBLE (under an
/// `if`, in a block), which leaves the slot contested. The port folds none
/// of these, so each counts as a superseding event here and the earlier site
/// declines: where the reference folds the later writer's body instead (a
/// certain `module_function` copy) the port is silent, a coverage loss and
/// never a false positive. Only a site the harvest itself captured (a direct
/// body `def self.x`, a `class << self` body `def`) wins the slot back. The
/// explicit-receiver forms (`V.instance_eval { def x }`,
/// `V.singleton_class.alias_method`, `def V.x`) may sit anywhere in the
/// file, and match an owner by the receiver path's last segment, a superset
/// of the lexical resolutions.
pub(crate) fn decline_superseded_singletons(ast: &LoweredAst, out: &mut [HarvestedFoldDef]) {
    let mut owners: Vec<(NodeId, String, WrittenChain)> = Vec::new();
    qualified_bodies(ast, ast.root(), "", &WrittenChain::default(), &mut owners);
    let idx = supersede_index(ast, &owners);
    let mut events: HashMap<String, Vec<SingletonEvent>> = HashMap::new();
    for (id, owner, written) in &owners {
        events
            .entry(owner.clone())
            .or_default()
            .extend(singleton_events(ast, *id, owner, written, &idx));
    }
    let foreign = foreign_singleton_events(ast);
    for (owner, evs) in &mut events {
        let tail = owner.rsplit("::").next().unwrap_or(owner);
        evs.extend(
            foreign
                .iter()
                .filter(|(path, _)| path.rsplit("::").next() == Some(tail))
                .map(|(_, e)| e.clone()),
        );
        evs.sort_by_key(|(off, _)| *off);
        for d in out.iter_mut() {
            if d.owner == *owner && d.kind == DefKind::Singleton && superseded(evs, &d.method) {
                d.tail = FoldTail::Decline;
            }
        }
    }
}

/// `(offset, Ok(def name) | Err(superseded names, None = every name))`.
type SingletonEvent = (usize, Result<String, Option<Vec<String>>>);

/// Whether a `def <path>.x` receiver names `owner` — used for contexts
/// whose owner prefix is a RESOLVED segment chain (an eval receiver's
/// `eval_receiver_prefix`, a `class << Const` owner): `path` is a TAIL of
/// `owner`'s qualified segments (`V` or `A::V` inside `module A::V`; never
/// a longer path than the prefix, and never under an empty one).
fn receiver_path_is_owner(path: &str, owner: &str) -> bool {
    owner == path || owner.strip_suffix(path).is_some_and(|h| h.ends_with("::"))
}

/// `def_receiver_targets_lexical_self?` — the same check over the WRITTEN
/// declaration-name chain, where each `class`/`module` keyword contributes
/// its header text as ONE element: `def V.x` inside `module A; module V`
/// names `V`, inside `module A::V` it names nothing (the innermost written
/// name is `"A::V"`, #490 review). A receiver of more segments than the
/// chain is long can only defensively match `A::V`-style spellings Ruby
/// cannot actually parse.
fn receiver_path_targets_written(path: &str, written: &WrittenChain) -> bool {
    let path = path.strip_prefix("::").unwrap_or(path);
    let segs: Vec<&str> = path.split("::").collect();
    written.tail_is(&segs)
}

fn superseded(events: &[SingletonEvent], name: &str) -> bool {
    let last_def = events
        .iter()
        .filter(|(_, e)| matches!(e, Ok(m) if m == name))
        .map(|(off, _)| *off)
        .max();
    events.iter().any(|(off, e)| match e {
        Err(None) => last_def.is_none_or(|d| *off > d),
        Err(Some(ns)) => ns.iter().any(|m| m == name) && last_def.is_none_or(|d| *off > d),
        Ok(_) => false,
    })
}

/// Every named class/module body with its qualified name, as
/// [`walk_fold_defs`] qualifies it, and its WRITTEN-name chain (one element
/// per declaration keyword — see `receiver_path_targets_written`).
fn qualified_bodies(
    ast: &LoweredAst,
    node: NodeId,
    parent_qual: &str,
    written: &WrittenChain,
    out: &mut Vec<(NodeId, String, WrittenChain)>,
) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                qualified_bodies(ast, child, parent_qual, written, out);
            }
        }
        Node::ClassDef { name, body, rooted, .. } | Node::ModuleDef { name, body, rooted, .. } => {
            if name.is_empty() {
                return;
            }
            let qualified = qualify_str(parent_qual, name);
            let child_written = if *rooted {
                WrittenChain::default()
            } else {
                written.clone()
            };
            let child_written = child_written.push(name.clone());
            out.push((node, qualified.clone(), child_written.clone()));
            for &child in body {
                qualified_bodies(ast, child, &qualified, &child_written, out);
            }
        }
        _ => {}
    }
}

/// One node kind the old `nested` scope list carried: every `Definition`,
/// `ClassDef` and `ModuleDef` inside an owner's span, flagged by whether it
/// is a `class << self` (`own` in the old tuple).
#[derive(Clone, Copy)]
enum ScopeKind {
    /// `ClassDef` / `ModuleDef`, named or not — an unnamed one is never an
    /// owner but still blocks attribution to outer owners, as `nested` did.
    ClassModule,
    /// A plain `def` (non-singleton-class `Definition`).
    MethodDef,
    /// `class << self` (`nested`'s `own: true`).
    SclassSelf,
    /// `class << <non-self>` (`nested`'s `own: false`).
    SclassOther,
}

/// One `*_eval` / `*_exec` call with a literal block — the union of the old
/// `self_eval_blocks` / `foreign_eval_blocks` domains. Whether a receiver is
/// "self" is owner-dependent, so the receiver is kept unclassified.
struct EvalSite {
    /// The whole CALL's span — the `within(n.span(), class_span)` side of the
    /// old scan (an eval is a candidate only for owners containing the call).
    call: Span,
    /// The literal block's span — the `within(span, *b)` side.
    block: Span,
    /// `instance_eval` / `instance_exec` vs the `class_eval` family.
    instance: bool,
    recv: EvalRecv,
}

enum EvalRecv {
    /// No receiver or a `self` receiver — self for every owner.
    Bare,
    /// A `ConstantRead` receiver (raw `name`, `::`-prefix stripped at check)
    /// — self iff the path tail-matches the owner.
    Const(String),
    /// Any other receiver — foreign to every owner.
    Other,
}

/// rigor-rs#424 — the per-file indexes `singleton_events` needs, computed
/// ONCE instead of the per-owner `ast.iter()` scans (three per owner plus an
/// O(scopes) `nested` search per node — cubic on a deep `class` nest).
///
/// The owner a node reports to is decided by its innermost enclosing scope
/// alone: for an owner `O`, `nested` held every scope inside `O`, so the
/// innermost one containing the node is simply the node's own innermost
/// scope `s1` (every scope inside `O` and containing the node is inside
/// `s1`'s chain... `s1` is by definition the innermost ANYWHERE). A node
/// therefore reports to `s1` when `s1` is a class/module, to EVERY enclosing
/// class/module when `s1` is a `class << self` (`nested`'s `own` arm — the
/// transparency the old code had, where an outer owner's innermost was also
/// `s1`), and to no one otherwise.
struct SupersedeIndex {
    /// Per arena node (indexed by `NodeId`), `scopes` index +1 of its
    /// innermost enclosing scope — 0 for top-level nodes.
    inner_scope: Vec<u32>,
    /// Scope `(id, span, kind)` in arena order (the `nested` list's domain).
    scopes: Vec<(NodeId, Span, ScopeKind)>,
    /// Per scope, its parent scope's `scopes` index +1.
    scope_parent: Vec<u32>,
    /// Per arena node, `evals` index +1 of the innermost eval BLOCK
    /// containing it.
    inner_eval: Vec<u32>,
    evals: Vec<EvalSite>,
    /// Per eval, the `evals` index +1 of the eval block containing ITS
    /// block — walking `inner_eval` then `eval_parent` enumerates a node's
    /// enclosing eval blocks innermost-first.
    eval_parent: Vec<u32>,
    /// Owner `NodeId` → its reporting nodes, in arena order.
    attributed: HashMap<NodeId, Vec<NodeId>>,
}

/// One merged containment sweep: `containers` (scope spans, eval BLOCK
/// spans) and `queries` (every node's span) sorted by (start, end desc); a
/// container still open when an item passes is its innermost enclosing one —
/// the same `within` semantics the per-node `min_by_key` searches applied,
/// at O(sort) instead of O(containers × queries). Returns each query's
/// innermost container +1 and each container's parent container +1.
/// Shared with `method_returns::superseded_defs` (rigor-rs#424).
pub(crate) fn containment_links(containers: &[Span], queries: &[Span]) -> (Vec<u32>, Vec<u32>) {
    use std::cmp::Reverse;
    let mut items: Vec<(usize, Reverse<usize>, bool, u32)> =
        Vec::with_capacity(containers.len() + queries.len());
    for (i, s) in containers.iter().enumerate() {
        items.push((s.0, Reverse(s.1), false, i as u32));
    }
    for (i, s) in queries.iter().enumerate() {
        items.push((s.0, Reverse(s.1), true, i as u32));
    }
    items.sort_unstable();
    let mut inner = vec![0u32; queries.len()];
    let mut parent = vec![0u32; containers.len()];
    let mut stack: Vec<u32> = Vec::new();
    for (_, _, is_query, i) in items {
        let end = if is_query {
            queries[i as usize].1
        } else {
            containers[i as usize].1
        };
        while stack.last().is_some_and(|&t| containers[t as usize].1 < end) {
            stack.pop();
        }
        if is_query {
            inner[i as usize] = stack.last().map_or(0, |&t| t + 1);
        } else {
            parent[i as usize] = stack.last().map_or(0, |&t| t + 1);
            stack.push(i);
        }
    }
    (inner, parent)
}

fn supersede_index(
    ast: &LoweredAst,
    owners: &[(NodeId, String, WrittenChain)],
) -> SupersedeIndex {
    let owner_pos: HashMap<NodeId, usize> = owners
        .iter()
        .enumerate()
        .map(|(i, (id, _, _))| (*id, i))
        .collect();
    let mut scopes: Vec<(NodeId, Span, ScopeKind)> = Vec::new();
    let mut scope_spans: Vec<Span> = Vec::new();
    let mut evals: Vec<EvalSite> = Vec::new();
    let mut eval_spans: Vec<Span> = Vec::new();
    // Queries are EVERY node's span, indexed by `NodeId` (`iter()` walks the
    // arena in id order).
    let mut queries: Vec<Span> = Vec::new();
    for (id, n) in ast.iter() {
        queries.push(n.span());
        match n {
            Node::ClassDef { .. } | Node::ModuleDef { .. } => {
                scopes.push((id, n.span(), ScopeKind::ClassModule));
                scope_spans.push(n.span());
            }
            Node::Definition {
                is_singleton_class,
                singleton_operand,
                ..
            } => {
                let kind = if *is_singleton_class {
                    if singleton_operand
                        .is_some_and(|op| matches!(ast.get(op), Node::SelfExpr { .. }))
                    {
                        ScopeKind::SclassSelf
                    } else {
                        ScopeKind::SclassOther
                    }
                } else {
                    ScopeKind::MethodDef
                };
                scopes.push((id, n.span(), kind));
                scope_spans.push(n.span());
            }
            _ => {}
        }
        if let Node::Call {
            receiver,
            method,
            block_span: Some(block),
            ..
        } = n
        {
            let instance = INSTANCE_EVALS.contains(&method.as_str());
            if instance || CLASS_EVALS.contains(&method.as_str()) {
                let recv = match receiver.map(|r| ast.get(r)) {
                    None | Some(Node::SelfExpr { .. }) => EvalRecv::Bare,
                    Some(Node::ConstantRead { name, .. }) => EvalRecv::Const(name.to_string()),
                    Some(_) => EvalRecv::Other,
                };
                evals.push(EvalSite {
                    call: n.span(),
                    block: *block,
                    instance,
                    recv,
                });
                eval_spans.push(*block);
            }
        }
    }
    let (inner_scope, scope_parent) = containment_links(&scope_spans, &queries);
    let (inner_eval, eval_parent) = containment_links(&eval_spans, &queries);
    // Attribute each node to the owners it reports to (arena order, so each
    // owner's list comes out id-sorted exactly like the old `ast.iter()`
    // loop). `class << self` interiors fan out to EVERY enclosing
    // class/module — the transparency the old `nested` searches had.
    let mut attributed: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for (id, _) in ast.iter() {
        // A scope node's own span contains its query, so its innermost
        // container is ITSELF — step out to the innermost OTHER scope first,
        // exactly the old searches' `*sid != id` exclusion (without this a
        // direct-body `def` would see itself as its scope and file nowhere).
        let si = match inner_scope[id.0 as usize] {
            si if si > 0 && scopes[(si - 1) as usize].0 == id => {
                scope_parent[(si - 1) as usize]
            }
            si => si,
        };
        if si == 0 {
            continue;
        }
        let (sid, _, kind) = scopes[(si - 1) as usize];
        match kind {
            ScopeKind::ClassModule => {
                if owner_pos.contains_key(&sid) {
                    attributed.entry(sid).or_default().push(id);
                }
            }
            ScopeKind::SclassSelf => {
                let mut a = scope_parent[(si - 1) as usize];
                while a > 0 {
                    let (aid, _, akind) = scopes[(a - 1) as usize];
                    if matches!(akind, ScopeKind::ClassModule) && owner_pos.contains_key(&aid) {
                        attributed.entry(aid).or_default().push(id);
                    }
                    a = scope_parent[(a - 1) as usize];
                }
            }
            _ => {}
        }
    }
    SupersedeIndex {
        inner_scope,
        scopes,
        scope_parent,
        inner_eval,
        evals,
        eval_parent,
        attributed,
    }
}

/// The `*_eval` / `*_exec` calls whose block body runs with `self` the
/// receiver: an `instance_*` block's every `def` binds on the receiver's
/// singleton, a `class_eval`-family block's `def self.x` does.
const INSTANCE_EVALS: [&str; 2] = ["instance_eval", "instance_exec"];
const CLASS_EVALS: [&str; 4] = ["class_eval", "class_exec", "module_eval", "module_exec"];

fn literal_name(ast: &LoweredAst, id: NodeId) -> Option<String> {
    match ast.get(id) {
        Node::SymbolLit { value, .. } | Node::StringLit { value, .. } => Some(value.clone()),
        _ => None,
    }
}

/// `None` = every name.
fn all_literal_names(ast: &LoweredAst, args: &[NodeId]) -> Option<Vec<String>> {
    let names: Vec<String> = args.iter().filter_map(|&a| literal_name(ast, a)).collect();
    (!names.is_empty() && names.len() == args.len()).then_some(names)
}

/// The names a naming call on a singleton class replaces
/// (`singleton_naming_call_names`): `Some(None)` for every name, `None` for a
/// call that names nothing.
fn singleton_naming(ast: &LoweredAst, method: &str, args: &[NodeId]) -> Option<Option<Vec<String>>> {
    Some(match method {
        "class_eval" | "class_exec" | "module_eval" | "module_exec" | "instance_eval"
        | "instance_exec" | "send" | "public_send" | "__send__" => None,
        "alias_method" | "define_method" => all_literal_names(ast, &args[..args.len().min(1)]),
        "remove_method" | "undef_method" => all_literal_names(ast, args),
        "attr_reader" => all_literal_names(ast, args),
        "attr_writer" => {
            all_literal_names(ast, args).map(|ns| ns.iter().map(|n| format!("{n}=")).collect())
        }
        "attr_accessor" => all_literal_names(ast, args)
            .map(|ns| ns.iter().flat_map(|n| [n.clone(), format!("{n}=")]).collect()),
        _ => return None,
    })
}

/// The `def` ids of `class_id`'s body the harvest captures as singleton
/// sites ([`walk_fold_defs`] / [`walk_singleton_fold_defs`]): a direct
/// `def self.x`, and EVERY `def` of a direct `class << self` body — inside
/// `class <<` the reference's `in_singleton_class` files a receiver def on
/// the owner whatever the receiver names (`def W.x`, `def obj.x` — #490
/// review). These are the reference's certain slot writes; every other
/// singleton write is possible or not folded.
fn certain_singleton_defs(
    ast: &LoweredAst,
    body: &[NodeId],
    in_sc: bool,
    written: &WrittenChain,
    out: &mut HashSet<NodeId>,
) {
    for &child in body {
        match ast.get(child) {
            Node::Definition { is_singleton_class: true, singleton_operand, body: inner, .. } => {
                if !in_sc && singleton_operand.is_some_and(|op| matches!(ast.get(op), Node::SelfExpr { .. })) {
                    certain_singleton_defs(ast, inner, true, written, out);
                }
            }
            Node::Definition { name: Some(_), .. } if in_sc => {
                out.insert(child);
            }
            // Issue #490 — `def self.x` inside a `class <<` body is a certain
            // write of the OWNER's slot on the reference
            // (`def_singleton?` reads only the SelfNode receiver).
            Node::Definition { singleton_name: Some(_), .. } => {
                out.insert(child);
            }
            // `def <recv>.x`: inside `class <<` it is certain whatever the
            // receiver; in a direct body it must name the class by the
            // written-name chain (`module A::V` does not take `def V.x`).
            Node::Definition { receiver_def_name: Some(_), def_receiver_path, .. }
                if in_sc
                    || def_receiver_path
                        .as_ref()
                        .is_some_and(|p| receiver_path_targets_written(p, written)) =>
            {
                out.insert(child);
            }
            _ => {}
        }
    }
}

fn singleton_events(
    ast: &LoweredAst,
    class_id: NodeId,
    owner: &str,
    written: &WrittenChain,
    idx: &SupersedeIndex,
) -> Vec<SingletonEvent> {
    let within = |a: Span, b: Span| b.0 <= a.0 && a.1 <= b.1;
    let class_node = ast.get(class_id);
    let class_span = class_node.span();
    let mut certain: HashSet<NodeId> = HashSet::new();
    if let Node::ClassDef { body, .. } | Node::ModuleDef { body, .. } = class_node {
        certain_singleton_defs(ast, body, false, written, &mut certain);
    }
    let self_or_none =
        |r: &Option<NodeId>| r.is_none_or(|r| matches!(ast.get(r), Node::SelfExpr { .. }));
    // Whether one of the index's eval sites keeps THIS class's self
    // (`eval_named_receiver?`): a bare or `self` receiver, or a constant
    // receiver tail-matching this class. The index kept the receiver
    // unclassified because the answer is owner-dependent; any other
    // receiver moves the block's self to the receiver (`W.instance_eval
    // { def x }` inside `module V` binds `W.x`, and an unnameable
    // receiver's interior names nothing — both are
    // [`foreign_singleton_events`]' domain, not this class's).
    let eval_is_self = |e: &EvalSite| match &e.recv {
        EvalRecv::Bare => true,
        EvalRecv::Const(name) => {
            receiver_path_is_owner(name.strip_prefix("::").unwrap_or(name), owner)
        }
        EvalRecv::Other => false,
    };
    let mut events: Vec<SingletonEvent> = Vec::new();
    // Bare `module_function` toggles of this body, and the receiverless
    // instance `def`s of this body (each a copy candidate).
    let mut mf_toggles: Vec<usize> = Vec::new();
    let mut instance_defs: Vec<(usize, String)> = Vec::new();
    // The old scan's domain in index terms: every node the body holds whose
    // innermost OTHER scope is this class itself (the direct body, `in_sc`
    // false) or a `class << self` somewhere inside it (`in_sc` true), and
    // which no foreign eval block encloses. `supersede_index` attributed
    // exactly those nodes per owner, in arena order.
    for &id in idx.attributed.get(&class_id).into_iter().flatten() {
        let n = ast.get(id);
        let span = n.span();
        // The innermost scope holding `n`, not `n` itself — a scope node's
        // own span contains its query, so step out to the parent first (the
        // old search's `*sid != id`). Attribution guarantees the result is
        // this class or a `class << self` inside it; anything else files
        // under another owner.
        let mut si = idx.inner_scope[id.0 as usize];
        if si > 0 && idx.scopes[(si - 1) as usize].0 == id {
            si = idx.scope_parent[(si - 1) as usize];
        }
        let (in_sc, sclass_span) = match si.checked_sub(1).map(|i| idx.scopes[i as usize]) {
            Some((sid, _, ScopeKind::ClassModule)) if sid == class_id => (false, None),
            Some((_, ss, ScopeKind::SclassSelf)) => (true, Some(ss)),
            _ => continue,
        };
        // The node's enclosing eval blocks, innermost first. An eval counts
        // for this owner only when its CALL sits inside the body (the old
        // collection scan's `within(n.span(), class_span)` — an eval
        // ENCLOSING the class is another owner's business, and its blocks
        // hold no node the body could report).
        let mut foreign = false;
        let mut self_ei = 0u32;
        let mut ei = idx.inner_eval[id.0 as usize];
        while ei > 0 {
            let e = &idx.evals[(ei - 1) as usize];
            if within(e.call, class_span) {
                if eval_is_self(e) {
                    if self_ei == 0 {
                        self_ei = ei;
                    }
                } else {
                    // An eval-block interior whose block runs under another
                    // self files its writes on THAT owner, not this class.
                    foreign = true;
                    break;
                }
            }
            ei = idx.eval_parent[(ei - 1) as usize];
        }
        if foreign {
            continue;
        }
        // The innermost self-eval block holding `n` — eval blocks nest
        // between `Definition` scopes, so which context governs a def or a
        // call depends on the INNERMOST of the two: a `class <<` inside a
        // self-eval is `in_singleton_class` territory (it outranks the
        // eval's `call_singleton`), while an eval nested INSIDE a
        // `class <<` rebinds the defs its body reaches. `self_ei` is that
        // block's index from the chain walk above (0 = none).
        let sclass_innermost = match sclass_span {
            Some(ss) => self_ei == 0 || within(ss, idx.evals[(self_ei - 1) as usize].block),
            None => false,
        };
        // A certain site wins the slot back; any other write of the name
        // contests it.
        let write = |m: &String| {
            if certain.contains(&id) {
                (span.0, Ok(m.clone()))
            } else {
                (span.0, Err(Some(vec![m.clone()])))
            }
        };
        // Whether `n`'s innermost scope is a `class << self` whose body holds
        // an `instance_*` eval `n` sits in — its `def`s bind on the
        // singleton's own singleton (`defs_singleton == :unnameable`), which
        // nothing names, so the reference skips the write entirely. An eval
        // OUTSIDE the `class <<` (`instance_eval { class << self; def x }`)
        // does not match: the def still contests this class's slot. Eval
        // blocks nest, so the chain walk can stop at the first block past
        // the `class <<`.
        let in_singleton_self_eval = sclass_span.is_some_and(|ss| {
            let mut ei = idx.inner_eval[id.0 as usize];
            while ei > 0 {
                let e = &idx.evals[(ei - 1) as usize];
                if !within(e.block, ss) {
                    break;
                }
                if e.instance && eval_is_self(e) {
                    return true;
                }
                ei = idx.eval_parent[(ei - 1) as usize];
            }
            false
        });
        match n {
            Node::Definition { singleton_name: Some(m), .. } if !(in_sc && in_singleton_self_eval) => {
                events.push(write(m))
            }
            // `def <recv>.x` — the eval context outranks the `class <<`
            // one: inside an `instance_*` self-eval the def binds the eval
            // owner's singleton unconditionally (`defs_singleton`), except
            // under a `class <<` interior where it binds the singleton's
            // own singleton — `:unnameable`, skipped. Inside a `class_eval`
            // self-eval it binds only when the receiver names the eval's
            // owner. Inside `class <<` outside any eval ANY receiver files
            // the owner's slot (`in_singleton_class` is unconditional —
            // `def W.x`, `def obj.x`). In a direct body the receiver must
            // name the class by the WRITTEN-name chain
            // (`def_receiver_targets_lexical_self?`). A def that names
            // another class's singleton stays
            // [`foreign_singleton_events`]' domain.
            Node::Definition { receiver_def_name: Some(m), def_receiver_path, .. } => {
                if sclass_innermost {
                    // `class <<` governs — inside it ANY receiver files
                    // the owner's slot (`in_singleton_class` is
                    // unconditional — `def W.x`, `def obj.x`).
                    events.push(write(m));
                } else if self_ei > 0 {
                    let self_eval = &idx.evals[(self_ei - 1) as usize];
                    if self_eval.instance {
                        // `instance_*` eval — `defs_singleton` binds every
                        // def, except under a `class <<` interior where
                        // it binds the singleton's own singleton
                        // (`:unnameable`, skipped).
                        if !in_singleton_self_eval {
                            events.push((span.0, Err(Some(vec![m.clone()]))));
                        }
                    } else if def_receiver_path
                        .as_ref()
                        .is_some_and(|p| receiver_path_is_owner(p, owner))
                    {
                        events.push((span.0, Err(Some(vec![m.clone()]))));
                    }
                } else if def_receiver_path
                    .as_ref()
                    .is_some_and(|p| receiver_path_targets_written(p, written))
                {
                    // A direct-body `def <recv>.x` must name the class by
                    // the WRITTEN-name chain
                    // (`def_receiver_targets_lexical_self?`). A def that
                    // names another class's singleton stays
                    // [`foreign_singleton_events`]' domain.
                    events.push(write(m));
                }
            }
            Node::Definition { name: Some(m), .. } => {
                // A bare `def x` binds the singleton under an `instance_*`
                // self-eval (unless it is the `:unnameable` shape) and under
                // `class <<`; under `class_eval` it is an ordinary instance
                // def of the eval's owner — no singleton write at all.
                if sclass_innermost {
                    events.push(write(m));
                } else if self_ei > 0 && idx.evals[(self_ei - 1) as usize].instance {
                    if !in_singleton_self_eval {
                        events.push((span.0, Err(Some(vec![m.clone()]))));
                    }
                } else if self_ei == 0 {
                    instance_defs.push((span.0, m.clone()));
                }
                // `class_eval` interior — instance-side, no event.
            }
            Node::Alias { new_name, .. } if in_sc => {
                events.push((span.0, Err(literal_name(ast, *new_name).map(|m| vec![m]))));
            }
            Node::Call { receiver: None, method, args, .. } if method == "module_function" && !in_sc => {
                if args.is_empty() {
                    mf_toggles.push(span.0);
                } else {
                    // `module_function :x` copies `x` at the call; a
                    // `module_function def x` argument copies nothing
                    // (`each_singleton_copy`).
                    let names: Vec<String> = args.iter().filter_map(|&a| literal_name(ast, a)).collect();
                    if !names.is_empty() {
                        events.push((span.0, Err(Some(names))));
                    }
                }
            }
            Node::Call { receiver, method, args, .. } => {
                let on_singleton_class = receiver.is_some_and(|r| {
                    matches!(ast.get(r), Node::Call { method, receiver: rr, .. }
                        if method == "singleton_class" && self_or_none(rr))
                });
                // `singleton_call_target` — `singleton_class.X` and a bare
                // `define_singleton_method` need `!in_singleton_class`
                // (inside `class <<` they are instance-of-singleton-side),
                // while inside `class <<` every bare/`self.` naming call
                // supersedes. `sclass_innermost` applies the same nesting
                // rule as the def arms.
                let names = if on_singleton_class && !sclass_innermost {
                    singleton_naming(ast, method, args)
                } else if self_or_none(receiver) && method == "define_singleton_method" && !sclass_innermost {
                    Some(all_literal_names(ast, &args[..args.len().min(1)]))
                } else if self_or_none(receiver) && sclass_innermost {
                    singleton_naming(ast, method, args)
                } else {
                    None
                };
                if let Some(names) = names {
                    events.push((span.0, Err(names)));
                }
            }
            _ => {}
        }
    }
    // A bare `module_function` copies every later receiverless `def` onto the
    // singleton, up to a bare `public` / `private` / `protected` that ends
    // the mode (`each_singleton_sibling`, #1968). The toggle is read wherever
    // it sits and a reset only as a direct body statement, where it surely
    // runs: an over-approximation, so only more declines.
    let resets: Vec<usize> = match class_node {
        Node::ClassDef { body, .. } | Node::ModuleDef { body, .. } => body
            .iter()
            .filter_map(|&c| match ast.get(c) {
                Node::Call { receiver, method, args, span, .. }
                    if args.is_empty()
                        && self_or_none(receiver)
                        && matches!(method.as_str(), "public" | "private" | "protected") =>
                {
                    Some(span.0)
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    for (off, m) in instance_defs {
        let on = mf_toggles
            .iter()
            .filter(|&&t| t < off)
            .max()
            .is_some_and(|&t| !resets.iter().any(|&r| t < r && r < off));
        if on {
            events.push((off, Err(Some(vec![m]))));
        }
    }
    events
}

/// The singleton writes of the file whose owner a CONSTANT receiver names, as
/// `(receiver path, event)`: `C.instance_eval { def x }` (every `def`),
/// `C.class_eval { def self.x }` (a singleton `def`, or one in a `class <<`
/// there), `C.singleton_class.<naming call>`, `def C.x`, and — issue #490 —
/// the singleton-slot writers the reference reads INSIDE an eval block: a
/// bare/`self.` `define_singleton_method`, a bare/`self.`-`singleton_class`
/// naming call (`singleton_class.alias_method`, `.define_method`, `.attr_*`,
/// the eval/`send` family), and `alias` under an `instance_*` eval
/// (`supersede_singleton_def_nodes` under `defs_singleton`).
fn foreign_singleton_events(ast: &LoweredAst) -> Vec<(String, SingletonEvent)> {
    use rigor_parse::Span;
    let within = |a: Span, b: Span| b.0 <= a.0 && a.1 <= b.1;
    let const_path = |r: Option<NodeId>| match r.map(|r| ast.get(r)) {
        Some(Node::ConstantRead { name, .. }) if !name.is_empty() => Some(name.to_string()),
        _ => None,
    };
    let self_or_none =
        |r: &Option<NodeId>| r.is_none_or(|r| matches!(ast.get(r), Node::SelfExpr { .. }));
    // (receiver path, block span, `instance_*`?)
    let mut eval_blocks: Vec<(String, Span, bool)> = Vec::new();
    let mut out: Vec<(String, SingletonEvent)> = Vec::new();
    // `class <<` spans, used before `sclasses` below to keep
    // `Const.singleton_class.X` quiet inside them: the reference's first
    // `singleton_call_target` arm is gated on `!in_singleton_class`.
    let sclass_all: Vec<Span> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::Definition { is_singleton_class: true, .. } => Some(n.span()),
            _ => None,
        })
        .collect();
    for (_, n) in ast.iter() {
        let Node::Call { receiver, method, args, block_span, span, .. } = n else {
            continue;
        };
        if let (Some(path), Some(b)) = (const_path(*receiver), block_span) {
            if INSTANCE_EVALS.contains(&method.as_str()) {
                eval_blocks.push((path, *b, true));
            } else if CLASS_EVALS.contains(&method.as_str()) {
                eval_blocks.push((path, *b, false));
            }
        }
        let sc_path = receiver.and_then(|r| match ast.get(r) {
            Node::Call { method, receiver: rr, .. } if method == "singleton_class" => const_path(*rr),
            _ => None,
        });
        if let Some(path) = sc_path {
            if !sclass_all.iter().any(|s| within(*span, *s)) {
                if let Some(names) = singleton_naming(ast, method, args) {
                    out.push((path, (span.0, Err(names))));
                }
            }
        }
    }
    // `class <<` interiors OUTSIDE a class body's own scan — at top level,
    // inside a `class << Const`, inside a `def`, inside an eval block —
    // still read `in_singleton_class` writers on the reference
    // (`walk_singleton_body`): a bare/`self.` naming call (`alias_method`,
    // `define_method`, `attr_*`, the eval/`send` family — never
    // `define_singleton_method`, which the naming list does not reach) and
    // the `alias` keyword supersede the `class <<` TARGET's slots. `self`
    // names the innermost enclosing eval receiver (`class << self` inside
    // `V.instance_eval` opens V's singleton); `class << self` inside a
    // class body is already covered by `singleton_events`' in_sc arm, and
    // `class << self` inside another `class <<` names the singleton's own
    // singleton — `:unnameable`, skipped.
    let sclasses: Vec<(NodeId, Span, Option<String>)> = ast
        .iter()
        .filter_map(|(id, n)| match n {
            Node::Definition { is_singleton_class: true, singleton_operand, .. } => {
                let owner = match singleton_operand.map(|op| ast.get(op)) {
                    Some(Node::ConstantRead { name, .. }) if !name.is_empty() => {
                        Some(name.strip_prefix("::").unwrap_or(name).to_string())
                    }
                    Some(Node::SelfExpr { .. }) => eval_blocks
                        .iter()
                        .filter(|(_, b, _)| within(n.span(), *b))
                        .min_by_key(|(_, b, _)| b.1 - b.0)
                        .map(|(p, _, _)| p.clone()),
                    _ => None,
                };
                Some((id, n.span(), owner))
            }
            _ => None,
        })
        .collect();
    let defs: Vec<Span> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::Definition { is_singleton_class: false, .. } => Some(n.span()),
            _ => None,
        })
        .collect();
    for (_, n) in ast.iter() {
        let span = n.span();
        if matches!(n, Node::Definition { .. }) {
            continue;
        }
        // Exclude nodes inside a `def` body — def interiors are never
        // reached by the reference's singleton walk.
        if defs.iter().any(|d| within(span, *d) && *d != span) {
            continue;
        }
        // Its innermost `class <<` — a deeper one owns the interior.
        let Some((_, sc_span, owner)) = sclasses
            .iter()
            .filter(|(_, s, _)| within(span, *s) && *s != span)
            .min_by_key(|(_, s, _)| s.1 - s.0)
        else {
            continue;
        };
        // An eval block nested inside that `class <<` governs its own
        // interior; a `class <<` inside an eval is still `class <<`
        // territory (`in_singleton_class` outranks `call_singleton`).
        if eval_blocks
            .iter()
            .any(|(_, b, _)| within(span, *b) && within(*b, *sc_span))
        {
            continue;
        }
        let Some(owner) = owner.clone() else {
            continue;
        };
        match n {
            Node::Call { receiver, method, args, .. } => {
                if self_or_none(receiver) {
                    if let Some(names) = singleton_naming(ast, method, args) {
                        out.push((owner, (span.0, Err(names))));
                    }
                }
            }
            Node::Alias { new_name, .. } => {
                out.push((owner, (span.0, Err(literal_name(ast, *new_name).map(|m| vec![m])))));
            }
            _ => {}
        }
    }
    if eval_blocks.is_empty() {
        return out;
    }
    // The eval block a node belongs to is its INNERMOST one — the reference's
    // walk threads a single def-owner prefix, so a call inside
    // `V.instance_eval { W.class_eval { … } }` contests `W`, not `V`.
    let innermost = |span: Span| {
        eval_blocks
            .iter()
            .filter(|(_, b, _)| within(span, *b))
            .min_by_key(|(_, b, _)| b.1 - b.0)
    };
    for (_, n) in ast.iter() {
        let span = n.span();
        let Some((path, _, instance)) = innermost(span) else {
            continue;
        };
        match n {
            Node::Call { receiver, method, args, .. } => {
                let on_bare_singleton_class = receiver.is_some_and(|r| {
                    matches!(ast.get(r), Node::Call { method, receiver: rr, .. }
                        if method == "singleton_class" && self_or_none(rr))
                });
                let names = if self_or_none(receiver) && method == "define_singleton_method" {
                    Some(all_literal_names(ast, &args[..args.len().min(1)]))
                } else if on_bare_singleton_class {
                    singleton_naming(ast, method, args)
                } else {
                    None
                };
                if let Some(names) = names {
                    out.push((path.clone(), (span.0, Err(names))));
                }
            }
            // `alias` binds on the singleton only under an `instance_*` split;
            // under `class_eval` the keyword stays instance-side.
            Node::Alias { new_name, .. } if *instance => {
                out.push((path.clone(), (span.0, Err(literal_name(ast, *new_name).map(|m| vec![m])))));
            }
            _ => {}
        }
    }
    for (_, n) in ast.iter() {
        let Node::Definition { is_singleton_class: false, name, singleton_name, receiver_def_name, def_receiver_path, .. } = n
        else {
            continue;
        };
        let Some(m) = name.as_ref().or(singleton_name.as_ref()).or(receiver_def_name.as_ref()) else {
            continue;
        };
        let span = n.span();
        let Some((path, b, instance)) = innermost(span) else {
            continue;
        };
        // Under an `instance_*` split every `def` binds on the eval owner's
        // singleton (`defs_singleton` is unconditional). Under `class_eval`
        // a `def` binds only when `def_singleton?` says so: `def self.x`, a
        // receiver def naming the eval's owner (`def V.x` — its last
        // segment), or a def inside a `class <<` nested in the block. A
        // bare `def x` or `def W.x` there is an ordinary instance def.
        // Inside a nested `class <<` the def files on the SCLASS's owner —
        // `self` names the eval's receiver, `class << W` names `W`.
        let inner_sclass = sclasses
            .iter()
            .filter(|(_, s, _)| within(*s, *b) && within(span, *s))
            .min_by_key(|(_, s, _)| s.1 - s.0);
        let target = match inner_sclass {
            Some((_, _, owner)) => owner.clone(),
            None => {
                if *instance
                    || singleton_name.is_some()
                    || def_receiver_path
                        .as_ref()
                        .is_some_and(|p| receiver_path_is_owner(p, path))
                {
                    Some(path.clone())
                } else {
                    None
                }
            }
        };
        if let Some(target) = target {
            out.push((target, (span.0, Err(Some(vec![m.clone()])))));
        }
    }
    out
}

/// The `def` children of a `class << <operand>` body — each files on the
/// resolved singleton owner ([`walk_fold_defs`]'s sclass arm) as
/// `DefKind::Singleton`. A `def self.x` directly inside files on the owner
/// too — the reference's `def_singleton?` reads only the SelfNode receiver,
/// so its slot write lands on the owner's singleton table even though MRI
/// would bind it on the singleton's singleton (issue #490). Nested
/// `class <<` bodies re-resolve their operand the same way, except
/// `class << self`: `self` in a `class <<` body is the singleton class
/// itself, so a nested `class << self` opens the singleton's own singleton,
/// which no `Const.x` call reaches (issue #476; the reference walks that
/// body as unnameable).
fn walk_singleton_fold_defs(
    ast: &LoweredAst,
    body: &[NodeId],
    owner: &str,
    out: &mut Vec<HarvestedFoldDef>,
) {
    for &child in body {
        match ast.get(child) {
            Node::Definition {
                is_singleton_class: true,
                singleton_operand,
                body: inner,
                ..
            } => {
                let inner_owner = match singleton_operand.map(|op| ast.get(op)) {
                    Some(Node::ConstantRead { name, .. }) if !name.is_empty() => {
                        Some(name.strip_prefix("::").unwrap_or(name).to_string())
                    }
                    _ => None,
                };
                if let Some(inner_owner) = inner_owner {
                    walk_singleton_fold_defs(ast, inner, &inner_owner, out);
                }
            }
            Node::Definition {
                name,
                singleton_name,
                receiver_def_name,
                body: def_body,
                has_explicit_return,
                param_shape,
                ..
            } => {
                // Issue #490 — inside a `class <<` body the reference's
                // `def_singleton?` short-circuits on `in_singleton_class`:
                // EVERY `def` there files on the body's owner — `def self.x`,
                // `def V.x`, `def W.x`, even `def obj.x` — so each is a
                // singleton site on the owner regardless of its receiver
                // (review findings w1/w13).
                let method = name
                    .as_ref()
                    .or(singleton_name.as_ref())
                    .or(receiver_def_name.as_ref())
                    .cloned();
                if let Some(method) = method {
                    if let Some(&tail) = def_body.last() {
                        out.push(HarvestedFoldDef {
                            owner: owner.to_string(),
                            method,
                            kind: DefKind::Singleton,
                            tail: capture_fold_tail(ast, tail, 0),
                            has_explicit_return: *has_explicit_return,
                            arity: fold_arity(param_shape),
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

/// **Issue #113 — capture one def's tail into an owned [`FoldTail`].**
///
/// The exact structural mirror of the pre-capture `fold_expr`: the same match
/// arms, in the same order, reading the same fields, with the same
/// `block_body.is_empty()` / `args.is_empty()` / `name.is_empty()` guards. Every
/// arm `fold_expr` did not have is this function's `_ => Decline`.
///
/// **Why this is exact and not a subset argument.** `fold_expr`'s decline is
/// decided by SYNTAX alone at every arm — the shape tag, `args.is_empty()`,
/// `block_body.is_empty()`, `name.is_empty()`. Nothing in the declining path
/// consults merged state, the `CoreIndex`, or another file. So the decision is
/// already available at harvest time and a `Decline` here is provably the `None`
/// `fold_expr` would have returned (`docs/notes/20260826-ast-eviction-probe.md`
/// §1.1). What is NOT decidable here — `resolve_instance_owner`, the definers
/// index, the overridable degrade, `folding::fold`'s own table — is exactly what
/// stays behind as an unresolved [`FoldExpr`] node for the merge to apply.
///
/// **`depth` is the cap, and it is now the ONLY place it lives.** `fold_expr`
/// checked `depth > FOLD_DEPTH_CAP` on ENTRY and `fold_key_sites` always entered
/// at 0, so nodes at depth `0..=16` were evaluated and a node at depth 17
/// declined without being looked at. The capture cuts at exactly that boundary
/// and [`SourceIndex::fold_tail`] carries no depth at all — which keeps the two
/// halves from disagreeing, and makes an off-by-one here observable rather than
/// masked by a second check downstream. `probes_s92`'s `fold_depth_cap_*` tests
/// pin the 15/16/17 boundary against the verbatim pre-capture oracle.
fn capture_fold_tail(ast: &LoweredAst, node_id: NodeId, depth: usize) -> FoldTail {
    if depth > FOLD_DEPTH_CAP {
        return FoldTail::Decline;
    }
    let expr = match ast.get(node_id) {
        Node::StringLit { value, .. } => FoldExpr::Scalar(Scalar::Str(value.clone())),
        Node::IntegerLit { value: Some(value), .. } => FoldExpr::Scalar(Scalar::Int(*value)),
        Node::FloatLit { value, .. } => FoldExpr::Scalar(Scalar::Float(*value)),
        Node::SymbolLit { value, .. } => FoldExpr::Scalar(Scalar::Sym(value.clone())),
        Node::NilLit { .. } => FoldExpr::Scalar(Scalar::Nil),
        Node::TrueLit { .. } => FoldExpr::Scalar(Scalar::Bool(true)),
        Node::FalseLit { .. } => FoldExpr::Scalar(Scalar::Bool(false)),
        Node::Call { receiver: None, method, block_body, .. } if block_body.is_empty() => {
            FoldExpr::SelfCall { method: method.clone() }
        }
        Node::Call { receiver: Some(r), method, args, block_body, .. }
            if block_body.is_empty() =>
        {
            if method == "!" && args.is_empty() {
                FoldExpr::Not { operand: capture_fold_tail(ast, *r, depth + 1) }
            } else {
                // `Const.method` — an OWN-CLASS singleton project call. Reached
                // ONLY here: a bare `ConstantRead` anywhere else falls to
                // `_ => Decline`, exactly as it hit `fold_expr`'s `_ => None`.
                let const_owner = match (args.is_empty(), ast.get(*r)) {
                    (true, Node::ConstantRead { name, .. }) if !name.is_empty() => {
                        Some(name.strip_prefix("::").unwrap_or(name).to_string())
                    }
                    _ => None,
                };
                match const_owner {
                    Some(owner) => FoldExpr::ConstCall { owner, method: method.clone() },
                    None => FoldExpr::CoreCall {
                        recv: capture_fold_tail(ast, *r, depth + 1),
                        method: method.clone(),
                        args: args
                            .iter()
                            .map(|&a| capture_fold_tail(ast, a, depth + 1))
                            .collect(),
                    },
                }
            }
        }
        _ => return FoldTail::Decline,
    };
    FoldTail::Expr(Box::new(expr))
}

/// ADR-0038 — invert the merged def sites into the `(method, kind) -> [qualified
/// owners]` definers index that drives the overridable degrade and implicit-self
/// resolution. Every read of the owner `Vec` is an `.any(…)` (`owner_defines`,
/// `overridden_in_project`), so its order is not semantic — which is just as
/// well: it comes from `HashMap` key iteration and is already unstable between
/// processes on the same input (issue #92 §3.4).
pub(crate) fn invert_definers(defs: &FoldDefs<'_>) -> HashMap<(String, DefKind), Vec<String>> {
    let mut definers: HashMap<(String, DefKind), Vec<String>> = HashMap::new();
    for (owner, method, kind) in defs.keys() {
        let owners = definers.entry((method.clone(), *kind)).or_default();
        if !owners.contains(owner) {
            owners.push(owner.clone());
        }
    }
    definers
}
