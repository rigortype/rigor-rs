//! Block calls: typing a call that carries a literal block
//! ([`Typer::type_block_call`] / [`Typer::block_call_result`]) — the
//! exactly-once (`tap` / `then` / `yield_self`) and never-completes (`bot`)
//! analyses, `break` arm types, the block's `self` and entry env, and
//! block-parameter splat tables.

use std::collections::HashSet;

use rigor_parse::{BlockParamKind, JumpKind, LoweredAst, Node, NodeId, StatementsKind};
use rigor_types::{Interner, Scalar, Type, TypeId};

use crate::{shape_key_to_scalar, TypeEnv, Typer};

/// rigor-rs#140 (upstream rigor#1105): the `(owner, method)` catalogue of
/// callees that invoke a literal block EXACTLY ONCE, immediately, and before
/// returning — `Kernel#tap` / `#then` / `#yield_self`. Keyed by owner in the
/// reference (`BlockCallTiming::EXACTLY_ONCE_IMMEDIATE`); here the name list is
/// the cheap pre-gate and [`Typer::exactly_once_kernel_receiver`] proves the
/// resolved declaration is Kernel's.
const EXACTLY_ONCE_BLOCK_CALLS: &[&str] = &["tap", "then", "yield_self"];

/// rigor-rs#140: receiver-less (or `self.`/`Kernel.`-spelled) calls that never
/// return normally — they raise, throw, or end the process
/// (`BlockCallTiming::NON_RETURNING_CALLS`). `loop` is deliberately absent:
/// its declared `bot` notwithstanding, a `StopIteration` ends it normally.
const NON_RETURNING_KERNEL_CALLS: &[&str] = &["raise", "fail", "throw", "exit", "exit!", "abort"];

/// A block-level jump the [`Typer::block_level_jumps`] scan collected: its
/// span, its control-flow kind, and the lowered VALUE expressions of a valued
/// `break e` / `next e` (empty for the argument-less forms).
/// One union member's auto-splat arm — the `arm_of` half of the reference's
/// `BlockAutoSplat` (rigor-rs#140).
enum SplatArm {
    /// `Array[T]` or a Tuple member — fills the fixed positions with its
    /// element type; the second slot is the named `*r` rest element
    /// (`Some` only for `Array[T]`; a Tuple leaves the `Array[Dynamic[top]]`
    /// default, as in the reference's `tuple_table`).
    Elem(TypeId, Option<TypeId>),
    /// An array the table cannot decompose — a raw `Array`, an
    /// `Array[untyped]`/`Array[top]`, or a `Dynamic` over any array — every
    /// slot takes `Dynamic[top]`.
    Opaque,
    /// A `nil` member — fills every slot with `nil`, softened away whenever
    /// a firm member fills the same slot.
    Nilish,
    /// Any other member — fills every slot with `Dynamic[top]` but does not
    /// license the spread on its own.
    Unknown,
}

struct BlockJump {
    span: rigor_parse::Span,
    kind: JumpKind,
    values: Vec<NodeId>,
}

impl<'i> Typer<'i> {
    /// Type a method call that carries a BLOCK (`recv.method { ... }`), modeling
    /// the block-form return like the reference's block-overload selection
    /// (`OverloadSelector` with `block_required: true`, `rbs_dispatch.rb`):
    /// resolve the receiver's concrete class, look up the method's
    /// block-overload return via [`rigor_index::method_return_with_block`], and
    /// intern it as a `Nominal` so a chained call on the result is checkable.
    ///
    /// Declines to `Dynamic[top]` (silent — zero-FP) whenever the receiver isn't
    /// a concrete modeled class, the block form isn't modeled for the method, or
    /// the returned class isn't registered. We never fall back to the no-block
    /// return for a block call (that was the FP the placeholder guarded against).
    // The `explicit_arg_list`/`block_params`/`safe_nav` additions took this
    // past clippy's limit; all are call-site descriptors, so bundling them
    // would be ceremony for a single caller (matches
    // `exactly_once_block_call`'s allow above).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn type_block_call(
        &self,
        ast: &LoweredAst,
        receiver: NodeId,
        method: &str,
        block_body: &[NodeId],
        block_span: Option<rigor_parse::Span>,
        block_params: &[(String, BlockParamKind)],
        explicit_arg_list: bool,
        safe_nav: bool,
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> TypeId {
        // A block-bearing `X.new(...) { ... }` still constructs an `X` instance
        // (e.g. `Array.new(n) { |i| … } : Array`, `Hash.new { … } : Hash`), so it
        // types via the SHARED `.new` path — not the block-overload return below.
        if method == "new" {
            // The block form carries no positional-arg view here; the curated
            // constant-constructor lifts key on pinned positionals, so pass
            // none (a block-bearing `Pathname.new("x") { }` keeps its mint —
            // the lift shapes do not occur with blocks in practice).
            if let Some(ty) = self.type_dot_new(ast, receiver, &[], env, interner) {
                return ty;
            }
        }
        let recv_ty = self.type_of(ast, receiver, env, interner);
        if safe_nav {
            // `recv&.m { … }` — `safe_navigation_call_type`
            // (`expression_typer.rb:1673`): a LITERAL `nil&.m` is the
            // statically-skipped call and folds to nil without dispatching —
            // `nil&.tap { break "s" }` is `nil`, not `"s"`. An INFERRED
            // exactly-nil receiver deliberately does NOT fold (#540/#541 —
            // the nil traces to a wrong uplink), so its `Bot` non-nil
            // fragment keeps the plain pipeline on the unaltered receiver —
            // `c = nil; c&.tap { break "s" }` answers `"s"` in the reference,
            // and the probe that pins it is `x.frobnicate_zzz` firing
            // `for "s"`. Everything else dispatches on the nil-stripped
            // receiver and unions the skipped-call nil back in.
            // `(nil)` unwraps to `NilLit` but is NOT a `NilNode` in the
            // reference's syntax-level reading — `(nil)&.tap` follows the
            // inferred-nil path, not the literal fold (rigor-rs#140).
            if matches!(ast.get(receiver), Node::NilLit { .. })
                && !ast.paren_unwrapped(receiver)
            {
                return interner.intern(Type::Constant(Scalar::Nil));
            }
            let non_nil = self.narrow_non_nil(recv_ty, interner);
            if !matches!(interner.get(non_nil), Type::Bottom | Type::Dynamic(_))
                && non_nil != recv_ty
            {
                let inner = self.block_call_result(
                    ast,
                    method,
                    block_body,
                    block_span,
                    block_params,
                    explicit_arg_list,
                    non_nil,
                    env,
                    interner,
                );
                let nil_ty = interner.intern(Type::Constant(Scalar::Nil));
                return rigor_types::Algebra::join(interner, inner, nil_ty);
            }
        }
        self.block_call_result(
            ast,
            method,
            block_body,
            block_span,
            block_params,
            explicit_arg_list,
            recv_ty,
            env,
            interner,
        )
    }

    /// The block-overload dispatch shared by the plain and `&.` paths of
    /// [`Self::type_block_call`]: the receiver class's `block_required` RBS
    /// return plus the rigor-rs#140 exactly-once/break-arm adjustment.
    #[allow(clippy::too_many_arguments)]
    fn block_call_result(
        &self,
        ast: &LoweredAst,
        method: &str,
        block_body: &[NodeId],
        block_span: Option<rigor_parse::Span>,
        block_params: &[(String, BlockParamKind)],
        explicit_arg_list: bool,
        recv_ty: TypeId,
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> TypeId {
        // The receiver must resolve to a concrete class the index models; a
        // Dynamic / unknown receiver ⇒ silent (never guess the block return).
        let Some(class_name) = self.index.class_name_of(interner, recv_ty) else {
            return interner.untyped();
        };
        // The block-overload return for `class_name#method`. `None` ⇒ the block
        // form isn't precisely modeled ⇒ decline to Dynamic (silent).
        let result = self
            .index
            .method_return_with_block(class_name, method)
            .and_then(|ret_class| self.index.class_id(ret_class))
            .map(|class_id| interner.intern(Type::Nominal { class: class_id, args: vec![] }));

        // rigor-rs#140 (upstream rigor#1105): `Kernel#tap` / `#then` /
        // `#yield_self` run a literal block exactly once, immediately, before
        // returning — so a block that cannot complete normally makes the
        // callee's ordinary return unreachable, and the call types to its
        // `break` arms alone (`bot` when none). The block's `break` values also
        // join the result when it CAN complete (upstream #853), scoped here to
        // the same three candidates.
        if let Some(ty) = self.exactly_once_block_call(
            ast,
            class_name,
            method,
            block_body,
            block_span,
            block_params,
            explicit_arg_list,
            result,
            recv_ty,
            env,
            interner,
        ) {
            return ty;
        }
        result.unwrap_or_else(|| interner.untyped())
    }

    /// The rigor-rs#140 block-timing answer for `receiver.method { … }`, or
    /// `None` to keep the pre-#140 `result`. Two adjustments, both ported from
    /// upstream `ExpressionTyper#call_dispatch_type_for`
    /// (`expression_typer.rb:1763`):
    ///
    /// - **arms union (#853, scoped)**: the `break` values a literal block
    ///   carries out join the call's ordinary result — `x.tap { break "s" if
    ///   c }` is `X | "s"`, which keeps `tap`'s receiver in the answer where
    ///   the block might not break at all.
    /// - **exactly-once drop (#1105)**: when the resolved declaration is
    ///   `Kernel#tap`/`#then`/`#yield_self` AND the block provably never
    ///   completes normally, the ordinary return is unreachable — the call is
    ///   its `break` arms alone, `bot` when there are none (`tap { break "s" }`
    ///   : `"s"`, `tap { raise "x" }` : `bot`).
    ///
    /// Every gate the reference checks is mirrored; anything unproven keeps
    /// `result`, the zero-FP direction.
    #[allow(clippy::too_many_arguments)]
    fn exactly_once_block_call(
        &self,
        ast: &LoweredAst,
        class_name: &str,
        method: &str,
        block_body: &[NodeId],
        block_span: Option<rigor_parse::Span>,
        block_params: &[(String, BlockParamKind)],
        explicit_arg_list: bool,
        result: Option<TypeId>,
        recv_ty: TypeId,
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        // Cheap name-only pre-gate — `BlockCallTiming.candidate_name?`.
        if !EXACTLY_ONCE_BLOCK_CALLS.contains(&method) {
            return None;
        }
        // A literal `BlockNode` — `block_span` is `None` for a `&blk`
        // block-pass, whose `block_body` holds the passed expression, not a
        // body to prove anything about.
        let block_span = block_span?;
        let jumps = self.block_level_jumps(ast, block_span);

        // `exactly_once_block_never_completes?` — the reference ANDs four
        // proofs: candidate name (above), a literal block with a body, no
        // ArgumentsNode (`tap 1` / `tap(1)` declines — none of the three takes
        // an argument; `tap()` has no ArgumentsNode and does NOT decline, just
        // like the reference's `node.arguments` check), the syntactic
        // never-completes walk, the resolved Kernel owner, and a block-return
        // pass that must answer exactly `bot`. That last half is
        // `block_return_type_for(...).is_a?(Type::Bot)` — what the body's
        // evaluation types to, NOT whether it can complete: `tap { [raise
        // "x"] }` never completes yet evaluates to `Array[Integer]`, and
        // `tap { raise "x"; next "s" }` evaluates to `"s"` because a live
        // block-level `next` joins the block's return. [`Self::block_value_bot`]
        // is the port of that evaluation.
        if !explicit_arg_list
            && self.block_never_completes(ast, block_body)
            && self.block_value_bot(
                ast,
                block_body,
                &jumps,
                block_params,
                recv_ty,
                env,
                interner,
            )
            && self.exactly_once_kernel_receiver(class_name, method)
        {
            let arms = self.block_break_arm_types(
                ast,
                block_body,
                &jumps,
                block_params,
                recv_ty,
                env,
                interner,
            );
            return Some(if arms.is_empty() {
                interner.intern(Type::Bottom)
            } else {
                arms.into_iter()
                    .reduce(|a, b| rigor_types::Algebra::join(interner, a, b))
                    .expect("non-empty arms")
            });
        }

        // The block completes normally (or the proof declines): the #853
        // union — the ordinary result beside its `break` arms. When the
        // ordinary result itself is not modelled (`yield_self`'s generic
        // block-typed return), the port keeps its prior `Dynamic` answer
        // rather than minting an arms-only type the reference never computes.
        let result = result?;
        if jumps.iter().any(|j| matches!(j.kind, JumpKind::Break)) {
            let arms = self.block_break_arm_types(
                ast,
                block_body,
                &jumps,
                block_params,
                recv_ty,
                env,
                interner,
            );
            let combined = arms
                .into_iter()
                .fold(result, |acc, arm| rigor_types::Algebra::join(interner, acc, arm));
            Some(combined)
        } else {
            None
        }
    }

    /// `BlockCallTiming.exactly_once_call?` for an instance receiver whose
    /// class resolved to `class_name`: does `class_name#method` dispatch to the
    /// catalogued `Kernel` declaration? Declines (false) on anything unproven —
    /// a toplevel/`Object`/`Kernel`/`BasicObject` def of the name (a private
    /// `Object` method or a monkey-patch shadows Kernel's), a project def on
    /// the class or any RBS ancestor, an incomplete chain, or a resolved owner
    /// that is not `Kernel` (a receiver-side override keeps the old behavior).
    fn exactly_once_kernel_receiver(&self, class_name: &str, method: &str) -> bool {
        // `project_redefines_root?`: `is_toplevel_def` already covers both the
        // toplevel `def` case and defs on the `Object`/`Kernel`/`BasicObject`
        // patchable roots, whose methods the harvest merges into it.
        if self.source.is_toplevel_def(self.file_key, method) {
            return false;
        }
        // `discovered_method_through_ancestors?` + `rbs_ancestor_patched?` —
        // a project reopening of the receiver class or any of its RBS
        // ancestors (`module Enumerable; def tap`) redefines the method
        // without touching RBS. The chain is asked of every ancestor; an
        // incomplete chain declines.
        let Some(ancestors) = self.index.ancestor_names(class_name) else {
            return false;
        };
        if ancestors
            .iter()
            .any(|a| self.source.project_declares_method(self.file_key, a, method))
        {
            return false;
        }
        // `exactly_once_owner?` — the declaration the call resolves to must be
        // Kernel's, not merely share the name.
        self.index.declaring_ancestor(class_name, method) == Some("Kernel")
    }

    /// The `never_completes_normally?` walk of `BlockCallTiming`
    /// (`block_call_timing.rb:147`), ported over the lowered arena: whether
    /// EVERY path through a block-body statement list ends in a block-level
    /// `break`, `return`, `redo`/`retry`, or a non-returning Kernel call. `any`
    /// over the statements is the faithful port — a statement that never
    /// completes makes every later statement unreachable.
    fn block_never_completes(&self, ast: &LoweredAst, body: &[NodeId]) -> bool {
        body.iter().any(|&s| self.stmt_never_completes(ast, s))
    }

    /// One statement of [`Self::block_never_completes`]. Only unconditionally-
    /// evaluated children are descended — a nested block, lambda, `def` or loop
    /// is never entered (its body may not run, and it retargets `break`), a
    /// `&&`/`||` counts only its left operand, a `begin`/`rescue` qualifies only
    /// when its own body and EVERY rescue clause must exit, and an `ensure`
    /// that must exit qualifies on its own — exactly the reference's shape.
    fn stmt_never_completes(&self, ast: &LoweredAst, id: NodeId) -> bool {
        match ast.get(id) {
            // A jump statement: `break`/`return`/`redo`/`retry` end the path;
            // `next` completes the block normally (Prism's `NextNode` is not in
            // the reference's accepted set).
            Node::Statements { kind: StatementsKind::Jump(kind), .. }
            | Node::Other { jump: Some(kind), .. } => !matches!(kind, JumpKind::Next),
            Node::Return { .. } => true,
            Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
                self.block_never_completes(ast, body)
            }
            Node::Call { .. } => self.call_never_returns(ast, id),
            Node::If { predicate, then_body, else_body, .. } => {
                // `conditional_never_completes?` — a never-completing predicate
                // settles it; otherwise BOTH arms must exist and never
                // complete. The lowered `else_body` already normalises
                // `elsif` (`[If]`) and `else` (`[BeginRescue]`), so one
                // `block_never_completes` call covers `branch_never_completes?`.
                self.stmt_never_completes(ast, *predicate)
                    || (!then_body.is_empty()
                        && !else_body.is_empty()
                        && self.block_never_completes(ast, then_body)
                        && self.block_never_completes(ast, else_body))
            }
            Node::Logical { left, .. } => self.stmt_never_completes(ast, *left),
            Node::BeginRescue { main_body, ensure_body, clauses, .. } => {
                // `begin_never_completes?` — an `ensure` that must exit proves
                // it alone; else the protected body must exit AND every rescue
                // clause must too. `main_body` excludes the merged `else`
                // statements, which run only on normal completion.
                (!ensure_body.is_empty() && self.block_never_completes(ast, ensure_body))
                    || (self.block_never_completes(ast, main_body)
                        && clauses
                            .iter()
                            .all(|c| self.block_never_completes(ast, &c.body)))
            }
            Node::ArrayLit { elements, .. } => {
                elements.iter().any(|&e| self.stmt_never_completes(ast, e))
            }
            Node::LocalVariableWrite { value, .. }
            | Node::InstanceVariableWrite { value, .. } => {
                self.stmt_never_completes(ast, *value)
            }
            _ => false,
        }
    }

    /// `call_never_returns?` — a call never completes normally when its
    /// receiver or any argument can't, or when it names a NON-RETURNING Kernel
    /// function spelled the way Kernel's own is reached (`raise`, `fail`,
    /// `throw`, `exit`, `exit!`, `abort` — receiver-less, `self.`, `Kernel.` or
    /// `::Kernel.`) and the project redefines the name nowhere. Anything else —
    /// including `loop`, which a `StopIteration` ends normally — proves nothing.
    fn call_never_returns(&self, ast: &LoweredAst, id: NodeId) -> bool {
        let Node::Call { receiver, args, .. } = ast.get(id) else {
            return false;
        };
        if receiver.is_some_and(|r| self.stmt_never_completes(ast, r)) {
            return true;
        }
        if args.iter().any(|&a| self.stmt_never_completes(ast, a)) {
            return true;
        }
        self.call_declares_bot(ast, id)
    }

    /// The callee half of [`Self::call_never_returns`]: does the call ITSELF
    /// name a non-returning Kernel function? Receiver and argument evaluation
    /// are deliberately NOT consulted — `x.push(raise "y")` can never complete
    /// yet its VALUE is `push`'s return (`Array`), which is what
    /// [`Self::expr_value_bot`] needs to know (probed: `tap { [1, 2].push(raise
    /// "x") }` keeps the receiver, i.e. the call's value is non-bot).
    fn call_declares_bot(&self, ast: &LoweredAst, id: NodeId) -> bool {
        let Node::Call { receiver, method, .. } = ast.get(id) else {
            return false;
        };
        if !NON_RETURNING_KERNEL_CALLS.contains(&method.as_str()) {
            return false;
        }
        self.kernel_spelled_receiver(ast, *receiver) && !self.project_defines_anywhere(method)
    }

    /// The block's RETURN value — the `bot` half of the reference's
    /// `exactly_once_block_never_completes?`, which asks
    /// `block_return_type_for(...).is_a?(Type::Bot)` — i.e. what evaluating the
    /// body types to, a different question from [`Self::block_never_completes`]
    /// (control flow). Two things give the block a non-bot value
    /// (`block_body_type_joining_nexts`):
    ///
    /// - the fall-through TAIL — the last statement's evaluated type, per
    ///   [`Self::expr_value_bot`];
    /// - every live block-level `next` arm, which the evaluator's next-sink
    ///   joins into the return: `raise "x"; next "s"; raise "y"` still types
    ///   the block `"s"`, and `next raise "x"` contributes nothing (bot). A
    ///   `next` on a dead branch is never collected — the same
    ///   [`Self::span_on_dead_branch`] gate the `break` arms use.
    ///
    /// `bot` only when the tail is `bot` AND every live `next` carries a `bot`
    /// value — `tap { [raise "x"] }` keeps `Array[Integer]`, `tap { raise "x";
    /// break "s" }` drops to `"s"`, `tap { break "s"; 1 }` keeps
    /// `"s" | Array[Integer]`.
    // Same allow as `type_block_call` — ast/body/jumps/params/receiver/env/
    // interner are each a distinct call-site descriptor; bundling them is
    // ceremony for single-caller helpers.
    #[allow(clippy::too_many_arguments)]
    fn block_value_bot(
        &self,
        ast: &LoweredAst,
        body: &[NodeId],
        jumps: &[BlockJump],
        block_params: &[(String, BlockParamKind)],
        recv_ty: TypeId,
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> bool {
        for jump in jumps {
            if !matches!(jump.kind, JumpKind::Next) {
                continue;
            }
            let benv = self.block_entry_env(
                ast,
                body,
                jump.span.0,
                block_params,
                recv_ty,
                env,
                interner,
            );
            if self.span_on_dead_branch(ast, jump.span, &benv, interner) {
                continue;
            }
            // A bare `next` carries nil and a multi-value `next` a tuple —
            // both non-bot; only a single carried value can be bot.
            let carried_bot = matches!(jump.values.as_slice(), [single] if
                self.expr_value_bot(ast, *single, &benv, interner));
            if !carried_bot {
                return false;
            }
        }
        // The tail reads the block-entry env at its own offset (`y = "a"; case
        // y` folds `y` to its pinned constant).
        let tail_offset = body.last().map(|&t| ast.get(t).span().0).unwrap_or(0);
        let benv = self.block_entry_env(
            ast,
            body,
            tail_offset,
            block_params,
            recv_ty,
            env,
            interner,
        );
        self.stmt_seq_value_bot(ast, body, &benv, interner)
    }

    /// Whether a statement sequence's VALUE — its last statement's evaluated
    /// type — is `bot`. The tail-only view used by [`Self::block_value_bot`]
    /// and the branch joins inside [`Self::expr_value_bot`].
    fn stmt_seq_value_bot(
        &self,
        ast: &LoweredAst,
        body: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> bool {
        body.last()
            .is_some_and(|&tail| self.expr_value_bot(ast, tail, env, interner))
    }

    /// Whether the VALUE `id` evaluates to is `bot` — the evaluator's answer,
    /// not [`Self::stmt_never_completes`]'s control-flow one. The two diverge
    /// exactly where a non-completing PART still leaves the whole expression a
    /// normal type: `[raise "x"]` is `Array[Integer]` (the array's own value),
    /// `x.push(raise "y")` is `push`'s return, `if raise "x"; break "s"; else
    /// 1; end` is `1`, and `break "s" if raise "x"` is `nil` — every one of
    /// them keeps the ordinary `tap` result, per the reference's
    /// `block_return_type_for` pass (probed at e59b7b89).
    ///
    /// The join-modelled constructs follow the evaluator's union of arm
    /// values: `if`/`case` are `bot` only when EVERY arm's value is (a missing
    /// `else` keeps the implicit `nil` arm — `break "s" unless false` keeps
    /// `Array`), `a && b` / `a || b` join their operands (`raise("x") && 1`
    /// and `raise("x") || 1` both keep `Array`), and `begin`/`rescue` joins
    /// the protected tail with every rescue tail — `ensure` never contributes
    /// a value (`begin 1; ensure raise "x"; end` still types the block's tail
    /// `1`; its non-completion is [`Self::stmt_never_completes`]' question).
    ///
    /// A `break`/`redo`/`retry` tail is `bot`; a `next` tail is NOT — the
    /// statement itself falls through to the block's return with `nil`
    /// (`tap { next raise "x" }` keeps `Array`), its carried value reaching
    /// the return through [`Self::block_value_bot`]'s arm join instead.
    fn expr_value_bot(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> bool {
        match ast.get(id) {
            Node::Statements { kind: StatementsKind::Jump(kind), .. }
            | Node::Other { jump: Some(kind), .. } => !matches!(kind, JumpKind::Next),
            Node::Return { .. } => true,
            Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
                self.stmt_seq_value_bot(ast, body, env, interner)
            }
            Node::If { then_body, else_body, .. } => {
                // The evaluator joins arm values without folding the predicate
                // — `if 1; break "s"; else 1; end` still unions `1` in
                // (probed), so a missing or non-bot arm keeps the block.
                !then_body.is_empty()
                    && !else_body.is_empty()
                    && self.stmt_seq_value_bot(ast, then_body, env, interner)
                    && self.stmt_seq_value_bot(ast, else_body, env, interner)
            }
            Node::Case { predicate, branches, else_body, .. } => {
                let all_whens_bot = !branches.is_empty()
                    && branches
                        .iter()
                        .all(|&b| self.when_tail_value_bot(ast, b, env, interner));
                if !all_whens_bot {
                    return false;
                }
                match predicate {
                    Some(p) => {
                        let pty = self.type_of(ast, *p, env, interner);
                        match interner.get(pty) {
                            // A value-pinned subject folds: a `when` whose
                            // condition is the same constant is the definite
                            // match and contributes its value alone
                            // (`case 1; when 1 then break "b"` gives `bot`);
                            // one provably matching NOTHING yields the `else`
                            // — or `nil` when it is absent (`case 1; when 2
                            // then …` keeps the ordinary result, probed).
                            Type::Constant(s) => {
                                let s = s.clone();
                                let matched = branches.iter().copied().find(|&b| {
                                    matches!(ast.get(b), Node::When { conditions, .. } if
                                        conditions.iter().any(|&c| {
                                            let cty = self.type_of(ast, c, env, interner);
                                            matches!(
                                                interner.get(cty),
                                                Type::Constant(cs) if *cs == s
                                            )
                                        }))
                                });
                                match matched {
                                    Some(b) => self.when_tail_value_bot(ast, b, env, interner),
                                    None => {
                                        !else_body.is_empty()
                                            && self.stmt_seq_value_bot(
                                                ast, else_body, env, interner,
                                            )
                                    }
                                }
                            }
                            // An unanswerable subject (Dynamic / Top) lets the
                            // evaluator join only the arm values it can see —
                            // no no-match `nil` (`case <untyped>; when 1 then
                            // break 1; end` drops, probed): `bot` iff every
                            // `when` and the `else` (when present) are.
                            Type::Dynamic(_) | Type::Top | Type::Bottom => {
                                else_body.is_empty()
                                    || self.stmt_seq_value_bot(ast, else_body, env, interner)
                            }
                            // A concrete-but-unpinned subject may match
                            // nothing — the no-match path contributes the
                            // `else` or `nil` (`case <Integer>; when 1 then
                            // break 1; end` unions, probed).
                            _ => {
                                !else_body.is_empty()
                                    && self.stmt_seq_value_bot(ast, else_body, env, interner)
                            }
                        }
                    }
                    None => {
                        !else_body.is_empty()
                            && self.stmt_seq_value_bot(ast, else_body, env, interner)
                    }
                }
            }
            Node::Logical { left, right, .. } => {
                self.expr_value_bot(ast, *left, env, interner)
                    && self.expr_value_bot(ast, *right, env, interner)
            }
            Node::BeginRescue { body, main_body, ensure_body, clauses, .. } => {
                // The evaluator joins the protected tail — or the `else`'s
                // when the body completes — with every rescue tail; `ensure`
                // contributes NO value (`begin 1; ensure raise "x"; end`
                // still types its tail `1`, probed — the non-completion it
                // causes is `stmt_never_completes`' question). `body` is the
                // flat carrier: the `else` statements are those past
                // `main_body` that belong to neither a clause nor `ensure`.
                let clause_ids: HashSet<NodeId> = clauses
                    .iter()
                    .flat_map(|c| c.body.iter().copied())
                    .collect();
                let ensure_ids: HashSet<NodeId> = ensure_body.iter().copied().collect();
                let else_tail = body[main_body.len()..]
                    .iter()
                    .copied()
                    .filter(|id| !clause_ids.contains(id) && !ensure_ids.contains(id))
                    .next_back();
                self.stmt_seq_value_bot(ast, main_body, env, interner)
                    && clauses
                        .iter()
                        .all(|c| self.stmt_seq_value_bot(ast, &c.body, env, interner))
                    && else_tail
                        .is_none_or(|t| self.expr_value_bot(ast, t, env, interner))
            }
            Node::LocalVariableWrite { value, .. }
            | Node::LocalVariableOpWrite { value, .. }
            | Node::InstanceVariableWrite { value, .. }
            | Node::VariableWrite { value, .. }
            | Node::ConstantWrite { value, .. } => {
                self.expr_value_bot(ast, *value, env, interner)
            }
            Node::Call { .. } => self.call_declares_bot(ast, id),
            _ => false,
        }
    }

    /// One `when` clause's contribution to [`Self::expr_value_bot`]'s `case`
    /// join: the clause's value is its last body statement — or, for a
    /// bodiless `when X`, its last condition (the `When` node documents the
    /// same rule for the typer).
    fn when_tail_value_bot(
        &self,
        ast: &LoweredAst,
        branch: NodeId,
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> bool {
        match ast.get(branch) {
            Node::When { conditions, body, .. } => match body.last() {
                Some(&tail) => self.expr_value_bot(ast, tail, env, interner),
                None => conditions
                    .last()
                    .is_some_and(|&c| self.expr_value_bot(ast, c, env, interner)),
            },
            _ => false,
        }
    }

    /// `kernel_spelled_receiver?` — implicit self or the `Kernel` module
    /// itself (`Kernel.` / `::Kernel.`). The lowered `ConstantRead` renders
    /// `::Kernel` bare as `"Kernel"` and leaves a dynamic-base path's leaf
    /// name on `name` too, so the `dynamic_base` flag is what keeps
    /// `x::Kernel.raise` from passing.
    ///
    /// `self.` is deliberately NOT accepted although the reference's syntactic
    /// `kernel_spelled_receiver?` lists `SelfNode`: an explicit-self call to a
    /// private Kernel function (`self.raise`) resolves to nothing in the
    /// reference's own dispatch, so its block-return pass types the call
    /// `Dynamic`, not `bot` — the `bot` half of
    /// `exactly_once_block_never_completes?` declines and the union stays
    /// (probed: `tap { self.raise "x" }` keeps `x : Array` and the reference
    /// FIRES `x.upcase`). Declining the spelling here reproduces that.
    fn kernel_spelled_receiver(&self, ast: &LoweredAst, receiver: Option<NodeId>) -> bool {
        match receiver {
            None => true,
            Some(r) => match ast.get(r) {
                Node::ConstantRead { name, dynamic_base, .. } => {
                    name == "Kernel" && !dynamic_base
                }
                _ => false,
            },
        }
    }

    /// `project_defines_anywhere?` — deliberately coarse, like the reference:
    /// ANY project `def` of the name, on any class or module and either side,
    /// disables the non-returning-call proof.
    fn project_defines_anywhere(&self, method: &str) -> bool {
        self.source.is_toplevel_def(self.file_key, method) || self.source.project_defines_method_name(method)
    }

    /// Every jump node (`Other{jump}` or the `Jump` carrier) whose span sits
    /// inside `block_span` but outside every boundary a `break`/`next` would
    /// retarget onto — a nested literal block, `->`, `def`, class/module body,
    /// or `while`/`until`/`for`/`loop` — and outside the inert carriers
    /// (`defined?` operand, `super`/`yield` args, `BEGIN`/`END` body) whose
    /// contents the reference's own tree-walks skip. This is the port of the
    /// reference's `block_level_jump_nodes` (`JUMP_BOUNDARY_NODES`): the
    /// lowered arena has no parent links, so boundary spans are collected from
    /// the arena instead of pruning a traversal.
    fn block_level_jumps(&self, ast: &LoweredAst, block_span: rigor_parse::Span) -> Vec<BlockJump> {
        let mut boundaries: Vec<rigor_parse::Span> = Vec::new();
        for (_, node) in ast.iter() {
            let span = match node {
                Node::Call { block_span: Some(bs), .. } => *bs,
                Node::Loop { span, .. }
                | Node::Lambda { span, .. }
                | Node::Definition { span, .. }
                | Node::ClassDef { span, .. }
                | Node::ModuleDef { span, .. } => *span,
                _ => continue,
            };
            // Strict containment — a nested boundary sits strictly inside the
            // outer block's span; the outer `block_span` itself is not a
            // boundary of itself.
            if span != block_span && block_span.0 <= span.0 && span.1 <= block_span.1 {
                boundaries.push(span);
            }
        }
        let mut jumps = Vec::new();
        for (_, node) in ast.iter() {
            let (span, kind, values) = match node {
                Node::Other { span, jump: Some(kind) } => (*span, *kind, Vec::new()),
                Node::Statements { span, kind: StatementsKind::Jump(kind), body, .. } => {
                    (*span, *kind, body.clone())
                }
                _ => continue,
            };
            if !(block_span.0 <= span.0 && span.1 <= block_span.1) || span == block_span {
                continue;
            }
            if boundaries
                .iter()
                .any(|b| b.0 <= span.0 && span.1 <= b.1)
            {
                continue;
            }
            if ast.in_inert_carrier(span) {
                continue;
            }
            jumps.push(BlockJump { span, kind, values });
        }
        jumps.sort_by_key(|j| j.span.0);
        jumps
    }

    /// `call_break_arm_types` — the types the block-level `break`s carry out of
    /// the call, in source order. A bare `break` carries `nil`. A `break` on a
    /// branch the analysis proved dead contributes nothing (the reference's
    /// sink never reaches it); the port's syntactic mirror is the literal
    /// `if false` / `unless true` branch check, which is also what keeps the
    /// `break "s" if false` row's `undefined-method` firing instead of going
    /// silent on a phantom union member.
    ///
    /// Each arm is typed in a block-entry env extended with the top-level
    /// `LocalVariableWrite`s that precede it — `y = "s"; break y` contributes
    /// `"s"`, matching the reference's "typed in the scope that actually
    /// reaches it". A write the flat overlay can't see (inside an `if`, a
    /// `begin`, a nested carrier) leaves the read to the outer env — the same
    /// Dynamic a miss yields everywhere else, never a wrong type.
    #[allow(clippy::too_many_arguments)]
    fn block_break_arm_types(
        &self,
        ast: &LoweredAst,
        block_body: &[NodeId],
        jumps: &[BlockJump],
        block_params: &[(String, BlockParamKind)],
        recv_ty: TypeId,
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Vec<TypeId> {
        let mut arms = Vec::new();
        for jump in jumps {
            if !matches!(jump.kind, JumpKind::Break) {
                continue;
            }
            // Block-entry env: outer env + every top-level write before this arm.
            let benv = self.block_entry_env(
                ast,
                block_body,
                jump.span.0,
                block_params,
                recv_ty,
                env,
                interner,
            );
            if self.span_on_dead_branch(ast, jump.span, &benv, interner) {
                continue;
            }
            let arm = match jump.values.as_slice() {
                [] => interner.intern(Type::Constant(Scalar::Nil)),
                [single] => self.type_of(ast, *single, &benv, interner),
                many => {
                    let elems: Vec<TypeId> = many
                        .iter()
                        .map(|&v| self.type_of(ast, v, &benv, interner))
                        .collect();
                    interner.intern(Type::Tuple(elems))
                }
            };
            arms.push(arm);
        }
        arms
    }

    /// `Narrowing.narrow_non_nil` (`narrowing.rb:112`) — the non-nil fragment
    /// of a type: `Constant(nil)` and `Nominal[NilClass]` narrow to `bot`
    /// (which `Algebra::join` drops out of a union), a `Union` distributes,
    /// and every other shape — `Top`, `Dynamic`, `Singleton`, `Tuple`,
    /// `HashShape`, `Bot` — is its own non-nil fragment. Used by the
    /// `safe_navigation_call_type` port in [`Self::type_block_call`].
    fn narrow_non_nil(&self, ty: TypeId, interner: &mut Interner) -> TypeId {
        match interner.get(ty).clone() {
            Type::Constant(Scalar::Nil) => interner.intern(Type::Bottom),
            Type::Nominal { class, .. }
                if self.index.class_name_for_id(class) == Some("NilClass") =>
            {
                interner.intern(Type::Bottom)
            }
            Type::Union(members) => {
                let kept: Vec<TypeId> = members
                    .into_iter()
                    .map(|m| self.narrow_non_nil(m, interner))
                    .collect();
                kept.into_iter()
                    .reduce(|a, b| rigor_types::Algebra::join(interner, a, b))
                    .unwrap_or_else(|| interner.intern(Type::Bottom))
            }
            _ => ty,
        }
    }

    /// The env a statement at `offset` inside the block actually runs under:
    /// the caller's `env` overlaid with the block's own top-level
    /// `LocalVariableWrite`s that precede it — `y = "s"; break y` contributes
    /// `"s"`, matching the reference's "typed in the scope that actually
    /// reaches it". A write the flat overlay can't see (inside an `if`, a
    /// `begin`, a nested carrier) leaves the read to the outer env — the same
    /// Dynamic a miss yields everywhere else, never a wrong type.
    ///
    /// Every block parameter name is REMOVED first: `|v|` redeclares `v`
    /// inside the block, so an outer local by the same name must not leak
    /// into arm typing — `v = "s"; [1].tap { |v| break v }` contributes the
    /// receiver type, not `"s"` (the reference's `BlockParameterBinder` opens
    /// a fresh scope for the parameter list). The params `tap`/`then`/
    /// `yield_self` feed — a `yield self` — are then bound: the first
    /// positional (`|v|`, `|v = 1|`, `it`, `_1`) gets the receiver's
    /// SELF-TYPE ([`Self::block_self_type`] — a nominal of its class, never
    /// the value-pinned carrier) and `*rest` binds `Array`. Destructured `|(v, w)|` names stay unbound:
    /// the reference's `MultiTargetBinder`
    /// DOES project a Tuple receiver element-wise, but every destructure slot
    /// bound from a nominal `Array[T]` — the shape a `tap` receiver actually
    /// reaches the binder as — is reported OPTIMISTIC (issue #1093's
    /// short-array pad), and an optimistic slot declines diagnostics wherever
    /// it flows. Unbound ⇒ `Dynamic[top]` reproduces the observable
    /// diagnostics exactly: probe `[1, 2].tap { |(f, w)| break f }; x.upcase`
    /// is silent in the reference while a concrete `1` binding would fire.
    /// `**kw` binds `Hash` and `&blk` binds `Proc` — the reference's nominal
    /// answers for both. Plain keywords and `|;local|` declarations stay
    /// unbound — the reference leaves them `Dynamic[top]` too (a `|;local|`
    /// is bound nowhere, not even to `nil`).
    ///
    /// ## Auto-splat (`BlockAutoSplat`, upstream #1116/#1093)
    ///
    /// When the parameter list is one CRuby spreads a lone array argument
    /// across (`ParameterShape.splats?` — a required/post positional, or two
    /// optionals, except a bare `|a|`) AND the receiver carries an array
    /// member, the positions bind from [`Self::block_splat_table`] instead:
    /// `[1, 2].tap { |v, w| break v }` reads `v` as the array's element type
    /// (`1 | 2`), not the whole `[1, 2]`. The element type the port binds is
    /// the JOIN of a Tuple's members — the reference reaches the same shape
    /// because its array literal is `Array[1 | 2]`, whose slots all take the
    /// `1 | 2` element. Optimistic-slot bookkeeping does not port: the
    /// observable answer it produces — `x = 1 | 2` declining the union rule
    /// as a same-class join — the port's own union check already makes.
    /// The `self` a `tap` / `then` / `yield_self` block's first positional
    /// binds — the reference's `extract_block_param_types` self slot
    /// (`rbs_dispatch.rb:1617`): `Nominal[class_name]`, upgraded to
    /// `Nominal[class_name, *receiver_args]` when the `SelfSubstitute`
    /// keep-verdict holds — which for these three non-mutating names reduces
    /// to "the receiver's own type args are non-empty and not every one
    /// deep-widens to `Dynamic[top]`". The slot is NEVER the value-pinned
    /// carrier: `receiver_descriptor` projects a `Constant` to its class's
    /// raw nominal (`1.tap { |a| }` reads `a` as `Integer`, `nil` as
    /// `NilClass`, `"ab"` as `String`, `:a` as `Symbol`, `true` as
    /// `TrueClass`, `1.5` as `Float`), a `Tuple` / `HashShape` projects to
    /// `Array` / `Hash` applied to its OWN unions — constants KEPT, so
    /// `[1, 2]` → `Array[1 | 2]` and `{a: 1}` → `Hash[:a, 1]` — and a
    /// `Singleton` receiver stays `Singleton` (`String.tap`). A `Refined` /
    /// `Difference` / `Dynamic` unwraps to its base / static facet, the
    /// substitute's `Dynamic` re-wrap being verdict-only (the built
    /// `self_type` is the plain `nominal_of(class_name, type_args: …)`).
    ///
    /// A `Union` receiver answers the member-wise self type only when EVERY
    /// member's probe agrees — `probe_block_param_types_union`'s all-equal
    /// rule — so `[1] | [2]` or `[1, 2] | nil` produces the empty probe and
    /// the slot defaults to `Dynamic[top]`. Anything the descriptor does not
    /// project declines the same way.
    fn block_self_type(&self, recv_ty: TypeId, interner: &mut Interner) -> TypeId {
        match interner.get(recv_ty).clone() {
            Type::Union(members) => {
                let mut selves = members
                    .iter()
                    .map(|&m| self.block_self_member_type(m, interner));
                let Some(first) = selves.next() else {
                    return interner.untyped();
                };
                if selves.all(|t| t == first) {
                    first
                } else {
                    interner.untyped()
                }
            }
            _ => self.block_self_member_type(recv_ty, interner),
        }
    }

    /// One member of [`Self::block_self_type`] — `receiver_descriptor`'s
    /// `(class_name, kind, receiver_args)` triple plus the `SelfSubstitute`
    /// keep-verdict, reduced for the always-non-mutating
    /// `tap`/`then`/`yield_self` names (none is a `KNOWN_MUTATORS` /
    /// `ARRAY_MUTATORS` / `HASH_MUTATORS` entry, none ends `!`, so
    /// `preserves_type_args?` holds and the verdict is `projected_self`'s).
    fn block_self_member_type(&self, ty: TypeId, interner: &mut Interner) -> TypeId {
        let (class, singleton, receiver_args) = match interner.get(ty).clone() {
            Type::Nominal { class, args } => (class, false, args),
            Type::Singleton(class) => (class, true, Vec::new()),
            Type::Tuple(elems) => {
                let Some(class) = self.index.class_id("Array") else {
                    return interner.untyped();
                };
                // `tuple_type_args` — `[union(*elements)]`, pins kept.
                let args = if elems.is_empty() {
                    Vec::new()
                } else {
                    vec![elems
                        .into_iter()
                        .reduce(|a, b| rigor_types::Algebra::join(interner, a, b))
                        .expect("non-empty tuple")]
                };
                (class, false, args)
            }
            Type::HashShape(members) => {
                let Some(class) = self.index.class_id("Hash") else {
                    return interner.untyped();
                };
                // `hash_shape_type_args` — `[union(constant keys),
                // union(values)]`; an open shape's `untyped` arms have no
                // port analogue (HashShape carries no open mark).
                let args = if members.is_empty() {
                    Vec::new()
                } else {
                    let mut keys: Vec<TypeId> = Vec::with_capacity(members.len());
                    for m in &members {
                        keys.push(match shape_key_to_scalar(&m.key) {
                            Some(s) => interner.intern(Type::Constant(s)),
                            None => interner.untyped(),
                        });
                    }
                    let vals: Vec<TypeId> = members.iter().map(|m| m.value).collect();
                    vec![
                        keys.into_iter()
                            .reduce(|a, b| rigor_types::Algebra::join(interner, a, b))
                            .expect("non-empty keys"),
                        vals.into_iter()
                            .reduce(|a, b| rigor_types::Algebra::join(interner, a, b))
                            .expect("non-empty values"),
                    ]
                };
                (class, false, args)
            }
            Type::Constant(_) | Type::IntegerRange { .. } => {
                // `value.class.name` — the descriptor hands a Constant /
                // bounded-integer receiver its class's RAW nominal.
                let Some(class) = self
                    .index
                    .class_name_of(interner, ty)
                    .and_then(|name| self.index.class_id(name))
                else {
                    return interner.untyped();
                };
                (class, false, Vec::new())
            }
            Type::DataInstance { class, .. } => (class, false, Vec::new()),
            // The descriptor recurses through the wrapper to the base /
            // static facet; `SelfSubstitute`'s matching arms do the same for
            // the verdict, and the built `self_type` is the plain nominal.
            Type::Refined { base, .. } | Type::Difference { base, .. } => {
                return self.block_self_member_type(base, interner);
            }
            Type::Dynamic(facet) => return self.block_self_member_type(facet, interner),
            _ => return interner.untyped(),
        };
        if singleton {
            // `kind == :singleton` → `singleton_of(class_name)`; no
            // `SelfSubstitute` arm matches a `Singleton` receiver.
            return interner.intern(Type::Singleton(class));
        }
        // `SelfSubstitute.for` on a non-mutating name: nil when the args are
        // empty (`return nil if receiver_args.empty?`) and when EVERY arg
        // deep-widens to `Dynamic[top]` (`projected_self`'s bail). The kept
        // args are the receiver's OWN — the substitute's widened copy is
        // verdict-only.
        let keep = receiver_args
            .iter()
            .any(|&a| self.self_substitute_arg_informative(a, interner));
        interner.intern(Type::Nominal {
            class,
            args: if keep { receiver_args } else { Vec::new() },
        })
    }

    /// `!untyped?(deep_widen(arg))` — `projected_self`'s informativeness
    /// test. `deep_widen` produces `Dynamic[top]` only from a `Dynamic` arg
    /// whose facet is — or unions a member that deep-widens to — `Top`;
    /// every other shape maps to a non-Dynamic carrier and so counts as
    /// informative (`Array[top]` keeps its `Top` arg: `widen_value_pinned`
    /// leaves `Top` untouched and `untyped?` reads `Dynamic`, not `Top`).
    fn self_substitute_arg_informative(&self, arg: TypeId, interner: &Interner) -> bool {
        let Type::Dynamic(facet) = interner.get(arg) else {
            return true;
        };
        !Self::deep_widen_is_top(*facet, interner)
    }

    /// `deep_widen(ty) == Top` — `Top` survives `widen_value_pinned`
    /// unchanged and a union absorbs to `Top` when any member widens there;
    /// a `Dynamic` facet re-wraps (`Dynamic[…]` is never bare `Top`).
    fn deep_widen_is_top(ty: TypeId, interner: &Interner) -> bool {
        match interner.get(ty) {
            Type::Top => true,
            Type::Union(members) => members
                .iter()
                .any(|&m| Self::deep_widen_is_top(m, interner)),
            _ => false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn block_entry_env(
        &self,
        ast: &LoweredAst,
        block_body: &[NodeId],
        offset: usize,
        block_params: &[(String, BlockParamKind)],
        recv_ty: TypeId,
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> TypeEnv {
        let mut benv = env.clone();
        // `ParameterShape.splats?` — the parameter-shape counts CRuby's
        // `setup_parameters_complex` spreads on: `required + post > 0`, or
        // more than one optional, minus the ambiguous `|a|` case. `Other`
        // names are non-first destructure members; each contiguous group
        // holds one positional slot (adjacent groups merge harmlessly — an
        // undercount there can never produce the `mandatory == 1` boundary
        // case the gate distinguishes).
        let (mut required, mut optional, mut post, mut rest) = (0usize, 0usize, 0usize, false);
        let mut counted_destructure = false;
        let mut in_other_group = false;
        for (_, kind) in block_params {
            match kind {
                BlockParamKind::SelfArg | BlockParamKind::RequiredArg => {
                    required += 1;
                    in_other_group = false;
                }
                BlockParamKind::DestructuredSelfArg => {
                    if !counted_destructure {
                        required += 1;
                        counted_destructure = true;
                    }
                    in_other_group = true;
                }
                BlockParamKind::SelfOpt | BlockParamKind::OptionalArg => {
                    optional += 1;
                    in_other_group = false;
                }
                BlockParamKind::PostArg => {
                    post += 1;
                    in_other_group = false;
                }
                BlockParamKind::Other => {
                    if !in_other_group {
                        required += 1;
                    }
                    in_other_group = true;
                }
                BlockParamKind::Rest | BlockParamKind::ImplicitRest => {
                    rest = true;
                    in_other_group = false;
                }
                BlockParamKind::Keyword
                | BlockParamKind::KwRest
                | BlockParamKind::Block
                | BlockParamKind::Local => {
                    in_other_group = false;
                }
            }
        }
        let mandatory = required + post;
        let splats = (mandatory > 0 || optional > 1) && !(mandatory == 1 && optional == 0 && !rest);
        // The `yield self` value the three catalogued methods hand the block
        // is the reference's `self_type` — `Nominal[class]` (plus the
        // receiver's own type args when `SelfSubstitute` keeps them), never
        // the value-pinned carrier — and it is also the auto-splat carrier:
        // `BlockAutoSplat.for` reads `expected_param_types[0]`, the projected
        // `Array[1 | 2]`, so a splatted slot keeps its pinned element
        // constants.
        let self_ty = self.block_self_type(recv_ty, interner);
        let splat = if splats {
            self.block_splat_table(self_ty, interner)
        } else {
            None
        };
        for (name, kind) in block_params {
            benv.remove(name);
            let bound = match kind {
                BlockParamKind::SelfArg | BlockParamKind::SelfOpt => {
                    Some(match &splat {
                        // A splatted optional slot takes `Dynamic[top]`; a
                        // required/first-required slot takes the carrier's
                        // element; no splat keeps the whole `self` type.
                        None => self_ty,
                        Some((slot, _)) if matches!(kind, BlockParamKind::SelfArg) => *slot,
                        Some(_) => interner.untyped(),
                    })
                }
                BlockParamKind::RequiredArg | BlockParamKind::PostArg => {
                    splat.map(|(slot, _)| slot)
                }
                BlockParamKind::OptionalArg => splat.map(|_| interner.untyped()),
                // Destructured names hide the outer name but bind no type —
                // see the doc comment above for why the reference's
                // optimistic Array-slot read declines either way.
                BlockParamKind::DestructuredSelfArg
                | BlockParamKind::Other
                | BlockParamKind::Keyword
                | BlockParamKind::Local
                | BlockParamKind::ImplicitRest => None,
                BlockParamKind::Rest => Some(match &splat {
                    Some((_, rest_elem)) => self
                        .index
                        .class_id("Array")
                        .map(|class| {
                            interner.intern(Type::Nominal { class, args: vec![*rest_elem] })
                        })
                        .unwrap_or_else(|| self.nominal_or_untyped("Array", interner)),
                    None => self.nominal_or_untyped("Array", interner),
                }),
                BlockParamKind::KwRest => Some(self.nominal_or_untyped("Hash", interner)),
                BlockParamKind::Block => Some(self.nominal_or_untyped("Proc", interner)),
            };
            if let Some(ty) = bound {
                benv.insert(name.clone(), ty);
            }
        }
        for &id in block_body {
            let Node::LocalVariableWrite { name, value, span, .. } = ast.get(id) else {
                continue;
            };
            if span.1 <= offset {
                let vty = self.type_of(ast, *value, &benv, interner);
                benv.insert(name.clone(), vty);
            }
        }
        benv
    }

    /// `BlockAutoSplat.for` (`block_auto_splat.rb`) over the port's type
    /// shapes: the `(positional, rest-element)` pair a splatted block binds
    /// when the one yielded value decomposes, or `None` when no receiver
    /// member is an array carrier — a lone non-array carrier leaves the
    /// declared binding alone (the first positional takes the whole value).
    ///
    /// Member arms (`arm_of`): a `Tuple` and an `Array[T]` both fill the
    /// fixed positions with their element — for a `Tuple` the port joins the
    /// members, matching the reference where an array literal types
    /// `Array[1 | 2]` (a real per-position `Tuple` carrier is the one case
    /// the join over-approximates, and only ever toward silence). An OPAQUE
    /// carrier — a raw `Array`, an `Array[untyped]`/`Array[top]`, a
    /// `Refined`/`Difference` over either, or a `Dynamic` over any array —
    /// fills the positions with `Dynamic[top]`. A `nil` member contributes
    /// `nil`, which drops out of any position a firm member fills (the
    /// reference's cross-member softening). Every other member fills the
    /// positions with `Dynamic[top]` but does not license the spread on its
    /// own.
    ///
    /// The `rest` element is the member's element type only when EVERY
    /// member supplies one (a sole `Array[T]`); the reference's named-rest
    /// default `Array[Dynamic[top]]` stands otherwise.
    fn block_splat_table(&self, recv_ty: TypeId, interner: &mut Interner) -> Option<(TypeId, TypeId)> {
        let members: Vec<TypeId> = match interner.get(recv_ty).clone() {
            Type::Union(m) => m,
            _ => vec![recv_ty],
        };
        let arms: Vec<SplatArm> = members
            .iter()
            .map(|&m| self.splat_member_arm(m, interner))
            .collect();
        // `arms_of` — the spread needs at least one Tuple / Array / opaque
        // carrier; a union of non-array members leaves the binding alone.
        if !arms
            .iter()
            .any(|a| matches!(a, SplatArm::Elem(..) | SplatArm::Opaque))
        {
            return None;
        }
        let slot = if arms
            .iter()
            .any(|a| matches!(a, SplatArm::Opaque | SplatArm::Unknown))
        {
            interner.untyped()
        } else {
            let firm: Vec<TypeId> = arms
                .iter()
                .filter_map(|a| match a {
                    SplatArm::Elem(t, _) => Some(*t),
                    _ => None,
                })
                .collect();
            firm.into_iter()
                .reduce(|a, b| rigor_types::Algebra::join(interner, a, b))
                .unwrap_or_else(|| interner.intern(Type::Constant(Scalar::Nil)))
        };
        // `join`'s named-rest rule: only when every member supplies one;
        // otherwise the binder's `Array[Dynamic[top]]` default stands.
        let rest_elems: Vec<TypeId> = arms
            .iter()
            .filter_map(|a| match a {
                SplatArm::Elem(_, Some(r)) => Some(*r),
                _ => None,
            })
            .collect();
        let rest_elem = if rest_elems.len() == arms.len() && !arms.is_empty() {
            rest_elems
                .into_iter()
                .reduce(|a, b| rigor_types::Algebra::join(interner, a, b))
                .expect("non-empty")
        } else {
            interner.untyped()
        };
        Some((slot, rest_elem))
    }

    /// `arm_of` — one union member's auto-splat arm.
    fn splat_member_arm(&self, ty: TypeId, interner: &mut Interner) -> SplatArm {
        match interner.get(ty).clone() {
            // A Tuple's port element is the join of its members — the shape
            // the reference's `Array[union]` literal yields on every slot.
            Type::Tuple(elems) => {
                let slot = elems
                    .into_iter()
                    .reduce(|a, b| rigor_types::Algebra::join(interner, a, b))
                    .unwrap_or_else(|| interner.untyped());
                SplatArm::Elem(slot, None)
            }
            Type::Nominal { class, args }
                if self.index.class_name_for_id(class) == Some("Array") =>
            {
                match args.as_slice() {
                    // `array_element_type` declines an untyped / top element —
                    // that carrier is OPAQUE, not `Elem(untyped)`.
                    [t] => match interner.get(*t) {
                        Type::Dynamic(_) | Type::Top => SplatArm::Opaque,
                        _ => SplatArm::Elem(*t, Some(*t)),
                    },
                    _ => SplatArm::Opaque,
                }
            }
            Type::Refined { base, .. } | Type::Difference { base, .. } => {
                self.splat_member_arm(base, interner)
            }
            // `opaque_array_carrier?` — a `Dynamic` over ANY array carrier is
            // opaque; over anything else it is just an unknown member.
            Type::Dynamic(facet) => {
                let members: Vec<TypeId> = match interner.get(facet).clone() {
                    Type::Union(m) => m,
                    _ => vec![facet],
                };
                let array_facet = members.iter().any(|&m| {
                    matches!(
                        self.splat_member_arm(m, interner),
                        SplatArm::Elem(..) | SplatArm::Opaque
                    )
                });
                if array_facet {
                    SplatArm::Opaque
                } else {
                    SplatArm::Unknown
                }
            }
            Type::Constant(Scalar::Nil) => SplatArm::Nilish,
            _ => SplatArm::Unknown,
        }
    }

    /// Whether `span` sits inside an `if`/`unless` branch the analysis proves
    /// unreachable — the syntactic mirror of the reference's sink, which
    /// evaluates `break "s" if false`'s branch never. Two predicate shapes
    /// prove a dead branch:
    ///
    /// - **Literal predicates** — the `flow.unreachable-branch` rule's own
    ///   literal set (`TRUTHY_LITERAL_NODES` / `FALSEY_LITERAL_NODES`):
    ///   `true`, integer, float, string and symbol literals are always truthy
    ///   (a regex literal is too, but it has no owned node here and declines);
    ///   `false` and `nil` are always falsey.
    /// - **Any predicate expression that evaluates to a `Constant` scalar** —
    ///   the arm-collecting pass is the reference's `StatementEvaluator`
    ///   under a break-value sink, NOT the literal-only
    ///   `flow.unreachable-branch` rule: the evaluator types `if P` itself
    ///   and never enters a branch whose `P` folds to a known truthy/falsey
    ///   constant. A bare local read pinned `a = nil` decides `if a`, and so
    ///   does a tuple fold — `v = [1]; break "s" if v.empty?` never collects
    ///   the arm because `[1].empty?` evaluates to `Constant(false)` in both
    ///   engines (probe row: the reference types the call `Array[Integer]`,
    ///   not the `Array[Integer] | "s"` a kept arm would union).
    ///
    /// `unless` swaps the branches: `unless false`'s `then` RUNS.
    fn span_on_dead_branch(
        &self,
        ast: &LoweredAst,
        span: rigor_parse::Span,
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> bool {
        for (_, node) in ast.iter() {
            let Node::If { predicate, then_body, else_body, is_unless, .. } = node else {
                continue;
            };
            let pred_ty = self.type_of(ast, *predicate, env, interner);
            let truthy = matches!(
                ast.get(*predicate),
                Node::TrueLit { .. }
                    | Node::IntegerLit { .. }
                    | Node::FloatLit { .. }
                    | Node::StringLit { .. }
                    | Node::SymbolLit { .. }
            ) || matches!(
                interner.get(pred_ty),
                Type::Constant(
                    Scalar::Int(_) | Scalar::Float(_) | Scalar::Str(_) | Scalar::Sym(_)
                        | Scalar::Bool(true)
                )
            );
            let falsey = matches!(
                ast.get(*predicate),
                Node::FalseLit { .. } | Node::NilLit { .. }
            ) || matches!(
                interner.get(pred_ty),
                Type::Constant(Scalar::Nil | Scalar::Bool(false))
            );
            let (then_dead, else_dead) = match (*is_unless, truthy, falsey) {
                // `if P`: a truthy literal kills `else`, a falsey one kills `then`.
                (false, true, false) => (false, true),
                (false, false, true) => (true, false),
                // `unless P`: the branches swap.
                (true, true, false) => (true, false),
                (true, false, true) => (false, true),
                _ => (false, false),
            };
            if !then_dead && !else_dead {
                continue;
            }
            let in_body = |body: &[NodeId]| {
                body.iter().any(|&s| {
                    let ss = ast.get(s).span();
                    ss.0 <= span.0 && span.1 <= ss.1
                })
            };
            if (then_dead && in_body(then_body)) || (else_dead && in_body(else_body)) {
                return true;
            }
        }
        false
    }
}
