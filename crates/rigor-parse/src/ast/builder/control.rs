//! `Builder::lower_control` holds the control-flow arms of `Builder::lower_node`:
//! the conditionals, `case`, the loops, `begin`/`rescue`/`ensure`, and `and`/`or`.

use crate::ruby_prism::Node as PrismNode;

use super::{
    case_arm_exits, constant_string, for_index_writes, literal_truthiness, pattern_bound_locals,
    rescue_reference_index_writes, span_of, unconditional_exit, Builder, Node, NodeId, RescueClause,
    Span, UNMATCHED,
};

impl<'src> Builder<'src> {
    /// [`Self::lower_node`]'s control-flow arms: `if`/`unless`/`else`,
    /// `case`/`when`/`in`, the loops, `begin`/`rescue`/`ensure`, `and`/`or`.
    #[inline(never)]
    pub(crate) fn lower_control(&mut self, node: &PrismNode<'_>, span: Span) -> NodeId {
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
}
