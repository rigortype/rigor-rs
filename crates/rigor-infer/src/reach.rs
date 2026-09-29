//! Argument reach: what the REFERENCE's type for an argument expression can
//! hold ([`Reach`], computed by [`Typer::arg_reach`] and its local / ivar /
//! cvar / gvar / chain arms) — the gate of the #521 / #1021 untyped-argument
//! declines — with the definite-assignment and untyped-root helpers behind it.

use rigor_parse::{LoweredAst, Node, NodeId, StatementsKind};

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
    /// the helper from running at all. Real files carry a handful of these, and
    /// the whole analysis is skipped unless the argument already types
    /// `Dynamic[top]`. Revisit if a sweep file regresses.
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
            Node::If { then_body, else_body, .. } => self
                .body_value_reach(ast, then_body, seen)
                .join(self.body_value_reach(ast, else_body, seen)),
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
            | Node::InterpolatedString { .. }
            | Node::FloatLit { .. }
            | Node::SymbolLit { .. }
            | Node::InterpolatedSymbol { .. }
            | Node::NilLit { .. }
            | Node::TrueLit { .. }
            | Node::FalseLit { .. }
            | Node::ArrayLit { .. }
            | Node::HashLit { .. } => Reach::LITERAL,
            // `rand`'s `(?0) -> Float` overload accepts the literal `0`, and a
            // Range is what its two Range overloads take: either member keeps a
            // second overload in `rand`'s join (see [`Reach`]).
            Node::IntegerLit { value, .. } => {
                if *value == Some(0) {
                    Reach::OPAQUE
                } else {
                    Reach::LITERAL
                }
            }
            Node::Range { .. } => Reach::OPAQUE,
            _ => self.chain_reach(ast, id, seen),
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
    /// expression with no root (an implicit-self call, `self`) is precise.
    fn chain_reach(&self, ast: &LoweredAst, id: NodeId, seen: &mut Vec<String>) -> Reach {
        let Some(root) = untyped_expr_root(ast, id, 8) else { return Reach::OPAQUE };
        let reach = self.root_reach(ast, &root, ast.get(id).span(), seen);
        if matches!(ast.get(id), Node::Call { .. }) {
            Reach { untyped: reach.untyped, precise: reach.precise, opaque: reach.precise }
        } else {
            reach
        }
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
            UntypedRoot::Local(name) => self.local_reach(ast, name, use_span, seen, false),
            UntypedRoot::Ivar(name) => self.ivar_reach(ast, name, use_span, seen),
            UntypedRoot::Cvar(name) => self.cvar_reach(ast, name, use_span, seen),
            UntypedRoot::Gvar(name) => self.gvar_reach(ast, name, seen),
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
    pub(crate) fn local_reach(
        &self,
        ast: &LoweredAst,
        root: &str,
        use_span: rigor_parse::Span,
        seen: &mut Vec<String>,
        skip_class_guards: bool,
    ) -> Reach {
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
        let (region, skip_defs, flow_body, params): (_, _, &[NodeId], &[String]) = match def {
            Some((d, id)) => match ast.get(id) {
                Node::Definition { body, param_names, .. } => (d, false, body, param_names),
                _ => return Reach::UNKNOWN,
            },
            None => match binder {
                Some((_, true)) => match ast.get(ast.root()) {
                    Node::Program { body, span } => (*span, true, body, &[]),
                    _ => return Reach::UNKNOWN,
                },
                _ => return Reach::OPAQUE,
            },
        };
        let in_region = |s: rigor_parse::Span| {
            contains(region, s)
                && !lambda_spans.iter().any(|&l| contains(l, s))
                && !(skip_defs && def_spans.iter().any(|&d| contains(d, s)))
        };
        // Only the blocks INSIDE the region can hold a block parameter, and only
        // they (or a loop) can carry a later write back round to the read.
        blocks_around.retain(|&b| contains(region, b));
        loop_spans.retain(|&l| contains(region, l));
        let loopy = !blocks_around.is_empty() || !loop_spans.is_empty();
        // A guard is a narrowing, not a binding, so the `->` skip does not apply
        // to it — only the region does.
        let guards_here = |s: rigor_parse::Span| contains(region, s);
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
                    if name == root && in_region(*span) =>
                {
                    writes.push((*span, LocalWrite::Plain(*value)));
                }
                // A `for` index binds the element type, which this analysis
                // cannot see into: decline (rigor-rs#151).
                Node::Loop { index, .. }
                    if index.iter().any(|(n, s)| n == root && in_region(*s)) =>
                {
                    return Reach::UNKNOWN;
                }
                Node::LocalVariableOpWrite { name, value, span }
                    if name == root && in_region(*span) =>
                {
                    writes.push((*span, LocalWrite::Op(*value)));
                }
                Node::MultiWrite { targets, value, span, .. }
                    if in_region(*span)
                        && targets.bound_names().iter().any(|(n, _)| n == root) =>
                {
                    writes.push((*span, LocalWrite::Multi(*value)));
                }
                Node::BeginRescue { clauses, span, .. }
                    if in_region(*span)
                        && clauses.iter().any(|c| c.bound_name.as_deref() == Some(root)) =>
                {
                    return Reach::OPAQUE;
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
                    return Reach::OPAQUE;
                }
                Node::Case { predicate, span, .. }
                    if guards_here(*span) && predicate.is_some_and(reads_root) =>
                {
                    return Reach::OPAQUE;
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
        let kill = latest_definite_assignment(ast, flow_body, use_span, &is_target);
        let mut reach = match kill {
            Some(_) => Reach::NONE,
            None => {
                let outer_write = writes
                    .iter()
                    .any(|(w, _)| !blocks_around.iter().any(|&b| contains(b, *w)));
                if params.iter().any(|p| p == root) || !outer_write {
                    Reach::UNTYPED
                } else {
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
                LocalWrite::Plain(v) => self.expr_reach(ast, v, seen),
                LocalWrite::Op(v) => {
                    let r = self.expr_reach(ast, v, seen);
                    Reach { untyped: r.untyped, precise: true, opaque: true }
                }
                LocalWrite::Multi(v) => match ast.get(v) {
                    Node::ArrayLit { elements, .. } => {
                        let mut r = Reach::LITERAL;
                        for &e in elements {
                            r = r.join(self.expr_reach(ast, e, seen));
                        }
                        r
                    }
                    _ => Reach::UNKNOWN,
                },
            });
        }
        reach
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
                Node::InstanceVariableWrite { name, value, span, .. }
                    if name == root && scope.contains(*span) =>
                {
                    // A `def`-body write is what the class-ivar table collects;
                    // a class-body (or top-level) write binds only inside that
                    // same body.
                    if scope.def_of(*span).is_some() || !use_in_def {
                        writes.push((*span, *value));
                    }
                }
                Node::Definition { span, is_singleton_class: false, .. }
                    if contains(*span, use_span) && scope.contains(*span) =>
                {
                    if def.is_none_or(|(d, _)| span.1 - span.0 < d.1 - d.0) {
                        def = Some((*span, id));
                    }
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
                if let Some(kill) = latest_definite_assignment(ast, body, use_span, &is_target) {
                    let mut reach = Reach::NONE;
                    for &(span, value) in &writes {
                        if contains(d, span)
                            && span.0 >= kill.0
                            && (loopy || span.1 <= use_span.0)
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
        let mut writes: Vec<NodeId> = Vec::new();
        for (_, n) in ast.iter() {
            if let Node::VariableWrite { name, value, span } = n {
                if name == root
                    && scope.contains(*span)
                    && (scope.def_of(*span).is_some() || !use_in_def)
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
    fn gvar_reach(&self, ast: &LoweredAst, root: &str, seen: &mut Vec<String>) -> Reach {
        let writes: Vec<NodeId> = ast
            .iter()
            .filter_map(|(_, n)| match n {
                Node::VariableWrite { name, value, .. } if name == root => Some(*value),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reach {
    /// A `Dynamic[Top]` value may reach.
    pub(crate) untyped: bool,
    /// A precisely-typed value may reach.
    pub(crate) precise: bool,
    /// A precise value that is not known to be a `rand`-pinning literal may
    /// reach (a Range, the literal `0`, or anything not a literal).
    opaque: bool,
}

impl Reach {
    /// Nothing reaches (a join's identity, and a revisited recursion node).
    const NONE: Reach = Reach { untyped: false, precise: false, opaque: false };
    /// Only the untyped carrier reaches.
    const UNTYPED: Reach = Reach { untyped: true, precise: false, opaque: false };
    /// A precise literal `rand` would pin on.
    const LITERAL: Reach = Reach { untyped: false, precise: true, opaque: false };
    /// A precise value of unknown shape.
    const OPAQUE: Reach = Reach { untyped: false, precise: true, opaque: true };
    /// Anything at all — the decline side of every gate.
    const UNKNOWN: Reach = Reach { untyped: true, precise: true, opaque: true };

    fn join(self, other: Reach) -> Reach {
        Reach {
            untyped: self.untyped || other.untyped,
            precise: self.precise || other.precise,
            opaque: self.opaque || other.opaque,
        }
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
fn latest_definite_assignment(
    ast: &LoweredAst,
    body: &[NodeId],
    use_span: rigor_parse::Span,
    is_target: &dyn Fn(&Node) -> bool,
) -> Option<rigor_parse::Span> {
    let contains = |s: rigor_parse::Span, i: rigor_parse::Span| s.0 <= i.0 && i.1 <= s.1;
    let holds = |b: &[NodeId]| b.iter().any(|&s| contains(ast.get(s).span(), use_span));
    let mut kill = None;
    let mut body: &[NodeId] = body;
    for _ in 0..64 {
        let Some(pos) = body.iter().position(|&s| contains(ast.get(s).span(), use_span)) else {
            break;
        };
        for &s in &body[..pos] {
            if definitely_assigns(ast, s, is_target) {
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
                    if id == stmt || !contains(outer, sp) || !contains(sp, use_span) {
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

/// Whether statement `id` is a `return`, possibly wrapped in the clause-less
/// carrier an `else` clause lowers to — an `if` arm that never falls through.
fn ends_in_return(ast: &LoweredAst, id: NodeId) -> bool {
    match ast.get(id) {
        Node::Return { .. } => true,
        Node::BeginRescue { body, clauses, .. } if clauses.is_empty() => {
            body.last().is_some_and(|&l| ends_in_return(ast, l))
        }
        Node::Statements { body, .. } => body.last().is_some_and(|&l| ends_in_return(ast, l)),
        _ => false,
    }
}

/// Whether statement `id` assigns on every path that falls through it — see
/// [`latest_definite_assignment`].
fn definitely_assigns(ast: &LoweredAst, id: NodeId, is_target: &dyn Fn(&Node) -> bool) -> bool {
    let node = ast.get(id);
    if is_target(node) {
        return true;
    }
    let arm = |b: &[NodeId]| {
        b.iter().any(|&s| definitely_assigns(ast, s, is_target))
            || b.last().is_some_and(|&l| ends_in_return(ast, l))
    };
    match node {
        Node::If { predicate, then_body, else_body, .. } => {
            definitely_assigns(ast, *predicate, is_target) || (arm(then_body) && arm(else_body))
        }
        Node::Case { predicate, branches, else_body, .. } => {
            predicate.is_some_and(|p| definitely_assigns(ast, p, is_target))
                || (!else_body.is_empty()
                    && arm(else_body)
                    && branches.iter().all(|&w| match ast.get(w) {
                        Node::When { body, .. } => arm(body),
                        _ => false,
                    }))
        }
        // Only a real sequence: a write in a `rescue` modifier or another
        // recovery carrier may be skipped, and one under `defined?` / `END`
        // never runs in sequence (rigor-rs#153).
        Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
            body.iter().any(|&s| definitely_assigns(ast, s, is_target))
        }
        // A clause-less `begin` — which is also the carrier an `if`'s `else`
        // clause lowers to — runs its body to the end.
        Node::BeginRescue { body, clauses, .. } if clauses.is_empty() => {
            body.iter().any(|&s| definitely_assigns(ast, s, is_target))
        }
        _ => false,
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
