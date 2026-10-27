//! The lowering walk: `Builder` turns one borrowed Prism node into owned arena
//! nodes (`lower_node`).

mod calls;
mod expressions;
mod control;
mod definitions;
mod jumps;
#[cfg(test)]
mod family_inventory;

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
/// not one of its types. `push`'s `debug_assert!` keeps every real id below
/// `u32::MAX`, but only in debug builds. In release nothing checks it; a
/// collision would need an arena of `u32::MAX` nodes, so it is unreachable in
/// practice. Were it reached, `lower_node` would read the real id as "no
/// match" and the fallback would lower that node a second time: wrong output,
/// not UB.
const UNMATCHED: NodeId = NodeId(u32::MAX);

/// [`Builder::lower_node`]'s family methods in try order, by name, for the
/// debug disjointness check's messages. `FAMILIES.len()` stands for the
/// fallback.
#[cfg(debug_assertions)]
const FAMILIES: [&str; 5] = [
    "lower_calls",
    "lower_expressions",
    "lower_control",
    "lower_definitions",
    "lower_jumps",
];

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
    ///
    /// The arms live in five family methods, tried in this order:
    /// [`Self::lower_calls`], [`Self::lower_expressions`],
    /// [`Self::lower_control`], [`Self::lower_definitions`],
    /// [`Self::lower_jumps`]. A family returns [`UNMATCHED`] if and only if
    /// none of its arms matched; an arm never returns it. A node no family
    /// claims reaches [`Self::lower_fallback`].
    ///
    /// The try order cannot change the output, because the families' Prism
    /// type sets are pairwise disjoint: every arm's predicate is a pure
    /// `as_X_node()` chain, and no type appears in two families. Debug builds
    /// enforce this on every node (`check_families_disjoint`): once family
    /// *k* matches, the later families run on the same node and must each
    /// return `UNMATCHED`. A new Prism type goes into exactly one family, as
    /// an arm before that family's trailing `UNMATCHED`. A test checks the
    /// same statically, for every claimed type a debug run may never lower:
    /// it parses the arms' layout, which `builder/family_inventory.rs`'s
    /// module doc describes.
    ///
    /// The families and the fallback are `#[inline(never)]`, which keeps
    /// their frames out of this one: only one family frame is live per
    /// nesting level. Without it LLVM inlines the families back, and the
    /// release nesting limit on a 2 MiB thread roughly halves (#413). It is
    /// not a perf hint.
    pub(crate) fn lower_node(&mut self, node: &PrismNode<'_>) -> NodeId {
        let span = span_of(&node.location());
        // One `id`, reassigned rather than shadowed: an unoptimised build
        // gives every `let` its own slot in this frame, which each nesting
        // level pays (debug frame 128 B, against 160 B with six `let`s).
        let mut id = self.lower_calls(node, span);
        if id != UNMATCHED {
            #[cfg(debug_assertions)]
            self.check_families_disjoint(node, span, 0, id);
            return id;
        }
        id = self.lower_expressions(node);
        if id != UNMATCHED {
            #[cfg(debug_assertions)]
            self.check_families_disjoint(node, span, 1, id);
            return id;
        }
        id = self.lower_control(node, span);
        if id != UNMATCHED {
            #[cfg(debug_assertions)]
            self.check_families_disjoint(node, span, 2, id);
            return id;
        }
        id = self.lower_definitions(node);
        if id != UNMATCHED {
            #[cfg(debug_assertions)]
            self.check_families_disjoint(node, span, 3, id);
            return id;
        }
        id = self.lower_jumps(node, span);
        if id != UNMATCHED {
            #[cfg(debug_assertions)]
            self.check_families_disjoint(node, span, 4, id);
            return id;
        }
        id = self.lower_fallback(node, span);
        #[cfg(debug_assertions)]
        self.check_families_disjoint(node, span, FAMILIES.len(), id);
        id
    }

    /// [`Self::lower_node`]'s last resort, for a node no family claims. It
    /// always pushes a node, so it never returns [`UNMATCHED`].
    // Not a perf hint: keeps this frame out of `lower_node`'s recursion (the
    // fallback recurses through `lower_recovered`). Inlined, `lower_node`'s
    // release frame grows from 80 B to 432 B, paid at every level (#416).
    #[inline(never)]
    fn lower_fallback(&mut self, node: &PrismNode<'_>, span: Span) -> NodeId {
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

    /// The debug half of [`Self::lower_node`]'s disjointness contract: `node`
    /// was lowered to `id` by `FAMILIES[matched]`, or by the fallback when
    /// `matched == FAMILIES.len()`. Asserts `id` is a real id, then runs every
    /// LATER family on `node` and asserts each returns [`UNMATCHED`].
    ///
    /// While the invariant holds, each of those calls is a run of failed
    /// `as_X_node()` tests: it pushes nothing and recurses nowhere. The
    /// earlier families already returned `UNMATCHED` on the way to `matched`,
    /// and the fallback is never re-run, since it always pushes. It runs
    /// after the matched family has returned, so it adds nothing to the
    /// per-level frame: at the deepest point only the leaf's check is live.
    #[cfg(debug_assertions)]
    #[inline(never)]
    fn check_families_disjoint(
        &mut self,
        node: &PrismNode<'_>,
        span: Span,
        matched: usize,
        id: NodeId,
    ) {
        assert_ne!(
            id,
            UNMATCHED,
            "lower_node returned UNMATCHED for Prism {} at line {} (byte {})",
            prism_type_name(node),
            self.line_at(span.0),
            span.0,
        );
        for (later, later_name) in FAMILIES.iter().enumerate().skip(matched + 1) {
            let other = match later {
                1 => self.lower_expressions(node),
                2 => self.lower_control(node, span),
                3 => self.lower_definitions(node),
                4 => self.lower_jumps(node, span),
                _ => unreachable!("family {later} is never a later family"),
            };
            assert_eq!(
                other,
                UNMATCHED,
                "Prism {} at line {} (byte {}) matched both {} and {}: \
                 the lower_node families' type sets must be disjoint",
                prism_type_name(node),
                self.line_at(span.0),
                span.0,
                FAMILIES[matched],
                later_name,
            );
        }
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

/// Prism's type name for `node` (`CallNode`, `IfNode`, …): the head of its
/// `Debug` rendering, which ruby-prism writes as `TypeName(fields…)`. Only
/// the debug disjointness check's failure messages call it, because the
/// rendering walks the whole subtree.
#[cfg(debug_assertions)]
fn prism_type_name(node: &PrismNode<'_>) -> String {
    let rendered = format!("{node:?}");
    let end = rendered.find('(').unwrap_or(rendered.len());
    rendered[..end].to_string()
}

/// Whether a Prism node is an assignment the lowering cannot reproduce —
/// every operator/and/or write on an instance, class, global or constant
/// target (`@x += 1`, `K += 1`), a `K::V` constant-path write, and the
/// pattern-binding nodes (`expr => pat`, `expr in pat`, a regex literal's
/// named captures). Local writes, multiwrites, `for` indexes, the compound
/// INDEX writes (`h[k] ||= v` — `Node::IndexWrite`, rigor-rs#135), the
/// compound ATTRIBUTE writes (`x.f ||= v` — `Node::AttrWrite`, rigor-rs#343),
/// and the plain `K = v`/`@x = v`/`$g = v`/`@@x = v` forms have owned variants
/// already; multiwrite TARGETS never reach `lower_node` standalone (a
/// `MultiWriteNode` wraps them).
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
