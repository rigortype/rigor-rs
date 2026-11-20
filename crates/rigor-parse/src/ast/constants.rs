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

/// One walk of a `ConstantPathNode` chain — the single traversal the lenient
/// render, the strict render, the `rooted`/`self_anchored` flags and the
/// lowering all share (rigor-rs#657: the recursive helpers each re-walked the
/// chain, and the recursive `format!` render made a deep class/module header
/// write O(depth²) bytes).
pub(crate) struct ConstPathChain<'pr> {
    /// The chain's `ConstantPathNode`s OUTERMOST first (`nodes[0]` is the node
    /// `collect` was given — `A::B::C`'s node, whose `name()` is `"C"`).
    pub nodes: Vec<ruby_prism::ConstantPathNode<'pr>>,
    /// The innermost non-path parent: `None` for the rooted `::A` spelling,
    /// else the chain's leftmost segment — a `ConstantReadNode` (`A::B`), a
    /// `SelfNode` (`self::A`) or a dynamic expression (`expr::A`).
    pub base: Option<PrismNode<'pr>>,
}

impl<'pr> ConstPathChain<'pr> {
    /// Walk `node`'s parent links to the chain's base. `None` when `node` is
    /// not a `ConstantPathNode` at all (the bare `ConstantReadNode` name case
    /// has no chain — callers handle it before collecting).
    pub(crate) fn collect(node: &PrismNode<'pr>) -> Option<Self> {
        let cp = node.as_constant_path_node()?;
        let mut nodes = vec![cp];
        let base = loop {
            match nodes.last().unwrap().parent() {
                Some(parent) => match parent.as_constant_path_node() {
                    Some(next) => nodes.push(next),
                    None => break Some(parent),
                },
                None => break None,
            }
        };
        Some(Self { nodes, base })
    }

    /// `Source::ConstantPath.rooted?` — `true` when the innermost path node
    /// has no parent (the `::Foo` spelling re-anchors at the top level).
    pub(crate) fn rooted(&self) -> bool {
        self.base.is_none()
    }

    /// `self_anchored_tail` — `true` when the chain's leftmost segment is
    /// `self` (`self::X`, `self::X::Y`).
    pub(crate) fn self_anchored(&self) -> bool {
        self.base.as_ref().is_some_and(|b| b.as_self_node().is_some())
    }

    /// The chain's segment names OUTERMOST last — i.e. innermost→outermost,
    /// the order the lenient join appends them (`nodes` is outermost-first,
    /// so this is `nodes` reversed). Each entry is `(segment-name, named)`:
    /// `named` is `false` for a segment `name()` cannot answer (the strict
    /// render declines the whole path on one).
    fn segments(&self) -> impl Iterator<Item = (String, bool)> + '_ {
        self.nodes.iter().rev().map(|cp| {
            (
                cp.name()
                    .map(|n| constant_string(n.as_slice()))
                    .unwrap_or_default(),
                cp.name().is_some(),
            )
        })
    }

    /// The lenient render — what `constant_path_string` produced recursively:
    /// the base's own lenient head (a `ConstantReadNode`'s name, `""` for any
    /// other base) joined with every segment name in innermost→outermost
    /// order, `::` inserted only before a non-empty tail.
    pub(crate) fn lenient(&self) -> String {
        let mut name = self
            .base
            .as_ref()
            .map(|b| constant_path_string(b))
            .unwrap_or_default();
        for (last, _) in self.segments() {
            if !name.is_empty() {
                name.push_str("::");
            }
            name.push_str(&last);
        }
        name
    }

    /// The strict render — `qualified_name_or_nil`: `None` when the base is
    /// anything but a constant read (`self`, a dynamic expression) or any
    /// segment is un-namable. A rooted chain renders its un-rooted join.
    pub(crate) fn strict(&self) -> Option<String> {
        let mut name = match &self.base {
            None => String::new(),
            Some(b) => strict_constant_path_string(b)?,
        };
        for (last, named) in self.segments() {
            if !named {
                return None;
            }
            if !name.is_empty() {
                name.push_str("::");
            }
            name.push_str(&last);
        }
        Some(name)
    }
}

/// The dotted constant-path name of a `class`/`module` declaration's path node:
/// `Point` -> `"Point"`, `Foo::Bar` -> `"Foo::Bar"`. A `ConstantReadNode` is the
/// bare-name case; a `ConstantPathNode` is the `A::B` case (walked iteratively
/// by [`ConstPathChain`]). Any other node (an unusual dynamic-constant form)
/// yields an empty string, which the SourceIndex treats as un-namable (no
/// instance typing for it).
pub(crate) fn constant_path_string(node: &PrismNode<'_>) -> String {
    if let Some(cr) = node.as_constant_read_node() {
        return constant_string(cr.name().as_slice());
    }
    ConstPathChain::collect(node)
        .map(|chain| chain.lenient())
        .unwrap_or_default()
}

/// Whether a constant-path node's leftmost ancestor is `self` (`self::X`,
/// `self::X::Y`). Mirrors the reference's `self_anchored_tail` recognition
/// (fb781023): the chain is walked to its base, which must be a
/// `Prism::SelfNode` — every other base (`::`, a constant, a dynamic
/// expression) answers `false`.
pub(crate) fn self_anchored_constant_path(node: &PrismNode<'_>) -> bool {
    ConstPathChain::collect(node).is_some_and(|c| c.self_anchored())
}

/// Whether a constant-path node is written with a leading `::` (`::Foo`,
/// `::Foo::Bar`) — the reference's `Source::ConstantPath.rooted?`. Prism spells
/// the root as a `ConstantPathNode` with a nil parent, so the answer lives at
/// the LEFTMOST segment: the walk reaches it and asks whether the chain ends
/// on no parent (rooted) or on a non-path base — a `ConstantReadNode` (bare
/// `Foo`), a `SelfNode` (`self::Foo`) or a dynamic expression — which is not.
pub(crate) fn rooted_constant_path(node: &PrismNode<'_>) -> bool {
    ConstPathChain::collect(node).is_some_and(|c| c.rooted())
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
    ConstPathChain::collect(node)?.strict()
}

/// `record_class_new_constant_decl` (`scope_indexer.rb:13196`) for the write
/// shapes the lowered AST cannot model — `Foo ||= …`, `::Foo = …`,
/// `M::Foo = …`, `M::Foo ||= …` (a plain `Foo = …` lowers to
/// `Node::ConstantWrite` and is censused on the lowered side). Returns the
/// AS-WRITTEN constant path — a leading `::` marks a rooted target — when the
/// rvalue is one of the four factory calls behind the reference's
/// `unwrap_or_guard` / `unwrap_freeze_tail` / `meta_new_constant_rvalue?`
/// (rigor-rs#672). `var::K` and `self::K` targets decline, matching
/// `meta_new_path_target_nameable?` / `self_anchored_tail`.
pub(crate) fn meta_new_write_name(node: &PrismNode<'_>) -> Option<String> {
    let (written, value) = if let Some(w) = node.as_constant_or_write_node() {
        (constant_string(w.name().as_slice()), w.value())
    } else if let Some(w) = node.as_constant_path_write_node() {
        (meta_new_path_target_name(&w.target().as_node())?, w.value())
    } else if let Some(w) = node.as_constant_path_or_write_node() {
        (meta_new_path_target_name(&w.target().as_node())?, w.value())
    } else {
        return None;
    };
    meta_new_rvalue(&written, value).then_some(written)
}

/// The as-written name of a path-write target (`M::Foo`, `::Foo`,
/// `self::Foo`) for [`meta_new_write_name`]. A `self::`-anchored tail keeps
/// its marker — the census resolves it against the enclosing `self_prefix`
/// (`meta_new_child_prefix`'s `self_anchored_tail` arm). `var::K` — a
/// dynamic base — renders nothing (`qualified_name_or_nil` declines).
fn meta_new_path_target_name(target: &PrismNode<'_>) -> Option<String> {
    if self_anchored_constant_path(target) {
        let chain = ConstPathChain::collect(target)?;
        let mut written = String::from("self");
        for (last, named) in chain.segments() {
            if !named {
                return None;
            }
            written.push_str("::");
            written.push_str(&last);
        }
        return Some(written);
    }
    let strict = strict_constant_path_string(target)?;
    Some(if rooted_constant_path(target) {
        format!("::{strict}")
    } else {
        strict
    })
}


/// `meta_new_rvalue` (`scope_indexer.rb:4960`): `K = K || <factory>` /
/// `K ||= K || <factory>` unwraps when the left operand re-reads the written
/// constant, then the repeated receiverful `.freeze` tail, then
/// `meta_new_constant_rvalue?` — `Class.new` / `Module.new` / `Struct.new` /
/// `Data.define` on a bare or `::`-rooted receiver. Argument-shape rules are
/// deliberately NOT checked (same looseness as the lowered-side
/// `is_meta_new_call`): an over-broad answer only ever costs coverage.
fn meta_new_rvalue(written: &str, mut value: PrismNode<'_>) -> bool {
    // `unwrap_or_guard`: `Foo ||= Foo || Struct.new(:a)` unwraps to the
    // factory only when the left operand names the constant being written.
    if let Some(or) = value.as_or_node() {
        if strict_constant_path_string(&or.left()).as_deref()
            == Some(written.trim_start_matches("::"))
        {
            value = or.right();
        }
    }
    // `unwrap_freeze_tail`: `freeze` must be receiverful, argument-less,
    // block-less and not `&.`.
    while let Some(c) = value.as_call_node() {
        if !(constant_string(c.name().as_slice()) == "freeze"
            && c.receiver().is_some()
            && c.arguments().is_none()
            && c.block().is_none()
            && !c.is_safe_navigation())
        {
            break;
        }
        value = c.receiver().unwrap();
    }
    let Some(call) = value.as_call_node() else {
        return false;
    };
    let Some(recv) = call.receiver() else {
        return false;
    };
    let method = constant_string(call.name().as_slice());
    META_NEW_SELECTORS
        .iter()
        .any(|&(k, m)| m == method && meta_new_receiver(&recv, k))
}

/// The class-creating factory calls a constant write's rvalue can name —
/// `META_NEW_SELECTORS`, shared with the lowered-side recogniser.
const META_NEW_SELECTORS: &[(&str, &str)] = &[
    ("Class", "new"),
    ("Module", "new"),
    ("Struct", "new"),
    ("Data", "define"),
];

/// `meta_constant_receiver?` — a bare `Struct`/`Class`/`Module`/`Data` read,
/// or the same name `::`-rooted (`::Struct.new` is a factory too); a
/// `Foo::Struct` base is a different class entirely.
fn meta_new_receiver(recv: &PrismNode<'_>, expected: &str) -> bool {
    if let Some(cr) = recv.as_constant_read_node() {
        return constant_string(cr.name().as_slice()) == expected;
    }
    match ConstPathChain::collect(recv) {
        Some(chain) => {
            chain.base.is_none()
                && chain.nodes.len() == 1
                && chain.nodes[0]
                    .name()
                    .is_some_and(|n| constant_string(n.as_slice()) == expected)
        }
        None => false,
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
