//! Constant-path rendering (lenient, strict, rooted, `self::`-anchored) and
//! upstream #540's constant-mutation census.

use crate::ruby_prism::{self, Node as PrismNode};

use super::constant_string;

/// One site where a CONSTANT-shaped receiver is mutated — the raw material of
/// upstream #540's `collect_literal_receiver_mutations` census
/// (`lib/rigor/inference/scope_indexer.rb`), which widens a literal-shape
/// constant the file itself mutates so reads stop folding through a shape the
/// program has already outgrown.
///
/// # Why a side table
///
/// Two of the three mutating shapes are invisible in the owned arena. A plain
/// `C[i] = v` / `C.x = v` is a Prism `CallNode` and lowers to [`Node::Call`], but
/// the `Index{Or,And,Operator}Write` family (`C[i] ||= v`, `C[i] += v`) has no
/// owned variant — it falls to the [`Node::Other`] / recovered-children path,
/// which keeps the subtree reachable but erases which node was the mutated
/// RECEIVER. Giving those an owned `Call`-shaped variant would put a synthetic
/// `[]=` dispatch in front of every call rule (a new arity/undefined-method
/// surface for zero gain), so the census is collected during the lowering walk —
/// where the Prism tree is still in hand — and consumed by the SourceIndex.
///
/// The census is deliberately RAW: it names the receiver and the mutating method
/// and leaves the `SHAPE_MUTATORS` membership test (`is_shape_mutator`) to
/// `rigor-infer`, which owns those tables.
///
/// [`Node::Call`]: crate::ast::Node::Call
/// [`Node::Other`]: crate::ast::Node::Other
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstMutation {
    /// The enclosing `class`/`module` header names, outermost first, each the
    /// RENDERED path of one header (`class A::B` contributes one `"A::B"`
    /// segment — mirroring the reference's `qualified_prefix + [name]`).
    pub prefix: Vec<String>,
    /// The receiver's dotted constant name (`"C"`, `"Outer::T"`).
    pub receiver: String,
    /// `true` when the receiver was written as a `A::B` PATH (or `::A`), which
    /// the reference records as the full name only — a BARE name instead
    /// contributes every lexical-resolution candidate.
    pub receiver_is_path: bool,
    /// The mutating call's method name, or `None` when the site is a mutation
    /// unconditionally — the `Index{Or,And,Operator}Write` family and a Prism
    /// `attribute_write?` call (`C.x = v`, `C[i] = v`).
    pub method: Option<String>,
}

/// The dotted constant-path name of a `class`/`module` declaration's path node:
/// `Point` -> `"Point"`, `Foo::Bar` -> `"Foo::Bar"`. A `ConstantReadNode` is the
/// bare-name case; a `ConstantPathNode` is the `A::B` case (recurse on parent).
/// Any other node (an unusual dynamic-constant form) yields an empty string,
/// which the SourceIndex treats as un-namable (no instance typing for it).
pub(crate) fn constant_path_string(node: &PrismNode<'_>) -> String {
    if let Some(cr) = node.as_constant_read_node() {
        return constant_string(cr.name().as_slice());
    }
    if let Some(cp) = node.as_constant_path_node() {
        // `name()` is the last component (`Bar` in `Foo::Bar`); the parent is the
        // scope. `::Foo` has no parent (top-level) — render just the name.
        let last = cp
            .name()
            .map(|n| constant_string(n.as_slice()))
            .unwrap_or_default();
        match cp.parent() {
            Some(parent) => {
                let head = constant_path_string(&parent);
                if head.is_empty() {
                    last
                } else {
                    format!("{head}::{last}")
                }
            }
            None => last,
        }
    } else {
        String::new()
    }
}

/// Whether a constant-path node's leftmost ancestor is `self` (`self::X`,
/// `self::X::Y`). Mirrors the reference's `self_anchored_tail` recognition
/// (fb781023): the chain is walked to its base, which must be a
/// `Prism::SelfNode` — every other base (`::`, a constant, a dynamic
/// expression) answers `false`.
pub(crate) fn self_anchored_constant_path(node: &PrismNode<'_>) -> bool {
    let Some(mut cp) = node.as_constant_path_node() else {
        return false;
    };
    while let Some(parent) = cp.parent() {
        if parent.as_self_node().is_some() {
            return true;
        }
        match parent.as_constant_path_node() {
            Some(next) => cp = next,
            None => return false,
        }
    }
    false
}

/// Whether a constant-path node is written with a leading `::` (`::Foo`,
/// `::Foo::Bar`) — the reference's `Source::ConstantPath.rooted?`. Prism spells
/// the root as a `ConstantPathNode` with a nil parent, so the answer lives at
/// the LEFTMOST segment: the walk reaches it and asks whether the chain ends
/// on no parent (rooted) or on a non-path base — a `ConstantReadNode` (bare
/// `Foo`), a `SelfNode` (`self::Foo`) or a dynamic expression — which is not.
pub(crate) fn rooted_constant_path(node: &PrismNode<'_>) -> bool {
    let Some(mut cp) = node.as_constant_path_node() else {
        return false;
    };
    loop {
        match cp.parent() {
            None => return true,
            Some(parent) => match parent.as_constant_path_node() {
                Some(next) => cp = next,
                None => return false,
            },
        }
    }
}

/// The STRICT twin of [`constant_path_string`] — the reference's
/// `Source::ConstantPath.qualified_name_or_nil`. A dynamic base anywhere in the
/// chain (`expr::Bar`) yields `None` rather than a best-effort trailing name, so
/// a caller that statically NAMES constants treats the path as opaque. A leading
/// `::` renders as the un-rooted name under both policies (`::Foo` => `"Foo"`),
/// because the discovered-constant tables are keyed by un-rooted names.
pub(crate) fn strict_constant_path_string(node: &PrismNode<'_>) -> Option<String> {
    if let Some(cr) = node.as_constant_read_node() {
        return Some(constant_string(cr.name().as_slice()));
    }
    let cp = node.as_constant_path_node()?;
    let last = constant_string(cp.name()?.as_slice());
    match cp.parent() {
        None => Some(last),
        Some(parent) => Some(format!("{}::{last}", strict_constant_path_string(&parent)?)),
    }
}

/// Upstream #540 (`fc3b8b42`) — the whole-file mutation census behind the
/// SourceIndex's two wideners, a faithful port of `ScopeIndexer`'s
/// `collect_literal_receiver_mutations` / `walk_literal_receiver_mutations` /
/// `record_literal_receiver_mutation` / `mutating_receiver_of`.
///
/// Scope-INSENSITIVE: blocks, method bodies and the top level all count; the
/// only thing tracked is the lexical `class`/`module` prefix. A NAMED
/// class/module is entered through its BODY only (its header path and
/// superclass expression are not walked) — the reference's `return` after
/// recursing into `node.body`.
///
/// Class-variable receivers (`@@table << x`) are deliberately NOT recorded: the
/// port's lowering has no cvar index to widen (`@@x` lowers to the nameless
/// [`Node::VariableRead`], already `Dynamic[top]`), so the reference's cvar half
/// is a no-op here.
///
/// [`Node::VariableRead`]: crate::ast::Node::VariableRead
pub(crate) fn collect_const_mutations(root: &PrismNode<'_>) -> Vec<ConstMutation> {
    use ruby_prism::Visit;

    struct Census {
        out: Vec<ConstMutation>,
        prefix: Vec<String>,
    }

    impl Census {
        /// `record_literal_receiver_mutation`: keep the site iff the receiver is
        /// a constant read or a statically-nameable constant path.
        fn record<'pr>(&mut self, receiver: Option<PrismNode<'pr>>, method: Option<String>) {
            let Some(receiver) = receiver else { return };
            let is_path = receiver.as_constant_path_node().is_some();
            if !is_path && receiver.as_constant_read_node().is_none() {
                return;
            }
            let Some(name) = strict_constant_path_string(&receiver) else { return };
            self.out.push(ConstMutation {
                prefix: self.prefix.clone(),
                receiver: name,
                receiver_is_path: is_path,
                method,
            });
        }
    }

    impl<'pr> Visit<'pr> for Census {
        fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
            let name = constant_path_string(&node.constant_path());
            if name.is_empty() {
                ruby_prism::visit_class_node(self, node);
                return;
            }
            self.prefix.push(name);
            if let Some(body) = node.body() {
                self.visit(&body);
            }
            self.prefix.pop();
        }

        fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
            let name = constant_path_string(&node.constant_path());
            if name.is_empty() {
                ruby_prism::visit_module_node(self, node);
                return;
            }
            self.prefix.push(name);
            if let Some(body) = node.body() {
                self.visit(&body);
            }
            self.prefix.pop();
        }

        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if node.receiver().is_some() {
                // `attribute_write?` (`C.x = v`, `C[i] = v`) is a mutation
                // whatever it is called; anything else is one only when the name
                // is in `rigor-infer`'s mutator tables, which decides later.
                let method = (!node.is_attribute_write())
                    .then(|| constant_string(node.name().as_slice()));
                self.record(node.receiver(), method);
            }
            ruby_prism::visit_call_node(self, node);
        }

        fn visit_index_or_write_node(&mut self, node: &ruby_prism::IndexOrWriteNode<'pr>) {
            self.record(node.receiver(), None);
            ruby_prism::visit_index_or_write_node(self, node);
        }

        fn visit_index_and_write_node(&mut self, node: &ruby_prism::IndexAndWriteNode<'pr>) {
            self.record(node.receiver(), None);
            ruby_prism::visit_index_and_write_node(self, node);
        }

        fn visit_index_operator_write_node(
            &mut self,
            node: &ruby_prism::IndexOperatorWriteNode<'pr>,
        ) {
            self.record(node.receiver(), None);
            ruby_prism::visit_index_operator_write_node(self, node);
        }
    }

    let mut census = Census { out: Vec::new(), prefix: Vec::new() };
    census.visit(root);
    census.out
}

/// The name of a constant *reference* used as a superclass (`< Bar`,
/// `< Foo::Bar`): the **last** path component, since that is what the
/// source-superclass chain walk resolves against the SourceIndex / RBS by simple
/// name. Returns `None` for a non-constant superclass expression (e.g.
/// `< Struct.new(...)`), which leaves the chain deliberately open (unknown
/// ancestor ⇒ the conservative gate stays silent).
pub(crate) fn constant_node_name(node: &PrismNode<'_>) -> Option<String> {
    if let Some(cr) = node.as_constant_read_node() {
        return Some(constant_string(cr.name().as_slice()));
    }
    if let Some(cp) = node.as_constant_path_node() {
        return cp.name().map(|n| constant_string(n.as_slice()));
    }
    None
}
