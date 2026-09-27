//! The recovered-children walk behind the `Statements` carriers of Prism nodes
//! that have no owned variant, and its call-suppressing `defined?` variant.

use crate::ruby_prism::{self, Node as PrismNode};

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
pub(crate) fn collect_recoverable_children<'pr>(node: &PrismNode<'pr>) -> Vec<PrismNode<'pr>> {
    collect_recoverable(node, false)
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
pub(crate) fn collect_defined_operand_children<'pr>(node: &PrismNode<'pr>) -> Vec<PrismNode<'pr>> {
    collect_recoverable(node, true)
}

fn collect_recoverable<'pr>(node: &PrismNode<'pr>, suppress_calls: bool) -> Vec<PrismNode<'pr>> {
    use ruby_prism::Visit;
    struct Collector<'a, 'pr> {
        out: &'a mut Vec<PrismNode<'pr>>,
        suppress_calls: bool,
    }
    // Override each recoverable node type to RECORD it and stop (do not recurse —
    // `lower_node` will recurse into it once, normally). Every OTHER node type
    // keeps the trait's default recursion, so we descend through the unhandled
    // wrapper(s) until we reach the outermost recoverable nodes. This guarantees
    // each recoverable node is collected exactly once (no double-lowering, which
    // for a call would otherwise mint a duplicate diagnostic).
    impl<'pr> Visit<'pr> for Collector<'_, 'pr> {
        fn visit_local_variable_read_node(
            &mut self,
            node: &ruby_prism::LocalVariableReadNode<'pr>,
        ) {
            self.out.push(node.as_node());
        }
        fn visit_local_variable_write_node(
            &mut self,
            node: &ruby_prism::LocalVariableWriteNode<'pr>,
        ) {
            self.out.push(node.as_node());
        }
        fn visit_local_variable_operator_write_node(
            &mut self,
            node: &ruby_prism::LocalVariableOperatorWriteNode<'pr>,
        ) {
            self.out.push(node.as_node());
        }
        fn visit_local_variable_and_write_node(
            &mut self,
            node: &ruby_prism::LocalVariableAndWriteNode<'pr>,
        ) {
            self.out.push(node.as_node());
        }
        fn visit_local_variable_or_write_node(
            &mut self,
            node: &ruby_prism::LocalVariableOrWriteNode<'pr>,
        ) {
            self.out.push(node.as_node());
        }
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if self.suppress_calls {
                // Under a `defined?` operand: not reachable code, so never
                // recorded — but keep descending, because a local READ inside the
                // call's receiver/arguments still counts as a read.
                ruby_prism::visit_call_node(self, node);
                return;
            }
            self.out.push(node.as_node());
        }
        // A `defined?` buried under an unhandled wrapper (`super(defined?(x))`)
        // is recovered WHOLE, so its own lowering applies the call suppression
        // instead of this walk recovering the operand's calls as live code.
        fn visit_defined_node(&mut self, node: &ruby_prism::DefinedNode<'pr>) {
            self.out.push(node.as_node());
        }
        // A multi-write buried under an unhandled wrapper is recovered WHOLE
        // (its own lowering re-lowers the RHS), so its target names still reach
        // `collect_flow_writes`. Without this the wrapper's default recursion
        // would recover only the RHS's reads/calls and drop the LHS names again.
        fn visit_multi_write_node(&mut self, node: &ruby_prism::MultiWriteNode<'pr>) {
            self.out.push(node.as_node());
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
            self.out.push(node.as_node());
        }
        fn visit_next_node(&mut self, node: &ruby_prism::NextNode<'pr>) {
            self.out.push(node.as_node());
        }
        fn visit_redo_node(&mut self, node: &ruby_prism::RedoNode<'pr>) {
            self.out.push(node.as_node());
        }
        fn visit_retry_node(&mut self, node: &ruby_prism::RetryNode<'pr>) {
            self.out.push(node.as_node());
        }
    }
    let mut out = Vec::new();
    let mut c = Collector { out: &mut out, suppress_calls };
    // Visit the wrapper's CHILDREN (not the wrapper itself), so we don't re-handle
    // the unhandled root. The default `visit` dispatches the root to its own
    // (non-overridden) per-type method, which recurses into children — exactly
    // what we want for the root wrapper.
    c.visit(node);
    out
}
