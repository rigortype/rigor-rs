//! `Builder::lower_expressions` holds the expression arms of
//! `Builder::lower_node`, from the program wrapper and locals to literals,
//! collections and lambdas.
//!
//! Only the PLAIN instance, class, global and constant writes (`@x = 1`,
//! `@@x = 1`, `$x = 1`, `K = 1`) are here. Their compound forms (`@x += 1`),
//! every `K::V` path write and the pattern-match writes are `UnmodeledWrite`
//! carriers in `jumps.rs`. The local writes, plain and compound, are here.

use crate::ruby_prism::Node as PrismNode;

use super::{
    constant_list_names, constant_path_string, constant_string, integer_decimal, integer_value,
    lower_multi_targets, rooted_constant_path, self_anchored_constant_path, span_of,
    strict_constant_path_string, Builder, Node, NodeId, StatementsKind, UNMATCHED,
};

impl<'src> Builder<'src> {
    /// [`Self::lower_node`]'s expression arms: the program and statement
    /// wrappers, local reads and writes, multi-writes, literals, collections,
    /// parentheses, instance/class/global variables, constants, `self`,
    /// interpolation and lambdas.
    // Not a perf hint: keeps this frame out of `lower_node`'s recursion.
    // Inlined, the release nesting limit on a 2 MiB thread roughly halves (#413).
    #[inline(never)]
    pub(super) fn lower_expressions(&mut self, node: &PrismNode<'_>) -> NodeId {
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
                kind: StatementsKind::Sequence,
            });
        }

        if let Some(write) = node.as_local_variable_write_node() {
            let name = constant_string(write.name().as_slice());
            let value = self.lower_node(&write.value());
            return self.push(Node::LocalVariableWrite {
                name,
                value,
                name_span: span_of(&write.name_loc()),
                span: span_of(&write.location()),
            });
        }

        // Operator / and / or local writes (`x += 1`, `y ||= 5`, `z &&= w`). All
        // three lower to the same owned variant: their target name is a READ of
        // the prior binding (mirrors the reference `reading_assignment?`), and the
        // assigned value is lowered for call reachability. Without this variant
        // they fall through to `Node::Other` and the dead-assignment walk loses
        // sight of the target read — the one false-positive risk this rule has.
        if let Some(opw) = node.as_local_variable_operator_write_node() {
            let name = constant_string(opw.name().as_slice());
            let value = self.lower_node(&opw.value());
            return self.push(Node::LocalVariableOpWrite {
                name,
                value,
                span: span_of(&opw.location()),
            });
        }
        if let Some(andw) = node.as_local_variable_and_write_node() {
            let name = constant_string(andw.name().as_slice());
            let value = self.lower_node(&andw.value());
            return self.push(Node::LocalVariableOpWrite {
                name,
                value,
                span: span_of(&andw.location()),
            });
        }
        if let Some(orw) = node.as_local_variable_or_write_node() {
            let name = constant_string(orw.name().as_slice());
            let value = self.lower_node(&orw.value());
            return self.push(Node::LocalVariableOpWrite {
                name,
                value,
                span: span_of(&orw.location()),
            });
        }

        // `a, b = rhs` / `a, (b, c), *rest = rhs`. The target tree is lowered
        // structurally (NOT into the arena — targets bind names, they are not
        // value expressions) and the RHS is lowered as a normal child.
        if let Some(mw) = node.as_multi_write_node() {
            let mut recovered = Vec::new();
            let targets = lower_multi_targets(
                &mw.lefts(),
                mw.rest().as_ref(),
                &mw.rights(),
                span_of(&mw.location()),
                &mut recovered,
                self.recovery_iterative > 0,
            );
            // Lower the expressions embedded in non-local targets so the
            // structural walks keep seeing those reads/calls — the old
            // recovered-children carrier did, and `flow.dead-assignment`
            // depends on it (netrc `item[3], item[5] = info`).
            let target_exprs: Vec<NodeId> = self.lower_recovered(recovered);
            let value = self.lower_node(&mw.value());
            return self.push(Node::MultiWrite {
                targets,
                value,
                target_exprs,
                span: span_of(&mw.location()),
            });
        }

        if let Some(read) = node.as_local_variable_read_node() {
            let name = constant_string(read.name().as_slice());
            return self.push(Node::LocalVariableRead {
                name,
                span: span_of(&read.location()),
            });
        }

        if let Some(read) = node.as_it_local_variable_read_node() {
            // Ruby 3.4 `it` — the node carries no `name`; the implicit local is
            // always `it`, matching the binding `block_param_names` installs
            // for `ItParametersNode` (`reference/rigor`'s `it_read`).
            return self.push(Node::LocalVariableRead {
                name: "it".to_string(),
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
            // Prism's `TryInto<i32>` covers only `i32`; the digit view widens
            // that to all of `i64`. A Bignum lowers `value` to `None` — never
            // to a wrong value (`3_000_000_000` once lowered to `0` and
            // folded) — while `digits` keeps the exact decimal spelling so
            // the typer can pin it as `Scalar::BigInt` (rigor-rs#194).
            let value = integer_value(&int.value());
            let digits = if value.is_none() {
                Some(integer_decimal(&int.value()))
            } else {
                None
            };
            return self.push(Node::IntegerLit {
                value,
                digits,
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

        if let Some(arr) = node.as_array_node() {
            // Elements are `eval_value_container` operands — TYPED, never
            // `evaluate`d (`x = [h.default ||= 0]` keeps `h`'s indexed
            // narrowings on the oracle, rigor-rs#343).
            self.typed_depth += 1;
            let elements = self.lower_body(&arr.elements());
            self.typed_depth -= 1;
            return self.push(Node::ArrayLit {
                elements,
                span: span_of(&arr.location()),
            });
        }

        if let Some(hash) = node.as_hash_node() {
            // Lower each assoc's key + value (a call can hide in either).
            // `all_assoc` stays true only if every element is a proper assoc — a
            // `**splat` (non-assoc) makes the arity/keys unknown, so the typer
            // must fall back to the bare `Hash` nominal.
            let dup_keys = self.hash_keys_of(&hash.elements());
            let mut elements = Vec::new();
            let mut all_assoc = true;
            // Assoc keys/values are `eval_value_container` operand elements —
            // each re-gated on `OperandEffects.any?` (rigor-rs#343, #361).
            self.typed_depth += 1;
            for el in hash.elements().iter() {
                if let Some(assoc) = el.as_assoc_node() {
                    elements.push(self.lower_typed_operand(&assoc.key()));
                    elements.push(self.lower_typed_operand(&assoc.value()));
                } else {
                    all_assoc = false;
                    elements.push(self.lower_typed_operand(&el));
                }
            }
            self.typed_depth -= 1;
            return self.push(Node::HashLit {
                elements,
                all_assoc,
                dup_keys,
                span: span_of(&hash.location()),
            });
        }

        if let Some(range) = node.as_range_node() {
            // Lower both bounds for reachability; the node itself types
            // Dynamic. The ids stay linked on the node: a range evaluates
            // its bounds unconditionally in order (the reference's
            // `OPERAND_CONTAINERS` includes `RangeNode`), which the flow
            // replay reads (rigor-rs#306). The bounds are TYPED operands —
            // `eval_value_container` — `thread_operand`-gated per bound on
            // `OperandEffects.any?` (rigor-rs#343, #361).
            self.typed_depth += 1;
            let left = range.left().map(|l| self.lower_typed_operand(&l));
            let right = range.right().map(|r| self.lower_typed_operand(&r));
            self.typed_depth -= 1;
            return self.push(Node::Range {
                left,
                right,
                span: span_of(&range.location()),
            });
        }

        if let Some(khash) = node.as_keyword_hash_node() {
            // Bare keyword arguments — `foo(wait: 30.minutes)`. Prism wraps these
            // in a KeywordHashNode (not a HashNode); lower each assoc's key + value
            // so a call hiding in either is walked. Reuse the HashLit shape (Dynamic
            // is correct here — a keyword-hash is not a precise value).
            // Bare keyword args are scanned for duplicate keys too (`m(a: 1, a: 2)`
            // — Prism's KeywordHashNode, same `-w` warning as a braced literal).
            let dup_keys = self.hash_keys_of(&khash.elements());
            let mut elements = Vec::new();
            // Keyword arguments are `type_of` operands too (rigor-rs#343) —
            // gated per element on `OperandEffects.any?` (rigor-rs#361).
            self.typed_depth += 1;
            for el in khash.elements().iter() {
                if let Some(assoc) = el.as_assoc_node() {
                    elements.push(self.lower_typed_operand(&assoc.key()));
                    elements.push(self.lower_typed_operand(&assoc.value()));
                } else {
                    elements.push(self.lower_typed_operand(&el));
                }
            }
            self.typed_depth -= 1;
            // A bare keyword-hash argument is not a precise value carrier — keep
            // `all_assoc: false` so the typer leaves it the bare `Hash` nominal.
            return self.push(Node::HashLit {
                elements,
                all_assoc: false,
                dup_keys,
                span: span_of(&khash.location()),
            });
        }

        if let Some(parens) = node.as_parentheses_node() {
            // A parenthesized expression — `(30.seconds)`, `(15)`, grouped
            // operands, range endpoints. `(e)` is pure grouping (`(e)` ≡ `e`), so
            // a single-statement parens is UNWRAPPED to its inner node: a
            // parenthesized receiver then types precisely (`(15).foo` witnesses on
            // Integer — real-corpus coverage-gap audit). Multi-statement / empty
            // parens keep the block wrapper (their value is the last statement,
            // which the wrapper types as Dynamic — unchanged).
            let body = self.lower_optional_body(parens.body().as_ref());
            if let [only] = body[..] {
                // Mark the unwrapped id: `(nil)` is NOT a NilNode in the
                // reference's syntax-level reading, so the `nil&.m` literal
                // fold must not see through the parens.
                self.paren_unwrapped.push(only.0);
                return only;
            }
            let main_body = body.clone();
            return self.push(Node::BeginRescue {
                body,
                main_body,
                ensure_body: Vec::new(),
                clauses: Vec::new(),
                span: span_of(&parens.location()),
            });
        }

        if let Some(ivw) = node.as_instance_variable_write_node() {
            let name = constant_string(ivw.name().as_slice());
            let value = self.lower_node(&ivw.value());
            return self.push(Node::InstanceVariableWrite {
                name,
                value,
                name_span: span_of(&ivw.name_loc()),
                span: span_of(&ivw.location()),
            });
        }
        if let Some(cvw) = node.as_class_variable_write_node() {
            let name = constant_string(cvw.name().as_slice());
            let value = self.lower_node(&cvw.value());
            return self.push(Node::VariableWrite {
                name,
                value,
                span: span_of(&cvw.location()),
            });
        }
        if let Some(gvw) = node.as_global_variable_write_node() {
            let name = constant_string(gvw.name().as_slice());
            let value = self.lower_node(&gvw.value());
            return self.push(Node::VariableWrite {
                name,
                value,
                span: span_of(&gvw.location()),
            });
        }

        if let Some(ivr) = node.as_instance_variable_read_node() {
            return self.push(Node::VariableRead {
                name: constant_string(ivr.name().as_slice()),
                span: span_of(&ivr.location()),
            });
        }
        if let Some(cvr) = node.as_class_variable_read_node() {
            return self.push(Node::VariableRead {
                name: constant_string(cvr.name().as_slice()),
                span: span_of(&cvr.location()),
            });
        }
        if let Some(gvr) = node.as_global_variable_read_node() {
            return self.push(Node::VariableRead {
                name: constant_string(gvr.name().as_slice()),
                span: span_of(&gvr.location()),
            });
        }

        if let Some(cw) = node.as_constant_write_node() {
            let name = constant_string(cw.name().as_slice());
            let value = self.lower_node(&cw.value());
            return self.push(Node::ConstantWrite {
                name,
                value,
                span: span_of(&cw.location()),
            });
        }
        if let Some(cr) = node.as_constant_read_node() {
            return self.push(Node::ConstantRead {
                name: constant_string(cr.name().as_slice()),
                span: span_of(&cr.location()),
                dynamic_base: false,
                self_anchored: false,
                rooted: false,
            });
        }
        if let Some(cp) = node.as_constant_path_node() {
            // `Foo::Bar` — lower the parent scope (it may itself be a call/const).
            if let Some(parent) = cp.parent() {
                self.lower_node(&parent);
            }
            return self.push(Node::ConstantRead {
                name: constant_path_string(node),
                span: span_of(&cp.location()),
                dynamic_base: strict_constant_path_string(node).is_none(),
                self_anchored: self_anchored_constant_path(node),
                rooted: rooted_constant_path(node),
            });
        }

        if let Some(self_node) = node.as_self_node() {
            return self.push(Node::SelfExpr {
                span: span_of(&self_node.location()),
            });
        }

        if let Some(interp) = node.as_interpolated_string_node() {
            // Lower every interpolation part (`#{call}`) so its calls are walked,
            // and keep the ids: the node types as a `String` instance, with the
            // parts as the reachability carrier. Parts are `type_of` operands —
            // `eval_interpolation` types each part (rigor-rs#343) — gated
            // per element on `OperandEffects.any?` (rigor-rs#361).
            self.typed_depth += 1;
            let parts: Vec<NodeId> = interp
                .parts()
                .iter()
                .map(|p| self.lower_typed_operand(&p))
                .collect();
            self.typed_depth -= 1;
            return self.push(Node::InterpolatedString {
                parts,
                span: span_of(&interp.location()),
            });
        }
        if let Some(interp) = node.as_interpolated_symbol_node() {
            // `:"sym#{x}"` — lower into a dedicated `InterpolatedSymbol`
            // variant: a structural twin of `InterpolatedString` that types as
            // `Nominal { Symbol }` instead of `Nominal { String }`. Using the
            // generic `Statements` wrapper here was the bug: value-descent
            // resolves a `Statements` to its LAST child (the trailing string
            // fragment), so the symbol mis-typed as a `String` and minted
            // false `undefined-method` diagnostics (e.g. `.to_proc`). `parts`
            // is kept as the reachability carrier so a local read inside the
            // interpolation stays visible to structural walks like
            // `flow.dead-assignment`, exactly as `InterpolatedString` does.
            // Parts are `type_of` operands (rigor-rs#343), gated per element
            // on `OperandEffects.any?` (rigor-rs#361).
            self.typed_depth += 1;
            let parts: Vec<NodeId> = interp
                .parts()
                .iter()
                .map(|p| self.lower_typed_operand(&p))
                .collect();
            self.typed_depth -= 1;
            return self.push(Node::InterpolatedSymbol {
                parts,
                span: span_of(&interp.location()),
            });
        }
        if let Some(embedded) = node.as_embedded_statements_node() {
            // The `#{ … }` inside a string: lower its statements and KEEP the link
            // (a `Statements` wrapper, mirroring Prism's tree shape). The link
            // matters for structural walks — `flow.dead-assignment` must see a
            // local read inside interpolation (`"v=#{x}"` reads `x`); orphaning
            // the lowered statements would lose that read. (The env-builder only
            // descends a `Statements` that is a direct body statement, so a nested
            // interpolation wrapper has no effect there.)
            let body = embedded
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            return self.push(Node::Statements {
                body,
                span: span_of(&embedded.location()),
                kind: StatementsKind::Sequence,
            });
        }

        if let Some(lambda) = node.as_lambda_node() {
            // `-> { … }` / `->(x) { … }`. Lower the body so calls/reads inside stay
            // visible to the rule walk (closing the `Node::Other` soundness gap),
            // AND mark the lambda boundary so `flow.return-in-ensure` recognises it
            // as a return barrier. The body is iterated for recovery purposes:
            // a compound index write inside widens its receiver on the oracle
            // even when the lambda is never called (rigor-rs#312). A compound
            // ATTRIBUTE write's scope effect does NOT cross the closure —
            // `OperandEffects.any?` excludes `Call*WriteNode`s (rigor-rs#343).
            // The body is not an operand position either — clear the gate
            // while it lowers (rigor-rs#361).
            self.recovery_iterative += 1;
            self.closure_depth += 1;
            let operand_typed = std::mem::replace(&mut self.typed_depth, 0);
            let lexical_loop = std::mem::replace(&mut self.lexical_loop, 0);
            if let Some(b) = lambda.body() {
                self.iterated_body_spans.push(span_of(&b.location()));
            }
            let body = self.lower_optional_body(lambda.body().as_ref());
            self.typed_depth = operand_typed;
            self.lexical_loop = lexical_loop;
            self.recovery_iterative -= 1;
            self.closure_depth -= 1;
            return self.push(Node::Lambda {
                body,
                locals: constant_list_names(&lambda.locals()),
                span: span_of(&lambda.location()),
            });
        }

        UNMATCHED
    }
}
