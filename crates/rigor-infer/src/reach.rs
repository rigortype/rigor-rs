//! Argument reach: what the REFERENCE's type for an argument expression can
//! hold ([`Reach`], computed by [`Typer::arg_reach`] and its local / ivar /
//! cvar / gvar / chain arms) — the gate of the #521 / #1021 untyped-argument
//! declines — with the definite-assignment and untyped-root helpers behind it.

use rigor_parse::{LoweredAst, Node, NodeId, Span, StatementsKind};
use rigor_types::Scalar;

use crate::dead::DeadPositions;
use crate::folding;
use crate::Typer;

impl<'i> Typer<'i> {
    /// What can the REFERENCE's type for this argument expression hold? — the
    /// gate of the #521 / #1021 declines (see [`Typer::type_implicit_self_call`]
    /// and [`Typer::rbs_dispatch_declines_on_untyped_arg`]).
    ///
    /// Upstream asks `imprecise_arg?(t)` (#1021, `5496acd6`): is the argument the
    /// bare untyped carrier `Dynamic[Top]`, OR a UNION with such a member
    /// (`Dynamic[top] | "x"`)? Either one skips the strict and alias overload
    /// passes, so the gradual pass answers every overload that accepts ALL of
    /// the union's members and the dispatch joins their returns.
    /// [`Reach::untyped`] is that question; the other two flags describe the
    /// union's precise members, which only `rand` needs (see [`Reach`]).
    ///
    /// rigor-rs cannot answer this from the argument's TYPE. A use site inside a
    /// method body reads an EMPTY `TypeEnv` (the rules layer's `ScopedEnv::at`:
    /// a Ruby method body is an independent local scope, and reading the flat
    /// top-level env there typed the wrong value — two `wrong-arity` and two
    /// `undefined-method` false positives on rigor-survey), so EVERY def-body
    /// local read answers `Dynamic[top]` here. `def f(u) = Float(u)`
    /// (reference-SILENT) and `s = "x"; Float(s)` (reference-FIRING) are
    /// literally the same `TypeId` at this call site.
    ///
    /// So the answer is a SYNTACTIC reach analysis over the argument's ROOT (a
    /// local, ivar, cvar, gvar or constant — [`untyped_expr_root`]): which
    /// VALUES can reach this read, and is any of them untyped on the reference?
    /// Each root kind has its own arm, a port of how the reference seeds that
    /// kind of variable; the value side is [`Typer::expr_reach`]. A class-GUARD
    /// on a local (`is_a?` & co., `case`) still refuses outright — the
    /// reference narrows the parameter to a Nominal and fires (rows
    /// q6/q8/q9/q10/a25/a31, and l29 where the guard follows a `Dynamic | "x"`
    /// rebind).
    ///
    /// Every error in this analysis must fall on the DECLINE side: a value the
    /// port cannot place is untyped ([`Reach::UNKNOWN`]), because a decline
    /// withholds a diagnostic (a coverage loss) while a wrong precise answer
    /// mints one the reference does not emit.
    ///
    /// COST: two arena scans per root visited, so quadratic in the number of
    /// such folds per file. Measured on a synthetic worst case (9000 lines, 4500
    /// `Float`/`Integer`/`Array` calls on bare parameters): 1.48s vs 0.89s user
    /// for the same file with literal arguments, where the `type_of` gate keeps
    /// the helper from running at all. Real files carry a handful of these.
    /// #332 widened the trigger a little — `arg_reach` also runs on
    /// nominal-typed arguments of `Constant`-receiver calls and non-bare joins,
    /// though literal arguments still short-circuit in [`Typer::expr_reach`]
    /// before any scan. Revisit if a sweep file regresses.
    pub(crate) fn arg_reach(&self, ast: &LoweredAst, arg: NodeId) -> Reach {
        self.expr_reach(ast, arg, &mut Vec::new())
    }

    /// The VALUE side of [`Typer::arg_reach`]: what the reference's type for
    /// the value of expression `id` can hold. A branching value (`c ? "x" : s`,
    /// `s || "x"`, an `if`/`case` used as a value) is the join of its arms — the
    /// reference unions them, so one untyped arm makes the whole value
    /// imprecise (row l31). A literal is precise; anything else goes through its
    /// root ([`Typer::chain_reach`]).
    fn expr_reach(&self, ast: &LoweredAst, id: NodeId, seen: &mut Vec<String>) -> Reach {
        match ast.get(id) {
            Node::If {
                predicate,
                then_body,
                else_body,
                is_unless,
                ..
            } => match self.expr_truthiness(ast, *predicate, seen) {
                // rigor-rs#368 — `live_branch_for_if`: a folded predicate
                // contributes only the live arm's value; the dead arm's
                // writes and expressions are never evaluated.
                Some(truthy) => self.body_value_reach(
                    ast,
                    if truthy != *is_unless {
                        then_body
                    } else {
                        else_body
                    },
                    seen,
                ),
                None => self
                    .body_value_reach(ast, then_body, seen)
                    .join(self.body_value_reach(ast, else_body, seen)),
            },
            Node::Logical { left, right, .. } => {
                self.expr_reach(ast, *left, seen).join(self.expr_reach(ast, *right, seen))
            }
            Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
                self.body_value_reach(ast, body, seen)
            }
            // A recovery / inert carrier's value is not its last recovered child
            // (`s rescue nil` may be `nil`; `defined?(s)` is a String or `nil`).
            Node::Statements { .. } => Reach::UNKNOWN,
            // Also the carrier an `if`'s `else` clause lowers to (no clauses).
            Node::BeginRescue { body, clauses, .. } => {
                let mut reach = self.body_value_reach(ast, body, seen);
                for c in clauses {
                    // rigor-rs#368 — `live_rescues`: a terminating arm's value
                    // does not join the `begin`'s type.
                    if self.arm_exits(ast, &c.body, seen) {
                        continue;
                    }
                    reach = reach.join(self.body_value_reach(ast, &c.body, seen));
                }
                reach
            }
            Node::Case { branches, else_body, .. } => {
                let mut reach = self.body_value_reach(ast, else_body, seen);
                for &b in branches {
                    reach = reach.join(match ast.get(b) {
                        Node::When { body, .. } => self.body_value_reach(ast, body, seen),
                        _ => Reach::UNKNOWN,
                    });
                }
                reach
            }
            Node::StringLit { .. }
            | Node::FloatLit { .. }
            | Node::SymbolLit { .. }
            | Node::NilLit { .. }
            | Node::TrueLit { .. }
            | Node::FalseLit { .. } => Reach::LITERAL,
            // Precise literals the reference does NOT carry as `Type::Constant`
            // — an interpolated string / symbol is its literal-string carrier,
            // an Array / Hash literal a `Tuple` / `HashShape` — so a
            // `Constant`-receiver call with one cannot value-fold either
            // (`"abc"[[1]]` keeps the `String | nil` join, rigor-rs#332).
            Node::InterpolatedString { .. }
            | Node::InterpolatedSymbol { .. }
            | Node::ArrayLit { .. }
            | Node::HashLit { .. } => Reach::PINLESS,
            // `rand`'s `(?0) -> Float` overload accepts the literal `0`: it
            // stays `opaque` for `declines_rand`, but `0` is still a
            // `Type::Constant` on the reference — `"abc"[0]` folds to `"a"`
            // — so it is `pinned` too.
            Node::IntegerLit { value, .. } => {
                if *value == Some(0) {
                    Reach::PINNED_OPAQUE
                } else {
                    Reach::LITERAL
                }
            }
            // A range literal pins to `Constant[Range]` on the reference only
            // when every endpoint is static — a literal or an expression it
            // types `Type::Constant` (`static_range_endpoint`). A non-static
            // endpoint leaves `Nominal[Range]` — still precise, still `opaque`
            // for `rand`, but no `Constant`-receiver call folds through it:
            // `"abc"[v..]` keeps `String | nil` (#332).
            Node::Range { left, right, .. } => Reach {
                untyped: false,
                precise: true,
                opaque: true,
                multi: false,
                pinned: [left, right].iter().all(|e| match e {
                    Some(e) => self.expr_reach(ast, *e, seen).pins_one_constant(),
                    None => true,
                }),
            },
            _ => {
                // The reference pins `Type::Constant` whenever every reaching
                // value folds to ONE scalar — a literal, a call whose operands
                // pin (`"x".to_i` -> `Constant[0]`, `1 + 1` -> `Constant[2]`
                // — the `ConstantFolding` whitelist `folding::fold`
                // implements), or a chain through a pinned local (`w = v + 1`
                // with `v = 1`, and `v = 1; v = 1 if c`, whose `1 | 1` the
                // member fold still collapses to `Constant[1]`). That is
                // [`Typer::expr_scalar`]; a hit here is exactly `pinned`
                // (rigor-rs#332).
                if let Some(scalar) = self.expr_scalar(ast, id, seen) {
                    return pinned_scalar_reach(&scalar);
                }
                let mut reach = self.chain_reach(ast, id, seen);
                // The operands of a call COMPOSE into its result rather than
                // alternate like branch arms: an untyped or multi-valued
                // argument makes the reference's type for the call carry that
                // carrier (`1 + v` is `2 | 3` when `v` is `1 | 2`, #332), while
                // a precise receiver plus a precise argument still make ONE
                // value — so the arg folds in by `compose`, not `join`.
                let mut cur = id;
                while let Node::Call { receiver, args, .. } = ast.get(cur) {
                    for &a in args {
                        reach = reach.compose(self.expr_reach(ast, a, seen));
                    }
                    match receiver {
                        Some(r) => cur = *r,
                        None => break,
                    }
                }
                reach
            }
        }
    }

    /// The single `Scalar` every value reaching `id` folds to — the foldable
    /// view of [`Reach::pinned`] (rigor-rs#332). `Some` only when the
    /// expression is a literal scalar, a call whose operand pins let
    /// [`folding::fold`] answer a scalar (`"x".upcase`, `1 + v` with `v = 1`),
    /// or a local whose reaching values all pin to the SAME scalar —
    /// `v = 1; v = 1 if c` still collapses to `Constant[1]` on the reference,
    /// which is why the gate is `pin.is_some()`, not `!multi`. `None` is the
    /// decline side: an unpinned operand, a `fold` miss (`to_i` is not in the
    /// whitelist), an untyped or imprecise reach.
    ///
    /// Shares `seen` with the reach walk — a local's read is keyed exactly as
    /// [`Typer::root_reach`] keys a `Local` root, so a self-referential write
    /// (`while c; v = "abc"[v]; end`) terminates on the decline side.
    pub(crate) fn expr_scalar(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        seen: &mut Vec<String>,
    ) -> Option<Scalar> {
        if let Some(scalar) = literal_scalar(ast, id) {
            return Some(scalar);
        }
        match ast.get(id) {
            Node::LocalVariableRead { name, span } => {
                let key = format!("{name}@{}", span.0);
                if seen.contains(&key) || seen.len() >= 4 {
                    return None;
                }
                seen.push(key);
                let (reach, pin) = self.local_reach(ast, name, *span, seen, false);
                seen.pop();
                // `pin` is only worth anything beside a precise reach — every
                // contributor having folded to the same scalar is meaningless
                // when `Dynamic[top]` can also reach.
                if reach.precise && !reach.untyped {
                    pin
                } else {
                    None
                }
            }
            Node::Call { receiver: Some(recv), method, args, .. } => {
                let recv_scalar = self.expr_scalar(ast, *recv, seen)?;
                let arg_scalars = args
                    .iter()
                    .map(|&a| self.expr_scalar(ast, a, seen))
                    .collect::<Option<Vec<_>>>()?;
                if folding::sidecar_blows_up(method, &arg_scalars) {
                    return None;
                }
                folding::fold(&recv_scalar, method, &arg_scalars)
            }
            _ => None,
        }
    }

    /// The value of a statement list — its last statement, or `nil` when empty
    /// (an `if` with no `else`).
    fn body_value_reach(&self, ast: &LoweredAst, body: &[NodeId], seen: &mut Vec<String>) -> Reach {
        match body.last() {
            Some(&last) => self.expr_reach(ast, last, seen),
            None => Reach::LITERAL,
        }
    }

    /// A root read, or a call CHAIN over one. An arbitrary chain over an untyped
    /// root is untyped on the reference as well (a method on `Dynamic[Top]`
    /// answers `Dynamic[Top]`) — which is what reaches fixture 60's
    /// `Float(kwargs[:upload_duration])` — and a chain over a union keeps that
    /// untyped member beside whatever the precise members answer. A chain over
    /// a precise root is precise, but never a `rand`-safe literal. An
    /// expression with no root (an implicit-self call, `self`) is precise —
    /// UNLESS the link itself is `dynamic_top` on the reference
    /// (rigor-rs#368): an unresolved implicit-self call (`Float(q)` with `q`
    /// never bound — `call.unresolved-toplevel` already names it at file
    /// scope), a bare `self` outside any class/module, or a call whose
    /// pinned receiver's class lacks the method (`Float(x.no_such)` — the
    /// inner `call.undefined-method` fires, but the value it feeds is
    /// `dynamic_top`, so the `Float` declines).
    fn chain_reach(&self, ast: &LoweredAst, id: NodeId, seen: &mut Vec<String>) -> Reach {
        let mut cur = id;
        for _ in 0..8 {
            let Node::Call {
                receiver, method, ..
            } = ast.get(cur)
            else {
                break;
            };
            match receiver {
                Some(r) => {
                    if let Some(cls) = self.expr_receiver_class(ast, *r, seen) {
                        // `cls`'s whole chain is loaded and lacks `method` ⇒
                        // witnessed-absent ⇒ the link types `dynamic_top`.
                        if !self.index.class_has_method(cls, method) {
                            return Reach::UNTYPED;
                        }
                    }
                    cur = *r;
                }
                None => {
                    if !self.implicit_self_resolves(ast, cur, method) {
                        return Reach::UNTYPED;
                    }
                    break;
                }
            }
        }
        if let Node::SelfExpr { span } = ast.get(cur) {
            if self.enclosing_prefix(*span).is_empty() {
                return Reach::UNTYPED;
            }
            return Reach::OPAQUE;
        }
        let Some(root) = untyped_expr_root(ast, id, 8) else { return Reach::OPAQUE };
        let reach = self.root_reach(ast, &root, ast.get(id).span(), seen);
        if matches!(ast.get(id), Node::Call { .. }) {
            Reach {
                untyped: reach.untyped,
                precise: reach.precise,
                opaque: reach.precise,
                multi: reach.multi,
                // A call over even a pinned literal is only `pinned` on the
                // reference when the call itself folds to a `Constant`
                // (`"x".to_i` -> `Constant[0]`), which this analysis cannot
                // see — the conservative `false` (rigor-rs#332).
                pinned: false,
            }
        } else {
            reach
        }
    }

    /// The class a pinned receiver dispatches on for the missing-method link
    /// check — `expr_scalar` carries the value, [`folding::scalar_class`] the
    /// class that value's calls dispatch against.
    fn expr_receiver_class(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        seen: &mut Vec<String>,
    ) -> Option<&'static str> {
        self.expr_scalar(ast, id, seen)
            .map(|s| folding::scalar_class(&s))
    }

    /// Whether the receiverless (`implicit-self`) call `call_id` resolves to
    /// anything the reference could type. The gates mirror
    /// `call.unresolved-toplevel`'s silence envelope: at file scope a
    /// toplevel `def`, an `Object`/`Kernel` surface name, or `gem`; inside a
    /// class/module body additionally the enclosing class's own project
    /// methods (and any core class surface the qualified name loads). An
    /// unresolved call types `dynamic_top` on the reference — never a
    /// dispatchable value — so `chain_reach` answers untyped.
    fn implicit_self_resolves(&self, ast: &LoweredAst, call_id: NodeId, method: &str) -> bool {
        if method == "gem" {
            return true;
        }
        // `puts`/`raise`/`p` and friends live on Object's (Kernel-included)
        // instance surface at every scope.
        if self.index.class_has_method("Object", method) {
            return true;
        }
        if self.source.is_toplevel_def(Some(ast.file_key()), method)
            || self.toplevel_def_in_file(ast, method)
        {
            return true;
        }
        let prefix = self.enclosing_prefix(ast.get(call_id).span());
        if prefix.is_empty() {
            return false;
        }
        let qual = prefix.join("::");
        self.source
            .project_declares_method(self.file_key(), &qual, method)
            || self.index.class_has_method(&qual, method)
    }

    /// A `def <name>` at FILE scope in this AST — the same-file fallback for
    /// `is_toplevel_def` when the `Typer` was built on an empty
    /// [`crate::SourceIndex`] (unit tests). Definitions nested inside a
    /// class/module/def are not file-scope.
    fn toplevel_def_in_file(&self, ast: &LoweredAst, name: &str) -> bool {
        let containers: Vec<Span> = ast
            .iter()
            .filter_map(|(_, n)| match n {
                Node::ClassDef { span, .. }
                | Node::ModuleDef { span, .. }
                | Node::Definition { span, .. } => Some(*span),
                _ => None,
            })
            .collect();
        ast.iter().any(|(_, n)| {
            matches!(n, Node::Definition { name: Some(m), span, .. }
                if m == name
                    && !containers
                        .iter()
                        .any(|&c| c != *span && c.0 <= span.0 && span.1 <= c.1))
        })
    }

    /// Dispatch a root to its arm. `seen` carries the `(root, position)` pairs
    /// already on the recursion, so a self-referential value (`s = s.to_s`,
    /// `@x = @x.foo`) terminates. A revisited pair contributes NOTHING — the
    /// least fixpoint, which is exact here: an untyped value can only originate
    /// from a non-cyclic source (a parameter, an unwritten variable, an
    /// untyped write), and every such source is joined in by the outer visit.
    /// Running out of depth instead answers [`Reach::UNKNOWN`] — the decline
    /// side.
    fn root_reach(
        &self,
        ast: &LoweredAst,
        root: &UntypedRoot,
        use_span: rigor_parse::Span,
        seen: &mut Vec<String>,
    ) -> Reach {
        let key = format!("{}@{}", root.spelling(), use_span.0);
        if seen.contains(&key) {
            return Reach::NONE;
        }
        if seen.len() >= 4 {
            return Reach::UNKNOWN;
        }
        seen.push(key);
        let reach = match root {
            UntypedRoot::Local(name) => self.local_reach(ast, name, use_span, seen, false).0,
            UntypedRoot::Ivar(name) => self.ivar_reach(ast, name, use_span, seen),
            UntypedRoot::Cvar(name) => self.cvar_reach(ast, name, use_span, seen),
            UntypedRoot::Gvar(name) => self.gvar_reach(ast, name, use_span, seen),
            UntypedRoot::Const(name) => {
                if self.const_is_reference_untyped(name, use_span) {
                    Reach::UNTYPED
                } else {
                    Reach::OPAQUE
                }
            }
        };
        seen.pop();
        reach
    }

    /// The LOCAL arm of [`Typer::root_reach`].
    ///
    /// The REGION whose writes and guards decide the answer is the innermost
    /// enclosing `def`. With NO enclosing `def` the use site must sit directly
    /// in a PROC-LIKE binder body (`->`, `lambda {}`, `proc {}`, `Proc.new {}`),
    /// whose parameters the reference carries as `Dynamic[Top]` exactly like a
    /// method's (rows r6/r7/r8, and the gitlab-foss `filter_evaluator.rb:15`
    /// site); the region is then the whole file, and writes inside an unrelated
    /// `def` are excluded as a different scope. An ORDINARY block's parameter
    /// is deliberately NOT admitted there — the reference types it from the RBS
    /// yield (`[1, 2].each { |x| Float(x) }` fires `for 1.0`, row m11) — so the
    /// innermost binder must be proc-like. A write INSIDE a `->` body never
    /// binds on the reference (rows r11/p13), while the `lambda {}` / `proc {}`
    /// / ordinary-block spellings DO (rows m1/m2/m6/m13), so a
    /// `Node::Lambda`-interior write is skipped and every other write counts.
    ///
    /// Inside the region the reaching values are, flow-approximately:
    ///
    /// * the local's INITIAL value — untyped for a parameter (the reference
    ///   carries every parameter kind, defaults and keywords included, as
    ///   `Dynamic[Top]`: rows l14-l17), and `nil` for a plain local. A name the
    ///   `def` does not list as a parameter can still be a BLOCK parameter
    ///   (which the arena does not record) when the read sits inside a block;
    ///   it is then taken as untyped unless the region writes it OUTSIDE every
    ///   block around the read (a captured outer local, row l5). A local with
    ///   no write at all is untyped for the same reason (a pattern or `for`
    ///   binding is not an arena write either).
    /// * every write of the name — EXCEPT that a DEFINITE assignment on the
    ///   read's statement path ([`latest_definite_assignment`]) cuts off the
    ///   initial value and every earlier write: `s = "x"; Float(s)` fires (rows
    ///   q4/l04), and so does an `if`/`else` that rebinds on BOTH arms (row l05),
    ///   while a conditional rebind leaves the parameter reachable — the
    ///   reference's `Dynamic[top] | "x"`, which #1021 declines on (rows a7/a20,
    ///   l01/l09/l11/l19/l23/l24). A write lexically AFTER the read only counts
    ///   when a loop or block can carry it back round (row l27 — the
    ///   parameter itself reaches the read there).
    /// * `x ||= v` / `x += v` / `x &&= v` never cut anything off (each may keep
    ///   the old value) and add `v`'s reach — so `s ||= "x"` over a parameter
    ///   declines (row q3, retracted upstream by #1021) while `t = nil; t ||=
    ///   "x"` still fires (row l06).
    ///
    /// A `rescue => name` binding and a class guard refuse the whole test (the
    /// reference types both precisely). With `skip_class_guards` a guard of the
    /// exact shape `root.is_a?(C)` / `kind_of?` / `instance_of?` is instead
    /// stepped over, so the caller can ask what reaches the root BEFORE the
    /// guard narrows it ([`Typer::arg_is_guarded_parameter`]); every other guard
    /// shape (`C === root`, a `case`) still refuses.
    ///
    /// The pair's second half is the PIN: `Some(s)` when every value that can
    /// reach the read folds to the same scalar `s` — a single write's literal
    /// or foldable chain, or several writes whose scalars agree (`v = 1; v =
    /// 1 if c` — the reference's member fold still collapses `1 | 1` to
    /// `Constant[1]`, rigor-rs#332). It stays `None` on an untyped, opaque,
    /// disagreeing or unscalarable (`op=`, multi-write) contributor — every
    /// early return carries `None` — and is only meaningful to callers beside
    /// the returned [`Reach`] (see [`Typer::expr_scalar`]).
    pub(crate) fn local_reach(
        &self,
        ast: &LoweredAst,
        root: &str,
        use_span: rigor_parse::Span,
        seen: &mut Vec<String>,
        skip_class_guards: bool,
    ) -> (Reach, Option<Scalar>) {
        let contains = |s: rigor_parse::Span, i: rigor_parse::Span| s.0 <= i.0 && i.1 <= s.1;
        // One pass for the scope shapes: every `def` span, every `->` span, the
        // innermost `def` around the use site, the narrowest binder (a `def`, a
        // `->`, or any block-bearing call) around it, and every block extent
        // around it.
        let mut def_spans: Vec<rigor_parse::Span> = Vec::new();
        let mut lambda_spans: Vec<rigor_parse::Span> = Vec::new();
        let mut def: Option<(rigor_parse::Span, NodeId)> = None;
        let mut binder: Option<(rigor_parse::Span, bool)> = None;
        let mut blocks_around: Vec<rigor_parse::Span> = Vec::new();
        let mut loop_spans: Vec<rigor_parse::Span> = Vec::new();
        let mut note_binder = |span: rigor_parse::Span, proc_like: bool| {
            if contains(span, use_span) {
                let narrower = binder.is_none_or(|(b, _)| span.1 - span.0 < b.1 - b.0);
                if narrower {
                    binder = Some((span, proc_like));
                }
            }
        };
        for (id, n) in ast.iter() {
            match n {
                Node::Definition { span, .. } => {
                    def_spans.push(*span);
                    if contains(*span, use_span) {
                        let narrower = def.is_none_or(|(d, _)| span.1 - span.0 < d.1 - d.0);
                        if narrower {
                            def = Some((*span, id));
                        }
                    }
                    note_binder(*span, false);
                }
                Node::Lambda { span, .. } => {
                    lambda_spans.push(*span);
                    if contains(*span, use_span) {
                        blocks_around.push(*span);
                    }
                    note_binder(*span, true);
                }
                Node::Loop { span, .. } if contains(*span, use_span) => loop_spans.push(*span),
                Node::Call { receiver, method, block_body, .. } if !block_body.is_empty() => {
                    // A call's own span covers its receiver and arguments too, so
                    // the binder region is the BLOCK BODY's extent.
                    let lo = block_body.iter().map(|&b| ast.get(b).span().0).min();
                    let hi = block_body.iter().map(|&b| ast.get(b).span().1).max();
                    if let (Some(lo), Some(hi)) = (lo, hi) {
                        if contains((lo, hi), use_span) {
                            blocks_around.push((lo, hi));
                        }
                        note_binder((lo, hi), proc_like_block(ast, *receiver, method));
                    }
                }
                _ => {}
            }
        }
        // Issue #146: a read inside a literal block / `->` no scope-recording
        // evaluation enters (`unrecorded_closure` — the reference's
        // `propagate` / `closure_scope` fill) resolves its locals against the
        // ENCLOSING scope as of the closure's position: the closure's own
        // writes never reach it, its bound names read `Dynamic[top]`, and a
        // captured local sees only what the enclosing scope binds there.
        let operand = unrecorded_closure(ast, use_span);
        if let Some((_, boundary)) = operand {
            // A name bound by ANY closure inside the typed-only boundary — a
            // parameter, `;`-local or body-introduced local — is floored to
            // `Dynamic[top]` by `closure_scope`, whatever its outer writes
            // say: `{ a: [1].each { |n| Float(n) } }` stays silent even when
            // an unrelated block elsewhere wrote the same name (cross-block
            // bleed is how this was reached).
            let floored = ast.iter().any(|(_, n)| {
                let (extent, locals) = match n {
                    Node::Lambda { span, locals, .. } => (*span, locals),
                    Node::Call { block_span: Some(b), block_locals, .. } => (*b, block_locals),
                    _ => return false,
                };
                contains(boundary, extent)
                    && contains(extent, use_span)
                    && locals.iter().any(|l| l == root)
            });
            if floored {
                return (Reach::UNTYPED, None);
            }
        }
        let (region, skip_defs, flow_body, params): (_, _, &[NodeId], &[String]) = match def {
            Some((d, id)) => match ast.get(id) {
                Node::Definition { body, param_names, .. } => (d, false, body, param_names),
                _ => return (Reach::UNKNOWN, None),
            },
            None => {
                // The whole file is the region when the read's innermost
                // binder is proc-like — or when the read sits inside a
                // closure the recorder never enters: `propagate` /
                // `closure_scope` resolves every block-local name against the
                // ENCLOSING scope there, whatever the block's yield contract
                // is (`{ a: xs.each { |n| Float(n) } }` is reference-silent
                // where the same `each` at statement level fires `for 1.0`).
                // A bare top-level read has no binder at all and resolves the
                // same way — the propagate'd scope binds every top-level
                // write the flow-approximate way a `def` region binds its
                // own, with the same `Dynamic[top]` for an unwritten local.
                let program_region = match binder {
                    Some((_, true)) | None => true,
                    Some((_, false)) => operand.is_some(),
                };
                if !program_region {
                    return (Reach::OPAQUE, None);
                }
                match ast.get(ast.root()) {
                    Node::Program { body, span } => (*span, true, body, &[]),
                    _ => return (Reach::UNKNOWN, None),
                }
            }
        };
        // A `when` clause's conditions and an `in` clause's pattern are
        // never scope-evaluated: `eval_when_or_in` sub-evals
        // `node.statements` alone while `Narrowing.case_when_scopes` /
        // `apply_in_pattern_bindings` shape-read the rest, so a write
        // there binds nothing on the reference — `case v when (q = 1;
        // Integer) then Float(q).w` is silent (rigor-rs#341). A wrapper
        // that FLATTENS the `case` into a `Statements{Recovered}` carrier
        // (a rescue modifier — `x = (case v when (q = 1; Integer) then 1
        // end) rescue nil`) leaves no `Node::When` to span, but the same
        // positions are marked `Recovered::blocked`, so those extents are
        // excluded identically (rigor-rs#357).
        let case_clauses = unevaluated_case_clause_spans(ast);
        let in_case_clause = |s: rigor_parse::Span| case_clauses.iter().any(|&c| contains(c, s));
        // rigor-rs#368 — provably-dead positions: a folded `if`/`unless` arm's
        // writes reach nothing at all, and a `rescue` arm's write reaches a
        // read inside its own arm or `ensure`, plus the post-`begin` scope —
        // only when the arm falls through (`live_rescues`,
        // `branch_terminates?`). The position rule is `dead.write_reaches`.
        let dead = self.dead_positions(ast, seen);
        let in_region = |s: rigor_parse::Span| {
            if !contains(region, s) || in_case_clause(s) || ast.in_blocked_carrier(s) {
                return false;
            }
            if lambda_spans.iter().any(|&l| contains(l, s)) {
                return false;
            }
            !(skip_defs && def_spans.iter().any(|&d| contains(d, s)))
        };
        // Only the blocks INSIDE the region can hold a block parameter, and only
        // they (or a loop) can carry a later write back round to the read.
        blocks_around.retain(|&b| contains(region, b));
        loop_spans.retain(|&l| contains(region, l));
        // A never-entered closure's body inherits a STATIC snapshot of the
        // enclosing scope — the deferred-run model does not apply to it, so
        // only a real loop can carry a write positioned after the closure
        // back round to the read.
        let loopy = if operand.is_some() {
            !loop_spans.is_empty()
        } else {
            !blocks_around.is_empty() || !loop_spans.is_empty()
        };
        // A guard is a narrowing, not a binding, so the `->` skip does not apply
        // to it — only the region does. A guard inside a `when` condition or an
        // `in` pattern/guard is excluded for the same reason a write is:
        // `case_when_scopes` narrows the SUBJECT local only, so `q.is_a?(C)`
        // there types no `q` (rigor-rs#341) — and so is a guard recovered
        // under a blocked wrapper position (rigor-rs#357).
        let guards_here = |s: rigor_parse::Span| {
            contains(region, s) && !in_case_clause(s) && !ast.in_blocked_carrier(s)
        };
        let reads_root = |i: NodeId| {
            matches!(ast.get(i), Node::LocalVariableRead { name, .. } if name == root)
        };
        let mut writes: Vec<(rigor_parse::Span, LocalWrite)> = Vec::new();
        for (_, n) in ast.iter() {
            // A write under `defined?` / `END` / `BEGIN` never runs in sequence
            // on the reference, so it contributes no value (rigor-rs#153).
            if ast.in_inert_carrier(n.span()) {
                continue;
            }
            match n {
                Node::LocalVariableWrite { name, value, span, .. }
                    if name == root
                        && in_region(*span)
                        && dead.write_reaches(*span, use_span)
                        && !closure_bound_elsewhere(ast, *span, root, use_span) =>
                {
                    writes.push((*span, LocalWrite::Plain(*value)));
                }
                // A `for` index binds the element type, which this analysis
                // cannot see into: decline (rigor-rs#151).
                Node::Loop { index, .. }
                    if index.iter().any(|(n, s)| {
                        n == root && in_region(*s) && dead.write_reaches(*s, use_span)
                    }) =>
                {
                    return (Reach::UNKNOWN, None);
                }
                Node::LocalVariableOpWrite { name, value, span }
                    if name == root
                        && in_region(*span)
                        && dead.write_reaches(*span, use_span)
                        && !closure_bound_elsewhere(ast, *span, root, use_span) =>
                {
                    writes.push((*span, LocalWrite::Op(*value)));
                }
                Node::MultiWrite { targets, value, span, .. }
                    if in_region(*span)
                        && dead.write_reaches(*span, use_span)
                        && !closure_bound_elsewhere(ast, *span, root, use_span)
                        && targets.bound_names().iter().any(|(n, _)| n == root) =>
                {
                    writes.push((*span, LocalWrite::Multi(*value)));
                }
                Node::BeginRescue { clauses, span, .. }
                    if in_region(*span)
                        && clauses.iter().any(|c| {
                            c.bound_name.as_deref() == Some(root)
                                && dead.write_reaches(c.span, use_span)
                        }) =>
                {
                    return (Reach::OPAQUE, None);
                }
                Node::Call { receiver, method, args, span, .. }
                    if skip_class_guards
                        && guards_here(*span)
                        && matches!(method.as_str(), "is_a?" | "kind_of?" | "instance_of?")
                        && receiver.is_some_and(reads_root)
                        && matches!(args.as_slice(), [c] if matches!(ast.get(*c), Node::ConstantRead { .. })) => {}
                Node::Call { receiver, method, args, span, .. }
                    if guards_here(*span)
                        && matches!(
                            method.as_str(),
                            "is_a?" | "kind_of?" | "instance_of?" | "==="
                        )
                        && (receiver.is_some_and(reads_root)
                            || args.iter().copied().any(&reads_root)) =>
                {
                    return (Reach::OPAQUE, None);
                }
                Node::Case { predicate, span, .. }
                    if guards_here(*span) && predicate.is_some_and(reads_root) =>
                {
                    return (Reach::OPAQUE, None);
                }
                _ => {}
            }
        }
        let is_target = |n: &Node| match n {
            Node::LocalVariableWrite { name, .. } => name == root,
            Node::MultiWrite { targets, .. } => {
                targets.bound_names().iter().any(|(n, _)| n == root)
            }
            _ => false,
        };
        // At an unrecorded-closure read the scope index's fill sees the
        // closure's own POSITION, not the read's: the definite-assignment cut
        // runs to the closure-bearing statement and no further into its body.
        let kill_span = operand.map(|(b, _)| ast.get(b).span()).unwrap_or(use_span);
        let kill =
            self.latest_definite_assignment(ast, flow_body, kill_span, &is_target, &dead, seen);
        // The pin runs beside the reach join: `None` until a value
        // contributes, `Some(Some(s))` while every contributor folds to the
        // same scalar, `Some(None)` once one does not (`pin_join`).
        let mut pin: Option<Option<Scalar>> = None;
        let mut reach = match kill {
            Some(_) => Reach::NONE,
            None => {
                // Inside a never-entered closure a write has to sit BEFORE
                // the closure (or loop back round it) to reach the read — a
                // write outside the block but positioned after it cannot.
                let outer_write = writes.iter().any(|(w, _)| {
                    !blocks_around.iter().any(|&b| contains(b, *w))
                        && (operand.is_none() || loopy || w.1 <= kill_span.0)
                });
                if params.iter().any(|p| p == root) || !outer_write {
                    pin = pin_join(pin, None);
                    Reach::UNTYPED
                } else {
                    pin = pin_join(pin, Some(Scalar::Nil));
                    Reach::LITERAL // an unassigned local reads `nil`
                }
            }
        };
        for (span, write) in &writes {
            if kill.is_some_and(|k| span.0 < k.0) {
                continue;
            }
            if !loopy && (span.1 > use_span.0) {
                continue; // at or after the read, and nothing loops back
            }
            reach = reach.join(match *write {
                LocalWrite::Plain(v) => {
                    pin = pin_join(pin, self.expr_scalar(ast, v, seen));
                    self.expr_reach(ast, v, seen)
                }
                LocalWrite::Op(v) => {
                    pin = pin_join(pin, None);
                    let r = self.expr_reach(ast, v, seen);
                    Reach {
                        untyped: r.untyped,
                        precise: true,
                        opaque: true,
                        multi: r.multi,
                        pinned: false,
                    }
                }
                LocalWrite::Multi(v) => {
                    pin = pin_join(pin, None);
                    match ast.get(v) {
                        Node::ArrayLit { elements, .. } => {
                            let mut r = Reach::LITERAL;
                            for &e in elements {
                                r = r.join(self.expr_reach(ast, e, seen));
                            }
                            r
                        }
                        _ => Reach::UNKNOWN,
                    }
                }
            });
        }
        (reach, pin.flatten())
    }

    /// The INSTANCE-VARIABLE arm — the port of the reference's class-ivar
    /// pre-pass (`scope_indexer.rb`'s `build_class_ivar_index`).
    ///
    /// The reference seeds a method body's ivars from a per-CLASS table built
    /// from `@x = …` writes inside the class's `def` bodies, unioned
    /// flow-insensitively. Measured at the pins:
    ///
    /// * a class with NO write for the name has no entry at all, so the read is
    ///   `Dynamic[Top]` — rows r2/r4/i7, and the gitlab-foss
    ///   `pull_policy.rb:28` (`Array(@config).presence`) site. A CLASS-BODY
    ///   `@x = "s"` is not an instance-ivar write and contributes nothing (row
    ///   i1); neither does a write in a NESTED class (row i9), an `@x ||= …`
    ///   (row i2 — the collector only recognises a plain
    ///   `InstanceVariableWriteNode`, exactly as this arena does), nor a write
    ///   in a `def` when the read is in another class entirely.
    /// * otherwise the entry is the UNION of the writes, and since #1021 one
    ///   untyped write is enough to make it imprecise: an untyped ctor write
    ///   beside a typed one (rows r5/z6/z10), and an untyped write in a non-ctor
    ///   method — whose `contribute_read_before_write_nil!` nil only adds a
    ///   precise member (rows z7/i12) — all decline now. Only an entry whose
    ///   every write is precise keeps firing (rows r3/n2).
    /// * inside the reading `def` itself the reference is flow-sensitive: a
    ///   DEFINITE `@x = …` on the read's statement path replaces the seeded
    ///   entry (`@x = "s"; Float(@x)` fires however the class writes it
    ///   elsewhere, rows i6/v08).
    ///
    /// The read-before-write nil is deliberately NOT added as a precise member:
    /// it only matters to `rand`'s join, where a member the reference does not
    /// actually contribute would turn a decline into a false positive.
    ///
    /// No guard scan: the reference does not class-narrow an ivar at all here —
    /// `return unless @x.is_a?(String)` then `@x.typo` is silent on BOTH the
    /// bare read and every fold (rows q1/q2/q4/q5, i13, z9, n5), where the same
    /// guard on a def LOCAL fires `for String` (row q3).
    ///
    /// A `MultiWrite` anywhere in the region with a non-local target refuses the
    /// whole test: `@a, @b = "s", 1` IS collected by the reference
    /// (`record_multi_write_ivars`, row i5 fires) and the arena's
    /// `MultiTarget::Ignored` carries no name to match.
    fn ivar_reach(
        &self,
        ast: &LoweredAst,
        root: &str,
        use_span: rigor_parse::Span,
        seen: &mut Vec<String>,
    ) -> Reach {
        let contains = |s: rigor_parse::Span, i: rigor_parse::Span| s.0 <= i.0 && i.1 <= s.1;
        let scope = self.class_ivar_scope(ast, use_span);
        let use_in_def = scope.def_of(use_span).is_some();
        // rigor-rs#368 — same dead-position rule as `local_reach`, applied to
        // the class-body / top-level writes (a `def`-interior write is the
        // text-scanned census entry the reference's class-ivar index keeps).
        let dead = self.dead_positions(ast, seen);
        let case_clauses = unevaluated_case_clause_spans(ast);
        let in_case_clause =
            |s: rigor_parse::Span| case_clauses.iter().any(|&c| c.0 <= s.0 && s.1 <= c.1);
        let mut writes: Vec<(rigor_parse::Span, NodeId)> = Vec::new();
        let mut def: Option<(rigor_parse::Span, NodeId)> = None;
        let mut loopy = false;
        for (id, n) in ast.iter() {
            match n {
                Node::MultiWrite { targets, span, .. }
                    if scope.contains(*span) && has_non_local_target(targets) =>
                {
                    return Reach::OPAQUE;
                }
                // A `def`-body write is what the class-ivar table collects —
                // a TEXT scan, so it sees a write inside a `when` condition
                // too (`case v when (@x = 1; Integer) then …` inside a `def`
                // fires on the reference) — while a class-body/top-level
                // write binds only inside that same body, which a
                // never-evaluated condition extent does not reach
                // (rigor-rs#341), and neither does an extent recovered
                // under a blocked wrapper position (rigor-rs#357).
                Node::InstanceVariableWrite { name, value, span, .. }
                    if name == root
                        && scope.contains(*span)
                        && (scope.def_of(*span).is_some()
                            || (!use_in_def
                                && !in_case_clause(*span)
                                && !ast.in_blocked_carrier(*span)
                                && dead.write_reaches(*span, use_span))) =>
                {
                    writes.push((*span, *value));
                }
                Node::Definition { span, is_singleton_class: false, .. }
                    if contains(*span, use_span)
                        && scope.contains(*span)
                        && def.is_none_or(|(d, _)| span.1 - span.0 < d.1 - d.0) =>
                {
                    def = Some((*span, id));
                }
                Node::Loop { span, .. } if contains(*span, use_span) => loopy = true,
                Node::Lambda { span, .. } if contains(*span, use_span) => loopy = true,
                Node::Call { block_body, .. } if !block_body.is_empty() => {
                    let lo = block_body.iter().map(|&b| ast.get(b).span().0).min();
                    let hi = block_body.iter().map(|&b| ast.get(b).span().1).max();
                    if let (Some(lo), Some(hi)) = (lo, hi) {
                        loopy |= contains((lo, hi), use_span);
                    }
                }
                _ => {}
            }
        }
        // The reading def's own flow: a definite write on the read's path
        // replaces the seeded entry.
        if let Some((d, id)) = def {
            if let Node::Definition { body, .. } = ast.get(id) {
                let is_target =
                    |n: &Node| matches!(n, Node::InstanceVariableWrite { name, .. } if name == root);
                if let Some(kill) =
                    self.latest_definite_assignment(ast, body, use_span, &is_target, &dead, seen)
                {
                    let mut reach = Reach::NONE;
                    for &(span, value) in &writes {
                        if contains(d, span)
                            && span.0 >= kill.0
                            && (loopy || span.1 <= use_span.0)
                            && dead.write_reaches(span, use_span)
                        {
                            reach = reach.join(self.expr_reach(ast, value, seen));
                        }
                    }
                    return reach;
                }
            }
        }
        if writes.is_empty() {
            return Reach::UNTYPED;
        }
        let mut reach = Reach::NONE;
        for &(_, value) in &writes {
            reach = reach.join(self.expr_reach(ast, value, seen));
        }
        reach
    }

    /// The CLASS-VARIABLE arm. `build_class_cvar_index` collects
    /// `@@x = …` writes from the enclosing class's `def` bodies ONLY — a
    /// class-body `@@n = nil` is walked past and never recorded, so it leaves
    /// the read `Dynamic[Top]` (row r14, and row n7 where a class-body write
    /// sits beside an untyped `def` one). There is no read-before-write nil
    /// contribution for cvars; an entry is the union of its writes, so one
    /// untyped write makes it imprecise (rows n3, c01) and only all-precise
    /// writes keep firing (rows c1, c02).
    fn cvar_reach(
        &self,
        ast: &LoweredAst,
        root: &str,
        use_span: rigor_parse::Span,
        seen: &mut Vec<String>,
    ) -> Reach {
        let scope = self.class_ivar_scope(ast, use_span);
        let use_in_def = scope.def_of(use_span).is_some();
        // rigor-rs#368 — same dead-position rule as `ivar_reach`.
        let dead = self.dead_positions(ast, seen);
        let mut writes: Vec<NodeId> = Vec::new();
        for (_, n) in ast.iter() {
            if let Node::VariableWrite { name, value, span } = n {
                // Same split as `ivar_reach`: a `def`-body `@@x = …` is a
                // text-scanned census entry, while a class-body write under a
                // blocked wrapper position binds nothing (rigor-rs#357).
                if name == root
                    && scope.contains(*span)
                    && (scope.def_of(*span).is_some()
                        || (!use_in_def
                            && !ast.in_blocked_carrier(*span)
                            && dead.write_reaches(*span, use_span)))
                {
                    writes.push(*value);
                }
            }
        }
        self.writes_reach(ast, &writes, seen)
    }

    /// The GLOBAL-VARIABLE arm. `build_program_global_index` is program-wide —
    /// every `$x = …` in the file counts, at top level and inside any `def`
    /// alike — so the region is the whole file and there is no scope gate. A
    /// gvar nothing writes is `Dynamic[Top]` (row g2); `$g = nil` at top level
    /// or `$g = "s"` in a def keeps firing (rows r15/g3), and a gvar with any
    /// untyped write is imprecise (rows n4, g01/g02).
    fn gvar_reach(
        &self,
        ast: &LoweredAst,
        root: &str,
        use_span: rigor_parse::Span,
        seen: &mut Vec<String>,
    ) -> Reach {
        // rigor-rs#368 — program-wide writes still respect dead positions.
        let dead = self.dead_positions(ast, seen);
        let writes: Vec<NodeId> = ast
            .iter()
            .filter_map(|(_, n)| match n {
                Node::VariableWrite { name, value, span, .. }
                    if name == root && dead.write_reaches(*span, use_span) =>
                {
                    Some(*value)
                }
                _ => None,
            })
            .collect();
        self.writes_reach(ast, &writes, seen)
    }

    /// A flow-insensitive table entry: untyped when nothing writes it, else
    /// the join of its writes.
    fn writes_reach(&self, ast: &LoweredAst, writes: &[NodeId], seen: &mut Vec<String>) -> Reach {
        if writes.is_empty() {
            return Reach::UNTYPED;
        }
        let mut reach = Reach::NONE;
        for &v in writes {
            reach = reach.join(self.expr_reach(ast, v, seen));
        }
        reach
    }

    /// The CONSTANT arm: a name NOTHING can resolve reads `Dynamic[Top]` on the
    /// reference too (row r17/k2). The gate is deliberately narrow, because
    /// every resolvable spelling must keep firing:
    ///
    /// * a QUALIFIED path (`Float::INFINITY`, `Errno::ENOENT`) is refused
    ///   outright — the reference resolves class-scoped RBS constants that this
    ///   port has no table for, and both rows fire on both engines (k4/k6);
    /// * a name the bundled RBS or project `sig/` knows as a class or module is
    ///   refused (`Array(String)`, row k3);
    /// * so is a top-level RBS object constant (`ENV`, `ARGV`, `STDOUT` —
    ///   [`CoreIndex::object_constant_class`]) and anything the project writes
    ///   anywhere (rows k1/k5/k8, whose value the port folds precisely and which
    ///   therefore never even reach this predicate).
    ///
    /// [`CoreIndex::object_constant_class`]: rigor_index::CoreIndex::object_constant_class
    fn const_is_reference_untyped(&self, root: &str, use_span: rigor_parse::Span) -> bool {
        if root.is_empty() || root.contains("::") {
            return false;
        }
        if self.source.constant_defined_anywhere(root) || self.source.project_writes_constant(root)
        {
            return false;
        }
        if self.index.object_constant_class(root).is_some() {
            return false;
        }
        let prefix = self.enclosing_prefix(use_span);
        !self.constant_names_a_known_class(&self.resolve_constant_as_written(root, prefix))
    }

    /// The class/module body that owns an ivar or cvar read at `use_span`, as a
    /// span plus the `def` spans inside it — the port of the reference's
    /// "qualified prefix" keying. The innermost enclosing `ClassDef`/`ModuleDef`
    /// wins, and a class NESTED inside it is a barrier (its ivars belong to its
    /// own class, row i9). With no enclosing class the region is the whole file,
    /// which is where a top-level `def`'s own `@x = …` still binds (row i6)
    /// while a top-level class-body write does not reach it (row t2).
    fn class_ivar_scope(&self, ast: &LoweredAst, use_span: rigor_parse::Span) -> IvarScope {
        let contains = |s: rigor_parse::Span, i: rigor_parse::Span| s.0 <= i.0 && i.1 <= s.1;
        let mut region = ast.get(ast.root()).span();
        let mut class_spans: Vec<rigor_parse::Span> = Vec::new();
        for (_, n) in ast.iter() {
            let span = match n {
                Node::ClassDef { span, .. } | Node::ModuleDef { span, .. } => *span,
                _ => continue,
            };
            class_spans.push(span);
            if contains(span, use_span) && span.1 - span.0 < region.1 - region.0 {
                region = span;
            }
        }
        let barriers: Vec<rigor_parse::Span> = class_spans
            .into_iter()
            .filter(|&c| contains(region, c) && c != region)
            .collect();
        let defs: Vec<(rigor_parse::Span, Option<String>)> = ast
            .iter()
            .filter_map(|(_, n)| match n {
                Node::Definition { span, name, .. } if contains(region, *span) => {
                    Some((*span, name.clone()))
                }
                _ => None,
            })
            .collect();
        IvarScope { region, barriers, defs }
    }
}

/// What the REFERENCE's type for a value can hold, as far as the overload
/// selector's `imprecise_arg?` (#1021, `5496acd6`) and the joins behind it
/// care. Computed by [`Typer::arg_reach`]; the flags join by OR.
///
/// `untyped` is the question every #521/#1021 gate asks: is the bare
/// `Dynamic[Top]` carrier the whole type or a member of its union. For
/// `Float`, `Integer` (both arities) and `Array` that alone decides — each has
/// an overload that takes anything (`(untyped, ?exception: bool) -> Float?`,
/// `(untyped, ?untyped, ?exception: bool) -> Integer?`, `[T] (T) -> [T]`) plus
/// an interface overload whose gradual acceptance is lenient, so whatever the
/// union's precise members are, two overloads with different returns survive
/// and the join is `Dynamic[union]` (grid G1, eleven member kinds, all silent).
///
/// `rand` is the exception, and what `precise` / `opaque` are for. Its
/// overloads are `(?0) -> Float`, `(int) -> Integer` and two Range ones, and a
/// union argument must be accepted member-wise: a string, `nil`, a non-zero
/// Integer, a Float, a Symbol, an Array or Hash literal, `true` rules out
/// every overload but `(int)` (whose `_ToInt` arm accepts leniently), so the
/// join is `Integer` and the reference FIRES (grid G1 row R); only a Range or
/// the literal `0` member keeps a second overload alive. So `rand` declines
/// only when the argument is BARE untyped (`!precise`) or a member might be
/// one of those (`opaque` — also any precise value this analysis cannot see
/// into, the conservative side).
///
/// `multi` answers a different question — whether TWO OR MORE distinct precise
/// values may reach, the shape an unconditional-then-conditional rebind leaves
/// (`v = 1; v = 2 if c` — issue #146). It is not one of `imprecise_arg?`'s
/// flags: a `1 | 2` argument is PRECISE, so the strict overload passes run,
/// but the reference's per-member fold over a `Constant` receiver answers the
/// UNION of the folds (`"abc"[v]` -> `"b" | "c"`, `1.fdiv(v)` -> `1.0 | 0.5`)
/// — a carrier no negative rule fires on — never the flat `method_return`
/// class tier 3 would mint. [`Typer::type_call`] reads `multi` to withhold
/// that nominal; the Kernel folds do not consult it (the reference joins
/// THEIR overloads to the conversion class regardless — `Float(v)` fires
/// `for Float`, `rand(v)` `for Integer`, `String(v)` `for String`, all
/// oracle-measured).
///
/// `pinned` is the #332 half of that story: whether every precise value that
/// may reach is one the reference holds as `Type::Constant` — a scalar
/// literal, or a range literal whose endpoints are all static — so a
/// `Constant`-receiver call over it folds to ONE value. Without it the call
/// cannot fold and the overload join stands (`"abc"[i]` with `i = rand.to_i`
/// is `String | nil`, silent; the port's bare `String` was the FP). A single
/// non-literal write — `i = rand.to_i`, `i = x.to_i` on a nominal `x` — is
/// precise-but-unpinned; a `Union[Constant…]` is `pinned` but `multi`, and
/// member-folds to a union either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reach {
    /// A `Dynamic[Top]` value may reach.
    pub(crate) untyped: bool,
    /// A precisely-typed value may reach.
    pub(crate) precise: bool,
    /// A precise value that is not known to be a `rand`-pinning literal may
    /// reach (a Range, the literal `0`, or anything not a literal).
    opaque: bool,
    /// More than one distinct precise value may reach — a member-wise folded
    /// receiver call answers a union, not one value (issue #146).
    pub(crate) multi: bool,
    /// Every precise value that may reach is one the reference would hold as
    /// `Type::Constant` (issue #332).
    pinned: bool,
}

impl Reach {
    /// Nothing reaches (a join's identity, and a revisited recursion node).
    /// `pinned` is `true` as the `&&` identity — a join still reports the
    /// other side's pinning.
    const NONE: Reach =
        Reach { untyped: false, precise: false, opaque: false, multi: false, pinned: true };
    /// Only the untyped carrier reaches.
    const UNTYPED: Reach =
        Reach { untyped: true, precise: false, opaque: false, multi: false, pinned: false };
    /// A precise literal `rand` would pin on.
    const LITERAL: Reach =
        Reach { untyped: false, precise: true, opaque: false, multi: false, pinned: true };
    /// A precise literal the reference does NOT carry as `Type::Constant` —
    /// an interpolated string/symbol, a Tuple literal, a HashShape.
    const PINLESS: Reach =
        Reach { untyped: false, precise: true, opaque: false, multi: false, pinned: false };
    /// A precise value that is `opaque` for `rand` but still a
    /// `Type::Constant` on the reference — the literal `0` and a
    /// fully-static range literal.
    const PINNED_OPAQUE: Reach =
        Reach { untyped: false, precise: true, opaque: true, multi: false, pinned: true };
    /// A precise value of unknown shape.
    const OPAQUE: Reach =
        Reach { untyped: false, precise: true, opaque: true, multi: false, pinned: false };
    /// Anything at all — the decline side of every gate.
    const UNKNOWN: Reach =
        Reach { untyped: true, precise: true, opaque: true, multi: true, pinned: false };

    fn join(self, other: Reach) -> Reach {
        Reach {
            untyped: self.untyped || other.untyped,
            precise: self.precise || other.precise,
            opaque: self.opaque || other.opaque,
            // Two precise sources meeting is exactly what makes the carrier a
            // union of distinct values rather than one pinned value.
            multi: self.multi || other.multi || (self.precise && other.precise),
            // ALTERNATIVES pin together only if every alternative pins; the
            // union a multi one produces still declines via `multi`.
            pinned: self.pinned && other.pinned,
        }
    }

    /// Two operands COMPOSED into one result — a call and its argument, a
    /// range and its endpoint. Unlike [`Reach::join`], a precise value on
    /// each side does NOT create a `multi`: the operands make ONE value, not
    /// two alternatives. The untyped and multi carriers still propagate —
    /// `1 + v` is `2 | 3` when `v` is `1 | 2` — and the composite is pinned
    /// only if both sides are (a call result itself never is; the flag is
    /// for the endpoint-composition case [`Typer::expr_reach`]'s Range arm
    /// checks member-wise).
    fn compose(self, other: Reach) -> Reach {
        Reach {
            untyped: self.untyped || other.untyped,
            precise: self.precise || other.precise,
            opaque: self.opaque || other.opaque,
            multi: self.multi || other.multi,
            pinned: self.pinned && other.pinned,
        }
    }

    /// Whether the reference would hold exactly ONE `Type::Constant` here —
    /// every precise value is a foldable literal and there is only one of
    /// them — the only argument shape whose `Constant`-receiver call folds to
    /// a single reportable value (rigor-rs#332).
    pub(crate) fn pins_one_constant(self) -> bool {
        self.precise && self.pinned && !self.untyped && !self.multi
    }

    /// Whether `rand(arg)` declines: see the type's doc.
    pub(crate) fn declines_rand(self) -> bool {
        self.untyped && (!self.precise || self.opaque)
    }
}

/// One write of a local, as [`Typer::local_reach`] collects them.
#[derive(Debug, Clone, Copy)]
enum LocalWrite {
    /// `x = v`.
    Plain(NodeId),
    /// `x op= v` — keeps the old value on some path, so it never cuts one off.
    Op(NodeId),
    /// `a, x = v` — the right-hand side.
    Multi(NodeId),
}

/// The span of the LAST statement on `use_span`'s statement path that
/// DEFINITELY assigns the variable `is_target` recognises — the flow cut of
/// [`Typer::local_reach`] and [`Typer::ivar_reach`].
///
/// Starting from `body` (a `def` body, or the file), the walk takes the
/// statement containing the read, checks every statement BEFORE it, and
/// descends into the branch, loop body, `begin`/`rescue`/`ensure` section or
/// block body that holds the read, repeating there. A `->` body is not
/// entered (its writes never bind on the reference, rows r11/p13), nor is any
/// other expression shape. A statement definitely assigns when it is the
/// write, or an `if`/`else` or an `else`-bearing `case` every arm of which
/// definitely assigns or ends in `return` (row l05), or a sequence containing
/// such a statement. A `begin` body with a `rescue` does not count — the
/// `rescue` path may skip the write (row l24, reference-silent).
impl<'i> Typer<'i> {
    #[allow(clippy::too_many_arguments)]
    fn latest_definite_assignment(
        &self,
        ast: &LoweredAst,
        body: &[NodeId],
        use_span: rigor_parse::Span,
        is_target: &dyn Fn(&Node) -> bool,
        dead: &DeadPositions,
        seen: &mut Vec<String>,
    ) -> Option<rigor_parse::Span> {
    let contains = |s: rigor_parse::Span, i: rigor_parse::Span| s.0 <= i.0 && i.1 <= s.1;
    let holds = |b: &[NodeId]| b.iter().any(|&s| contains(ast.get(s).span(), use_span));
    // A `when` clause's conditions / an `in` clause's pattern are never
    // scope-evaluated ([`unevaluated_case_clause_spans`]), so for a read
    // inside one the statement path ends at the `case` — descending into
    // the clause's own `Statements` would mint a definite assignment out
    // of a write that never binds (rigor-rs#341).
    let case_clauses = unevaluated_case_clause_spans(ast);
    let in_case_clause = |s: rigor_parse::Span| case_clauses.iter().any(|&c| contains(c, s));
    let mut kill = None;
    let mut body: &[NodeId] = body;
    for _ in 0..64 {
        let Some(pos) = body.iter().position(|&s| contains(ast.get(s).span(), use_span)) else {
            break;
        };
        for &s in &body[..pos] {
            if self.definitely_assigns(ast, s, is_target, &case_clauses, dead, seen) {
                kill = Some(ast.get(s).span());
            }
        }
        // The statement's own sections first; failing that (the read sits in
        // an expression — `x = items.map { |v| t = 1; Float(t) }`, a hash of
        // `lambda {}`s, an `if` predicate's block), the OUTERMOST section of a
        // node nested in it. Skipping the expression layers in between is
        // sound: an expression orders no statements, so only a section can
        // hold a cut. A `->` between the statement and the read stops the
        // walk — its writes never bind (rows r11/p13).
        let stmt = body[pos];
        let next: Option<&[NodeId]> =
            statement_sections(ast, stmt).into_iter().find(|b| holds(b)).or_else(|| {
                let outer = ast.get(stmt).span();
                let mut best: Option<(rigor_parse::Span, &[NodeId])> = None;
                for (id, n) in ast.iter() {
                    let sp = n.span();
                    if id == stmt
                        || !contains(outer, sp)
                        || !contains(sp, use_span)
                        || in_case_clause(sp)
                        || ast.in_blocked_carrier(sp)
                    {
                        continue;
                    }
                    if matches!(n, Node::Lambda { .. }) {
                        return None;
                    }
                    if best.is_some_and(|(b, _)| sp.1 - sp.0 <= b.1 - b.0) {
                        continue;
                    }
                    if let Some(section) = statement_sections(ast, id).into_iter().find(|b| holds(b)) {
                        best = Some((sp, section));
                    }
                }
                best.map(|(_, section)| section)
            });
        match next {
            Some(b) => body = b,
            None => break,
        }
    }
    kill
    }
}

/// The statement lists a node sequences — an `if`'s arms, a `case`'s `when`
/// bodies and `else`, a loop body, a `begin`'s body / `ensure` / `rescue`
/// clauses, a block body, a parenthesised sequence. A `->` body is not one
/// (see [`latest_definite_assignment`]), nor is a nested `def`'s.
fn statement_sections(ast: &LoweredAst, id: NodeId) -> Vec<&[NodeId]> {
    match ast.get(id) {
        Node::If { then_body, else_body, .. } => vec![then_body, else_body],
        Node::Case { branches, else_body, .. } => branches
            .iter()
            .filter_map(|&w| match ast.get(w) {
                Node::When { body, .. } => Some(body.as_slice()),
                _ => None,
            })
            .chain(std::iter::once(else_body.as_slice()))
            .collect(),
        Node::Loop { body, .. }
        | Node::Statements { body, kind: StatementsKind::Sequence, .. } => vec![body],
        Node::BeginRescue { body, ensure_body, clauses, .. } => [body.as_slice(), ensure_body]
            .into_iter()
            .chain(clauses.iter().map(|c| c.body.as_slice()))
            .collect(),
        Node::Call { block_body, .. } => vec![block_body],
        _ => Vec::new(),
    }
}

impl<'i> Typer<'i> {
    /// Whether statement `id` assigns on every path that falls through it —
    /// see [`Typer::latest_definite_assignment`]. A folded-dead position
    /// assigns nothing (rigor-rs#368), and a folded `if`/`unless` asks only
    /// its live arm; the terminating-arm test is the reference's
    /// `branch_unconditionally_exits?` port ([`Typer::stmt_exits`]).
    fn definitely_assigns(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        is_target: &dyn Fn(&Node) -> bool,
        case_clauses: &[rigor_parse::Span],
        dead: &DeadPositions,
        seen: &mut Vec<String>,
    ) -> bool {
    let node = ast.get(id);
    let span = node.span();
    // A statement inside a `when` condition / `in` pattern is never
    // scope-evaluated — it assigns nothing on the reference
    // ([`unevaluated_case_clause_spans`], rigor-rs#341), and neither does
    // one recovered under a blocked wrapper position (a modifier rescue's
    // flattened `case` — rigor-rs#357) or one inside a folded-dead arm
    // (rigor-rs#368).
    if case_clauses
        .iter()
        .any(|&c| c.0 <= span.0 && span.1 <= c.1)
        || ast.in_blocked_carrier(span)
        || dead.covers(span)
    {
        return false;
    }
    if is_target(node) {
        return true;
    }
    match node {
        Node::If { predicate, then_body, else_body, is_unless, .. } => {
            self.definitely_assigns(ast, *predicate, is_target, case_clauses, dead, seen)
                || match self.expr_truthiness(ast, *predicate, seen) {
                    // The fold keeps one arm (`live_branch_for_if`) — only it
                    // can definitely assign.
                    Some(truthy) => self.arm_assigns(
                        ast,
                        if truthy != *is_unless {
                            then_body
                        } else {
                            else_body
                        },
                        is_target,
                        case_clauses,
                        dead,
                        seen,
                    ),
                    None => {
                        self.arm_assigns(ast, then_body, is_target, case_clauses, dead, seen)
                            && self
                                .arm_assigns(ast, else_body, is_target, case_clauses, dead, seen)
                    }
                }
        }
        Node::Case { predicate, branches, else_body, .. } => {
            predicate.is_some_and(|p| {
                self.definitely_assigns(ast, p, is_target, case_clauses, dead, seen)
            }) || (!else_body.is_empty()
                && self.arm_assigns(ast, else_body, is_target, case_clauses, dead, seen)
                && branches.iter().all(|&w| match ast.get(w) {
                    Node::When { body, .. } => {
                        self.arm_assigns(ast, body, is_target, case_clauses, dead, seen)
                    }
                    _ => false,
                }))
        }
        // Only a real sequence: a write in a `rescue` modifier or another
        // recovery carrier may be skipped, and one under `defined?` / `END`
        // never runs in sequence (rigor-rs#153).
        Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
            body.iter()
                .any(|&s| self.definitely_assigns(ast, s, is_target, case_clauses, dead, seen))
        }
        // A clause-less `begin` — which is also the carrier an `if`'s `else`
        // clause lowers to — runs its body to the end.
        Node::BeginRescue { body, clauses, .. } if clauses.is_empty() => {
            body.iter()
                .any(|&s| self.definitely_assigns(ast, s, is_target, case_clauses, dead, seen))
        }
        _ => false,
    }
    }

    /// An arm "holds" a definite assignment when some statement in it assigns
    /// or its tail exits — `latest_definite_assignment`'s `arm` half.
    fn arm_assigns(
        &self,
        ast: &LoweredAst,
        b: &[NodeId],
        is_target: &dyn Fn(&Node) -> bool,
        case_clauses: &[rigor_parse::Span],
        dead: &DeadPositions,
        seen: &mut Vec<String>,
    ) -> bool {
        b.iter()
            .any(|&s| self.definitely_assigns(ast, s, is_target, case_clauses, dead, seen))
            || self.arm_exits(ast, b, seen)
    }
}

/// The BINDING a value expression is rooted at, for
/// [`Typer::arg_reach`]. The five kinds each have their own
/// "would the reference type this `Dynamic[Top]`" rule; nothing else can be
/// answered (a literal, an implicit-self call, `self`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum UntypedRoot {
    /// A bare local read (`u`), including a lambda/proc parameter.
    Local(String),
    /// An instance variable, name WITH the `@` (`"@config"`).
    Ivar(String),
    /// A class variable, name with both `@`s (`"@@count"`).
    Cvar(String),
    /// A global variable, name with the `$` (`"$config"`).
    Gvar(String),
    /// A constant read, as written (`"CONFIG"`, `"Foo::BAR"`).
    Const(String),
}

impl UntypedRoot {
    /// The root's source spelling — the recursion's `seen` key. The five kinds
    /// cannot collide: only an ivar/cvar/gvar carries a sigil, and a constant is
    /// the only capitalised one.
    fn spelling(&self) -> &str {
        match self {
            UntypedRoot::Local(n)
            | UntypedRoot::Ivar(n)
            | UntypedRoot::Cvar(n)
            | UntypedRoot::Gvar(n)
            | UntypedRoot::Const(n) => n,
        }
    }
}

/// The binding a value expression is rooted at, walking down call receivers:
/// `u` for `u`, for `kwargs[:k]` and for `u.foo.bar`; `@config` for
/// `@config.presence`. `None` for any other root (a literal, an implicit-self
/// call, `self`).
///
/// Used by [`Typer::arg_reach`]. Walking receivers is sound for
/// that purpose because a call on an untyped receiver is itself untyped on the
/// reference — which is exactly fixture 60's `Float(kwargs[:upload_duration])`
/// and row z8's `Array(@s8.to_s)`. `depth` bounds the walk so a pathological
/// chain cannot recurse away.
fn untyped_expr_root(ast: &LoweredAst, id: NodeId, depth: u32) -> Option<UntypedRoot> {
    if depth == 0 {
        return None;
    }
    match ast.get(id) {
        Node::LocalVariableRead { name, .. } => Some(UntypedRoot::Local(name.clone())),
        Node::ConstantRead { name, .. } => Some(UntypedRoot::Const(name.clone())),
        Node::VariableRead { name, .. } => {
            if let Some(rest) = name.strip_prefix("@@") {
                (!rest.is_empty()).then(|| UntypedRoot::Cvar(name.clone()))
            } else if let Some(rest) = name.strip_prefix('@') {
                (!rest.is_empty()).then(|| UntypedRoot::Ivar(name.clone()))
            } else if let Some(rest) = name.strip_prefix('$') {
                (!rest.is_empty()).then(|| UntypedRoot::Gvar(name.clone()))
            } else {
                None
            }
        }
        Node::Call { receiver: Some(r), .. } => untyped_expr_root(ast, *r, depth - 1),
        _ => None,
    }
}

/// The span of the innermost literal block or `->` body enclosing `use_span`
/// when no scope-recording evaluation reaches that closure — issue #146's
/// "operand position".
///
/// The reference's scope index (`scope_indexer.rb`) records a type
/// environment per node the statement evaluator ENTERS. A closure sitting in
/// a position the evaluator only TYPES — an element of an array/hash/range
/// literal or interpolation, a call's argument or receiver, a `Constant`'s
/// right-hand side, a multi-assignment or index-write value, a
/// `return`/`next`/`break` value, a `rescue` modifier — is never entered:
/// `propagate` fills the unrecorded body with the PARENT scope, and
/// `closure_scope` floors every name the closure itself binds (parameters,
/// `;`-locals and body-introduced locals) to `Dynamic[top]`. An in-body write
/// therefore never reaches a read in that body — `{ a: lambda { |q| q = 1;
/// Float(q) } }` is reference-silent where the same `lambda` at statement
/// level fires `for 1.0` — and a closure-bound name never reads the precise
/// value the entry scope might suggest.
///
/// `Some((bearing, boundary))` — the innermost unrecorded closure's node and
/// the span of the OUTERMOST ancestor whose edge toward it is typed-only —
/// when the read sits inside such a closure; `None` when the read is inside
/// no closure at all, or when the closure sits on an evaluate-through edge
/// all the way up (a statement, a `def`/branch/loop/`begin`/`case` section,
/// a local/ivar/gvar/`op=` RHS, a `&&`/`||` operand, an evaluated call's own
/// literal block, a `->` body). Every closure inside `boundary` is unentered
/// — nested operand positions (`f(g { … })`, `h = [xs.map { … }]`) mark
/// every ancestor they pass through.
fn unrecorded_closure(
    ast: &LoweredAst,
    use_span: rigor_parse::Span,
) -> Option<(NodeId, rigor_parse::Span)> {
    let contains = |s: rigor_parse::Span, i: rigor_parse::Span| s.0 <= i.0 && i.1 <= s.1;
    let mut inner: Option<(rigor_parse::Span, NodeId)> = None;
    for (id, n) in ast.iter() {
        // The CLOSURE's extent is the literal block node (`{ |q| … }`,
        // parameters included) for a call, the whole `->` node for a lambda —
        // the reference floors params and `;`-locals the same way it does
        // body-introduced locals.
        let extent = match n {
            Node::Lambda { span, .. } => *span,
            Node::Call { block_span: Some(b), .. } => *b,
            _ => continue,
        };
        if contains(extent, use_span)
            && inner.is_none_or(|(c, _)| extent.1 - extent.0 < c.1 - c.0)
        {
            inner = Some((extent, id));
        }
    }
    let (_, bearing) = inner?;
    let bearing_span = ast.get(bearing).span();
    // The closure is scope-recorded iff EVERY ancestor edge preserves
    // evaluation; a single typed-only ancestor leaves the whole subtree
    // unentered (`x = [y = 1, lambda { … }]` threads the write through the
    // operand walker but still records no scope inside the `lambda`). Keep
    // the OUTERMOST such ancestor: it is the boundary inside which every
    // closure is unentered.
    let mut boundary: Option<rigor_parse::Span> = None;
    for (id, n) in ast.iter() {
        let span = n.span();
        if id == bearing || !contains(span, bearing_span) || edge_evaluates(ast, id, bearing_span)
        {
            continue;
        }
        if boundary.is_none_or(|b| span.1 - span.0 > b.1 - b.0) {
            boundary = Some(span);
        }
    }
    boundary.map(|b| (bearing, b))
}

/// Meet one more contributor into a running pin ([`Typer::local_reach`] /
/// [`Typer::expr_scalar`]): `Some(Some(s))` while every contributor folds to
/// the same scalar `s`, `Some(None)` once one fails to or two disagree,
/// `None` before the first contributor.
fn pin_join(acc: Option<Option<Scalar>>, value: Option<Scalar>) -> Option<Option<Scalar>> {
    match (acc, value) {
        (None, value) => Some(value),
        (Some(Some(a)), Some(b)) if a == b => Some(Some(a)),
        (Some(_), _) => Some(None),
    }
}

/// The `Scalar` a literal node carries — the input `folding::fold` wants —
/// or `None` for a non-literal. Used by [`Typer::expr_scalar`] to pin an
/// all-literal chain (`"x".to_i`) to the `Constant` the reference's own fold
/// would give it (rigor-rs#332).
fn literal_scalar(ast: &LoweredAst, id: NodeId) -> Option<Scalar> {
    match ast.get(id) {
        Node::IntegerLit { value: Some(v), .. } => Some(Scalar::Int(*v)),
        Node::IntegerLit { digits: Some(d), .. } => Some(Scalar::BigInt(d.clone())),
        Node::FloatLit { value, .. } => Some(Scalar::Float(*value)),
        Node::StringLit { value, .. } => Some(Scalar::Str(value.clone())),
        Node::SymbolLit { value, .. } => Some(Scalar::Sym(value.clone())),
        Node::TrueLit { .. } => Some(Scalar::Bool(true)),
        Node::FalseLit { .. } => Some(Scalar::Bool(false)),
        Node::NilLit { .. } => Some(Scalar::Nil),
        _ => None,
    }
}

/// The [`Reach`] of a folded literal call — `precise` + `pinned`, with the
/// `rand` opacity of the scalar it landed on (`0` keeps `(?0) -> Float`
/// alive).
fn pinned_scalar_reach(scalar: &Scalar) -> Reach {
    if matches!(scalar, Scalar::Int(0)) {
        Reach::PINNED_OPAQUE
    } else {
        Reach::LITERAL
    }
}

/// The spans a scope-recording evaluation never enters inside a `case`:
/// every `when` clause's CONDITIONS, and every `in` clause's PATTERN —
/// the first body entry of the `BeginRescue` carrier an `in` lowers to,
/// which a sibling [`Node::UnmodeledWrite`] at the clause's own span
/// marks (Prism folds `in P if G`/`unless G` into the pattern as an
/// `IfNode`, so a guard rides the same extent).
///
/// `StatementEvaluator::eval_case_when_branches` sub-evals only a
/// clause's `node.statements`; the conditions/pattern are shape-read by
/// `Narrowing.case_when_scopes` / `apply_in_pattern_bindings` and
/// back-filled by `propagate` — never entered. A write inside one binds
/// nothing on the reference, so `case v when (q = 1; Integer) then
/// Float(q).w` is silent while the span scan that collected the write
/// minted `Float` (rigor-rs#341 — the [`edge_evaluates`] `when`-pattern
/// exclusion's sibling hole). A `case` SUBJECT's own span is not one:
/// the predicate IS sub-evaled.
fn unevaluated_case_clause_spans(ast: &LoweredAst) -> Vec<rigor_parse::Span> {
    let mut unmodeled: Vec<rigor_parse::Span> = Vec::new();
    let mut branches: Vec<NodeId> = Vec::new();
    for (_, n) in ast.iter() {
        match n {
            Node::Case { branches: bs, .. } => branches.extend(bs.iter().copied()),
            Node::UnmodeledWrite { span, .. } => unmodeled.push(*span),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for &b in &branches {
        match ast.get(b) {
            Node::When { conditions, .. } => {
                out.extend(conditions.iter().map(|&c| ast.get(c).span()));
            }
            Node::BeginRescue { body, span, .. } if unmodeled.contains(span) => {
                if let Some(&p) = body.first() {
                    out.push(ast.get(p).span());
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether `name` is bound by a literal block / `->` that contains `span`
/// but NOT `use_span` — a parameter, `;`-local or body-introduced local
/// (Prism's `BlockNode#locals`, which a captured outer local never joins).
/// A write to it rebinds THAT closure's scope only, so it cannot reach a
/// read outside the block: `xs.each { |x| x = 1 }` leaves a top-level `x`
/// untouched, and `x` inside one `lambda`'s body is a different variable
/// from `x` inside a sibling's. The write-collectors of [`Typer::local_reach`]
/// skip such writes, the cross-closure bleed that otherwise minted a precise
/// value for a name the reference reads as `Dynamic[top]`.
fn closure_bound_elsewhere(
    ast: &LoweredAst,
    span: rigor_parse::Span,
    name: &str,
    use_span: rigor_parse::Span,
) -> bool {
    let contains = |s: rigor_parse::Span, i: rigor_parse::Span| s.0 <= i.0 && i.1 <= s.1;
    ast.iter().any(|(_, n)| {
        let (extent, locals) = match n {
            Node::Lambda { span: s, locals, .. } => (*s, locals),
            Node::Call { block_span: Some(b), block_locals, .. } => (*b, block_locals),
            _ => return false,
        };
        contains(extent, span) && !contains(extent, use_span) && locals.iter().any(|l| l == name)
    })
}

/// Whether `child_span`'s subtree is reached by a scope-recording evaluation
/// when `parent` itself is ([`unrecorded_closure`]): the edge classes the
/// reference's statement evaluator threads scopes through — anything else
/// types, defers or discards its operand.
fn edge_evaluates(ast: &LoweredAst, parent: NodeId, child_span: rigor_parse::Span) -> bool {
    let contains = |s: rigor_parse::Span, i: rigor_parse::Span| s.0 <= i.0 && i.1 <= s.1;
    match ast.get(parent) {
        // An evaluated call enters its LITERAL block
        // (`evaluate_block_if_present`); its receiver, arguments and `&expr`
        // block-pass are operands (`thread_operand`).
        Node::Call { block_span, .. } => block_span.is_some_and(|b| contains(b, child_span)),
        // `eval_lambda` sub-evals the `->` body — but not its parameter
        // defaults, which ride the same span boundary; a `def`/class/module
        // evaluates its body (and only its body — never a parameter default).
        // A `when` clause is the same split: `eval_when_or_in` sub-evals only
        // `node.statements` — the CONDITIONS are never entered (the first gets
        // an `on_enter` entry-scope record for `flow.unreachable-clause`, and
        // `Narrowing.case_when_scopes` reads their shape; `propagate` fills
        // the rest). A lambda/proc in condition position is therefore an
        // unentered closure whose own locals floor to `Dynamic[top]` — `case v
        // when lambda { |q| q = 1; Float(q).w }` is reference-silent
        // (rigor-rs#332).
        Node::Lambda { body, .. }
        | Node::Definition { body, .. }
        | Node::ClassDef { body, .. }
        | Node::ModuleDef { body, .. }
        | Node::When { body, .. } => {
            body.iter().any(|&b| contains(ast.get(b).span(), child_span))
        }
        // Only a real statement sequence evaluates its children: the
        // `Recovered` carrier (`rescue` modifier, splat, `super`/`yield`),
        // the `Inert` one (`defined?`, `BEGIN`/`END`, `super`/`yield`
        // arguments) and the `Jump` one (`next e` / `break e` values —
        // `jump_scope` evaluates them but deliberately records no per-node
        // scope) do not.
        Node::Statements { kind, .. } => matches!(kind, StatementsKind::Sequence),
        // Every child edge of these preserves evaluation: a statement's own
        // sections, a predicate, and the value side of the writes that
        // sub-eval their RHS (`x =`, `x op=`, `@x =`, `$x =` — all measured
        // firing `for 1.0` where the ConstantRHS / multi-write / index-write
        // spellings are silent).
        Node::Program { .. }
        | Node::If { .. }
        | Node::Case { .. }
        | Node::Loop { .. }
        | Node::BeginRescue { .. }
        | Node::Logical { .. }
        | Node::LocalVariableWrite { .. }
        | Node::LocalVariableOpWrite { .. }
        | Node::VariableWrite { .. }
        | Node::InstanceVariableWrite { .. } => true,
        // Everything else types, defers or discards the operand instead of
        // recording a scope: `ConstantWrite` (`X = lambda { … }` is silent —
        // `eval_constant_write` uses `type_of`), `MultiWrite` and `IndexWrite`
        // (measured silent), `ArrayLit`/`HashLit`/`Range`/`Interpolated*`
        // (value containers — `eval_value_container` types them), `Return`,
        // `Other`, `Alias`, `UnmodeledWrite` and the read/literal leaves.
        _ => false,
    }
}

/// Whether a block-bearing call is one of the PROC-LIKE spellings whose
/// parameters the reference carries as `Dynamic[Top]` — `lambda { }`,
/// `proc { }` and `Proc.new { }`. Every other block (`each`, `map`, a project
/// method's) has its parameters typed from the RBS yield instead, so its
/// parameter is NOT reference-untyped (rows r9/r10/m11, which fire).
fn proc_like_block(ast: &LoweredAst, receiver: Option<NodeId>, method: &str) -> bool {
    match receiver {
        None => matches!(method, "lambda" | "proc"),
        Some(r) => {
            method == "new"
                && matches!(
                    ast.get(r),
                    Node::ConstantRead { name, .. } if name == "Proc" || name == "::Proc"
                )
        }
    }
}

/// Whether a multi-assignment binds anything the arena cannot name — an ivar,
/// constant, index or attribute target ([`rigor_parse::MultiTarget::Ignored`]).
/// The reference's `record_multi_write_ivars` DOES collect an ivar target
/// (row i5 fires), so an unnameable slot refuses the ivar test outright.
fn has_non_local_target(targets: &rigor_parse::MultiTargets) -> bool {
    fn any_ignored(t: &rigor_parse::MultiTarget) -> bool {
        match t {
            rigor_parse::MultiTarget::Ignored { .. }
            | rigor_parse::MultiTarget::Index { .. } => true,
            rigor_parse::MultiTarget::Local { .. } => false,
            rigor_parse::MultiTarget::Nested(inner) => has_non_local_target(inner),
        }
    }
    targets.lefts.iter().any(any_ignored)
        || targets.rest.as_deref().is_some_and(any_ignored)
        || targets.rights.iter().any(any_ignored)
}

/// The class/module body an ivar or cvar read belongs to, resolved once per
/// test by [`Typer::class_ivar_scope`].
struct IvarScope {
    /// The innermost enclosing `ClassDef`/`ModuleDef` span, or the whole file.
    region: rigor_parse::Span,
    /// Class/module bodies NESTED inside `region` — barriers, because their
    /// ivars belong to their own class (row i9).
    barriers: Vec<rigor_parse::Span>,
    /// Every `def` inside `region`, with its name (`initialize` is the
    /// read-before-write nil exemption).
    defs: Vec<(rigor_parse::Span, Option<String>)>,
}

impl IvarScope {
    /// Whether `span` belongs to this class body rather than a nested one.
    fn contains(&self, span: rigor_parse::Span) -> bool {
        self.region.0 <= span.0
            && span.1 <= self.region.1
            && !self.barriers.iter().any(|b| b.0 <= span.0 && span.1 <= b.1)
    }

    /// The innermost `def` of this class body containing `span`, as
    /// `Some(method name)` — `None` when `span` sits directly in the class body
    /// (or at the top level).
    fn def_of(&self, span: rigor_parse::Span) -> Option<&Option<String>> {
        self.defs
            .iter()
            .filter(|(d, _)| d.0 <= span.0 && span.1 <= d.1)
            .min_by_key(|(d, _)| d.1 - d.0)
            .map(|(_, name)| name)
    }
}
