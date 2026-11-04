//! Upstream v0.4.6 (#2285 / issue #2262, `Inference::DeepErasure`) — the
//! block calls and `for` loops whose body mutates an element of the
//! collection in place, and the locals that collection is read from.
//!
//! A loop hands its collection's own elements to its body, so `a.each { |x|
//! x << 2 }` (or `for x in a; x << 2; end`) changes a shape `a`'s binding
//! still carries one level down. The reference drops every shape of the
//! receiver's variables at every depth (`DeepErasure.widen`); the port
//! records a content-mutation flow write for each such variable instead,
//! which widens the binding further — strictly fewer diagnostics.
//!
//! `element_mutation?(body, names)` (`deep_erasure.rb:57`): `names` is what
//! the block's parameters (or the `for` index) bind, plus every name a block
//! or `for` nested in the body binds. A site is a `SHAPE_MUTATORS` call, or
//! an index / attribute operator-write or target, whose receiver is rooted
//! (`element_rooted?`) at a read of one of `names`, of a numbered parameter
//! `_1`..`_9`, or of `it`. A block argument naming a mutator (`&:strip!`)
//! mutates every element too (`symbol_proc_mutation?`). The receiver's
//! variables are `ReceiverAlias.candidates` (locals only here: the port's
//! flow writes carry locals).

use crate::ruby_prism::{Node as PrismNode, Visit};

use super::constant_string;
use crate::mutators::is_shape_mutator;

type Span = (usize, usize);

/// `(span of the block call or the `for` node, locals its receiver or
/// collection may evaluate to)`, one entry per mutating construct.
pub(crate) fn collect(root: &PrismNode<'_>) -> Vec<(Span, Vec<String>)> {
    let mut w = Walk { out: Vec::new() };
    w.visit(root);
    w.out
}

struct Walk {
    out: Vec<(Span, Vec<String>)>,
}

fn span_of(n: &PrismNode<'_>) -> Span {
    let l = n.location();
    (l.start_offset(), l.end_offset())
}

/// Every node under `n`, `n` included.
fn descendants<'pr>(n: &PrismNode<'pr>) -> Vec<PrismNode<'pr>> {
    struct All<'pr>(Vec<PrismNode<'pr>>);
    impl<'pr> Visit<'pr> for All<'pr> {
        fn visit_branch_node_enter(&mut self, node: PrismNode<'pr>) {
            self.0.push(node);
        }
        fn visit_leaf_node_enter(&mut self, node: PrismNode<'pr>) {
            self.0.push(node);
        }
    }
    let mut all = All(Vec::new());
    all.visit(n);
    all.0
}

/// `parameter_names`: the names `ELEMENT_PARAMETER_NODES` under `params` bind.
fn parameter_names(params: Option<PrismNode<'_>>, out: &mut Vec<String>) {
    let Some(p) = params else { return };
    for n in descendants(&p) {
        let name = if let Some(r) = n.as_required_parameter_node() {
            Some(r.name())
        } else if let Some(o) = n.as_optional_parameter_node() {
            Some(o.name())
        } else if let Some(r) = n.as_rest_parameter_node() {
            r.name()
        } else if let Some(k) = n.as_keyword_rest_parameter_node() {
            k.name()
        } else {
            n.as_local_variable_target_node().map(|t| t.name())
        };
        if let Some(name) = name {
            let s = constant_string(name.as_slice());
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
}

/// `element_mutation?(body, names)`.
fn element_mutation(body: Option<PrismNode<'_>>, mut names: Vec<String>) -> bool {
    let Some(body) = body else { return false };
    let all = descendants(&body);
    // `nested_parameter_names`.
    for n in &all {
        if let Some(b) = n.as_block_node() {
            parameter_names(b.parameters(), &mut names);
        } else if let Some(f) = n.as_for_node() {
            parameter_names(Some(f.index()), &mut names);
        }
    }
    all.iter().any(|n| element_mutation_site(n, &names))
}

/// `element_mutation_site?`.
fn element_mutation_site(n: &PrismNode<'_>, names: &[String]) -> bool {
    if let Some(c) = n.as_call_node() {
        return is_shape_mutator(&constant_string(c.name().as_slice()))
            && element_rooted(c.receiver(), names);
    }
    let receiver = if let Some(x) = n.as_index_operator_write_node() {
        x.receiver()
    } else if let Some(x) = n.as_index_or_write_node() {
        x.receiver()
    } else if let Some(x) = n.as_index_and_write_node() {
        x.receiver()
    } else if let Some(x) = n.as_call_operator_write_node() {
        x.receiver()
    } else if let Some(x) = n.as_call_or_write_node() {
        x.receiver()
    } else if let Some(x) = n.as_call_and_write_node() {
        x.receiver()
    } else if let Some(x) = n.as_index_target_node() {
        Some(x.receiver())
    } else if let Some(x) = n.as_call_target_node() {
        Some(x.receiver())
    } else {
        return false;
    };
    element_rooted(receiver, names)
}

/// `element_rooted?`: down the receiver chain, then through parentheses to
/// their last statement, to a read of an element name, `_1`..`_9`, or `it`.
fn element_rooted(node: Option<PrismNode<'_>>, names: &[String]) -> bool {
    let Some(mut node) = node else { return false };
    loop {
        let next = node.as_call_node().and_then(|c| c.receiver());
        match next {
            Some(r) => node = r,
            None => break,
        }
    }
    loop {
        let next = node.as_parentheses_node().and_then(|p| {
            p.body()
                .and_then(|b| b.as_statements_node())
                .and_then(|s| s.body().iter().last())
        });
        match next {
            Some(last) => node = last,
            None => break,
        }
    }
    if let Some(lv) = node.as_local_variable_read_node() {
        let name = constant_string(lv.name().as_slice());
        let numbered = name.len() == 2
            && name.starts_with('_')
            && matches!(name.as_bytes()[1], b'1'..=b'9');
        return numbered || names.contains(&name);
    }
    node.as_it_local_variable_read_node().is_some()
}

/// `ReceiverAlias.candidates`, local names only.
fn candidates(node: Option<PrismNode<'_>>, depth: u32, out: &mut Vec<String>) {
    const WALK_DEPTH_CAP: u32 = 6;
    let Some(node) = node else { return };
    if depth > WALK_DEPTH_CAP {
        return;
    }
    let mut push = |s: String| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    if let Some(w) = node.as_local_variable_write_node() {
        return push(constant_string(w.name().as_slice()));
    }
    if let Some(w) = node.as_local_variable_or_write_node() {
        return push(constant_string(w.name().as_slice()));
    }
    if let Some(w) = node.as_local_variable_and_write_node() {
        return push(constant_string(w.name().as_slice()));
    }
    if let Some(w) = node.as_local_variable_operator_write_node() {
        return push(constant_string(w.name().as_slice()));
    }
    if let Some(r) = node.as_local_variable_read_node() {
        return push(constant_string(r.name().as_slice()));
    }
    if let Some(p) = node.as_parentheses_node() {
        return candidates(p.body(), depth + 1, out);
    }
    if let Some(s) = node.as_statements_node() {
        return candidates(s.body().iter().last(), depth + 1, out);
    }
    if let Some(e) = node.as_else_node() {
        return candidates(e.statements().map(|s| s.as_node()), depth + 1, out);
    }
    if let Some(i) = node.as_if_node() {
        candidates(i.statements().map(|s| s.as_node()), depth + 1, out);
        return candidates(i.subsequent(), depth + 1, out);
    }
    if let Some(u) = node.as_unless_node() {
        candidates(u.statements().map(|s| s.as_node()), depth + 1, out);
        return candidates(u.else_clause().map(|e| e.as_node()), depth + 1, out);
    }
    if let Some(o) = node.as_or_node() {
        candidates(Some(o.left()), depth + 1, out);
        return candidates(Some(o.right()), depth + 1, out);
    }
    if let Some(a) = node.as_and_node() {
        candidates(Some(a.left()), depth + 1, out);
        candidates(Some(a.right()), depth + 1, out);
    }
}

impl Walk {
    fn record(&mut self, span: Span, receiver: Option<PrismNode<'_>>) {
        let mut names = Vec::new();
        candidates(receiver, 0, &mut names);
        if !names.is_empty() {
            self.out.push((span, names));
        }
    }
}

impl<'pr> Visit<'pr> for Walk {
    fn visit_call_node(&mut self, node: &crate::ruby_prism::CallNode<'pr>) {
        if let Some(block) = node.block() {
            let mutates = if let Some(b) = block.as_block_node() {
                // `widen_after_block`: a literal block with a body.
                let mut names = Vec::new();
                parameter_names(b.parameters(), &mut names);
                element_mutation(b.body(), names)
            } else if let Some(arg) = block.as_block_argument_node() {
                // `symbol_proc_mutation?`.
                arg.expression()
                    .and_then(|e| e.as_symbol_node().map(|s| s.unescaped().to_vec()))
                    .is_some_and(|name| is_shape_mutator(&String::from_utf8_lossy(&name)))
            } else {
                false
            };
            if mutates {
                self.record(span_of(&node.as_node()), node.receiver());
            }
        }
        crate::ruby_prism::visit_call_node(self, node);
    }

    fn visit_for_node(&mut self, node: &crate::ruby_prism::ForNode<'pr>) {
        // `widen_for_elements` / `for_collection_after`'s `deep`.
        let mut names = Vec::new();
        parameter_names(Some(node.index()), &mut names);
        if element_mutation(node.statements().map(|s| s.as_node()), names) {
            self.record(span_of(&node.as_node()), Some(node.collection()));
        }
        crate::ruby_prism::visit_for_node(self, node);
    }
}
