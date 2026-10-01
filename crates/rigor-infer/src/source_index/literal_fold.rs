//! ADR-0038 interprocedural literal-tail fold: the Pass-4a tail capture (issue #113), the Pass-4b
//! fold with its overridable degrade (#94), and its lookups.

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
        self_qual: &str,
        self_kind: DefKind,
        method: &str,
    ) -> Option<Scalar> {
        let (owner, kind) = match self_kind {
            DefKind::Singleton => self.resolve_singleton_slot(self_qual, method)?,
            DefKind::Instance => (self.resolve_instance_owner(self_qual, method)?, DefKind::Instance),
        };
        self.literal_returns
            .get(&(owner, method.to_string(), kind))
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
        defs: &FoldDefs<'_>,
    ) -> HashMap<(String, String, DefKind), Scalar> {
        let mut memo: HashMap<(String, String, DefKind), Option<Scalar>> = HashMap::new();
        // Born here, dies here: the closure memo is valid exactly as long as
        // `override_classes` is frozen, which is exactly this call (every reader
        // below takes `&self`, and M1's replay finished before M3 started).
        let mut closures: AncestorClosures = AncestorClosures::new();
        for key in defs.keys() {
            let mut visiting: HashSet<(String, String, DefKind)> = HashSet::new();
            self.resolve_fold_key(key, defs, &mut memo, &mut visiting, &mut closures);
        }
        memo.into_iter().filter_map(|(k, v)| v.map(|s| (k, s))).collect()
    }

    /// Resolve one `(owner, method, kind)`'s folded literal (memoized), applying
    /// the overridable degrade: a value-pinned base return is dropped when a
    /// RELATED subclass/includer redefines the method (else adopting the base's
    /// literal as a flow constant is unsound — the reference `degrade_if_overridable`).
    fn resolve_fold_key(
        &self,
        key: &(String, String, DefKind),
        defs: &FoldDefs<'_>,
        memo: &mut HashMap<(String, String, DefKind), Option<Scalar>>,
        visiting: &mut HashSet<(String, String, DefKind)>,
        closures: &mut AncestorClosures,
    ) -> Option<Scalar> {
        if let Some(v) = memo.get(key) {
            return v.clone();
        }
        if visiting.contains(key) {
            return None; // cycle (recursive method) ⇒ decline, don't memoize.
        }
        visiting.insert(key.clone());
        let raw = self.fold_key_sites(key, defs, memo, visiting, closures);
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
            let s = self.fold_tail(site.tail, &key.0, key.2, defs, memo, visiting, closures)?;
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
                        (self.resolve_instance_owner(self_qual, method)?, DefKind::Instance)
                    }
                };
                self.resolve_fold_key(&(owner, method.clone(), kind), defs, memo, visiting, closures)
            }
            // `Const.method` — an OWN-CLASS singleton project call.
            FoldExpr::ConstCall { owner, method } => self.resolve_fold_key(
                &(owner.clone(), method.clone(), DefKind::Singleton),
                defs,
                memo,
                visiting,
                closures,
            ),
            // `!expr`: fold the receiver and invert its Ruby truthiness (this is
            // what turns `read_write? = !read_only?` into `true`).
            FoldExpr::Not { operand } => {
                let s =
                    self.fold_tail(operand, self_qual, self_kind, defs, memo, visiting, closures)?;
                Some(Scalar::Bool(!scalar_truthy(&s)))
            }
            // A core fold on a value-pinned receiver + args (`1 + 1`, `"x" ==
            // "y"`). Declines unless every part folds.
            FoldExpr::CoreCall { recv, method, args } => {
                let recv =
                    self.fold_tail(recv, self_qual, self_kind, defs, memo, visiting, closures)?;
                let mut arg_scalars = Vec::with_capacity(args.len());
                for a in args {
                    arg_scalars.push(self.fold_tail(
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
    pub(crate) fn resolve_instance_owner(&self, qual: &str, method: &str) -> Option<String> {
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

/// Ruby truthiness of a folded scalar: only `nil` / `false` are falsey.
pub(crate) fn scalar_truthy(s: &Scalar) -> bool {
    !matches!(s, Scalar::Nil | Scalar::Bool(false))
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
pub(crate) fn walk_fold_defs(
    ast: &LoweredAst,
    node: NodeId,
    prefix: &[String],
    out: &mut Vec<HarvestedFoldDef>,
) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
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

/// The `def` children of a `class << <operand>` body — each files on the
/// resolved singleton owner ([`walk_fold_defs`]'s sclass arm) as
/// `DefKind::Singleton`. Nested `class <<` bodies re-resolve their operand
/// the same way; a `def self.x` / `def <recv>.x` inside one is a
/// singleton-of-singleton and stays out of scope.
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
                    Some(Node::SelfExpr { .. }) => Some(owner.to_string()),
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
                ..
            } => {
                if let Some(&tail) = def_body.last() {
                    out.push(HarvestedFoldDef {
                        owner: owner.to_string(),
                        method: m.clone(),
                        kind: DefKind::Singleton,
                        tail: capture_fold_tail(ast, tail, 0),
                        has_explicit_return: *has_explicit_return,
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
