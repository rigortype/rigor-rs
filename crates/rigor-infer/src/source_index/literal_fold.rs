//! ADR-0038 interprocedural literal-tail fold: the Pass-4a tail capture (issue #113), the Pass-4b
//! fold with its overridable degrade (#94), and its lookups.

use rigor_index::CoreIndex;
use std::collections::{HashMap, HashSet, VecDeque};

use rigor_parse::{LoweredAst, Node, NodeId};
use rigor_types::Scalar;

use super::{
    qualify, split_qualified, DefKind, FoldDefs, HarvestedFoldDef, SourceIndex,
    OVERRIDE_ANCESTOR_WALK_LIMIT,
};

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
        // ADR-17: a `pre_eval:` patch of the name may be the method that
        // runs (the reference's patched tier answers `Dynamic` ahead of the
        // project's own `def`), so its body is no evidence.
        if self.pre_eval_defines_name(&key.1) {
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
        let settled = self.settled_definer(core, qual, method, false, |n| {
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

pub(crate) fn walk_fold_defs(
    ast: &LoweredAst,
    node: NodeId,
    prefix: &[String],
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
                    walk_fold_defs(ast, child, prefix, out);
                }
            }
        }
        Node::Statements { body, .. } => {
            for &child in body {
                walk_fold_defs(ast, child, prefix, out);
            }
        }
        Node::ClassDef { name, body, .. } | Node::ModuleDef { name, body, .. } => {
            if name.is_empty() {
                return;
            }
            let qualified = qualify(prefix, name);
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
                        body: def_body,
                        has_explicit_return,
                        param_shape,
                        ..
                    } => {
                        let entry = match (name, singleton_name) {
                            (Some(m), _) => Some((m.clone(), DefKind::Instance)),
                            (None, Some(m)) => Some((m.clone(), DefKind::Singleton)),
                            _ => None,
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
            let child_prefix = split_qualified(&qualified);
            for &child in body {
                walk_fold_defs(ast, child, &child_prefix, out);
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
    let mut owners: Vec<(NodeId, String)> = Vec::new();
    qualified_bodies(ast, ast.root(), &[], &mut owners);
    let mut events: HashMap<String, Vec<SingletonEvent>> = HashMap::new();
    for (id, owner) in owners {
        events.entry(owner).or_default().extend(singleton_events(ast, id));
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
/// [`walk_fold_defs`] qualifies it.
fn qualified_bodies(ast: &LoweredAst, node: NodeId, prefix: &[String], out: &mut Vec<(NodeId, String)>) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                qualified_bodies(ast, child, prefix, out);
            }
        }
        Node::ClassDef { name, body, .. } | Node::ModuleDef { name, body, .. } => {
            if name.is_empty() {
                return;
            }
            let qualified = qualify(prefix, name);
            out.push((node, qualified.clone()));
            let child_prefix = split_qualified(&qualified);
            for &child in body {
                qualified_bodies(ast, child, &child_prefix, out);
            }
        }
        _ => {}
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
/// `def self.x`, and a direct `def` of a direct `class << self` body. These
/// are the reference's certain slot writes; every other singleton write is
/// possible or not folded.
fn certain_singleton_defs(ast: &LoweredAst, body: &[NodeId], in_sc: bool, out: &mut HashSet<NodeId>) {
    for &child in body {
        match ast.get(child) {
            Node::Definition { is_singleton_class: true, singleton_operand, body: inner, .. } => {
                if !in_sc && singleton_operand.is_some_and(|op| matches!(ast.get(op), Node::SelfExpr { .. })) {
                    certain_singleton_defs(ast, inner, true, out);
                }
            }
            Node::Definition { name: Some(_), .. } if in_sc => {
                out.insert(child);
            }
            Node::Definition { singleton_name: Some(_), .. } if !in_sc => {
                out.insert(child);
            }
            _ => {}
        }
    }
}

fn singleton_events(ast: &LoweredAst, class_id: NodeId) -> Vec<SingletonEvent> {
    use rigor_parse::Span;
    let within = |a: Span, b: Span| b.0 <= a.0 && a.1 <= b.1;
    let class_node = ast.get(class_id);
    let class_span = class_node.span();
    let mut certain: HashSet<NodeId> = HashSet::new();
    if let Node::ClassDef { body, .. } | Node::ModuleDef { body, .. } = class_node {
        certain_singleton_defs(ast, body, false, &mut certain);
    }
    let self_or_none =
        |r: &Option<NodeId>| r.is_none_or(|r| matches!(ast.get(r), Node::SelfExpr { .. }));
    // The scopes nested in this body: (id, span, `class << self`?).
    let nested: Vec<(NodeId, Span, bool)> = ast
        .iter()
        .filter(|(id, n)| *id != class_id && within(n.span(), class_span))
        .filter_map(|(id, n)| match n {
            Node::ClassDef { .. } | Node::ModuleDef { .. } => Some((id, n.span(), false)),
            Node::Definition { is_singleton_class: true, singleton_operand, .. } => {
                let own = singleton_operand
                    .is_some_and(|op| matches!(ast.get(op), Node::SelfExpr { .. }));
                Some((id, n.span(), own))
            }
            Node::Definition { .. } => Some((id, n.span(), false)),
            _ => None,
        })
        .collect();
    // The `instance_*` eval blocks inside this body, any receiver: their
    // `def`s bind on a singleton (this class's, for a `self` receiver; an
    // explicit receiver's is also read by [`foreign_singleton_events`]).
    let instance_eval_blocks: Vec<Span> = ast
        .iter()
        .filter(|(_, n)| within(n.span(), class_span))
        .filter_map(|(_, n)| match n {
            Node::Call { method, block_span: Some(b), .. } if INSTANCE_EVALS.contains(&method.as_str()) => {
                Some(*b)
            }
            _ => None,
        })
        .collect();
    let mut events: Vec<SingletonEvent> = Vec::new();
    // Bare `module_function` toggles of this body, and the receiverless
    // instance `def`s of this body (each a copy candidate).
    let mut mf_toggles: Vec<usize> = Vec::new();
    let mut instance_defs: Vec<(usize, String)> = Vec::new();
    for (id, n) in ast.iter() {
        let span = n.span();
        if id == class_id || !within(span, class_span) {
            continue;
        }
        // The innermost nested scope holding `n` (not `n` itself).
        let scope = nested
            .iter()
            .filter(|(sid, s, _)| *sid != id && within(span, *s))
            .min_by_key(|(_, s, _)| s.1 - s.0);
        let in_sc = match scope {
            None => false,
            Some((_, _, true)) => true,
            Some(_) => continue,
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
        match n {
            Node::Definition { singleton_name: Some(m), .. } if !in_sc => events.push(write(m)),
            Node::Definition { name: Some(m), .. } if in_sc => events.push(write(m)),
            Node::Definition { name: Some(m), .. } => {
                if instance_eval_blocks.iter().any(|b| within(span, *b)) {
                    events.push((span.0, Err(Some(vec![m.clone()]))));
                }
                instance_defs.push((span.0, m.clone()));
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
                let names = if on_singleton_class && !in_sc {
                    singleton_naming(ast, method, args)
                } else if self_or_none(receiver) && method == "define_singleton_method" && !in_sc {
                    Some(all_literal_names(ast, &args[..args.len().min(1)]))
                } else if self_or_none(receiver) && in_sc {
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
/// there), `C.singleton_class.<naming call>`, and `def C.x`.
fn foreign_singleton_events(ast: &LoweredAst) -> Vec<(String, SingletonEvent)> {
    use rigor_parse::Span;
    let within = |a: Span, b: Span| b.0 <= a.0 && a.1 <= b.1;
    let const_path = |r: Option<NodeId>| match r.map(|r| ast.get(r)) {
        Some(Node::ConstantRead { name, .. }) if !name.is_empty() => Some(name.clone()),
        _ => None,
    };
    // (receiver path, block span, `instance_*`?)
    let mut eval_blocks: Vec<(String, Span, bool)> = Vec::new();
    let mut out: Vec<(String, SingletonEvent)> = Vec::new();
    for (_, n) in ast.iter() {
        match n {
            Node::Call { receiver, method, args, block_span, span, .. } => {
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
                    if let Some(names) = singleton_naming(ast, method, args) {
                        out.push((path, (span.0, Err(names))));
                    }
                }
            }
            Node::Definition { receiver_def_name: Some(m), def_receiver_path: Some(path), span, .. } => {
                out.push((path.clone(), (span.0, Err(Some(vec![m.clone()])))));
            }
            _ => {}
        }
    }
    if eval_blocks.is_empty() {
        return out;
    }
    let sclasses: Vec<Span> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::Definition { is_singleton_class: true, .. } => Some(n.span()),
            _ => None,
        })
        .collect();
    for (_, n) in ast.iter() {
        let Node::Definition { is_singleton_class: false, name, singleton_name, receiver_def_name, .. } = n
        else {
            continue;
        };
        let Some(m) = name.as_ref().or(singleton_name.as_ref()).or(receiver_def_name.as_ref()) else {
            continue;
        };
        let span = n.span();
        for (path, b, instance) in &eval_blocks {
            if !within(span, *b) {
                continue;
            }
            let binds = *instance
                || name.is_none()
                || sclasses.iter().any(|s| within(*s, *b) && within(span, *s));
            if binds {
                out.push((path.clone(), (span.0, Err(Some(vec![m.clone()])))));
            }
        }
    }
    out
}

/// The `def` children of a `class << <operand>` body — each files on the
/// resolved singleton owner ([`walk_fold_defs`]'s sclass arm) as
/// `DefKind::Singleton`. Nested `class <<` bodies re-resolve their operand
/// the same way, except `class << self`: `self` in a `class <<` body is the
/// singleton class itself, so a nested `class << self` opens the
/// singleton's own singleton, which no `Const.x` call reaches (issue #476;
/// the reference walks that body as unnameable). A `def self.x` /
/// `def <recv>.x` inside one is a singleton-of-singleton and stays out of
/// scope too.
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
                name: Some(m),
                body: def_body,
                has_explicit_return,
                param_shape,
                ..
            } => {
                if let Some(&tail) = def_body.last() {
                    out.push(HarvestedFoldDef {
                        owner: owner.to_string(),
                        method: m.clone(),
                        kind: DefKind::Singleton,
                        tail: capture_fold_tail(ast, tail, 0),
                        has_explicit_return: *has_explicit_return,
                        arity: fold_arity(param_shape),
                    });
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
