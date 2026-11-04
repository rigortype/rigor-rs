//! Top-level `def`s (reference `Generator#collect_def_node`, #1676): a plain
//! `def` outside every `class` / `module` defines a private `Object` method.
//! sig-gen writes nothing for it but reports it as skipped
//! (`sig.skipped.top-level-def`), so a file of top-level methods is not taken
//! for one that was never read.
//!
//! The reference reports a `def` with no receiver, at an empty namespace
//! prefix, outside a `class << …` body and outside every block or lambda
//! (`@block_depth.zero?`). It does not descend into a `def` body. A
//! `def self.x` and a block-body `def` stay unreported.

use rigor_parse::ruby_prism::{
    BlockNode, ClassNode, DefNode, LambdaNode, ModuleNode, ParseResult, SingletonClassNode, Visit,
};

/// Every reported top-level `def` in the file: `(name, start offset)`, in
/// source order.
pub fn collect(result: &ParseResult<'_>) -> Vec<(String, usize)> {
    let mut walker = Walker { out: Vec::new() };
    walker.visit(&result.node());
    walker.out
}

struct Walker {
    out: Vec<(String, usize)>,
}

impl<'pr> Visit<'pr> for Walker {
    fn visit_def_node(&mut self, node: &DefNode<'pr>) {
        if node.receiver().is_none() {
            let name = String::from_utf8_lossy(node.name().as_slice()).into_owned();
            self.out.push((name, node.location().start_offset()));
        }
    }

    // A `class` / `module` body is walked at a non-empty prefix: a header
    // with an expression base (`class foo::X`) still declares `X`
    // (`declaration_prefix`), and a declined `self::X` body is not walked.
    fn visit_class_node(&mut self, _node: &ClassNode<'pr>) {}

    fn visit_module_node(&mut self, _node: &ModuleNode<'pr>) {}

    // Every `def` below these is in a singleton-class body or at a non-zero
    // block depth, so none is reported.
    fn visit_singleton_class_node(&mut self, _node: &SingletonClassNode<'pr>) {}

    fn visit_block_node(&mut self, _node: &BlockNode<'pr>) {}

    fn visit_lambda_node(&mut self, _node: &LambdaNode<'pr>) {}
}
