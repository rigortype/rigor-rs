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
    self, BlockNode, ClassNode, DefNode, LambdaNode, ModuleNode, Node as PrismNode, ParseResult,
    SingletonClassNode, Visit,
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

/// Whether a `class` / `module` header names a namespace the walk enters with
/// a non-empty prefix (`descend_into_namespace?`). A `self::X` header is
/// declined outright, and its body is not walked either. A header whose base
/// is an expression (`foo::X`) names nothing: the reference walks its
/// children at the enclosing (here: empty) prefix.
fn opens_namespace(path: &PrismNode<'_>) -> Option<bool> {
    if path.as_constant_read_node().is_some() {
        return Some(true);
    }
    let cp = path.as_constant_path_node()?;
    match cp.parent() {
        None => Some(true),
        Some(parent) if parent.as_self_node().is_some() => Some(true),
        Some(parent) => opens_namespace(&parent),
    }
}

impl<'pr> Visit<'pr> for Walker {
    fn visit_def_node(&mut self, node: &DefNode<'pr>) {
        if node.receiver().is_none() {
            let name = String::from_utf8_lossy(node.name().as_slice()).into_owned();
            self.out.push((name, node.location().start_offset()));
        }
    }

    fn visit_class_node(&mut self, node: &ClassNode<'pr>) {
        if opens_namespace(&node.constant_path()) != Some(true) {
            ruby_prism::visit_class_node(self, node);
        }
    }

    fn visit_module_node(&mut self, node: &ModuleNode<'pr>) {
        if opens_namespace(&node.constant_path()) != Some(true) {
            ruby_prism::visit_module_node(self, node);
        }
    }

    // Every `def` below these is in a singleton-class body or at a non-zero
    // block depth, so none is reported.
    fn visit_singleton_class_node(&mut self, _node: &SingletonClassNode<'pr>) {}

    fn visit_block_node(&mut self, _node: &BlockNode<'pr>) {}

    fn visit_lambda_node(&mut self, _node: &LambdaNode<'pr>) {}
}
