//! The recovered-children walk behind the `Statements` carriers of Prism nodes
//! that have no owned variant, and its call-suppressing `defined?` variant.

use crate::ruby_prism::{self, Node as PrismNode};

/// One recovered child of an unhandled wrapper: the outermost recoverable
/// Prism node plus `bound`, the union of `locals` of every `BlockNode` /
/// `LambdaNode` the walk CROSSED to reach it (rigor-rs#137, upstream
/// rigor#1245). A block buried under a wrapper the evaluator never enters —
/// `super { |o| o + 1 }` is the upstream row — has no `Node::Call` /
/// `Node::Lambda` to carry its bound names, so they are recorded here and the
/// shadow pass applies them at the recovered child's use sites. A `super`
/// ARGUMENT recovered outside the block keeps `bound` empty and reads the
/// enclosing scope, exactly as it should.
pub(crate) struct Recovered<'pr> {
    pub(crate) node: PrismNode<'pr>,
    pub(crate) bound: Vec<String>,
    /// Whether the walk reached this node inside a scope-JOINING
    /// construct — a `rescue` modifier's arm, an `&&`/`||` right
    /// operand, an `if`/`case`/`in` arm, a loop body, a
    /// `begin … rescue`, a crossed block/lambda body. The reference
    /// merges each such position's post-scope with a sibling scope
    /// (`eval_rescue_modifier`, `eval_and_or`, `eval_if`, `eval_loop`,
    /// `eval_begin`, `join_case_branch_scopes`), and the join intersects
    /// away per-slot facts like the `h[k] -> stored` narrowing
    /// `eval_index_or_write` records — while the write's `[]=`
    /// receiver widening still applies. Nested recovery during this
    /// node's lowering inherits the mark (rigor-rs#312).
    pub(crate) joined: bool,
    /// Whether the walk reached this node inside a position whose
    /// post-scope is DISCARDED outright — an unconditionally-exiting
    /// branch (`branch_unconditionally_exits?`), a never-evaluated
    /// operand (`super`/`yield`/`BEGIN`/`END`), or a fresh local scope
    /// (`def`/`class`/`module`/`class <<`). Scope effects there can
    /// never reach an enclosing local, joined or not; nested recovery
    /// during this node's lowering inherits the block.
    pub(crate) blocked: bool,
}

/// Collect the OUTERMOST "recoverable" descendant Prism nodes of an unhandled
/// node — a local read / write / operator-write / call — WITHOUT descending past
/// one (so [`Builder::lower_node`] recurses into it once, normally). Used by the
/// catch-all to recover reads/calls buried under a wrapper Prism node that has no
/// owned variant (`return`, `super`, `*splat`, …), keeping them visible to
/// structural walks like `flow.dead-assignment` and the call rules.
///
/// We deliberately do NOT collect a `def`/`class`/`module` here: those are not
/// found inside expression wrappers in practice, and recovering one flatly (no
/// owned `Definition`) would confuse the dead-assignment nested-unit barrier.
///
/// [`Builder::lower_node`]: crate::ast::Builder::lower_node
/// `joined` is the enclosing scope-join mark: `true` when `node` was already
/// recovered inside a scope-joining construct, so a lowered carrier's nested
/// recovery does not resurrect a slot narrowing the join had already erased
/// (rigor-rs#312 — see [`Recovered::joined`]).
pub(crate) fn collect_recoverable_children<'pr>(
    node: &PrismNode<'pr>,
    joined: bool,
    blocked: bool,
) -> Vec<Recovered<'pr>> {
    collect_recoverable(node, false, joined, blocked)
}

/// `collect_recoverable_children` with CALLS suppressed — the recovery a
/// `defined?` operand gets (upstream #318 / `9e55deae`, pin `v0.3.4`).
///
/// `defined?` inspects its argument statically and never runs it, so no call
/// under it is reachable code and none of the call rules may see one. But the
/// local READS under it are still reads: the reference's #318 change touched its
/// three engine tree-walks only, and `DeadAssignmentCollector#gather_read_names`
/// does its own `rigor_each_child` recursion, which still descends into a
/// `DefinedNode`. So `y = 1; defined?(y)` is NOT a dead assignment there —
/// measured at the `v0.3.4` pin, and the reason this is a call-suppressing
/// recovery rather than the leaf it started as. Calls are recursed THROUGH
/// rather than recorded, so a read in `defined?(foo(y))` still counts.
pub(crate) fn collect_defined_operand_children<'pr>(
    node: &PrismNode<'pr>,
    joined: bool,
) -> Vec<Recovered<'pr>> {
    // A `defined?` operand is never evaluated — the block is unconditional.
    collect_recoverable(node, true, joined, true)
}

fn collect_recoverable<'pr>(
    node: &PrismNode<'pr>,
    suppress_calls: bool,
    joined: bool,
    blocked: bool,
) -> Vec<Recovered<'pr>> {
    use ruby_prism::Visit;
    struct Collector<'a, 'pr> {
        out: &'a mut Vec<Recovered<'pr>>,
        suppress_calls: bool,
        /// `locals` of the `BlockNode` / `LambdaNode` chain the walk is
        /// currently inside — a recovered child's closure-shadow set
        /// (rigor-rs#137).
        bound: Vec<String>,
        /// Depth of scope-JOINING constructs the walk is inside — see
        /// [`Recovered::joined`]. Seeded by the caller's mark so nested
        /// recovery under a joined carrier stays joined.
        joined: u32,
        /// Depth of scope-DISCARDING positions the walk is inside — see
        /// [`Recovered::blocked`]. Seeded the same way.
        blocked: u32,
    }
    impl<'pr> Collector<'_, 'pr> {
        fn push(&mut self, node: PrismNode<'pr>) {
            self.out.push(Recovered {
                node,
                bound: self.bound.clone(),
                joined: self.joined > 0,
                blocked: self.blocked > 0,
            });
        }

        /// Visit `f` with the walk marked inside a scope-joining
        /// construct: every recoverable node `f` reaches records
        /// [`Recovered::joined`].
        fn under_join(&mut self, f: impl FnOnce(&mut Self)) {
            self.joined += 1;
            f(self);
            self.joined -= 1;
        }

        /// Visit `f` where the reference DISCARDS the position's post-scope
        /// or never evaluates it — see [`Recovered::blocked`]. The join
        /// mark is cleared too: an inner joiner cannot re-arm a widening
        /// that never reaches the merge.
        fn blocked_subtree(&mut self, f: impl FnOnce(&mut Self)) {
            let saved = std::mem::replace(&mut self.joined, 0);
            self.blocked += 1;
            f(self);
            self.blocked -= 1;
            self.joined = saved;
        }

        /// Whether a compound index write reached HERE is recovered whole
        /// (lowers to `Node::IndexWrite` and widens its receiver): only
        /// when its scope reaches a join that drops the slot narrowing
        /// (`joined`) without being discarded first (`blocked`).
        fn index_write_joined(&self) -> bool {
            self.joined > 0 && self.blocked == 0
        }

        /// One rescue arm's non-chain children — exceptions, `rescue => x`
        /// reference, statements. The caller iterates `subsequent` itself so
        /// each arm can carry its own live/blocked mark.
        fn visit_rescue_arm_parts(&mut self, arm: &ruby_prism::RescueNode<'pr>) {
            for exception in arm.exceptions().iter() {
                self.visit(&exception);
            }
            if let Some(reference) = arm.reference() {
                self.visit(&reference);
            }
            if let Some(statements) = arm.statements() {
                self.visit(&statements.as_node());
            }
        }

        /// The `when`/`in` arm list of a `case`/`case in`, per
        /// `join_case_branch_scopes`: a terminated arm contributes no scope
        /// (blocked); a sole live arm returns unjoined (inherits the outer
        /// mark); two-or-more live arms join (statements under the mark,
        /// conditions/pattern/guard inheriting — they shape-narrow only).
        /// An absent `else` still counts the no-match arm as live; the
        /// all-dead fallback joins every arm anyway.
        fn visit_case_arms(
            &mut self,
            arms: &[PrismNode<'pr>],
            else_node: Option<&PrismNode<'pr>>,
        ) {
            let else_dead = else_node.is_some_and(|e| case_arm_exits(e));
            let live = arms.iter().filter(|a| !case_arm_exits(a)).count()
                + usize::from(else_node.is_none() || !else_dead);
            for arm in arms {
                if case_arm_exits(arm) {
                    if live == 0 {
                        self.under_join(|c| c.visit(arm));
                    } else {
                        self.blocked_subtree(|c| c.visit(arm));
                    }
                } else {
                    self.visit_case_arm_parts(arm, live >= 2);
                }
            }
            if let Some(e) = else_node {
                if else_dead {
                    if live == 0 {
                        self.under_join(|c| c.visit(e));
                    } else {
                        self.blocked_subtree(|c| c.visit(e));
                    }
                } else {
                    self.visit_case_arm_parts(e, live >= 2);
                }
            }
        }

        /// One `when`/`in`/`else` arm: the conditions, pattern and guard are
        /// shape-narrowed only (`Narrowing.case_when_scopes` pattern-matches
        /// them; `eval_when_or_in` evals `node.statements` alone), so a write
        /// inside never applies scope effects — blocked in every case.
        /// `join_body` decides whether the statements' scope is joined.
        fn visit_case_arm_parts(&mut self, arm: &PrismNode<'pr>, join_body: bool) {
            let visit_body = |c: &mut Self, s: ruby_prism::Node<'pr>| {
                if join_body {
                    c.under_join(|c2| c2.visit(&s));
                } else {
                    c.visit(&s);
                }
            };
            if let Some(when) = arm.as_when_node() {
                for condition in when.conditions().iter() {
                    self.blocked_subtree(|c| c.visit(&condition));
                }
                if let Some(statements) = when.statements() {
                    visit_body(self, statements.as_node());
                }
                return;
            }
            if let Some(in_node) = arm.as_in_node() {
                // Prism 1.9 folds `in P if G` into the pattern as an `IfNode`
                // wrapper, so the guard is visited with the pattern — both
                // blocked (shape-narrowed, never scope-evaluated).
                self.blocked_subtree(|c| c.visit(&in_node.pattern()));
                if let Some(statements) = in_node.statements() {
                    visit_body(self, statements.as_node());
                }
                return;
            }
            if let Some(else_node) = arm.as_else_node() {
                if let Some(statements) = else_node.statements() {
                    visit_body(self, statements.as_node());
                }
            }
        }
    }
    // Override each recoverable node type to RECORD it and stop (do not recurse —
    // `lower_node` will recurse into it once, normally). Every OTHER node type
    // keeps the trait's default recursion, so we descend through the unhandled
    // wrapper(s) until we reach the outermost recoverable nodes. This guarantees
    // each recoverable node is collected exactly once (no double-lowering, which
    // for a call would otherwise mint a duplicate diagnostic).
    impl<'pr> Visit<'pr> for Collector<'_, 'pr> {
        // A `BlockNode` / `LambdaNode` buried under the wrapper is CROSSED, not
        // recorded: the calls inside stay reachable, but the closure's `locals`
        // shadow the enclosing scope for everything under it (rigor-rs#137 —
        // `super { |o| o + 1 }` reads `o` as the parameter, not an outer
        // local). The names accumulate on `self.bound` until the block exits.
        // The body is also a JOINED position for an outer local's compound
        // index write: the reference threads the body's result scope into
        // `block_writebacks`, which widens a mutated receiver without keeping
        // its slot narrowing (rigor-rs#312).
        fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
            let mark = self.bound.len();
            self.bound.extend(
                node.locals()
                    .iter()
                    .map(|name| String::from_utf8_lossy(name.as_slice()).into_owned()),
            );
            self.joined += 1;
            ruby_prism::visit_block_node(self, node);
            self.joined -= 1;
            self.bound.truncate(mark);
        }
        fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
            let mark = self.bound.len();
            self.bound.extend(
                node.locals()
                    .iter()
                    .map(|name| String::from_utf8_lossy(name.as_slice()).into_owned()),
            );
            self.joined += 1;
            ruby_prism::visit_lambda_node(self, node);
            self.joined -= 1;
            self.bound.truncate(mark);
        }
        fn visit_local_variable_read_node(
            &mut self,
            node: &ruby_prism::LocalVariableReadNode<'pr>,
        ) {
            self.push(node.as_node());
        }
        fn visit_local_variable_write_node(
            &mut self,
            node: &ruby_prism::LocalVariableWriteNode<'pr>,
        ) {
            self.push(node.as_node());
        }
        fn visit_local_variable_operator_write_node(
            &mut self,
            node: &ruby_prism::LocalVariableOperatorWriteNode<'pr>,
        ) {
            self.push(node.as_node());
        }
        fn visit_local_variable_and_write_node(
            &mut self,
            node: &ruby_prism::LocalVariableAndWriteNode<'pr>,
        ) {
            self.push(node.as_node());
        }
        fn visit_local_variable_or_write_node(
            &mut self,
            node: &ruby_prism::LocalVariableOrWriteNode<'pr>,
        ) {
            self.push(node.as_node());
        }
        // A compound index write (`h[k] ||= v` / `h[k] &&= v` / `h[k] op= v`)
        // is observably different under the port's shape-recording
        // `StatementsKind::Recovered` carrier: recording it whole materialises
        // a `Node::IndexWrite`, and its `[]=` mutation widens the receiver —
        // the slot then reads the widened binding. Where the reference keeps
        // the write's `h[k] -> stored` narrowing (`eval_index_or_write` →
        // `Scope#with_indexed_narrowing` — every position whose post-scope
        // reaches the read without passing a join) it still answers the
        // slot's constant: `puts(*[h[:a] ||= 1])` fires
        // `call.undefined-method` for `1` on the oracle. Recording there is
        // unsound, so the walk DESCENDS instead — the write contributes its
        // operand reads, no widening — which keeps the stored slot exactly
        // as the kept narrowing leaves it. Only where the post-scope passes
        // a join that intersects the narrowing away (`index_write_joined`)
        // is the write recovered whole (rigor-rs#312).
        fn visit_index_or_write_node(
            &mut self,
            node: &ruby_prism::IndexOrWriteNode<'pr>,
        ) {
            if self.index_write_joined() {
                self.push(node.as_node());
            } else {
                ruby_prism::visit_index_or_write_node(self, node);
            }
        }
        fn visit_index_and_write_node(
            &mut self,
            node: &ruby_prism::IndexAndWriteNode<'pr>,
        ) {
            if self.index_write_joined() {
                self.push(node.as_node());
            } else {
                ruby_prism::visit_index_and_write_node(self, node);
            }
        }
        fn visit_index_operator_write_node(
            &mut self,
            node: &ruby_prism::IndexOperatorWriteNode<'pr>,
        ) {
            if self.index_write_joined() {
                self.push(node.as_node());
            } else {
                ruby_prism::visit_index_operator_write_node(self, node);
            }
        }
        // Scope-joining constructs crossed under a wrapper, granular to match
        // `statement_evaluator.rb`. `eval_rescue_modifier` joins the
        // expression scope with the arm's — but when the arm unconditionally
        // exits (`branch_unconditionally_exits?` →
        // `[type, after_expression]`) the arm's scope is DISCARDED and the
        // expression's survives unjoined.
        fn visit_rescue_modifier_node(
            &mut self,
            node: &ruby_prism::RescueModifierNode<'pr>,
        ) {
            if unconditional_exit(&node.rescue_expression()) {
                self.visit(&node.expression());
                self.blocked_subtree(|c| c.visit(&node.rescue_expression()));
            } else {
                self.under_join(|c| ruby_prism::visit_rescue_modifier_node(c, node));
            }
        }
        // `and_or_with_edges`: the LEFT operand's scope feeds the right and
        // the final join — a left-side narrowing survives, so the left
        // inherits the outer mark. The right joins — unless it terminates
        // (`branch_terminates?` → the skipped edge alone returns, the right's
        // scope discarded).
        fn visit_and_node(&mut self, node: &ruby_prism::AndNode<'pr>) {
            self.visit(&node.left());
            if unconditional_exit(&node.right()) {
                self.blocked_subtree(|c| c.visit(&node.right()));
            } else {
                self.under_join(|c| c.visit(&node.right()));
            }
        }
        fn visit_or_node(&mut self, node: &ruby_prism::OrNode<'pr>) {
            self.visit(&node.left());
            if unconditional_exit(&node.right()) {
                self.blocked_subtree(|c| c.visit(&node.right()));
            } else {
                self.under_join(|c| c.visit(&node.right()));
            }
        }
        // `eval_if`: the predicate evaluates straight-line (both edges
        // inherit its narrowing). A constant-folded predicate
        // (`branch_certainty` → `live_branch_for_if`) evaluates ONLY the
        // live arm straight-line — the dead arm's scope never exists.
        // Otherwise both branches join — except the two early returns:
        // a terminated then-arm with no `else` (`→ falsey_scope`), and a
        // terminated else with a then (`→ then_scope`). Each discards one
        // arm's scope while the other survives UNJOINED.
        fn visit_if_node(&mut self, node: &ruby_prism::IfNode<'pr>) {
            self.visit(&node.predicate());
            let statements = node.statements().map(|s| s.as_node());
            let subsequent = node.subsequent();
            match literal_truthiness(&node.predicate()) {
                Some(true) => {
                    if let Some(s) = &statements {
                        self.visit(s);
                    }
                    if let Some(s) = &subsequent {
                        self.blocked_subtree(|c| c.visit(s));
                    }
                }
                Some(false) => {
                    if let Some(s) = &statements {
                        self.blocked_subtree(|c| c.visit(s));
                    }
                    if let Some(s) = &subsequent {
                        self.visit(s);
                    }
                }
                None => {
                    let then_dropped = statements
                        .as_ref()
                        .is_some_and(|s| unconditional_exit(s))
                        && subsequent.is_none();
                    let else_dropped = subsequent
                        .as_ref()
                        .is_some_and(|s| unconditional_exit(s))
                        && statements.is_some();
                    if then_dropped {
                        if let Some(s) = &statements {
                            self.blocked_subtree(|c| c.visit(s));
                        }
                    } else if else_dropped {
                        if let Some(s) = &statements {
                            self.visit(s);
                        }
                        if let Some(s) = &subsequent {
                            self.blocked_subtree(|c| c.visit(s));
                        }
                    } else {
                        self.under_join(|c| {
                            if let Some(s) = &statements {
                                c.visit(s);
                            }
                            if let Some(s) = &subsequent {
                                c.visit(s);
                            }
                        });
                    }
                }
            }
        }
        // `eval_unless` — same early returns, truthy/falsey edges swapped.
        fn visit_unless_node(&mut self, node: &ruby_prism::UnlessNode<'pr>) {
            self.visit(&node.predicate());
            let statements = node.statements().map(|s| s.as_node());
            let else_clause = node.else_clause().map(|e| e.as_node());
            match literal_truthiness(&node.predicate()) {
                // `unless` runs its body when the predicate is FALSEY.
                Some(false) => {
                    if let Some(s) = &statements {
                        self.visit(s);
                    }
                    if let Some(s) = &else_clause {
                        self.blocked_subtree(|c| c.visit(s));
                    }
                }
                Some(true) => {
                    if let Some(s) = &statements {
                        self.blocked_subtree(|c| c.visit(s));
                    }
                    if let Some(s) = &else_clause {
                        self.visit(s);
                    }
                }
                None => {
                    let body_dropped = statements
                        .as_ref()
                        .is_some_and(|s| unconditional_exit(s))
                        && else_clause.is_none();
                    let else_dropped = else_clause
                        .as_ref()
                        .is_some_and(|s| unconditional_exit(s))
                        && statements.is_some();
                    if body_dropped {
                        if let Some(s) = &statements {
                            self.blocked_subtree(|c| c.visit(s));
                        }
                    } else if else_dropped {
                        if let Some(s) = &statements {
                            self.visit(s);
                        }
                        if let Some(s) = &else_clause {
                            self.blocked_subtree(|c| c.visit(s));
                        }
                    } else {
                        self.under_join(|c| {
                            if let Some(s) = &statements {
                                c.visit(s);
                            }
                            if let Some(s) = &else_clause {
                                c.visit(s);
                            }
                        });
                    }
                }
            }
        }
        // `eval_loop`: the predicate's narrowing lands in `post_pred`, which
        // both join members derive from — it survives (predicate inherits).
        // The body's scope joins the predicate's — its narrowings die.
        fn visit_while_node(&mut self, node: &ruby_prism::WhileNode<'pr>) {
            self.visit(&node.predicate());
            self.under_join(|c| {
                if let Some(statements) = node.statements() {
                    c.visit(&statements.as_node());
                }
            });
        }
        fn visit_until_node(&mut self, node: &ruby_prism::UntilNode<'pr>) {
            self.visit(&node.predicate());
            self.under_join(|c| {
                if let Some(statements) = node.statements() {
                    c.visit(&statements.as_node());
                }
            });
        }
        fn visit_for_node(&mut self, node: &ruby_prism::ForNode<'pr>) {
            self.visit(&node.collection());
            self.under_join(|c| {
                c.visit(&node.index());
                if let Some(statements) = node.statements() {
                    c.visit(&statements.as_node());
                }
            });
        }
        // `eval_case`: the subject evaluates once, straight-line. Each arm's
        // statements feed `join_case_branch_scopes` — an arm that terminates
        // contributes NO scope (blocked), a sole live arm returns unjoined
        // (inherits), two-or-more live arms join. Conditions / patterns /
        // guards shape-narrow only (`Narrowing.case_when_scopes` — no scope
        // threading) so they inherit the outer mark, never the arm's.
        fn visit_case_node(&mut self, node: &ruby_prism::CaseNode<'pr>) {
            if let Some(predicate) = node.predicate() {
                self.visit(&predicate);
            }
            let arms: Vec<PrismNode<'pr>> = node.conditions().iter().collect();
            let else_node = node.else_clause().map(|e| e.as_node());
            self.visit_case_arms(&arms, else_node.as_ref());
        }
        fn visit_case_match_node(&mut self, node: &ruby_prism::CaseMatchNode<'pr>) {
            if let Some(predicate) = node.predicate() {
                self.visit(&predicate);
            }
            let arms: Vec<PrismNode<'pr>> = node.conditions().iter().collect();
            let else_node = node.else_clause().map(|e| e.as_node());
            self.visit_case_arms(&arms, else_node.as_ref());
        }
        // `eval_begin`: body + rescue arms + else are alternative exit paths
        // joined by `reduce_scopes_with_nil_injection` — but a rescue arm
        // that terminates is filtered out (`live_rescue_results` →
        // `branch_terminates?`), and when NO arm lives the primary scope
        // returns unjoined (`live_rescues.empty?` fast path). `ensure` is
        // `sub_eval`'d straight-line ONTO the joined exit scope, so its own
        // effects never pass through the join — it inherits the outer mark.
        fn visit_begin_node(&mut self, node: &ruby_prism::BeginNode<'pr>) {
            if node.rescue_clause().is_none() {
                ruby_prism::visit_begin_node(self, node);
                return;
            }
            let arm_dead = |r: &ruby_prism::RescueNode<'pr>| {
                r.statements()
                    .is_some_and(|s| unconditional_exit(&s.as_node()))
            };
            let mut live = 0usize;
            let mut cur = node.rescue_clause();
            while let Some(r) = cur {
                live += usize::from(!arm_dead(&r));
                cur = r.subsequent();
            }
            if live == 0 {
                ruby_prism::visit_begin_node(self, node);
                return;
            }
            if let Some(statements) = node.statements() {
                self.under_join(|c| c.visit(&statements.as_node()));
            }
            let mut cur = node.rescue_clause();
            while let Some(r) = cur {
                if arm_dead(&r) {
                    self.blocked_subtree(|c| c.visit_rescue_arm_parts(&r));
                } else {
                    self.under_join(|c| c.visit_rescue_arm_parts(&r));
                }
                cur = r.subsequent();
            }
            if let Some(else_clause) = node.else_clause() {
                self.under_join(|c| c.visit(&else_clause.as_node()));
            }
            if let Some(ensure_clause) = node.ensure_clause() {
                self.visit(&ensure_clause.as_node());
            }
        }
        // `super` / `yield` / `BEGIN` / `END` have no evaluator in the
        // reference — the operand never runs, so a write inside can neither
        // widen nor narrow an enclosing local (operand reads are still
        // recovered — `flow.dead-assignment` counts them).
        fn visit_super_node(&mut self, node: &ruby_prism::SuperNode<'pr>) {
            self.blocked_subtree(|c| ruby_prism::visit_super_node(c, node));
        }
        fn visit_forwarding_super_node(
            &mut self,
            node: &ruby_prism::ForwardingSuperNode<'pr>,
        ) {
            self.blocked_subtree(|c| ruby_prism::visit_forwarding_super_node(c, node));
        }
        fn visit_yield_node(&mut self, node: &ruby_prism::YieldNode<'pr>) {
            self.blocked_subtree(|c| ruby_prism::visit_yield_node(c, node));
        }
        fn visit_pre_execution_node(
            &mut self,
            node: &ruby_prism::PreExecutionNode<'pr>,
        ) {
            self.blocked_subtree(|c| ruby_prism::visit_pre_execution_node(c, node));
        }
        fn visit_post_execution_node(
            &mut self,
            node: &ruby_prism::PostExecutionNode<'pr>,
        ) {
            self.blocked_subtree(|c| ruby_prism::visit_post_execution_node(c, node));
        }
        // `def` / `class` / `module` / `class <<` open a fresh local scope:
        // a compound index write crossed inside mutates an INNER local, so
        // the enclosing scope's marks block — descending keeps the write's
        // operand reads reachable without inventing a `Node::IndexWrite`
        // that would widen a same-named outer local.
        fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
            self.blocked_subtree(|c| ruby_prism::visit_def_node(c, node));
        }
        fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
            self.blocked_subtree(|c| ruby_prism::visit_class_node(c, node));
        }
        fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
            self.blocked_subtree(|c| ruby_prism::visit_module_node(c, node));
        }
        fn visit_singleton_class_node(
            &mut self,
            node: &ruby_prism::SingletonClassNode<'pr>,
        ) {
            self.blocked_subtree(|c| ruby_prism::visit_singleton_class_node(c, node));
        }
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if self.suppress_calls {
                // Under a `defined?` operand: not reachable code, so never
                // recorded — but keep descending, because a local READ inside the
                // call's receiver/arguments still counts as a read.
                ruby_prism::visit_call_node(self, node);
                return;
            }
            self.push(node.as_node());
        }
        // A `defined?` buried under an unhandled wrapper (`super(defined?(x))`)
        // is recovered WHOLE, so its own lowering applies the call suppression
        // instead of this walk recovering the operand's calls as live code.
        fn visit_defined_node(&mut self, node: &ruby_prism::DefinedNode<'pr>) {
            self.push(node.as_node());
        }
        // A multi-write buried under an unhandled wrapper is recovered WHOLE
        // (its own lowering re-lowers the RHS), so its target names still reach
        // `collect_flow_writes`. Without this the wrapper's default recursion
        // would recover only the RHS's reads/calls and drop the LHS names again.
        fn visit_multi_write_node(&mut self, node: &ruby_prism::MultiWriteNode<'pr>) {
            self.push(node.as_node());
        }
        // A jump buried under an unhandled wrapper (`raise "x" rescue break
        // "s"` — a RescueModifierNode has no owned variant) is recovered WHOLE
        // too: its own lowering emits the `Jump`/`Other{jump}` carrier the
        // exactly-once block-timing proof (rigor-rs#140) scans for, and the
        // reference's block-level jump scan descends the same wrappers —
        // `JUMP_BOUNDARY_NODES` prunes only nested blocks/lambdas/defs/loops.
        // Recovered here only means the node exists in the arena; the
        // `Recovered` carrier still keeps its order and reachability unknown.
        fn visit_break_node(&mut self, node: &ruby_prism::BreakNode<'pr>) {
            self.push(node.as_node());
        }
        fn visit_next_node(&mut self, node: &ruby_prism::NextNode<'pr>) {
            self.push(node.as_node());
        }
        fn visit_redo_node(&mut self, node: &ruby_prism::RedoNode<'pr>) {
            self.push(node.as_node());
        }
        fn visit_retry_node(&mut self, node: &ruby_prism::RetryNode<'pr>) {
            self.push(node.as_node());
        }
    }
    let mut out = Vec::new();
    let mut c = Collector {
        out: &mut out,
        suppress_calls,
        bound: Vec::new(),
        joined: u32::from(joined),
        blocked: u32::from(blocked),
    };
    // Visit the wrapper's CHILDREN (not the wrapper itself), so we don't re-handle
    // the unhandled root. The default `visit` dispatches the root to its own
    // (non-overridden) per-type method, which recurses into children — exactly
    // what we want for the root wrapper.
    c.visit(node);
    out
}

/// Prism-level analog of the reference's `branch_unconditionally_exits?`
/// (statement_evaluator.rb): SYNTACTIC exits only — a `return`/`next`/
/// `break`, a receiverless `raise`/`throw`/`exit`/`abort`/`fail` call, a
/// statement list whose final node exits, parentheses whose body exits, or
/// an `if`/`unless` whose arms BOTH exit. The reference's other verdict
/// half — `branch_terminates?`'s `Type::Bot` test — needs inferred types
/// the collect walk does not have, so a branch ending in an always-raising
/// RESOLVED call reads as live here (the descending/gap side, never an FP
/// source). `redo`/`retry` fold in for free: their eval type is `Bot`, so
/// the reference reaches the same verdict through that half.
fn unconditional_exit(node: &PrismNode<'_>) -> bool {
    if node.as_return_node().is_some()
        || node.as_next_node().is_some()
        || node.as_break_node().is_some()
        || node.as_redo_node().is_some()
        || node.as_retry_node().is_some()
    {
        return true;
    }
    if let Some(call) = node.as_call_node() {
        return call.receiver().is_none()
            && matches!(
                call.name().as_slice(),
                b"raise" | b"throw" | b"exit" | b"abort" | b"fail"
            );
    }
    if let Some(stmts) = node.as_statements_node() {
        return stmts
            .body()
            .iter()
            .last()
            .is_some_and(|last| unconditional_exit(&last));
    }
    // An `else` clause exits exactly when its body does — the reference
    // tests the clause's eval TYPE (`Bot` iff the body exits) since
    // `branch_unconditionally_exits?` has no `ElseNode` arm.
    if let Some(else_node) = node.as_else_node() {
        return else_node
            .statements()
            .is_some_and(|s| unconditional_exit(&s.as_node()));
    }
    if let Some(paren) = node.as_parentheses_node() {
        return paren.body().is_some_and(|b| unconditional_exit(&b));
    }
    if let Some(if_node) = node.as_if_node() {
        return if_node
            .statements()
            .is_some_and(|s| unconditional_exit(&s.as_node()))
            && if_node
                .subsequent()
                .is_some_and(|s| unconditional_exit(&s));
    }
    if let Some(unless_node) = node.as_unless_node() {
        return unless_node
            .statements()
            .is_some_and(|s| unconditional_exit(&s.as_node()))
            && unless_node
                .else_clause()
                .is_some_and(|e| unconditional_exit(&e.as_node()));
    }
    false
}

/// Syntactic constant-truthiness — the literal subset of the reference's
/// `branch_certainty`/`predicate_certainty` (statement_evaluator.rb): the
/// join folds the dead arm away when the predicate type is provably
/// truthy/falsey. Anything needing an inferred type is `None` here —
/// including non-falsey nominal carriers the reference still folds (a
/// coverage-gap limitation, not an FP source).
fn literal_truthiness(node: &PrismNode<'_>) -> Option<bool> {
    if node.as_true_node().is_some() {
        return Some(true);
    }
    if node.as_false_node().is_some() || node.as_nil_node().is_some() {
        return Some(false);
    }
    if let Some(paren) = node.as_parentheses_node() {
        return paren.body().and_then(|b| literal_truthiness(&b));
    }
    // `!lit` flips a known literal (`!`/`not` lower to a `!` call).
    if let Some(call) = node.as_call_node() {
        if call.name().as_slice() == b"!" && call.arguments().is_none() {
            if let Some(receiver) = call.receiver() {
                return literal_truthiness(&receiver).map(|v| !v);
            }
        }
        return None;
    }
    // Every other literal is non-nil and non-false — always truthy.
    if node.as_integer_node().is_some()
        || node.as_float_node().is_some()
        || node.as_rational_node().is_some()
        || node.as_imaginary_node().is_some()
        || node.as_string_node().is_some()
        || node.as_interpolated_string_node().is_some()
        || node.as_symbol_node().is_some()
        || node.as_interpolated_symbol_node().is_some()
        || node.as_x_string_node().is_some()
        || node.as_interpolated_x_string_node().is_some()
        || node.as_array_node().is_some()
        || node.as_hash_node().is_some()
        || node.as_range_node().is_some()
        || node.as_regular_expression_node().is_some()
        || node.as_interpolated_regular_expression_node().is_some()
        || node.as_lambda_node().is_some()
    {
        return Some(true);
    }
    None
}

/// Whether a `case`/`case in` arm's evaluated body unconditionally exits —
/// `eval_when_or_in` returns `sub_eval(node.statements)`, whose type is
/// `Bot` exactly when the body exits (the `branch_terminates?` test
/// `join_case_branch_scopes` applies to each arm).
fn case_arm_exits(arm: &PrismNode<'_>) -> bool {
    let statements = if let Some(when) = arm.as_when_node() {
        when.statements().map(|s| s.as_node())
    } else if let Some(in_node) = arm.as_in_node() {
        in_node.statements().map(|s| s.as_node())
    } else if let Some(else_node) = arm.as_else_node() {
        else_node.statements().map(|s| s.as_node())
    } else {
        None
    };
    statements.is_some_and(|s| unconditional_exit(&s))
}
