//! Owned, `NodeId`-indexed AST + the Prism lowering pass (ADR-0012).
//!
//! `ruby-prism` nodes borrow the parse buffer (a lifetime on every node).
//! Threading that lifetime through the inference engine reproduces the
//! pervasive-`'a` pain ADR-0005/0006 deliberately avoid, so [`lower`] walks the
//! borrowed Prism tree exactly once and produces *owned* nodes keyed by a dense
//! [`NodeId`]. Inference and rules then walk this owned arena, never the
//! borrowed Prism tree.
//!
//! The owned node *shape* mirrors Prism's (rather than normalizing to a
//! semantically different HIR) so node-level behaviour stays aligned with the
//! reference for diagnostic-set parity (ADR-0002/0012). This is the minimal
//! tracer-bullet subset; the long tail of Prism nodes lowers to [`Node::Other`]
//! so an unhandled construct never aborts the walk.

use crate::ruby_prism::{self, Node as PrismNode, ParseResult};

/// A dense handle into [`LoweredAst::nodes`]. Cheap to copy; stable for the
/// lifetime of the owned AST (ADR-0012: owned, `NodeId`-keyed nodes).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct NodeId(pub u32);

/// A source span as a half-open byte-offset range `[start, end)`.
///
/// Byte offsets (not line/col) are the load-bearing location per ADR-0030; the
/// CLI computes line/col lazily from the source when presenting (ADR-0030:
/// Prism columns are 0-based, the presenter adds 1).
pub type Span = (usize, usize);

/// One owned node. Mirrors a minimal Prism subset (ADR-0012); every variant
/// carries the byte [`Span`] needed to key a diagnostic (ADR-0030).
#[derive(Clone, Debug)]
pub enum Node {
    /// The compilation unit: an ordered list of top-level statements.
    Program { body: Vec<NodeId>, span: Span },
    /// A sequence of statements (a `begin`/method/program body in Prism).
    Statements { body: Vec<NodeId>, span: Span },
    /// `name = <value>`. The written local feeds the inference environment as
    /// statements are walked in order (ADR-0023 tier-0 literal typing).
    LocalVariableWrite { name: String, value: NodeId, span: Span },
    /// A read of a previously-written local (`s`).
    LocalVariableRead { name: String, span: Span },
    /// A string literal (`"Hello"`); `value` is the unescaped contents.
    StringLit { value: String, span: Span },
    /// An integer literal (`42`).
    IntegerLit { value: i64, span: Span },
    /// A method call. `receiver` is `None` for an implicit-self call.
    /// `message_span` is the precise span of the *method name* token — the
    /// location a `call.undefined-method` diagnostic keys on (ADR-0002/0030).
    Call {
        receiver: Option<NodeId>,
        method: String,
        /// Span of the method-name token (`lenght`), the diagnostic anchor.
        message_span: Span,
        /// Span of the whole call expression.
        span: Span,
    },
    /// Catch-all for any Prism node not in the tracer-bullet subset, so the
    /// lowering walk is total. Carries the original span for completeness.
    ///
    // TODO(spec): grow the owned-node set toward full Prism coverage, and add
    // synthetic-node variants (plugin/macro-generated definitions with no
    // source text) per ADR-0012 / ADR-0013. No plugins yet, so no synthetic
    // variant is materialized in this slice.
    Other { span: Span },
}

impl Node {
    /// The byte span of this node, regardless of variant.
    pub fn span(&self) -> Span {
        match self {
            Node::Program { span, .. }
            | Node::Statements { span, .. }
            | Node::LocalVariableWrite { span, .. }
            | Node::LocalVariableRead { span, .. }
            | Node::StringLit { span, .. }
            | Node::IntegerLit { span, .. }
            | Node::Call { span, .. }
            | Node::Other { span } => *span,
        }
    }
}

/// The owned AST: a flat arena of [`Node`]s plus the [`NodeId`] of the root
/// `Program`. Free of the Prism parse-buffer lifetime (ADR-0012).
#[derive(Clone, Debug)]
pub struct LoweredAst {
    nodes: Vec<Node>,
    root: NodeId,
}

impl LoweredAst {
    /// Resolve a handle to its owned node.
    pub fn get(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    /// The root `Program` node id.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Number of owned nodes in the arena.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the arena is empty. Never true after a successful [`lower`].
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Iterate `(NodeId, &Node)` over the arena in id order. Rules use this to
    /// walk every node in a single converged pass (ADR-0005).
    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (NodeId(i as u32), n))
    }
}

/// Lower a borrowed Prism [`ParseResult`] into the owned, `NodeId`-indexed AST
/// (ADR-0012). Walks the tree once; unhandled Prism nodes become
/// [`Node::Other`] so the walk is total and never panics on a novel construct
/// (ADR-0016 never-crash posture).
pub fn lower(result: &ParseResult<'_>) -> LoweredAst {
    let mut builder = Builder { nodes: Vec::new() };
    let root_prism = result.node();
    let root = builder.lower_node(&root_prism);
    LoweredAst {
        nodes: builder.nodes,
        root,
    }
}

/// Mutable accumulator for the owned arena during the lowering walk.
struct Builder {
    nodes: Vec<Node>,
}

impl Builder {
    /// Push an owned node, returning its fresh [`NodeId`].
    fn push(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(node);
        id
    }

    /// Lower one borrowed Prism node (and its children) into owned nodes,
    /// returning the id of the produced root. Recursion is bounded by the
    /// source's nesting depth.
    fn lower_node(&mut self, node: &PrismNode<'_>) -> NodeId {
        let span = span_of(&node.location());

        if let Some(program) = node.as_program_node() {
            let stmts = program.statements();
            let body = self.lower_body(&stmts.body());
            return self.push(Node::Program {
                body,
                span: span_of(&program.location()),
            });
        }

        if let Some(stmts) = node.as_statements_node() {
            let body = self.lower_body(&stmts.body());
            return self.push(Node::Statements {
                body,
                span: span_of(&stmts.location()),
            });
        }

        if let Some(write) = node.as_local_variable_write_node() {
            let name = constant_string(write.name().as_slice());
            let value = self.lower_node(&write.value());
            return self.push(Node::LocalVariableWrite {
                name,
                value,
                span: span_of(&write.location()),
            });
        }

        if let Some(read) = node.as_local_variable_read_node() {
            let name = constant_string(read.name().as_slice());
            return self.push(Node::LocalVariableRead {
                name,
                span: span_of(&read.location()),
            });
        }

        if let Some(s) = node.as_string_node() {
            // `unescaped()` is the decoded contents (`"Hello"` -> Hello).
            let value = String::from_utf8_lossy(s.unescaped()).into_owned();
            return self.push(Node::StringLit {
                value,
                span: span_of(&s.location()),
            });
        }

        if let Some(int) = node.as_integer_node() {
            // Prism's `Integer` exposes only `TryInto<i32>` in this binding;
            // for the tracer-bullet literal subset that range suffices.
            // TODO(spec): widen to full bignum / i64 once a wider accessor is
            // available (or via to_u32_digits) — value-lattice Constant[Int].
            let value: i64 = TryInto::<i32>::try_into(int.value())
                .map(i64::from)
                .unwrap_or(0);
            return self.push(Node::IntegerLit {
                value,
                span: span_of(&int.location()),
            });
        }

        if let Some(call) = node.as_call_node() {
            let receiver = call.receiver().map(|r| self.lower_node(&r));
            let method = constant_string(call.name().as_slice());
            // The message_loc is the method-name token; fall back to the whole
            // call span if Prism elides it (e.g. operator-ish forms).
            let message_span = call
                .message_loc()
                .map(|l| span_of(&l))
                .unwrap_or(span);
            // NOTE: arguments and blocks are intentionally not lowered in this
            // slice — the rule only needs receiver + method name.
            // TODO(spec): lower arguments/blocks for argument-contract rules.
            return self.push(Node::Call {
                receiver,
                method,
                message_span,
                span: span_of(&call.location()),
            });
        }

        // Anything outside the tracer-bullet subset lowers to a span-only
        // placeholder so the walk is total (ADR-0012/0016).
        self.push(Node::Other { span })
    }

    /// Lower a Prism `NodeList` body (statement sequence) into owned ids in
    /// source order — the order inference relies on to populate the env.
    fn lower_body(&mut self, body: &ruby_prism::NodeList<'_>) -> Vec<NodeId> {
        body.iter().map(|n| self.lower_node(&n)).collect()
    }
}

/// Decode a Prism `ConstantId` byte slice (a method / variable name) to an
/// owned `String`. Names are UTF-8 in practice; lossy decode keeps the walk
/// total on exotic encodings.
fn constant_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Convert a Prism `Location` to a byte-offset [`Span`].
fn span_of(loc: &ruby_prism::Location<'_>) -> Span {
    (loc.start_offset(), loc.end_offset())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowers_assignment_and_call_with_precise_spans() {
        let src = b"s = \"Hello\"\ns.lenght\n";
        let result = crate::parse(src);
        let ast = lower(&result);

        // Find the single Call node and assert its method + message span maps
        // back to `lenght` in the source.
        let call = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::Call { method, message_span, .. } => {
                    Some((method.clone(), *message_span))
                }
                _ => None,
            })
            .expect("expected a Call node");
        assert_eq!(call.0, "lenght");
        let (start, end) = call.1;
        assert_eq!(&src[start..end], b"lenght");
    }

    #[test]
    fn lowers_local_write_and_string_literal() {
        let src = b"s = \"Hello\"\n";
        let result = crate::parse(src);
        let ast = lower(&result);

        let has_write = ast.iter().any(|(_, n)| {
            matches!(n, Node::LocalVariableWrite { name, .. } if name == "s")
        });
        let has_str = ast.iter().any(|(_, n)| {
            matches!(n, Node::StringLit { value, .. } if value == "Hello")
        });
        assert!(has_write, "expected a LocalVariableWrite for `s`");
        assert!(has_str, "expected a StringLit \"Hello\"");
    }

    #[test]
    fn lowering_is_total_for_unhandled_nodes() {
        // A construct outside the subset (a method def) must still lower
        // without panicking, landing in `Other`.
        let src = b"def foo; end\n";
        let result = crate::parse(src);
        let ast = lower(&result);
        assert!(!ast.is_empty());
        assert!(ast.iter().any(|(_, n)| matches!(n, Node::Other { .. })));
    }
}
