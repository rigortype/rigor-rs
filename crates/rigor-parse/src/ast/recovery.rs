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
    /// `eval_begin`), and the join intersects away per-slot facts like
    /// the `h[k] -> stored` narrowing `eval_index_or_write` records —
    /// while the write's `[]=` receiver widening still applies. Nested
    /// recovery during this node's lowering inherits the mark
    /// (rigor-rs#312).
    pub(crate) joined: bool,
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
) -> Vec<Recovered<'pr>> {
    collect_recoverable(node, false, joined)
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
    collect_recoverable(node, true, joined)
}

fn collect_recoverable<'pr>(
    node: &PrismNode<'pr>,
    suppress_calls: bool,
    joined: bool,
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
    }
    impl<'pr> Collector<'_, 'pr> {
        fn push(&mut self, node: PrismNode<'pr>) {
            self.out.push(Recovered {
                node,
                bound: self.bound.clone(),
                joined: self.joined > 0,
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

        /// Visit `f` with the join mark cleared: `f` opens a fresh local
        /// scope, where a compound index write mutates an INNER local and
        /// the enclosing scope's join semantics no longer apply.
        fn fresh_scope(&mut self, f: impl FnOnce(&mut Self)) {
            let saved = std::mem::replace(&mut self.joined, 0);
            f(self);
            self.joined = saved;
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
        // the write's `h[k] -> stored` narrowing (every operand-transparent
        // position — `Scope` join would discard it, but a straight-line
        // operand never passes through one) it still answers the slot's
        // constant: `puts(*[h[:a] ||= 1])` fires `call.undefined-method` for
        // `1` on the oracle. Recording there is unsound, so the walk DESCENDS
        // instead — the write contributes its operand reads, no widening —
        // which keeps the stored slot exactly as the kept narrowing leaves
        // it. Only under a scope-joining construct, where the narrowing dies
        // at the join while the `[]=` widening survives, is the write
        // recovered whole (rigor-rs#312).
        fn visit_index_or_write_node(
            &mut self,
            node: &ruby_prism::IndexOrWriteNode<'pr>,
        ) {
            if self.joined > 0 {
                self.push(node.as_node());
            } else {
                ruby_prism::visit_index_or_write_node(self, node);
            }
        }
        fn visit_index_and_write_node(
            &mut self,
            node: &ruby_prism::IndexAndWriteNode<'pr>,
        ) {
            if self.joined > 0 {
                self.push(node.as_node());
            } else {
                ruby_prism::visit_index_and_write_node(self, node);
            }
        }
        fn visit_index_operator_write_node(
            &mut self,
            node: &ruby_prism::IndexOperatorWriteNode<'pr>,
        ) {
            if self.joined > 0 {
                self.push(node.as_node());
            } else {
                ruby_prism::visit_index_operator_write_node(self, node);
            }
        }
        // Scope-joining constructs crossed under a wrapper — the positions
        // where the reference threads the joined child's scope through a
        // join, so a compound index write's slot narrowing is intersected
        // away while its receiver widening survives. The granularity follows
        // `statement_evaluator.rb`: `rescue` joins its expression arm with
        // the rescue arm (`eval_rescue_modifier`); `&&`/`||` join only the
        // right operand (`eval_and_or` keeps the left's facts — the left's
        // post-scope feeds the right); an `if`/`unless`/`while`/`until`
        // predicate and a `case` subject evaluate straight-line (both branch
        // scopes inherit the predicate's narrowing, so it survives their
        // join — measured: `x = (if h[:a] ||= 1 …)` and
        // `x = case (h[:a] ||= 1) …` still fire `for 1`); a `for`
        // collection evaluates once (the join is over iterations of the
        // index/body); a `begin`'s body joins only when a rescue clause is
        // present (`eval_begin`'s `live_rescues.empty?` fast path returns
        // the primary scope unchanged — measured: `x = begin h[:a] ||= 1
        // end` fires, `x = begin h[:a] ||= 1 rescue nil end` is silent); a
        // crossed block/lambda body writes mutated outer locals back through
        // the `block_writebacks` sink, which drops each slot's narrowing.
        fn visit_rescue_modifier_node(
            &mut self,
            node: &ruby_prism::RescueModifierNode<'pr>,
        ) {
            self.under_join(|c| ruby_prism::visit_rescue_modifier_node(c, node));
        }
        fn visit_and_node(&mut self, node: &ruby_prism::AndNode<'pr>) {
            self.visit(&node.left());
            self.under_join(|c| c.visit(&node.right()));
        }
        fn visit_or_node(&mut self, node: &ruby_prism::OrNode<'pr>) {
            self.visit(&node.left());
            self.under_join(|c| c.visit(&node.right()));
        }
        fn visit_if_node(&mut self, node: &ruby_prism::IfNode<'pr>) {
            self.visit(&node.predicate());
            self.under_join(|c| {
                if let Some(statements) = node.statements() {
                    c.visit(&statements.as_node());
                }
                if let Some(subsequent) = node.subsequent() {
                    c.visit(&subsequent);
                }
            });
        }
        fn visit_unless_node(&mut self, node: &ruby_prism::UnlessNode<'pr>) {
            self.visit(&node.predicate());
            self.under_join(|c| {
                if let Some(statements) = node.statements() {
                    c.visit(&statements.as_node());
                }
                if let Some(else_clause) = node.else_clause() {
                    c.visit(&else_clause.as_node());
                }
            });
        }
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
        fn visit_case_node(&mut self, node: &ruby_prism::CaseNode<'pr>) {
            if let Some(predicate) = node.predicate() {
                self.visit(&predicate);
            }
            self.under_join(|c| {
                for condition in node.conditions().iter() {
                    c.visit(&condition);
                }
                if let Some(else_clause) = node.else_clause() {
                    c.visit(&else_clause.as_node());
                }
            });
        }
        fn visit_case_match_node(&mut self, node: &ruby_prism::CaseMatchNode<'pr>) {
            if let Some(predicate) = node.predicate() {
                self.visit(&predicate);
            }
            self.under_join(|c| {
                for condition in node.conditions().iter() {
                    c.visit(&condition);
                }
                if let Some(else_clause) = node.else_clause() {
                    c.visit(&else_clause.as_node());
                }
            });
        }
        fn visit_when_node(&mut self, node: &ruby_prism::WhenNode<'pr>) {
            self.under_join(|c| ruby_prism::visit_when_node(c, node));
        }
        fn visit_in_node(&mut self, node: &ruby_prism::InNode<'pr>) {
            self.under_join(|c| ruby_prism::visit_in_node(c, node));
        }
        fn visit_begin_node(&mut self, node: &ruby_prism::BeginNode<'pr>) {
            if node.rescue_clause().is_some() {
                self.under_join(|c| ruby_prism::visit_begin_node(c, node));
            } else {
                ruby_prism::visit_begin_node(self, node);
            }
        }
        // `def` / `class` / `module` / `class <<` open a fresh local scope: a
        // compound index write crossed inside mutates an INNER local, so the
        // enclosing scope's join mark resets — descending keeps the write's
        // operand reads reachable without inventing a `Node::IndexWrite` that
        // would widen a same-named outer local.
        fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
            self.fresh_scope(|c| ruby_prism::visit_def_node(c, node));
        }
        fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
            self.fresh_scope(|c| ruby_prism::visit_class_node(c, node));
        }
        fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
            self.fresh_scope(|c| ruby_prism::visit_module_node(c, node));
        }
        fn visit_singleton_class_node(
            &mut self,
            node: &ruby_prism::SingletonClassNode<'pr>,
        ) {
            self.fresh_scope(|c| ruby_prism::visit_singleton_class_node(c, node));
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
    };
    // Visit the wrapper's CHILDREN (not the wrapper itself), so we don't re-handle
    // the unhandled root. The default `visit` dispatches the root to its own
    // (non-overridden) per-type method, which recurses into children — exactly
    // what we want for the root wrapper.
    c.visit(node);
    out
}
