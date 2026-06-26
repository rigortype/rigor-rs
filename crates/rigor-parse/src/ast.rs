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
//! reference for diagnostic-set parity (ADR-0002/0012). The long tail of Prism
//! nodes still lowers to [`Node::Other`] so an unhandled construct never aborts
//! the walk.
//!
//! ## Recursive structural lowering
//!
//! Beyond the top-level literal/call subset, [`lower`] recurses into the bodies
//! of definitions (`def`/`class`/`module`/singleton class), control flow
//! (`if`/`unless`/ternary, `case`/`when`/`in`, `while`/`until`/`for`,
//! `begin`/`rescue`/`ensure`, `&&`/`||`), blocks (`foo { ... }`), and into the
//! receivers/values of variable, constant, array, hash, index, range and
//! string-interpolation nodes. The point is reachability: EVERY nested call
//! lands in the arena as a [`Node::Call`], so the single rule walk
//! (`ast.iter()` filtering `Node::Call`) analyses calls inside a method/branch
//! body, not just top-level ones. Structural variants carry child [`NodeId`]s so
//! the typer can recurse into a receiver/argument; constructs we don't type
//! precisely still get their children lowered (and so analysed).

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
    /// A float literal (`3.14`); `value` is the parsed `f64`.
    FloatLit { value: f64, span: Span },
    /// A symbol literal (`:foo`); `value` is the symbol name (no leading colon).
    SymbolLit { value: String, span: Span },
    /// The `nil` literal.
    NilLit { span: Span },
    /// The `true` literal.
    TrueLit { span: Span },
    /// The `false` literal.
    FalseLit { span: Span },
    /// A method call. `receiver` is `None` for an implicit-self call.
    /// `message_span` is the precise span of the *method name* token — the
    /// location a `call.undefined-method` diagnostic keys on (ADR-0002/0030).
    Call {
        receiver: Option<NodeId>,
        method: String,
        /// Positional argument expressions in source order (ADR-0023: needed
        /// for argument-contract rules such as `call.wrong-arity` and for
        /// argument-dependent constant folding). Splat/keyword/block args are
        /// intentionally not collected here in this slice.
        args: Vec<NodeId>,
        /// Statements of an attached block (`foo { ... }` / `do…end`), lowered
        /// so calls inside the block reach the rule walk. Empty for a call with
        /// no block. Not a *value* of the call — purely a reachability handle.
        block_body: Vec<NodeId>,
        /// Span of the method-name token (`lenght`), the diagnostic anchor.
        message_span: Span,
        /// Span of the whole call expression.
        span: Span,
    },
    /// A definition (`def` / `class` / `module` / singleton class). Carries its
    /// lowered body statements only — a definition is not a value, so the typer
    /// never types it; the body is lowered purely so nested calls are reachable.
    Definition { body: Vec<NodeId>, span: Span },
    /// `if`/`unless`/ternary. `predicate`, the `then` branch and the optional
    /// `else`/`elsif` subsequent are all lowered. Typed as `Dynamic[top]` (an
    /// `if`-as-expression has no precise branch-union type in this slice).
    // TODO(spec): branch-union typing (ADR-0022 flow narrowing).
    If {
        predicate: NodeId,
        then_body: Vec<NodeId>,
        else_body: Vec<NodeId>,
        span: Span,
    },
    /// `case`/`when` or `case`/`in`. The optional subject predicate, every
    /// branch condition/pattern, and every branch body are lowered. Typed as
    /// `Dynamic[top]`.
    Case {
        predicate: Option<NodeId>,
        branches: Vec<NodeId>,
        else_body: Vec<NodeId>,
        span: Span,
    },
    /// `while`/`until`/`for`. The (optional) predicate/collection and the loop
    /// body are lowered. Typed as `Dynamic[top]`.
    Loop {
        predicate: Option<NodeId>,
        body: Vec<NodeId>,
        span: Span,
    },
    /// `begin`/`rescue`/`else`/`ensure`. The protected body, each rescue body,
    /// the else body and the ensure body are all lowered. Typed `Dynamic[top]`.
    BeginRescue { body: Vec<NodeId>, span: Span },
    /// `&&` / `||` / `and` / `or`. Both operands are lowered (so a call on
    /// either side is analysed). Typed `Dynamic[top]` — the result is one of the
    /// two operand types, which we don't union here.
    Logical { left: NodeId, right: NodeId, span: Span },
    /// An array literal (`[a, b]`). Elements are lowered. Typed `Nominal Array`
    /// so a typo'd method on an array literal flags via the real Array RBS.
    // TODO(spec): Tuple precision (element types) per ADR-0023.
    ArrayLit { elements: Vec<NodeId>, span: Span },
    /// A hash literal (`{ k => v }`). Key/value children are lowered. Typed
    /// `Nominal Hash` so a typo'd method flags via the real Hash RBS.
    // TODO(spec): HashShape precision per ADR-0023.
    HashLit { elements: Vec<NodeId>, span: Span },
    /// A range (`a..b` / `a...b`). Both bounds (when present) lowered. Typed
    /// `Dynamic[top]`. Note: an index read `a[i]` is a Prism `CallNode` named
    /// `[]`, so it lowers as a [`Node::Call`] (receiver + index args) and needs
    /// no dedicated variant.
    Range { span: Span },
    /// An instance/class/global variable read (`@x`, `@@x`, `$x`). Typed
    /// `Dynamic[top]` — no ivar/cvar/gvar type tracking in this slice.
    // TODO(spec): ivar typing (ADR-0022).
    VariableRead { span: Span },
    /// An instance/class/global variable write (`@x = v`). The value is lowered
    /// (so a call in the assigned expression is analysed). Not a value itself.
    VariableWrite { value: NodeId, span: Span },
    /// A constant read (`Foo`, `Foo::Bar`). For a path, the parent scope is
    /// lowered. Typed `Dynamic[top]` — no constant resolution in this slice.
    // TODO(spec): constant resolution (ADR-0019).
    ConstantRead { span: Span },
    /// A constant write (`FOO = v`). The value is lowered. Not a value itself.
    ConstantWrite { value: NodeId, span: Span },
    /// `self`. Typed `Dynamic[top]` — the enclosing-class type is not tracked in
    /// this slice.
    SelfExpr { span: Span },
    /// Catch-all for any Prism node not yet given an owned variant, so the
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
            | Node::FloatLit { span, .. }
            | Node::SymbolLit { span, .. }
            | Node::NilLit { span }
            | Node::TrueLit { span }
            | Node::FalseLit { span }
            | Node::Call { span, .. }
            | Node::Definition { span, .. }
            | Node::If { span, .. }
            | Node::Case { span, .. }
            | Node::Loop { span, .. }
            | Node::BeginRescue { span, .. }
            | Node::Logical { span, .. }
            | Node::ArrayLit { span, .. }
            | Node::HashLit { span, .. }
            | Node::Range { span }
            | Node::VariableRead { span }
            | Node::VariableWrite { span, .. }
            | Node::ConstantRead { span }
            | Node::ConstantWrite { span, .. }
            | Node::SelfExpr { span }
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

        if let Some(f) = node.as_float_node() {
            return self.push(Node::FloatLit {
                value: f.value(),
                span: span_of(&f.location()),
            });
        }

        if let Some(sym) = node.as_symbol_node() {
            // `unescaped()` is the decoded symbol name (`:foo` -> foo).
            let value = String::from_utf8_lossy(sym.unescaped()).into_owned();
            return self.push(Node::SymbolLit {
                value,
                span: span_of(&sym.location()),
            });
        }

        if let Some(n) = node.as_nil_node() {
            return self.push(Node::NilLit {
                span: span_of(&n.location()),
            });
        }

        if let Some(t) = node.as_true_node() {
            return self.push(Node::TrueLit {
                span: span_of(&t.location()),
            });
        }

        if let Some(fa) = node.as_false_node() {
            return self.push(Node::FalseLit {
                span: span_of(&fa.location()),
            });
        }

        if let Some(call) = node.as_call_node() {
            let receiver = call.receiver().map(|r| self.lower_node(&r));
            let method = constant_string(call.name().as_slice());
            // Lower positional arguments in source order (ADR-0023: argument
            // contracts + arg-dependent folding). Splat/keyword/block args lower
            // like any other node — a downstream rule that needs to distinguish
            // them does so by inspecting the lowered child, never here.
            // TODO(spec): mark splat/keyword/block args so the arity rule can
            // bail on non-plain-positional shapes (it is conservative regardless).
            let args = call
                .arguments()
                .map(|a| self.lower_body(&a.arguments()))
                .unwrap_or_default();
            // Lower an attached block's body so calls inside it reach the walk.
            // The block is a BlockNode (`{ … }` / `do…end`); a `&block`-arg form
            // is a BlockArgumentNode with no inline body, which lowers to nothing
            // here (its calls, if any, are in the referenced symbol/proc).
            let block_body = call
                .block()
                .and_then(|b| b.as_block_node())
                .map(|b| self.lower_optional_body(b.body().as_ref()))
                .unwrap_or_default();
            // The message_loc is the method-name token; fall back to the whole
            // call span if Prism elides it (e.g. operator-ish forms).
            let message_span = call
                .message_loc()
                .map(|l| span_of(&l))
                .unwrap_or(span);
            return self.push(Node::Call {
                receiver,
                method,
                args,
                block_body,
                message_span,
                span: span_of(&call.location()),
            });
        }

        if let Some(def) = node.as_def_node() {
            // Lower the method body so its calls reach the walk. Parameters are
            // intentionally NOT bound to any type: an unknown local read is
            // already `Dynamic[top]` (silent), the zero-FP-safe choice — binding
            // a param to a guessed type could mint a false `undefined-method`.
            let body = self.lower_optional_body(def.body().as_ref());
            return self.push(Node::Definition {
                body,
                span: span_of(&def.location()),
            });
        }

        if let Some(class) = node.as_class_node() {
            let body = self.lower_optional_body(class.body().as_ref());
            return self.push(Node::Definition {
                body,
                span: span_of(&class.location()),
            });
        }

        if let Some(module) = node.as_module_node() {
            let body = self.lower_optional_body(module.body().as_ref());
            return self.push(Node::Definition {
                body,
                span: span_of(&module.location()),
            });
        }

        if let Some(sclass) = node.as_singleton_class_node() {
            let body = self.lower_optional_body(sclass.body().as_ref());
            return self.push(Node::Definition {
                body,
                span: span_of(&sclass.location()),
            });
        }

        if let Some(if_node) = node.as_if_node() {
            // `if` / ternary. Prism's ternary is also an IfNode.
            let predicate = self.lower_node(&if_node.predicate());
            let then_body = if_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            // `subsequent` is the `elsif`/`else` chain (an IfNode or ElseNode).
            let else_body = if_node
                .subsequent()
                .map(|sub| vec![self.lower_node(&sub)])
                .unwrap_or_default();
            return self.push(Node::If {
                predicate,
                then_body,
                else_body,
                span: span_of(&if_node.location()),
            });
        }

        if let Some(unless_node) = node.as_unless_node() {
            let predicate = self.lower_node(&unless_node.predicate());
            let then_body = unless_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            let else_body = unless_node
                .else_clause()
                .map(|e| vec![self.lower_node(&e.as_node())])
                .unwrap_or_default();
            return self.push(Node::If {
                predicate,
                then_body,
                else_body,
                span: span_of(&unless_node.location()),
            });
        }

        if let Some(else_node) = node.as_else_node() {
            // An `else` clause body (reached via an If/Unless subsequent).
            let body = else_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            return self.push(Node::BeginRescue {
                body,
                span: span_of(&else_node.location()),
            });
        }

        if let Some(case_node) = node.as_case_node() {
            // `case`/`when`. Lower the subject, every `when` (conditions + body),
            // and the `else`.
            let predicate = case_node.predicate().map(|p| self.lower_node(&p));
            let mut branches = Vec::new();
            for cond in case_node.conditions().iter() {
                branches.push(self.lower_node(&cond));
            }
            let else_body = case_node
                .else_clause()
                .and_then(|e| e.statements())
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            return self.push(Node::Case {
                predicate,
                branches,
                else_body,
                span: span_of(&case_node.location()),
            });
        }

        if let Some(case_match) = node.as_case_match_node() {
            // `case`/`in` pattern matching. Same shape as CaseNode.
            let predicate = case_match.predicate().map(|p| self.lower_node(&p));
            let mut branches = Vec::new();
            for cond in case_match.conditions().iter() {
                branches.push(self.lower_node(&cond));
            }
            let else_body = case_match
                .else_clause()
                .and_then(|e| e.statements())
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            return self.push(Node::Case {
                predicate,
                branches,
                else_body,
                span: span_of(&case_match.location()),
            });
        }

        if let Some(when_node) = node.as_when_node() {
            // A `when` branch: lower its condition expressions and body.
            let mut body: Vec<NodeId> = when_node
                .conditions()
                .iter()
                .map(|c| self.lower_node(&c))
                .collect();
            if let Some(s) = when_node.statements() {
                body.extend(self.lower_body(&s.body()));
            }
            return self.push(Node::BeginRescue {
                body,
                span: span_of(&when_node.location()),
            });
        }

        if let Some(in_node) = node.as_in_node() {
            // An `in` pattern branch: lower the pattern and the body.
            let mut body = vec![self.lower_node(&in_node.pattern())];
            if let Some(s) = in_node.statements() {
                body.extend(self.lower_body(&s.body()));
            }
            return self.push(Node::BeginRescue {
                body,
                span: span_of(&in_node.location()),
            });
        }

        if let Some(while_node) = node.as_while_node() {
            let predicate = Some(self.lower_node(&while_node.predicate()));
            let body = while_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            return self.push(Node::Loop {
                predicate,
                body,
                span: span_of(&while_node.location()),
            });
        }

        if let Some(until_node) = node.as_until_node() {
            let predicate = Some(self.lower_node(&until_node.predicate()));
            let body = until_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            return self.push(Node::Loop {
                predicate,
                body,
                span: span_of(&until_node.location()),
            });
        }

        if let Some(for_node) = node.as_for_node() {
            // `for x in coll; …; end`. Lower the collection (a call can live
            // there) and the body. The index target is a write target, no call.
            let predicate = Some(self.lower_node(&for_node.collection()));
            let body = for_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            return self.push(Node::Loop {
                predicate,
                body,
                span: span_of(&for_node.location()),
            });
        }

        if let Some(begin_node) = node.as_begin_node() {
            // `begin`/`rescue`/`else`/`ensure`. Collect every sub-body's calls.
            let mut body: Vec<NodeId> = begin_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            // Walk the rescue chain (each RescueNode links to the next).
            let mut rescue = begin_node.rescue_clause();
            while let Some(r) = rescue {
                for exc in r.exceptions().iter() {
                    body.push(self.lower_node(&exc));
                }
                if let Some(s) = r.statements() {
                    body.extend(self.lower_body(&s.body()));
                }
                rescue = r.subsequent();
            }
            if let Some(e) = begin_node.else_clause().and_then(|e| e.statements()) {
                body.extend(self.lower_body(&e.body()));
            }
            if let Some(e) = begin_node.ensure_clause().and_then(|e| e.statements()) {
                body.extend(self.lower_body(&e.body()));
            }
            return self.push(Node::BeginRescue {
                body,
                span: span_of(&begin_node.location()),
            });
        }

        if let Some(and_node) = node.as_and_node() {
            let left = self.lower_node(&and_node.left());
            let right = self.lower_node(&and_node.right());
            return self.push(Node::Logical {
                left,
                right,
                span: span_of(&and_node.location()),
            });
        }

        if let Some(or_node) = node.as_or_node() {
            let left = self.lower_node(&or_node.left());
            let right = self.lower_node(&or_node.right());
            return self.push(Node::Logical {
                left,
                right,
                span: span_of(&or_node.location()),
            });
        }

        if let Some(arr) = node.as_array_node() {
            let elements = self.lower_body(&arr.elements());
            return self.push(Node::ArrayLit {
                elements,
                span: span_of(&arr.location()),
            });
        }

        if let Some(hash) = node.as_hash_node() {
            // Lower each assoc's key + value (a call can hide in either).
            let mut elements = Vec::new();
            for el in hash.elements().iter() {
                if let Some(assoc) = el.as_assoc_node() {
                    elements.push(self.lower_node(&assoc.key()));
                    elements.push(self.lower_node(&assoc.value()));
                } else {
                    elements.push(self.lower_node(&el));
                }
            }
            return self.push(Node::HashLit {
                elements,
                span: span_of(&hash.location()),
            });
        }

        if let Some(range) = node.as_range_node() {
            // Lower both bounds for reachability; the node itself types Dynamic.
            if let Some(l) = range.left() {
                self.lower_node(&l);
            }
            if let Some(r) = range.right() {
                self.lower_node(&r);
            }
            return self.push(Node::Range {
                span: span_of(&range.location()),
            });
        }

        if let Some(ivw) = node.as_instance_variable_write_node() {
            let value = self.lower_node(&ivw.value());
            return self.push(Node::VariableWrite {
                value,
                span: span_of(&ivw.location()),
            });
        }
        if let Some(cvw) = node.as_class_variable_write_node() {
            let value = self.lower_node(&cvw.value());
            return self.push(Node::VariableWrite {
                value,
                span: span_of(&cvw.location()),
            });
        }
        if let Some(gvw) = node.as_global_variable_write_node() {
            let value = self.lower_node(&gvw.value());
            return self.push(Node::VariableWrite {
                value,
                span: span_of(&gvw.location()),
            });
        }

        if let Some(ivr) = node.as_instance_variable_read_node() {
            return self.push(Node::VariableRead {
                span: span_of(&ivr.location()),
            });
        }
        if let Some(cvr) = node.as_class_variable_read_node() {
            return self.push(Node::VariableRead {
                span: span_of(&cvr.location()),
            });
        }
        if let Some(gvr) = node.as_global_variable_read_node() {
            return self.push(Node::VariableRead {
                span: span_of(&gvr.location()),
            });
        }

        if let Some(cw) = node.as_constant_write_node() {
            let value = self.lower_node(&cw.value());
            return self.push(Node::ConstantWrite {
                value,
                span: span_of(&cw.location()),
            });
        }
        if let Some(cr) = node.as_constant_read_node() {
            return self.push(Node::ConstantRead {
                span: span_of(&cr.location()),
            });
        }
        if let Some(cp) = node.as_constant_path_node() {
            // `Foo::Bar` — lower the parent scope (it may itself be a call/const).
            if let Some(parent) = cp.parent() {
                self.lower_node(&parent);
            }
            return self.push(Node::ConstantRead {
                span: span_of(&cp.location()),
            });
        }

        if let Some(self_node) = node.as_self_node() {
            return self.push(Node::SelfExpr {
                span: span_of(&self_node.location()),
            });
        }

        if let Some(interp) = node.as_interpolated_string_node() {
            // Lower every interpolation part (`#{call}`) so its calls are walked.
            let _parts: Vec<NodeId> = interp
                .parts()
                .iter()
                .map(|p| self.lower_node(&p))
                .collect();
            return self.push(Node::Other {
                span: span_of(&interp.location()),
            });
        }
        if let Some(interp) = node.as_interpolated_symbol_node() {
            let _parts: Vec<NodeId> = interp
                .parts()
                .iter()
                .map(|p| self.lower_node(&p))
                .collect();
            return self.push(Node::Other {
                span: span_of(&interp.location()),
            });
        }
        if let Some(embedded) = node.as_embedded_statements_node() {
            // The `#{ … }` inside a string: lower its statements.
            if let Some(s) = embedded.statements() {
                self.lower_body(&s.body());
            }
            return self.push(Node::Other {
                span: span_of(&embedded.location()),
            });
        }

        // Anything outside the handled subset lowers to a span-only placeholder
        // so the walk is total (ADR-0012/0016).
        self.push(Node::Other { span })
    }

    /// Lower a Prism `NodeList` body (statement sequence) into owned ids in
    /// source order — the order inference relies on to populate the env.
    fn lower_body(&mut self, body: &ruby_prism::NodeList<'_>) -> Vec<NodeId> {
        body.iter().map(|n| self.lower_node(&n)).collect()
    }

    /// Lower an *optional* body node (a `def`/`class`/`module`/block body, which
    /// Prism types as `Option<Node>`). A `StatementsNode` body is flattened to
    /// its statement ids so each lands in the arena individually; a `BeginNode`
    /// body (present when the method has an inline `rescue`/`ensure`) or any
    /// other single node is lowered as one id. `None` (empty body) yields `[]`.
    fn lower_optional_body(&mut self, body: Option<&PrismNode<'_>>) -> Vec<NodeId> {
        match body {
            None => Vec::new(),
            Some(node) => {
                if let Some(stmts) = node.as_statements_node() {
                    self.lower_body(&stmts.body())
                } else {
                    vec![self.lower_node(node)]
                }
            }
        }
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
    fn lowers_call_positional_arguments() {
        // `s.include?("e", "x")` lowers two positional string-literal args, in
        // source order, as children of the Call node.
        let src = b"s = \"Hello\"\ns.include?(\"e\", \"x\")\n";
        let result = crate::parse(src);
        let ast = lower(&result);

        let args = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::Call { method, args, .. } if method == "include?" => Some(args.clone()),
                _ => None,
            })
            .expect("expected an include? Call node");
        assert_eq!(args.len(), 2, "expected two positional args");
        let vals: Vec<String> = args
            .iter()
            .map(|id| match ast.get(*id) {
                Node::StringLit { value, .. } => value.clone(),
                other => panic!("expected StringLit arg, got {other:?}"),
            })
            .collect();
        assert_eq!(vals, vec!["e".to_string(), "x".to_string()]);
    }

    #[test]
    fn lowers_nil_true_false_symbol_float_literals() {
        let src = b"nil\ntrue\nfalse\n:foo\n3.5\n";
        let result = crate::parse(src);
        let ast = lower(&result);
        assert!(ast.iter().any(|(_, n)| matches!(n, Node::NilLit { .. })));
        assert!(ast.iter().any(|(_, n)| matches!(n, Node::TrueLit { .. })));
        assert!(ast.iter().any(|(_, n)| matches!(n, Node::FalseLit { .. })));
        assert!(ast
            .iter()
            .any(|(_, n)| matches!(n, Node::SymbolLit { value, .. } if value == "foo")));
        assert!(ast
            .iter()
            .any(|(_, n)| matches!(n, Node::FloatLit { value, .. } if (*value - 3.5).abs() < f64::EPSILON)));
    }

    #[test]
    fn lowering_is_total_for_unhandled_nodes() {
        // A construct still outside the owned subset (a `yield`) must lower
        // without panicking, landing in `Other`.
        let src = b"def foo; yield; end\n";
        let result = crate::parse(src);
        let ast = lower(&result);
        assert!(!ast.is_empty());
        assert!(ast.iter().any(|(_, n)| matches!(n, Node::Other { .. })));
    }

    /// True iff the arena contains a `Call` to `method`.
    fn has_call(ast: &LoweredAst, method: &str) -> bool {
        ast.iter()
            .any(|(_, n)| matches!(n, Node::Call { method: m, .. } if m == method))
    }

    #[test]
    fn lowers_call_inside_method_def() {
        // A call in a `def` body must reach the arena (the whole point).
        let src = b"def slug(t)\n  t.downcase\nend\n";
        let ast = lower(&crate::parse(src));
        assert!(
            ast.iter().any(|(_, n)| matches!(n, Node::Definition { .. })),
            "expected a Definition node for the def"
        );
        assert!(has_call(&ast, "downcase"), "call inside def must be lowered");
    }

    #[test]
    fn lowers_calls_inside_if_and_else_branches() {
        let src = b"if x\n  a.foo\nelse\n  b.bar\nend\n";
        let ast = lower(&crate::parse(src));
        assert!(ast.iter().any(|(_, n)| matches!(n, Node::If { .. })));
        assert!(has_call(&ast, "foo"), "then-branch call must be lowered");
        assert!(has_call(&ast, "bar"), "else-branch call must be lowered");
    }

    #[test]
    fn lowers_calls_inside_case_when_branches() {
        let src = b"case v\nwhen 1\n  a.foo\nwhen 2\n  b.bar\nelse\n  c.baz\nend\n";
        let ast = lower(&crate::parse(src));
        assert!(ast.iter().any(|(_, n)| matches!(n, Node::Case { .. })));
        assert!(has_call(&ast, "foo"));
        assert!(has_call(&ast, "bar"));
        assert!(has_call(&ast, "baz"));
    }

    #[test]
    fn lowers_calls_inside_loops_and_begin_rescue() {
        let w = lower(&crate::parse(b"while x\n  a.foo\nend\n"));
        assert!(w.iter().any(|(_, n)| matches!(n, Node::Loop { .. })));
        assert!(has_call(&w, "foo"));

        let b = lower(&crate::parse(b"begin\n  a.foo\nrescue => e\n  b.bar\nensure\n  c.baz\nend\n"));
        assert!(b.iter().any(|(_, n)| matches!(n, Node::BeginRescue { .. })));
        assert!(has_call(&b, "foo"));
        assert!(has_call(&b, "bar"));
        assert!(has_call(&b, "baz"));
    }

    #[test]
    fn lowers_call_inside_block_body() {
        // `[1,2].each { |n| n.foo }` — the block's inner call must be lowered.
        let src = b"[1, 2].each { |n| n.foo }\n";
        let ast = lower(&crate::parse(src));
        assert!(has_call(&ast, "foo"), "block-body call must be lowered");
        // The outer `each` call carries the block body ids.
        let has_block = ast.iter().any(|(_, n)| {
            matches!(n, Node::Call { method, block_body, .. } if method == "each" && !block_body.is_empty())
        });
        assert!(has_block, "the each call should record its block body");
    }

    #[test]
    fn lowers_array_and_hash_literals() {
        let a = lower(&crate::parse(b"[1, 2, 3]\n"));
        assert!(a.iter().any(|(_, n)| matches!(n, Node::ArrayLit { .. })));
        let h = lower(&crate::parse(b"{ a: 1, b: 2 }\n"));
        assert!(h.iter().any(|(_, n)| matches!(n, Node::HashLit { .. })));
    }

    #[test]
    fn lowers_logical_operands() {
        // Both sides of `&&` must be reachable.
        let src = b"a.foo && b.bar\n";
        let ast = lower(&crate::parse(src));
        assert!(ast.iter().any(|(_, n)| matches!(n, Node::Logical { .. })));
        assert!(has_call(&ast, "foo"));
        assert!(has_call(&ast, "bar"));
    }

    #[test]
    fn lowers_ivar_and_constant_writes_recursively() {
        // The assigned value's call must be lowered.
        let iv = lower(&crate::parse(b"@x = a.foo\n"));
        assert!(iv.iter().any(|(_, n)| matches!(n, Node::VariableWrite { .. })));
        assert!(has_call(&iv, "foo"));
        let cw = lower(&crate::parse(b"FOO = a.bar\n"));
        assert!(cw.iter().any(|(_, n)| matches!(n, Node::ConstantWrite { .. })));
        assert!(has_call(&cw, "bar"));
    }

    #[test]
    fn lowers_call_inside_string_interpolation() {
        let src = b"x = \"hi #{a.foo}\"\n";
        let ast = lower(&crate::parse(src));
        assert!(has_call(&ast, "foo"), "interpolated call must be lowered");
    }
}
