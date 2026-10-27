//! The lowering walk: `Builder` turns one borrowed Prism node into owned arena
//! nodes (`lower_node`).

mod calls;
mod expressions;

use crate::ruby_prism::{self, Node as PrismNode};

use super::operand_effects::{self, OperandMode};
use super::{
    all_param_names, block_param_names, body_has_explicit_return, collect_defined_operand_children,
    collect_recoverable_children, constant_node_name, constant_path_string, constant_string,
    direct_method_names, discover_visibilities_and_includes, for_index_writes, lower_multi_targets,
    rescue_reference_index_writes,
    case_arm_exits, has_targeted_break, has_targeted_next, has_targeted_retry,
    literal_truthiness, unconditional_exit,
    param_shape_of, plain_positional_params, rooted_constant_path, self_anchored_constant_path,
    span_of, strict_constant_path_string, Compound, JumpKind, Node, NodeId, ParamShape,
    Recovered, RescueClause, ScopeMarks, Span, StatementsKind,
};

/// What a family method of [`Builder::lower_node`] returns for a node that is
/// not one of its types. `push` never mints it: its `debug_assert!` keeps
/// every id below `u32::MAX`, which no real arena reaches.
const UNMATCHED: NodeId = NodeId(u32::MAX);

/// Mutable accumulator for the owned arena during the lowering walk.
pub(crate) struct Builder<'src> {
    pub(crate) nodes: Vec<Node>,
    /// The full parsed source, for capturing verbatim key slices + line numbers.
    pub(crate) source: &'src [u8],
    /// Byte offset of every line start (index 0 = line 1).
    pub(crate) line_starts: Vec<usize>,
    /// Arena ids a `(e)` unwrap returned — see
    /// [`LoweredAst::paren_unwrapped`].
    ///
    /// [`LoweredAst::paren_unwrapped`]: crate::ast::LoweredAst::paren_unwrapped
    pub(crate) paren_unwrapped: Vec<u32>,
    /// `(arena id, bound names)` for recovered children the recovery walk
    /// reached by CROSSING a `BlockNode`/`LambdaNode` — the closure's `locals`
    /// still shadow the enclosing scope inside the recovered subtree
    /// (rigor-rs#137, upstream rigor#1245). See [`LoweredAst::closure_bindings`].
    ///
    /// [`LoweredAst::closure_bindings`]: crate::ast::LoweredAst::closure_bindings
    pub(crate) closure_bindings: Vec<(NodeId, Vec<String>)>,
    /// Depth of JOINED recovered children being lowered — the mark
    /// [`Recovered::joined`] carried into `lower_recovered`. A nested
    /// recovery run under one (a `Recovered` carrier's own collect, a
    /// `defined?` operand, a multi-write target's embedded exprs) inherits
    /// the mark, so a compound index write buried there still lowers to a
    /// widening `Node::IndexWrite` rather than being descended through
    /// (rigor-rs#312).
    ///
    /// [`Recovered::joined`]: super::Recovered::joined
    pub(crate) recovery_joined: u32,
    /// Depth of BLOCKED recovered children being lowered — the mark
    /// [`Recovered::blocked`] carried into `lower_recovered`. Under it a
    /// nested recovery run cannot re-arm the joined widening: the position
    /// never reaches a merge.
    ///
    /// [`Recovered::blocked`]: super::Recovered::blocked
    pub(crate) recovery_blocked: u32,
    /// Depth of ITERATED-body recovered children being lowered — the mark
    /// [`Recovered::iterative`] carried into `lower_recovered`, and set by
    /// `lower_node` itself while a `while`/`until` body or a block/lambda
    /// body lowers: the reference's content-writeback text scan
    /// (`loop_content_writeback`, `content_writeback_block_captures`)
    /// lands every compound index write in that text whatever wraps it
    /// (rigor-rs#312).
    ///
    /// [`Recovered::iterative`]: super::Recovered::iterative
    pub(crate) recovery_iterative: u32,
    /// Depth of `next`-sink bodies being lowered — [`Recovered::next_sink`]:
    /// a `for` body, whose `next` arms re-merge through `loop_iteration`
    /// even though it has no content writeback.
    ///
    /// [`Recovered::next_sink`]: super::Recovered::next_sink
    pub(crate) recovery_next_sink: u32,
    /// Depth of fresh-local-scope recovered children —
    /// [`Recovered::suppressed`].
    ///
    /// [`Recovered::suppressed`]: super::Recovered::suppressed
    pub(crate) recovery_suppressed: u32,
    /// Depth of typed-operand positions the lowering sits inside — a call
    /// receiver or argument, a splat's contents, a literal-container
    /// element, an interpolation part, a `rescue`-modifier operand. The
    /// reference gates each such ELEMENT on `OperandEffects.any?`
    /// (`thread_operand`, `statement_evaluator.rb:2509-2535`): an element
    /// whose subtree carries an outliving effect is `evaluate`d, the rest
    /// `type_of`'d. `lower_typed_operand` applies that gate per operand
    /// child, so this depth only reads > 0 while a GATED-IN container,
    /// sequence or call operand lowers (rigor-rs#343, #361).
    ///
    /// [`Recovered::typed`]: super::Recovered::typed
    pub(crate) typed_depth: u32,
    /// Depth of never-evaluated operand positions the lowering sits
    /// inside — the `dead` half of `thread_operand`'s gate: an operand
    /// element without an outliving effect, plus the children an
    /// evaluator `type_of`s even when it runs (a `return` operand's
    /// `jump_value_type`, an `in` pattern's captured bindings, an index
    /// write's receiver/indices, a compound attribute write's own
    /// receiver and value). No `eval_*` handler ever runs inside one, so
    /// `Node::AttrWrite`'s `evaluated` flag is `false` at depth > 0 and
    /// `OperandEffects.any?` cannot rescue it (rigor-rs#361).
    ///
    /// [`Recovered::dead`]: super::Recovered::dead
    pub(crate) dead_operand: u32,
    /// Depth of DEFERRED bodies the lowering sits inside — a literal
    /// block or lambda body. A compound attribute write there still
    /// evaluates, but `widen_attribute_write` lands on the block's own
    /// scope and `evaluate_invocation`'s `OperandEffects.any?` writeback
    /// check excludes it, so it never lands here either (rigor-rs#343).
    ///
    /// [`Recovered::closure`]: super::Recovered::closure
    pub(crate) closure_depth: u32,
    /// Spans of `Statements{Inert}` carriers (`super`/`yield`/`BEGIN`/`END`
    /// operands) emitted while inside an iterated body — the operand is
    /// still never evaluated, but a content-writeback text scan covers it
    /// (rigor-rs#312; see [`LoweredAst::in_scanned_inert_carrier`]).
    ///
    /// [`LoweredAst::in_scanned_inert_carrier`]:
    ///     crate::ast::LoweredAst::in_scanned_inert_carrier
    pub(crate) scanned_inert_spans: Vec<Span>,
    /// The spans of recovered children the walk reached inside a position
    /// whose post-scope the reference DISCARDS or never evaluates
    /// ([`Recovered::blocked`]: a `when`/`in` condition or guard, an
    /// unconditionally-exiting arm, a `super`/`yield`/`BEGIN`/`END` operand
    /// under a wrapper, the dead side of a constant or short-circuit fold).
    /// A local write in one binds nothing — the lowering path's sibling of
    /// `inert_spans` — so reach/flow scans must not collect it
    /// (rigor-rs#357).
    ///
    /// [`Recovered::blocked`]: crate::ast::Recovered::blocked
    pub(crate) blocked_spans: Vec<Span>,
    /// The spans of positions the reference EVALUATES but whose post-scope
    /// the join drops — terminated `if`/`case`/`in`/rescue-clause arms and
    /// exiting `and`/`or` right sides (rigor-rs#362). Not a carrier: reads
    /// inside still witness the position's writes;
    /// [`DeadPositions::write_reaches`] scopes them per read.
    ///
    /// [`DeadPositions::write_reaches`]: rigor_infer::dead::DeadPositions::write_reaches
    pub(crate) discarded_spans: Vec<Span>,
    /// The `blocked_spans` subset inside an iterated body the writeback TEXT
    /// scan still covers ([`Recovered::iterative`]): a local rebind there
    /// still binds nothing, but a `[]=`/mutator content mark lands on the
    /// reference — `while w; super(h[:a] ||= 1); end` under a wrapper
    /// (rigor-rs#312).
    ///
    /// [`Recovered::iterative`]: crate::ast::Recovered::iterative
    pub(crate) blocked_iterative_spans: Vec<Span>,
    /// Depth of `while`/`until`/`for` bodies the lowering sits inside —
    /// counted so a `break` argument under one is known to land its
    /// scope on the loop's break join. `join_break_scopes`
    /// (`statement_evaluator.rb`) unions the break scope's LOCAL
    /// bindings only: the indexed-narrowing table is not part of the
    /// join, so an attribute write's record drop in that scope never
    /// propagates (rigor-rs#363). Masked inside block/lambda bodies —
    /// a `break` there targets the yielding call, not the loop.
    pub(crate) lexical_loop: u32,
    /// The spans of `break` nodes whose argument scope lands on a
    /// lexical loop's locals-only break join ([`Self::lexical_loop`] —
    /// rigor-rs#363; [`LoweredAst::in_locals_only_carrier`]). Split by
    /// whether an iterated body encloses the span: `while`/`until` run
    /// `loop_content_writeback`, which text-scans the whole body — even
    /// inside a nested `for` — and re-joins the CONTENT_MUTATORS
    /// mutators' stored arguments into the continuation binding, so a
    /// later element read declines anyway. `for` alone collects no
    /// writeback, so `bare_locals_only_spans` (no enclosing iterated
    /// body) suppresses every mutator's drop.
    ///
    /// [`LoweredAst::in_locals_only_carrier`]:
    ///     crate::ast::LoweredAst::in_locals_only_carrier
    pub(crate) locals_only_spans: Vec<Span>,
    /// [`Self::locals_only_spans`] under no iterated body — `eval_for`
    /// joins break scopes through `join_break_scopes` but runs no
    /// `loop_content_writeback`, so nothing re-joins an adder mark's
    /// store evidence (rigor-rs#363).
    pub(crate) bare_locals_only_spans: Vec<Span>,
    /// Every body span lowered under `recovery_iterative` — `while`/
    /// `until` bodies and block/lambda bodies: the regions the
    /// reference's content-writeback TEXT scans cover
    /// (`loop_content_writeback`, `content_writeback_block_captures`).
    /// A scan-visible content mutation inside one lands its drop on the
    /// post-region scope even when the write's own position is
    /// never-evaluated or join-dropped — a folded `if` arm, a dead
    /// `rescue` clause — because the scan reads the body textually
    /// (rigor-rs#363 review).
    pub(crate) iterated_body_spans: Vec<Span>,
    /// The STACK of enclosing lexical-loop body spans — pushed on each
    /// `while`/`until`/`for` `lexical_loop` bump — so a valued `next`
    /// pairs its argument with the INNERMOST body's join it lands on
    /// (`while c; for i; next w; end; h[:a].m` reaches the in-body read
    /// past the `for` — rigor-rs#363 review). A body-less loop pushes a
    /// degenerate span so the stack stays aligned with the counter.
    pub(crate) lexical_loop_bodies: Vec<Span>,
    /// `(argument span, owning lexical-loop body span)` for every valued
    /// `next` under a lexical loop — `DeadPositions::mutation_reaches`
    /// consumes the pair: a `next` argument joins the iteration fixpoint
    /// (its marks DO land post-loop, unlike `break`'s locals-only join),
    /// but a use the owning body still contains evaluates pre-join and
    /// keeps the record — `next (h[:a] = 9; h[:a].m)` fires in-argument
    /// on the oracle (rigor-rs#363 review).
    pub(crate) next_arg_spans: Vec<(Span, Span)>,
}

impl<'src> Builder<'src> {
    /// Push an owned node, returning its fresh [`NodeId`].
    fn push(&mut self, node: Node) -> NodeId {
        debug_assert!(self.nodes.len() < u32::MAX as usize);
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(node);
        id
    }

    /// The 1-based source line of a byte offset (binary search over line starts).
    pub(crate) fn line_at(&self, offset: usize) -> u32 {
        self.line_starts.partition_point(|&ls| ls <= offset) as u32
    }

    /// Lower one borrowed Prism node (and its children) into owned nodes,
    /// returning the id of the produced root. Recursion is bounded by the
    /// source's nesting depth.
    pub(crate) fn lower_node(&mut self, node: &PrismNode<'_>) -> NodeId {
        let span = span_of(&node.location());
        let id = self.lower_calls(node, span);
        if id != UNMATCHED {
            return id;
        }
        let id = self.lower_expressions(node);
        if id != UNMATCHED {
            return id;
        }
        let id = self.lower_control(node, span);
        if id != UNMATCHED {
            return id;
        }
        let id = self.lower_definitions(node);
        if id != UNMATCHED {
            return id;
        }
        self.lower_jumps(node, span)
    }

    /// [`Self::lower_node`]'s definition arms: `def`, `class`, `module` and
    /// `class << self`.
    #[inline(never)]
    fn lower_definitions(&mut self, node: &PrismNode<'_>) -> NodeId {
        if let Some(def) = node.as_def_node() {
            // Lower the method body so its calls reach the walk. Parameters are
            // intentionally NOT bound to any type: an unknown local read is
            // already `Dynamic[top]` (silent), the zero-FP-safe choice — binding
            // a param to a guessed type could mint a false `undefined-method`.
            let body = self.lower_optional_body(def.body().as_ref());
            // Retain the method name (None for a receiver-bearing `def self.x` /
            // `def obj.x` — a singleton method, never an instance method, so it
            // must not be harvested as a tier-4b instance-method body) and whether
            // any explicit `return` appears in the body (the tier-4b decline gate).
            let name = def
                .receiver()
                .is_none()
                .then(|| constant_string(def.name().as_slice()));
            let has_explicit_return = def
                .body()
                .as_ref()
                .map(body_has_explicit_return)
                .unwrap_or(false);
            // The plain-positional param names (for tier-4b call-site binding),
            // or `None` to decline when the signature has anything that breaks
            // positional index<->arg alignment (splat/post/kwargs/block/optional).
            let params = plain_positional_params(def.parameters().as_ref());
            // The full RBS-relevant param structure (for sig-gen's initialize stub).
            let param_shape = param_shape_of(def.parameters().as_ref());
            // The method-NAME token span (for the override-visibility rule's
            // diagnostic anchor); `None` for a receiver-bearing singleton def
            // (kept parallel to `name`, which is also `None` there).
            let name_span = def
                .receiver()
                .is_none()
                .then(|| span_of(&def.name_loc()));
            // A receiver-bearing def (`def recv.x`) evaluates `recv` in the
            // ENCLOSING scope. Lower it so its reads are visible — otherwise a
            // `def local.m` looks like `local` is assigned-but-never-read
            // (flow.dead-assignment FP, real-corpus audit: textbringer). The node
            // lives in the arena; the span-scan analyses find it (orphan-proof).
            if let Some(recv) = def.receiver() {
                let _ = self.lower_node(&recv);
            }
            // C2: lower each parameter DEFAULT-VALUE expression as an arena node
            // so the call rules reach it (the reference checks positional and
            // keyword defaults, incl. nested calls within — `def f(t =
            // Time.current)`, `def g(a: [1,2].frob)`). Orphaned like the
            // receiver above: it lives in the arena so the span-scan / `ast.iter`
            // call walk finds it, but is not a body statement (so a default's
            // write is not mis-attributed to the method body). Params themselves
            // stay unbound (Dynamic ⇒ silent), so only a literal/constant
            // receiver in a default is ever witnessed — the FP-safe subset.
            if let Some(params) = def.parameters() {
                for opt in params.optionals().iter() {
                    if let Some(o) = opt.as_optional_parameter_node() {
                        let _ = self.lower_node(&o.value());
                    }
                }
                for kw in params.keywords().iter() {
                    if let Some(o) = kw.as_optional_keyword_parameter_node() {
                        let _ = self.lower_node(&o.value());
                    }
                }
            }
            // A `def self.x` (SELF receiver) captures its method name here so
            // `sig-gen` can collect the singleton; `name` stays `None` so the
            // instance-method harvest still skips it. A non-self receiver
            // (`def obj.x`) is left `None`.
            let singleton_name = def
                .receiver()
                .filter(|r| r.as_self_node().is_some())
                .map(|_| constant_string(def.name().as_slice()));
            // The mirror for a NON-self receiver (`def IO.console_size`). Its
            // rendered constant path rides along so the def-attribution walk
            // can apply `def_receiver_targets_lexical_self?` (`def Object.x`
            // inside `Object.class_eval` is a singleton def, not `Object#x`).
            let def_receiver_path = def
                .receiver()
                .filter(|r| r.as_self_node().is_none())
                .map(|r| constant_path_string(&r))
                .filter(|p| !p.is_empty());
            let receiver_def_name = def
                .receiver()
                .filter(|r| r.as_self_node().is_none())
                .map(|_| constant_string(def.name().as_slice()));
            return self.push(Node::Definition {
                name,
                is_singleton_class: false,
                singleton_name,
                receiver_def_name,
                def_receiver_path,
                param_span: def.parameters().as_ref().map(|p| span_of(&p.location())),
                has_explicit_return,
                params,
                param_shape,
                param_names: all_param_names(def.parameters().as_ref()),
                name_span,
                singleton_operand: None,
                body,
                span: span_of(&def.location()),
            });
        }

        if let Some(class) = node.as_class_node() {
            // The constant-path name (`Point`, `Foo::Bar`). The superclass name,
            // if a `< Bar` clause is written (a bare const or a const path; its
            // last component is what the source-chain walk keys on). The instance
            // methods are the `def` names defined directly in the body — read
            // from Prism BEFORE lowering, since lowering erases a def's name.
            let name = constant_path_string(&class.constant_path());
            let superclass = class
                .superclass()
                .and_then(|s| constant_node_name(&s));
            // The FULL written superclass path (for the override-visibility walk).
            let superclass_path = class.superclass().map(|s| constant_path_string(&s)).filter(|s| !s.is_empty());
            let methods = class
                .body()
                .as_ref()
                .map(direct_method_names)
                .unwrap_or_default();
            let body = self.lower_optional_body(class.body().as_ref());
            // Harvest per-method bodies for tier-4b RETURN inference. The DIRECT
            // children of the lowered class body (lower_optional_body flattens the
            // Statements wrapper) are exactly the body's top-level statements, so
            // a direct, named `Definition` among them is a direct instance method
            // — the same inclusion rule as `direct_method_names` (`def self.x`
            // lowers to a name-less Definition and is skipped; a def nested in a
            // conditional/inner class is not a direct child and is skipped).
            let method_bodies = self.harvest_method_bodies(&body);
            // ADR-35 slice 1: the source-discovered instance-method visibility
            // table + include/prepend names, read from the Prism body BEFORE
            // lowering (lowering erases the modifier-call/`def`-name structure
            // the discovery needs). Mirrors `scope_indexer.rb` exactly.
            let (method_visibilities, includes) = class
                .body()
                .as_ref()
                .map(discover_visibilities_and_includes)
                .unwrap_or_default();
            return self.push(Node::ClassDef {
                name,
                rooted: rooted_constant_path(&class.constant_path()),
                self_anchored: self_anchored_constant_path(&class.constant_path()),
                superclass,
                superclass_path,
                methods,
                method_bodies,
                method_visibilities,
                includes,
                body,
                span: span_of(&class.location()),
            });
        }

        if let Some(module) = node.as_module_node() {
            let name = constant_path_string(&module.constant_path());
            let methods = module
                .body()
                .as_ref()
                .map(direct_method_names)
                .unwrap_or_default();
            let body = self.lower_optional_body(module.body().as_ref());
            let method_bodies = self.harvest_method_bodies(&body);
            let (method_visibilities, includes) = module
                .body()
                .as_ref()
                .map(discover_visibilities_and_includes)
                .unwrap_or_default();
            return self.push(Node::ModuleDef {
                name,
                rooted: rooted_constant_path(&module.constant_path()),
                self_anchored: self_anchored_constant_path(&module.constant_path()),
                methods,
                method_bodies,
                method_visibilities,
                includes,
                body,
                span: span_of(&module.location()),
            });
        }

        if let Some(sclass) = node.as_singleton_class_node() {
            let operand = self.lower_node(&sclass.expression());
            let body = self.lower_optional_body(sclass.body().as_ref());
            return self.push(Node::Definition {
                name: None, // `class << self` has no single method name.
                is_singleton_class: true, // a CLASS scope, not a method def.
                singleton_name: None, // the BODY's inner defs are the singletons.
                receiver_def_name: None,
                def_receiver_path: None,
                param_span: None,
                has_explicit_return: false,
                params: None,    // no single method ⇒ no param binding.
                param_shape: ParamShape::default(),
                param_names: Vec::new(),
                name_span: None, // no single name ⇒ no name span.
                singleton_operand: Some(operand),
                body,
                span: span_of(&sclass.location()),
            });
        }

        UNMATCHED
    }

    /// [`Self::lower_node`]'s control-flow arms: `if`/`unless`/`else`,
    /// `case`/`when`/`in`, the loops, `begin`/`rescue`/`ensure`, `and`/`or`.
    #[inline(never)]
    fn lower_control(&mut self, node: &PrismNode<'_>, span: Span) -> NodeId {
        if let Some(if_node) = node.as_if_node() {
            // `if` / ternary. Prism's ternary is also an IfNode. Arms whose
            // scope the join discards carry the blocked counter, mirroring
            // the Collector's `visit_if_node` (rigor-rs#362): a
            // constant-folded predicate's dead arm never exists
            // (`lower_blocked` — never evaluated), and an arm that exits
            // unconditionally while the other side lives contributes no
            // scope downstream (`lower_discarded` — evaluated, so in-arm
            // reads still witness its writes) — `if c; h.default ||= 0;
            // return; end` keeps the receiver's indexed narrowings on the
            // oracle.
            let predicate = self.lower_node(&if_node.predicate());
            let statements = if_node.statements();
            // `subsequent` is the `elsif`/`else` chain (an IfNode or ElseNode).
            let subsequent = if_node.subsequent();
            let (then_body, else_body) = match literal_truthiness(&if_node.predicate()) {
                Some(true) => (
                    statements.map(|s| self.lower_body(&s.body())),
                    subsequent.map(|sub| vec![self.lower_blocked(&sub)]),
                ),
                Some(false) => (
                    statements.map(|s| self.lower_body_blocked(&s.body())),
                    subsequent.map(|sub| vec![self.lower_node(&sub)]),
                ),
                None => {
                    let then_dropped = statements
                        .as_ref()
                        .is_some_and(|s| {
                            unconditional_exit(&s.as_node()) && !self.arm_lands(&s.as_node())
                        })
                        && subsequent.is_none();
                    let else_dropped = subsequent
                        .as_ref()
                        .is_some_and(|s| unconditional_exit(s) && !self.arm_lands(s))
                        && statements.is_some();
                    let then_body = statements.map(|s| {
                        if then_dropped {
                            self.lower_body_discarded(&s.body())
                        } else {
                            self.lower_body(&s.body())
                        }
                    });
                    let else_body = subsequent.map(|sub| {
                        if else_dropped {
                            vec![self.lower_discarded(&sub)]
                        } else {
                            vec![self.lower_node(&sub)]
                        }
                    });
                    (then_body, else_body)
                }
            };
            return self.push(Node::If {
                predicate,
                then_body: then_body.unwrap_or_default(),
                else_body: else_body.unwrap_or_default(),
                is_unless: false,
                span: span_of(&if_node.location()),
            });
        }

        if let Some(unless_node) = node.as_unless_node() {
            // Same as `if` with the truthy/falsey edges swapped — `unless`
            // runs its body when the predicate is FALSEY.
            let predicate = self.lower_node(&unless_node.predicate());
            let statements = unless_node.statements();
            let else_clause = unless_node.else_clause();
            let (then_body, else_body) = match literal_truthiness(&unless_node.predicate()) {
                Some(false) => (
                    statements.map(|s| self.lower_body(&s.body())),
                    else_clause.map(|e| vec![self.lower_blocked(&e.as_node())]),
                ),
                Some(true) => (
                    statements.map(|s| self.lower_body_blocked(&s.body())),
                    else_clause.map(|e| vec![self.lower_node(&e.as_node())]),
                ),
                None => {
                    let body_dropped = statements
                        .as_ref()
                        .is_some_and(|s| {
                            unconditional_exit(&s.as_node()) && !self.arm_lands(&s.as_node())
                        })
                        && else_clause.is_none();
                    let else_dropped = else_clause
                        .as_ref()
                        .is_some_and(|e| {
                            unconditional_exit(&e.as_node()) && !self.arm_lands(&e.as_node())
                        })
                        && statements.is_some();
                    let then_body = statements.map(|s| {
                        if body_dropped {
                            self.lower_body_discarded(&s.body())
                        } else {
                            self.lower_body(&s.body())
                        }
                    });
                    let else_body = else_clause.map(|e| {
                        if else_dropped {
                            vec![self.lower_discarded(&e.as_node())]
                        } else {
                            vec![self.lower_node(&e.as_node())]
                        }
                    });
                    (then_body, else_body)
                }
            };
            return self.push(Node::If {
                predicate,
                then_body: then_body.unwrap_or_default(),
                else_body: else_body.unwrap_or_default(),
                is_unless: true,
                span: span_of(&unless_node.location()),
            });
        }

        if let Some(else_node) = node.as_else_node() {
            // An `else` clause body (reached via an If/Unless subsequent).
            let body = else_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            let main_body = body.clone();
            return self.push(Node::BeginRescue {
                body,
                main_body,
                ensure_body: Vec::new(),
                clauses: Vec::new(),
                span: span_of(&else_node.location()),
            });
        }

        if let Some(case_node) = node.as_case_node() {
            // `case`/`when`. Lower the subject, every `when` (conditions + body),
            // and the `else`. `visit_case_arms`'s live/dead split applies
            // here too (`no_match_path = true`: an absent `else` is a real
            // fall-through arm): a terminated arm contributes no scope to
            // the post-`case` join, so its writes are `blocked` — unless it
            // still lands downstream (`arm_lands`) or no arm lives at all.
            let predicate = case_node.predicate().map(|p| self.lower_node(&p));
            let arms: Vec<PrismNode> = case_node.conditions().iter().collect();
            let else_dead = case_node
                .else_clause()
                .is_some_and(|e| case_arm_exits(&e.as_node()) && !self.arm_lands(&e.as_node()));
            let live = arms
                .iter()
                .filter(|a| !case_arm_exits(a) || self.arm_lands(a))
                .count()
                + usize::from(!else_dead);
            let mut branches = Vec::new();
            for arm in &arms {
                let dead = case_arm_exits(arm) && !self.arm_lands(arm);
                if dead && live != 0 {
                    branches.push(self.lower_discarded(arm));
                } else {
                    branches.push(self.lower_node(arm));
                }
            }
            let else_dead_and_dropped = else_dead && live != 0;
            let else_body = case_node
                .else_clause()
                .and_then(|e| e.statements())
                .map(|s| {
                    if else_dead_and_dropped {
                        self.lower_body_discarded(&s.body())
                    } else {
                        self.lower_body(&s.body())
                    }
                })
                .unwrap_or_default();
            return self.push(Node::Case {
                predicate,
                branches,
                else_body,
                span: span_of(&case_node.location()),
            });
        }

        if let Some(case_match) = node.as_case_match_node() {
            // `case`/`in` pattern matching. Same shape as CaseNode — with
            // `no_match_path = false` (`case`/`in` without `else` raises
            // `NoMatchingPatternError` on no match, so an absent `else`
            // contributes no live arm).
            let predicate = case_match.predicate().map(|p| self.lower_node(&p));
            let arms: Vec<PrismNode> = case_match.conditions().iter().collect();
            let else_dead = match case_match.else_clause() {
                Some(e) => case_arm_exits(&e.as_node()) && !self.arm_lands(&e.as_node()),
                None => true,
            };
            let live = arms
                .iter()
                .filter(|a| !case_arm_exits(a) || self.arm_lands(a))
                .count()
                + usize::from(!else_dead);
            let mut branches = Vec::new();
            for arm in &arms {
                let dead = case_arm_exits(arm) && !self.arm_lands(arm);
                if dead && live != 0 {
                    branches.push(self.lower_discarded(arm));
                } else {
                    branches.push(self.lower_node(arm));
                }
            }
            let else_dead_and_dropped = else_dead && live != 0;
            let else_body = case_match
                .else_clause()
                .and_then(|e| e.statements())
                .map(|s| {
                    if else_dead_and_dropped {
                        self.lower_body_discarded(&s.body())
                    } else {
                        self.lower_body(&s.body())
                    }
                })
                .unwrap_or_default();
            return self.push(Node::Case {
                predicate,
                branches,
                else_body,
                span: span_of(&case_match.location()),
            });
        }

        if let Some(when_node) = node.as_when_node() {
            // A `when` branch: lower its condition expressions and body into the
            // dedicated `When` variant's SEPARATE lists (pre-split, both were
            // concatenated into a reused `BeginRescue` carrier's body). The
            // conditions are `blocked` — `Narrowing.case_when_scopes`
            // shape-narrows them without `sub_eval`, so a write inside lands
            // no scope effect (rigor-rs#362); the Collector's
            // `visit_case_arm_parts` marks them the same way.
            let conditions: Vec<NodeId> = when_node
                .conditions()
                .iter()
                .map(|c| self.lower_blocked(&c))
                .collect();
            let body = when_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            return self.push(Node::When {
                conditions,
                body,
                span: span_of(&when_node.location()),
            });
        }

        if let Some(in_node) = node.as_in_node() {
            // An `in` pattern branch: lower the pattern and the body. The
            // pattern's bindings write locals the lowering cannot name — mark
            // the clause so the per-element block fold declines rather than
            // answer a tail with the pre-bind value (rigor-rs#194).
            self.push(Node::UnmodeledWrite {
                span: span_of(&in_node.location()),
                // The pattern binds real locals on the oracle (`in [s]` is a
                // conditional `s = …`) — name them so the rebind/widen
                // censuses grade the clause like any other write
                // (rigor-rs#352 review).
                names: pattern_bound_locals(&in_node.pattern()),
            });
            // The pattern is matched in `in_arm_position` — typed, never
            // evaluated (rigor-rs#343); it is the dead half of the operand
            // gate — `Narrowing.case_when_scopes` types it only, so no
            // `OperandEffects.any?` rescues a write inside (rigor-rs#361).
            // Also `blocked`, the Collector's mark for the same position.
            self.dead_operand += 1;
            let pattern = self.lower_blocked(&in_node.pattern());
            self.dead_operand -= 1;
            let mut body = vec![pattern];
            if let Some(s) = in_node.statements() {
                body.extend(self.lower_body(&s.body()));
            }
            let main_body = body.clone();
            return self.push(Node::BeginRescue {
                body,
                main_body,
                ensure_body: Vec::new(),
                clauses: Vec::new(),
                span: span_of(&in_node.location()),
            });
        }

        if let Some(while_node) = node.as_while_node() {
            let predicate = Some(self.lower_node(&while_node.predicate()));
            // The body is ITERATED — see `recovery_iterative`: the
            // reference's `loop_content_writeback` text-scans it, so a
            // wrapped compound index write inside widens its receiver
            // whatever position or jump it sits under (rigor-rs#312).
            self.recovery_iterative += 1;
            self.lexical_loop += 1;
            let body_span = if let Some(s) = while_node.statements() {
                let b = span_of(&s.location());
                self.iterated_body_spans.push(b);
                b
            } else {
                (span.0, span.0)
            };
            self.lexical_loop_bodies.push(body_span);
            let body = while_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            self.lexical_loop_bodies.pop();
            self.recovery_iterative -= 1;
            self.lexical_loop -= 1;
            return self.push(Node::Loop {
                predicate,
                body,
                index: Vec::new(),
                index_writes: Vec::new(),
                span: span_of(&while_node.location()),
            });
        }

        if let Some(until_node) = node.as_until_node() {
            let predicate = Some(self.lower_node(&until_node.predicate()));
            self.recovery_iterative += 1;
            self.lexical_loop += 1;
            let body_span = if let Some(s) = until_node.statements() {
                let b = span_of(&s.location());
                self.iterated_body_spans.push(b);
                b
            } else {
                (span.0, span.0)
            };
            self.lexical_loop_bodies.push(body_span);
            let body = until_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            self.lexical_loop_bodies.pop();
            self.recovery_iterative -= 1;
            self.lexical_loop -= 1;
            return self.push(Node::Loop {
                predicate,
                body,
                index: Vec::new(),
                index_writes: Vec::new(),
                span: span_of(&until_node.location()),
            });
        }

        if let Some(for_node) = node.as_for_node() {
            // `for x in coll; …; end`. Lower the collection (a call can live
            // there) and the body. The index target is a write target, not an
            // arena node: only the LOCAL names it binds are recorded, so the flow
            // write collectors see the rebind (rigor-rs#151) — and the `[]=`
            // stores an index-target index performs (`for h[:k] in xs`,
            // rigor-rs#134) widen the receiver's locals through `index_writes`.
            let predicate = Some(self.lower_node(&for_node.collection()));
            // A `for` body joins its fall-through scope into the
            // continuation (`joined`) and re-merges targeting `next` arms
            // through `loop_iteration`'s sink (`next_sink`), but has NO
            // content writeback — `break`/`raise` arms drop
            // (rigor-rs#312).
            self.recovery_joined += 1;
            self.recovery_next_sink += 1;
            // `eval_for` (`statement_evaluator.rb`) joins its break
            // scopes through the same locals-only `join_break_scopes`,
            // so a `break` argument's scope effects never propagate
            // here either (rigor-rs#363).
            self.lexical_loop += 1;
            // `for` bodies are NOT `iterated_body_spans` (no writeback
            // scan) — the stack entry exists only to pair a valued
            // `next` inside with the body it lands under.
            self.lexical_loop_bodies.push(
                for_node
                    .statements()
                    .map(|s| span_of(&s.location()))
                    .unwrap_or((span.0, span.0)),
            );
            let body = for_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            self.lexical_loop_bodies.pop();
            self.recovery_joined -= 1;
            self.recovery_next_sink -= 1;
            self.lexical_loop -= 1;
            let (index, index_writes) = for_index_writes(&for_node.index());
            return self.push(Node::Loop {
                predicate,
                body,
                index,
                index_writes,
                span: span_of(&for_node.location()),
            });
        }

        if let Some(begin_node) = node.as_begin_node() {
            // `begin`/`rescue`/`else`/`ensure`. Collect every sub-body's calls.
            let mut body: Vec<NodeId> = begin_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            // Just the protected body's own statements — the ids `body` will
            // still hold alone once the rescue / else / ensure children are
            // appended below (the `never_completes_normally?` walk must not
            // count an `else` clause's statements toward the body's).
            let main_body = body.clone();
            // Walk the rescue chain (each RescueNode links to the next). Build the
            // per-clause `RescueClause` view ALONGSIDE the flat `body`: every
            // `lower_node`/`lower_body` call happens in the exact same order as
            // before, and each produced id is pushed to `body` exactly as before,
            // so `body` is byte-for-byte unchanged — the clause view just records
            // the SAME ids per clause (no double-lowering).
            let mut clauses: Vec<RescueClause> = Vec::new();
            let mut rescue = begin_node.rescue_clause();
            // A `begin`/`rescue`/`end` keyword clause is EVALUATED — its
            // scope is only dropped at the post-`begin` join — while a
            // rescue MODIFIER's arm (`x = y rescue z`) binds nothing even
            // internally on the oracle (`x = y rescue (q = 2; q.w)` is
            // silent — rigor-rs#362), so dead clauses of the two forms take
            // the two different marks.
            let keyword_begin = begin_node.begin_keyword_loc().is_some();
            while let Some(r) = rescue {
                // A clause whose body exits unconditionally contributes no
                // scope to the post-`begin` join (`live_rescue_results`
                // filters it) — `blocked`, the Collector's `visit_begin_node`
                // mark (rigor-rs#362); a `retry` arm stays live.
                let arm_dead = r
                    .statements()
                    .is_some_and(|s| {
                        unconditional_exit(&s.as_node()) && !self.arm_lands(&s.as_node())
                    });
                let mut exceptions = Vec::new();
                for exc in r.exceptions().iter() {
                    let id = if arm_dead && keyword_begin {
                        self.lower_discarded(&exc)
                    } else if arm_dead {
                        self.lower_blocked(&exc)
                    } else {
                        self.lower_node(&exc)
                    };
                    body.push(id);
                    exceptions.push(id);
                }
                let mut clause_body = Vec::new();
                if let Some(s) = r.statements() {
                    let ids = if arm_dead && keyword_begin {
                        self.lower_body_discarded(&s.body())
                    } else if arm_dead {
                        self.lower_body_blocked(&s.body())
                    } else {
                        self.lower_body(&s.body())
                    };
                    body.extend(ids.iter().copied());
                    clause_body = ids;
                }
                // A dead arm's `=> target` carries no scope either — record
                // its span so an index-target write there stays blocked.
                if arm_dead {
                    if let Some(reference) = r.reference() {
                        let span = span_of(&reference.location());
                        self.blocked_spans.push(span);
                        if self.recovery_iterative > 0 {
                            self.blocked_iterative_spans.push(span);
                        }
                    }
                }
                let bound_name = r
                    .reference()
                    .and_then(|reference| reference.as_local_variable_target_node())
                    .map(|target| constant_string(target.name().as_slice()));
                // `rescue => h[:e]` stores the exception through `[]=` on `h`
                // (rigor-rs#134); a non-index reference contributes nothing.
                let index_writes = r
                    .reference()
                    .map(|reference| rescue_reference_index_writes(&reference))
                    .unwrap_or_default();
                clauses.push(RescueClause {
                    exceptions,
                    body: clause_body,
                    bound_name,
                    index_writes,
                    span: span_of(&r.location()),
                });
                rescue = r.subsequent();
            }
            if let Some(e) = begin_node.else_clause().and_then(|e| e.statements()) {
                body.extend(self.lower_body(&e.body()));
            }
            // Lower the ensure statements ONCE, then record them BOTH in the flat
            // `body` (behavior-preserving for every existing consumer) AND in the
            // dedicated `ensure_body` (the `flow.return-in-ensure` dispatch view).
            let ensure_body = if let Some(e) = begin_node.ensure_clause().and_then(|e| e.statements())
            {
                self.lower_body(&e.body())
            } else {
                Vec::new()
            };
            body.extend(ensure_body.iter().copied());
            return self.push(Node::BeginRescue {
                body,
                main_body,
                ensure_body,
                clauses,
                span: span_of(&begin_node.location()),
            });
        }

        if let Some(and_node) = node.as_and_node() {
            let left = self.lower_node(&and_node.left());
            // The right joins — unless it terminates: the skipped edge alone
            // returns and the right's scope is discarded (`blocked`,
            // rigor-rs#362 — `c && (h.default ||= 0; return)` lands nothing).
            let right_node = and_node.right();
            let right = if unconditional_exit(&right_node) && !self.arm_lands(&right_node) {
                self.lower_discarded(&right_node)
            } else {
                self.lower_node(&right_node)
            };
            return self.push(Node::Logical {
                left,
                right,
                is_and: true,
                span: span_of(&and_node.location()),
            });
        }

        if let Some(or_node) = node.as_or_node() {
            let left = self.lower_node(&or_node.left());
            let right_node = or_node.right();
            let right = if unconditional_exit(&right_node) && !self.arm_lands(&right_node) {
                self.lower_discarded(&right_node)
            } else {
                self.lower_node(&right_node)
            };
            return self.push(Node::Logical {
                left,
                right,
                is_and: false,
                span: span_of(&or_node.location()),
            });
        }

        UNMATCHED
    }

    /// [`Self::lower_node`]'s jump and carrier arms (`return`, `alias`, the four
    /// jumps, `defined?`, `BEGIN`/`END`/`super`/`yield`, the unmodeled writes),
    /// then the fallback that recovers any other node's meaningful descendants.
    #[inline(never)]
    fn lower_jumps(&mut self, node: &PrismNode<'_>, span: Span) -> NodeId {
        if let Some(ret) = node.as_return_node() {
            // An explicit `return` — a real owned variant (sig-gen's
            // `DefReturnTyper` port needs the VALUE expressions to union a def's
            // explicit returns). The value exprs are FULLY lowered as children,
            // a strict superset of the old recovered-children carrier (reads /
            // op-writes / calls inside a return stay visible to `flow.dead-
            // assignment` + the call rules, plus literals now exist too). The
            // node itself stays a STATEMENT: the typer's catch-all types it
            // `Dynamic[top]` exactly like the `Statements` carrier it replaces.
            // The values are `jump_value_type` operands — TYPED, never
            // `evaluate`d (`return h.default ||= 0` keeps `h`'s indexed
            // narrowings on the oracle, rigor-rs#343). Dead, not gated:
            // `jump_value_type` types the operand for the jump value only
            // and its scope is discarded outright, so no
            // `OperandEffects.any?` rescues a write inside — even
            // `return (h.default ||= (y = 1))` keeps the narrowing
            // (rigor-rs#361).
            self.dead_operand += 1;
            let values = ret
                .arguments()
                .map(|a| self.lower_body(&a.arguments()))
                .unwrap_or_default();
            self.dead_operand -= 1;
            return self.push(Node::Return { values, span });
        }

        if let Some(alias) = node.as_alias_method_node() {
            // `alias new old` — the operand expressions are lowered so calls
            // inside interpolated names stay reachable; `record_alias_method`'s
            // name extraction reads `SymbolLit`/`StringLit` back off them.
            let new_name = self.lower_node(&alias.new_name());
            let old_name = self.lower_node(&alias.old_name());
            return self.push(Node::Alias {
                new_name,
                old_name,
                span,
            });
        }

        // `next` / `break` / `redo` / `retry`. An ARGUMENT-LESS `next` / `break`
        // stays the tagged `Node::Other` leaf the class-narrowing pass already
        // reads (it binds nothing, and the typer's catch-all types it
        // `Dynamic[top]` exactly as before). A VALUED `next e` / `break e` and
        // an argument-less `redo` / `retry` lower to the `StatementsKind::Jump`
        // carrier instead — the reference's `never_completes_normally?`
        // (block_call_timing.rb) discriminates all four kinds, and the carrier
        // keeps the value expressions lowered (the `Recovered` fallback would
        // have kept them reachable anyway; the kind is the added information).
        // Every binder reads `Jump` exactly like `Recovered` — the value ids
        // are argument positions, so nothing in them binds.
        if let Some(n) = node.as_next_node() {
            return match n.arguments() {
                None => self.push(Node::Other { span, jump: Some(JumpKind::Next) }),
                Some(args) => {
                    // A valued `next` under a lexical loop joins the
                    // iteration fixpoint rather than `join_break_scopes`
                    // — its marks land on the post-loop scope — but a
                    // use the owning body still contains evaluates
                    // pre-join and keeps the record
                    // (`while c; next (h[:a] = 9; h[:a].m); end` fires
                    // in-argument on the oracle — rigor-rs#363 review).
                    // `lexical_loop` is masked inside a block/lambda:
                    // a `next` there targets the block itself.
                    if self.lexical_loop > 0 {
                        self.next_arg_spans.push((
                            span,
                            self.lexical_loop_bodies
                                .last()
                                .copied()
                                .unwrap_or((0, 0)),
                        ));
                    }
                    let body = self.lower_body(&args.arguments());
                    self.push(Node::Statements {
                        body,
                        span,
                        kind: StatementsKind::Jump(JumpKind::Next),
                    })
                }
            };
        }
        if let Some(n) = node.as_break_node() {
            return match n.arguments() {
                None => self.push(Node::Other { span, jump: Some(JumpKind::Break) }),
                Some(args) => {
                    // Under a lexical loop the argument's scope lands on
                    // `join_break_scopes` — a LOCALS-only union, so a scope
                    // effect the argument performs (an attribute write's
                    // indexed-record drop) never propagates, not even to
                    // reads inside the argument itself (rigor-rs#363).
                    // A block/lambda body masks `lexical_loop`: a `break`
                    // there targets the yielding call instead. With no
                    // iterated body enclosing it (`recovery_iterative`),
                    // the loop is a bare `for` — `eval_for` runs no
                    // `loop_content_writeback`, so nothing re-joins an
                    // adder mark's store evidence.
                    if self.lexical_loop > 0 {
                        if self.recovery_iterative > 0 {
                            self.locals_only_spans.push(span);
                        } else {
                            self.bare_locals_only_spans.push(span);
                        }
                    }
                    let body = self.lower_body(&args.arguments());
                    self.push(Node::Statements {
                        body,
                        span,
                        kind: StatementsKind::Jump(JumpKind::Break),
                    })
                }
            };
        }
        if node.as_redo_node().is_some() {
            return self.push(Node::Statements {
                body: Vec::new(),
                span,
                kind: StatementsKind::Jump(JumpKind::Redo),
            });
        }
        if node.as_retry_node().is_some() {
            return self.push(Node::Statements {
                body: Vec::new(),
                span,
                kind: StatementsKind::Jump(JumpKind::Retry),
            });
        }

        // `defined?(expr)` — the operand is NEVER EVALUATED (`defined?` inspects
        // the expression statically), so no CALL under it is reachable code.
        // Upstream #318 / `9e55deae` (shipped in `v0.3.4`) stopped all three of
        // its engine tree-walks descending here; before it, both implementations
        // checked the operand and agreed. Measured at the `v0.3.4` pin, keeping
        // the old behaviour is 2 false positives on the standing sweep
        // (dependabot-core's `if defined?(git_dir)` idiom) plus two more shapes on
        // a hand probe: `defined?(s.frobnicate)` on a typed receiver, and an
        // implicit-self call as the operand.
        //
        // Local READS under it survive — see
        // [`collect_defined_operand_children`]: the reference's
        // `DeadAssignmentCollector` does its own recursion and still counts them,
        // so `y = 1; defined?(y)` is not a dead assignment there.
        //
        // Ruby's BARE `defined? a && a.m` binds lower than `&&`, so Prism hands the
        // whole `a && a.m` back as the operand and all of it goes quiet — which is
        // the point of the upstream issue. The parenthesised `defined?(a) && a.m`
        // leaves the second call outside the operand, where it stays live code:
        // that follows from the parse, not from anything special here.
        if let Some(defined) = node.as_defined_node() {
            // The OPERAND is what gets the suppressed recovery, not the
            // `DefinedNode` itself: the collector records a `DefinedNode` whole
            // (so a nested one is re-entered here), and handing it its own root
            // would record it forever.
            let recovered =
                collect_defined_operand_children(&defined.value(), self.recovery_marks().joined);
            if recovered.is_empty() {
                return self.push(Node::Other { span, jump: None });
            }
            let body: Vec<NodeId> = self.lower_recovered(recovered);
            return self.push(Node::Statements { body, span, kind: StatementsKind::Inert });
        }

        // `BEGIN { … }` / `END { … }`. The reference's statement evaluator has no
        // handler for `PreExecutionNode` / `PostExecutionNode`: it types them as
        // pure expressions (`END` as `nil`, `expression_typer.rb:174`) and leaves
        // the scope unchanged, and an `END` body is deferred to exit
        // (`scope_indexer.rb` `DEFERRED_RANGE_NODES`). So a write in either body
        // never binds or widens the local it names (probes r1/c1/f7/b1/b2,
        // rigor-rs#153). The body is still recovered, exactly as the generic
        // carrier below recovered it, so the structural walks see the same
        // children; only the carrier's kind differs.
        //
        // `super(…)`, a bare `super`, and `yield(…)` are the same case: the
        // statement evaluator has no handler for `SuperNode`,
        // `ForwardingSuperNode` or `YieldNode` and does not thread their
        // arguments or block, so a write inside one never binds, at the top
        // level, inside a `def`, or as an assignment's value (probes g1/g2,
        // s1/s2/s3/s5, m1/m3/m4/m5).
        if node.as_pre_execution_node().is_some()
            || node.as_post_execution_node().is_some()
            || node.as_super_node().is_some()
            || node.as_forwarding_super_node().is_some()
            || node.as_yield_node().is_some()
        {
            let recovered = collect_recoverable_children(
                node,
                ScopeMarks {
                    blocked: true,
                    ..self.recovery_marks()
                },
            );
            if recovered.is_empty() {
                return self.push(Node::Other { span, jump: None });
            }
            let body: Vec<NodeId> = self.lower_recovered(recovered);
            // Inside an iterated body the operand is still never
            // evaluated — a write in it binds nothing — but the reference's
            // content-writeback text scan (`loop_content_writeback`,
            // `content_writeback_block_captures`) reads it, so a content
            // mutation there lands its `[]=`/mutator widening:
            // `while w; super(h[:a] ||= 1); end` widens `h` on the oracle
            // (rigor-rs#312). The carrier stays `Inert` — binds and uses
            // keep the no-eval rule — and the span is recorded as SCANNED
            // so `flow_writes` keeps the mutation marks (only) inside it.
            if self.recovery_iterative > 0 {
                self.scanned_inert_spans.push(span);
            }
            return self.push(Node::Statements { body, span, kind: StatementsKind::Inert });
        }

        // Assignment shapes with no owned variant — operator/and/or writes to
        // ivars, cvars, globals, constants, `x.f` / `a[i]` targets, `K::V`
        // path writes, and `expr => pat` / `expr in pat` bindings. The node is
        // additionally marked with a sibling [`Node::UnmodeledWrite`] (visible
        // to span-scanning consumers — the per-element block-fold gate must
        // decline rather than answer a tail with the pre-write binding,
        // rigor-rs#194) while the node itself keeps the plain `Recovered`
        // carrier every other consumer already handles.
        if is_unmodeled_write(node) {
            // A pattern-binding node's locals are real rebinds on the oracle —
            // `expr => pat`, `expr in pat`, the `MatchWriteNode` target list —
            // every other unmodeled write (`@i += 1`, `K += 1`, `K::V = …`)
            // binds no LOCAL (rigor-rs#352 review).
            let names = if let Some(n) = node.as_match_write_node() {
                n.targets()
                    .iter()
                    .flat_map(|t| pattern_bound_locals(&t))
                    .collect()
            } else if let Some(n) = node.as_match_required_node() {
                pattern_bound_locals(&n.pattern())
            } else if let Some(n) = node.as_match_predicate_node() {
                pattern_bound_locals(&n.pattern())
            } else {
                Vec::new()
            };
            self.push(Node::UnmodeledWrite { span, names });
            let recovered = collect_recoverable_children(node, self.recovery_marks());
            if recovered.is_empty() {
                return self.push(Node::Other { span, jump: None });
            }
            let body: Vec<NodeId> = self.lower_recovered(recovered);
            return self.push(Node::Statements {
                body,
                span,
                kind: StatementsKind::Recovered,
            });
        }

        // Anything outside the handled subset: RECOVER any meaningful descendant
        // nodes (local reads / op-writes / calls) so structural walks see them.
        //
        // The long tail of Prism nodes (`super`, `yield`, a `*splat`
        // arg, a block-arg, an assoc-splat, …) has no owned variant. Lowering them
        // to a bare span-only `Other` would DROP their subtree — and with it any
        // `LocalVariableRead` underneath. For `flow.dead-assignment` that is a
        // false-positive source: `return [entries, policy]` / `super(x: a)` /
        // `[*rest.map { … }]` read locals that would then look unread. We collect
        // the relevant descendant nodes via the Prism `Visit` recursion and lower
        // each into the arena, linked under a `Statements` carrier (Dynamic-typed;
        // purely a reachability handle). This also keeps a CALL inside such a
        // wrapper reachable for the existing call rules — a strict improvement.
        let recovered = collect_recoverable_children(node, self.recovery_marks());
        if recovered.is_empty() {
            return self.push(Node::Other { span, jump: None });
        }
        let body: Vec<NodeId> = self.lower_recovered(recovered);
        self.push(Node::Statements { body, span, kind: StatementsKind::Recovered })
    }

    /// Lower a Prism `NodeList` body (statement sequence) into owned ids in
    /// source order — the order inference relies on to populate the env.
    ///
    /// Inside a typed operand (`typed_depth > 0`) the list is a
    /// `thread_operand` element list — a call's arguments, a container's
    /// elements, a sequence's statements — so each element is gated on
    /// `OperandEffects.any?` individually (`x = [h.default ||= 0, y = 1]`
    /// keeps the write's narrowing while `y = 1` still evaluates —
    /// rigor-rs#361).
    fn lower_body(&mut self, body: &ruby_prism::NodeList<'_>) -> Vec<NodeId> {
        if self.typed_depth > 0 {
            return body
                .iter()
                .map(|n| self.lower_typed_operand(&n))
                .collect();
        }
        body.iter().map(|n| self.lower_node(&n)).collect()
    }

    /// `thread_operand` (`statement_evaluator.rb:2509-2535`): lower one
    /// operand ELEMENT of a typed position — a call receiver/argument, a
    /// container element, a splat's contents, an interpolation part, a
    /// `rescue`-modifier operand — through the `OperandEffects.any?` gate.
    /// [`OperandMode::Dead`] subtrees (`type_of` — never evaluated) are
    /// marked `dead_operand`; [`OperandMode::Gate`] subtrees (an
    /// effect-bearing call, container or sequence) re-gate their own
    /// operand children via the `typed_depth` bump; an
    /// [`OperandMode::Eval`] element (any other effect-bearing node with
    /// an `eval_*` handler) is `operand.evaluate`d whole — a fresh
    /// evaluated context, `typed_depth` cleared.
    ///
    /// [`OperandMode`]: super::operand_effects::OperandMode
    fn lower_typed_operand(&mut self, node: &PrismNode<'_>) -> NodeId {
        // A never-evaluated position stays never-evaluated all the way
        // down: `type_of` runs no handlers on any descendant.
        if self.dead_operand > 0 {
            return self.lower_node(node);
        }
        match operand_effects::operand_mode(node) {
            OperandMode::Dead => {
                self.dead_operand += 1;
                let id = self.lower_node(node);
                self.dead_operand -= 1;
                id
            }
            OperandMode::Gate => {
                self.typed_depth += 1;
                let id = self.lower_node(node);
                self.typed_depth -= 1;
                id
            }
            OperandMode::Eval => {
                let typed = std::mem::replace(&mut self.typed_depth, 0);
                let id = self.lower_node(node);
                self.typed_depth = typed;
                id
            }
        }
    }

    /// The enclosing scope-landing marks for a fresh recovery collect —
    /// the ambient `recovery_*` flags as a [`ScopeMarks`].
    fn recovery_marks(&self) -> ScopeMarks {
        ScopeMarks {
            joined: self.recovery_joined > 0,
            blocked: self.recovery_blocked > 0,
            iterative: self.recovery_iterative > 0,
            next_sink: self.recovery_next_sink > 0,
            suppressed: self.recovery_suppressed > 0,
            typed: self.typed_depth > 0,
            dead: self.dead_operand > 0,
            closure: self.closure_depth > 0,
        }
    }

    /// Lower one recovered-children batch (the [`Recovered`] list a wrapper's
    /// recovery produced), recording each child's crossed-block `bound` names
    /// into [`Self::closure_bindings`] — the closure-shadow side table
    /// `LoweredAst::closure_bindings` publishes (rigor-rs#137).
    ///
    /// [`Recovered`]: crate::ast::Recovered
    fn lower_recovered(&mut self, recovered: Vec<Recovered<'_>>) -> Vec<NodeId> {
        recovered
            .into_iter()
            .map(|Recovered {
                     node,
                     bound,
                     joined,
                     blocked,
                     iterative,
                     next_sink,
                     suppressed,
                     typed,
                     dead,
                     closure,
                 }| {
                let child_span = span_of(&node.location());
                if blocked {
                    self.blocked_spans.push(child_span);
                    if iterative {
                        self.blocked_iterative_spans.push(child_span);
                    }
                }
                self.recovery_joined += u32::from(joined);
                self.recovery_blocked += u32::from(blocked);
                self.recovery_iterative += u32::from(iterative);
                self.recovery_next_sink += u32::from(next_sink);
                self.recovery_suppressed += u32::from(suppressed);
                self.dead_operand += u32::from(dead);
                self.closure_depth += u32::from(closure);
                // A `typed`-marked recovered node is an operand ELEMENT —
                // `thread_operand` re-gates it on `OperandEffects.any?`
                // rather than forcing `evaluated: false` outright
                // (rigor-rs#361). `lower_typed_operand` no-ops the gate
                // when `dead_operand` already dominates.
                let id = if typed {
                    self.lower_typed_operand(&node)
                } else {
                    self.lower_node(&node)
                };
                self.recovery_joined -= u32::from(joined);
                self.recovery_blocked -= u32::from(blocked);
                self.recovery_iterative -= u32::from(iterative);
                self.recovery_next_sink -= u32::from(next_sink);
                self.recovery_suppressed -= u32::from(suppressed);
                self.dead_operand -= u32::from(dead);
                self.closure_depth -= u32::from(closure);
                if !bound.is_empty() {
                    self.closure_bindings.push((id, bound));
                }
                id
            })
            .collect()
    }

    /// Lower `node` under the `blocked` recovery mark — the handled path's
    /// `blocked_subtree`: the position NEVER scope-evaluates (a `when`/`in`
    /// condition or pattern, a folded-dead arm, a rescue MODIFIER's arm,
    /// an `and`/`or` operand the wrapper discards), so its writes land no
    /// bindings, widenings or narrowing drops anywhere — reads inside the
    /// position itself witness nothing either (`when (q = 1; q.w)` is
    /// silent on the oracle — rigor-rs#362). A position the reference
    /// DOES evaluate before discarding its post-scope — a terminated arm,
    /// a dead `begin`/`rescue` clause — is [`Self::lower_discarded`]
    /// instead: its writes still bind for in-position reads
    /// (rigor-rs#368). `lower_recovered` records the same span pair for
    /// `Recovered` children; this is the `lower_node` counterpart.
    fn lower_blocked(&mut self, node: &PrismNode<'_>) -> NodeId {
        let span = span_of(&node.location());
        self.blocked_spans.push(span);
        if self.recovery_iterative > 0 {
            self.blocked_iterative_spans.push(span);
        }
        self.recovery_blocked += 1;
        let id = self.lower_node(node);
        self.recovery_blocked -= 1;
        id
    }

    /// Lower `node` as a position the reference still EVALUATES but whose
    /// post-scope the join drops — a terminated `if`/`case`/`in`/
    /// rescue-clause arm or an `and`/`or` right side that exits
    /// (`live_rescue_results`, `join_case_branch_scopes`, `eval_if`'s
    /// edges). The span goes to `discarded_spans`, NOT `blocked_spans`:
    /// the position's writes DO land inside it (`rescue; x = 2; x.w;
    /// raise` fires `for 2`; `if c; h.default ||= 0; h[:a].f; return;
    /// end` drops the record in-arm — rigor-rs#368/#362), so writes keep
    /// `evaluated` and `DeadPositions::write_reaches` answers per read
    /// whether the scope lands. `blocked` is only for positions the
    /// oracle never scope-evaluates.
    fn lower_discarded(&mut self, node: &PrismNode<'_>) -> NodeId {
        self.discarded_spans.push(span_of(&node.location()));
        self.lower_node(node)
    }

    /// [`Self::lower_discarded`] for a `StatementsNode`'s body list — one
    /// `discarded` span covering the WHOLE arm: an arm's earlier write
    /// reaches its later reads (`rescue; x = 2; x.w` fires `for 2`), so
    /// per-statement spans would sever the arm's own flow.
    fn lower_body_discarded(&mut self, body: &ruby_prism::NodeList<'_>) -> Vec<NodeId> {
        if let (Some(first), Some(last)) = (body.iter().next(), body.iter().last()) {
            self.discarded_spans
                .push((first.location().start_offset(), last.location().end_offset()));
        }
        body.iter().map(|n| self.lower_node(&n)).collect()
    }

    /// [`Self::lower_blocked`] for a `StatementsNode`'s body list — one
    /// `blocked` span per statement, matching `lower_recovered`'s
    /// per-child recording.
    fn lower_body_blocked(&mut self, body: &ruby_prism::NodeList<'_>) -> Vec<NodeId> {
        self.recovery_blocked += 1;
        let ids = body
            .iter()
            .map(|n| {
                let span = span_of(&n.location());
                self.blocked_spans.push(span);
                if self.recovery_iterative > 0 {
                    self.blocked_iterative_spans.push(span);
                }
                self.lower_node(&n)
            })
            .collect();
        self.recovery_blocked -= 1;
        ids
    }

    /// Whether a syntactically-exiting arm still lands its scope downstream —
    /// the Collector's `arm_lands`: an arm holding a `next` that targets the
    /// iterated/`for` body it lives in merges back through `loop_iteration`'s
    /// join (`for`'s `next_sink`, `while`/`until`/block/lambda's `iterative`
    /// — `eval_loop` merges the scope at every targeting `next` for all of
    /// them); an arm holding a `break` targeting an iterated body lands its
    /// locals through `join_break_scopes` — `eval_for` joins break scopes
    /// too, but the `for` lowering runs under `next_sink` rather than
    /// `iterative`, so a `for` body's `break` arm is not treated as
    /// landing here; and an arm holding a
    /// `retry` lands its scope on the retry edge (`retry_edge_for` re-enters
    /// the `begin` carrying it — `rescue; unless r; r = ex; retry; end;
    /// raise; end` keeps `r` bound).
    fn arm_lands(&self, node: &PrismNode<'_>) -> bool {
        let iterated = self.recovery_next_sink > 0 || self.recovery_iterative > 0;
        (iterated && has_targeted_next(node))
            || (self.recovery_iterative > 0 && has_targeted_break(node))
            || has_targeted_retry(node)
    }

    /// Lower an *optional* body node (a `def`/`class`/`module`/block body, which
    /// Prism types as `Option<Node>`). A `StatementsNode` body is flattened to
    /// its statement ids so each lands in the arena individually; a `BeginNode`
    /// body (present when the method has an inline `rescue`/`ensure`) or any
    /// other single node is lowered as one id. `None` (empty body) yields `[]`.
    ///
    /// Inside a typed operand (`typed_depth > 0` — e.g. a `ParenthesesNode`
    /// operand element, an `OPERAND_SEQUENCES` member) a single-node body is
    /// itself an operand element and re-gates on `OperandEffects.any?`
    /// (rigor-rs#361).
    fn lower_optional_body(&mut self, body: Option<&PrismNode<'_>>) -> Vec<NodeId> {
        match body {
            None => Vec::new(),
            Some(node) => {
                if let Some(stmts) = node.as_statements_node() {
                    self.lower_body(&stmts.body())
                } else if self.typed_depth > 0 {
                    vec![self.lower_typed_operand(node)]
                } else {
                    vec![self.lower_node(node)]
                }
            }
        }
    }
}

/// The names a Prism scope's `locals` constant list holds (`BlockNode` /
/// `LambdaNode` — the names that scope BINDS: parameters of every form,
/// `;`-declared block-locals, and locals first written inside it; never a
/// captured outer local), decoded to owned `String`s.
fn constant_list_names(list: &ruby_prism::ConstantList<'_>) -> Vec<String> {
    list.iter().map(|c| constant_string(c.as_slice())).collect()
}

/// Whether a Prism node is an assignment the lowering cannot reproduce —
/// every operator/and/or write on a non-local target (`@x += 1`, `K += 1`),
/// a `K::V` constant-path write, and the
/// pattern-binding nodes (`expr => pat`, `expr in pat`). Local writes,
/// multiwrites, `for` indexes, the compound ATTRIBUTE writes (`x.f ||= v` —
/// `Node::AttrWrite`, rigor-rs#343), and the plain `K = v`/`@x = v`/`$g = v`/`@@x =
/// v` forms have owned variants already; multiwrite TARGETS never reach
/// `lower_node` standalone (a `MultiWriteNode` wraps them).
fn is_unmodeled_write(node: &PrismNode<'_>) -> bool {
    node.as_class_variable_and_write_node().is_some()
        || node.as_class_variable_operator_write_node().is_some()
        || node.as_class_variable_or_write_node().is_some()
        || node.as_constant_and_write_node().is_some()
        || node.as_constant_operator_write_node().is_some()
        || node.as_constant_or_write_node().is_some()
        || node.as_constant_path_and_write_node().is_some()
        || node.as_constant_path_operator_write_node().is_some()
        || node.as_constant_path_or_write_node().is_some()
        || node.as_constant_path_write_node().is_some()
        || node.as_global_variable_and_write_node().is_some()
        || node.as_global_variable_operator_write_node().is_some()
        || node.as_global_variable_or_write_node().is_some()
        || node.as_instance_variable_and_write_node().is_some()
        || node.as_instance_variable_operator_write_node().is_some()
        || node.as_instance_variable_or_write_node().is_some()
        || node.as_match_write_node().is_some()
        || node.as_match_predicate_node().is_some()
        || node.as_match_required_node().is_some()
}

/// Every LOCAL a pattern subtree binds — the `LocalVariableTargetNode`s under
/// it (`in [s]`, `in {a: s}`, `in Integer => s`, `v => s`, `v in [s]`,
/// `in [a, *b]`). Pinned reads (`^s`) lower as READS, not targets, so only the
/// bindings land here. `flow_writes` counts each as a rebind exactly like
/// `s = v`: the oracle binds the name on match (rigor-rs#352 review).
fn pattern_bound_locals(pattern: &PrismNode<'_>) -> Vec<String> {
    struct Locals {
        names: Vec<String>,
    }
    impl<'pr> ruby_prism::Visit<'pr> for Locals {
        fn visit_local_variable_target_node(
            &mut self,
            node: &ruby_prism::LocalVariableTargetNode<'pr>,
        ) {
            self.names.push(constant_string(node.name().as_slice()));
        }
    }
    use ruby_prism::Visit as _;
    let mut v = Locals { names: Vec::new() };
    v.visit(pattern);
    v.names
}

/// A prism integer's value when it fits `i64`, from its little-endian `u32`
/// digits; `None` for a Bignum.
pub(crate) fn integer_value(int: &ruby_prism::Integer<'_>) -> Option<i64> {
    let (negative, digits) = int.to_u32_digits();
    let mut mag: i128 = 0;
    for &d in digits.iter().rev() {
        mag = mag.checked_mul(1 << 32)?.checked_add(i128::from(d))?;
    }
    i64::try_from(if negative { -mag } else { mag }).ok()
}

/// A Bignum's signed decimal spelling — the string `Integer#inspect` prints —
/// from the little-endian `u32` digit view. Base-2^32 to base-10 by repeated
/// chunk division (10^9 fits `u32` products in `u64`); no precision is ever
/// lost, so an arbitrarily long literal renders exactly.
fn integer_decimal(int: &ruby_prism::Integer<'_>) -> String {
    const CHUNK: u64 = 1_000_000_000; // largest power of ten below 2^32
    let (negative, digits) = int.to_u32_digits();
    let mut digits = digits.to_vec();
    let mut chunks: Vec<u64> = Vec::new();
    while !digits.is_empty() {
        let mut rem: u64 = 0;
        for d in digits.iter_mut().rev() {
            let cur = (rem << 32) | u64::from(*d);
            *d = (cur / CHUNK) as u32;
            rem = cur % CHUNK;
        }
        while digits.last() == Some(&0) {
            digits.pop();
        }
        chunks.push(rem);
    }
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    match chunks.last() {
        // A Bignum is never zero (0 lowers to `value: Some(0)`), so a `None`
        // arm only fires on an empty digit list — spell it `0` anyway.
        None => out.push('0'),
        Some(&top) => {
            out.push_str(&top.to_string());
            for &chunk in chunks.iter().rev().skip(1) {
                out.push_str(&format!("{chunk:09}"));
            }
        }
    }
    out
}
