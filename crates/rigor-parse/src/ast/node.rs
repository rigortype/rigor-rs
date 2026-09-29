//! The owned node shape (ADR-0012): `Node`, its `span`, and the
//! `StatementsKind` / `JumpKind` / `RescueClause` payloads.

use super::{
    BlockParamKind, HashKey, MethodBody, MultiTargets, NodeId, ParamShape, Span, Visibility,
};

/// One `rescue` clause of a `begin`/`def` rescue chain, in chain order — the
/// per-clause structure `flow.shadowed-rescue-clause` compares. `exceptions` are
/// the lowered exception-designator node ids (`rescue A, B` → two ids; a bare
/// `rescue`/`rescue => e` → empty), referencing the SAME arena ids the flat
/// [`Node::BeginRescue::body`] already holds (no double-lowering). `body` is the
/// clause's own lowered statement ids. `span` is the [`Prism::RescueNode`]
/// location — it starts at the `rescue` keyword, so `span.0` is the anchor the
/// diagnostic points at (the later dead clause's `rescue`) and the 1-based line
/// the message names for an earlier clause.
///
/// Purely additive: the flat `body` is byte-for-byte what it was before this
/// field existed (every existing pass is untouched), and only the reused-carrier
/// `BeginRescue` variants (else/when/in/parens) carry an EMPTY `clauses` list.
#[derive(Clone, Debug)]
pub struct RescueClause {
    pub exceptions: Vec<NodeId>,
    pub body: Vec<NodeId>,
    /// The name of the `=> e` bound exception variable, if present (Prism
    /// `RescueNode#reference`, a `LocalVariableTargetNode`). `None` for a clause
    /// with no `=>` capture. Consumed by `def.ivar-write-mismatch` (increment a):
    /// inside the clause body, a read of this name types to the clause's single
    /// resolvable exception class, so `@x = "s"; rescue C => e; @x = e` flags the
    /// `String → C` drift. Populated only from a real `BeginNode` rescue chain
    /// (empty for the reused carriers).
    pub bound_name: Option<String>,
    pub span: Span,
}

/// One owned node. Mirrors a minimal Prism subset (ADR-0012); every variant
/// carries the byte [`Span`] needed to key a diagnostic (ADR-0030).
#[derive(Clone, Debug)]
pub enum Node {
    /// The compilation unit: an ordered list of top-level statements.
    Program { body: Vec<NodeId>, span: Span },
    /// A sequence of statements (a `begin`/method/program body in Prism), or a
    /// recovery carrier that links the recovered children of a Prism node with
    /// no owned variant. `kind` tells the two apart; see [`StatementsKind`].
    Statements { body: Vec<NodeId>, span: Span, kind: StatementsKind },
    /// `name = <value>`. The written local feeds the inference environment as
    /// statements are walked in order (ADR-0023 tier-0 literal typing).
    ///
    /// `name_span` is the precise span of the *name* token (Prism `name_loc`),
    /// the anchor a `flow.dead-assignment` diagnostic keys on — mirroring the
    /// reference's `Diagnostic.from_name_loc(write_node)` (it anchors on the
    /// declared-name span, not the whole `name = value` location).
    LocalVariableWrite {
        name: String,
        value: NodeId,
        name_span: Span,
        span: Span,
    },
    /// `name OP= <value>` — an operator/and/or local write (`x += 1`, `y ||= 5`,
    /// `z &&= w`). Lowered as a dedicated variant so the dead-assignment walk can
    /// see the target NAME (Prism would otherwise drop these into [`Node::Other`],
    /// losing the name). Per the reference's `reading_assignment?`, an op-write
    /// READS its target (it reads-then-writes), so the dead-assignment walk counts
    /// this `name` as a READ — and it is NOT itself a fireable dead-write candidate
    /// (the reference's collector fires only on plain `LocalVariableWriteNode`).
    /// `value` is lowered for call reachability.
    LocalVariableOpWrite { name: String, value: NodeId, span: Span },
    /// A multiple assignment (`a, b = rhs`, `a, (b, c), *rest = rhs`) — Prism's
    /// `MultiWriteNode`. `targets` is the `lefts`/`rest`/`rights` triple; `value`
    /// is the lowered right-hand side.
    ///
    /// Before this variant existed the node fell through
    /// `collect_recoverable_children` into a `Statements` carrier (or
    /// `Node::Other`) and the LHS names were DROPPED from the arena entirely, so
    /// a multi-write rebind was invisible to `collect_flow_writes` and never
    /// widened an earlier straight-line binding — a live `flow.always-truthy-
    /// condition` false positive.
    ///
    /// The RHS stays a fully-lowered child, so reads/calls inside it remain
    /// visible to the structural walks exactly as under the old carrier.
    /// `flow.dead-assignment` and `static.value-use.void` both key on
    /// `LocalVariableWrite`, so neither sees this variant — matching the
    /// reference, which skips `MultiWriteNode` in both collectors.
    ///
    /// `target_exprs` holds the lowered EXPRESSIONS embedded in non-local
    /// targets — `item` in `item[3], item[5] = info`, the receiver of a
    /// `CallTargetNode`, an index argument. The reference's read-gathering
    /// walks the real Prism tree, so those reads count; dropping them would
    /// falsely make `item = …` a `flow.dead-assignment` candidate (measured on
    /// netrc 0.11.0 `Netrc#[]=`). They live here so the structural walks find
    /// them by span, exactly as under the old recovered-children carrier.
    MultiWrite {
        targets: MultiTargets,
        value: NodeId,
        target_exprs: Vec<NodeId>,
        span: Span,
    },
    /// A read of a previously-written local (`s`).
    LocalVariableRead { name: String, span: Span },
    /// A string literal (`"Hello"`); `value` is the unescaped contents.
    StringLit { value: String, span: Span },
    /// An interpolated string or heredoc (`"a#{x}b"`, `<<~SQL ... #{t} ... SQL`).
    /// Types as a `String` *instance* (a `Nominal { String }`): an interpolated
    /// string literal is always a `String` regardless of the interpolated
    /// values. `parts` carries the lowered interpolation segments so calls
    /// inside `#{ … }` stay reachable for the walk.
    InterpolatedString { parts: Vec<NodeId>, span: Span },
    /// An interpolated symbol (`:"a#{x}b"`). A structural twin of
    /// `InterpolatedString`: it types as a `Nominal { Symbol }` instance
    /// instead of `String`, so a symbol never mis-types as a `String`
    /// (e.g. via value-descent picking the trailing string fragment). `parts`
    /// carries the lowered interpolation segments so calls inside `#{ … }`
    /// stay reachable for the walk, exactly like `InterpolatedString`.
    InterpolatedSymbol { parts: Vec<NodeId>, span: Span },
    /// An integer literal (`42`). `value` is `None` for a literal outside
    /// `i64` (a Bignum); `digits` then carries its signed decimal spelling so
    /// the typer can still pin the VALUE (`Scalar::BigInt`) exactly as the
    /// reference's arbitrary-precision `Constant[…]` does (rigor-rs#194) —
    /// the same witnesses the literal `Constant` gives, without any `i64`
    /// consumer being able to read a wrong value out of `value`.
    IntegerLit {
        value: Option<i64>,
        /// Signed decimal digits — `Some` iff `value` is `None` (a Bignum);
        /// never a lossy or truncated rendering.
        digits: Option<String>,
        span: Span,
    },
    /// A float literal (`3.14`); `value` is the parsed `f64`.
    FloatLit { value: f64, span: Span },
    /// A symbol literal (`:foo`); `value` is the symbol name (no leading colon).
    SymbolLit { value: String, span: Span },
    /// The `nil` literal.
    NilLit { span: Span },
    /// The `true` literal.
    TrueLit { span: Span },
    /// The `false` literal.
    FalseLit { span: Span },
    /// A method call. `receiver` is `None` for an implicit-self call.
    /// `message_span` is the precise span of the *method name* token — the
    /// location a `call.undefined-method` diagnostic keys on (ADR-0002/0030).
    Call {
        receiver: Option<NodeId>,
        method: String,
        /// Positional argument expressions in source order (ADR-0023: needed
        /// for argument-contract rules such as `call.wrong-arity` and for
        /// argument-dependent constant folding). Splat/keyword/forwarding args
        /// lower like any other node and are collected here too — the lowered
        /// subtree does not preserve their shape, so rules that must
        /// distinguish them read `args_plain_positional` / `args_all_plain` /
        /// `first_arg_nonplain` rather than inspecting the children.
        args: Vec<NodeId>,
        /// Statements of an attached block (`foo { ... }` / `do…end`), lowered
        /// so calls inside the block reach the rule walk. Empty for a call with
        /// no block. Not a *value* of the call — purely a reachability handle.
        block_body: Vec<NodeId>,
        /// Span of the attached LITERAL block (`{ … }` / `do … end`) — Prism's
        /// `BlockNode` location, parameters and delimiters included. `None` for
        /// a call with no block AND for a `&expr` block-pass (a
        /// `BlockArgumentNode`), whose expression still rides `block_body`.
        /// Read by `call.unresolved-toplevel`'s receiver-eval carve-out, which
        /// mirrors the reference's `receiver_eval_block_ranges` offset test —
        /// the whole block node, not just its body statements (a heredoc body or
        /// a block-parameter default sits inside the node but outside every
        /// body statement's span).
        block_span: Option<Span>,
        /// The local names the attached literal block BINDS in its own scope —
        /// Prism's `BlockNode#locals`: every parameter form (required,
        /// optional, splat, post, destructured, keyword, keyword-rest, `&`
        /// block), the `;`-declared block-locals, numbered parameters, and
        /// locals first written inside the block body — but NOT a captured
        /// outer local. Empty for a call with no block and for a `&expr`
        /// block-pass. Read by `toplevel_rebinds` (rigor-rs#166): a write to
        /// one of these names is a block-scoped write, not a rebind of the
        /// top-level local of the same name.
        block_locals: Vec<String>,
        /// Span of the method-name token (`lenght`), the diagnostic anchor.
        message_span: Span,
        /// `true` for a safe-navigation call (`x&.foo`), `false` for a plain
        /// dot call (`x.foo`). Prism's `CallNode::is_safe_navigation()` drives
        /// this. Consumed by `call.possible-nil-receiver` (the reference's
        /// safe-nav suppression clause): a `&.` call short-circuits on a nil
        /// receiver at runtime, so a nil-bearing receiver is not a bug there.
        safe_nav: bool,
        /// `true` when the FIRST positional argument is a non-plain shape — a
        /// splat (`*a`), a bare keyword-hash (`a: 1`, Prism's `KeywordHashNode`),
        /// or forwarded arguments (`...`). The lowered arg subtree does not
        /// otherwise preserve this (a `KeywordHashNode` and a braced `HashNode`
        /// both lower to `Node::HashLit`), so `call.raise-non-exception` reads
        /// this flag to bail exactly as the reference's
        /// `first_positional_raise_operand` does (`raise(a: 1)` is silent; a
        /// positional `raise({a: 1})` fires). `false` when there is no first
        /// argument or it is an ordinary expression.
        first_arg_nonplain: bool,
        /// `true` iff every argument in the call's argument list is a plain
        /// positional expression — none is a splat `*a`, a bare keyword-hash
        /// `a: 1`, a `BlockArgumentNode`, or forwarded `...` — a faithful
        /// mirror of the reference's `plain_positional_call?` /
        /// `simple_positional?` (`check_rules.rb:1680`), computed over
        /// `call.arguments()` exactly as the reference reads
        /// `call_node.arguments.arguments`. A `&blk` block-pass does NOT
        /// disqualify: Prism puts it in `block()`, never in `arguments()`, so
        /// the oracle still arity-checks `first(1, 2, &)` (the
        /// `BlockArgumentNode` arm of `simple_positional?` is unreachable at
        /// this pin but is mirrored for faithfulness). An ordinary trailing
        /// block (`foo(a) { }`) does not count either. Consumed by
        /// `call.wrong-arity`, which declines when this is `false`.
        args_plain_positional: bool,
        /// `true` iff `args_plain_positional` AND the call carries no `&blk`
        /// block-pass — the same plain-positional test over all arguments
        /// (unlike `first_arg_nonplain`, which is first-only) plus Prism's
        /// `block()` being absent or a `BlockNode`. An ordinary trailing
        /// block (`foo(a) { }`) does NOT count. Consumed by
        /// `call.argument-type-mismatch`, which bails when this is `false`.
        /// The `!block_is_pass` term is a conservative decline, not oracle
        /// parity: the reference cannot see `&blk` in `arguments()` but still
        /// fires ATM on a block-pass call — measured `[1, 2, 3].fetch("x", &b)`
        /// and anonymous `fetch("x", &)` emit `call.argument-type-mismatch`
        /// on the reference and stay silent here. A safe-side coverage gap;
        /// `center("x", &b)` cannot show it (`center("x")` is silent on both).
        args_all_plain: bool,
        /// `true` iff the call carries a Prism ArgumentsNode — `foo(1)` or
        /// `foo 1`, but NOT `foo()`: Prism leaves `call.arguments` nil for
        /// empty parens (verified at the pin; the `opening_loc` is what
        /// records the `()`). The reference's exactly-once block-timing proof
        /// (rigor#1105 / rigor-rs#140) gates on that same `node.arguments`
        /// check, so `x.tap() { break v }` is treated exactly like
        /// `x.tap { break v }` on both engines, while `x.tap(1) { break v }`
        /// declines.
        explicit_arg_list: bool,
        /// Every local name the attached literal block's parameter list binds,
        /// tagged with how it binds — the port of the reference's
        /// `BlockParameterBinder` name set (rigor-rs#140). Covers required,
        /// optional, rest, post, keyword, keyword-rest and `&blk` parameters,
        /// destructured `|(v, w)|` targets, `|;local|` declarations, and the
        /// implicit `it` / numbered `_1.._9` parameters. Empty for a call with
        /// no literal block and for a `&expr` block-pass (which binds nothing
        /// in the caller). A `break`/`next` arm typed under the block's entry
        /// env must NOT read an outer local through one of these names —
        /// `{ |v| break v }` reads the parameter, not an outer `v`.
        block_params: Vec<(String, BlockParamKind)>,
        /// Span of the whole call expression.
        span: Span,
    },
    /// A definition (`def` / singleton class). Carries its lowered body
    /// statements only — a definition is not a value, so the typer never types
    /// it; the body is lowered purely so nested calls are reachable.
    ///
    /// `name` is the method name for an instance/singleton `def` (`None` for a
    /// singleton-class `class << self` body, which has no single name). It is
    /// retained for ADR-0023 tier-4b in-source RETURN-type inference: the
    /// SourceIndex pairs a class's direct instance method with its body so the
    /// method's return expression can be typed. `has_explicit_return` is `true`
    /// iff a `return` statement appears ANYWHERE in the Prism def body — the
    /// tier-4b gate declines (stores no return entry) whenever it is set, because
    /// we only look at the tail expression and an explicit `return` could carry a
    /// different type (the reference unions both; we conservatively decline).
    Definition {
        name: Option<String>,
        /// `true` when this name-less Definition is a singleton-class body
        /// (`class << X`), NOT a method `def`. A `class << X` is a CLASS scope —
        /// non-toplevel, so `call.unresolved-toplevel` must NOT fire inside it —
        /// whereas a `def self.x` / `def x` body is a method scope (a *toplevel*
        /// `def` body still counts as toplevel and DOES fire). Both are name-less,
        /// so this flag is the only reliable discriminator.
        is_singleton_class: bool,
        /// The method name for a SELF-singleton `def self.x` (`Some("x")`), else
        /// `None`. Kept SEPARATE from `name` (which stays `None` for a
        /// receiver-bearing def so it is never harvested as an instance method):
        /// this lets `sig-gen` collect `def self.x` singletons (their name is
        /// otherwise lost) WITHOUT touching the tier-4b instance-method harvest.
        /// A non-self receiver (`def obj.x`) leaves this `None` (a per-object
        /// singleton, out of scope).
        singleton_name: Option<String>,
        /// The method name for a def with a NON-`self` explicit receiver
        /// (`def IO.console_size` -> `Some("console_size")`), else `None`.
        /// Complementary to `singleton_name` (which covers `def self.x` only),
        /// and kept out of `name` for the same reason it is: this is never an
        /// instance method, so the tier-4b harvest must not see it.
        ///
        /// Read by the toplevel-def registry: the reference records a def whose
        /// LEXICAL PREFIX is empty under its `<toplevel>` key unless the receiver
        /// is `self` (or names the enclosing class, which an empty prefix makes
        /// impossible), so a toplevel `def Foo.bar` resolves a later bare `bar`
        /// there — and `call.unresolved-toplevel` must match.
        receiver_def_name: Option<String>,
        /// The RENDERED constant path of a non-`self` def receiver
        /// (`def Foo::Bar.baz` -> `Some("Foo::Bar")`, `def obj.x` -> `None` —
        /// a dynamic receiver names no constant). Set whenever
        /// `receiver_def_name` is; the reference's `def_singleton?` skips a
        /// receiver-bearing def whose rendered path equals the def-owner
        /// prefix's tail (`Object.class_eval { def Object.x }` is a singleton
        /// def, NOT `Object#x`), so the def walk needs the path itself, not
        /// just the method name (`Source::ConstantPath.render` —
        /// `fb781023`).
        def_receiver_path: Option<String>,
        /// The span of the method's PARAMETER LIST (Prism `DefNode#parameters`),
        /// or `None` when the def takes no parameters.
        ///
        /// Parameter DEFAULT-VALUE expressions are lowered as arena nodes (so the
        /// call rules reach a `def f(t = Time.current)`), which puts any write
        /// inside a default — `def in_range(start, limit = (not_set = true))` —
        /// inside the def's span. The reference's `DeadAssignmentCollector`
        /// gathers writes from `def_node.body` ONLY, so a default's write is not
        /// a dead-assignment candidate there; this span is how the span-scanning
        /// port excludes the same region (rigor-survey
        /// `rspec-benchmark-0.6.0/lib/rspec/benchmark/complexity_matcher.rb:50`).
        param_span: Option<Span>,
        has_explicit_return: bool,
        /// The method's PLAIN-POSITIONAL param names in order, or `None` to
        /// decline tier-4b param binding (splat/post/kwargs/block/optional
        /// present). See [`MethodBody::params`].
        params: Option<Vec<String>>,
        /// The full RBS-relevant parameter STRUCTURE (counts + flags), for
        /// `sig-gen`'s `initialize` stub. See [`ParamShape`].
        param_shape: ParamShape,
        /// EVERY name the parameter list binds — required, optional, rest,
        /// post, keyword, keyword-rest and block parameters, destructured
        /// (`def f((a, b))`) names included — in no particular order. Empty for a
        /// parameterless def and for a `class << X` body. Read by the #1021
        /// reach analysis (`rigor-infer`), which must know whether a def-body
        /// local STARTS as an untyped parameter or as an unassigned (`nil`)
        /// local. Over-collection is safe there (a name wrongly counted as a
        /// parameter only declines more); a missed name is not.
        param_names: Vec<String>,
        /// Precise span of the method-NAME token (Prism `name_loc`), or `None`
        /// for a name-less `class << self` body. The
        /// `def.override-visibility-reduced` rule anchors its diagnostic here
        /// (matching the reference's `Diagnostic.from_name_loc`).
        name_span: Option<Span>,
        /// For a singleton-class body (`is_singleton_class`), the lowered
        /// `class << <expr>` operand — `Some` always, since Prism requires the
        /// expression. `None` on every other Definition. The def-attribution
        /// walk needs it to tell `class << self` (body methods belong to the
        /// enclosing self's singleton) from `class << Const` (that constant's
        /// singleton) from `class << <expr>` (a singleton nothing names),
        /// mirroring the reference's `singleton_class_prefix` (fb781023).
        singleton_operand: Option<NodeId>,
        body: Vec<NodeId>,
        span: Span,
    },
    /// A `class` definition with structure (ADR-0023 tier-4 in-source typing):
    /// the constant-path `name` (`"Point"`, `"Foo::Bar"`), the written
    /// `superclass` name if any (`< Bar` -> `Some("Bar")`, a path keeps its last
    /// component for chain-walking), and the **instance** method names defined
    /// directly in the class body (from `def`s). `body` is still lowered so
    /// nested calls reach the rule walk. Not a value — never typed directly; the
    /// inference engine harvests `name`/`superclass`/`methods` into a per-run
    /// SourceIndex so `X.new` can be typed as an instance of `X`.
    ClassDef {
        name: String,
        /// `true` when the header is written `::`-rooted (`class ::Foo`) —
        /// `Source::ConstantPath.rooted?`. A rooted header RESETS the lexical
        /// prefix its body declares under (`declaration_prefix`), rather than
        /// appending to it.
        rooted: bool,
        /// `true` when the header's leftmost base is `self` (`class self::Foo`)
        /// — `self_anchored_tail`. A `self::` header under a REBOUND self (an
        /// eval/meta-new body) names `owner::Name`; under a nameless self it is
        /// unnameable; otherwise it resolves lexically like any other path.
        self_anchored: bool,
        superclass: Option<String>,
        /// ADR-35 slice 1: the FULL written superclass path (`< Foo::Bar` ->
        /// `Some("Foo::Bar")`), distinct from `superclass` (which keeps only the
        /// last component for the existing chain-walk). Used by the
        /// override-visibility ancestor walk to resolve against lexical nesting
        /// WITHOUT the last-component name-collision merge.
        superclass_path: Option<String>,
        methods: Vec<String>,
        /// Per direct instance method: `(name, lowered body node ids,
        /// has_explicit_return)`. Parallel to `methods` (same inclusion rule —
        /// instance-only, direct, `def self.x`/nested-class/conditional defs
        /// excluded) but carries the lowered body so ADR-0023 tier-4b can type
        /// the method's RETURN expression. Kept SEPARATE from `methods` so the
        /// existing `SourceIndex::add_source` signature and tests are untouched.
        method_bodies: Vec<MethodBody>,
        /// ADR-35 slice 1: the discovered instance-method visibility table, in
        /// source order — `(method name, visibility)` per the
        /// [`Visibility`] semantics. Singleton defs excluded; `private def foo`
        /// records as the running default (untracked, mirroring the reference).
        method_visibilities: Vec<(String, Visibility)>,
        /// ADR-35 slice 1: the `include X` / `prepend X` constant names (last
        /// path component, mirroring how `superclass` is captured) in source
        /// order. The override-visibility ancestor walk resolves these FIRST,
        /// then the superclass (Ruby MRO ordering).
        includes: Vec<String>,
        body: Vec<NodeId>,
        span: Span,
    },
    /// A `module` definition with structure. Like [`Node::ClassDef`] but with no
    /// superclass (a module has none). Harvested into the SourceIndex so an
    /// instance method defined on a module is visible when the module is included
    /// (include resolution is future work; the name/methods are recorded now).
    ModuleDef {
        name: String,
        /// `true` when the header is written `::`-rooted (`module ::Foo`) —
        /// see [`Node::ClassDef::rooted`].
        rooted: bool,
        /// `true` when the header's leftmost base is `self` (`module self::Foo`)
        /// — see [`Node::ClassDef::self_anchored`].
        self_anchored: bool,
        methods: Vec<String>,
        /// Per direct instance method `(name, body ids, has_explicit_return)`.
        /// See [`Node::ClassDef::method_bodies`].
        method_bodies: Vec<MethodBody>,
        /// ADR-35 slice 1 visibility table. See [`Node::ClassDef::method_visibilities`].
        method_visibilities: Vec<(String, Visibility)>,
        /// ADR-35 slice 1 include/prepend names. See [`Node::ClassDef::includes`].
        includes: Vec<String>,
        body: Vec<NodeId>,
        span: Span,
    },
    /// `if`/`unless`/ternary. `predicate`, the `then` branch and the optional
    /// `else`/`elsif` subsequent are all lowered. Typed as `Dynamic[top]` (an
    /// `if`-as-expression has no precise branch-union type in this slice).
    ///
    /// `is_unless` distinguishes the `unless` KEYWORD from `if`/ternary. Prism
    /// keeps `IfNode` and `UnlessNode` as separate types; the lowering collapses
    /// both into this one variant, so the keyword would otherwise be lost. It is
    /// load-bearing for `flow.unreachable-branch`: an `unless` INVERTS which
    /// branch a literal predicate makes dead (`unless false…else…` kills the
    /// ELSE branch, where the same predicate under `if` kills the THEN branch).
    /// Without it the diagnostic would anchor on LIVE code. For an `unless`,
    /// `then_body` is the `unless` body and `else_body` is its `else` clause —
    /// the same physical layout as `if`, just reached by the inverted predicate.
    // TODO(spec): branch-union typing (ADR-0022 flow narrowing).
    If {
        predicate: NodeId,
        then_body: Vec<NodeId>,
        else_body: Vec<NodeId>,
        /// `true` iff this came from the `unless` keyword (never for `if` or a
        /// ternary). See the variant doc for why the keyword must survive.
        is_unless: bool,
        span: Span,
    },
    /// `case`/`when` or `case`/`in`. The optional subject predicate, every
    /// branch condition/pattern, and every branch body are lowered. Typed as
    /// `Dynamic[top]`.
    Case {
        predicate: Option<NodeId>,
        branches: Vec<NodeId>,
        else_body: Vec<NodeId>,
        span: Span,
    },
    /// One `when` clause of a `case`/`when`: its condition expressions and its
    /// body statements, held in EXPLICITLY SEPARATE lists. Before this variant
    /// a `when` lowered to a reused [`Node::BeginRescue`] carrier with the
    /// conditions PREPENDED to the body — a lossy encoding no consumer could
    /// split, which the `is_a?`/`case-when` narrowing slice needs (a clause's
    /// body runs under its conditions' truthy edge). A `case`/`in` pattern
    /// branch still uses the `BeginRescue` carrier (patterns are not condition
    /// expressions). As an EXPRESSION the clause's value is its last body
    /// statement — or, for an empty body, its last condition (`when X` with no
    /// body: byte-compatible with the pre-split concatenated carrier) — see
    /// the typer's `stmt_value_type`. Typed `Dynamic[top]` as a bare node.
    When {
        conditions: Vec<NodeId>,
        body: Vec<NodeId>,
        span: Span,
    },
    /// `while`/`until`/`for`. The (optional) predicate/collection and the loop
    /// body are lowered. Typed as `Dynamic[top]`.
    ///
    /// `index` is the LOCAL names a `for` index target binds (`for w in xs`,
    /// `for a, (b, *c) in xs`), each with its target span, which sits inside the
    /// loop's span. It is empty for `while`/`until`, and for a `for` whose index
    /// binds no local (`for @a in xs`, `for A in xs`, `for h[:k] in xs`). The
    /// reference binds the index to the element type on every iteration
    /// (`statement_evaluator.rb` `bind_for_index`), so the flow write collectors
    /// treat each name as a rebind (rigor-rs#151).
    Loop {
        predicate: Option<NodeId>,
        body: Vec<NodeId>,
        index: Vec<(String, Span)>,
        span: Span,
    },
    /// `begin`/`rescue`/`else`/`ensure`. The protected body, each rescue body,
    /// the else body and the ensure body are all lowered. Typed `Dynamic[top]`.
    ///
    /// `ensure_body` records JUST the ensure-clause statement ids (empty when the
    /// begin has no `ensure`, and for the reused carriers — `else`/`when`/`in`/
    /// parenthesized groups — which are not real `begin` nodes). The ensure
    /// statements ALSO remain appended to `body` exactly as before, so every
    /// existing consumer (the typer's tail-value resolution, sig-gen, annotate)
    /// is byte-for-byte unaffected; `ensure_body` is a purely-additive view the
    /// `flow.return-in-ensure` rule dispatches on. Kept forward-compatible with a
    /// fuller per-clause `RescueClause` structure a later `flow.shadowed-rescue-clause`
    /// slice will need.
    BeginRescue {
        body: Vec<NodeId>,
        /// Just the `begin` node's OWN statements — the ids that lead `body`
        /// before the rescue / `else` / `ensure` children are appended.
        /// `body` deliberately stays flat (every existing consumer is
        /// byte-for-byte unaffected), but that flatness merges the `else`
        /// clause's statements into the same list, and an `else` must NOT
        /// count toward the `never_completes_normally?` walk (rigor-rs#140):
        /// it only runs when the protected body completes, so `begin; "x";
        /// else; break "s"; end` still completes normally via `else`. For the
        /// reused carriers (`else`/`in`/parenthesized groups — no real
        /// `begin` node) `main_body` equals `body`.
        main_body: Vec<NodeId>,
        ensure_body: Vec<NodeId>,
        /// The per-clause rescue-chain structure (empty for the reused carriers —
        /// `else`/`when`/`in`/parenthesized groups — and for a `begin` with no
        /// `rescue`). Populated only from a real `BeginNode`'s rescue chain; see
        /// [`RescueClause`]. Additive — leaves `body`/`ensure_body` untouched.
        clauses: Vec<RescueClause>,
        span: Span,
    },
    /// A lambda literal (`-> { … }` / `->(x) { … }`). Its `body` statements are
    /// lowered (so calls/reads inside stay visible to the rule walk, closing the
    /// pre-existing soundness gap where `-> {}` fell into a non-recursing
    /// [`Node::Other`]). A lambda opens a NEW return frame: `flow.return-in-ensure`
    /// treats it as a BARRIER (a `return` inside exits the lambda, not the method
    /// whose `ensure` is being scanned). Typed `Dynamic[top]` (no `Proc` typing in
    /// this slice). `locals` is Prism's `LambdaNode#locals` — the names the
    /// lambda binds in its own scope (parameters + lambda-scoped writes, not
    /// captured outer locals), the `toplevel_rebinds` shadow set
    /// ([`Node::Call::block_locals`], rigor-rs#166).
    Lambda {
        body: Vec<NodeId>,
        locals: Vec<String>,
        span: Span,
    },
    /// `&&` / `||` / `and` / `or`. Both operands are lowered (so a call on
    /// either side is analysed). Typed `Dynamic[top]` — the result is one of the
    /// two operand types, which we don't union here.
    ///
    /// `is_and` discriminates the CONJUNCTION (`&&`/`and`) from the disjunction
    /// (`||`/`or`). Prism has two node kinds and the lowering collapsed them;
    /// the compound-predicate narrowing (stage 3a-1,
    /// docs/notes/20260807-narrowing-stage3-spec.md) needs the operator because
    /// `&&` and `||` swap which edge concatenates and which joins.
    Logical { left: NodeId, right: NodeId, is_and: bool, span: Span },
    /// An array literal (`[a, b]`). Elements are lowered. Typed `Nominal Array`
    /// so a typo'd method on an array literal flags via the real Array RBS.
    // TODO(spec): Tuple precision (element types) per ADR-0023.
    ArrayLit { elements: Vec<NodeId>, span: Span },
    /// A hash literal (`{ k => v }`). Each assoc lowers its key then its value
    /// into `elements` (a flat `[k, v, k, v, …]` list), so a call hiding in
    /// either is still walked for reachability. `all_assoc` is `true` only for a
    /// real `HashNode` whose every element was an `AssocNode` (no `**` splat),
    /// which lets the typer re-pair `elements` into a value-pinned `HashShape`;
    /// a splat, or a bare keyword-hash argument, sets it `false` (types `Hash`).
    ///
    /// `dup_keys` is a parallel, precomputed list of the literal's value-pinned
    /// assoc keys in source order (see [`HashKey`]), consumed ONLY by
    /// `flow.duplicate-hash-key`. It is additive: `elements`/`all_assoc` keep
    /// their exact prior meaning, so `Typer::hash_shape_or_hash` (which indexes
    /// the flat `elements` list under `all_assoc`) is untouched. A `**`splat makes
    /// `all_assoc` false but does NOT remove the surrounding literal keys from
    /// `dup_keys` (the splat is inert to the duplicate check).
    HashLit {
        elements: Vec<NodeId>,
        all_assoc: bool,
        dup_keys: Vec<HashKey>,
        span: Span,
    },
    /// A range (`a..b` / `a...b`). Both bounds (when present) lowered. Typed
    /// `Dynamic[top]`. Note: an index read `a[i]` is a Prism `CallNode` named
    /// `[]`, so it lowers as a [`Node::Call`] (receiver + index args) and needs
    /// no dedicated variant.
    Range { span: Span },
    /// An instance/class/global variable read (`@x`, `@@x`, `$x`). Typed
    /// `Dynamic[top]` — no ivar/cvar/gvar type tracking in this slice.
    ///
    /// `name` carries the SIGIL (`"@x"`, `"@@x"`, `"$x"`), which is both the
    /// spelling the reference's `scope.ivar`/`cvar`/`global` tables key on and
    /// the discriminator between the three variable kinds. It exists for the
    /// inference layer's #521 untyped-argument gate
    /// (`Typer::arg_reach`), which must decide whether the
    /// reference would type THIS variable `Dynamic[Top]` — a question that needs
    /// the name to find the variable's writes. Nothing else reads it; the
    /// variant is still typed `Dynamic[top]`.
    // TODO(spec): ivar typing (ADR-0022).
    VariableRead { name: String, span: Span },
    /// A class/global variable write (`@@x = v`, `$x = v`). The value is lowered
    /// (so a call in the assigned expression is analysed). Not a value itself.
    /// An INSTANCE variable write (`@x = v`) lowers to the dedicated
    /// [`Node::InstanceVariableWrite`] instead (it carries the name the
    /// `def.ivar-write-mismatch` rule groups on); this variant keeps covering
    /// the class-var / global-var writes.
    ///
    /// `name` carries the SIGIL (`"@@x"`, `"$x"`) — the twin of
    /// [`Node::VariableRead`]'s, so the #521 gate can pair a cvar/gvar read with
    /// the writes that bind it. No RULE inspects it.
    VariableWrite { name: String, value: NodeId, span: Span },
    /// An instance variable write (`@x = v`). Lowered as a dedicated variant
    /// (mirroring [`Node::LocalVariableWrite`]) so the `def.ivar-write-mismatch`
    /// collector can see the target NAME + its value's type — Prism would
    /// otherwise fold it into the nameless [`Node::VariableWrite`], losing the
    /// name. `name` includes the leading `@` (Prism `InstanceVariableWriteNode#name`
    /// is `:@x`), matching the reference message's `@x` spelling. `name_span` is
    /// the precise span of the `@x` name token (Prism `name_loc`) — the anchor the
    /// diagnostic keys on (the reference's `Diagnostic.from_name_loc`). `value` is
    /// lowered (so a call in the assigned expression stays reachable). Not a value
    /// itself (`x = (@y = 5)` types via the RHS at the [`Node::VariableWrite`]-shaped
    /// consumers, which include this variant).
    InstanceVariableWrite {
        name: String,
        value: NodeId,
        name_span: Span,
        span: Span,
    },
    /// A constant read (`Foo`, `Foo::Bar`). For a path, the parent scope is
    /// lowered. `name` is the dotted constant path (`"Foo"`, `"Foo::Bar"`), kept
    /// so a `X.new` call can resolve `X` to a class name WITHOUT typing the bare
    /// constant read itself (which stays `Dynamic[top]` — no class-object typing,
    /// the zero-FP-safe choice). Empty for an un-namable dynamic constant.
    // TODO(spec): constant resolution (ADR-0019).
    /// A constant reference. `name` is the LENIENT rendering
    /// (`constant_path_string`): `::Foo` renders bare as `"Foo"`, and a
    /// dynamic base (`expr::Bar`) contributes nothing, so `k::LIMIT` also
    /// renders as `"LIMIT"`. `dynamic_base` is what separates those two — it
    /// is the reference's `Source::ConstantPath.qualified_name_or_nil`
    /// answering nil, which a consumer that must not read through a runtime
    /// receiver (the version-guard operand reader) tests.
    ///
    /// `self_anchored` is `true` for a `self::X` / `self::X::Y` path — the
    /// reference's `self_anchored_tail` arm of `eval_receiver_prefix`
    /// (fb781023): such a path resolves against the enclosing SELF (the eval
    /// block's rebound owner), never the lexical nesting, so the
    /// def-attribution walk needs it told apart from a plain `X`. `name` still
    /// renders `"X"`. `dynamic_base` stays `true` for this spelling too (a
    /// `self` parent is not a constant), which is what its existing consumers
    /// already assumed.
    ConstantRead {
        name: String,
        span: Span,
        dynamic_base: bool,
        self_anchored: bool,
        /// `rooted` is `true` for a `::X` / `::X::Y` path — the reference's
        /// `Source::ConstantPath.rooted?` (its `eval_constant_receiver_prefix`
        /// consults it FIRST, before the lexical walk): a rooted spelling
        /// re-anchors at the top level, so `::Object` inside `module M` names
        /// `Object`, never `M::Object` — even when the file declares
        /// `M::Object`. `name` still renders unrooted (`"Object"`): the flag
        /// carries the anchoring, exactly like `self_anchored` does.
        rooted: bool,
    },
    /// A constant write (`FOO = v`). The value is lowered. Not a value itself.
    /// `name` is the WRITTEN constant name (`"FOO"`; the last component for a
    /// `Foo::Bar = v` path-write, else empty for an un-namable dynamic form) —
    /// used by sig-gen to build the file's `Data.define`/`Struct.new` constant
    /// FQN map for qualified source-class naming.
    ConstantWrite { name: String, value: NodeId, span: Span },
    /// `self`. Typed `Dynamic[top]` — the enclosing-class type is not tracked in
    /// this slice.
    SelfExpr { span: Span },
    /// Catch-all for any Prism node not yet given an owned variant, so the
    /// lowering walk is total. Carries the original span for completeness.
    ///
    // TODO(spec): grow the owned-node set toward full Prism coverage, and add
    /// An explicit `return` statement. `values` are the lowered argument
    /// expressions in source order — empty for a bare `return`, one for
    /// `return e`, several for `return a, b`. A STATEMENT, not a value: the
    /// typer's catch-all types it `Dynamic[top]` (exactly like the recovered-
    /// children `Statements` carrier it replaced), so the check path is
    /// behavior-preserving; sig-gen's `DefReturnTyper` port reads `values` to
    /// union a def's explicit returns into its return type. The children live
    /// in the arena so calls / local reads inside a return stay visible to the
    /// rule walk (`flow.dead-assignment`, the call rules).
    Return { values: Vec<NodeId>, span: Span },
    // synthetic-node variants (plugin/macro-generated definitions with no
    // source text) per ADR-0012 / ADR-0013. No plugins yet, so no synthetic
    // variant is materialized in this slice.
    /// An UNMODELED construct, with an optional control-flow-jump
    /// discriminator.
    ///
    /// `jump` is `Some` for an ARGUMENT-LESS `next` / `break` and `None` for
    /// everything else. Prism models those as `NextNode`/`BreakNode`, which
    /// recover no children and would otherwise be indistinguishable from any
    /// other unmodeled leaf — yet the reference's
    /// `branch_unconditionally_exits?` treats them exactly like a `return`, so
    /// the class-narrowing pass's early-termination propagation needs to see
    /// them ([`Node::Other`] carries no children, so a one-bit discriminator is
    /// the whole change; a real owned `Jump` variant would have to be wired
    /// into every child walk for no measured gain).
    ///
    /// `next e` / `break e` deliberately stay `jump: None`: they carry a value
    /// that must remain reachable to the rule walk, which today happens through
    /// the recovered-children `Statements` carrier. Recognising them is a
    /// recorded DECLINE (probes `p16_next_with_value` / `p16b_break_with_value`
    /// — the reference narrows through them), not an oversight.
    Other { span: Span, jump: Option<JumpKind> },
    /// `alias new_name old_name` (Prism `AliasMethodNode`). Both operands are
    /// lowered so an interpolated name's calls stay reachable to the rule
    /// walk; the def-attribution walk reads the literal symbol names through
    /// `literal_method_name` (only a `SymbolLit` names a method — mirroring
    /// `record_alias_method`'s `new_name.is_a?(Prism::SymbolNode)` gate).
    Alias {
        new_name: NodeId,
        old_name: NodeId,
        span: Span,
    },
}

/// What a [`Node::Statements`] carrier is, for the passes that thread a local
/// environment through it (rigor-rs#151 / #153).
///
/// The lowering links recovered children under a `Statements` carrier so the
/// structural walks (the call rules, `flow.dead-assignment`'s read gather) keep
/// seeing them. That carrier used to look exactly like a real statement
/// sequence, so the env binders descended it as straight-line code and bound a
/// write that runs conditionally, later, or never. Every structural walk ignores
/// `kind`; only a pass that BINDS or WIDENS locals reads it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatementsKind {
    /// A real statement sequence: a Prism `StatementsNode`, or the `#{ … }` of
    /// an interpolation. Its children run in order, so a write in it binds.
    Sequence,
    /// The generic recovery carrier of a Prism node with no owned variant: a
    /// `rescue` modifier, `super(…)`, `yield`, a splat, … Its children are
    /// flattened out of arbitrary expression structure, so their order and
    /// whether they run at all are unknown (`(w = 1) rescue nil` may raise
    /// before the write; a conditional under `super(…)` is flattened away).
    /// A write in it must not bind; the binders widen it instead.
    Recovered,
    /// Code whose writes never reach the local scope, as far as flow is
    /// concerned: a `defined?` operand (never evaluated), an `END { }` body
    /// (deferred to exit), a `BEGIN { }` body, and the arguments and block of
    /// `super(…)` / `super` / `yield(…)`. The reference's statement evaluator
    /// has no handler for `DefinedNode`, `PostExecutionNode`,
    /// `PreExecutionNode`, `SuperNode`, `ForwardingSuperNode` or `YieldNode`, so
    /// it types them as pure expressions and leaves the scope unchanged: a write
    /// inside neither binds nor widens (probes r2/c2, r1/c1, b1/b2, g1/g2,
    /// s1-s5, m1/m3/m4/m5). The write collectors drop every write inside one;
    /// see [`LoweredAst::in_inert_carrier`].
    ///
    /// [`LoweredAst::in_inert_carrier`]: crate::ast::LoweredAst::in_inert_carrier
    Inert,
    /// A jump statement that carries (or could carry) VALUE expressions:
    /// `break e` / `next e` hold their argument list in `body`, and `redo` /
    /// `retry` ride the same carrier with an empty `body`. The children are
    /// real lowered expressions (a read under `break x` stays reachable, just
    /// as the `Recovered` carrier kept it), but the jump's control-flow kind is
    /// what the exactly-once block-timing proof (rigor-rs#140, upstream
    /// rigor#1105) discriminates on. A write in `body` must not bind — it is
    /// an argument position, not a sequence — so the kind reads like
    /// `Recovered` to every binder.
    Jump(JumpKind),
}

/// Which control-flow jump an argument-less [`Node::Other`] or a
/// [`StatementsKind::Jump`] carrier is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JumpKind {
    /// `next` — skip to the next iteration of the enclosing block / loop.
    Next,
    /// `break` — leave the enclosing block / loop.
    Break,
    /// `redo` — restart the enclosing block / loop body's current iteration.
    Redo,
    /// `retry` — restart the enclosing `begin`/`rescue` from the top.
    Retry,
}

impl Node {
    /// The byte span of this node, regardless of variant.
    pub fn span(&self) -> Span {
        match self {
            Node::Program { span, .. }
            | Node::Statements { span, .. }
            | Node::LocalVariableWrite { span, .. }
            | Node::LocalVariableOpWrite { span, .. }
            | Node::MultiWrite { span, .. }
            | Node::LocalVariableRead { span, .. }
            | Node::StringLit { span, .. }
            | Node::InterpolatedString { span, .. }
            | Node::InterpolatedSymbol { span, .. }
            | Node::IntegerLit { span, .. }
            | Node::FloatLit { span, .. }
            | Node::SymbolLit { span, .. }
            | Node::NilLit { span }
            | Node::TrueLit { span }
            | Node::FalseLit { span }
            | Node::Call { span, .. }
            | Node::Definition { span, .. }
            | Node::ClassDef { span, .. }
            | Node::ModuleDef { span, .. }
            | Node::If { span, .. }
            | Node::Case { span, .. }
            | Node::When { span, .. }
            | Node::Loop { span, .. }
            | Node::BeginRescue { span, .. }
            | Node::Lambda { span, .. }
            | Node::Logical { span, .. }
            | Node::ArrayLit { span, .. }
            | Node::HashLit { span, .. }
            | Node::Range { span }
            | Node::VariableRead { span, .. }
            | Node::VariableWrite { span, .. }
            | Node::InstanceVariableWrite { span, .. }
            | Node::ConstantRead { span, .. }
            | Node::ConstantWrite { span, .. }
            | Node::SelfExpr { span }
            | Node::Return { span, .. }
            | Node::Other { span, .. }
            | Node::Alias { span, .. } => *span,
        }
    }
}
