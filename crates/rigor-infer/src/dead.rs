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
/// (`EXIT_CALLS`, `statement_evaluator.rb:5025`).
const EXIT_CALLS: &[&str] = &["raise", "throw", "exit", "abort", "fail"];

/// The `scope.self_type` shape a call site's `self` takes — the input
/// `self_type_answers?` (`expression_typer.rb:1541`) dispatches on for the
/// toplevel-`def` bind veto.
pub(crate) enum CallSelf {
    /// `self` is an instance of the qualified class — a `def` body that is
    /// not singleton-side (`self_type_for_method_body` → `Nominal[path]`).
    Instance(String),
    /// `self` is the class object — `def self.x`, any `def` under a
    /// `class <<` frame, a `def <recv>.x` naming the enclosing class, and
    /// every class/module/`class <<` BODY statement
    /// (`self_type_for_class_body` → `Singleton[path]`).
    Singleton(String),
    /// `self_type` is nil — file top level, a `def`/`class <<` whose
    /// class-context path is empty, and `def <receiver>.x` forms the
    /// oracle keeps no modelled `self` for. Nothing vetoes the toplevel
    /// bind there.
    Unmodelled,
}

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
    ///
    /// Memoized on `(ast, pred)`: the fold re-enters through `expr_scalar` →
    /// `local_reach` → `definitely_assigns` → `expr_truthiness`, so without
    /// the memo every `if` on a read's path re-folds every other (the `seen`
    /// fuel bounds DEPTH, not the per-predicate fanout — exponential on real
    /// files). Re-entry to a predicate already folding answers `None`, the
    /// decline side.
    pub(crate) fn expr_truthiness(
        &self,
        ast: &LoweredAst,
        pred: NodeId,
        seen: &mut Vec<String>,
    ) -> Option<bool> {
        let key = (ast as *const LoweredAst as usize, pred.0);
        if let Some(hit) = self.truthy_memo.borrow().get(&key) {
            return *hit;
        }
        if !self.truthy_in_flight.borrow_mut().insert(key) {
            return None;
        }
        let out = self.expr_truthiness_fold(ast, pred, seen);
        self.truthy_in_flight.borrow_mut().remove(&key);
        self.truthy_memo.borrow_mut().insert(key, out);
        out
    }

    /// The actual predicate fold behind [`Typer::expr_truthiness`]'s memo.
    fn expr_truthiness_fold(
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
                let (qual, kind) = match self.call_self(ast, *span) {
                    CallSelf::Singleton(q) => (q, DefKind::Singleton),
                    CallSelf::Instance(q) => (q, DefKind::Instance),
                    // An unmodelled `self` resolves only the toplevel def —
                    // no class table lookup.
                    CallSelf::Unmodelled => (String::new(), DefKind::Instance),
                };
                self.source
                    .implicit_self_literal(&qual, kind, method)
                    .or_else(|| self.file_def_literal(ast, method, *span, seen))
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
    ///
    /// The oracle binds this surface UNCONDITIONALLY —
    /// `try_local_def_dispatch` → `infer_top_level_user_method` never
    /// consults `degrade_if_overridable`, so a same-named `def` in some
    /// unrelated class does NOT drop the fold (`def helper = true; module M;
    /// def helper = false; end; … if helper` still folds truthy on the
    /// oracle). The only gate is `self_type_answers?` (`expression_typer.rb:
    /// 1541`, issue #618): the toplevel `def` is a private `Object` method —
    /// the LAST link of every MRO — so it binds only where the call's own
    /// `self` does not answer `name` first. At file top level `self_type` is
    /// nil and nothing vetoes.
    pub(crate) fn file_def_literal(
        &self,
        ast: &LoweredAst,
        name: &str,
        site: Span,
        seen: &mut Vec<String>,
    ) -> Option<Scalar> {
        if self.self_answers(ast, site, name) {
            return None;
        }
        let containers: Vec<Span> = ast
            .iter()
            .filter_map(|(_, n)| match n {
                Node::ClassDef { span, .. }
                | Node::ModuleDef { span, .. }
                | Node::Definition { span, .. } => Some(*span),
                _ => None,
            })
            .collect();
        // The LAST same-named `def` wins a call (Ruby re-definition), so a
        // shadowing second `def` never lets the first one's literal fold.
        // A receiver-bearing `def Foo.bar` / `def obj.x` files under
        // `<toplevel>` exactly like a bare `def` (`file_def`'s empty-owner
        // arm); `def self.x` does not (the reference's `def_singleton?`
        // skip).
        ast.iter()
            .filter_map(|(_, n)| {
                let Node::Definition {
                    name: def_name,
                    receiver_def_name,
                    singleton_name: None,
                    is_singleton_class: false,
                    has_explicit_return,
                    body,
                    span,
                    ..
                } = n
                else {
                    return None;
                };
                let m = def_name.as_deref().or(receiver_def_name.as_deref());
                // `has_explicit_return`: the reference unions explicit
                // returns with the tail, so a `return` anywhere in the body
                // (a block-nested one included — it exits the method) drops
                // the fold (`def helper(x); return false if x; true; end`
                // stays silent on the oracle).
                if m != Some(name)
                    || *has_explicit_return
                    || containers.iter().any(|&c| c != *span && contains(c, *span))
                {
                    return None;
                }
                body.last().copied()
            })
            .last()
            .and_then(|tail| self.expr_scalar(ast, tail, seen))
    }

    /// `self_type_answers?` (`expression_typer.rb:1541`) — whether the call
    /// site's own `self` answers `name`, in which case a same-named toplevel
    /// `def` never binds there. `false` at file top level, where
    /// `self_type` is nil.
    fn self_answers(&self, ast: &LoweredAst, site: Span, name: &str) -> bool {
        match self.call_self(ast, site) {
            CallSelf::Singleton(qual) => {
                !qual.is_empty() && self.singleton_answers(&qual, name)
            }
            CallSelf::Instance(qual) => {
                !qual.is_empty() && self.instance_answers(&qual, name)
            }
            // `def <receiver>.x` — the oracle does not model `self` there,
            // so nothing vetoes (`def Foo.m; … if helper` binds the toplevel
            // `def helper` even when `Foo.self.helper` exists — probed).
            CallSelf::Unmodelled => false,
        }
    }

    /// The `self` a call at `site` dispatches on — the `scope.self_type`
    /// shape `self_type_answers?` reads. Rebuilt from the containing
    /// `def` / `class` / `module` / `class <<` nodes exactly as the
    /// reference builds `@class_context`: `class`/`module` push a plain
    /// frame and a `def` pushes none; `class << self` re-marks the
    /// innermost frame singleton, `class << Const` re-marks it when it
    /// names the innermost frame and REPLACES the whole stack otherwise,
    /// and `class << <other>` leaves the context unchanged
    /// (`singleton_context_for`, `statement_evaluator.rb:4856`).
    pub(crate) fn call_self(&self, ast: &LoweredAst, site: Span) -> CallSelf {
        let mut chain: Vec<&Node> = Vec::new();
        for (_, n) in ast.iter() {
            let span = match n {
                Node::Definition { span, .. }
                | Node::ClassDef { span, .. }
                | Node::ModuleDef { span, .. } => *span,
                _ => continue,
            };
            if contains(span, site) {
                chain.push(n);
            }
        }
        chain.sort_by_key(|n| {
            let s = n.span();
            s.1 - s.0
        });

        // The class-context frames, outermost-first — each `(frame name,
        // singleton-marked)`; a frame name keeps its written `A::B` path,
        // so `path` joins the same way `current_class_path` does.
        let mut frames: Vec<(String, bool)> = Vec::new();
        for n in chain.iter().rev() {
            match n {
                Node::ClassDef { name, .. } | Node::ModuleDef { name, .. }
                    if !name.is_empty() =>
                {
                    frames.push((name.clone(), false));
                }
                Node::Definition {
                    is_singleton_class: true,
                    singleton_operand,
                    ..
                } => match singleton_operand.map(|op| ast.get(op)) {
                    // `class << self` re-tags the innermost enclosing frame.
                    Some(Node::SelfExpr { .. }) => {
                        if let Some((_, s)) = frames.last_mut() {
                            *s = true;
                        }
                    }
                    Some(Node::ConstantRead { name, .. }) => {
                        if frames.last().is_some_and(|(n, _)| n == name) {
                            if let Some((_, s)) = frames.last_mut() {
                                *s = true;
                            }
                        } else {
                            frames.clear();
                            frames.push((name.clone(), true));
                        }
                    }
                    // `class << <other>` — the context is unchanged.
                    _ => {}
                },
                _ => {}
            }
        }
        let path = frames
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join("::");
        let inner_singleton = frames.last().is_some_and(|(_, s)| *s);

        let Some(inner) = chain.first() else {
            // File top level — `self_type` is nil.
            return CallSelf::Unmodelled;
        };
        match inner {
            // A `def` body — `self_type_for_method_body` + `singleton_def?`:
            // `def self.x`, ANY def under a `class <<` frame, and a
            // const-receiver `def C.x` naming the enclosing class
            // (`def_receiver_targets_lexical_self?`) are singleton-side;
            // every other `def`'s `self` is the instance. An empty path is
            // the reference's `nil` — no modelled `self` at all.
            Node::Definition {
                is_singleton_class: false,
                singleton_name,
                receiver_def_name,
                def_receiver_path,
                ..
            } => {
                if path.is_empty() {
                    return CallSelf::Unmodelled;
                }
                if singleton_name.is_some() || inner_singleton {
                    return CallSelf::Singleton(path);
                }
                if receiver_def_name.is_some() {
                    if let Some(rp) = def_receiver_path {
                        let segs: Vec<&str> = rp.split("::").collect();
                        if !segs.is_empty()
                            && segs.len() <= frames.len()
                            && frames[frames.len() - segs.len()..]
                                .iter()
                                .map(|(n, _)| n.as_str())
                                .eq(segs.iter().copied())
                        {
                            return CallSelf::Singleton(path);
                        }
                    }
                }
                CallSelf::Instance(path)
            }
            // A class / module / `class <<` BODY statement — `self` is the
            // class object itself (`self_type_for_class_body` →
            // `Singleton[path]`).
            _ if path.is_empty() => CallSelf::Unmodelled,
            _ => CallSelf::Singleton(path),
        }
    }

    /// The instance side of `self_type_answers?` (`instance_self_answers?`):
    /// a method the project declares on `qual` or a project ancestor —
    /// `def`, `attr_*`, `alias`, `define_method` — or a member an
    /// RBS-declared ancestor carries ahead of `Object` in the MRO.
    fn instance_answers(&self, qual: &str, name: &str) -> bool {
        self.source
            .project_declares_method_through_ancestors(self.file_key(), qual, name)
            || self.rbs_ancestor_answers(qual, name)
    }

    /// `rbs_ancestor_answers?` — whether an RBS surface the enclosing class
    /// inherits (its own when the name reopens a bundled class, else a
    /// written ancestor's) declares `name`. A declaration owned by `Object`,
    /// `Kernel` or `BasicObject` sits at-or-after the toplevel `def`'s own
    /// rung and does NOT veto (`instance_self_answers?`'s `::Object`
    /// cut-off). The walk uses the WRITTEN ancestor names —
    /// `override_ancestor_names` drops non-project ancestors, which is
    /// exactly the surface being asked here.
    fn rbs_ancestor_answers(&self, qual: &str, name: &str) -> bool {
        let mut queue: Vec<String> = vec![qual.to_string()];
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut visited = 0usize;
        while let Some(current) = queue.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            visited += 1;
            if visited > crate::source_index::OVERRIDE_ANCESTOR_WALK_LIMIT {
                return false;
            }
            if self.index.knows_toplevel_class(&current)
                || self.index.knows_qualified_class(&current)
            {
                // `declaring_ancestor` resolves the method's OWNER over the
                // whole RBS chain from `current`, so nothing under it needs
                // the queue — only a declaration before `Object` vetoes.
                if self
                    .index
                    .declaring_ancestor(&current, name)
                    .is_some_and(|owner| {
                        !matches!(owner, "Object" | "Kernel" | "BasicObject")
                    })
                {
                    return true;
                }
                continue;
            }
            // A project class: keep walking its written ancestors (the
            // project's own members were the
            // `project_declares_method_through_ancestors` arm's question).
            queue.extend(self.source.written_ancestor_names(&current));
        }
        false
    }

    /// `singleton_self_answers?` — a `def self.name` (or `class <<` def) on
    /// `qual`, on one of its `extend`ed modules' INSTANCE surface
    /// (ScopeIndexer folds `extend` into the extender's own singleton
    /// entries), or on a superclass reached through
    /// `singleton_def_through_ancestors`'s SUPERCLASS-ONLY chain
    /// (`mixins: false` — an `include`d module's `def self.x` is not
    /// callable on the includer); plus the RBS arm, which is own-class
    /// only (`rbs_declared_on_class?` on the `:singleton` definition).
    fn singleton_answers(&self, qual: &str, name: &str) -> bool {
        let mut current = qual.to_string();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut visited = 0usize;
        loop {
            if !seen.insert(current.clone()) {
                break;
            }
            visited += 1;
            // Past the walk cap the answer is "answers" — declining the
            // fold is the uncertainty side, mirroring
            // `discovered_method_through_ancestors?`.
            if visited > crate::source_index::OVERRIDE_ANCESTOR_WALK_LIMIT {
                return true;
            }
            if self
                .source
                .owner_defines(&current, name, DefKind::Singleton)
            {
                return true;
            }
            for ext in self.source.extended_names(&current) {
                if self.source.owner_defines(&ext, name, DefKind::Instance) {
                    return true;
                }
            }
            let Some(next) = self.source.project_superclass(&current) else {
                break;
            };
            current = next;
        }
        self.index.singleton_declared_own(qual, name)
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

    /// Whether a `retry` re-entering the `begin` that owns `clause_span`
    /// sits inside the clause — `retry` retries the NEAREST enclosing
    /// `begin`, so one under a nested `begin`/`def`/`class`/`module` (or a
    /// nested `class <<`) belongs to that inner frame and does not count.
    /// When a clause carries one, the arm re-runs on the next pass: its own
    /// writes reach earlier reads, exactly the carrier a loop is.
    pub(crate) fn clause_retries(&self, ast: &LoweredAst, clause_span: Span) -> bool {
        let nested: Vec<Span> = ast
            .iter()
            .filter_map(|(_, n)| match n {
                // Only a REAL `begin` frame bounds `retry`. The builder also
                // reuses `BeginRescue` as the carrier for an `if`'s else body,
                // a `case/in` arm, and multi-statement parens — each an empty
                // shell (`clauses`/`ensure_body` empty) that a `retry` passes
                // straight through to the enclosing `begin` (`rescue; if c;
                // else; retry; end; end` retries the OUTER begin —
                // `logger/log_device.rb`'s `retry_limit -= 1` shape). A bare
                // `begin … end` with no rescue/ensure is misread as a shell,
                // but that only widens the outer clause — losing a diagnostic,
                // never inventing one.
                Node::BeginRescue {
                    span,
                    clauses,
                    ensure_body,
                    ..
                } if contains(clause_span, *span)
                    && (!clauses.is_empty() || !ensure_body.is_empty()) =>
                {
                    Some(*span)
                }
                Node::Definition { span, .. }
                | Node::ClassDef { span, .. }
                | Node::ModuleDef { span, .. }
                    if contains(clause_span, *span) =>
                {
                    Some(*span)
                }
                _ => None,
            })
            .collect();
        ast.iter().any(|(_, n)| {
            let span = match n {
                Node::Statements {
                    kind: StatementsKind::Jump(JumpKind::Retry),
                    span,
                    ..
                }
                | Node::Other {
                    jump: Some(JumpKind::Retry),
                    span,
                } => *span,
                _ => return false,
            };
            contains(clause_span, span) && !nested.iter().any(|&s| contains(s, span))
        })
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
