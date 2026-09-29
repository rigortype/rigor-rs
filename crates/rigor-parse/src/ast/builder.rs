//! The lowering walk: `Builder` turns one borrowed Prism node into owned arena
//! nodes (`lower_node`).

use crate::ruby_prism::{self, Node as PrismNode};

use super::{
    all_param_names, block_param_names, body_has_explicit_return, collect_defined_operand_children,
    collect_recoverable_children, constant_node_name, constant_path_string, constant_string,
    direct_method_names, discover_visibilities_and_includes, for_index_writes, lower_multi_targets,
    rescue_reference_index_writes,
    param_shape_of, plain_positional_params, rooted_constant_path, self_anchored_constant_path,
    span_of, strict_constant_path_string, JumpKind, Node, NodeId, ParamShape, RescueClause,
    StatementsKind,
};

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
}

impl<'src> Builder<'src> {
    /// Push an owned node, returning its fresh [`NodeId`].
    fn push(&mut self, node: Node) -> NodeId {
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
            );
            // Lower the expressions embedded in non-local targets so the
            // structural walks keep seeing those reads/calls — the old
            // recovered-children carrier did, and `flow.dead-assignment`
            // depends on it (netrc `item[3], item[5] = info`).
            let target_exprs: Vec<NodeId> =
                recovered.iter().map(|c| self.lower_node(c)).collect();
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

        if let Some(call) = node.as_call_node() {
            let receiver = call.receiver().map(|r| self.lower_node(&r));
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
                        self.lower_optional_body(bn.body().as_ref())
                    } else if let Some(ba) = b.as_block_argument_node() {
                        ba.expression()
                            .map(|e| vec![self.lower_node(&e)])
                            .unwrap_or_default()
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
                is_unless: false,
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
            // A `when` branch: lower its condition expressions and body into the
            // dedicated `When` variant's SEPARATE lists (pre-split, both were
            // concatenated into a reused `BeginRescue` carrier's body).
            let conditions: Vec<NodeId> = when_node
                .conditions()
                .iter()
                .map(|c| self.lower_node(&c))
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
            });
            let mut body = vec![self.lower_node(&in_node.pattern())];
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
            let body = while_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
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
            let body = until_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
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
            let body = for_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
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
            while let Some(r) = rescue {
                let mut exceptions = Vec::new();
                for exc in r.exceptions().iter() {
                    let id = self.lower_node(&exc);
                    body.push(id);
                    exceptions.push(id);
                }
                let mut clause_body = Vec::new();
                if let Some(s) = r.statements() {
                    let ids = self.lower_body(&s.body());
                    body.extend(ids.iter().copied());
                    clause_body = ids;
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
            let right = self.lower_node(&and_node.right());
            return self.push(Node::Logical {
                left,
                right,
                is_and: true,
                span: span_of(&and_node.location()),
            });
        }

        if let Some(or_node) = node.as_or_node() {
            let left = self.lower_node(&or_node.left());
            let right = self.lower_node(&or_node.right());
            return self.push(Node::Logical {
                left,
                right,
                is_and: false,
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
            // `all_assoc` stays true only if every element is a proper assoc — a
            // `**splat` (non-assoc) makes the arity/keys unknown, so the typer
            // must fall back to the bare `Hash` nominal.
            let dup_keys = self.hash_keys_of(&hash.elements());
            let mut elements = Vec::new();
            let mut all_assoc = true;
            for el in hash.elements().iter() {
                if let Some(assoc) = el.as_assoc_node() {
                    elements.push(self.lower_node(&assoc.key()));
                    elements.push(self.lower_node(&assoc.value()));
                } else {
                    all_assoc = false;
                    elements.push(self.lower_node(&el));
                }
            }
            return self.push(Node::HashLit {
                elements,
                all_assoc,
                dup_keys,
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

        if let Some(khash) = node.as_keyword_hash_node() {
            // Bare keyword arguments — `foo(wait: 30.minutes)`. Prism wraps these
            // in a KeywordHashNode (not a HashNode); lower each assoc's key + value
            // so a call hiding in either is walked. Reuse the HashLit shape (Dynamic
            // is correct here — a keyword-hash is not a precise value).
            // Bare keyword args are scanned for duplicate keys too (`m(a: 1, a: 2)`
            // — Prism's KeywordHashNode, same `-w` warning as a braced literal).
            let dup_keys = self.hash_keys_of(&khash.elements());
            let mut elements = Vec::new();
            for el in khash.elements().iter() {
                if let Some(assoc) = el.as_assoc_node() {
                    elements.push(self.lower_node(&assoc.key()));
                    elements.push(self.lower_node(&assoc.value()));
                } else {
                    elements.push(self.lower_node(&el));
                }
            }
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
            // parts as the reachability carrier.
            let parts: Vec<NodeId> = interp
                .parts()
                .iter()
                .map(|p| self.lower_node(&p))
                .collect();
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
            let parts: Vec<NodeId> = interp
                .parts()
                .iter()
                .map(|p| self.lower_node(&p))
                .collect();
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
            // as a return barrier.
            let body = self.lower_optional_body(lambda.body().as_ref());
            return self.push(Node::Lambda {
                body,
                locals: constant_list_names(&lambda.locals()),
                span: span_of(&lambda.location()),
            });
        }

        if let Some(ret) = node.as_return_node() {
            // An explicit `return` — a real owned variant (sig-gen's
            // `DefReturnTyper` port needs the VALUE expressions to union a def's
            // explicit returns). The value exprs are FULLY lowered as children,
            // a strict superset of the old recovered-children carrier (reads /
            // op-writes / calls inside a return stay visible to `flow.dead-
            // assignment` + the call rules, plus literals now exist too). The
            // node itself stays a STATEMENT: the typer's catch-all types it
            // `Dynamic[top]` exactly like the `Statements` carrier it replaces.
            let values = ret
                .arguments()
                .map(|a| self.lower_body(&a.arguments()))
                .unwrap_or_default();
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
            let recovered = collect_defined_operand_children(&defined.value());
            if recovered.is_empty() {
                return self.push(Node::Other { span, jump: None });
            }
            let body: Vec<NodeId> = recovered.iter().map(|c| self.lower_node(c)).collect();
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
            let recovered = collect_recoverable_children(node);
            if recovered.is_empty() {
                return self.push(Node::Other { span, jump: None });
            }
            let body: Vec<NodeId> = recovered.iter().map(|c| self.lower_node(c)).collect();
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
            self.push(Node::UnmodeledWrite { span });
            let recovered = collect_recoverable_children(node);
            if recovered.is_empty() {
                return self.push(Node::Other { span, jump: None });
            }
            let body: Vec<NodeId> =
                recovered.iter().map(|c| self.lower_node(c)).collect();
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
        let recovered = collect_recoverable_children(node);
        if recovered.is_empty() {
            return self.push(Node::Other { span, jump: None });
        }
        let body: Vec<NodeId> = recovered.iter().map(|c| self.lower_node(c)).collect();
        self.push(Node::Statements { body, span, kind: StatementsKind::Recovered })
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

/// The names a Prism scope's `locals` constant list holds (`BlockNode` /
/// `LambdaNode` — the names that scope BINDS: parameters of every form,
/// `;`-declared block-locals, and locals first written inside it; never a
/// captured outer local), decoded to owned `String`s.
fn constant_list_names(list: &ruby_prism::ConstantList<'_>) -> Vec<String> {
    list.iter().map(|c| constant_string(c.as_slice())).collect()
}

/// Whether a Prism node is an assignment the lowering cannot reproduce —
/// every operator/and/or write on a non-local target (`@x += 1`, `K += 1`,
/// `a[i] ||= v`, `x.f &&= v`), a `K::V` constant-path write, and the
/// pattern-binding nodes (`expr => pat`, `expr in pat`). Local writes,
/// multiwrites, `for` indexes, and the plain `K = v`/`@x = v`/`$g = v`/`@@x =
/// v` forms have owned variants already; multiwrite TARGETS never reach
/// `lower_node` standalone (a `MultiWriteNode` wraps them).
fn is_unmodeled_write(node: &PrismNode<'_>) -> bool {
    node.as_call_and_write_node().is_some()
        || node.as_call_operator_write_node().is_some()
        || node.as_call_or_write_node().is_some()
        || node.as_class_variable_and_write_node().is_some()
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
        || node.as_index_and_write_node().is_some()
        || node.as_index_operator_write_node().is_some()
        || node.as_index_or_write_node().is_some()
        || node.as_instance_variable_and_write_node().is_some()
        || node.as_instance_variable_operator_write_node().is_some()
        || node.as_instance_variable_or_write_node().is_some()
        || node.as_match_write_node().is_some()
        || node.as_match_predicate_node().is_some()
        || node.as_match_required_node().is_some()
}

/// A prism integer's value when it fits `i64`, from its little-endian `u32`
/// digits; `None` for a Bignum.
fn integer_value(int: &ruby_prism::Integer<'_>) -> Option<i64> {
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
