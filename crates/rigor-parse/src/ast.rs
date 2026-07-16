//! Owned, `NodeId`-indexed AST + the Prism lowering pass (ADR-0012).
//!
//! `ruby-prism` nodes borrow the parse buffer (a lifetime on every node).
//! Threading that lifetime through the inference engine reproduces the
//! pervasive-`'a` pain ADR-0005/0006 deliberately avoid, so [`lower`] walks the
//! borrowed Prism tree exactly once and produces *owned* nodes keyed by a dense
//! [`NodeId`]. Inference and rules then walk this owned arena, never the
//! borrowed Prism tree.
//!
//! The owned node *shape* mirrors Prism's (rather than normalizing to a
//! semantically different HIR) so node-level behaviour stays aligned with the
//! reference for diagnostic-set parity (ADR-0002/0012). The long tail of Prism
//! nodes still lowers to [`Node::Other`] so an unhandled construct never aborts
//! the walk.
//!
//! ## Recursive structural lowering
//!
//! Beyond the top-level literal/call subset, [`lower`] recurses into the bodies
//! of definitions (`def`/`class`/`module`/singleton class), control flow
//! (`if`/`unless`/ternary, `case`/`when`/`in`, `while`/`until`/`for`,
//! `begin`/`rescue`/`ensure`, `&&`/`||`), blocks (`foo { ... }`), and into the
//! receivers/values of variable, constant, array, hash, index, range and
//! string-interpolation nodes. The point is reachability: EVERY nested call
//! lands in the arena as a [`Node::Call`], so the single rule walk
//! (`ast.iter()` filtering `Node::Call`) analyses calls inside a method/branch
//! body, not just top-level ones. Structural variants carry child [`NodeId`]s so
//! the typer can recurse into a receiver/argument; constructs we don't type
//! precisely still get their children lowered (and so analysed).

use crate::ruby_prism::{self, Node as PrismNode, ParseResult};

/// A dense handle into [`LoweredAst::nodes`]. Cheap to copy; stable for the
/// lifetime of the owned AST (ADR-0012: owned, `NodeId`-keyed nodes).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct NodeId(pub u32);

/// A source span as a half-open byte-offset range `[start, end)`.
///
/// Byte offsets (not line/col) are the load-bearing location per ADR-0030; the
/// CLI computes line/col lazily from the source when presenting (ADR-0030:
/// Prism columns are 0-based, the presenter adds 1).
pub type Span = (usize, usize);

/// A direct instance method harvested for ADR-0023 tier-4b RETURN inference:
/// the method `name`, the lowered `body` statement ids (so the return
/// expression can be typed), and `has_explicit_return` (any `return` in the
/// Prism body ⇒ the inference declines). Carried on [`Node::ClassDef`] /
/// [`Node::ModuleDef`] alongside the method-name list.
///
/// `params` records the method's PLAIN-POSITIONAL parameter names in order
/// (`def full(x, y)` -> `Some(["x", "y"])`), enabling ADR-0023 tier-4b call-site
/// PARAMETER BINDING: a method whose tail reads / chains off a bare positional
/// param can have its return re-derived from the ARGUMENT type at the call site.
/// It is `None` — meaning "decline this method for param binding" — whenever the
/// signature has ANYTHING that breaks positional index<->arg alignment: a splat
/// (`*args`), a post-splat positional, a keyword / double-splat (`**`), a block
/// param (`&blk`), or a default-valued (optional) param (`def f(x = 1)`). The
/// param-INDEPENDENT inference (a tail that types to a concrete core class under
/// an empty env) is unaffected by `params` — it never reads a param.
#[derive(Clone, Debug)]
pub struct MethodBody {
    pub name: String,
    pub body: Vec<NodeId>,
    pub has_explicit_return: bool,
    /// `Some(plain positional param names in order)`, or `None` to DECLINE
    /// param binding for this method (splat/post/kwargs/block/optional present).
    pub params: Option<Vec<String>>,
}

/// The RBS-relevant STRUCTURE of a method's parameter list — the counts + flags
/// `sig-gen`'s `initialize` stub renders (`(untyped, ?untyped, *untyped, name:
/// untyped, ?opt: untyped, **untyped, ?{ (?) -> void })`). Distinct from
/// [`MethodBody::params`], which captures only plain-positional NAMES and
/// declines any complex shape. Posts (`def f(a, *rest, b)`'s `b`) are
/// deliberately omitted — the reference's stub renderer drops them too.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParamShape {
    /// Number of required positionals (each → `untyped`).
    pub required: usize,
    /// Number of optional positionals (each → `?untyped`).
    pub optional: usize,
    /// A rest param `*args` is present (→ `*untyped`).
    pub has_rest: bool,
    /// Keyword params in source order: `(name, is_optional)` (→ `name: untyped`
    /// / `?name: untyped`).
    pub keywords: Vec<(String, bool)>,
    /// A keyword-rest `**opts` is present (→ `**untyped`).
    pub has_kwrest: bool,
    /// A block param `&blk` is present (→ `?{ (?) -> void }`).
    pub has_block: bool,
}

impl ParamShape {
    /// A trivial parameter list (all-empty) — the reference EXCLUDES a trivial
    /// `initialize` from the stub (the `Object#initialize` RBS covers it).
    pub fn is_trivial(&self) -> bool {
        self.required == 0
            && self.optional == 0
            && !self.has_rest
            && self.keywords.is_empty()
            && !self.has_kwrest
            && !self.has_block
    }
}

/// Instance-method visibility as discovered at lowering time (ADR-35 slice 1,
/// the `def.override-visibility-reduced` rule). Mirrors the reference's
/// `scope_indexer.rb` visibility table semantics exactly:
///   * a class/module body is walked left-to-right with a running default that
///     starts [`Visibility::Public`];
///   * a bare `private` / `protected` / `public` call (no args) FLIPS the
///     running default for subsequent `def`s;
///   * `private :foo, :bar` / `private "foo"` (literal symbol/string args)
///     BACK-PATCHES those named methods to that visibility;
///   * a plain `def foo` records `foo` at the current running default;
///   * `private def foo` (the modifier-wrapping-a-def form) is NOT tracked — it
///     records as the running default — matching the reference gap exactly so
///     the witness set stays ⊆ the reference's;
///   * dynamic forms (`send(:private, …)`, `private(*names)`) are NOT recognised;
///   * singleton defs (`def self.x`, inside `class << self`) are EXCLUDED.
///
/// The `Ord`-by-rank comparison (public > protected > private) lives in the rule
/// layer; this enum only carries the discovered atom.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility {
    Public,
    Protected,
    Private,
}

/// The comparable identity of a value-pinned Hash-literal key, tagged by literal
/// KIND so the literal spaces stay separate (`:a` ≠ `"a"`, `1` ≠ `1.0`) — Ruby
/// `Hash#eql?` semantics. A faithful port of the reference's `literal_key`
/// (`duplicate_hash_key_collector.rb`): only these value-pinned forms participate
/// in the `flow.duplicate-hash-key` check; any other key form (interpolated
/// string/symbol, constant, call, local, `**splat`) never enters the seen set.
///
/// Float identity is carried as the `f64` bit pattern (`to_bits`), so `1.0` and
/// `1.00` collide (same bits) exactly as `1.0.eql?(1.00)` is true, while
/// `Int(1)` and `Float(1.0)` never compare (distinct variants).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HashKeyTag {
    Sym(String),
    Str(String),
    /// Parsed integer value (radix/underscore-normalised) — value identity, so
    /// `1` and `0x1` collide exactly as `1.eql?(0x1)` is true. `i128` covers every
    /// realistic literal key; a value beyond it is treated as non-pinned (skipped).
    Int(i128),
    /// `f64::to_bits` of the parsed float (Ruby `eql?`/`hash` identity).
    Float(u64),
    True,
    False,
    Nil,
}

/// A value-pinned key of a Hash literal (braced `HashNode` or bare-kwargs
/// `KeywordHashNode`), precomputed at lowering time so the `flow.duplicate-hash-key`
/// rule stays source-free. `tag` is the collision identity; `label` is the
/// message rendering (symbol → `:name`, string → Ruby `String#inspect`,
/// integer/float/`true`/`false`/`nil` → the verbatim source slice, so
/// `{ 1.0 => x, 1.00 => y }` renders `` `1.00' ``); `anchor` is the key node's
/// byte span (the diagnostic anchors at its start); `line` is the key's 1-based
/// start line (the message's "first set at line N"). Only value-pinned assoc keys
/// are recorded, in source order — splats and non-pinned keys are omitted (they
/// never participate), so the rule's seen-map dedup over this list reproduces the
/// reference's element walk exactly.
#[derive(Clone, Debug)]
pub struct HashKey {
    pub anchor: Span,
    pub line: u32,
    pub tag: HashKeyTag,
    pub label: String,
}

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
    /// A sequence of statements (a `begin`/method/program body in Prism).
    Statements { body: Vec<NodeId>, span: Span },
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
    /// An integer literal (`42`).
    IntegerLit { value: i64, span: Span },
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
        /// argument-dependent constant folding). Splat/keyword/block args are
        /// intentionally not collected here in this slice.
        args: Vec<NodeId>,
        /// Statements of an attached block (`foo { ... }` / `do…end`), lowered
        /// so calls inside the block reach the rule walk. Empty for a call with
        /// no block. Not a *value* of the call — purely a reachability handle.
        block_body: Vec<NodeId>,
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
        has_explicit_return: bool,
        /// The method's PLAIN-POSITIONAL param names in order, or `None` to
        /// decline tier-4b param binding (splat/post/kwargs/block/optional
        /// present). See [`MethodBody::params`].
        params: Option<Vec<String>>,
        /// The full RBS-relevant parameter STRUCTURE (counts + flags), for
        /// `sig-gen`'s `initialize` stub. See [`ParamShape`].
        param_shape: ParamShape,
        /// Precise span of the method-NAME token (Prism `name_loc`), or `None`
        /// for a name-less `class << self` body. The
        /// `def.override-visibility-reduced` rule anchors its diagnostic here
        /// (matching the reference's `Diagnostic.from_name_loc`).
        name_span: Option<Span>,
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
    /// `while`/`until`/`for`. The (optional) predicate/collection and the loop
    /// body are lowered. Typed as `Dynamic[top]`.
    Loop {
        predicate: Option<NodeId>,
        body: Vec<NodeId>,
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
    /// this slice).
    Lambda { body: Vec<NodeId>, span: Span },
    /// `&&` / `||` / `and` / `or`. Both operands are lowered (so a call on
    /// either side is analysed). Typed `Dynamic[top]` — the result is one of the
    /// two operand types, which we don't union here.
    Logical { left: NodeId, right: NodeId, span: Span },
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
    // TODO(spec): ivar typing (ADR-0022).
    VariableRead { span: Span },
    /// A class/global variable write (`@@x = v`, `$x = v`). The value is lowered
    /// (so a call in the assigned expression is analysed). Not a value itself.
    /// An INSTANCE variable write (`@x = v`) lowers to the dedicated
    /// [`Node::InstanceVariableWrite`] instead (it carries the name the
    /// `def.ivar-write-mismatch` rule groups on); this nameless variant keeps
    /// covering the class-var / global-var writes, which no rule inspects by name.
    VariableWrite { value: NodeId, span: Span },
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
    ConstantRead { name: String, span: Span },
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
    Other { span: Span },
}

impl Node {
    /// The byte span of this node, regardless of variant.
    pub fn span(&self) -> Span {
        match self {
            Node::Program { span, .. }
            | Node::Statements { span, .. }
            | Node::LocalVariableWrite { span, .. }
            | Node::LocalVariableOpWrite { span, .. }
            | Node::LocalVariableRead { span, .. }
            | Node::StringLit { span, .. }
            | Node::InterpolatedString { span, .. }
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
            | Node::Loop { span, .. }
            | Node::BeginRescue { span, .. }
            | Node::Lambda { span, .. }
            | Node::Logical { span, .. }
            | Node::ArrayLit { span, .. }
            | Node::HashLit { span, .. }
            | Node::Range { span }
            | Node::VariableRead { span }
            | Node::VariableWrite { span, .. }
            | Node::InstanceVariableWrite { span, .. }
            | Node::ConstantRead { span, .. }
            | Node::ConstantWrite { span, .. }
            | Node::SelfExpr { span }
            | Node::Return { span, .. }
            | Node::Other { span } => *span,
        }
    }
}

/// The owned AST: a flat arena of [`Node`]s plus the [`NodeId`] of the root
/// `Program`. Free of the Prism parse-buffer lifetime (ADR-0012).
#[derive(Clone, Debug)]
pub struct LoweredAst {
    nodes: Vec<Node>,
    root: NodeId,
}

impl LoweredAst {
    /// Resolve a handle to its owned node.
    pub fn get(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    /// The root `Program` node id.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Number of owned nodes in the arena.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the arena is empty. Never true after a successful [`lower`].
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Iterate `(NodeId, &Node)` over the arena in id order. Rules use this to
    /// walk every node in a single converged pass (ADR-0005).
    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (NodeId(i as u32), n))
    }
}

/// Lower a borrowed Prism [`ParseResult`] into the owned, `NodeId`-indexed AST
/// (ADR-0012). Walks the tree once; unhandled Prism nodes become
/// [`Node::Other`] so the walk is total and never panics on a novel construct
/// (ADR-0016 never-crash posture).
pub fn lower(result: &ParseResult<'_>) -> LoweredAst {
    let source = result.source();
    // Byte offset of every line start, so a node's 1-based line is a binary
    // search (used to precompute Hash-key lines for `flow.duplicate-hash-key`,
    // whose message embeds the first occurrence's line — the rule is source-free).
    let mut line_starts: Vec<usize> = vec![0];
    for (i, &b) in source.iter().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let mut builder = Builder {
        nodes: Vec::new(),
        source,
        line_starts,
    };
    let root_prism = result.node();
    let root = builder.lower_node(&root_prism);
    LoweredAst {
        nodes: builder.nodes,
        root,
    }
}

/// Mutable accumulator for the owned arena during the lowering walk.
struct Builder<'src> {
    nodes: Vec<Node>,
    /// The full parsed source, for capturing verbatim key slices + line numbers.
    source: &'src [u8],
    /// Byte offset of every line start (index 0 = line 1).
    line_starts: Vec<usize>,
}

impl<'src> Builder<'src> {
    /// Push an owned node, returning its fresh [`NodeId`].
    fn push(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(node);
        id
    }

    /// The 1-based source line of a byte offset (binary search over line starts).
    fn line_at(&self, offset: usize) -> u32 {
        self.line_starts.partition_point(|&ls| ls <= offset) as u32
    }

    /// Build the value-pinned [`HashKey`] list for a Hash/keyword-hash literal's
    /// elements, in source order. A faithful port of the reference's `literal_key`
    /// and `key_label`: only a symbol, plain-string, integer, float, `true`,
    /// `false`, or `nil` key is recorded (non-value-pinned keys and `**`splats are
    /// skipped, never entering the seen set). Uses the borrowed Prism nodes so the
    /// verbatim source slice (integer/float labels) is available.
    fn hash_keys_of(&self, elements: &ruby_prism::NodeList<'_>) -> Vec<HashKey> {
        let mut keys = Vec::new();
        for el in elements.iter() {
            let Some(assoc) = el.as_assoc_node() else {
                continue; // `**splat` (AssocSplatNode) — inert, skipped.
            };
            let key = assoc.key();
            let loc = key.location();
            let anchor = span_of(&loc);
            let line = self.line_at(anchor.0);
            let raw = || constant_string(loc.as_slice());
            let (tag, label) = if let Some(sym) = key.as_symbol_node() {
                let name = constant_string(sym.unescaped());
                (HashKeyTag::Sym(name.clone()), format!(":{name}"))
            } else if let Some(s) = key.as_string_node() {
                let contents = constant_string(s.unescaped());
                (HashKeyTag::Str(contents.clone()), ruby_inspect_string(&contents))
            } else if key.as_integer_node().is_some() {
                match parse_ruby_integer(&raw()) {
                    Some(v) => (HashKeyTag::Int(v), raw()),
                    None => continue, // bignum beyond i128 — treat as non-pinned.
                }
            } else if let Some(f) = key.as_float_node() {
                (HashKeyTag::Float(f.value().to_bits()), raw())
            } else if key.as_true_node().is_some() {
                (HashKeyTag::True, raw())
            } else if key.as_false_node().is_some() {
                (HashKeyTag::False, raw())
            } else if key.as_nil_node().is_some() {
                (HashKeyTag::Nil, raw())
            } else {
                continue; // interpolated / constant / call / local — non-pinned.
            };
            keys.push(HashKey { anchor, line, tag, label });
        }
        keys
    }

    /// Lower one borrowed Prism node (and its children) into owned nodes,
    /// returning the id of the produced root. Recursion is bounded by the
    /// source's nesting depth.
    fn lower_node(&mut self, node: &PrismNode<'_>) -> NodeId {
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

        if let Some(read) = node.as_local_variable_read_node() {
            let name = constant_string(read.name().as_slice());
            return self.push(Node::LocalVariableRead {
                name,
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
            // Prism's `Integer` exposes only `TryInto<i32>` in this binding;
            // for the tracer-bullet literal subset that range suffices.
            // TODO(spec): widen to full bignum / i64 once a wider accessor is
            // available (or via to_u32_digits) — value-lattice Constant[Int].
            let value: i64 = TryInto::<i32>::try_into(int.value())
                .map(i64::from)
                .unwrap_or(0);
            return self.push(Node::IntegerLit {
                value,
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
            // contracts + arg-dependent folding). Splat/keyword/block args lower
            // like any other node — a downstream rule that needs to distinguish
            // them does so by inspecting the lowered child, never here.
            // TODO(spec): mark splat/keyword/block args so the arity rule can
            // bail on non-plain-positional shapes (it is conservative regardless).
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
                message_span,
                // `x&.foo` ⇒ safe-nav; `x.foo` ⇒ plain dot. Threaded so
                // `call.possible-nil-receiver` can faithfully suppress on `&.`.
                safe_nav: call.is_safe_navigation(),
                first_arg_nonplain,
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
            // A `def self.x` (SELF receiver) captures its method name here so
            // `sig-gen` can collect the singleton; `name` stays `None` so the
            // instance-method harvest still skips it. A non-self receiver
            // (`def obj.x`) is left `None`.
            let singleton_name = def
                .receiver()
                .filter(|r| r.as_self_node().is_some())
                .map(|_| constant_string(def.name().as_slice()));
            return self.push(Node::Definition {
                name,
                is_singleton_class: false,
                singleton_name,
                has_explicit_return,
                params,
                param_shape,
                name_span,
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
                methods,
                method_bodies,
                method_visibilities,
                includes,
                body,
                span: span_of(&module.location()),
            });
        }

        if let Some(sclass) = node.as_singleton_class_node() {
            let body = self.lower_optional_body(sclass.body().as_ref());
            return self.push(Node::Definition {
                name: None, // `class << self` has no single method name.
                is_singleton_class: true, // a CLASS scope, not a method def.
                singleton_name: None, // the BODY's inner defs are the singletons.
                has_explicit_return: false,
                params: None,    // no single method ⇒ no param binding.
                param_shape: ParamShape::default(),
                name_span: None, // no single name ⇒ no name span.
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
            return self.push(Node::BeginRescue {
                body,
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
            // A `when` branch: lower its condition expressions and body.
            let mut body: Vec<NodeId> = when_node
                .conditions()
                .iter()
                .map(|c| self.lower_node(&c))
                .collect();
            if let Some(s) = when_node.statements() {
                body.extend(self.lower_body(&s.body()));
            }
            return self.push(Node::BeginRescue {
                body,
                ensure_body: Vec::new(),
                clauses: Vec::new(),
                span: span_of(&when_node.location()),
            });
        }

        if let Some(in_node) = node.as_in_node() {
            // An `in` pattern branch: lower the pattern and the body.
            let mut body = vec![self.lower_node(&in_node.pattern())];
            if let Some(s) = in_node.statements() {
                body.extend(self.lower_body(&s.body()));
            }
            return self.push(Node::BeginRescue {
                body,
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
                span: span_of(&until_node.location()),
            });
        }

        if let Some(for_node) = node.as_for_node() {
            // `for x in coll; …; end`. Lower the collection (a call can live
            // there) and the body. The index target is a write target, no call.
            let predicate = Some(self.lower_node(&for_node.collection()));
            let body = for_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
            return self.push(Node::Loop {
                predicate,
                body,
                span: span_of(&for_node.location()),
            });
        }

        if let Some(begin_node) = node.as_begin_node() {
            // `begin`/`rescue`/`else`/`ensure`. Collect every sub-body's calls.
            let mut body: Vec<NodeId> = begin_node
                .statements()
                .map(|s| self.lower_body(&s.body()))
                .unwrap_or_default();
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
                clauses.push(RescueClause {
                    exceptions,
                    body: clause_body,
                    bound_name,
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
                span: span_of(&and_node.location()),
            });
        }

        if let Some(or_node) = node.as_or_node() {
            let left = self.lower_node(&or_node.left());
            let right = self.lower_node(&or_node.right());
            return self.push(Node::Logical {
                left,
                right,
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
                return only;
            }
            return self.push(Node::BeginRescue {
                body,
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
            let value = self.lower_node(&cvw.value());
            return self.push(Node::VariableWrite {
                value,
                span: span_of(&cvw.location()),
            });
        }
        if let Some(gvw) = node.as_global_variable_write_node() {
            let value = self.lower_node(&gvw.value());
            return self.push(Node::VariableWrite {
                value,
                span: span_of(&gvw.location()),
            });
        }

        if let Some(ivr) = node.as_instance_variable_read_node() {
            return self.push(Node::VariableRead {
                span: span_of(&ivr.location()),
            });
        }
        if let Some(cvr) = node.as_class_variable_read_node() {
            return self.push(Node::VariableRead {
                span: span_of(&cvr.location()),
            });
        }
        if let Some(gvr) = node.as_global_variable_read_node() {
            return self.push(Node::VariableRead {
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
            // `:"sym#{x}"` — lower and KEEP the parts linked (a `Statements`
            // carrier, NOT InterpolatedString: a symbol must not type as String,
            // which would risk a `String#typo` false positive). The link keeps a
            // local read inside the interpolation visible to structural walks like
            // `flow.dead-assignment`; `Statements` itself types Dynamic.
            let parts: Vec<NodeId> = interp
                .parts()
                .iter()
                .map(|p| self.lower_node(&p))
                .collect();
            return self.push(Node::Statements {
                body: parts,
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
            return self.push(Node::Other { span });
        }
        let body: Vec<NodeId> = recovered.iter().map(|c| self.lower_node(c)).collect();
        self.push(Node::Statements { body, span })
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

    /// Harvest the `(name, body, has_explicit_return)` of every DIRECT instance
    /// method from the already-lowered class/module body's direct-child ids. A
    /// direct child that is a `Definition` with a `name` (i.e. an instance `def`
    /// — `def self.x` lowered to a name-less Definition) is a direct instance
    /// method; its lowered body and explicit-return flag are recorded for
    /// ADR-0023 tier-4b. Reads the arena read-only (the nodes already exist).
    fn harvest_method_bodies(&self, direct_children: &[NodeId]) -> Vec<MethodBody> {
        direct_children
            .iter()
            .filter_map(|&id| match &self.nodes[id.0 as usize] {
                Node::Definition {
                    name: Some(name),
                    has_explicit_return,
                    params,
                    body,
                    ..
                } => Some(MethodBody {
                    name: name.clone(),
                    body: body.clone(),
                    has_explicit_return: *has_explicit_return,
                    params: params.clone(),
                }),
                _ => None,
            })
            .collect()
    }
}

/// Decode a Prism `ConstantId` byte slice (a method / variable name) to an
/// owned `String`. Names are UTF-8 in practice; lossy decode keeps the walk
/// total on exotic encodings.
fn constant_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Parse a Ruby integer literal's source slice to its `i128` value for
/// duplicate-key IDENTITY (Ruby compares hash keys by value: `1.eql?(0x1)`).
/// Handles an optional sign, `0x`/`0o`/`0b`/`0d` radix prefixes (and the bare
/// `0NNN` octal + `0NN` … actually Ruby's leading-zero octal), and `_` digit
/// separators. Returns `None` for a value beyond `i128` (rare bignum key) or an
/// unparsable form, so the caller treats it as non-value-pinned (FP-safe: a
/// missed witness, never a false one).
fn parse_ruby_integer(raw: &str) -> Option<i128> {
    let s = raw.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (radix, digits) = if let Some(r) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        (16, r)
    } else if let Some(r) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
        (8, r)
    } else if let Some(r) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        (2, r)
    } else if let Some(r) = s.strip_prefix("0d").or_else(|| s.strip_prefix("0D")) {
        (10, r)
    } else if s.len() > 1 && s.starts_with('0') {
        (8, &s[1..]) // Ruby leading-zero octal (`0755`).
    } else {
        (10, s)
    };
    let cleaned: String = digits.chars().filter(|&c| c != '_').collect();
    if cleaned.is_empty() {
        return None;
    }
    let mag = i128::from_str_radix(&cleaned, radix).ok()?;
    Some(if neg { -mag } else { mag })
}

/// Ruby `String#inspect` for the duplicate-hash-key label of a STRING key. Wraps
/// in double quotes and escapes the characters Ruby escapes in a double-quoted
/// literal. Covers the ASCII forms that appear as hash-key literals; a byte-exact
/// match of Ruby's full Unicode escaping is out of scope (string keys with
/// control/non-ASCII bytes duplicated in one literal do not occur in the probe
/// matrix or realistic corpora).
fn ruby_inspect_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            '\x07' => out.push_str("\\a"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            '\x0b' => out.push_str("\\v"),
            '\x1b' => out.push_str("\\e"),
            '#' => out.push('#'),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The dotted constant-path name of a `class`/`module` declaration's path node:
/// `Point` -> `"Point"`, `Foo::Bar` -> `"Foo::Bar"`. A `ConstantReadNode` is the
/// bare-name case; a `ConstantPathNode` is the `A::B` case (recurse on parent).
/// Any other node (an unusual dynamic-constant form) yields an empty string,
/// which the SourceIndex treats as un-namable (no instance typing for it).
fn constant_path_string(node: &PrismNode<'_>) -> String {
    if let Some(cr) = node.as_constant_read_node() {
        return constant_string(cr.name().as_slice());
    }
    if let Some(cp) = node.as_constant_path_node() {
        // `name()` is the last component (`Bar` in `Foo::Bar`); the parent is the
        // scope. `::Foo` has no parent (top-level) — render just the name.
        let last = cp
            .name()
            .map(|n| constant_string(n.as_slice()))
            .unwrap_or_default();
        match cp.parent() {
            Some(parent) => {
                let head = constant_path_string(&parent);
                if head.is_empty() {
                    last
                } else {
                    format!("{head}::{last}")
                }
            }
            None => last,
        }
    } else {
        String::new()
    }
}

/// The name of a constant *reference* used as a superclass (`< Bar`,
/// `< Foo::Bar`): the **last** path component, since that is what the
/// source-superclass chain walk resolves against the SourceIndex / RBS by simple
/// name. Returns `None` for a non-constant superclass expression (e.g.
/// `< Struct.new(...)`), which leaves the chain deliberately open (unknown
/// ancestor ⇒ the conservative gate stays silent).
fn constant_node_name(node: &PrismNode<'_>) -> Option<String> {
    if let Some(cr) = node.as_constant_read_node() {
        return Some(constant_string(cr.name().as_slice()));
    }
    if let Some(cp) = node.as_constant_path_node() {
        return cp.name().map(|n| constant_string(n.as_slice()));
    }
    None
}

/// The instance-method names defined *directly* in a class/module body via
/// `def name`. Reads the Prism body before lowering (lowering drops a def's
/// name). Only direct, top-of-body defs are collected — methods inside nested
/// classes/conditionals are out of scope for this slice (and would be unsound to
/// attribute to the outer class). A singleton/`self.` def is excluded: it is not
/// an instance method, so it must not count toward instance-method existence.
fn direct_method_names(body: &PrismNode<'_>) -> Vec<String> {
    let mut names = Vec::new();
    let collect = |stmt: &PrismNode<'_>, names: &mut Vec<String>| {
        if let Some(def) = stmt.as_def_node() {
            // A def with a receiver (`def self.foo` / `def obj.foo`) is a
            // singleton method, NOT an instance method — exclude it.
            if def.receiver().is_none() {
                names.push(constant_string(def.name().as_slice()));
            }
        }
    };
    if let Some(stmts) = body.as_statements_node() {
        for stmt in stmts.body().iter() {
            collect(&stmt, &mut names);
        }
    } else {
        collect(body, &mut names);
    }
    names
}

/// ADR-35 slice 1 (`def.override-visibility-reduced`): discover a class/module
/// body's instance-method VISIBILITY table + its `include`/`prepend` ancestor
/// names, reading the Prism body BEFORE lowering. Mirrors
/// `scope_indexer.rb#build_discovered_method_visibilities` /
/// `collect_includes` EXACTLY (the witness set must stay ⊆ the reference's):
///
///   * the body is walked left-to-right with a running default that starts
///     [`Visibility::Public`];
///   * a bare `private` / `protected` / `public` call (receiver-less, NO args)
///     FLIPS the running default for subsequent `def`s;
///   * `private :foo, :bar` / `private "foo"` (literal symbol/string args ONLY)
///     BACK-PATCHES those named methods to that visibility WITHOUT changing the
///     running default; a non-literal arg in the list is ignored;
///   * a plain `def foo` (receiver-less) records `foo` at the running default;
///   * a `def foo` nested as an ARGUMENT to a modifier call (`private def foo`)
///     is recorded at the (unchanged) running default — NOT at the modifier's
///     visibility — exactly mirroring the reference's tracking gap;
///   * `include X` / `prepend X` collect `X`'s last path component (mirroring how
///     `superclass` is captured) for the MRO ancestor walk;
///   * a singleton def (`def self.x`) is EXCLUDED from the visibility table.
///
/// Back-patches are applied to the LAST recorded entry for a name (a reopened
/// `def` then `private :name` re-marks it), matching the reference's
/// last-write-wins accumulator.
fn discover_visibilities_and_includes(
    body: &PrismNode<'_>,
) -> (Vec<(String, Visibility)>, Vec<String>) {
    let mut vis: Vec<(String, Visibility)> = Vec::new();
    let mut includes: Vec<String> = Vec::new();
    let mut current = Visibility::Public;

    // A class/module body is a `StatementsNode` (or absent). Walk its direct
    // statements left-to-right; the order is what makes the running-default flow
    // correct. A bare (non-Statements) single-statement body is handled as one.
    if let Some(stmts) = body.as_statements_node() {
        for stmt in stmts.body().iter() {
            process_visibility_stmt(&stmt, &mut current, &mut vis, &mut includes);
        }
    } else {
        process_visibility_stmt(body, &mut current, &mut vis, &mut includes);
    }
    (vis, includes)
}

/// Apply one direct body statement to the running visibility default + the
/// discovered tables. See [`discover_visibilities_and_includes`] for the rules.
fn process_visibility_stmt(
    stmt: &PrismNode<'_>,
    current: &mut Visibility,
    vis: &mut Vec<(String, Visibility)>,
    includes: &mut Vec<String>,
) {
    // A receiver-less `def name` records at the running default.
    if let Some(def) = stmt.as_def_node() {
        if def.receiver().is_none() {
            vis.push((constant_string(def.name().as_slice()), *current));
        }
        return;
    }
    let Some(call) = stmt.as_call_node() else {
        return;
    };
    // Modifier / mixin calls only ever have an implicit-self receiver.
    if call.receiver().is_some() {
        return;
    }
    let name = constant_string(call.name().as_slice());
    if let Some(modifier) = visibility_of_modifier(&name) {
        let args = collect_call_args(&call);
        if args.is_empty() {
            // Bare modifier ⇒ flip the running default.
            *current = modifier;
        } else {
            // `private :foo, …` ⇒ back-patch the named methods (running default
            // unchanged for this form).
            for arg in &args {
                if let Some(target) = literal_symbol_or_string_name(arg) {
                    back_patch_visibility(vis, &target, modifier);
                }
            }
            // A `private def foo` arg records the nested def at the UNCHANGED
            // running default (the reference's tracking gap).
            record_nested_defs(&args, *current, vis);
        }
        return;
    }
    if name == "include" || name == "prepend" {
        for arg in &collect_call_args(&call) {
            // Capture the FULL written constant path (`Foo::Bar`, not just `Bar`)
            // so the override-visibility ancestor walk can resolve it against the
            // subclass's lexical nesting WITHOUT the name-collision merge that a
            // last-component-only name would cause (the gitlab-foss FP cluster).
            // A non-constant include arg yields an empty string ⇒ skipped.
            let path = constant_path_string(arg);
            if !path.is_empty() {
                includes.push(path);
            }
        }
    }
}

/// Map a receiver-less call name to its visibility, or `None` if it is not one
/// of the three modifiers.
fn visibility_of_modifier(name: &str) -> Option<Visibility> {
    match name {
        "public" => Some(Visibility::Public),
        "protected" => Some(Visibility::Protected),
        "private" => Some(Visibility::Private),
        _ => None,
    }
}

/// The positional arguments of a call as Prism nodes (empty if none).
fn collect_call_args<'pr>(call: &ruby_prism::CallNode<'pr>) -> Vec<PrismNode<'pr>> {
    call.arguments()
        .map(|a| a.arguments().iter().collect())
        .unwrap_or_default()
}

/// The literal method name a `private :foo` / `private "foo"` argument names, or
/// `None` for any non-literal (dynamic) argument — which the reference ignores.
fn literal_symbol_or_string_name(arg: &PrismNode<'_>) -> Option<String> {
    if let Some(sym) = arg.as_symbol_node() {
        return Some(String::from_utf8_lossy(sym.unescaped()).into_owned());
    }
    if let Some(s) = arg.as_string_node() {
        return Some(String::from_utf8_lossy(s.unescaped()).into_owned());
    }
    None
}

/// Re-mark the LAST recorded entry for `name` to `visibility` (last-write-wins,
/// matching the reference accumulator). No-op if the name was never recorded.
fn back_patch_visibility(vis: &mut [(String, Visibility)], name: &str, visibility: Visibility) {
    if let Some(slot) = vis.iter_mut().rev().find(|(n, _)| n == name) {
        slot.1 = visibility;
    }
}

/// Record any `def`s nested directly inside a modifier call's argument list
/// (`private def foo`) at the supplied running default. Mirrors the reference's
/// full-subtree recursion landing the inner def at the unchanged default.
fn record_nested_defs(
    args: &[PrismNode<'_>],
    current: Visibility,
    vis: &mut Vec<(String, Visibility)>,
) {
    for arg in args {
        if let Some(def) = arg.as_def_node() {
            if def.receiver().is_none() {
                vis.push((constant_string(def.name().as_slice()), current));
            }
        }
    }
}

/// The PLAIN-POSITIONAL parameter names of a `def` for ADR-0023 tier-4b
/// call-site PARAMETER BINDING, or `None` to DECLINE the method when its
/// signature has anything that breaks positional index<->argument alignment.
///
/// A method with NO parameters (`def f; ...; end`, Prism `parameters() == None`)
/// returns `Some([])` — there is nothing to bind, and the param-INDEPENDENT
/// inference still applies; the call-site binder just never reads an arg.
///
/// We accept ONLY `requireds` (the leading `x, y` positionals). Any of the
/// following makes the method decline (return `None`), because the call-site
/// binder maps positional ARG index -> positional PARAM index 1:1 and these
/// break that alignment:
///   * `optionals` — `def f(x = 1)`: a defaulted param may be filled by the
///     default (no arg) so arg index N need not be param N.
///   * `rest` — `*args`: a splat absorbs a variable arg count.
///   * `posts` — a positional AFTER a splat (`def f(*a, z)`): its arg index
///     depends on the splat length.
///   * `keywords` / `keyword_rest` — `k:`, `**opts`: keyword args are not
///     positional.
///   * `block` — `&blk`: a block param is not a positional arg.
fn plain_positional_params(params: Option<&ruby_prism::ParametersNode<'_>>) -> Option<Vec<String>> {
    let Some(params) = params else {
        // No parameter list at all ⇒ zero plain positionals (bindable, no-op).
        return Some(Vec::new());
    };
    // Decline on ANY non-plain-positional construct (conservative; a decline is
    // never a false positive — only a missed witness).
    if params.optionals().iter().next().is_some()
        || params.rest().is_some()
        || params.posts().iter().next().is_some()
        || params.keywords().iter().next().is_some()
        || params.keyword_rest().is_some()
        || params.block().is_some()
    {
        return None;
    }
    // Every required must be a simple named `RequiredParameterNode`. A
    // destructuring positional (`def f((a, b))`) is a `MultiTargetNode`, which
    // has no single name and breaks the 1:1 mapping ⇒ decline.
    let mut names = Vec::new();
    for req in params.requireds().iter() {
        let rp = req.as_required_parameter_node()?;
        names.push(constant_string(rp.name().as_slice()));
    }
    Some(names)
}

/// Capture the full RBS-relevant [`ParamShape`] from a Prism `ParametersNode`
/// (for `sig-gen`'s `initialize` stub). Mirrors the inputs the reference's
/// `render_initialize_param_list` reads — requireds/optionals counts, rest,
/// keyword `(name, optional)` in order, keyword-rest, block. POSTS are omitted
/// because the reference's renderer drops them.
fn param_shape_of(params: Option<&ruby_prism::ParametersNode<'_>>) -> ParamShape {
    let Some(p) = params else {
        return ParamShape::default();
    };
    let mut keywords = Vec::new();
    for kw in p.keywords().iter() {
        if let Some(req) = kw.as_required_keyword_parameter_node() {
            keywords.push((constant_string(req.name().as_slice()), false));
        } else if let Some(opt) = kw.as_optional_keyword_parameter_node() {
            keywords.push((constant_string(opt.name().as_slice()), true));
        }
    }
    ParamShape {
        required: p.requireds().iter().count(),
        optional: p.optionals().iter().count(),
        has_rest: p.rest().is_some(),
        keywords,
        has_kwrest: p.keyword_rest().is_some(),
        has_block: p.block().is_some(),
    }
}

/// Whether a Prism `def` body contains an explicit `return` statement ANYWHERE
/// (ADR-0023 tier-4b decline gate). We only infer a return type from the body's
/// TAIL expression; an explicit `return` could carry a different type on another
/// path (the reference unions explicit returns + the tail — we take only the
/// tail), so the presence of ANY `return` makes us decline. A `ReturnVisitor`
/// walks the whole subtree (the default `Visit` recursion) and trips on the
/// first `ReturnNode`. A return nested inside a block/lambda/inner def also trips
/// it — conservatively safe (decline is never a false positive).
fn body_has_explicit_return(body: &PrismNode<'_>) -> bool {
    use ruby_prism::Visit;
    struct ReturnVisitor {
        found: bool,
    }
    impl<'pr> Visit<'pr> for ReturnVisitor {
        fn visit_return_node(&mut self, _node: &ruby_prism::ReturnNode<'pr>) {
            self.found = true;
            // No need to recurse further once found.
        }
    }
    let mut v = ReturnVisitor { found: false };
    v.visit(body);
    v.found
}

/// Convert a Prism `Location` to a byte-offset [`Span`].
fn span_of(loc: &ruby_prism::Location<'_>) -> Span {
    (loc.start_offset(), loc.end_offset())
}

/// Collect the OUTERMOST "recoverable" descendant Prism nodes of an unhandled
/// node — a local read / write / operator-write / call — WITHOUT descending past
/// one (so [`Builder::lower_node`] recurses into it once, normally). Used by the
/// catch-all to recover reads/calls buried under a wrapper Prism node that has no
/// owned variant (`return`, `super`, `*splat`, …), keeping them visible to
/// structural walks like `flow.dead-assignment` and the call rules.
///
/// We deliberately do NOT collect a `def`/`class`/`module` here: those are not
/// found inside expression wrappers in practice, and recovering one flatly (no
/// owned `Definition`) would confuse the dead-assignment nested-unit barrier.
fn collect_recoverable_children<'pr>(node: &PrismNode<'pr>) -> Vec<PrismNode<'pr>> {
    use ruby_prism::Visit;
    struct Collector<'a, 'pr> {
        out: &'a mut Vec<PrismNode<'pr>>,
    }
    // Override each recoverable node type to RECORD it and stop (do not recurse —
    // `lower_node` will recurse into it once, normally). Every OTHER node type
    // keeps the trait's default recursion, so we descend through the unhandled
    // wrapper(s) until we reach the outermost recoverable nodes. This guarantees
    // each recoverable node is collected exactly once (no double-lowering, which
    // for a call would otherwise mint a duplicate diagnostic).
    impl<'pr> Visit<'pr> for Collector<'_, 'pr> {
        fn visit_local_variable_read_node(
            &mut self,
            node: &ruby_prism::LocalVariableReadNode<'pr>,
        ) {
            self.out.push(node.as_node());
        }
        fn visit_local_variable_write_node(
            &mut self,
            node: &ruby_prism::LocalVariableWriteNode<'pr>,
        ) {
            self.out.push(node.as_node());
        }
        fn visit_local_variable_operator_write_node(
            &mut self,
            node: &ruby_prism::LocalVariableOperatorWriteNode<'pr>,
        ) {
            self.out.push(node.as_node());
        }
        fn visit_local_variable_and_write_node(
            &mut self,
            node: &ruby_prism::LocalVariableAndWriteNode<'pr>,
        ) {
            self.out.push(node.as_node());
        }
        fn visit_local_variable_or_write_node(
            &mut self,
            node: &ruby_prism::LocalVariableOrWriteNode<'pr>,
        ) {
            self.out.push(node.as_node());
        }
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            self.out.push(node.as_node());
        }
    }
    let mut out = Vec::new();
    let mut c = Collector { out: &mut out };
    // Visit the wrapper's CHILDREN (not the wrapper itself), so we don't re-handle
    // the unhandled root. The default `visit` dispatches the root to its own
    // (non-overridden) per-type method, which recurses into children — exactly
    // what we want for the root wrapper.
    c.visit(node);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowers_assignment_and_call_with_precise_spans() {
        let src = b"s = \"Hello\"\ns.lenght\n";
        let result = crate::parse(src);
        let ast = lower(&result);

        // Find the single Call node and assert its method + message span maps
        // back to `lenght` in the source.
        let call = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::Call { method, message_span, .. } => {
                    Some((method.clone(), *message_span))
                }
                _ => None,
            })
            .expect("expected a Call node");
        assert_eq!(call.0, "lenght");
        let (start, end) = call.1;
        assert_eq!(&src[start..end], b"lenght");
    }

    #[test]
    fn lowers_local_write_and_string_literal() {
        let src = b"s = \"Hello\"\n";
        let result = crate::parse(src);
        let ast = lower(&result);

        let has_write = ast.iter().any(|(_, n)| {
            matches!(n, Node::LocalVariableWrite { name, .. } if name == "s")
        });
        let has_str = ast.iter().any(|(_, n)| {
            matches!(n, Node::StringLit { value, .. } if value == "Hello")
        });
        assert!(has_write, "expected a LocalVariableWrite for `s`");
        assert!(has_str, "expected a StringLit \"Hello\"");
    }

    #[test]
    fn lowers_operator_and_or_writes_to_op_write_variant() {
        // `x += 1`, `y ||= 2`, `z &&= w` all lower to LocalVariableOpWrite with
        // their target name preserved (so the dead-assignment walk sees the
        // implicit read). Each must lower its assigned value for reachability.
        for (src, name) in [
            (&b"x = 0\nx += 1\n"[..], "x"),
            (&b"y = 0\ny ||= 2\n"[..], "y"),
            (&b"z = 0\nz &&= 3\n"[..], "z"),
        ] {
            let ast = lower(&crate::parse(src));
            let found = ast.iter().any(|(_, n)| {
                matches!(n, Node::LocalVariableOpWrite { name: nm, .. } if nm == name)
            });
            assert!(found, "expected LocalVariableOpWrite for `{name}` in {src:?}");
        }
    }

    #[test]
    fn local_write_records_name_span() {
        // The name_span anchors on the NAME token only (`result`), not the whole
        // `result = 1` — mirroring the reference's `from_name_loc`.
        let src = b"result = 1\n";
        let ast = lower(&crate::parse(src));
        let name_span = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::LocalVariableWrite { name, name_span, .. } if name == "result" => {
                    Some(*name_span)
                }
                _ => None,
            })
            .expect("expected a LocalVariableWrite for `result`");
        assert_eq!(&src[name_span.0..name_span.1], b"result");
    }

    #[test]
    fn lowers_interpolated_string_to_node() {
        // `"a#{x}b"` lowers to an InterpolatedString whose parts are non-empty
        // (the `#{x}` segment is lowered, keeping its calls reachable).
        let src = b"\"a#{x}b\"\n";
        let result = crate::parse(src);
        let ast = lower(&result);

        let parts = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::InterpolatedString { parts, .. } => Some(parts.clone()),
                _ => None,
            })
            .expect("expected an InterpolatedString node");
        assert!(!parts.is_empty(), "expected non-empty interpolation parts");
    }

    #[test]
    fn lowers_call_positional_arguments() {
        // `s.include?("e", "x")` lowers two positional string-literal args, in
        // source order, as children of the Call node.
        let src = b"s = \"Hello\"\ns.include?(\"e\", \"x\")\n";
        let result = crate::parse(src);
        let ast = lower(&result);

        let args = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::Call { method, args, .. } if method == "include?" => Some(args.clone()),
                _ => None,
            })
            .expect("expected an include? Call node");
        assert_eq!(args.len(), 2, "expected two positional args");
        let vals: Vec<String> = args
            .iter()
            .map(|id| match ast.get(*id) {
                Node::StringLit { value, .. } => value.clone(),
                other => panic!("expected StringLit arg, got {other:?}"),
            })
            .collect();
        assert_eq!(vals, vec!["e".to_string(), "x".to_string()]);
    }

    #[test]
    fn lowers_nil_true_false_symbol_float_literals() {
        let src = b"nil\ntrue\nfalse\n:foo\n3.5\n";
        let result = crate::parse(src);
        let ast = lower(&result);
        assert!(ast.iter().any(|(_, n)| matches!(n, Node::NilLit { .. })));
        assert!(ast.iter().any(|(_, n)| matches!(n, Node::TrueLit { .. })));
        assert!(ast.iter().any(|(_, n)| matches!(n, Node::FalseLit { .. })));
        assert!(ast
            .iter()
            .any(|(_, n)| matches!(n, Node::SymbolLit { value, .. } if value == "foo")));
        assert!(ast
            .iter()
            .any(|(_, n)| matches!(n, Node::FloatLit { value, .. } if (*value - 3.5).abs() < f64::EPSILON)));
    }

    #[test]
    fn lowering_is_total_for_unhandled_nodes() {
        // A construct still outside the owned subset (a `yield`) must lower
        // without panicking, landing in `Other`.
        let src = b"def foo; yield; end\n";
        let result = crate::parse(src);
        let ast = lower(&result);
        assert!(!ast.is_empty());
        assert!(ast.iter().any(|(_, n)| matches!(n, Node::Other { .. })));
    }

    /// True iff the arena contains a `Call` to `method`.
    fn has_call(ast: &LoweredAst, method: &str) -> bool {
        ast.iter()
            .any(|(_, n)| matches!(n, Node::Call { method: m, .. } if m == method))
    }

    #[test]
    fn lowers_call_inside_method_def() {
        // A call in a `def` body must reach the arena (the whole point).
        let src = b"def slug(t)\n  t.downcase\nend\n";
        let ast = lower(&crate::parse(src));
        assert!(
            ast.iter().any(|(_, n)| matches!(n, Node::Definition { .. })),
            "expected a Definition node for the def"
        );
        assert!(has_call(&ast, "downcase"), "call inside def must be lowered");
    }

    #[test]
    fn lowers_calls_inside_if_and_else_branches() {
        let src = b"if x\n  a.foo\nelse\n  b.bar\nend\n";
        let ast = lower(&crate::parse(src));
        assert!(ast.iter().any(|(_, n)| matches!(n, Node::If { .. })));
        assert!(has_call(&ast, "foo"), "then-branch call must be lowered");
        assert!(has_call(&ast, "bar"), "else-branch call must be lowered");
    }

    #[test]
    fn lowers_if_and_unless_keyword_distinctly() {
        // The `is_unless` flag must survive lowering: Prism keeps `IfNode` and
        // `UnlessNode` distinct, and `flow.unreachable-branch` relies on the
        // keyword to decide which branch a literal predicate kills. Both keywords
        // must also preserve BOTH branches (then + else).
        let if_ast = lower(&crate::parse(b"if x\n  a.foo\nelse\n  b.bar\nend\n"));
        let (_, if_node) = if_ast
            .iter()
            .find(|(_, n)| matches!(n, Node::If { .. }))
            .expect("if must lower to a Node::If");
        match if_node {
            Node::If { is_unless, then_body, else_body, .. } => {
                assert!(!is_unless, "`if` must lower with is_unless == false");
                assert!(!then_body.is_empty(), "then branch preserved");
                assert!(!else_body.is_empty(), "else branch preserved");
            }
            _ => unreachable!(),
        }

        let unless_ast = lower(&crate::parse(b"unless x\n  a.foo\nelse\n  b.bar\nend\n"));
        let (_, unless_node) = unless_ast
            .iter()
            .find(|(_, n)| matches!(n, Node::If { .. }))
            .expect("unless must lower to a Node::If");
        match unless_node {
            Node::If { is_unless, then_body, else_body, .. } => {
                assert!(is_unless, "`unless` must lower with is_unless == true");
                assert!(!then_body.is_empty(), "then (unless body) preserved");
                assert!(!else_body.is_empty(), "else branch preserved");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn lowers_calls_inside_case_when_branches() {
        let src = b"case v\nwhen 1\n  a.foo\nwhen 2\n  b.bar\nelse\n  c.baz\nend\n";
        let ast = lower(&crate::parse(src));
        assert!(ast.iter().any(|(_, n)| matches!(n, Node::Case { .. })));
        assert!(has_call(&ast, "foo"));
        assert!(has_call(&ast, "bar"));
        assert!(has_call(&ast, "baz"));
    }

    #[test]
    fn lowers_calls_inside_loops_and_begin_rescue() {
        let w = lower(&crate::parse(b"while x\n  a.foo\nend\n"));
        assert!(w.iter().any(|(_, n)| matches!(n, Node::Loop { .. })));
        assert!(has_call(&w, "foo"));

        let b = lower(&crate::parse(b"begin\n  a.foo\nrescue => e\n  b.bar\nensure\n  c.baz\nend\n"));
        assert!(b.iter().any(|(_, n)| matches!(n, Node::BeginRescue { .. })));
        assert!(has_call(&b, "foo"));
        assert!(has_call(&b, "bar"));
        assert!(has_call(&b, "baz"));
    }

    #[test]
    fn lowers_call_inside_block_body() {
        // `[1,2].each { |n| n.foo }` — the block's inner call must be lowered.
        let src = b"[1, 2].each { |n| n.foo }\n";
        let ast = lower(&crate::parse(src));
        assert!(has_call(&ast, "foo"), "block-body call must be lowered");
        // The outer `each` call carries the block body ids.
        let has_block = ast.iter().any(|(_, n)| {
            matches!(n, Node::Call { method, block_body, .. } if method == "each" && !block_body.is_empty())
        });
        assert!(has_block, "the each call should record its block body");
    }

    #[test]
    fn safe_nav_flag_distinguishes_dot_from_amp_dot() {
        // `x&.foo` lowers with safe_nav: true; `x.foo` with safe_nav: false.
        let safe = lower(&crate::parse(b"x&.foo\n"));
        let safe_flag = safe.iter().find_map(|(_, n)| match n {
            Node::Call { method, safe_nav, .. } if method == "foo" => Some(*safe_nav),
            _ => None,
        });
        assert_eq!(safe_flag, Some(true), "x&.foo must lower safe_nav: true");

        let plain = lower(&crate::parse(b"x.foo\n"));
        let plain_flag = plain.iter().find_map(|(_, n)| match n {
            Node::Call { method, safe_nav, .. } if method == "foo" => Some(*safe_nav),
            _ => None,
        });
        assert_eq!(plain_flag, Some(false), "x.foo must lower safe_nav: false");
    }

    #[test]
    fn lowers_array_and_hash_literals() {
        let a = lower(&crate::parse(b"[1, 2, 3]\n"));
        assert!(a.iter().any(|(_, n)| matches!(n, Node::ArrayLit { .. })));
        let h = lower(&crate::parse(b"{ a: 1, b: 2 }\n"));
        assert!(h.iter().any(|(_, n)| matches!(n, Node::HashLit { .. })));
    }

    #[test]
    fn lowers_call_inside_keyword_hash_value() {
        // Bare keyword args wrap a KeywordHashNode; the value call must be lowered.
        let src = b"foo(wait: 30.minutes)\n";
        let ast = lower(&crate::parse(src));
        assert!(
            has_call(&ast, "minutes"),
            "keyword-hash value call must be lowered"
        );
    }

    #[test]
    fn lowers_calls_inside_parenthesized_range_bounds() {
        // `(30.seconds)..(10.minutes)` — both parenthesized bounds must be reachable.
        let src = b"x = (30.seconds)..(10.minutes)\n";
        let ast = lower(&crate::parse(src));
        assert!(has_call(&ast, "seconds"), "parenthesized left-bound call must be lowered");
        assert!(has_call(&ast, "minutes"), "parenthesized right-bound call must be lowered");
    }

    #[test]
    fn lowers_logical_operands() {
        // Both sides of `&&` must be reachable.
        let src = b"a.foo && b.bar\n";
        let ast = lower(&crate::parse(src));
        assert!(ast.iter().any(|(_, n)| matches!(n, Node::Logical { .. })));
        assert!(has_call(&ast, "foo"));
        assert!(has_call(&ast, "bar"));
    }

    #[test]
    fn lowers_ivar_and_constant_writes_recursively() {
        // The assigned value's call must be lowered.
        let iv = lower(&crate::parse(b"@x = a.foo\n"));
        assert!(iv.iter().any(
            |(_, n)| matches!(n, Node::InstanceVariableWrite { name, .. } if name == "@x")
        ));
        assert!(has_call(&iv, "foo"));
        let cw = lower(&crate::parse(b"FOO = a.bar\n"));
        assert!(cw.iter().any(|(_, n)| matches!(n, Node::ConstantWrite { .. })));
        assert!(has_call(&cw, "bar"));
    }

    #[test]
    fn lowers_call_inside_string_interpolation() {
        let src = b"x = \"hi #{a.foo}\"\n";
        let ast = lower(&crate::parse(src));
        assert!(has_call(&ast, "foo"), "interpolated call must be lowered");
    }

    /// Locate the single `ClassDef` and return its (name, superclass, methods).
    fn class_def(ast: &LoweredAst) -> (String, Option<String>, Vec<String>) {
        ast.iter()
            .find_map(|(_, n)| match n {
                Node::ClassDef { name, superclass, methods, .. } => {
                    Some((name.clone(), superclass.clone(), methods.clone()))
                }
                _ => None,
            })
            .expect("expected a ClassDef node")
    }

    #[test]
    fn lowers_class_def_name_super_and_methods() {
        // `class Point; def x; end; def y; end; end` — name "Point", no super,
        // instance methods [x, y]. The body's calls still reach the arena.
        let src = b"class Point\n  def x\n    1\n  end\n  def y\n    @a.foo\n  end\nend\n";
        let ast = lower(&crate::parse(src));
        let (name, sup, methods) = class_def(&ast);
        assert_eq!(name, "Point");
        assert_eq!(sup, None);
        assert_eq!(methods, vec!["x".to_string(), "y".to_string()]);
        // A call inside a method body is still lowered (reachability preserved).
        assert!(has_call(&ast, "foo"), "call inside def body must be lowered");
    }

    #[test]
    fn lowers_class_def_superclass_name() {
        // `class User < ApplicationRecord; end` — superclass recorded as the
        // simple last-component name.
        let ast = lower(&crate::parse(b"class User < ApplicationRecord\nend\n"));
        let (name, sup, _) = class_def(&ast);
        assert_eq!(name, "User");
        assert_eq!(sup.as_deref(), Some("ApplicationRecord"));
    }

    #[test]
    fn lowers_namespaced_class_name_and_super_path() {
        // `class Foo::Bar < Base::Thing; end` — dotted name, super last comp.
        let ast = lower(&crate::parse(b"class Foo::Bar < Base::Thing\nend\n"));
        let (name, sup, _) = class_def(&ast);
        assert_eq!(name, "Foo::Bar");
        assert_eq!(sup.as_deref(), Some("Thing"));
    }

    #[test]
    fn singleton_def_is_not_an_instance_method() {
        // `def self.make` is a singleton method — it must NOT be collected as an
        // instance method (else `X.new.make` would wrongly look defined).
        let ast = lower(&crate::parse(b"class C\n  def self.make\n  end\n  def go\n  end\nend\n"));
        let (_, _, methods) = class_def(&ast);
        assert_eq!(methods, vec!["go".to_string()]);
    }

    #[test]
    fn reopened_class_lowers_two_class_defs() {
        // Two `class C` bodies lower to two ClassDef nodes; the SourceIndex
        // unions them (tested in rigor-infer). Here we just assert both appear.
        let ast = lower(&crate::parse(b"class C\n  def a\n  end\nend\nclass C\n  def b\n  end\nend\n"));
        let defs: Vec<_> = ast
            .iter()
            .filter_map(|(_, n)| match n {
                Node::ClassDef { name, methods, .. } => Some((name.clone(), methods.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0], ("C".to_string(), vec!["a".to_string()]));
        assert_eq!(defs[1], ("C".to_string(), vec!["b".to_string()]));
    }

    /// The `method_bodies` of the single ClassDef in `ast`.
    fn class_method_bodies(ast: &LoweredAst) -> Vec<MethodBody> {
        ast.iter()
            .find_map(|(_, n)| match n {
                Node::ClassDef { method_bodies, .. } => Some(method_bodies.clone()),
                _ => None,
            })
            .expect("expected a ClassDef node")
    }

    #[test]
    fn harvests_method_body_with_name() {
        // `def full_name; "#{first} #{last}"; end` — harvested as ("full_name",
        // <non-empty body>, has_explicit_return=false).
        let src = b"class User\n  def full_name\n    \"#{first} #{last}\"\n  end\nend\n";
        let ast = lower(&crate::parse(src));
        let mbs = class_method_bodies(&ast);
        assert_eq!(mbs.len(), 1);
        assert_eq!(mbs[0].name, "full_name");
        assert!(!mbs[0].body.is_empty(), "body ids must be captured");
        assert!(!mbs[0].has_explicit_return);
    }

    #[test]
    fn harvest_excludes_singleton_def() {
        // `def self.make` is a singleton method — not harvested as a tier-4b body.
        let src = b"class C\n  def self.make\n    1\n  end\n  def go\n    2\n  end\nend\n";
        let ast = lower(&crate::parse(src));
        let mbs = class_method_bodies(&ast);
        let names: Vec<_> = mbs.iter().map(|m| m.name.clone()).collect();
        assert_eq!(names, vec!["go".to_string()]);
    }

    #[test]
    fn harvest_excludes_nested_and_conditional_defs() {
        // A def inside an `if` and a def inside an inner class are NOT direct
        // children of the outer class body, so they are not harvested for it.
        let src = b"class Outer\n  def direct\n    1\n  end\n  if cond\n    def conditional\n      2\n    end\n  end\n  class Inner\n    def nested\n      3\n    end\n  end\nend\n";
        let ast = lower(&crate::parse(src));
        // Locate the OUTER ClassDef (name "Outer") specifically.
        let mbs = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::ClassDef { name, method_bodies, .. } if name == "Outer" => {
                    Some(method_bodies.clone())
                }
                _ => None,
            })
            .expect("expected an Outer ClassDef");
        let names: Vec<_> = mbs.iter().map(|m| m.name.clone()).collect();
        assert_eq!(names, vec!["direct".to_string()]);
    }

    #[test]
    fn harvest_records_has_explicit_return() {
        // A body with `return` is flagged; a tail-only body is not.
        let with = lower(&crate::parse(
            b"class C\n  def m\n    return 1 if x\n    2\n  end\nend\n",
        ));
        assert!(class_method_bodies(&with)[0].has_explicit_return);
        let without = lower(&crate::parse(b"class C\n  def m\n    1\n  end\nend\n"));
        assert!(!class_method_bodies(&without)[0].has_explicit_return);
    }

    #[test]
    fn lowers_module_def_name_and_methods() {
        let ast = lower(&crate::parse(b"module M\n  def helper\n  end\nend\n"));
        let (name, methods) = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::ModuleDef { name, methods, .. } => Some((name.clone(), methods.clone())),
                _ => None,
            })
            .expect("expected a ModuleDef node");
        assert_eq!(name, "M");
        assert_eq!(methods, vec!["helper".to_string()]);
    }

    // --- ADR-35 slice 1: visibility-table + include discovery ----------------

    /// The `(method_visibilities, includes)` of the single ClassDef in `ast`.
    fn class_vis_includes(
        ast: &LoweredAst,
    ) -> (Vec<(String, Visibility)>, Vec<String>) {
        ast.iter()
            .find_map(|(_, n)| match n {
                Node::ClassDef { method_visibilities, includes, .. } => {
                    Some((method_visibilities.clone(), includes.clone()))
                }
                _ => None,
            })
            .expect("expected a ClassDef node")
    }

    #[test]
    fn discovers_bare_modifier_flips_running_default() {
        // `def a` is public; after a bare `private`, `def b` is private; a
        // subsequent bare `public` makes `def c` public again.
        let src = b"class C\n  def a\n  end\n  private\n  def b\n  end\n  public\n  def c\n  end\nend\n";
        let ast = lower(&crate::parse(src));
        let (vis, _) = class_vis_includes(&ast);
        assert_eq!(
            vis,
            vec![
                ("a".to_string(), Visibility::Public),
                ("b".to_string(), Visibility::Private),
                ("c".to_string(), Visibility::Public),
            ]
        );
    }

    #[test]
    fn discovers_named_arg_back_patch() {
        // `private :foo` back-patches an already-recorded `foo` to private,
        // leaving the running default (and `bar`) public.
        let src = b"class C\n  def foo\n  end\n  def bar\n  end\n  private :foo\nend\n";
        let ast = lower(&crate::parse(src));
        let (vis, _) = class_vis_includes(&ast);
        assert_eq!(
            vis,
            vec![
                ("foo".to_string(), Visibility::Private),
                ("bar".to_string(), Visibility::Public),
            ]
        );
    }

    #[test]
    fn discovers_string_arg_back_patch() {
        // `protected "foo"` (a string literal arg) marks `foo` protected.
        let src = b"class C\n  def foo\n  end\n  protected \"foo\"\nend\n";
        let ast = lower(&crate::parse(src));
        let (vis, _) = class_vis_includes(&ast);
        assert_eq!(vis, vec![("foo".to_string(), Visibility::Protected)]);
    }

    #[test]
    fn private_def_modifier_records_at_default_not_private() {
        // `private def foo; end` — the wrap-around form is NOT tracked as a
        // visibility change: `foo` records at the running default (Public),
        // mirroring the reference gap (keeps the witness set ⊆ reference's).
        let src = b"class C\n  private def foo\n  end\nend\n";
        let ast = lower(&crate::parse(src));
        let (vis, _) = class_vis_includes(&ast);
        assert_eq!(vis, vec![("foo".to_string(), Visibility::Public)]);
    }

    #[test]
    fn discovers_include_and_prepend_full_path() {
        // `include Foo::Bar` / `prepend Baz` collect the FULL written constant
        // path (so the override walk can resolve against lexical nesting).
        let src = b"class C\n  include Foo::Bar\n  prepend Baz\n  def a\n  end\nend\n";
        let ast = lower(&crate::parse(src));
        let (_, includes) = class_vis_includes(&ast);
        assert_eq!(includes, vec!["Foo::Bar".to_string(), "Baz".to_string()]);
    }

    #[test]
    fn singleton_def_excluded_from_visibility_table() {
        // `def self.x` is a singleton method — never in the visibility table.
        let src = b"class C\n  private\n  def self.x\n  end\n  def y\n  end\nend\n";
        let ast = lower(&crate::parse(src));
        let (vis, _) = class_vis_includes(&ast);
        // Only the instance method `y` (at the running private default) appears.
        assert_eq!(vis, vec![("y".to_string(), Visibility::Private)]);
    }

    #[test]
    fn module_discovers_visibility_and_includes() {
        // The ModuleDef carries the same tables.
        let src = b"module M\n  include Helper\n  def a\n  end\n  private\n  def b\n  end\nend\n";
        let ast = lower(&crate::parse(src));
        let (vis, includes) = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::ModuleDef { method_visibilities, includes, .. } => {
                    Some((method_visibilities.clone(), includes.clone()))
                }
                _ => None,
            })
            .expect("expected a ModuleDef node");
        assert_eq!(
            vis,
            vec![
                ("a".to_string(), Visibility::Public),
                ("b".to_string(), Visibility::Private),
            ]
        );
        assert_eq!(includes, vec!["Helper".to_string()]);
    }

    #[test]
    fn definition_records_name_span_on_name_token() {
        // The `Definition` node anchors `name_span` on the method-NAME token.
        let src = b"def foo\nend\n";
        let ast = lower(&crate::parse(src));
        let name_span = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::Definition { name: Some(nm), name_span: Some(sp), .. } if nm == "foo" => {
                    Some(*sp)
                }
                _ => None,
            })
            .expect("expected a named Definition");
        assert_eq!(&src[name_span.0..name_span.1], b"foo");
    }
}
