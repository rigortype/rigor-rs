//! `Builder::lower_jumps` holds the jump and carrier arms of `Builder::lower_node`:
//! `return`, `alias`, the four jumps, `defined?`, the `BEGIN`/`END`/`super`/`yield`
//! carriers and the unmodeled writes. A node no family claims falls through to
//! `Builder::lower_fallback` in `builder.rs`.
//!
//! The unmodeled writes are the `UnmodeledWrite` carriers: the COMPOUND
//! instance, class, global and constant writes (`@x += 1`, `@@x ||= 1`,
//! `$x &&= 1`, `K += 1`), every `K::V` path write, and the pattern-match writes
//! (`expr => pat`, `expr in pat`, a regex literal's named captures). The PLAIN
//! writes (`@x = 1`, `@@x = 1`, `$x = 1`, `K = 1`) are owned variants in
//! `expressions.rs`, as are the local writes, plain and compound.

use crate::ruby_prism::Node as PrismNode;

use super::{
    collect_defined_operand_children, collect_recoverable_children, is_unmodeled_write,
    pattern_bound_locals, Builder, JumpKind, Node, NodeId, ScopeMarks, Span, StatementsKind,
    UNMATCHED,
};

impl<'src> Builder<'src> {
    /// [`Self::lower_node`]'s jump and carrier arms (`return`, `alias`, the four
    /// jumps, `defined?`, `BEGIN`/`END`/`super`/`yield`, the unmodeled writes).
    // Not a perf hint: keeps this frame out of `lower_node`'s recursion.
    // Inlined, the release nesting limit on a 2 MiB thread roughly halves (#413).
    #[inline(never)]
    pub(super) fn lower_jumps(&mut self, node: &PrismNode<'_>, span: Span) -> NodeId {
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
        // ivars, cvars, globals and constants, `K::V` path writes, and
        // `expr => pat` / `expr in pat` / named-capture bindings. (The
        // compound `a[i]` and `x.f` writes are owned, in `lower_calls`:
        // `Node::IndexWrite`, rigor-rs#135, and `Node::AttrWrite`,
        // rigor-rs#343.) The node is
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

        UNMATCHED
    }
}
