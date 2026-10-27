//! `Builder::lower_calls` holds the call-shaped arms of `Builder::lower_node`:
//! the compound index and attribute writes, and `CallNode`.

use crate::ruby_prism::Node as PrismNode;

use super::operand_effects;
use super::{
    block_param_names, constant_list_names, constant_string, span_of, Builder, Compound, Node,
    NodeId, Span, UNMATCHED,
};

impl<'src> Builder<'src> {
    /// [`Self::lower_node`]'s call-shaped arms: the compound index writes
    /// (`h[k] ||= v`), the compound attribute writes (`recv.attr ||= v`) and
    /// `CallNode` (receiver, arguments, block).
    #[inline(never)]
    pub(crate) fn lower_calls(&mut self, node: &PrismNode<'_>, span: Span) -> NodeId {
        // `h[k] ||= v` / `h[k] &&= v` / `h[k] op= v` — Prism's three compound
        // index-write nodes. A plain `h[k] = v` stays a `[]=` `Node::Call`;
        // the compound forms read the slot AND store through `[]=`, so they
        // need a dedicated shape the reference's `IndexWriteWidening`
        // (`index_write_widening.rb`) consumes — receiver, index args, value.
        // They are NOT `Node::Call`s: the reference's `eval_index_write` /
        // `eval_index_or_write` widen the receiver binding and type the node
        // without dispatching `call.*` rules on the synthesized `[]` / `[]=`
        // (`c[0] ||= 1` is silent for a `class C; end` receiver — probed at
        // `e59b7b89`); a `Call` here would fire `call.undefined-method` /
        // `call.wrong-arity` the reference never emits.
        let index_write = node
            .as_index_or_write_node()
            .map(|w| (Compound::Or, w.receiver(), w.arguments(), w.value()))
            .or_else(|| {
                node.as_index_and_write_node()
                    .map(|w| (Compound::And, w.receiver(), w.arguments(), w.value()))
            })
            .or_else(|| {
                node.as_index_operator_write_node().map(|w| {
                    (
                        Compound::Op(
                            constant_string(w.binary_operator().as_slice()),
                        ),
                        w.receiver(),
                        w.arguments(),
                        w.value(),
                    )
                })
            });
        if let Some((compound, receiver, arguments, value)) = index_write {
            // The receiver and the index arguments are TYPED operands —
            // `eval_index_or_write` reads `scope.type_of(node.receiver)` and
            // `index_write_arg_types` types the indices; only the RHS value is
            // `sub_eval`'d (`statement_evaluator.rb:754-789`, rigor-rs#343).
            // `type_of` is the dead half of the operand gate: no
            // `OperandEffects.any?` can re-enter it, so a compound attribute
            // write buried in `h[h.default ||= (y = 1)] ||= 0` stays
            // `evaluated: false` (rigor-rs#361).
            self.dead_operand += 1;
            let receiver = receiver.as_ref().map(|r| self.lower_node(r));
            let indices = arguments
                .map(|a| self.lower_body(&a.arguments()))
                .unwrap_or_default();
            self.dead_operand -= 1;
            let value = self.lower_node(&value);
            // `operand`: the write lowered where it EVALUATES inline —
            // straight-line code or a scope-transparent recovery position
            // (the collector only `push`es it there now — see
            // `index_write_transparent` in `recovery.rs`). A joined position
            // leaves the flag off: the reference's scope join erases the
            // `h[k] -> stored` narrowing an `||=` would otherwise record
            // (rigor-rs#325), so the flow machinery treats the write as a
            // plain conditional `[]=` mutation.
            let operand = self.recovery_joined == 0
                && self.recovery_blocked == 0
                && self.recovery_iterative == 0
                && self.recovery_next_sink == 0
                && self.recovery_suppressed == 0;
            return self.push(Node::IndexWrite {
                receiver,
                indices,
                value,
                compound,
                operand,
                span,
            });
        }

        // `recv.attr ||= v` / `recv.attr &&= v` / `recv.attr op= v` — Prism's
        // compound ATTRIBUTE-write nodes. Like `Node::IndexWrite` above, this
        // is deliberately NOT a `Node::Call`: the reference dispatches them to
        // `eval_attribute_compound_write` (`statement_evaluator.rb`), which
        // types the node via `call_or_write_type_for` and applies
        // `widen_attribute_write(receiver, node.write_name, s)` — a synthesized
        // `attr`/`attr=` `call.*` dispatch would mint diagnostics the oracle
        // never emits (rigor-rs#343).
        let attr_write = node
            .as_call_or_write_node()
            .map(|w| {
                (
                    Compound::Or,
                    w.receiver(),
                    w.value(),
                    w.read_name(),
                    w.write_name(),
                    w.is_safe_navigation(),
                )
            })
            .or_else(|| {
                node.as_call_and_write_node().map(|w| {
                    (
                        Compound::And,
                        w.receiver(),
                        w.value(),
                        w.read_name(),
                        w.write_name(),
                        w.is_safe_navigation(),
                    )
                })
            })
            .or_else(|| {
                node.as_call_operator_write_node().map(|w| {
                    (
                        Compound::Op(constant_string(w.binary_operator().as_slice())),
                        w.receiver(),
                        w.value(),
                        w.read_name(),
                        w.write_name(),
                        w.is_safe_navigation(),
                    )
                })
            });
        if let Some((compound, receiver, value, read_name, write_name, safe_nav)) = attr_write {
            // `evaluated`: whether the occurrence sits where
            // `eval_attribute_compound_write` can run — NOT under a
            // never-evaluated position (`dead_operand`: an
            // `OperandEffects`-free operand element, a `return` operand's
            // `jump_value_type`, an `in` pattern, another write's
            // receiver/indices/value), a `defined?`/`super`/`yield`
            // suppression carrier (`suppressed`), or a literal
            // block/lambda body (`closure_depth`: `evaluate_invocation`'s
            // `OperandEffects.any?` writeback check excludes them too —
            // `x.each { h.default ||= 0 }` keeps `h`'s indexed
            // narrowings). Under a TYPED operand (`typed_depth`,
            // rigor-rs#343) the reference's `thread_operand` gates the
            // write on `OperandEffects.any?(subtree)` — an outliving
            // effect inside (a local/ivar/gvar write, a compound index
            // write, a targeted `next`/`break`, a shape-mutator call on
            // an outliving receiver) keeps it EVALUATED so the widening
            // still lands: `puts(h.default ||= (y = 1))` is silent on
            // the oracle while `puts(h.default ||= 0)` keeps the `(h, k)`
            // narrowing (rigor-rs#361). Joined/iterated/`for` positions
            // still EVALUATE (measured: a joining `if` arm, a `while`
            // predicate or body, an `ensure` body all land the writer's
            // widening) — a `Recovered::blocked` position does not:
            // `Narrowing.case_when_scopes` shape-narrows `when`/`in`
            // conditions without `sub_eval` (so
            // `eval_attribute_compound_write` never runs there), and a
            // terminated arm's scope is discarded before the join —
            // `when (h.default ||= 0)`, `if c; h.default ||= 0; return;
            // end`, `rescue; h.default ||= 0; raise; end` all keep the
            // receiver's indexed narrowings on the oracle (rigor-rs#362).
            // The iterated-body content-writeback scan covers `[]=`/
            // mutator `CallNode`s only — never this node — so `blocked`
            // excludes unconditionally.
            let evaluated = self.dead_operand == 0
                && self.closure_depth == 0
                && self.recovery_suppressed == 0
                && self.recovery_blocked == 0
                && (self.typed_depth == 0
                    || operand_effects::any(node));
            // `closure_evaluated` — `evaluated` minus the `closure_depth`
            // clause: whether the INNERMOST deferred body (a literal
            // block/lambda — `typed_depth` restarts at its boundary) runs
            // this write in its own scope. The block-scope
            // `widen_attribute_write` still drops the receiver's indexed
            // narrowings for the reads that follow it in the body
            // (rigor-rs#366) even though the effect never lands on the
            // enclosing scope. A blocked position stays blocked inside
            // the block's own scope too — `each { case c; when (h.default
            // ||= 0); end; h[:a].m }` keeps the record in-body on the
            // oracle (rigor-rs#401).
            let closure_evaluated = self.dead_operand == 0
                && self.recovery_suppressed == 0
                && self.recovery_blocked == 0
                && (self.typed_depth == 0
                    || operand_effects::any(node));
            // The receiver and RHS are `type_of` operands of the write —
            // `call_or_write_type_for` types them; a compound attribute write
            // nested inside never evaluates (`h.default ||= (h.x ||= 1)`
            // widens `h` once, through the outer `default=`).
            self.dead_operand += 1;
            let receiver = receiver.as_ref().map(|r| self.lower_node(r));
            let value = self.lower_node(&value);
            self.dead_operand -= 1;
            return self.push(Node::AttrWrite {
                receiver,
                read_name: constant_string(read_name.as_slice()),
                write_name: constant_string(write_name.as_slice()),
                compound,
                safe_nav,
                evaluated,
                closure_evaluated,
                value,
                span,
            });
        }

        if let Some(call) = node.as_call_node() {
            // The receiver, the positional arguments and the `&expr`
            // block-pass are OPERAND positions — `OperandWalk.thread_operand`
            // gates each on `OperandEffects.any?` (`lower_typed_operand`):
            // an element carrying an outliving effect keeps evaluating, so a
            // compound attribute write inside it still lands its widening —
            // `puts(h.default ||= (y = 1))` is silent on the oracle; an
            // element without one is `type_of`'d whole — `puts(h.default ||=
            // 0)` keeps the `(h, k)` narrowing (rigor-rs#343, #361).
            self.typed_depth += 1;
            let receiver = call.receiver().map(|r| self.lower_typed_operand(&r));
            let method = constant_string(call.name().as_slice());
            // Lower positional arguments in source order (ADR-0023: argument
            // contracts + arg-dependent folding). Splat/keyword/forwarding args
            // lower like any other node — a downstream rule that needs to
            // distinguish them reads `args_plain_positional` / `args_all_plain`
            // / `first_arg_nonplain`, never the lowered children.
            let args = call
                .arguments()
                .map(|a| self.lower_body(&a.arguments()))
                .unwrap_or_default();
            self.typed_depth -= 1;
            // Whether the FIRST positional argument is a non-plain shape
            // (splat / bare keyword-hash / forwarded args) — recorded here
            // because the lowered subtree does not preserve the distinction
            // (a `KeywordHashNode` and a braced `HashNode` both become
            // `Node::HashLit`). `call.raise-non-exception` bails on it, mirroring
            // the reference's `first_positional_raise_operand`.
            let first_arg_nonplain = call
                .arguments()
                .and_then(|a| a.arguments().iter().next().map(|first| {
                    first.as_splat_node().is_some()
                        || first.as_keyword_hash_node().is_some()
                        || first.as_forwarding_arguments_node().is_some()
                }))
                .unwrap_or(false);
            // The reference's `plain_positional_call?` (`check_rules.rb:1680`):
            // EVERY argument in `call.arguments()` is `simple_positional?` —
            // no splat, no bare keyword-hash, no block-argument, no forwarded
            // `...`. The check is over `arguments()` ONLY: a `&blk` block-pass
            // rides Prism's `block()` and never enters `arguments()`, so it
            // does not disqualify here (the oracle arity-checks `first(1, 2, &)`);
            // an ordinary trailing block (a `BlockNode`) does not either.
            let args_plain_positional = call
                .arguments()
                .map(|a| {
                    a.arguments().iter().all(|x| {
                        x.as_splat_node().is_none()
                            && x.as_keyword_hash_node().is_none()
                            && x.as_block_argument_node().is_none()
                            && x.as_forwarding_arguments_node().is_none()
                    })
                })
                .unwrap_or(true);
            // `args_all_plain` adds the `&blk` block-pass decline
            // `call.argument-type-mismatch` wants on top of the
            // plain-positional argument test.
            let block_is_pass = call
                .block()
                .map(|b| b.as_block_argument_node().is_some())
                .unwrap_or(false);
            let args_all_plain = args_plain_positional && !block_is_pass;
            // Lower an attached block so calls/reads inside it reach the walk.
            //   * a BlockNode (`{ … }` / `do…end`) — lower its body statements.
            //   * a `&expr` block-pass (BlockArgumentNode) — lower the passed
            //     EXPRESSION. A `foo(&blk)` genuinely passes a block, and its `blk`
            //     read MUST surface in the arena: `flow.dead-assignment` gathers
            //     reads by arena span-scan, so an unlowered `&action` would leave
            //     `while action = q.pop; f(&action); end` with no read of `action`
            //     and FALSELY flag the write. `&` alone (argument forwarding) has
            //     no expression and lowers to nothing.
            let block_body = match call.block() {
                None => Vec::new(),
                Some(b) => {
                    if let Some(bn) = b.as_block_node() {
                        // An iterated body — `evaluate_invocation` /
                        // `content_writeback_block_captures` land every
                        // wrapped index write's `[]=` widening
                        // (rigor-rs#312). It is also a DEFERRED scope for a
                        // compound ATTRIBUTE write: `OperandEffects.any?`
                        // does not list `Call*WriteNode`s, so
                        // `x.each { h.default ||= 0 }` keeps `h`'s indexed
                        // narrowings on the OUTER scope (rigor-rs#343) —
                        // the block's own scope still drops them via
                        // `AttrWrite::closure_evaluated` (rigor-rs#366).
                        // And it is NOT an
                        // operand position — `call_operand_scope` threads
                        // receiver/args/`&expr` only — so the operand gate
                        // is cleared while its body lowers.
                        self.recovery_iterative += 1;
                        self.closure_depth += 1;
                        let operand_typed = std::mem::replace(&mut self.typed_depth, 0);
                        // A `break` inside the block targets the yielding
                        // call, not an enclosing lexical loop — its
                        // argument scope never lands on `join_break_scopes`
                        // (rigor-rs#363).
                        let lexical_loop = std::mem::replace(&mut self.lexical_loop, 0);
                        if let Some(b) = bn.body() {
                            self.iterated_body_spans.push(span_of(&b.location()));
                        }
                        let body = self.lower_optional_body(bn.body().as_ref());
                        self.typed_depth = operand_typed;
                        self.lexical_loop = lexical_loop;
                        self.recovery_iterative -= 1;
                        self.closure_depth -= 1;
                        body
                    } else if let Some(ba) = b.as_block_argument_node() {
                        // `&expr` is an operand like any other argument.
                        self.typed_depth += 1;
                        let pass = ba
                            .expression()
                            .map(|e| vec![self.lower_typed_operand(&e)])
                            .unwrap_or_default();
                        self.typed_depth -= 1;
                        pass
                    } else {
                        Vec::new()
                    }
                }
            };
            let block_span = call
                .block()
                .and_then(|b| b.as_block_node().map(|bn| span_of(&bn.location())));
            // The block's OWN local names (Prism's `BlockNode#locals`): params
            // of every form + `;`-declared block-locals + block-scoped writes,
            // never a captured outer local — the exact shadow set
            // `toplevel_rebinds` needs (rigor-rs#166). Empty when there is no
            // literal block (a `&expr` pass binds nothing).
            let block_locals = call
                .block()
                .and_then(|b| b.as_block_node())
                .map(|bn| constant_list_names(&bn.locals()))
                .unwrap_or_default();
            let block_params = call
                .block()
                .and_then(|b| b.as_block_node())
                .map(|bn| block_param_names(&bn))
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
                block_span,
                block_locals,
                block_params,
                explicit_arg_list: call.arguments().is_some(),
                message_span,
                // `x&.foo` ⇒ safe-nav; `x.foo` ⇒ plain dot. Threaded so
                // `call.possible-nil-receiver` can faithfully suppress on `&.`.
                safe_nav: call.is_safe_navigation(),
                first_arg_nonplain,
                args_plain_positional,
                args_all_plain,
                span: span_of(&call.location()),
            });
        }

        UNMATCHED
    }
}
