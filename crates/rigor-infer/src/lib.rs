//! The inference engine (ADR-0004/0005): flow-sensitive inference, narrowing,
//! RBS method-type translation, typed dispatch. Pure query functions take the
//! db explicitly (ADR-0006 — Salsa-ready, not Salsa-bound). Constant folding
//! splits between a conservative Rust core and the cached Ruby sidecar
//! (ADR-0008); foldability is decided here from an embedded catalogue.
//!
//! ## Tracer-bullet expression typer
//!
//! This slice ships the smallest [`type_of`] able to type the *receiver* of a
//! call: string/integer literals fold to value-pinned `Constant` carriers, a
//! local read is resolved from a flat [`TypeEnv`] populated as statements are
//! walked in order, and everything else degrades to `Dynamic[top]` (ADR-0023
//! tier-5 fallback). The pure-function-dispatched-by-node-variant shape mirrors
//! the reference's `ExpressionTyper` (ADR-0023).
//!
// TODO(spec): flow sensitivity, narrowing, the full dispatch tier cascade
// (folding -> shape -> RBS -> in-source -> Dynamic) and budgets (ADR-0023/0024).
#![allow(dead_code)]

pub mod folding;
pub mod kernel_fold;
pub mod multi_target_binder;
pub mod source_index;

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use rigor_index::{ClassOrdering, CoreIndex};
use rigor_parse::{LoweredAst, Node, NodeId};
use rigor_types::{Interner, Scalar, ShapeKey, ShapeMember, Type, TypeId};

pub use folding::RubyFolder;
pub use source_index::{
    lexical_scopes, method_body_spans, ConstLit, DefKind, ParamBoundReturn, SourceIndex,
    SOURCE_CLASS_BASE,
};

/// A process-wide empty [`SourceIndex`], used as the default `source` for a
/// [`Typer`] built via [`Typer::new`] (callers that predate in-source typing).
/// Sharing one empty index keeps `Typer::new` allocation-free and infallible.
fn empty_source() -> &'static SourceIndex {
    static EMPTY: OnceLock<SourceIndex> = OnceLock::new();
    EMPTY.get_or_init(SourceIndex::default)
}

/// The value-pinned scalar key a hash-literal key NODE carries, or `None` when
/// the key is dynamic (a computed expression, an interpolated string, a
/// constant, a local, …). A faithful port of the reference `static_hash_key`:
/// the accepted set is Symbol / String / Integer / Float / true / false / nil
/// (`HashShape::ALLOWED_KEY_CLASSES`). Floats key by raw bits so `1.0` == `1.00`
/// while `1` (an `Int`) stays a distinct key.
fn static_shape_key_of_node(node: &Node) -> Option<ShapeKey> {
    match node {
        Node::SymbolLit { value, .. } => Some(ShapeKey::Sym(value.clone())),
        Node::StringLit { value, .. } => Some(ShapeKey::Str(value.clone())),
        Node::IntegerLit { value, .. } => Some(ShapeKey::Int(*value)),
        Node::FloatLit { value, .. } => Some(ShapeKey::Float(value.to_bits())),
        Node::TrueLit { .. } => Some(ShapeKey::Bool(true)),
        Node::FalseLit { .. } => Some(ShapeKey::Bool(false)),
        Node::NilLit { .. } => Some(ShapeKey::Nil),
        _ => None,
    }
}

/// The [`ShapeKey`] a value-pinned [`Scalar`] denotes when used as a hash key.
/// Every rigor-rs `Scalar` is a valid `HashShape` key (they are exactly the
/// reference's `ALLOWED_KEY_CLASSES`), so this is total — used by the projection
/// tier to resolve a `Constant`-typed argument to a lookup key and by `invert`
/// to key on a member's value.
fn scalar_to_shape_key(s: &Scalar) -> ShapeKey {
    match s {
        Scalar::Sym(v) => ShapeKey::Sym(v.clone()),
        Scalar::Str(v) => ShapeKey::Str(v.clone()),
        Scalar::Int(v) => ShapeKey::Int(*v),
        Scalar::Float(f) => ShapeKey::Float(f.to_bits()),
        Scalar::Bool(b) => ShapeKey::Bool(*b),
        Scalar::Nil => ShapeKey::Nil,
    }
}

/// The [`Scalar`] a [`ShapeKey`] denotes — the inverse of [`scalar_to_shape_key`],
/// used by `HashShape#invert` to turn an original key back into a `Constant`
/// value. `None` for the `Other` fallback (never built from a literal), so a
/// projection that reaches it declines.
fn shape_key_to_scalar(k: &ShapeKey) -> Option<Scalar> {
    Some(match k {
        ShapeKey::Sym(v) => Scalar::Sym(v.clone()),
        ShapeKey::Str(v) => Scalar::Str(v.clone()),
        ShapeKey::Int(v) => Scalar::Int(*v),
        ShapeKey::Float(bits) => Scalar::Float(f64::from_bits(*bits)),
        ShapeKey::Bool(b) => Scalar::Bool(*b),
        ShapeKey::Nil => Scalar::Nil,
        ShapeKey::Other => return None,
    })
}

/// A flat name -> type binding environment, populated by `LocalVariableWrite`
/// as the statement sequence is walked in order. Intentionally not
/// flow-sensitive in this slice.
pub type TypeEnv = HashMap<String, TypeId>;

/// Constants whose `.new`/`.define` returns a CLASS, not a plain instance of the
/// named class: `Struct.new(...)` and `Data.define(...)` build an anonymous
/// SUBCLASS; `Class.new` builds a `Class`. Their result must NOT be typed as an
/// instance of the receiver — doing so would witness a chained class-method call
/// (e.g. the second `.new` in `Struct.new(:a).new(1)`) falsely absent. We can't
/// model the anonymous class, so the result stays Dynamic (silent).
const CLASS_RETURNING_NEW: &[&str] = &["Struct", "Data", "Class"];

/// The reference's `Array.new(n)` tuple-lift cap (`ARRAY_NEW_TUPLE_LIMIT`,
/// `method_dispatcher.rb`): a constant size `n ≤ 16` lifts to a `Tuple`; a size
/// `> 16` (or a non-constant / zero-arg call) stays `Nominal[Array]`. Ported
/// faithfully (ADR-0039); re-measured on every upstream bump (UPSTREAM.md).
const ARRAY_NEW_TUPLE_LIMIT: i64 = 16;

/// The expression typer (ADR-0023: the reference's `ExpressionTyper` /
/// `MethodDispatcher` split). Holds a borrow of the [`CoreIndex`] so it can
/// resolve a receiver's class and a method's return type — the data a CHAINED
/// call needs to type correctly (`s.downcase : String`, so the next `.lenght`
/// can be flagged).
///
/// The index is a *field*, not a per-call parameter, so the existing free
/// [`type_of`] / [`build_toplevel_env`] signatures stay source-compatible: they
/// are thin wrappers over a [`Typer`] built with an empty index. Callers that
/// want chained-call result typing construct a [`Typer`] with the real index.
/// One local's class fact inside the narrowing flow pass
/// ([`Typer::class_narrowing_pass`]). The two variants are the two DIRECTIONS
/// the two engines' carriers can disagree in, and they never coexist for one
/// local: `Narrowed` requires a `Dynamic`/`Top` carrier, `Bot` a precise one.
#[derive(Clone, PartialEq, Eq, Debug)]
enum ClassFact {
    /// The local was narrowed FROM `Dynamic`/`Top` TO `Nominal[C]` — the
    /// reference's `narrow_class_other` (`narrowing.rb:2425`). A call on the
    /// local witnesses `call.undefined-method` against `C`.
    Narrowed(String),
    /// The guard class is DISJOINT from (or, under `instance_of?`, unequal to)
    /// the local's carrier class, so the reference's narrowing yields `Bot`
    /// (`narrow_nominal_to_class` / `narrow_shape_to_class` /
    /// `narrow_constant_to_class`). Dispatch through `Bot` witnesses nothing:
    /// every call on the local is SUPPRESSED, for every rule.
    Bot,
}

/// One local's class assertion on ONE edge of a predicate, before any gate is
/// applied — the raw syntactic output of [`Typer::analyse_predicate`], stage
/// 3a-1. The gates (carrier ALLOW-list, `Dynamic`/`Top`, disjoint collapse,
/// review R3) are applied per-local by [`Typer::apply_guards`], which is why
/// this stays a pure syntactic fact.
#[derive(Clone, PartialEq, Eq, Debug)]
struct GuardFact {
    /// The class names the edge asserts the local is one OF. Length 1 for an
    /// atomic guard; longer only after an `||` truthy join, where the reference
    /// narrows to a UNION (`Hash | String`) that this slice cannot represent —
    /// so a multi-class fact never MINTS, and only ever feeds the `Bot`
    /// collapse, which needs every member to collapse (probes `k_bot_or_bot`,
    /// `k_bot_or_same` vs the must-still-fire `k_bot_or_cond`).
    classes: Vec<String>,
    /// `instance_of?` — the reference's `exact:` path, whose collapse condition
    /// is a bare name mismatch rather than a proven-disjoint pair.
    exact: bool,
    /// May this fact mint a [`ClassFact::Narrowed`]? `false` for `===`.
    mintable: bool,
    /// Stage 3a-3: for a [`GuardTarget::Chain`] fact, the arena id of the CHAIN
    /// CALL the predicate guarded (`h.last` in `h.last.is_a?(String)`). The
    /// `narrow_class_other` Dynamic/Top carrier gate is evaluated against THAT
    /// node's type, not against any local's — `h = [1, 2]; h.last.is_a?(String)`
    /// types the address `Integer`, which the reference collapses to `Bot`
    /// (probe `k_root_array_lit`, reference-silent). `None` for a
    /// [`GuardTarget::Local`] fact.
    chain_call: Option<NodeId>,
}

/// What a [`GuardFact`] asserts a class of: a bare local (stages 1-3a-1) or a
/// stable single-hop chain address (stage 3a-3).
///
/// `Chain(root, m)` is the port of the reference's `stable_chain_address`
/// (`narrowing.rb:1826`) restricted to LOCAL roots — an ivar root is declined
/// because the arena's `VariableRead` carries no name (spec row c7b, a recorded
/// coverage gap).
#[derive(Clone, PartialEq, Eq, Debug)]
enum GuardTarget {
    Local(String),
    Chain(String, String),
}

impl GuardTarget {
    /// The LOCAL whose rebind invalidates this target — the name itself for a
    /// local, the ROOT for a chain address.
    fn root(&self) -> &str {
        match self {
            GuardTarget::Local(n) | GuardTarget::Chain(n, _) => n,
        }
    }
}

/// One edge's guard facts in SOURCE ORDER. A `Vec` rather than a map because
/// [`Typer::apply_guards`] must apply a same-target collision sequentially to
/// reproduce the reference's nested scopes (see its doc comment).
type GuardMap = Vec<(GuardTarget, GuardFact)>;

/// A stable single-hop chain address — `(root local name, method name)`, the
/// key of the stage-3a-3 `chain_env`.
type ChainAddr = (String, String);

/// The class-narrowing fact environment threaded through the flow pass: the
/// per-LOCAL facts stages 1-3a-1 established, plus the stage-3a-3 per-CHAIN
/// facts keyed by [`ChainAddr`].
///
/// The two families are deliberately separate maps rather than one keyed union:
/// the invalidation rules differ (any mention of the ROOT kills every chain
/// rooted at it, while a local fact dies only on a write/mutation of that
/// local), and the MINT gate reads a different carrier (the chain call's own
/// type, not the local's `tenv` entry).
///
/// Both families carry the same [`ClassFact`] value. A chain `Bot` was
/// introduced by the 2026-08-09 chain-guard-meet slice: the reference's
/// sequential meet collapses a re-guarded chain address exactly as it collapses
/// a local, and "absent" could not express it — a THIRD guard re-minted against
/// the empty env and witnessed where the reference stays silent (probe
/// `chain_third`). `Bot` is a sentinel that survives every later guard and
/// suppresses the recorded use.
#[derive(Clone, Default, Debug)]
struct Facts {
    /// Per-local facts. Was the bare `cenv: HashMap<String, ClassFact>` before
    /// stage 3a-3; every existing rule reads and writes exactly this field.
    locals: HashMap<String, ClassFact>,
    /// Stage 3a-3: `(root, method) -> class fact`.
    chains: HashMap<ChainAddr, ClassFact>,
}

impl Facts {
    /// Invalidate everything a REBIND of `name` invalidates: the local's own
    /// fact and EVERY chain address rooted at it (the reference drops a chain
    /// narrowing inside `Scope#with_local`, `narrowing.rb:1800` — probe `c7g`,
    /// where the reference fires a DIFFERENT diagnostic off the rebound value
    /// and we must stay silent).
    fn kill_local(&mut self, name: &str) {
        self.locals.remove(name);
        self.chains.retain(|(root, _), _| root != name);
    }

    /// Invalidate every chain address rooted at `name`, leaving the local fact
    /// alone — the port of `invalidate_chain_after_call`
    /// (`indexed_narrowing.rb:151`), widened to any MENTION of the root (spec
    /// rows c7c/f23: the reference keeps the fact through an argument-position
    /// mention and we decline, a pure coverage loss).
    fn kill_chains_rooted_at(&mut self, name: &str) {
        self.chains.retain(|(root, _), _| root != name);
    }
}

/// The output of [`Typer::class_narrowing_pass`] — the two per-call-node
/// snapshot sets the rules layer consumes.
#[derive(Default, Debug)]
pub struct ClassNarrowing {
    /// `call node id -> narrowed class name C` for a bare-local receiver the
    /// pass narrowed from `Dynamic`/`Top`. Read by `check_narrowed_call`.
    pub calls: HashMap<NodeId, String>,
    /// Call node ids whose bare-local receiver is `Bot` under a disjoint guard.
    /// The rules layer emits NOTHING at these sites — the reference cannot,
    /// because `Bot` has no dispatch surface.
    pub dead: HashSet<NodeId>,
}

pub struct Typer<'i> {
    index: &'i CoreIndex,
    /// The per-run in-source class index (ADR-0023 tier-4). Empty for a
    /// [`Typer::new`] caller; real for [`Typer::with_source`]. Lets `X.new` type
    /// to an instance of a project-defined class and a typo on it be witnessed.
    source: &'i SourceIndex,
    /// The optional real-Ruby folder (ADR-0008 sidecar). `None` keeps folding to
    /// the conservative Rust core (the sound subset); `Some` lets the dispatcher
    /// route a [`folding::sidecar_foldable`] call the Rust core declined to real
    /// Ruby. Must be `Sync` so one folder is shared across the file-parallel walk.
    folder: Option<&'i (dyn folding::RubyFolder + Sync)>,
    /// C1 (constant-shadow gate): the CURRENT FILE's lexical class/module scopes,
    /// `(span, qualified segments)`, so the `ConstantRead` arm can recover a
    /// use-site lexical prefix by span containment and consult
    /// [`SourceIndex::constant_shadowed`] precisely. Empty (`&[]`) for callers
    /// that do not set it (unit tests / pre-C1 entry points) — with no scopes
    /// every use site reads as toplevel, so only TOPLEVEL project definitions
    /// suppress, matching the conservative default.
    lexical_scopes: &'i [(rigor_parse::Span, Vec<String>)],
}

/// A shared empty lexical-scope slice — the default `lexical_scopes` for a
/// [`Typer`] built without the C1 per-file scopes.
const EMPTY_LEXICAL_SCOPES: &[(rigor_parse::Span, Vec<String>)] = &[];

impl<'i> Typer<'i> {
    /// Build a typer over a borrowed core index, with an EMPTY source index
    /// (no in-source typing). Kept for callers that predate tier-4.
    pub fn new(index: &'i CoreIndex) -> Self {
        Typer { index, source: empty_source(), folder: None, lexical_scopes: EMPTY_LEXICAL_SCOPES }
    }

    /// Build a typer over a borrowed core index AND a per-run [`SourceIndex`],
    /// enabling `X.new` instance typing and in-source method resolution.
    pub fn with_source(index: &'i CoreIndex, source: &'i SourceIndex) -> Self {
        Typer { index, source, folder: None, lexical_scopes: EMPTY_LEXICAL_SCOPES }
    }

    /// As [`Typer::with_source`], plus the ADR-0008 real-Ruby folder for
    /// sidecar-routed constant folds. `None` is byte-identical to
    /// [`Typer::with_source`] (the sound subset).
    pub fn with_source_and_folder(
        index: &'i CoreIndex,
        source: &'i SourceIndex,
        folder: Option<&'i (dyn folding::RubyFolder + Sync)>,
    ) -> Self {
        Typer { index, source, folder, lexical_scopes: EMPTY_LEXICAL_SCOPES }
    }

    /// C1: attach the CURRENT FILE's lexical class/module scopes (from
    /// [`source_index::lexical_scopes`]) so the `ConstantRead` arm resolves a
    /// use-site lexical prefix. A consuming builder — the analyze pass computes
    /// the scopes once per file and threads them here.
    pub fn with_lexical_scopes(
        mut self,
        scopes: &'i [(rigor_parse::Span, Vec<String>)],
    ) -> Self {
        self.lexical_scopes = scopes;
        self
    }

    /// C5: re-intern a harvested [`ConstLit`] against the local interner into the
    /// SAME carrier the Typer builds for the equivalent inline literal — a scalar
    /// → `Constant`, an array → `Tuple`, a static-keyed hash → `HashShape`, a
    /// range → `Nominal[Range]`. This is what makes a literal-constant diagnostic
    /// render identically to the reference's value-pinned receiver.
    fn intern_const_lit(&self, lit: &ConstLit, interner: &mut Interner) -> TypeId {
        match lit {
            ConstLit::Scalar(s) => interner.intern(Type::Constant(s.clone())),
            ConstLit::Tuple(elems) => {
                let ids: Vec<TypeId> =
                    elems.iter().map(|l| self.intern_const_lit(l, interner)).collect();
                interner.intern(Type::Tuple(ids))
            }
            ConstLit::Hash(members) => {
                let ms: Vec<ShapeMember> = members
                    .iter()
                    .map(|(key, l)| ShapeMember {
                        key: key.clone(),
                        value: self.intern_const_lit(l, interner),
                        optional: false,
                    })
                    .collect();
                interner.intern(Type::HashShape(ms))
            }
            // Range types to `Nominal[Range]` so witnessing resolves against
            // Range's RBS (an `IntegerRange` would erase to `Integer`).
            ConstLit::Range => self.nominal_or_untyped("Range", interner),
            // Slice B: a partially-literal container. `nominal_or_untyped`
            // yields `Nominal { args: [] }` — the projection-inert carrier (see
            // the `ConstLit::BareArray` docs); it degrades to Dynamic when the
            // class is unregistered, which is silent.
            ConstLit::BareArray => self.nominal_or_untyped("Array", interner),
            ConstLit::BareHash => self.nominal_or_untyped("Hash", interner),
        }
    }

    /// C1: the use-site lexical prefix (enclosing class/module qualified segments)
    /// for a node at `span` — the INNERMOST enclosing scope by span containment,
    /// or an empty slice at toplevel / when no scopes are attached.
    pub fn enclosing_prefix(&self, span: rigor_parse::Span) -> &[String] {
        let mut best: Option<&(rigor_parse::Span, Vec<String>)> = None;
        for sc in self.lexical_scopes {
            if sc.0 .0 <= span.0 && span.1 <= sc.0 .1 {
                // Contained: keep the innermost (narrowest span).
                match best {
                    None => best = Some(sc),
                    Some(b) if (sc.0 .1 - sc.0 .0) < (b.0 .1 - b.0 .0) => best = Some(sc),
                    _ => {}
                }
            }
        }
        best.map(|b| b.1.as_slice()).unwrap_or(&[])
    }

    /// The borrowed source index (for the rules layer's method-resolution gate).
    pub fn source(&self) -> &SourceIndex {
        self.source
    }

    /// The borrowed core index.
    pub fn core(&self) -> &CoreIndex {
        self.index
    }

    /// Type an owned-AST node against the current `env`, interning carriers into
    /// `interner`. Pure dispatch by node variant (ADR-0023): never mutates the
    /// AST, only reads `env`.
    ///
    /// - `StringLit` -> `Constant["..."]`
    /// - `IntegerLit` -> `Constant[n]`
    /// - `LocalVariableRead` -> the env binding, else `Dynamic[top]`
    /// - `Call { receiver: Some(r), method, .. }` -> the dispatch cascade below
    /// - anything else -> `Dynamic[top]` (`Interner::untyped`)
    ///
    /// Returning `untyped` (rather than guessing) on an unknown is the
    /// load-bearing behaviour that keeps downstream rules zero-false-positive
    /// (ADR-0023 tier-5).
    pub fn type_of(&self, ast: &LoweredAst, id: NodeId, env: &TypeEnv, interner: &mut Interner) -> TypeId {
        match ast.get(id) {
            Node::StringLit { value, .. } => {
                interner.intern(Type::Constant(Scalar::Str(value.clone())))
            }
            // An interpolated string / heredoc (`"a#{x}b"`) is always a `String`
            // instance regardless of the interpolated values, so type it as a
            // bare `String` Nominal — a typo'd / non-core method on it (e.g.
            // `.squish`, `.constantize`) then resolves against the real String
            // RBS and is witnessed, matching the reference.
            Node::InterpolatedString { .. } => self.nominal_or_untyped("String", interner),
            // An interpolated symbol (`:"a#{x}b"`) is always a `Symbol`
            // instance regardless of the interpolated values — a structural
            // twin of `InterpolatedString` above, differing only in the
            // nominal type name, so it never mis-types as a `String`.
            Node::InterpolatedSymbol { .. } => self.nominal_or_untyped("Symbol", interner),
            Node::IntegerLit { value, .. } => {
                interner.intern(Type::Constant(Scalar::Int(*value)))
            }
            Node::FloatLit { value, .. } => {
                interner.intern(Type::Constant(Scalar::Float(*value)))
            }
            Node::SymbolLit { value, .. } => {
                interner.intern(Type::Constant(Scalar::Sym(value.clone())))
            }
            Node::NilLit { .. } => interner.intern(Type::Constant(Scalar::Nil)),
            Node::TrueLit { .. } => interner.intern(Type::Constant(Scalar::Bool(true))),
            Node::FalseLit { .. } => interner.intern(Type::Constant(Scalar::Bool(false))),
            Node::LocalVariableRead { name, .. } => env
                .get(name)
                .copied()
                .unwrap_or_else(|| interner.untyped()),
            // `a, b = rhs` AS AN EXPRESSION is its right-hand side (Ruby: `(a, b
            // = [1, 2])` evaluates to `[1, 2]`). The reference routes
            // `Prism::MultiWriteNode` to `type_of_assignment_write`
            // (`expression_typer.rb:125`), the same handler the single-target
            // writes use.
            Node::MultiWrite { value, .. } => {
                let value = *value;
                self.type_of(ast, value, env, interner)
            }
            Node::Call { receiver: Some(r), method, args, block_body, .. } => {
                let (r, method) = (*r, method.clone());
                if !block_body.is_empty() {
                    // A block changes which RBS overload applies: the reference
                    // selects the block-bearing overload (`block_required: true`)
                    // and the call yields ITS return type. We model that
                    // RBS-derived behavior precisely: `arr.map { } : Array`,
                    // `h.select { } : Hash`, `h.reject { } : Hash`, `x.tap { } :
                    // x`, `arr.each { } : arr` (a `self` block return resolves to
                    // the receiver's own class). This recovers chained-witnessing
                    // (`arr.map { }.frist` flags on Array) WITHOUT the FP that the
                    // no-block return would cause (`h.select { }.keys` — keys IS
                    // on the Hash the block form returns, so it stays silent).
                    //
                    // Zero-FP discipline: when the block-form return is NOT
                    // precisely modeled (no block overload, or a generic/union/
                    // void/unknown return — `method_return_with_block` ⇒ None),
                    // OR the receiver isn't a concrete class we model, we decline
                    // to `Dynamic[top]` (silent), exactly as the prior blanket
                    // placeholder did for every block call. Never guess a type.
                    self.type_block_call(ast, r, &method, env, interner)
                } else {
                    let args = args.clone();
                    self.type_call(ast, r, &method, &args, env, interner)
                }
            }
            // An IMPLICIT-SELF call (`p x`, `format(...)`, …) never reaches
            // `type_call` (that path is `receiver: Some(_)` only). This is the
            // shared implicit-self dispatch entry (ADR-0038 inference-cluster
            // spec): keyed strictly off `receiver: None`, it lets receiverless
            // Kernel folds be typed. This slice implements ONLY Kernel `p`/`pp`
            // identity; every other implicit-self call declines and falls to
            // `Dynamic[top]` exactly as the catch-all did before (zero behaviour
            // change off the `p`/`pp` path). A block does NOT block the fold —
            // `p(x) { }` still types to `x` — because block reachability is the
            // rule walk's concern, not this value query.
            Node::Call { receiver: None, method, args, .. } => {
                let (method, args) = (method.clone(), args.clone());
                self.type_implicit_self_call(ast, &method, &args, env, interner)
                    .unwrap_or_else(|| interner.untyped())
            }
            // A bare constant read (`Time`, `Array`) types to the CLASS OBJECT
            // itself — `Type::Singleton(class)` — so a class-method typo on it
            // (`Time.current`) can be witnessed. The zero-FP gate (ADR-0023):
            //   * `name` is a GENUINE top-level RBS class (`knows_toplevel_class`)
            //     — excludes namespaced-only names (`Status`/`Instance`/`List`);
            //   * the PROJECT does NOT define `name` (`!source.knows_class`) —
            //     excludes top-level RBS classes that are ALSO project models
            //     (`Group`/`Report`), which the reference resolves to the project
            //     class and stays silent on; AND
            //   * `name` is registered so its id round-trips for rendering.
            // Any miss ⇒ fall through to Dynamic[top] (silent). Note: a `Foo.new`
            // receiver is intercepted earlier in `type_call` (before the constant
            // is typed), so `Time.new` still yields a Time INSTANCE, not Singleton.
            Node::ConstantRead { name, span, .. } => {
                // Both the C5 literal-fold and the C1 shadow gate resolve against
                // the use site's lexical prefix (Ruby constant lookup), so compute
                // it once.
                let prefix = self.enclosing_prefix(*span);
                // C5: a project constant with a single fully-literal assignment,
                // visible here lexically, types to that literal value
                // (Range -> Nominal[Range]) — consulted BEFORE the singleton gate
                // so `R = 1..1024; R.exclude?` witnesses on the range value.
                // Slice A (2026-08-08): the value only applies at a use site in
                // the SAME FILE as the assignment — the reference rebuilds its
                // in-source constant-value table per file, so a cross-file fold
                // is an emission the oracle never makes.
                if let Some(lit) = self.source.literal_constant(name, prefix, ast.file_id()) {
                    return self.intern_const_lit(lit, interner);
                }
                // Collection-shape stage 2e: the same C5 value reached by a
                // FULLY-QUALIFIED path (`::A::B::C::CONST`), which arrives as one
                // `ConstantRead` whose `name` is the whole path and so misses the
                // bare-name map above. Ambiguity declines (see
                // `SourceIndex::qualified_literal_constant`).
                if let Some(lit) =
                    self.source.qualified_literal_constant(name, prefix, ast.file_id())
                {
                    return self.intern_const_lit(lit, interner);
                }
                // C1: replace the pre-C1 bare-name project-wide suppression
                // (`!source.knows_class(name)`) with a LEXICALLY PRECISE
                // shadow gate: a nested project `module Time` suppresses the
                // core-RBS singleton only at use sites it is lexically visible
                // from; a toplevel definition still suppresses everywhere. See
                // `SourceIndex::constant_shadowed`.
                if !name.is_empty()
                    && self.index.knows_toplevel_class(name)
                    && !self.source.constant_shadowed(name, prefix)
                {
                    if let Some(class) = self.source.class_id(name) {
                        return interner.intern(Type::Singleton(class));
                    }
                }
                // ADR-0042 Slice 2: an unambiguous NAMESPACED constant
                // (`ERB::Util`) types to its class object so a class-method typo
                // witnesses. Gated on the QUALIFIED registry (not the short-key
                // `knows_toplevel_class`, which refuses namespaced names for the
                // defect-2 reason): a qualified key is its own isolated entry,
                // so `ERB::Util` never collides with `CGI::Util` or a project
                // `Util`. The project-shadow gate still applies (a project decl
                // of the same qualified name wins).
                if name.contains("::")
                    && self.index.knows_qualified_class(name)
                    && !self.source.constant_shadowed(name, prefix)
                {
                    if let Some(class) = self.source.class_id(name) {
                        return interner.intern(Type::Singleton(class));
                    }
                }
                interner.untyped()
            }
            // An array literal types to a value-pinned `Tuple` of its element
            // types (reference `array_type_for`): `[]` → the empty `Tuple[]`, a
            // non-splat literal → `Tuple[t1, .., tn]`. `class_name_of(Tuple)`
            // erases to `Array`, so a typo'd method (`[1,2].frist`) still flags
            // via the real Array RBS exactly as before — the Tuple only sharpens
            // the DISPLAY (`[1, 2]`, not `Array`) to match the reference. A splat
            // (or any element with no owned AST variant, lowered to
            // `Statements`/`Other`) makes the arity unknown, so it degrades to the
            // bare `Array` nominal (the reference's `Nominal[Array, [union]]`).
            Node::ArrayLit { elements, .. } => {
                if elements.is_empty() {
                    interner.intern(Type::Tuple(vec![]))
                } else if elements.iter().any(|&e| {
                    matches!(
                        ast.get(e),
                        Node::Statements { .. } | Node::Other { .. } | Node::Return { .. }
                    )
                }) {
                    self.nominal_or_untyped("Array", interner)
                } else {
                    let elem_ids: Vec<NodeId> = elements.clone();
                    let elems: Vec<TypeId> =
                        elem_ids.iter().map(|&e| self.type_of(ast, e, env, interner)).collect();
                    interner.intern(Type::Tuple(elems))
                }
            }
            // A hash literal types to a value-pinned `HashShape` (reference
            // `type_of_hash` / `static_hash_shape_for`) when every element is an
            // assoc with a static Symbol/String key: `{ a: 1 }` → `{ a: 1 }`,
            // `{}` → the empty `HashShape{}`. `class_name_of(HashShape)` erases to
            // `Hash`, so a typo'd method (`{ a: 1 }.fetchh`) still flags via the
            // real Hash RBS — the shape only sharpens the DISPLAY. A `**`splat, a
            // non-static (dynamic / integer) key, or a duplicate key degrades to
            // the bare `Hash` nominal (`all_assoc == false` short-circuits it).
            Node::HashLit { elements, all_assoc, .. } => {
                if *all_assoc {
                    let elem_ids = elements.clone();
                    self.hash_shape_or_hash(ast, &elem_ids, env, interner)
                } else {
                    self.nominal_or_untyped("Hash", interner)
                }
            }
            // An `if`/`unless`/ternary AS AN EXPRESSION evaluates to the union of
            // its branch values (reference `type_of_if`): each branch's tail
            // value, with a missing `else` contributing `nil`. A KNOWN-polarity
            // predicate elides the dead branch (`if str_value; a; end` → `a`, not
            // `a | nil`, since a Nominal/non-nil-Constant is always truthy). An
            // unknown predicate keeps both. Sharpens `type-of`/`annotate`; a
            // union receiver never witnesses (`class_name_of` ⇒ None), so this
            // adds no undefined-method firings and is FP-safe.
            Node::If { predicate, then_body, else_body, is_unless, .. } => {
                let then_ty = self.branch_value_type(ast, then_body, env, interner);
                let else_ty = if else_body.is_empty() {
                    interner.intern(Type::Constant(Scalar::Nil))
                } else {
                    self.branch_value_type(ast, else_body, env, interner)
                };
                // The union is symmetric, but ELISION on a known predicate must
                // pick the live branch by the keyword's polarity: an `unless`
                // runs its body when the predicate is FALSEY, so a truthy
                // predicate selects the else branch (inverted vs `if`).
                let (truthy_ty, falsey_ty) =
                    if *is_unless { (else_ty, then_ty) } else { (then_ty, else_ty) };
                let pred_ty = self.type_of(ast, *predicate, env, interner);
                match self.predicate_polarity(interner, pred_ty) {
                    Some(true) => truthy_ty,
                    Some(false) => falsey_ty,
                    None => rigor_types::Algebra::join(interner, then_ty, else_ty),
                }
            }
            // A `case`/`when` (or `case`/`in`) AS AN EXPRESSION types to the
            // union of its branch values + the `else` value (or `nil` when there
            // is no `else` — a non-exhaustive `case` returns nil). This is the
            // reference `type_of_case_simple_union` (a sound over-approximation of
            // the `===`-certainty-narrowed variant, which only ever DROPS
            // statically-impossible branches). Each branch lowers to a
            // `BeginRescue` carrier whose tail is the branch's value, resolved by
            // `stmt_value_type`. A union receiver never witnesses, so FP-safe.
            Node::Case { branches, else_body, .. } => {
                let branch_ids = branches.clone();
                let else_ids = else_body.clone();
                let mut acc: Option<TypeId> = None;
                for br in branch_ids {
                    let v = self.stmt_value_type(ast, br, env, interner);
                    acc = Some(match acc {
                        None => v,
                        Some(a) => rigor_types::Algebra::join(interner, a, v),
                    });
                }
                let else_ty = if else_ids.is_empty() {
                    interner.intern(Type::Constant(Scalar::Nil))
                } else {
                    self.branch_value_type(ast, &else_ids, env, interner)
                };
                match acc {
                    Some(a) => rigor_types::Algebra::join(interner, a, else_ty),
                    None => else_ty,
                }
            }
            // Any other carrier (`@ivar`, constant, `self`, index, range,
            // logical, variable read) is not precisely typed in this slice ->
            // Dynamic[top] (never guess; keeps the call rule silent). Implicit-
            // self calls are handled by the `receiver: None` arm above.
            // TODO(spec): ivar typing (ADR-0022), constant resolution,
            // container-element typing.
            _ => interner.untyped(),
        }
    }

    /// The value a branch body evaluates to (reference `statements_or_nil`): its
    /// tail statement's value, or `Constant[nil]` for an empty body.
    fn branch_value_type(
        &self,
        ast: &LoweredAst,
        body: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> TypeId {
        match body.last() {
            Some(&tail) => self.stmt_value_type(ast, tail, env, interner),
            None => interner.intern(Type::Constant(Scalar::Nil)),
        }
    }

    /// The value a single statement evaluates to: an assignment → its RHS value;
    /// a statements / `else`-clause wrapper (rigor-rs lowers an `else` body to a
    /// `BeginRescue` carrier) → its own tail statement's value; otherwise the
    /// node's type. Recursive over wrappers so a branch's tail resolves to the
    /// real value expression.
    fn stmt_value_type(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> TypeId {
        match ast.get(id) {
            Node::Statements { body, .. } | Node::BeginRescue { body, .. } => {
                match body.clone().last() {
                    Some(&tail) => self.stmt_value_type(ast, tail, env, interner),
                    None => interner.intern(Type::Constant(Scalar::Nil)),
                }
            }
            // A `when` clause's value: its last body statement — or, when the
            // body is empty, its last CONDITION (`when X` with no body). This is
            // byte-identical to the pre-split `BeginRescue` carrier, whose body
            // held `conditions ++ statements` concatenated.
            Node::When { conditions, body, .. } => {
                let tail = body.last().or(conditions.last()).copied();
                match tail {
                    Some(tail) => self.stmt_value_type(ast, tail, env, interner),
                    None => interner.intern(Type::Constant(Scalar::Nil)),
                }
            }
            // A multi-write evaluates to its RHS, exactly like the single-target
            // writes: `(a, b = [1, 2])` is `[1, 2]` (reference
            // `expression_typer.rb:119` / `eval_multi_write`).
            Node::LocalVariableWrite { value, .. }
            | Node::LocalVariableOpWrite { value, .. }
            | Node::MultiWrite { value, .. }
            | Node::VariableWrite { value, .. }
            | Node::InstanceVariableWrite { value, .. }
            | Node::ConstantWrite { value, .. } => {
                let value = *value;
                self.type_of(ast, value, env, interner)
            }
            _ => self.type_of(ast, id, env, interner),
        }
    }

    /// Three-valued truthiness of a predicate's type for branch elision
    /// (reference `Narrowing.predicate_certainty`): `Some(false)` for the only
    /// falsey values (`nil` / `false`), `Some(true)` for a value that is always
    /// truthy in Ruby (any Nominal / shape / non-nil-non-false Constant), and
    /// `None` (keep both branches) for anything whose truthiness is not statically
    /// decided (`Dynamic` / `Top` / a union / `bool`). Deliberately no more
    /// aggressive than the reference: a union is always `None`, so rigor-rs never
    /// elides a branch the reference keeps (which could only cost a witness, never
    /// add a false one).
    fn predicate_polarity(&self, interner: &Interner, ty: TypeId) -> Option<bool> {
        match interner.get(ty) {
            Type::Constant(Scalar::Nil) | Type::Constant(Scalar::Bool(false)) => Some(false),
            Type::Constant(_)
            | Type::Nominal { .. }
            | Type::Tuple(_)
            | Type::HashShape(_)
            | Type::IntegerRange { .. }
            | Type::Singleton(_)
            | Type::DataInstance { .. } => Some(true),
            _ => None,
        }
    }

    /// Intern a bare `Nominal { class }` for a registered core class name, or
    /// `Dynamic[top]` if the index doesn't register it. Used to type a literal
    /// container (array/hash) so a typo'd method on it resolves against the real
    /// RBS for that class, while staying silent if the class is somehow unknown.
    fn nominal_or_untyped(&self, class_name: &str, interner: &mut Interner) -> TypeId {
        match self.index.class_id(class_name) {
            Some(class) => interner.intern(Type::Nominal { class, args: vec![] }),
            None => interner.untyped(),
        }
    }

    /// Intern one RBS return-shape descriptor (`rigor_index::RbsReturnShape`)
    /// as a rigor-rs type — the rigor-rs half of the reference's
    /// `RbsTypeTranslator` (`rbs_type_translator.rb:162`, `translate_tuple`).
    ///
    /// A `Class` resolves its id the way every other RBS-return mint does: the
    /// core (CORE_CLASSES) id first, else the source-registry id (which Pass 2b
    /// of [`SourceIndex`] pre-registers for exactly the tuple-element classes).
    /// A `Tuple` recurses into a [`Type::Tuple`]. Anything else — and any name
    /// with no registry identity — becomes `Dynamic[top]`, matching the
    /// reference's total translator, whose unmodeled shapes degrade to `untyped`.
    /// A `Dynamic[top]` slot is silent in every rule, so the degrade can only
    /// lose recall.
    fn intern_rbs_shape(
        &self,
        shape: &rigor_index::RbsReturnShape,
        interner: &mut Interner,
    ) -> TypeId {
        match shape {
            rigor_index::RbsReturnShape::Class(name) => {
                if let Some(class) = self.index.class_id(name) {
                    return interner.intern(Type::Nominal { class, args: vec![] });
                }
                if let Some(class) = self.source.class_id(name) {
                    return interner.intern(Type::Nominal { class, args: vec![] });
                }
                interner.untyped()
            }
            rigor_index::RbsReturnShape::Tuple(elems) => {
                let ids: Vec<TypeId> =
                    elems.iter().map(|e| self.intern_rbs_shape(e, interner)).collect();
                interner.intern(Type::Tuple(ids))
            }
            rigor_index::RbsReturnShape::Unknown => interner.untyped(),
        }
    }

    /// Intern an RBS TUPLE return (`[Integer, Process::Status]`) as a
    /// [`Type::Tuple`] of interned element shapes. See [`Self::intern_rbs_shape`].
    fn intern_rbs_tuple(
        &self,
        shapes: &[rigor_index::RbsReturnShape],
        interner: &mut Interner,
    ) -> TypeId {
        let ids: Vec<TypeId> =
            shapes.iter().map(|s| self.intern_rbs_shape(s, interner)).collect();
        interner.intern(Type::Tuple(ids))
    }

    /// Build a value-pinned [`Type::HashShape`] from an all-assoc hash literal's
    /// flat `[k, v, k, v, …]` element list (guaranteed even by `all_assoc`), or
    /// fall back to the bare `Hash` nominal. A faithful port of the reference's
    /// `static_hash_shape_for`: every key must be a value-pinned scalar literal
    /// (Symbol / String / Integer / Float / true / false / nil — the reference's
    /// `HashShape::ALLOWED_KEY_CLASSES`); a non-static key degrades to `Hash`.
    ///
    /// Duplicate keys are LAST-WINS, matching the runtime (`{ a: 1, a: 2 }` keeps
    /// `a: 2`): the key keeps its FIRST insertion position while the value comes
    /// from the LAST occurrence. Key identity is Ruby `Hash#eql?` (`1` ≠ `1.0`;
    /// `1.0` == `1.00`), realised by [`ShapeKey`]'s derived equality. The empty
    /// list yields the empty `HashShape{}` (`{}`).
    fn hash_shape_or_hash(
        &self,
        ast: &LoweredAst,
        elem_ids: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> TypeId {
        let mut members: Vec<ShapeMember> = Vec::with_capacity(elem_ids.len() / 2);
        let mut i = 0;
        while i + 1 < elem_ids.len() {
            let Some(key) = static_shape_key_of_node(ast.get(elem_ids[i])) else {
                // A dynamic / non-scalar key can't pin a shape slot.
                return self.nominal_or_untyped("Hash", interner);
            };
            let value = self.type_of(ast, elem_ids[i + 1], env, interner);
            // Last-wins: an existing key keeps its FIRST position, takes the LAST
            // value; a new key appends in source order.
            if let Some(m) = members.iter_mut().find(|m| m.key == key) {
                m.value = value;
            } else {
                members.push(ShapeMember { key, value, optional: false });
            }
            i += 2;
        }
        interner.intern(Type::HashShape(members))
    }

    /// Type a method call with a receiver, running the conservative head of the
    /// dispatch cascade (ADR-0023):
    ///
    /// 1. **Constant folding** (ADR-0008 Rust core): if the receiver types to a
    ///    value-pinned `Constant(scalar)` and [`folding::fold`] yields a result,
    ///    return that pinned `Constant`.
    /// 2. **RBS-ish return resolution**: else resolve the receiver's class via
    ///    the index and look up [`rigor_index::method_return`]; intern the
    ///    result as a `Nominal { class }` so the *next* call in a chain can be
    ///    typed (and a typo on it flagged).
    /// 3. **Fallback**: otherwise `Dynamic[top]` — silence over a guess.
    ///
    // TODO(spec): tier-2 shape dispatch, tier-4 in-source bodies, argument
    // contracts, the Ruby sidecar for non-Rust-foldable calls (ADR-0008/0023).
    /// Type a `.new` call's result as an INSTANCE of the named class — shared by
    /// the plain (`X.new(...)`) and block-bearing (`X.new(...) { ... }`) paths so
    /// both agree that `X.new` (with or without a block) is an `X` instance.
    ///
    /// `Some(Nominal[X])` when `receiver` is a bare constant naming a class the
    /// core index (preferred) or the source index knows, and `X` is NOT a
    /// metaclass constructor (`Struct`/`Data`/`Class`, whose `.new`/`.define`
    /// build an anonymous SUBCLASS we can't model). `None` ⇒ not a typeable
    /// `.new`; the caller falls through to its normal path (Dynamic / block
    /// return), silent.
    ///
    /// This helper decides only the receiver TYPE. The
    /// non-core-`.new`-never-witnessed leniency (2026-06-26 correctness finding)
    /// lives in the RULES layer, which witnesses only receivers whose class is
    /// RBS-known in the core surface — a source-only `.new` instance types for
    /// chaining but is never a *witnessing* surface. Identical for both shapes.
    fn type_dot_new(
        &self,
        ast: &LoweredAst,
        receiver: NodeId,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        let Node::ConstantRead { name, .. } = ast.get(receiver) else {
            return None;
        };
        if name.is_empty() || CLASS_RETURNING_NEW.contains(&name.as_str()) {
            return None;
        }

        // Reference `meta_new` constant-constructor lifts (faithful decline
        // set): for a curated set of immutable value classes, an all-pinned
        // `.new` is lifted by the reference to a pinned VALUE carrier
        // (`Constant<Pathname>` / `Constant<Date>`), on which its UM stays
        // silent. rigor-rs does not model those carriers, so the observable-
        // equivalent is to DECLINE the mint (Dynamic, silent):
        //   - `CONSTANT_CONSTRUCTORS` = { Pathname }: exactly 1 arg, pinned
        //     String (`Pathname.new("x")` — fixture 38's pinned leniency;
        //     `Pathname.new(:sym)` RAISES in the lift, so the reference falls
        //     to Nominal and fires — we mint);
        //   - `date_new_lift` = { Date, DateTime }: 1..=8 args, every one
        //     pinned Integer|String (the reference also accepts Rational and
        //     validates by CONSTRUCTING the date; an invalid pinned date
        //     raises there and falls to Nominal — a rare under-emit here).
        // Everything else falls through to `Type::Combinator.nominal_of` in
        // the reference — a witnessable instance for ANY singleton receiver —
        // mirrored below by the core-id / source-registry mints.
        let pinned_lift = match name.as_str() {
            "Pathname" => {
                args.len() == 1
                    && matches!(
                        self.pin_arg_scalars(ast, args, env, interner).as_deref(),
                        Some([Scalar::Str(_)])
                    )
            }
            "Date" | "DateTime" => {
                (1..=8).contains(&args.len())
                    && self
                        .pin_arg_scalars(ast, args, env, interner)
                        .is_some_and(|scalars| {
                            scalars
                                .iter()
                                .all(|s| matches!(s, Scalar::Int(_) | Scalar::Str(_)))
                        })
            }
            // `set_new_lift`: `Set.new` → `Constant<Set.new>`; `Set.new(<Tuple
            // of all-Constant elements>)` → the pinned Set value. Both silent
            // in the reference; anything else falls to Nominal[Set].
            "Set" => {
                args.is_empty()
                    || (args.len() == 1 && {
                        let arg_ty = self.type_of(ast, args[0], env, interner);
                        match interner.get(arg_ty).clone() {
                            Type::Tuple(elems) => elems
                                .iter()
                                .all(|&e| matches!(interner.get(e), Type::Constant(_))),
                            _ => false,
                        }
                    })
            }
            _ => false,
        };
        if pinned_lift {
            return None;
        }
        // Prefer a core (CORE_CLASSES) nominal id — its method existence resolves
        // via the core path; else a source class or a registered RBS-only instance
        // class (e.g. Pathname) carries a registry id in the high range.
        if let Some(class_id) = self.index.class_id(name) {
            return Some(interner.intern(Type::Nominal { class: class_id, args: vec![] }));
        }
        if let Some(class_id) = self.source.class_id(name) {
            return Some(interner.intern(Type::Nominal { class: class_id, args: vec![] }));
        }
        None
    }

    /// Fold a no-arg accessor / constant-index read on a value-pinned `Tuple`
    /// receiver to the pinned element or arity — a faithful port of the reference
    /// `ShapeDispatch` Tuple folds. `None` declines (leaves the RBS tier to widen
    /// to `Array[..]`). Only the no-arg / single-constant-index forms fold; an
    /// arg-form (`first(2)`) declines so the documented `Array[Elem]` RBS overload
    /// still applies.
    fn fold_tuple_projection(
        &self,
        recv_ty: TypeId,
        method: &str,
        ast: &LoweredAst,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        let elems = match interner.get(recv_ty) {
            Type::Tuple(e) => e.clone(),
            _ => return None,
        };
        let nil = |interner: &mut Interner| interner.intern(Type::Constant(Scalar::Nil));
        match method {
            "first" if args.is_empty() => {
                Some(elems.first().copied().unwrap_or_else(|| nil(interner)))
            }
            "last" if args.is_empty() => {
                Some(elems.last().copied().unwrap_or_else(|| nil(interner)))
            }
            "size" | "length" | "count" if args.is_empty() => {
                Some(interner.intern(Type::Constant(Scalar::Int(elems.len() as i64))))
            }
            "empty?" if args.is_empty() => {
                Some(interner.intern(Type::Constant(Scalar::Bool(elems.is_empty()))))
            }
            // `at(n)` — the STRICT single-Integer accessor. Deliberately not an
            // alias of `[]`: that one also takes a Range or a `(start, length)`
            // pair, while `Array#at` raises `ArgumentError` on anything else,
            // and a fold must never invent a value for a call that raises.
            // An out-of-range index DECLINES rather than folding to nil: Ruby
            // does return nil there, but proving nil on a receiver the RBS tier
            // types as `Elem?` newly SURFACES diagnostics, which is a different
            // decision from removing a Dynamic (upstream #121).
            "at" if args.len() == 1 => {
                let idx_ty = self.type_of(ast, args[0], env, interner);
                let Type::Constant(Scalar::Int(i)) = interner.get(idx_ty) else {
                    return None;
                };
                let (i, len) = (*i, elems.len() as i64);
                let real = if i < 0 { len + i } else { i };
                (0..len).contains(&real).then(|| elems[real as usize])
            }
            // `deconstruct` hands back the receiver itself (pattern matching's
            // array view of an Array is the Array).
            "deconstruct" if args.is_empty() => Some(recv_ty),
            // The set-operation family. Both sides must be value-pinned, and
            // membership is decided by `Scalar`'s equality — which is Ruby's
            // `eql?`, not `==`: `[1] & [1.0]` is EMPTY even though `1 == 1.0`.
            // (`Scalar` compares floats by raw bits and never across variants,
            // so the distinction falls out; NaN is excluded separately below,
            // where the two relations genuinely differ.)
            "&" | "intersection" => self
                .tuple_set_operation(&elems, args, ast, env, interner, set_intersection)
                .map(|r| interner.intern(Type::Tuple(r))),
            "|" | "union" => self
                .tuple_set_operation(&elems, args, ast, env, interner, set_union)
                .map(|r| interner.intern(Type::Tuple(r))),
            "-" | "difference" => self
                .tuple_set_operation(&elems, args, ast, env, interner, set_difference)
                .map(|r| interner.intern(Type::Tuple(r))),
            // The predicate form of `&`. Folds to a pinned bool, so it can prove
            // a condition constant (`if %w[a].intersect?(%w[b])` is falsey).
            "intersect?" => {
                let r =
                    self.tuple_set_operation(&elems, args, ast, env, interner, set_intersection)?;
                Some(interner.intern(Type::Constant(Scalar::Bool(!r.is_empty()))))
            }
            // `one?` with no block and no pattern — "exactly one TRUTHY element".
            // Only pinned elements have decidable truthiness; the block form
            // never reaches here (block calls route to `type_block_call`).
            "one?" if args.is_empty() => {
                let values = tuple_constant_values(&elems, interner)?;
                let truthy = values
                    .iter()
                    .filter(|s| !matches!(s, Scalar::Nil | Scalar::Bool(false)))
                    .count();
                Some(interner.intern(Type::Constant(Scalar::Bool(truthy == 1))))
            }
            // `t[n]` — a constant integer index (Ruby negative-from-end);
            // out-of-bounds folds to `nil`. A non-constant index declines.
            "[]" if args.len() == 1 => {
                let idx_ty = self.type_of(ast, args[0], env, interner);
                let Type::Constant(Scalar::Int(i)) = interner.get(idx_ty) else {
                    return None;
                };
                let (i, len) = (*i, elems.len() as i64);
                let real = if i < 0 { len + i } else { i };
                if (0..len).contains(&real) {
                    Some(elems[real as usize])
                } else {
                    Some(nil(interner))
                }
            }
            _ => None,
        }
    }

    /// Shared body for the Tuple set operations (`&` / `|` / `-` and their named
    /// spellings): unwrap the receiver and EVERY argument to pinned scalars, run
    /// `op` left-to-right over the argument list, and hand back the resulting
    /// element list ready to re-intern as a `Tuple`.
    ///
    /// Declines unless every element on both sides is pinned (an unknown element
    /// makes membership undecidable) and every argument is itself a `Tuple` —
    /// `Array#&` also accepts anything answering `to_ary`, which a shape cannot
    /// prove. Arity and result width are capped so the fold can never materialise
    /// an unbounded Tuple, the same discipline the other shape folds keep.
    fn tuple_set_operation(
        &self,
        elems: &[TypeId],
        args: &[NodeId],
        ast: &LoweredAst,
        env: &TypeEnv,
        interner: &mut Interner,
        op: fn(&[Scalar], &[Scalar]) -> Vec<Scalar>,
    ) -> Option<Vec<TypeId>> {
        /// Longer argument lists decline rather than folding.
        const MAX_SET_OPERATION_ARITY: usize = 8;
        /// Wider results decline rather than materialising a huge Tuple.
        const MAX_SET_OPERATION_SIZE: usize = 64;

        if args.is_empty() || args.len() > MAX_SET_OPERATION_ARITY {
            return None;
        }
        let mut acc = tuple_constant_values(elems, interner)?;
        for &arg in args {
            let arg_ty = self.type_of(ast, arg, env, interner);
            let Type::Tuple(other_elems) = interner.get(arg_ty) else {
                return None;
            };
            let other = tuple_constant_values(&other_elems.clone(), interner)?;
            acc = op(&acc, &other);
        }
        if acc.len() > MAX_SET_OPERATION_SIZE {
            return None;
        }
        Some(acc.into_iter().map(|s| interner.intern(Type::Constant(s))).collect())
    }

    /// The value-pinned scalar key an ARGUMENT node denotes, resolved through its
    /// type (a `Constant` scalar → its [`ShapeKey`]), or `None` when the argument
    /// is not statically a scalar. Mirrors the reference's `static_shape_key?`
    /// gate over a `Type::Constant` argument (so a local bound to `:a` folds just
    /// as a literal `:a` does). Non-literal / dynamic arguments decline.
    fn hash_arg_key(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<ShapeKey> {
        let ty = self.type_of(ast, id, env, interner);
        match interner.get(ty) {
            Type::Constant(s) => Some(scalar_to_shape_key(s)),
            _ => None,
        }
    }

    /// The value type a `HashShape` member holds under a `[]`/`dig`/`values_at`
    /// read: its declared value for a required key, `value | nil` for an optional
    /// key, and `Constant[nil]` for a missing key (Ruby's `Hash#[]` / `#dig`
    /// return nil, not a raise). rigor-rs never builds optional members today, so
    /// the optional arm is defensive parity with the reference `hash_dig_step`.
    fn hash_read_step(
        &self,
        members: &[ShapeMember],
        key: &ShapeKey,
        interner: &mut Interner,
    ) -> TypeId {
        match members.iter().find(|m| &m.key == key) {
            Some(m) if !m.optional => m.value,
            Some(m) => {
                let value = m.value;
                let nil = interner.intern(Type::Constant(Scalar::Nil));
                rigor_types::Algebra::join(interner, value, nil)
            }
            None => interner.intern(Type::Constant(Scalar::Nil)),
        }
    }

    /// Fold a static-key access / projection on a value-pinned `HashShape`
    /// receiver to its precise member type — a faithful port of the reference
    /// `ShapeDispatch`'s HashShape catalogue (the subset spec'd for this slice:
    /// `[]`, `fetch`, `dig`, `has_key?`/`key?`/`member?`/`include?`, `slice`,
    /// `except`, `values_at`, `invert`). `None` declines (leaves the RBS `Hash`
    /// tier to answer, and a typo'd method to witness). Every fold gates on a
    /// value-pinned scalar KEY argument (`static_shape_key?`); a non-literal key
    /// declines. Key identity is `ShapeKey` equality = Ruby `Hash#eql?`.
    ///
    /// Missing-key policy matches the runtime: `[]`/`dig`/`values_at` surface
    /// `Constant[nil]`, while `fetch` (no default, no block) DECLINES on a miss
    /// because Ruby raises `KeyError` — we prefer the conservative RBS answer.
    fn fold_hash_shape_projection(
        &self,
        recv_ty: TypeId,
        method: &str,
        ast: &LoweredAst,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        let members = match interner.get(recv_ty) {
            Type::HashShape(m) => m.clone(),
            _ => return None,
        };

        match method {
            // `h[k]` / `h.fetch(k)` — a single static scalar key. `[]` surfaces
            // `Constant[nil]` for a missing key; `fetch` declines on a miss (it
            // would raise `KeyError`).
            "[]" | "fetch" if args.len() == 1 => {
                let key = self.hash_arg_key(ast, args[0], env, interner)?;
                let present = members.iter().any(|m| m.key == key);
                if method == "fetch" && !present {
                    return None;
                }
                Some(self.hash_read_step(&members, &key, interner))
            }

            // `h.dig(k, …)` — a chain of static keys. Each step reads the key
            // (missing → `Constant[nil]`, Ruby's `Hash#dig` short-circuits on
            // nil); an intermediate `HashShape` recurses, a `Constant[nil]`
            // ends the chain, anything else declines.
            "dig" if !args.is_empty() => {
                let key = self.hash_arg_key(ast, args[0], env, interner)?;
                let step = self.hash_read_step(&members, &key, interner);
                if args.len() == 1 {
                    return Some(step);
                }
                if matches!(interner.get(step), Type::HashShape(_)) {
                    return self
                        .fold_hash_shape_projection(step, "dig", ast, &args[1..], env, interner);
                }
                if matches!(interner.get(step), Type::Constant(Scalar::Nil)) {
                    return Some(step);
                }
                None
            }

            // `h.has_key?(k)` (and aliases) — folds to a precise bool from the
            // statically known key set.
            "has_key?" | "key?" | "member?" | "include?" if args.len() == 1 => {
                let key = self.hash_arg_key(ast, args[0], env, interner)?;
                let present = members.iter().any(|m| m.key == key);
                Some(interner.intern(Type::Constant(Scalar::Bool(present))))
            }

            // `h.values_at(k, …)` — a `Tuple` of the per-key values (missing key
            // → `Constant[nil]`), in ARGUMENT order.
            "values_at" if !args.is_empty() => {
                let mut keys = Vec::with_capacity(args.len());
                for &a in args {
                    keys.push(self.hash_arg_key(ast, a, env, interner)?);
                }
                let vals: Vec<TypeId> =
                    keys.iter().map(|k| self.hash_read_step(&members, k, interner)).collect();
                Some(interner.intern(Type::Tuple(vals)))
            }

            // `h.slice(k, …)` — a sub-shape of the requested keys that are
            // present, in ARGUMENT order (Ruby `Hash#slice` semantics); missing
            // keys are silently omitted, duplicates deduped.
            "slice" if !args.is_empty() => {
                let mut keys = Vec::with_capacity(args.len());
                for &a in args {
                    keys.push(self.hash_arg_key(ast, a, env, interner)?);
                }
                let mut out: Vec<ShapeMember> = Vec::new();
                for key in &keys {
                    if out.iter().any(|m| &m.key == key) {
                        continue;
                    }
                    if let Some(m) = members.iter().find(|m| &m.key == key) {
                        out.push(m.clone());
                    }
                }
                Some(interner.intern(Type::HashShape(out)))
            }

            // `h.except(k, …)` — the receiver shape minus the named keys, keeping
            // RECEIVER order; keys not present are ignored.
            "except" if !args.is_empty() => {
                let mut excluded = Vec::with_capacity(args.len());
                for &a in args {
                    excluded.push(self.hash_arg_key(ast, a, env, interner)?);
                }
                let out: Vec<ShapeMember> =
                    members.iter().filter(|m| !excluded.contains(&m.key)).cloned().collect();
                Some(interner.intern(Type::HashShape(out)))
            }

            // `h.invert` — swap keys and values. Folds only when every value is a
            // `Constant` usable as a key; a duplicate value would alias under
            // inversion, so a collision DECLINES (matching the reference).
            "invert" if args.is_empty() => {
                let mut out: Vec<ShapeMember> = Vec::with_capacity(members.len());
                for m in &members {
                    let vs = match interner.get(m.value) {
                        Type::Constant(s) => s.clone(),
                        _ => return None,
                    };
                    let new_key = scalar_to_shape_key(&vs);
                    if out.iter().any(|o| o.key == new_key) {
                        return None;
                    }
                    let orig = shape_key_to_scalar(&m.key)?;
                    let new_val = interner.intern(Type::Constant(orig));
                    out.push(ShapeMember { key: new_key, value: new_val, optional: false });
                }
                Some(interner.intern(Type::HashShape(out)))
            }

            _ => None,
        }
    }

    /// Implicit-self (`receiver: None`) dispatch entry — the shared home for
    /// receiverless Kernel folds (ADR-0038 inference-cluster spec). Returns
    /// `Some(ty)` when a fold applies, `None` to decline (the caller falls to
    /// `Dynamic[top]`, silent). Folds Kernel `#p` / `#pp` identity AND the
    /// Kernel conversion functions `format`/`sprintf`, `String()`, `Hash()`,
    /// `Integer()`, `Float()` (ADR-0038 spec §3, ported from the reference
    /// `KernelDispatch`). The conversion evaluators live in [`kernel_fold`];
    /// each folds only cases it can prove render byte-identically to Ruby, and
    /// declines (silent) on any doubt — a fold-time error, an arg-count/-type
    /// mismatch, or an oversized result — so a decline is a coverage gap, never
    /// a false positive.
    ///
    /// Kernel `#p(x)` / `#pp(x)` mirror the runtime contract (reference
    /// `KernelDispatch#try_identity_printer`): `p x` returns `x`, `p a, b`
    /// returns `[a, b]`, bare `p` returns `nil`. So:
    ///
    /// | arity  | result                                          |
    /// |--------|-------------------------------------------------|
    /// | 0 args | `Constant[nil]`                                 |
    /// | 1 arg  | the argument's type object UNCHANGED (identity — pins/shapes/`Dynamic` all pass through) |
    /// | N args | `Tuple[t1, …, tn]`                              |
    ///
    /// Note the 0-arg case yields `Constant[nil]` DIRECTLY rather than declining
    /// (the reference declines because its RBS tier already answers `nil`;
    /// rigor-rs has no RBS tier on the implicit-self path, so the fold must
    /// carry the nil itself — probe p03, `for nil`, depends on it).
    ///
    /// Shared by the implicit-self dispatch entry AND the explicit `Kernel.`
    /// receiver spelling in [`Self::type_call`]: `Kernel.p(x)` / `Kernel.format(...)`
    /// dispatch to the same intrinsic via `module_function` (upstream c9d2e473), so
    /// that path routes a `Singleton[Kernel]` receiver here. A FOREIGN receiver
    /// (`obj.format(...)`) never routes here, so a user redefinition on another
    /// class is never hijacked by the fold.
    ///
    /// Guards (decline ⇒ Dynamic, silent), matching the reference's FP envelope:
    /// - a user redefinition of the name: rigor-rs has no scope object, so the
    ///   sanctioned conservative substitute is a FILE-WIDE scan for any
    ///   `def p` / `def pp` — if found, decline that name across the whole file
    ///   (under-emit is safe; probe p07);
    /// - a splat / forwarding argument makes the positional arity (and thus
    ///   identity-vs-`Tuple`) statically unknown ⇒ decline (probe p08).
    fn type_implicit_self_call(
        &self,
        ast: &LoweredAst,
        method: &str,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        // Only a fixed set of Kernel functions is folded on this path; every
        // other implicit-self call declines (a cheap membership test on the hot
        // path). `format`/`sprintf`/`String`/`Hash`/`Integer`/`Float` are the
        // Kernel constant-folds (ADR-0038 spec §3).
        let is_printer = matches!(method, "p" | "pp");
        let is_kernel_fold = matches!(
            method,
            "format" | "sprintf" | "String" | "Hash" | "Integer" | "Float" | "Array" | "rand"
        );
        if !is_printer && !is_kernel_fold {
            return None;
        }
        // User-redefinition guard (conservative file-wide substitute for the
        // reference's scope-aware check): a `def <name>` anywhere in the file
        // disables the fold for that name (under-emit, FP-safe).
        if self.file_defines_method(ast, method) {
            return None;
        }
        // Splat / forwarding guard: rigor-parse lowers a `*a` splat arg to a
        // `Statements` wrapper and a `...` forwarding arg to `Node::Other` (no
        // owned variants). Any such arg means the runtime arity is unknown, so
        // we cannot choose the fold shape — decline for the printer
        // (identity-vs-Tuple undecidable) and the conversion folds.
        // `format`/`sprintf` are the exception: their return is String
        // REGARDLESS of the positional arity, so the reference's literal-string
        // lift still types them under a splat (fixture 53) — nominal String.
        if args
            .iter()
            .any(|&a| matches!(ast.get(a), Node::Other { .. } | Node::Statements { .. }))
        {
            if matches!(method, "format" | "sprintf") && !args.is_empty() {
                return Some(self.nominal_or_untyped("String", interner));
            }
            return None;
        }

        if is_printer {
            return Some(match args {
                [] => interner.intern(Type::Constant(Scalar::Nil)),
                [only] => self.type_of(ast, *only, env, interner),
                many => {
                    let elems: Vec<TypeId> =
                        many.iter().map(|&a| self.type_of(ast, a, env, interner)).collect();
                    interner.intern(Type::Tuple(elems))
                }
            });
        }

        // `Hash(v)` folds on the argument's TYPE (HashShape identity, or an
        // empty HashShape for `nil` / an empty Tuple), not on scalar values, so
        // it is handled before the value-pinning path below.
        if method == "Hash" {
            return self.fold_kernel_hash(ast, args, env, interner);
        }

        // `Array(v)` folds on the argument's TYPE (M2-GO slice 2, reference
        // `try_array`): a Tuple passes through (Array(arr) returns arr), nil
        // collapses to the empty Tuple, a value-pinned scalar wraps
        // (`Array(5)` -> [5]), and ANYTHING else still types nominal Array —
        // the RBS envelope pins `Array(...) -> Array` regardless of the
        // argument (probed: the reference witnesses `Array(c).presence` on
        // `Array[Dynamic[top]]`; rigor-rs was silent).
        if method == "Array" {
            let [only] = args else {
                return None; // 0-arg raises; 2+ has no overload.
            };
            let arg_ty = self.type_of(ast, *only, env, interner);
            return Some(match interner.get(arg_ty).clone() {
                Type::Tuple(_) => arg_ty,
                Type::Constant(Scalar::Nil) => interner.intern(Type::Tuple(vec![])),
                Type::Constant(_) => interner.intern(Type::Tuple(vec![arg_ty])),
                _ => self.nominal_or_untyped("Array", interner),
            });
        }

        // `rand` (M2-GO slice 3), matching the reference's measured overload
        // pick exactly: `rand()` -> Float; ANY 1-arg call -> Integer (probed:
        // even a Float-pinned arg resolves its `(int) -> Integer` overload)
        // EXCEPT a Range argument, which it declines (the Range overload
        // returns the element type). Multi-arg raises -> decline.
        if method == "rand" {
            return match args {
                [] => Some(self.nominal_or_untyped("Float", interner)),
                [only] => {
                    if matches!(ast.get(*only), Node::Range { .. }) {
                        return None;
                    }
                    let arg_ty = self.type_of(ast, *only, env, interner);
                    if self.index.class_name_of(interner, arg_ty) == Some("Range") {
                        return None;
                    }
                    Some(self.nominal_or_untyped("Integer", interner))
                }
                _ => None,
            };
        }

        // The remaining folds (`format`/`sprintf`/`String`/`Integer`/`Float`)
        // fold to a value-pinned `Constant` only when EVERY argument is itself a
        // value-pinned `Constant` scalar. A fold-time DECLINE (arg-type mismatch,
        // unparseable input, oversized result) does NOT go silent: it falls to
        // the nominal fallback below, because the reference does not go silent
        // there either — its literal-string lift / RBS envelope still types
        // `format("%d", "abc")` String and `Integer("abc")` Integer (fixture 53).
        if let Some(scalars) = self.pin_arg_scalars(ast, args, env, interner) {
            let folded: Option<Scalar> = match method {
                "format" | "sprintf" => {
                    // Template = first arg (a Constant string); the rest are the
                    // format arguments.
                    scalars.split_first().and_then(|(template, rest)| {
                        let Scalar::Str(tmpl) = template else {
                            return None;
                        };
                        kernel_fold::sprintf(tmpl, rest).map(Scalar::Str)
                    })
                }
                "String" => match scalars.as_slice() {
                    [only] => Some(Scalar::Str(kernel_fold::ruby_string_of(only))),
                    _ => None,
                },
                "Integer" => match scalars.as_slice() {
                    [only] => kernel_fold::ruby_integer(only, None).map(Scalar::Int),
                    [only, Scalar::Int(base)] => {
                        kernel_fold::ruby_integer(only, Some(*base)).map(Scalar::Int)
                    }
                    _ => None,
                },
                "Float" => match scalars.as_slice() {
                    [only] => kernel_fold::ruby_float(only).map(Scalar::Float),
                    _ => None,
                },
                _ => None,
            };
            if let Some(folded) = folded {
                return Some(interner.intern(Type::Constant(folded)));
            }
        }

        // NOMINAL fallback (ADR ivar-write-mismatch increment b; widened by the
        // compat plan S1): when the args are NOT all value-pinned OR the value
        // fold declined, the calls still type to their conversion class — the
        // reference's RBS pins `Integer(...) -> Integer`, `Float(...) -> Float`,
        // `String(...) -> String` regardless of whether the argument folds
        // (probed: `Float(x).bogus` witnesses on Float), and its literal-string
        // lift types `format`/`sprintf` String on ANY arity ≥ 1. Gated on an
        // arity the conversion accepts so a wrong-arity call (which raises at
        // runtime) stays unfolded. `Hash` was handled above and keeps declining.
        // The shadow-def / splat guards above already ran, so this preserves the
        // reference's FP envelope (a `def Float` in the file still declines — an
        // FP-safe under-emit).
        let nominal_class = match (method, args.len()) {
            ("format" | "sprintf", n) if n >= 1 => Some("String"),
            ("String", 1) | ("Float", 1) | ("Integer", 1 | 2) => Some(method),
            _ => None,
        };
        nominal_class.map(|class| self.nominal_or_untyped(class, interner))
    }

    /// `Kernel#Hash(v)` fold (reference `try_hash`): a `HashShape` argument
    /// passes through unchanged (`Hash(h)` returns `h`); `Constant[nil]` and an
    /// empty `Tuple` (`Hash([])`) collapse to the empty `HashShape`; anything
    /// else declines (the `to_hash` protocol is not decidable from types alone).
    fn fold_kernel_hash(
        &self,
        ast: &LoweredAst,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        let [only] = args else {
            return None;
        };
        let arg_ty = self.type_of(ast, *only, env, interner);
        match interner.get(arg_ty).clone() {
            Type::HashShape(_) => Some(arg_ty),
            Type::Constant(Scalar::Nil) => Some(interner.intern(Type::HashShape(vec![]))),
            Type::Tuple(elems) if elems.is_empty() => {
                Some(interner.intern(Type::HashShape(vec![])))
            }
            _ => None,
        }
    }

    /// True when the file defines an instance method named `name` anywhere (a
    /// top-level or in-class `def name`). Used as the conservative file-wide
    /// user-redefinition guard for the Kernel folds: rigor-rs has no scope
    /// object, so a single `def p` disables the `p` fold file-wide (under-emit,
    /// FP-safe). Singleton `def self.p` lowers with `name: None`, so it does not
    /// trip the guard — matching that it does not shadow the private Kernel
    /// instance method.
    fn file_defines_method(&self, ast: &LoweredAst, name: &str) -> bool {
        ast.iter()
            .any(|(_, n)| matches!(n, Node::Definition { name: Some(m), .. } if m == name))
    }

    fn type_call(
        &self,
        ast: &LoweredAst,
        receiver: NodeId,
        method: &str,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> TypeId {
        // Tier 4 (in-source / RBS `.new`): `X.new` where `X` is a constant
        // naming a class known to the RBS index OR the SourceIndex types to a
        // Nominal INSTANCE of `X`, so a chained `X.new.method` can be checked.
        // We resolve the receiver constant's NAME directly (the bare constant
        // read itself stays Dynamic — we never type a class object). A core
        // (RBS) class wins its core ClassId; a source-only class gets a
        // source-range ClassId from the SourceIndex.
        if method == "new" {
            if let Some(ty) = self.type_dot_new(ast, receiver, args, env, interner) {
                return ty;
            }
            // Not a typeable `.new` (metaclass constructor / unknown constant /
            // a reference constant-constructor lift) ⇒ fall through to the
            // folding / RBS-return cascade below.
        }

        // Tier 4c (ADR-0038): interprocedural literal-tail fold on a `Const.method`
        // SINGLETON call. When `receiver` is a project class/module constant whose
        // OWN singleton `method` provably returns one scalar literal (and is not
        // overridable), the call types that pinned `Constant` — feeding
        // `flow.always-truthy-condition` (`Gitlab::Database.read_only? -> false`).
        // A dedicated, minimal-blast-radius tier: it consults the definers index
        // directly and does NOT type the bare constant as `Singleton`, so no other
        // rule's view of a project constant changes. Any miss falls through
        // (Dynamic, silent) — a project constant still types Dynamic as before.
        if let Node::ConstantRead { name, .. } = ast.get(receiver) {
            if !name.is_empty() {
                if let Some(scalar) = self.source.const_singleton_literal(name, method) {
                    return interner.intern(Type::Constant(scalar));
                }
            }
        }

        // C3a Part A: `self.class.name` / `self.class.to_s` inside a lexical
        // class/module returns the class name as a `String` (the reference
        // unwraps the `Module#name : String?` optional to `String` for
        // witnessing). This lights the `self.class.name.demodulize` /
        // `.underscore` idiom.
        //
        // We match the SPECIFIC `(self.class).name` shape and type ONLY the tail,
        // WITHOUT ever typing `self.class` itself to a witnessable `Singleton`.
        // Typing `self.class` to a project `Singleton` would route
        // `self.class.<class_method>` (calling one of the class's OWN class
        // methods — a ubiquitous idiom) through the class-method witnessing path,
        // which sees only the core RBS surface and cannot verify a project-defined
        // class method ⇒ a flood of false positives (`valid_provider?`,
        // `with_redis`, …). The reference resolves those against the project class
        // and stays silent, so `self.class` itself must remain untyped (Dynamic)
        // here — only the always-String `name`/`to_s` tail is resolved. Toplevel
        // (`enclosing_prefix` empty) declines → silent, matching the reference.
        if (method == "name" || method == "to_s") && args.is_empty() {
            if let Node::Call { receiver: Some(inner), method: inner_m, args: inner_args, .. } =
                ast.get(receiver)
            {
                if inner_m == "class"
                    && inner_args.is_empty()
                    && matches!(ast.get(*inner), Node::SelfExpr { .. })
                {
                    if let Node::SelfExpr { span } = ast.get(*inner) {
                        if !self.enclosing_prefix(*span).is_empty() {
                            return self.nominal_or_untyped("String", interner);
                        }
                    }
                }
            }
        }

        let recv_ty = self.type_of(ast, receiver, env, interner);

        // C3a Part B: `Module#name` / `Class#name` / `#to_s` on a CLASS OBJECT
        // (`Singleton` receiver) returns the class name as a `String`. This is a
        // real (core-RBS) `Singleton` — from the `ConstantRead` arm's zero-FP gate
        // (`Time.name`, `Foo.name` where `Foo` is a known top-level class) — so it
        // is NOT the project-class hazard Part A avoids: a core `Singleton` already
        // witnesses class-method typos against a KNOWN surface. `name`/`to_s` are
        // always valid on a class object and always yield `String`, so this is
        // zero-FP; the returned `String` is NON-nilable, so the possible-nil
        // channel (which resolves the receiver via `class_name_of`, `None` for a
        // `Singleton`) never mints a nilable fact from it.
        if (method == "name" || method == "to_s")
            && matches!(interner.get(recv_ty), Type::Singleton(_))
        {
            return self.nominal_or_untyped("String", interner);
        }

        // Kernel intrinsic explicit-receiver spelling: `Kernel.p(x)` /
        // `Kernel.format(...)` / `Kernel.String(x)` etc. `module_function` exposes
        // each Kernel intrinsic as a public singleton on the Kernel module object,
        // so the explicit `Kernel.` receiver dispatches to the SAME fold as the
        // implicit-self spelling (reference `kernel_owned_call?` +
        // `kernel_module_receiver?`, upstream c9d2e473 — pinned after the rigor-rs
        // port's harness found `Kernel.p` declining while `Kernel.format` folded).
        // Gated on the receiver TYPE resolving to `Singleton[Kernel]` (not the node
        // spelling), so a namespaced user `Kernel` constant — which types Dynamic,
        // never `Singleton[Kernel]` — cannot slip through. The shared fold carries
        // the same user-redefinition / splat decline guards; a non-fold Kernel
        // method (`Kernel.puts`) returns `None` and falls through unchanged.
        let kernel_module_receiver = matches!(
            interner.get(recv_ty),
            Type::Singleton(class) if self.source.class_name_for_id(*class) == Some("Kernel")
        );
        if kernel_module_receiver {
            if let Some(ty) = self.type_implicit_self_call(ast, method, args, env, interner) {
                return ty;
            }
        }

        // Singleton-method RBS return typing (M2-GO slice 4): a CLASS-method
        // call on a core `Singleton` receiver types its RBS return when that
        // return is unanimous across every overload (`Date.today -> Date`,
        // `Time.at -> Time`), so a chained AS-method typo witnesses
        // (`Date.today.end_of_month` — probed: the reference fires, rigor-rs
        // was silent). Divergent-overload returns (`Regexp.last_match`:
        // `MatchData?` vs `String?`) are `None` by the index's
        // all-overloads-agree collapse — decline, fall through (the receiver
        // stays `Singleton`, so class-method typo witnessing is unchanged).
        // `.new` never reaches here (intercepted by `type_dot_new` above).
        if let Type::Singleton(class) = interner.get(recv_ty) {
            let class = *class;
            if let Some(class_name) = self.source.class_name_for_id(class) {
                // A TUPLE return (`Process.wait2 : [Integer, Process::Status]`)
                // types to a `Type::Tuple` of its element classes — the shape the
                // flat `singleton_method_return` slot collapses to `None`. Same
                // all-overloads-agree discipline (the index declines a divergent
                // set), so this only ever REPLACES a `Dynamic[top]` result.
                if let Some(shapes) = self.index.singleton_method_tuple_return(class_name, method) {
                    return self.intern_rbs_tuple(shapes, interner);
                }
                // Collection-shape stage 2a: `Dir.glob(…)` / `Dir[…]` declare a
                // BLOCK overload returning `nil`, which breaks the flat slot's
                // all-overloads-agree collapse even though THIS call site — the
                // block-free path (a block routes to `type_block_call`) —
                // unambiguously yields `Array[String]`. The block-free slot is
                // populated only for methods with BOTH overload kinds and only
                // when every block-free overload agrees on one concrete class,
                // so it can only ever REPLACE a `Dynamic[top]` result.
                if let Some(ret) = self
                    .index
                    .singleton_method_return(class_name, method)
                    .or_else(|| self.index.singleton_method_return_block_free(class_name, method))
                {
                    // Mint the return instance with the type_dot_new id
                    // resolution: a core (CORE_CLASSES) nominal id when
                    // available, else the source-registry id in the high range
                    // (`Time`/`Date` are not in the 9-class core id space; the
                    // rules recover their name via `class_name_for_id_of`).
                    if let Some(class_id) = self.index.class_id(ret) {
                        return interner.intern(Type::Nominal { class: class_id, args: vec![] });
                    }
                    if let Some(class_id) = self.source.class_id(ret) {
                        return interner.intern(Type::Nominal { class: class_id, args: vec![] });
                    }
                }
            }
        }

        // Collection-shape stage 2b: an RBS TOP-LEVEL **object constant**
        // receiver — `ENV`, declared `ENV: RBS::Unnamed::ENVClass` in core RBS.
        // The reference resolves the constant's declared type and types
        // `ENV.keys` as `Array[String]`; rigor-rs left `ENV` `Dynamic[top]`, so
        // the whole `(ENV.keys.select { … } - base).present?` chain went
        // unwitnessed.
        //
        // Only the CALL's RETURN is typed here — the constant itself is never
        // minted as a `Nominal`, so no new undefined-method witnessing surface
        // appears for `ENV.<anything>` itself (a strict under-emit vs the
        // reference). Gated on the SAME lexical shadow predicate the
        // `ConstantRead` arm's C1 gate uses: a project `ENV` constant/class, or
        // a C5-harvested literal constant of that name, declines.
        if let Node::ConstantRead { name, span, .. } = ast.get(receiver) {
            if let Some(decl_class) = self.index.object_constant_class(name) {
                let prefix = self.enclosing_prefix(*span);
                if !self.source.constant_shadowed(name, prefix)
                    && !self.source.project_writes_constant(name)
                    && !self.source.literal_constant_visible_any_file(name, prefix)
                {
                    // NILABLE RETURNS DECLINE. `ENVClass#[]` is `(String) ->
                    // String?`; the reference carries the `String | nil` union
                    // and dispatch on it declines, so typing the chain as a bare
                    // `String` fired `ENV['X'].present?` where the oracle is
                    // silent (measured: 13 of the sweep's 15 FPs on the first
                    // cut of this arm). The flat `method_return` slot drops the
                    // nil bit, so this arm reads `method_return_nilable`
                    // instead. The block-free slot records only bare concrete
                    // returns, so it is non-nilable by construction.
                    if let Some(ret) = self
                        .index
                        .method_return_nilable(decl_class, method)
                        .and_then(|(c, nilable)| (!nilable).then_some(c))
                        .or_else(|| self.index.method_return_block_free(decl_class, method))
                    {
                        if let Some(class_id) =
                            self.index.class_id(ret).or_else(|| self.source.class_id(ret))
                        {
                            return interner
                                .intern(Type::Nominal { class: class_id, args: vec![] });
                        }
                    }
                }
            }
        }

        // Tier 1: constant folding on a value-pinned receiver. Fold only when
        // EVERY argument also types to a value-pinned `Constant` (ADR-0008
        // zero-FP: a non-pinned arg means we can't prove the result, so we
        // decline and widen to the nominal return / Dynamic below — never
        // guess). The nullary case (`args` empty) folds the no-arg core.
        if let Type::Constant(scalar) = interner.get(recv_ty).clone() {
            if let Some(arg_scalars) = self.pin_arg_scalars(ast, args, env, interner) {
                if let Some(folded) = folding::fold(&scalar, method, &arg_scalars) {
                    return interner.intern(Type::Constant(folded));
                }
                // ADR-0008 sidecar fallback: the Rust core declined, but if this
                // is a `sidecar_foldable` pure call and a real-Ruby folder is
                // wired (full-fidelity mode), execute it there. A declined /
                // absent folder leaves the value widened (sound subset).
                if let Some(folder) = self.folder {
                    if folding::sidecar_foldable(folding::scalar_class(&scalar), method) {
                        if let Some(folded) = folder.fold(&scalar, method, &arg_scalars) {
                            return interner.intern(Type::Constant(folded));
                        }
                    }
                }
            }
        }

        // Tier 2: value-pinned shape projection on a `Tuple` receiver (reference
        // ShapeDispatch). A no-arg accessor / constant-index read on a
        // value-pinned Tuple folds to the pinned element or arity — `[1, 2].first`
        // → `1`, `[1, 2].size` → `2`, `[1, 2][0]` → `1` — sharpening `type-of` /
        // `annotate` and chained witnessing (`[1, 2].first.frist` flags on `1`).
        // Only reached for BLOCK-FREE calls (the Call arm routes block calls to
        // `type_block_call`), so a block form never mis-folds here.
        if let Some(folded) = self.fold_tuple_projection(recv_ty, method, ast, args, env, interner) {
            return folded;
        }

        // Tier 2b: value-pinned shape projection on a `HashShape` receiver
        // (reference ShapeDispatch's HashShape catalogue). A static-key lookup /
        // slice / inversion folds to the precise member type — `{ a: 1 }[:a]` →
        // `1`, `{ a: 1 }.has_key?(:a)` → `true`. Declines (→ None) on any
        // uncertainty, so the RBS `Hash` dispatch below still answers (and a
        // typo'd method still witnesses via `class_name_of(HashShape) == Hash`).
        // Block-free only (block calls never reach `type_call`), so no over-fold.
        if let Some(folded) =
            self.fold_hash_shape_projection(recv_ty, method, ast, args, env, interner)
        {
            return folded;
        }

        // Tier 3 (-ish): resolve receiver class -> method return class.
        if let Some(class_name) = self.index.class_name_of(interner, recv_ty) {
            // The instance twin of the singleton tuple arm above
            // (`"a-b".partition("-") : [String, String, String]`): a tuple return
            // types per-position instead of collapsing to `Dynamic[top]`.
            if let Some(shapes) = self.index.method_tuple_return(class_name, method) {
                return self.intern_rbs_tuple(shapes, interner);
            }
            // Collection-shape stage 2c: the instance twin of the singleton
            // block-free arm above. `String#split: (…) -> Array[String] | (…)
            // { … } -> self` loses its return to the flat slot's
            // all-overloads-agree collapse; a BLOCK-FREE `x.split(':', 2)` (the
            // only kind that reaches `type_call`) is unambiguously an `Array`,
            // which is what the reference's `block_required: false` overload
            // selection resolves too.
            if let Some(ret_class) = self
                .index
                .method_return(class_name, method)
                .or_else(|| self.index.method_return_block_free(class_name, method))
            {
                if let Some(class_id) = self.index.class_id(ret_class) {
                    return interner.intern(Type::Nominal {
                        class: class_id,
                        args: vec![],
                    });
                }
            }
        }

        // Tier 4b (ADR-0023): in-source method RETURN inference. A SOURCE-class
        // receiver (a project `X.new` instance) whose called method has a
        // precomputed concrete CORE return interns that CORE nominal, so the
        // chained call witnesses against the real RBS (e.g. `user.full_name :
        // String`, then `.lenght` flags against String). The source receiver is
        // recovered via `class_name_for_id_of` (the core `class_name_of` above
        // returns `None` for a source-range id, so this never overlaps tier 3).
        // Any miss — no source receiver, no inferred return, or an unregistered
        // core name — falls through to Dynamic (silent; zero-FP).
        if let Some(src_name) = self.source.class_name_for_id_of(interner, recv_ty) {
            let src_name = src_name.to_string();
            if let Some(ret_core) = self.source.method_return(&src_name, method) {
                if let Some(class_id) = self.index.class_id(ret_core) {
                    return interner.intern(Type::Nominal { class: class_id, args: vec![] });
                }
            }
            // Tier 4b call-site PARAMETER BINDING (ADR-0023): a source method
            // whose return DEFERS to a positional argument. We bind the ARG's
            // type to the rooted param, then re-derive the core return — the
            // param-independent path above never fired for it (its tail is param-
            // rooted, hence Dynamic under the empty build-time env). The whole
            // safety argument is a STRICT under-approximation: we resolve only
            // when the bound arg AND every chain step land on a concrete CORE
            // class via the same `method_return` table tier 3 uses; any miss
            // (arg out of range, non-core arg, a chain step with no core return)
            // ⇒ Dynamic (silent). No AST/node-id is needed — the descriptor
            // carries the param index + the no-arg core chain, so this is fully
            // cross-file safe. No re-entry into `infer_method_returns` (the
            // chain walks the core return table only, never an in-source body),
            // so there is no recursion into the build pass.
            if let Some(pb) = self.source.param_bound_return(&src_name, method) {
                if let Some(core_class) =
                    self.resolve_param_bound(ast, pb, args, env, interner)
                {
                    if let Some(class_id) = self.index.class_id(&core_class) {
                        return interner.intern(Type::Nominal { class: class_id, args: vec![] });
                    }
                }
            }
        }

        // Tier 5: unknown -> Dynamic[top].
        interner.untyped()
    }

    /// Resolve a tier-4b call-site PARAMETER-BINDING descriptor against the
    /// actual call arguments, returning the concrete CORE class NAME the method
    /// returns for THIS call, or `None` to decline (Dynamic, silent).
    ///
    /// 1. The arg at `pb.param_index` must exist (arg count > index) — fewer args
    ///    than required positional params ⇒ decline.
    /// 2. Type that arg under the CURRENT call-site `env` and resolve its CORE
    ///    class; a Dynamic / non-core / source-only arg ⇒ decline (we can only
    ///    witness against core/RBS classes, the existing witness gate).
    /// 3. Walk `pb.chain` through the SAME `method_return` table tier 3 uses: each
    ///    no-arg core method must yield a registered core return; any miss ⇒
    ///    decline. The chain is core-only and uses the already-built index — it
    ///    cannot re-enter the in-source return inference, so there is no recursion
    ///    into the build pass and no fixpoint in this slice.
    fn resolve_param_bound(
        &self,
        ast: &LoweredAst,
        pb: &ParamBoundReturn,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<String> {
        // Gate 1: the bound positional arg must be present.
        let &arg_id = args.get(pb.param_index)?;
        // Gate 2: type the arg under the call-site env; keep only a concrete CORE
        // class (a Dynamic / Constant-of-unknown / source-only carrier ⇒ None).
        let arg_ty = self.type_of(ast, arg_id, env, interner);
        let mut class_name = self.index.class_name_of(interner, arg_ty)?.to_string();
        if !self.index.knows_class(&class_name) {
            return None;
        }
        // Gate 3: walk the no-arg core chain. Each step must yield a registered
        // core return; otherwise decline.
        for step in &pb.chain {
            let ret = self.index.method_return(&class_name, step)?;
            if !self.index.knows_class(ret) {
                return None;
            }
            class_name = ret.to_string();
        }
        Some(class_name)
    }

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
    fn type_block_call(
        &self,
        ast: &LoweredAst,
        receiver: NodeId,
        method: &str,
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
        // The receiver must resolve to a concrete class the index models; a
        // Dynamic / unknown receiver ⇒ silent (never guess the block return).
        let Some(class_name) = self.index.class_name_of(interner, recv_ty) else {
            return interner.untyped();
        };
        // The block-overload return for `class_name#method`. `None` ⇒ the block
        // form isn't precisely modeled ⇒ decline to Dynamic (silent).
        let Some(ret_class) = self.index.method_return_with_block(class_name, method) else {
            return interner.untyped();
        };
        match self.index.class_id(ret_class) {
            Some(class_id) => interner.intern(Type::Nominal { class: class_id, args: vec![] }),
            None => interner.untyped(),
        }
    }

    /// Type each argument and, if *every* one is a value-pinned `Constant`,
    /// return the owned scalars in order — the input [`folding::fold`] needs to
    /// compute a byte-exact result. Returns `None` the moment any argument is
    /// not a pinned `Constant` (Dynamic / Nominal / unknown), so the caller
    /// declines to fold rather than guessing (ADR-0008 zero-FP).
    fn pin_arg_scalars(
        &self,
        ast: &LoweredAst,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<Vec<Scalar>> {
        let mut out = Vec::with_capacity(args.len());
        for &arg in args {
            let ty = self.type_of(ast, arg, env, interner);
            match interner.get(ty) {
                Type::Constant(scalar) => out.push(scalar.clone()),
                _ => return None,
            }
        }
        Some(out)
    }

    /// Walk the top-level statement sequence in source order, binding each
    /// `LocalVariableWrite`'s name to the type of its value expression, and
    /// return the resulting [`TypeEnv`].
    ///
    /// This is the minimal flow needed so a later `s.lenght` can see `s :
    /// Constant["Hello"]`. Nested scopes / reassignment narrowing are out of
    /// scope for the tracer bullet.
    // TODO(spec): real flow-sensitive scoping + narrowing across branches (ADR-0022).
    pub fn build_toplevel_env(&self, ast: &LoweredAst, interner: &mut Interner) -> TypeEnv {
        let mut env = TypeEnv::new();
        let body = match ast.get(ast.root()) {
            Node::Program { body, .. } => body.clone(),
            _ => return env,
        };
        for stmt in body {
            // A program body may wrap statements directly or via a Statements node.
            self.bind_statement(ast, stmt, &mut env, interner);
        }
        env
    }

    /// Flow-sensitive local CONSTANT propagation (ADR-0022 first substrate
    /// slice). For every `if`/`unless`/ternary predicate NOT lexically inside a
    /// loop / block, record the [`TypeId`] the predicate folds to under the
    /// branch-joined flow environment that dominates it. The companion rule
    /// `flow.always-truthy-condition` fires only when that recorded type is a
    /// `Type::Constant`, so this query is the zero-FP keystone: it must be a
    /// strict UNDER-approximation of the reference's flow folder (witness set ⊆
    /// reference), achieved by **widening on any doubt**.
    ///
    /// Soundness model (why a constant here can never be a false positive):
    /// - **Straight-line writes** bind the local to the RHS type, exactly as the
    ///   flat env does.
    /// - **`if`/`unless` branches** are evaluated independently and JOINED: a
    ///   local keeps a binding only when both branches agree on the IDENTICAL
    ///   `TypeId`; any disagreement (or a local written in only one branch)
    ///   widens it to `Dynamic`. This is what stops `x = 5; if c; x = f; end;
    ///   if x` from folding `x` to `5` — the flat env's central unsoundness.
    /// - **Loops / blocks / `case` / `begin`-`rescue` / `&&`-`||` / any other
    ///   node** widen EVERY local written anywhere in their span (a loop iterates
    ///   0..n times; a closure may write a captured local; a `case`/`begin` arm
    ///   is conditional) and are NOT descended for predicate snapshots. Skipping
    ///   loop/block predicates matches the reference's own envelope; declining
    ///   the others is an extra conservative miss (never an FP).
    /// - **`def` / `class` / `module` bodies** are independent scopes: they are
    ///   descended with a FRESH local env (Ruby method/class bodies do not see
    ///   the enclosing locals) but INHERIT the loop/block suppression flag, so a
    ///   `def` nested in a block keeps its predicates suppressed (reference parity)
    ///   while a top-level `def`'s predicates are recorded. A nested scope never
    ///   perturbs the enclosing env.
    ///
    /// Writes are collected once (span-keyed) and widening filters that list by
    /// span-containment — orphan-proof, the same discipline as
    /// the dead-assignment collector.
    pub fn always_truthy_snapshots(
        &self,
        ast: &LoweredAst,
        interner: &mut Interner,
    ) -> HashMap<NodeId, TypeId> {
        let mut out = HashMap::new();
        let mut writes = collect_flow_writes(ast);
        writes.extend(indexed_flow_writes(ast, self.source));
        let body = match ast.get(ast.root()) {
            Node::Program { body, .. } => body.clone(),
            _ => return out,
        };
        let mut env = TypeEnv::new();
        self.flow_eval_scope(ast, &body, &mut env, false, None, DefKind::Instance, &writes, interner, &mut out);
        out
    }

    /// Thread `env` through a scope's statements in source order. `self_qual` /
    /// `self_kind` carry the enclosing class/module QUALIFIED name + method kind
    /// so an implicit-self predicate call can be resolved for the interprocedural
    /// literal-tail fold (ADR-0038); `None` at the top level (a receiverless call
    /// there has no project self to resolve against).
    #[allow(clippy::too_many_arguments)]
    fn flow_eval_scope(
        &self,
        ast: &LoweredAst,
        stmts: &[NodeId],
        env: &mut TypeEnv,
        in_loop_or_block: bool,
        self_qual: Option<&str>,
        self_kind: DefKind,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut HashMap<NodeId, TypeId>,
    ) {
        for &s in stmts {
            self.flow_eval_stmt(ast, s, env, in_loop_or_block, self_qual, self_kind, writes, interner, out);
        }
    }

    /// Evaluate one statement's effect on `env`, recording predicate snapshots.
    #[allow(clippy::too_many_arguments)]
    fn flow_eval_stmt(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        env: &mut TypeEnv,
        in_loop_or_block: bool,
        self_qual: Option<&str>,
        self_kind: DefKind,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut HashMap<NodeId, TypeId>,
    ) {
        match ast.get(id) {
            Node::Statements { body, .. } => {
                let body = body.clone();
                self.flow_eval_scope(ast, &body, env, in_loop_or_block, self_qual, self_kind, writes, interner, out);
            }
            Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                // A value expression may itself write OTHER locals (`x = (y = 5)`)
                // or capture-write via a block — widen those first, then bind.
                let vspan = ast.get(value).span();
                widen_flow_writes(writes, vspan, env, interner);
                // An if-EXPRESSION assigned to a local (`strategies = if
                // Gitlab::Database.read_write?; …`) still carries a predicate the
                // always-truthy rule visits — record its snapshot here (the
                // statement walk only reaches an `if` that is a bare statement).
                // The branch writes are already conservatively widened above, so
                // this only ADDS the predicate snapshot (no env perturbation).
                if !in_loop_or_block {
                    if let Node::If { predicate, .. } = ast.get(value) {
                        let predicate = *predicate;
                        let pty = self
                            .flow_predicate_type(ast, predicate, env, self_qual, self_kind, interner);
                        out.insert(value, pty);
                    }
                }
                let ty = self.type_of(ast, value, env, interner);
                env.insert(name, ty);
            }
            // `a, b = rhs` — destructure the RHS and rebind every target. This
            // is the arm that closes the multi-write flow-write FP: without it
            // an earlier `x = 5` survived the rebind and `if x` folded to a
            // constant.
            Node::MultiWrite { targets, value, .. } => {
                let (targets, value) = (targets.clone(), *value);
                // Same discipline as the single-target arm: the RHS may itself
                // write other locals — widen those first, then bind.
                let vspan = ast.get(value).span();
                widen_flow_writes(writes, vspan, env, interner);
                let rhs = self.type_of(ast, value, env, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    env.insert(name, ty);
                }
            }
            Node::LocalVariableOpWrite { name, .. } => {
                // `x += 1` / `x ||= 5` reads-then-writes; the result is not a
                // tracked constant in this slice — widen.
                let name = name.clone();
                let u = interner.untyped();
                env.insert(name, u);
            }
            Node::If { predicate, then_body, else_body, .. } => {
                let (predicate, then_body, else_body) =
                    (*predicate, then_body.clone(), else_body.clone());
                if !in_loop_or_block {
                    let pty = self.flow_predicate_type(
                        ast, predicate, env, self_qual, self_kind, interner,
                    );
                    out.insert(id, pty);
                }
                // Independently evaluate each branch from the dominating env, then
                // join: a binding survives only if both branches agree exactly.
                let mut then_env = env.clone();
                self.flow_eval_scope(
                    ast, &then_body, &mut then_env, in_loop_or_block, self_qual, self_kind, writes, interner, out,
                );
                let mut else_env = env.clone();
                self.flow_eval_scope(
                    ast, &else_body, &mut else_env, in_loop_or_block, self_qual, self_kind, writes, interner, out,
                );
                *env = join_flow_envs(&then_env, &else_env, interner);
                // A predicate may contain a write (`if (x = f)`); widen post-join.
                let pspan = ast.get(predicate).span();
                widen_flow_writes(writes, pspan, env, interner);
            }
            Node::Definition { body, singleton_name, .. } => {
                // Independent scope: fresh local env, inherited suppression flag.
                // The self KIND flips to singleton inside a `def self.x` (so an
                // implicit-self call there resolves against the owner's singleton
                // table); the enclosing class QUALIFIED name is unchanged.
                let (body, kind) = (
                    body.clone(),
                    if singleton_name.is_some() { DefKind::Singleton } else { DefKind::Instance },
                );
                let mut fresh = TypeEnv::new();
                self.flow_eval_scope(
                    ast, &body, &mut fresh, in_loop_or_block, self_qual, kind, writes, interner, out,
                );
            }
            Node::ClassDef { body, name, .. } | Node::ModuleDef { body, name, .. } => {
                // Independent scope: fresh local env, inherited suppression flag.
                // Extend the lexical self-qualified name so a nested class/module's
                // implicit-self calls resolve against the right owner; a body-level
                // call defaults to instance kind until a `def self.x` flips it.
                let (body, child_qual) = (body.clone(), qualify_self(self_qual, name));
                let mut fresh = TypeEnv::new();
                self.flow_eval_scope(
                    ast, &body, &mut fresh, in_loop_or_block, Some(&child_qual), DefKind::Instance, writes, interner, out,
                );
            }
            // Loop / case / begin-rescue / logical / call(+block) / any other node:
            // widen every local written in the span, do not descend for snapshots.
            other => {
                widen_flow_writes(writes, other.span(), env, interner);
            }
        }
    }

    /// The recorded flow type for an `if`/`unless`/ternary predicate. Tries the
    /// ADR-0038 interprocedural literal-tail fold on an IMPLICIT-SELF predicate
    /// call first (resolved against the enclosing class `self_qual`/`self_kind`) —
    /// this is the one fold that needs the self context `type_of` lacks — then
    /// falls back to the ordinary `type_of` (which itself folds a `Const.method`
    /// predicate via `type_call`'s tier 4c). Producing a `Type::Constant` here is
    /// what makes `flow.always-truthy-condition` fire.
    fn flow_predicate_type(
        &self,
        ast: &LoweredAst,
        predicate: NodeId,
        env: &TypeEnv,
        self_qual: Option<&str>,
        self_kind: DefKind,
        interner: &mut Interner,
    ) -> TypeId {
        if let Node::Call { receiver: None, method, block_body, .. } = ast.get(predicate) {
            if block_body.is_empty() {
                let method = method.clone();
                if let Some(q) = self_qual {
                    if let Some(scalar) = self.source.implicit_self_literal(q, self_kind, &method) {
                        return interner.intern(Type::Constant(scalar));
                    }
                }
            }
        }
        self.type_of(ast, predicate, env, interner)
    }

    /// Bind a single statement into `env` if it is a local write; recurse
    /// through a `Statements` wrapper. Other statements have no binding effect.
    fn bind_statement(&self, ast: &LoweredAst, id: NodeId, env: &mut TypeEnv, interner: &mut Interner) {
        match ast.get(id) {
            Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                let ty = self.type_of(ast, value, env, interner);
                env.insert(name, ty);
            }
            Node::MultiWrite { targets, value, .. } => {
                let (targets, value) = (targets.clone(), *value);
                let rhs = self.type_of(ast, value, env, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    env.insert(name, ty);
                }
            }
            Node::Statements { body, .. } => {
                for s in body.clone() {
                    self.bind_statement(ast, s, env, interner);
                }
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // ADR-0038 Slice 1 — `call.possible-nil-receiver` on the threaded flow-eval
    // -----------------------------------------------------------------------

    /// Compute the per-call-node nil-receiver snapshot map (ADR-0038 Slice 1):
    /// `call node id -> non-nil core arm C` for every bare-local receiver that is
    /// certainly `C | nil` and unguarded at the use. The rules layer's
    /// `check_nil_receiver` fires from this map (applying the method-absent-on-
    /// NilClass / present-on-C gate). This REPLACES the prior `enclosing_def`
    /// span-scan, so a nilable local now witnesses in block / top-level scopes,
    /// not only inside a named `def`.
    ///
    /// It threads two facts straight-line through the program, DESCENDING into
    /// block bodies:
    /// - `tenv` — a TYPE env, INHERITED (cloned) into block bodies so a slice /
    ///   `.new` receiver typed in an OUTER scope (`random_array = Array.new(n){…}`)
    ///   is visible to a source in a NESTED block (`select_subset = random_array[
    ///   0..n]`). Widened precisely (only written locals) on unmodeled constructs.
    /// - `nenv` — a NILABILITY fact map, `local -> non-nil core arm C` (the local
    ///   is currently `C | nil`). It starts EMPTY in every block body.
    ///
    /// ## FP-safety (ADR-0038 §2/§3 decline backstop)
    ///
    /// - **Same-block-body locality.** `nenv` is FRESH per block, so a fact never
    ///   crosses INTO a block. Block parameters are not lowered (so cannot be
    ///   cleared by name); the fresh env makes a param shadowing an outer local
    ///   unable to leak a stale fact — the shadowing FP class is structurally
    ///   impossible.
    /// - **Unmodeled ⇒ clear all.** ANY statement not in the modeled set (control
    ///   flow, multi-assign, ivar write, …) CLEARS ALL `nenv` facts. Multi-assign
    ///   targets are invisible in the lowered arena, so a per-name scan could miss
    ///   a reassignment; the clear-all is the bulletproof choice for the direct
    ///   fire gate.
    /// - **Block descent clears outer facts.** After descending a block, ALL outer
    ///   `nenv` facts are cleared (a block capture may invisibly reassign an outer
    ///   local).
    /// - **Guards clear the fact.** A `.nil?`/`present?`/`blank?`/`presence` call
    ///   or a safe-nav call on the local removes it (narrowed); an `&&`/`||`
    ///   operand context clears all facts (unmodeled narrowing in Slice 1).
    ///
    /// Residual (documented Slice 1 limit): a multi-assign that reassigns a
    /// SOURCE receiver's TYPE leaves `tenv` stale (targets invisible), which could
    /// feed a wrong NEW source. Contrived and survey-absent; closed when
    /// multi-assign is modeled. Every fire is gated by `fp_audit.py` on the survey.
    pub fn nilable_receiver_snapshots(
        &self,
        ast: &LoweredAst,
        interner: &mut Interner,
    ) -> HashMap<NodeId, &'static str> {
        let mut out = HashMap::new();
        let body = match ast.get(ast.root()) {
            Node::Program { body, .. } => body.clone(),
            _ => return out,
        };
        let mut writes = collect_flow_writes(ast);
        writes.extend(indexed_flow_writes(ast, self.source));
        let mut tenv = TypeEnv::new();
        let mut nenv: HashMap<String, &'static str> = HashMap::new();
        let mut penv: HashSet<String> = HashSet::new();
        self.nil_flow_scope(ast, &body, &mut tenv, &mut nenv, &mut penv, &writes, interner, &mut out);
        out
    }

    /// Thread `(tenv, nenv, penv)` through a scope's statements in source order.
    /// `penv` is the `Array.new`-Nominal-provenance set (ADR-0039 §2) — the locals
    /// currently bound to an array the reference keeps `Nominal[Array]` (not a
    /// `Tuple`), the only receivers the array-slice possible-nil source may fire on.
    /// It travels on the tenv side (inherited into blocks; widened by tenv's rules).
    #[allow(clippy::too_many_arguments)]
    fn nil_flow_scope(
        &self,
        ast: &LoweredAst,
        stmts: &[NodeId],
        tenv: &mut TypeEnv,
        nenv: &mut HashMap<String, &'static str>,
        penv: &mut HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
    ) {
        for &s in stmts {
            self.nil_flow_stmt(ast, s, tenv, nenv, penv, writes, interner, out);
        }
    }

    /// Apply one statement's effect on `(tenv, nenv, penv)` and record any nil uses.
    #[allow(clippy::too_many_arguments)]
    fn nil_flow_stmt(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        nenv: &mut HashMap<String, &'static str>,
        penv: &mut HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
    ) {
        match ast.get(id) {
            Node::Statements { body, .. } => {
                let body = body.clone();
                self.nil_flow_scope(ast, &body, tenv, nenv, penv, writes, interner, out);
            }
            Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                // Record uses in the RHS (and descend any block it carries) BEFORE
                // rebinding — a use of a currently-nilable local reads the fact.
                self.nil_flow_expr(ast, value, tenv, nenv, penv, writes, interner, out);
                let src = self.nilable_source_class(ast, value, tenv, penv, interner);
                let prov = self.array_new_nominal_provenance(ast, value, tenv, interner);
                let vty = self.type_of(ast, value, tenv, interner);
                tenv.insert(name.clone(), vty);
                // Rebinding always refreshes the provenance (any non-`Array.new`
                // RHS clears it).
                if prov {
                    penv.insert(name.clone());
                } else {
                    penv.remove(&name);
                }
                match src {
                    Some(c) => {
                        nenv.insert(name, c);
                    }
                    None => {
                        nenv.remove(&name);
                    }
                }
            }
            // `a, b = rhs` — record the RHS uses, then rebind every target to
            // its destructured slot type and DROP the per-name nil / `Array.new`
            // facts. Dropping is the FP-safe direction (a dropped `C | nil` fact
            // can only silence `call.possible-nil-receiver`, never add a
            // firing), and it is what the binder's `soften_optional_slot` says
            // anyway: a destructured slot never carries a manufactured nil.
            Node::MultiWrite { targets, value, .. } => {
                let (targets, value) = (targets.clone(), *value);
                self.nil_flow_expr(ast, value, tenv, nenv, penv, writes, interner, out);
                let rhs = self.type_of(ast, value, tenv, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    nenv.remove(&name);
                    penv.remove(&name);
                    tenv.insert(name, ty);
                }
            }
            Node::LocalVariableOpWrite { name, .. } => {
                // `x += …` / `x ||= …` reads-then-writes ⇒ the nil possibility is
                // narrowed/replaced; drop every fact and widen the type.
                let name = name.clone();
                nenv.remove(&name);
                penv.remove(&name);
                let u = interner.untyped();
                tenv.insert(name, u);
            }
            Node::Call { .. } => {
                self.nil_flow_expr(ast, id, tenv, nenv, penv, writes, interner, out);
            }
            Node::Definition { body, .. }
            | Node::ClassDef { body, .. }
            | Node::ModuleDef { body, .. } => {
                // Independent scope: fresh `tenv`/`nenv`/`penv`, no effect on the
                // enclosing scope.
                let body = body.clone();
                let mut t = TypeEnv::new();
                let mut n: HashMap<String, &'static str> = HashMap::new();
                let mut p: HashSet<String> = HashSet::new();
                self.nil_flow_scope(ast, &body, &mut t, &mut n, &mut p, writes, interner, out);
            }
            // Any other statement (`if`/`unless`/`while`/`case`/logical/begin/
            // multi-assign/ivar-write/…) is UNMODELED in Slice 1: widen `tenv` and
            // `penv` for the locals it writes, and CLEAR ALL `nenv` facts (decline
            // backstop — no fact survives an unmodeled construct). No descent.
            other => {
                let span = other.span();
                widen_flow_writes(writes, span, tenv, interner);
                widen_penv_writes(writes, span, penv);
                nenv.clear();
            }
        }
    }

    /// Evaluate an expression for nil-receiver USES: record `call -> arm` for a
    /// bare-local receiver in `nenv`, clear the fact on a guard/safe-nav call, and
    /// descend a block body with a FRESH `nenv` + INHERITED `(tenv, penv)`.
    #[allow(clippy::too_many_arguments)]
    fn nil_flow_expr(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        nenv: &mut HashMap<String, &'static str>,
        penv: &mut HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
    ) {
        match ast.get(id) {
            Node::Call { receiver, method, args, block_body, safe_nav, span, .. } => {
                let receiver = *receiver;
                let method = method.clone();
                let args = args.clone();
                let block_body = block_body.clone();
                let safe_nav = *safe_nav;
                let call_span = *span;
                // Recurse the receiver first (a nested use like `a.b` in `a.b.c`).
                if let Some(r) = receiver {
                    self.nil_flow_expr(ast, r, tenv, nenv, penv, writes, interner, out);
                }
                if let Some(r) = receiver {
                    if let Node::LocalVariableRead { name, .. } = ast.get(r) {
                        let is_guard = matches!(
                            method.as_str(),
                            "nil?" | "present?" | "blank?" | "presence"
                        );
                        // Record the use: currently-nilable bare local, plain (not
                        // safe-nav) call, non-guard method. `check_nil_receiver`
                        // applies the NilClass-absent / arm-present gate.
                        if !safe_nav && !is_guard {
                            if let Some(&arm) = nenv.get(name) {
                                out.insert(id, arm);
                            }
                        }
                        // A guard or safe-nav call on the local narrows nil away
                        // for SUBSEQUENT uses ⇒ drop the fact.
                        if safe_nav || is_guard {
                            nenv.remove(name);
                        }
                    }
                }
                for a in &args {
                    self.nil_flow_expr(ast, *a, tenv, nenv, penv, writes, interner, out);
                }
                if !block_body.is_empty() {
                    // Same-block locality: descend with a FRESH `nenv`, inheriting
                    // (cloning) `(tenv, penv)`. Afterwards CLEAR ALL outer `nenv`
                    // (a block capture may invisibly reassign an outer local), and
                    // widen `tenv`/`penv` for locals the block visibly writes (a
                    // capture-write must not leave a stale type/provenance behind).
                    let mut btenv = tenv.clone();
                    let mut bnenv: HashMap<String, &'static str> = HashMap::new();
                    let mut bpenv = penv.clone();
                    self.nil_flow_scope(
                        ast, &block_body, &mut btenv, &mut bnenv, &mut bpenv, writes, interner, out,
                    );
                    nenv.clear();
                    widen_flow_writes(writes, call_span, tenv, interner);
                    widen_penv_writes(writes, call_span, penv);
                }
            }
            Node::Logical { left, right, .. } => {
                // `&&`/`||` — unmodeled narrowing in Slice 1. Clear all facts
                // (decline), then recurse for block/call reachability.
                let (left, right) = (*left, *right);
                nenv.clear();
                self.nil_flow_expr(ast, left, tenv, nenv, penv, writes, interner, out);
                self.nil_flow_expr(ast, right, tenv, nenv, penv, writes, interner, out);
            }
            _ => {}
        }
    }

    /// Whether `rhs_id` is an `Array.new(...)` the REFERENCE keeps `Nominal[Array]`
    /// (not a `Tuple`) — the FP-safe provenance for the possible-nil array-slice
    /// source (ADR-0039 §2). True iff `Array.new` with ZERO args, or a first arg
    /// that types to `Constant(Int(n))` with `n > ARRAY_NEW_TUPLE_LIMIT`. A small /
    /// non-constant / non-integer size ⇒ false: the reference MIGHT `Tuple` it
    /// (it may fold a constant rigor-rs leaves `Dynamic`), so claiming Nominal
    /// would over-fire. Syntactic on the `Array` constant + a Constant size arg;
    /// never a bare `Nominal[Array]` (which a `.map` result the reference Tuples
    /// also carries).
    fn array_new_nominal_provenance(
        &self,
        ast: &LoweredAst,
        rhs_id: NodeId,
        tenv: &TypeEnv,
        interner: &mut Interner,
    ) -> bool {
        let Node::Call { receiver: Some(recv), method, args, .. } = ast.get(rhs_id) else {
            return false;
        };
        if method != "new" {
            return false;
        }
        let Node::ConstantRead { name, .. } = ast.get(*recv) else {
            return false;
        };
        if name != "Array" {
            return false;
        }
        // Zero-arg `Array.new` ⇒ the reference declines the tuple lift ⇒ Nominal.
        if args.is_empty() {
            return true;
        }
        // Else the FIRST arg must be a Constant integer strictly above the tuple
        // limit (small / non-constant / non-integer size ⇒ decline, FP-safe).
        let first = args[0];
        let fty = self.type_of(ast, first, tenv, interner);
        matches!(interner.get(fty), Type::Constant(Scalar::Int(n)) if *n > ARRAY_NEW_TUPLE_LIMIT)
    }

    /// The non-nil core arm `C` of a nilable SOURCE expression `value`, or `None`
    /// (not a modeled nil source ⇒ the local is treated non-nilable).
    ///
    /// Two sources (both zero-FP by construction):
    /// (a) **String slice** `str[Range]` — the single-`Range`-arg `#[]` form on a
    ///     non-`Constant` `String` receiver. RBS types it `String?`, so the
    ///     non-nil arm is `String`. A `Constant` receiver is declined: the
    ///     reference constant-folds a string LITERAL slice to a concrete non-nil
    ///     value (`"hello"[0..2]` ⇒ `"hel"`), so it never sees `String | nil`;
    ///     rigor-rs types a string literal as `Constant` and declines, matching.
    ///     A `String.new` / interpolated / method-return String is `Nominal` in
    ///     both (unfolded) and fires.
    /// (a2) **Array slice** `arr[Range]` ⇒ `Array?` — but ONLY when the receiver is
    ///     an `Array.new`-Nominal-provenance array (ADR-0039 §2 syntactic
    ///     provenance): a bare local in `penv`, or a direct `Array.new(nominal)`
    ///     call. NEVER a bare `Nominal[Array]` — the reference types array literals
    ///     and `Array.new(n≤16)` (and `.map`/… results) as `Tuple` whose slice is
    ///     non-nil, so firing off the type env would over-fire on those.
    /// (b) **Certain nilable RBS return** on a KNOWN core receiver
    ///     (`String#byteslice -> String?`). A `Constant` receiver is declined for
    ///     the same folding-parity reason — the keystone.
    fn nilable_source_class(
        &self,
        ast: &LoweredAst,
        value_id: NodeId,
        tenv: &TypeEnv,
        penv: &HashSet<String>,
        interner: &mut Interner,
    ) -> Option<&'static str> {
        let Node::Call { receiver: Some(recv), method, args, block_body, .. } = ast.get(value_id)
        else {
            return None;
        };
        if !block_body.is_empty() {
            return None;
        }
        let recv = *recv;
        let method = method.clone();
        let args = args.clone();
        // (c) `Regexp.last_match` — a CORE SINGLETON returning an optional (P2,
        // 2026-07-17). `Regexp.last_match() -> MatchData?`; `Regexp.last_match(n)`
        // / `(name) -> String?`. The receiver is a `ConstantRead "Regexp"` (both
        // `Regexp` and `::Regexp` lower to this bare name), whose type is a
        // `Singleton` — `class_name_of` below returns `None` for it, so this MUST
        // be matched syntactically here, before the receiver-class resolution. The
        // syntactic name gate mirrors the reference resolving `Regexp.last_match`
        // against core RBS; a project constant coincidentally named `Regexp` is not
        // a realistic hazard. The arm depends only on the ARITY (spec
        // `docs/notes/20260717-p2-optional-local-nil-spec.md`, widened by the
        // compat plan S2): EVERY 1-arity overload returns `String?` —
        // `(Integer) -> String?`, `(Symbol|String name) -> String?` — so the
        // reference resolves a 1-arg call to `String?` even when the arg is
        // non-literal (fixture 65). Arity, not arg shape, decides:
        //   - zero args         ⇒ `MatchData` (deref `#[]` / `#begin` / …),
        //   - one non-splat arg ⇒ `String`    (deref `#gsub` / `#upcase` / …),
        //   - splat / multi arg ⇒ DECLINE (arity unknown / raises — never guess).
        if method == "last_match" {
            if let Node::ConstantRead { name, .. } = ast.get(recv) {
                if name == "Regexp" {
                    return match args.as_slice() {
                        [] => Some("MatchData"),
                        // A splat lowers to `Statements` (receiver-call args) or
                        // `Other` (`...` forwarding) — arity unknown, decline.
                        [only] if !matches!(
                            ast.get(*only),
                            Node::Other { .. } | Node::Statements { .. }
                        ) =>
                        {
                            Some("String")
                        }
                        _ => None,
                    };
                }
            }
        }
        let rty = self.type_of(ast, recv, tenv, interner);
        // Folding-parity keystone (shared by both sources): a `Constant` receiver
        // is folded by the reference to a concrete non-nil value ⇒ decline.
        if matches!(interner.get(rty), Type::Constant(_)) {
            return None;
        }
        let cls = self.index.class_name_of(interner, rty)?;
        if !self.index.knows_class(cls) {
            return None;
        }
        let is_range_slice =
            method == "[]" && args.len() == 1 && matches!(ast.get(args[0]), Node::Range { .. });
        // (a) String slice — `str[Range]` ⇒ `String?`. String only (see doc).
        if is_range_slice && cls == "String" {
            return Some("String");
        }
        // (a2) Array slice — `arr[Range]` ⇒ `Array?`, provenance-gated (§2).
        if is_range_slice && cls == "Array" {
            let provenanced = match ast.get(recv) {
                Node::LocalVariableRead { name, .. } => penv.contains(name),
                _ => self.array_new_nominal_provenance(ast, recv, tenv, interner),
            };
            return provenanced.then_some("Array");
        }
        // (b) certain nilable RBS return.
        match self.index.method_return_nilable(cls, &method) {
            Some((core, true)) if self.index.knows_class(core) => Some(core),
            _ => None,
        }
    }

    // -----------------------------------------------------------------------
    // `is_a?` / `case-when` class narrowing (census mechanism 1, spec
    // docs/notes/20260807-class-narrowing-slice-spec.md) + the disjoint-guard
    // suppression (docs/notes/20260808-disjoint-guard-suppression.md)
    // -----------------------------------------------------------------------

    /// Compute the per-call-node class-narrowing snapshot map: `call node id ->
    /// narrowed class name C` for every bare-local receiver whose local was
    /// narrowed from `Dynamic`/`Top` to `Nominal[C]` by an `is_a?`/`kind_of?`/
    /// `instance_of?` guard (`if`/`elsif`/`unless`/ternary — mirror of the
    /// reference's `narrow_class_other`, `narrowing.rb:2425`) or a single-
    /// static-constant `case`/`when` clause (`case_when_scopes`,
    /// `narrowing.rb:374`). The rules layer's `check_narrowed_call` fires
    /// `call.undefined-method` from this map — and ONLY that rule (spec pitfall
    /// 7: wiring more rules over a narrowed receiver is out of slice).
    ///
    /// ## FP-safety envelope (every decline load-bearing)
    ///
    /// Strict subset of the reference's preconditions, so every recorded use is
    /// one the reference also narrows:
    /// - **Predicate shape**: an atomic guard is `local.is_a?(C)` (or
    ///   `kind_of?`/`instance_of?`/`C === local`) — bare `LocalVariableRead`
    ///   operand, no safe-nav, no block, exactly one `ConstantRead` argument.
    ///   Since stage 3a-1 those compose through `&&`, `||` and `!`
    ///   ([`Typer::analyse_predicate`]); chains and ivar receivers still
    ///   decline (ADR-0038 "unmodeled ⇒ decline"). A `Logical` in a VALUE
    ///   position still MINTS nothing (stage 3a-2); since stage 3b-1 it no
    ///   longer clears facts either — its operands are descended so an OUTER
    ///   fact's uses are recorded, and only a rebind in its span kills.
    /// - **Lexical constant resolution**: `C` resolves at the predicate's
    ///   lexical prefix; a project declaration shadowing `C`
    ///   ([`SourceIndex::constant_shadowed`]) declines entirely — we never
    ///   narrow to a project nominal in this slice.
    /// - **Dynamic/Top carriers only** (`narrow_class_other`): the local's type
    ///   in the threaded env must be `Dynamic`/`Top` (or unbound ⇒ untyped).
    ///   `Nominal`/union/scalar carriers are untouched.
    /// - **Per-edge guard maps** (stage 3a-1 reworded this invariant): an
    ///   atomic guard still narrows the TRUTHY edge only — the then-branch for
    ///   `if`/ternary, the else-branch for `unless`. The falsey edge is no
    ///   longer categorically unnarrowed: `!`, and the `&&`/`||` edge algebra
    ///   above it, can SWAP a fact onto it (`if !v.is_a?(C) … else USE end`
    ///   fires on the reference). A `case`/`when` clause is UNCHANGED — it
    ///   narrows only under its own single-constant condition (multi-condition
    ///   unions decline; no falsey threading between clauses).
    /// - **Early-return propagation** (`eval_if:486`/`:495`), stage 3a-1 runs
    ///   it in BOTH directions: when exactly one branch terminates (final
    ///   statement `return`/`raise` — a conservative approximation), the
    ///   OPPOSITE edge's guard map applies to the statements after the
    ///   conditional; per local it is declined if ANY write to that local lands
    ///   inside the conditional's span, and it is skipped entirely when BOTH
    ///   branches terminate (the code after is unreachable and the reference
    ///   emits nothing there).
    /// - **Invalidation**: any write to the local (`LocalVariableWrite`/
    ///   `OpWrite`/`MultiWrite` target), a [`MUTATOR_METHODS`] receiver call,
    ///   or a mutated-argument position (the [`collect_flow_writes`]/
    ///   [`indexed_flow_writes`] span machinery) kills the fact; any unmodeled
    ///   statement clears ALL facts (decline backstop).
    /// - **Unmodeled statement forms** (stage 3b-1,
    ///   docs/notes/20260807-narrowing-stage3-spec.md): the arms enumerated in
    ///   [`Typer::class_flow_stmt`] DESCEND instead of declining — inert leaf
    ///   statements, ivar/gvar/cvar/constant writes, `begin`/`rescue` bodies, a
    ///   loop PREDICATE and statement-position `&&`/`||` — and the literal
    ///   containers in [`Typer::class_flow_expr`] likewise. Every one of them
    ///   records uses under facts that already exist and MINTS NOTHING. A
    ///   `while`/`until`/`for` BODY and survival past a `begin`/loop/`case`
    ///   stay declined.
    /// - **Block bodies**: facts do NOT enter a `block_body` (fresh fact env,
    ///   ADR-0038 §3) — the archetype's `value.deep_transform_keys! { … }`
    ///   receiver sits OUTSIDE the block and is recorded before descent; after
    ///   a block descent all outer facts are cleared (a capture may invisibly
    ///   reassign a local).
    /// - **Position gate** (docs/notes/20260807-block-narrowing-position-rule
    ///   .md): a block body and a `case`/`when` clause narrow ONLY from
    ///   statement position or an assignment RHS. Consumed as a call receiver,
    ///   as an argument, or as a `return` operand they narrow nothing — the
    ///   reference types those positions with `ExpressionTyper`, which threads
    ///   no scope. `if`/ternary is the documented exception: its branches
    ///   narrow in every position, and only the early-return propagation PAST
    ///   it is statement-only (review R2, above).
    ///
    /// ## The DISJOINT-guard suppression ([`ClassNarrowing::dead`])
    ///
    /// The same walk carries a SECOND, opposite-direction fact
    /// (docs/notes/20260808-disjoint-guard-suppression.md). Where the narrowing
    /// map covers "our carrier is COARSER than the reference's", this covers
    /// "our carrier is PRECISE and the reference's is `Bot`": the reference's
    /// `narrow_nominal_to_class` / `narrow_shape_to_class` /
    /// `narrow_constant_to_class` (`narrowing.rb:2381,2404,2364`) collapse a
    /// guarded local to `Bot` when the guard class is DISJOINT from the
    /// carrier's class — dispatch through `Bot` then witnesses nothing, so the
    /// reference is silent on every call whose receiver is that local, for
    /// EVERY rule (measured: `undefined-method`, `wrong-arity`,
    /// `argument-type-mismatch`). rigor-rs never narrowed a precise carrier, so
    /// `check_call` kept firing on the pre-guard type — a live FP.
    ///
    /// The suppression is bounded to what our side can PROVE, because here
    /// silence is the fix and an over-broad rule loses real diagnostics:
    /// - the carrier must map to a class name through
    ///   [`CoreIndex::class_name_of`] — the SAME function the undefined-method
    ///   rule dispatches on, so the class we suppress against is exactly the
    ///   class we would have witnessed against;
    /// - `is_a?`/`kind_of?`/`===` suppress only on
    ///   [`ClassOrdering::Disjoint`], i.e. both names resolve in the core index
    ///   AND both ancestor chains are complete. `Unknown` (an unresolvable or
    ///   project class, a truncated chain) does NOT suppress, even though the
    ///   reference's SHAPE carriers collapse to `Bot` there too — proving that
    ///   arm needs a claim about which carriers the reference holds as a
    ///   `Tuple`/`HashShape` rather than a `Nominal`, and the probe corpus
    ///   refutes every cheap proxy for it (`h = *spec` and `h = []; h << 1`
    ///   witness on the reference under an unknown guard class; `h = [1, 2]`
    ///   and `h = [1, 2].compact` do not). Declining costs coverage only.
    /// - `instance_of?` suppresses on any NAME MISMATCH — the reference's
    ///   `exact:` path returns `Bot` unconditionally once the names differ
    ///   (`narrowing.rb:2384`, `subclass_of?:2440`), so no hierarchy fact is
    ///   needed;
    /// - a `case`/`when` clause suppresses only when EVERY condition is a
    ///   static constant that collapses (the reference unions the per-condition
    ///   narrowings, so `when Hash, Array` on an Array keeps the carrier);
    /// - once `Bot`, the local stays `Bot` on BOTH edges of any further guard
    ///   and past a nested conditional's join, and is killed only by a rebind
    ///   — the same invalidation the narrowing fact gets.
    pub fn class_narrowing_pass(
        &self,
        ast: &LoweredAst,
        interner: &mut Interner,
    ) -> ClassNarrowing {
        let mut out = ClassNarrowing::default();
        let body = match ast.get(ast.root()) {
            Node::Program { body, .. } => body.clone(),
            _ => return out,
        };
        let mut writes = collect_flow_writes(ast);
        writes.extend(indexed_flow_writes(ast, self.source));
        let mut tenv = TypeEnv::new();
        let mut cenv = Facts::default();
        let coarse = coarse_locals(ast, &body);
        self.class_flow_scope(
            ast, &body, &mut tenv, &mut cenv, &coarse, &writes, interner, &mut out, true,
        );
        out
    }

    /// The narrowed-call half of [`Typer::class_narrowing_pass`], for callers
    /// that only need `call node id -> narrowed class`.
    pub fn class_narrowing_snapshots(
        &self,
        ast: &LoweredAst,
        interner: &mut Interner,
    ) -> HashMap<NodeId, String> {
        self.class_narrowing_pass(ast, interner).calls
    }

    /// Thread `(tenv, cenv)` through a scope's statements in source order.
    /// `stmt_position` is the POSITION the whole statement list sits in (see
    /// [`Typer::class_flow_expr`]): a method/program body and the branch bodies
    /// of a statement-position conditional are statement position; the clause
    /// bodies of an expression-position `case`/ternary inherit `false`.
    #[allow(clippy::too_many_arguments)]
    fn class_flow_scope(
        &self,
        ast: &LoweredAst,
        stmts: &[NodeId],
        tenv: &mut TypeEnv,
        cenv: &mut Facts,
        coarse: &HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut ClassNarrowing,
        stmt_position: bool,
    ) {
        for &s in stmts {
            self.class_flow_stmt(ast, s, tenv, cenv, coarse, writes, interner, out, stmt_position);
        }
    }

    /// Apply one statement's effect on `(tenv, cenv)` and record narrowed uses.
    #[allow(clippy::too_many_arguments)]
    fn class_flow_stmt(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        cenv: &mut Facts,
        coarse: &HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut ClassNarrowing,
        stmt_position: bool,
    ) {
        match ast.get(id) {
            Node::Statements { body, .. } => {
                let body = body.clone();
                self.class_flow_scope(ast, &body, tenv, cenv, coarse, writes, interner, out, stmt_position);
            }
            // An assignment RHS keeps the statement's own position (oracle
            // probes s8/p1 for `=`, x3 for a multi-write, x4 for an op-write:
            // the reference narrows a block/`case` on the RHS of all three).
            Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                // Record uses in the RHS BEFORE rebinding — the RHS reads the
                // pre-write fact (`value = value.frobnicate if value.is_a?(…)`).
                self.class_flow_expr(ast, value, tenv, cenv, coarse, writes, interner, out, stmt_position);
                let vty = self.type_of(ast, value, tenv, interner);
                tenv.insert(name.clone(), vty);
                // Rebinding invalidates the narrowing (probe a4; `scope.rb:194`).
                cenv.kill_local(&name);
            }
            Node::MultiWrite { targets, value, .. } => {
                let (targets, value) = (targets.clone(), *value);
                self.class_flow_expr(ast, value, tenv, cenv, coarse, writes, interner, out, stmt_position);
                let rhs = self.type_of(ast, value, tenv, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    cenv.kill_local(&name);
                    tenv.insert(name, ty);
                }
            }
            Node::LocalVariableOpWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                self.class_flow_expr(ast, value, tenv, cenv, coarse, writes, interner, out, stmt_position);
                cenv.kill_local(&name);
                let u = interner.untyped();
                tenv.insert(name, u);
            }
            Node::Call { .. } => {
                self.class_flow_expr(ast, id, tenv, cenv, coarse, writes, interner, out, stmt_position);
            }
            // A `return E` evaluates its values in the current facts (`return
            // value.frobnicate if …` must witness). No fact effect: statements
            // after a `return` in the same list are unreachable, and the
            // reference's evaluator threads the same scope past them.
            //
            // The operand is EXPRESSION position: the reference is silent on a
            // `case`/block narrowed under `return` (probes s12, p7), so a
            // returned value may READ an outer fact but never establishes one
            // of its own.
            Node::Return { values, .. } => {
                let values = values.clone();
                for v in values {
                    self.class_flow_expr(ast, v, tenv, cenv, coarse, writes, interner, out, false);
                }
            }
            Node::If { .. } => {
                self.class_flow_if(ast, id, tenv, cenv, coarse, writes, interner, out, stmt_position);
            }
            Node::Case { .. } => {
                self.class_flow_case(ast, id, tenv, cenv, coarse, writes, interner, out, stmt_position);
            }
            Node::Definition { body, .. }
            | Node::ClassDef { body, .. }
            | Node::ModuleDef { body, .. } => {
                // Independent scope: fresh envs (INCLUDING a freshly computed
                // coarse-carrier set — local NAMES do not cross a `def`/`class`
                // boundary), no effect on the enclosing one.
                let body = body.clone();
                let mut t = TypeEnv::new();
                let mut c = Facts::default();
                let inner = coarse_locals(ast, &body);
                self.class_flow_scope(
                    ast, &body, &mut t, &mut c, &inner, writes, interner, out, true,
                );
            }
            // ---- stage 3b-1 (docs/notes/20260807-narrowing-stage3-spec.md) ----
            // Every arm below either DESCENDS to record uses under facts the
            // stage-1/2 machinery already established, or is a provable no-op.
            // NONE of them mints a fact.
            //
            // An INERT LEAF statement — a bare read or a literal. It binds
            // nothing, calls nothing, and (being a leaf) can contain no write,
            // so both halves of the `other` arm's decline are no-ops on it.
            // Routing it to `other` is what killed the whole recovered-carrier
            // op-assign family: `cache[v] ||= v.use` has no owned variant and
            // lowers through `collect_recoverable_children` into a `Statements`
            // carrier whose children are the bare reads `cache`, `v` and only
            // THEN the call, so the facts died before the call was reached
            // (probes d1/d2/d10a/d10b/d11/e3/e9). The literal/`ConstantRead`
            // members are load-bearing for `BeginRescue`, whose flat `body`
            // interleaves the protected statements with each clause's lowered
            // exception `ConstantRead`s (probe f7).
            //
            // Stage 3a-3 splits the bare LOCAL read out: it is still inert for
            // the per-local facts, but it MENTIONS a name that may be a chain
            // root, and this slice's invalidation kills a chain on any mention
            // (see the `class_flow_expr` arm for why that is a strict superset
            // of the reference's rule).
            Node::LocalVariableRead { name, .. } => {
                let name = name.clone();
                cenv.kill_chains_rooted_at(&name);
            }
            Node::ConstantRead { .. }
            | Node::VariableRead { .. }
            | Node::SelfExpr { .. }
            | Node::StringLit { .. }
            | Node::IntegerLit { .. }
            | Node::FloatLit { .. }
            | Node::SymbolLit { .. }
            | Node::NilLit { .. }
            | Node::TrueLit { .. }
            | Node::FalseLit { .. } => {}
            // `@x = E` / `$gx = E` / `@@cx = E` / `X = E`. None of these can
            // rebind a LOCAL, so every fact SURVIVES the statement (probes
            // e1/e6/e7/e8) — the half of the old `other` treatment that was
            // pure loss. A write nested in `E` still kills by span, exactly as
            // `other`'s `widen_flow_writes` + a `kill_cenv_writes` do.
            //
            // DESCENDING `E` to record the use (spec rows d4-d7) is DECLINED.
            // The spec's build measured it as the one arm that surfaces a
            // PRE-EXISTING carrier-fidelity gap: `narrow_class_other` narrows
            // Dynamic/Top carriers only, and rigor-rs types a `Node::Logical`
            // (and any project-method return that ends in one) as
            // `Dynamic[top]` where the reference produces a UNION — so the
            // reference's gate declines and ours does not. Two live FPs over
            // the standing sweep sat on this arm (gitlab-foss
            // `lib/ci/inputs/base_input.rb:30` via `spec_hash = spec || {}`,
            // `lib/gitlab/encrypted_configuration.rb:70` via a `deserialize`
            // whose body ends in `… || {}`); master emits the same FPs for a
            // bare-statement use, so the gap is orthogonal to this slice and
            // must be closed on the carrier side before d4-d7 can ship.
            Node::InstanceVariableWrite { span, .. }
            | Node::VariableWrite { span, .. }
            | Node::ConstantWrite { span, .. } => {
                let span = *span;
                widen_flow_writes(writes, span, tenv, interner);
                kill_cenv_writes(writes, span, cenv);
            }
            // `begin`/`rescue`/`else`/`ensure`. The flat `body` holds the
            // protected statements, each clause's exception constants and
            // clause body, the `else` body and the `ensure` body in source
            // order (`ast.rs:1516`), so ONE descent covers d19/f7/f8.
            //
            // A `rescue => e` capture REBINDS `e` with no `LocalVariableWrite`
            // node, so it is invisible to `collect_flow_writes`: kill the bound
            // names explicitly BEFORE descending (probe `rescuebind` — the
            // reference narrows the bound name to the exception class and says
            // `for StandardError`; keeping the stale fact would emit
            // `for String`, a live FP). Killing before the protected body costs
            // the coverage of a use that precedes the clause — a subset.
            //
            // Recording under exception paths is safe: facts are only KILLED
            // inside, never minted, and a runtime path that skips a rebind only
            // makes a recorded fact more true. Survival PAST the `begin` is
            // declined (widen + clear, exactly as `other` did) even though the
            // reference does keep it (probe post1) — unprobed at spec time and
            // a strict subset.
            Node::BeginRescue { body, clauses, span, .. } => {
                let (body, span) = (body.clone(), *span);
                let bound: Vec<String> =
                    clauses.iter().filter_map(|c| c.bound_name.clone()).collect();
                for name in &bound {
                    cenv.kill_local(name);
                }
                self.class_flow_scope(ast, &body, tenv, cenv, coarse, writes, interner, out, stmt_position);
                widen_flow_writes(writes, span, tenv, interner);
                // The body was DESCENDED into `cenv` itself, so a rebind inside
                // already removed the fact: no edge evidence is needed and no
                // span kill either (probes `bot_in_begin`, `bot_after_begin`).
                join_cenv(cenv, &[]);
            }
            // `while`/`until`/`for`. The predicate (for `for`, the COLLECTION)
            // is evaluated ONCE before the body, in the enclosing scope — the
            // same EXPRESSION position an `if` predicate gets. The reference
            // fires on a narrowed use in a `while`/`until` predicate and in a
            // `for` collection, even when the `for` index rebinds that very
            // local (probes g1/g1b/g1c/g1d — the collection is evaluated before
            // the rebind).
            //
            // The BODY is DECLINED. `Node::Loop` cannot distinguish `for`,
            // whose index rebind is INVISIBLE in the arena (`ast.rs:1501` drops
            // the index target) and where the reference is measured SILENT
            // (probe f10a: `for v in list` then `v.use`), from `while`/`until`,
            // where it fires (f10b/d21). Descending would be a live FP. Stage
            // 3b-2 lands an arena discriminator first.
            Node::Loop { predicate, span, .. } => {
                let (predicate, span) = (*predicate, *span);
                if let Some(p) = predicate {
                    self.class_flow_expr(ast, p, tenv, cenv, coarse, writes, interner, out, false);
                }
                widen_flow_writes(writes, span, tenv, interner);
                // The BODY is not descended, so a rebind inside it is invisible
                // to the edge evidence: keep the entry `Bot` but kill by span
                // (probe `bot_after_while`).
                join_cenv(cenv, &[]);
                kill_cenv_writes(writes, span, cenv);
            }
            // A statement-position `&&`/`||`. Descend both operands to record
            // uses of an OUTER fact — valid on both edges, and the reference
            // records them (probes f1/f2). Establishing a fact FROM the
            // operands is stage 3a-2 and is NOT done here. The operands are
            // EXPRESSION position. Afterwards: `widen_flow_writes` keeps the
            // `tenv` effect byte-identical to the `other` arm this replaces,
            // and `kill_cenv_writes` drops the fact of any local the
            // conditionally-executed right operand may rebind (a fact with no
            // write in the span survives — probe f4b, where the reference also
            // keeps it).
            Node::Logical { left, right, span, .. } => {
                let (left, right, span) = (*left, *right, *span);
                self.class_flow_expr(ast, left, tenv, cenv, coarse, writes, interner, out, false);
                self.class_flow_expr(ast, right, tenv, cenv, coarse, writes, interner, out, false);
                widen_flow_writes(writes, span, tenv, interner);
                kill_cenv_narrowed(writes, span, cenv);
            }
            // Any other statement (`case`/`in`/lambda/range/…) is UNMODELED:
            // widen `tenv` for the locals it writes and CLEAR ALL facts
            // (decline backstop). No descent.
            other => {
                let span = other.span();
                widen_flow_writes(writes, span, tenv, interner);
                // No descent, so no edge evidence: the entry `Bot` rides through
                // (nothing inside can widen it) and a rebind in the span kills.
                join_cenv(cenv, &[]);
                kill_cenv_writes(writes, span, cenv);
            }
        }
    }

    /// Evaluate an expression for narrowed-local USES: record `call -> C` for a
    /// bare-local receiver in `cenv`, descend a block body with a FRESH fact
    /// env, and kill facts a call's contained writes/mutations invalidate.
    ///
    /// `stmt_position` carries the POSITION rule (docs/notes/20260807-block-
    /// narrowing-position-rule.md): a block body and a `case`/`when` clause
    /// narrow only when the construct sits in statement position or on an
    /// assignment RHS. Reaching an expression as a call RECEIVER, as an
    /// ARGUMENT, or as a `return` OPERAND drops it to `false`, and no
    /// narrowing is established anywhere beneath — the reference's
    /// `ExpressionTyper` threads no scope through those positions.
    #[allow(clippy::too_many_arguments)]
    fn class_flow_expr(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        cenv: &mut Facts,
        coarse: &HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut ClassNarrowing,
        stmt_position: bool,
    ) {
        match ast.get(id) {
            Node::Call { receiver, method: _, args, block_body, safe_nav, span, .. } => {
                let receiver = *receiver;
                let args = args.clone();
                let block_body = block_body.clone();
                let safe_nav = *safe_nav;
                let call_span = *span;
                // ---- stage 3a-3: chain-address bookkeeping ------------------
                // Is THIS call the pure address read of a LIVE chain fact
                // (`h.last` while `(h, "last")` is narrowed)? Two things hang
                // off the answer, and both are load-bearing:
                //
                //  * the read must NOT invalidate its own fact. The reference
                //    agrees — `invalidate_chain_after_call` runs at the
                //    STATEMENT `eval_call` and `h.last.frobnicate_zzz`'s outer
                //    receiver is a `CallNode`, not a stable root, so nothing is
                //    dropped and a second read narrows too (probe `f11`, the
                //    reference fires twice);
                //  * the root read beneath it must not trip the
                //    root-MENTION kill below.
                let live_address = stable_chain_address(ast, id)
                    .filter(|addr| cenv.chains.contains_key(addr));
                // Recurse the receiver first (a nested use like `a.b` in `a.b.c`).
                // A receiver is EXPRESSION position (probes s5-s7, s11, p3).
                // Skipped for a live address read: its receiver is the bare root
                // local, whose descent would kill the very fact being read.
                if let Some(r) = receiver {
                    if live_address.is_none() {
                        self.class_flow_expr(
                            ast, r, tenv, cenv, coarse, writes, interner, out, false,
                        );
                    }
                }
                // INVALIDATION, the strict superset of
                // `invalidate_chain_after_call` this slice specced: a call whose
                // receiver READS the root, other than the pure address read
                // above, drops every chain rooted there (probe `c7d`, `h.pop`,
                // reference-silent). The reference's own rule is narrower — it
                // fires only for a statement-position call — so ours declines
                // some rows it keeps; killing is always a subset.
                //
                // A call whose receiver is the ADDRESS (`h.last.strip`,
                // `h.last << y`) is NOT a root-receiver call and does not
                // invalidate, on either engine (probes `n_call_on_address`,
                // `n_address_receiver_call`, both reference=1).
                if live_address.is_none() {
                    if let Some(r) = receiver {
                        if let Node::LocalVariableRead { name, .. } = ast.get(r) {
                            cenv.kill_chains_rooted_at(name);
                        }
                    }
                }
                // Record the CHAIN use: `<addr>.<m>(…)` where `<addr>` carries a
                // live fact. The outer call's own arguments and block are
                // irrelevant — the reference narrows the RECEIVER expression
                // (`method_chain_narrowing_for` gates the node it types, which
                // is the address, not its caller), so `h.last.zzz(1)` and
                // `h.last.zzz { }` both fire (probes `m_use_with_args`,
                // `m_use_with_block`). Safe-nav on the outer call declines, as
                // everywhere in this slice.
                //
                // A `Bot` chain fact records into `out.dead` instead, exactly
                // as a `Bot` LOCAL fact does below: the reference's meet
                // collapsed the address, so it has no dispatch surface and the
                // rules layer must emit nothing there. Safe-nav does not gate
                // the `Bot` half (same reasoning as the local twin: the
                // safe-nav decline is about which SHAPES we narrow, not about
                // dispatch through `Bot`).
                if let Some(r) = receiver {
                    if let Some(addr) = stable_chain_address(ast, r) {
                        match cenv.chains.get(&addr) {
                            Some(ClassFact::Bot) => {
                                out.dead.insert(id);
                            }
                            Some(ClassFact::Narrowed(c)) if !safe_nav => {
                                out.calls.insert(id, c.clone());
                            }
                            _ => {}
                        }
                    }
                }
                // Record the use: a plain (not safe-nav) call on a bare local
                // currently narrowed to `C`. Recorded BEFORE any invalidation
                // below — the receiver read happens before the call's effects
                // (`value.deep_transform_keys! { … }` must witness).
                //
                // The `Bot` fact is recorded on the SAME node key but under
                // safe-nav too: `narrow_*_to_class` collapsed the receiver, and
                // a safe-nav dispatch through `Bot` witnesses just as little as
                // a plain one (the safe-nav decline on `Narrowed` is about
                // which SHAPES the reference narrows, not about dispatch).
                if let Some(r) = receiver {
                    if let Node::LocalVariableRead { name, .. } = ast.get(r) {
                        match cenv.locals.get(name) {
                            Some(ClassFact::Bot) => {
                                out.dead.insert(id);
                            }
                            Some(ClassFact::Narrowed(c)) if !safe_nav => {
                                out.calls.insert(id, c.clone());
                            }
                            _ => {}
                        }
                    }
                }
                // An argument is EXPRESSION position (probes s9, s13, p2).
                for a in &args {
                    self.class_flow_expr(ast, *a, tenv, cenv, coarse, writes, interner, out, false);
                }
                if !block_body.is_empty() {
                    // Block-scope discipline (ADR-0038 §3): descend with a FRESH
                    // fact env + inherited (cloned) `tenv`; afterwards clear ALL
                    // outer facts (a capture may invisibly reassign a local) and
                    // widen `tenv` for locals the block visibly writes.
                    //
                    // POSITION GATE (docs/notes/20260807-block-narrowing-
                    // position-rule.md): descend ONLY from statement position.
                    // The reference fires on a guard narrowed inside a block
                    // whose call is a statement or an assignment RHS (probes
                    // s1-s4, s8, s10, x3-x5 — safe-nav included, which is why
                    // PR #63's `if !safe_nav` decline was the wrong axis) and
                    // is SILENT once the call's value is consumed as a receiver
                    // (s5-s7, s11), as an argument (s9, s13) or as a `return`
                    // operand (s12). Skipping the descent drops every narrowed
                    // recording inside the block — a strict subset of the
                    // reference, never an FP; the conservative clear/widen
                    // effects below still apply either way.
                    //
                    // The `Bot` facts DO cross into the block: they are not a
                    // narrowing claim about a Dynamic carrier but the statement
                    // that the reference's guarded scope bound the local to
                    // `Bot`, and that scope is exactly what the block body runs
                    // under (probes `bot_into_block`, `bot_into_block_doend`).
                    // A rebind inside the block drops it from `bcenv`, so the
                    // join below drops it from the outer env too
                    // (`bot_block_rebind`, where the reference fires).
                    //
                    // Stage 3a-3: CHAIN facts do NOT cross into a block. The
                    // reference does carry them (probe `n_into_block` fires),
                    // but a chain address is invalidated by a call on its root
                    // and a block body can invisibly reach the root through a
                    // capture — the same reason `Narrowed` locals stay out.
                    // A recorded coverage gap, never an FP.
                    let mut block_edge: Option<Facts> = None;
                    if stmt_position {
                        let mut btenv = tenv.clone();
                        let mut bcenv = Facts {
                            locals: cenv
                                .locals
                                .iter()
                                .filter(|(_, f)| **f == ClassFact::Bot)
                                .map(|(k, f)| (k.clone(), f.clone()))
                                .collect(),
                            chains: HashMap::new(),
                        };
                        self.class_flow_scope(
                            ast, &block_body, &mut btenv, &mut bcenv, coarse, writes, interner, out, true,
                        );
                        block_edge = Some(bcenv);
                    }
                    match block_edge {
                        Some(edge) => join_cenv(cenv, std::slice::from_ref(&edge)),
                        // Not descended (expression position): no edge evidence,
                        // so keep the entry `Bot` and kill by span instead.
                        None => {
                            join_cenv(cenv, &[]);
                            kill_cenv_writes(writes, call_span, cenv);
                        }
                    }
                    widen_flow_writes(writes, call_span, tenv, interner);
                }
                // Invalidation: kill the fact of every local with a recorded
                // write/mutation span INSIDE this call — covers a
                // `MUTATOR_METHODS` receiver (`value.merge!(…)`), a mutated
                // positional argument (`fill(value)`), and a write nested in an
                // argument (`f(value = x)`). A REBIND in the arguments was
                // already threaded by the expression-position write arms, so
                // what is left here is MUTATION, which cannot revive a `Bot`
                // (probe `bot_mutator_use`).
                kill_cenv_narrowed(writes, call_span, cenv);
            }
            Node::Logical { left, right, span, .. } => {
                // `&&`/`||`. Stage 3b-1: the up-front `cenv.clear()` is GONE.
                // Recording a use is POSITION-INDEPENDENT — the reference fires
                // on a use of an outer fact inside a logical operand in
                // statement, argument and assignment-RHS position alike (probes
                // f1-f4). Establishing a fact from the operands stays out of
                // slice (3a-2), so nothing new is minted here; a
                // conditionally-executed rebind inside the operands is killed
                // by span, exactly as the `Call` arm does.
                let (left, right, span) = (*left, *right, *span);
                self.class_flow_expr(ast, left, tenv, cenv, coarse, writes, interner, out, false);
                self.class_flow_expr(ast, right, tenv, cenv, coarse, writes, interner, out, false);
                kill_cenv_narrowed(writes, span, cenv);
            }
            // A write in EXPRESSION position (`f(value = x, value.frobnicate)`)
            // must thread its rebind IMMEDIATELY: Ruby evaluates arguments
            // left-to-right and the reference threads scope through them, so a
            // use AFTER the rebind reads the new binding — the post-call
            // `kill_cenv_writes` alone would leave the stale fact live for the
            // remaining sibling arguments (adversarial-review R1). Mirrors the
            // statement arms.
            Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                self.class_flow_expr(ast, value, tenv, cenv, coarse, writes, interner, out, false);
                let vty = self.type_of(ast, value, tenv, interner);
                tenv.insert(name.clone(), vty);
                cenv.kill_local(&name);
            }
            Node::MultiWrite { targets, value, .. } => {
                let (targets, value) = (targets.clone(), *value);
                self.class_flow_expr(ast, value, tenv, cenv, coarse, writes, interner, out, false);
                let rhs = self.type_of(ast, value, tenv, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    cenv.kill_local(&name);
                    tenv.insert(name, ty);
                }
            }
            Node::LocalVariableOpWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                self.class_flow_expr(ast, value, tenv, cenv, coarse, writes, interner, out, false);
                cenv.kill_local(&name);
                let u = interner.untyped();
                tenv.insert(name, u);
            }
            // A ternary is an expression-position `Node::If` (Prism parses it as
            // an IfNode); `propagate_if_branches` (`scope_indexer.rb:2742`)
            // gives it the same treatment as a statement `if` — EXCEPT the
            // early-return propagation, which is a STATEMENT-evaluator behavior
            // (`eval_if:481`) and unprobed for expression position (R2), so it
            // is gated off here.
            Node::If { .. } => {
                self.class_flow_if(ast, id, tenv, cenv, coarse, writes, interner, out, false);
            }
            // A `case` in an ASSIGNMENT RHS narrows its clause bodies like a
            // statement `case` (probe p1) — hence `stmt_position` rather than a
            // hard `false`; reached as a receiver/argument/`return` operand the
            // flag is already `false` and the clauses narrow nothing (p2, p3,
            // p7). Unlike `if`/ternary, which narrows its branches in EVERY
            // position (p4, p8) and so is left alone.
            Node::Case { .. } => {
                self.class_flow_case(ast, id, tenv, cenv, coarse, writes, interner, out, stmt_position);
            }
            // ---- stage 3b-1: pure-descent expression containers ------------
            // Recording a narrowed use is position-INDEPENDENT (probe f3), so
            // descending a literal container closes d14-d17 wherever it sits.
            // The elements are EXPRESSION position: a container is not a
            // statement list, so a BLOCK inside one is not descended (probe
            // blk4 — `x = [[1].map { … }]` — is a recorded coverage gap, not an
            // FP). Nothing is minted and nothing is cleared: an element cannot
            // rebind a local except through the expression-position write arms
            // above, which thread it immediately.
            Node::ArrayLit { elements, .. } | Node::HashLit { elements, .. } => {
                let elements = elements.clone();
                for e in elements {
                    self.class_flow_expr(ast, e, tenv, cenv, coarse, writes, interner, out, false);
                }
            }
            Node::InterpolatedString { parts, .. } | Node::InterpolatedSymbol { parts, .. } => {
                let parts = parts.clone();
                for p in parts {
                    self.class_flow_expr(ast, p, tenv, cenv, coarse, writes, interner, out, false);
                }
            }
            // A `Statements` carrier (the `collect_recoverable_children`
            // recovery for `x = *use`, `x = (use rescue nil)`, …) or a
            // `begin`/`rescue` in expression position (`x = begin USE rescue
            // end`) is a STATEMENT LIST: hand it to the statement walker, which
            // carries the same position the assignment RHS has (probes
            // d25/g6/d20). The `BeginRescue` arm's bound-name kill and its
            // post-clear apply here too.
            Node::Statements { .. } | Node::BeginRescue { .. } => {
                self.class_flow_stmt(ast, id, tenv, cenv, coarse, writes, interner, out, stmt_position);
            }
            // Stage 3a-3: a bare read of a chain ROOT anywhere OTHER than
            // beneath a live address read invalidates every chain rooted at it.
            // The reference does NOT — `invalidate_chain_after_call` only
            // matches a call whose RECEIVER is the root, so it keeps the fact
            // through `g(h)` and `other.push(h)` (probes `c7c_arg_mention`,
            // `f23_push`, `n_root_as_arg_to_mutator`, all reference=1). The
            // spec's decline set names this: we kill on ANY root mention,
            // paying coverage for an invalidation rule that is a strict
            // superset of the reference's and therefore cannot be an FP source.
            Node::LocalVariableRead { name, .. } => {
                let name = name.clone();
                cenv.kill_chains_rooted_at(&name);
            }
            _ => {}
        }
    }

    /// Narrow through one `if`/`unless`/ternary node. `stmt_position` is `true`
    /// only when the node is a direct STATEMENT (reached via
    /// [`Typer::class_flow_stmt`]): the early-return propagation is the
    /// statement evaluator's behavior (`eval_if:481`) and is unprobed for an
    /// expression-position conditional (`f(x.is_a?(C) ? x : raise)` —
    /// `propagate_if_branches` types the expression's branches, nothing shows
    /// the falsey edge propagating past it), so an expression-position `if`
    /// narrows its branches but never the statements after it (review R2).
    /// See [`Typer::class_narrowing_snapshots`] for the envelope.
    #[allow(clippy::too_many_arguments)]
    fn class_flow_if(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        cenv: &mut Facts,
        coarse: &HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut ClassNarrowing,
        stmt_position: bool,
    ) {
        let Node::If { predicate, then_body, else_body, is_unless, span } = ast.get(id) else {
            return;
        };
        let (predicate, is_unless, if_span) = (*predicate, *is_unless, *span);
        let then_body = then_body.clone();
        // Prism models an `else` clause as its own node, and the arena lowers an
        // `ElseNode` to a CLAUSE-LESS `BeginRescue` carrier (`ast.rs:1457`), so
        // `else_body` is the single-element `vec![carrier]`. Walking the carrier
        // as an ordinary statement would run the `BeginRescue` arm, whose
        // `join_cenv(cenv, &[])` blanket-wipes the edge's `Narrowed` facts — so
        // EVERY `if` with an `else` lost the incoming fact on its falsey edge,
        // whatever the else contained (probes `D_else_nil`/`E_both_nil`, where
        // the reference fires and master is silent). The carrier is not a real
        // `begin`: `subsequent` is only ever an `elsif` (an `If`, untouched
        // here) or an `else`, so unwrapping it is exact.
        let else_body = match else_body.as_slice() {
            [only] => match ast.get(*only) {
                Node::BeginRescue { body, ensure_body, clauses, .. }
                    if ensure_body.is_empty() && clauses.is_empty() =>
                {
                    body.clone()
                }
                _ => else_body.clone(),
            },
            _ => else_body.clone(),
        };
        // The predicate is evaluated first, in the current facts (its own calls
        // may read an OUTER narrowing) — EXPRESSION position.
        self.class_flow_expr(ast, predicate, tenv, cenv, coarse, writes, interner, out, false);
        // Stage 3a-1: the predicate yields a guard map for EACH edge (`&&`/`||`
        // /`!` recursion — [`Typer::analyse_predicate`]). A plain class guard
        // still yields facts on the truthy edge only, so the pre-3a-1 behavior
        // is the `(vec![g], vec![])` case of this.
        let (truthy_g, falsey_g) = self.analyse_predicate(ast, predicate, 64).unwrap_or_default();
        // Truthy edge: then-branch for `if`/ternary, else-branch for `unless`.
        let (truthy, falsey) =
            if is_unless { (&else_body, &then_body) } else { (&then_body, &else_body) };
        let truthy_edge = {
            let mut t = tenv.clone();
            let mut c = cenv.clone();
            self.apply_guards(&truthy_g, ast, tenv, &mut c, coarse, interner);
            // The branch bodies INHERIT the conditional's own position: a block
            // inside an expression-position ternary branch narrows nothing
            // (probe x2), while a statement `if`'s branches stay statement
            // position (probe x5). The branch-internal narrowing itself is
            // never position-gated (p4, p8).
            self.class_flow_scope(ast, truthy, &mut t, &mut c, coarse, writes, interner, out, stmt_position);
            c
        };
        let falsey_edge = {
            // 3a-1: the falsey edge is no longer categorically unnarrowed. It
            // carries facts exactly when the predicate SWAPPED one onto it —
            // `!guard` (probes c4d/c4f), an `||` whose disjuncts' falsey maps
            // concatenate (`x_or_falsey_bang`), or an `&&` whose falsey maps
            // join on the SAME class (`b2_and_bang_same`). A bare
            // `local.is_a?(C)` still contributes NOTHING here (`c1g`).
            let mut t = tenv.clone();
            let mut c = cenv.clone();
            self.apply_guards(&falsey_g, ast, tenv, &mut c, coarse, interner);
            self.class_flow_scope(ast, falsey, &mut t, &mut c, coarse, writes, interner, out, stmt_position);
            c
        };
        // Conservative join: widen every local written inside the conditional
        // and clear the branch-established facts (a `Narrowed` fact never
        // survives a branch merge in this slice; an entry `Bot` does, unless an
        // edge rebound the local) …
        widen_flow_writes(writes, if_span, tenv, interner);
        // Stage 3a-3: `join_cenv` wipes every chain fact, so snapshot them for
        // the propagation's re-seed below (see there for why).
        // S2: the LOCAL twin of the same snapshot. See the re-seed below.
        let pre_join = cenv.clone();
        let (pre_join_chains, pre_join_locals) = (&pre_join.chains, &pre_join.locals);
        let edges = [truthy_edge, falsey_edge];
        join_cenv(cenv, &edges);
        // Join-retention slice: put back every pre-join fact BOTH edges left
        // untouched. Runs BEFORE the propagation below, so a restored fact is
        // what the carried guard map MEETS against (the propagation re-reads the
        // same snapshot for its own targets — idempotent on top of this). Both
        // branch bodies are always descended here, so the chain edges carry real
        // evidence and the chain half is enabled — gated on the predicate's own
        // local mentions (see `locals_in_span`).
        let predicate_locals = locals_in_span(ast, ast.get(predicate).span());
        retain_joined_facts(cenv, &pre_join, &edges, writes, if_span, Some(&predicate_locals));
        // … EXCEPT the early-return propagation (`eval_if:486`/`:495`), which
        // 3a-1 runs in BOTH directions: a terminating FALSEY branch propagates
        // the truthy map (the `return unless guard` idiom a5/c1d), a
        // terminating TRUTHY branch the falsey map (`return if !guard` —
        // c4a/c4b/f22/t_c1d_or). Only in STATEMENT position (review R2), never
        // for a local rewritten inside the conditional's span, and never when
        // BOTH branches terminate: the statements after are then unreachable
        // and the reference emits nothing there (probe `t_both_terminate` — a
        // measured would-be FP).
        let truthy_terminates = !truthy.is_empty() && branch_terminates(ast, truthy);
        let falsey_terminates = !falsey.is_empty() && branch_terminates(ast, falsey);
        let rewritten = |local: &str| {
            writes.iter().any(|(ws, n)| n == local && if_span.0 <= ws.0 && ws.1 <= if_span.1)
        };
        if stmt_position && truthy_terminates != falsey_terminates {
            let carried = if falsey_terminates { &truthy_g } else { &falsey_g };
            // A CHAIN target is filtered on its ROOT: a rebind of the root
            // inside the conditional's span invalidates the address just as a
            // rebind of the local invalidates a local fact (probe `c7g`, where
            // the reference fires a DIFFERENT diagnostic off the rebound value
            // and we must stay silent).
            let carried: GuardMap =
                carried.iter().filter(|(t, _)| !rewritten(t.root())).cloned().collect();
            // Re-seed the PRE-JOIN chain facts for exactly the addresses this
            // map touches. Without it a sequential disjoint re-guard of one
            // address (`return unless h.last.is_a?(String)` then `return unless
            // h.last.is_a?(Hash)`) would mint `Hash` against an EMPTY env and
            // witness where the reference has already collapsed to `Bot` —
            // probe `d_seq_two_returns_disjoint`, reference-silent. This is the
            // chain analogue of the LOCAL-side defect recorded in the
            // `next`/`break` build note (`s1_two_returns_sequential`), which
            // sits on the same `join_cenv`-before-propagation ordering; the
            // narrow re-seed fixes it for chains without touching that ordering.
            // Only the touched addresses are restored, so an untouched chain
            // fact still dies at the join (probe `n_escape_after_if`).
            //
            // S2 (2026-08-08) closes the LOCAL side of exactly that defect, on
            // the same narrow re-seed. `return unless v.is_a?(File::Stat)` then
            // `return unless v.is_a?(URI::HTTP)` is reference-SILENT (its scope
            // carries `File::Stat` into the second guard, which collapses it to
            // `Bot`), while rigor-rs minted `URI::HTTP` against the wiped env and
            // witnessed — probe r7. The same shape on two CORE names
            // (`Hash` then `String`) was already firing before this slice: the
            // pre-existing FP the `next`/`break` build note recorded as
            // `s1_two_returns_sequential`. Re-seeding lets the sequential-guard
            // MEET in `apply_guards` see the prior fact: a disjoint re-guard
            // reaches `Bot` (silence on both spellings), a subclass re-guard
            // refines instead of dropping (`seq_subclass` fires `for Integer`
            // on the reference). Only the locals this map touches are restored,
            // so an untouched fact still dies at the join.
            for (t, _) in &carried {
                match t {
                    GuardTarget::Chain(root, m) => {
                        let addr: ChainAddr = (root.clone(), m.clone());
                        if let Some(fact) = pre_join_chains.get(&addr) {
                            cenv.chains.insert(addr, fact.clone());
                        }
                    }
                    GuardTarget::Local(name) => {
                        if let Some(fact) = pre_join_locals.get(name) {
                            cenv.locals.insert(name.clone(), fact.clone());
                        }
                    }
                }
            }
            self.apply_guards(&carried, ast, tenv, cenv, coarse, interner);
        }
    }

    /// Apply one edge's guard map to the fact env `c`, IN SOURCE ORDER.
    ///
    /// Sequencing is load-bearing: the reference's `analyse_and` evaluates the
    /// right conjunct's truthy scope UNDER the left's, so `v.is_a?(String) &&
    /// v.is_a?(Hash)` re-narrows an already-`Nominal[String]` carrier and
    /// reaches `Bot` — the reference is SILENT in that branch (probe
    /// `a_same_local_disjoint_then`). Applying the entries one at a time
    /// against the WORKING env reproduces that through the R3 conflict rule
    /// below, where the spec's "b wins a same-local collision" would have been
    /// a live false positive.
    ///
    /// Per local, in order:
    /// 1. **`Bot`** (PR #73) when EVERY class in the fact's union collapses the
    ///    local's precise carrier ([`Typer::guard_collapses`]). Tested first and
    ///    against `c`, so an earlier conjunct's `Bot` sticks.
    /// 2. **`Narrowed`** when the fact is mintable (not `===`), carries exactly
    ///    ONE class (an `||` union is stage 3a-4 — the reference narrows to
    ///    `Hash | String` and we decline), the local passes the carrier
    ///    ALLOW-list (`coarse`, PR #72) and its current type is `Dynamic`/`Top`
    ///    (`narrow_class_other`). Both gates are PER-LOCAL: a compound predicate
    ///    may narrow one local and decline another (probe `a_two_locals_then`).
    /// 3. **Sequential-guard meet** (replacing the old review-R3 blanket drop
    ///    for LOCALS): a guard over an existing `Narrowed` fact narrows the
    ///    FACT's carrier the way the reference's `narrow_nominal_to_class`
    ///    narrows `Nominal[C]` — same class keeps, a subclass guard refines, a
    ///    superclass guard is a no-op, proven-disjoint reaches `Bot`, `exact`
    ///    collapses on name mismatch, an unresolvable ordering KEEPS the
    ///    carrier (`:unknown stays conservative`, `narrowing.rb:2388`), and an
    ///    `||` union meets per member. See the arm's comment for probe rows.
    ///
    /// ## Stage 3a-3 — [`GuardTarget::Chain`] entries
    ///
    /// A chain target follows the SAME steps — Bot short-circuit, R3 conflict,
    /// sequential MEET, mint — with two differences, each measured:
    /// - the `Bot` STEP has no `guard_collapses` half. A precise chain carrier
    ///   IS collapsed by the reference (`h = [1, 2]; h.last.is_a?(String)` is
    ///   reference-silent, probe `k_root_array_lit`), but we reproduce that by
    ///   DECLINING the mint — the carrier gate below reads the chain call's own
    ///   type, which is precise in exactly that case. A chain `Bot` therefore
    ///   only ever arises from the MEET, and once it does it sticks: it survives
    ///   every later guard and records the use into `ClassNarrowing::dead`
    ///   rather than `calls`. Before the 2026-08-09 slice `chains` could not
    ///   express it at all (the value type was a bare class name), and the
    ///   "absent" stand-in let a third guard re-mint — probe `chain_third`, a
    ///   live false positive;
    /// - the carrier gate reads `type_of(chain_call)` instead of the local's
    ///   `tenv` entry, and the PR #72 `coarse` ALLOW-list does NOT apply. The
    ///   allow-list exists because the reference's carrier for a `||`-bound
    ///   LOCAL is a union its `narrow_class_other` declines; the carrier here is
    ///   the DISPATCH RESULT off that union, which the reference narrows
    ///   normally — `h = a || b; h.last.is_a?(String)` fires on the reference
    ///   (probe `k_root_or_union`), so applying the allow-list would be pure
    ///   coverage loss with no FP to pay for it.
    ///
    /// A sequential re-guard of the SAME address runs the identical
    /// `narrow_nominal_to_class` meet the LOCAL arm runs — measured row for row
    /// against the oracle over the 2026-08-09 `chain_*` matrix, which found the
    /// chain family to behave EXACTLY like the local one
    /// (`class_narrowing_chain_guard_meet_matrix`). A disjoint pair reaches
    /// `Bot` and stays silent (`chain_disjoint`, and the `||` union
    /// `chain_or_disjoint`, which the pre-slice `classes.len() == 1` mint gate
    /// skipped outright — a live FP); a subclass guard REFINES
    /// (`chain_subclass` fires `for Integer`, where the old blind
    /// keep-if-equal-else-remove dropped it); a superclass guard is a no-op
    /// (`chain_superclass`); the `Unknown` split keeps a project-class carrier
    /// (`chain_projclass`/`chain_projsub`/`chain_projsub_or`) and drops an
    /// RBS-space pair (`chain_r7`).
    ///
    /// What stays declined is RECOGNITION, not the meet: `guard_predicate`
    /// requires a bare local operand, so `===` and `nil?` on a chain receiver
    /// never produce a chain target at all (`chain_caseeq_same`,
    /// `chain_caseeq_subclass`, `chain_nilq` — the reference fires, we stay
    /// silent, pure coverage).
    #[allow(clippy::too_many_arguments)]
    fn apply_guards(
        &self,
        guards: &GuardMap,
        ast: &LoweredAst,
        tenv: &TypeEnv,
        c: &mut Facts,
        coarse: &HashSet<String>,
        interner: &mut Interner,
    ) {
        // The classes each TARGET was already asserted to EARLIER IN THIS MAP.
        // Needed because a NON-mintable assertion (`===`, `nil?`) writes nothing
        // to `c`, yet the reference's carrier at the next conjunct is that class
        // — `String === v && v.is_a?(Hash)` and `v.nil? && v.is_a?(String)` are
        // both measured reference-silent and would otherwise witness.
        let mut asserted: Vec<(&GuardTarget, &[String])> = Vec::new();
        for (target, g) in guards {
            let prior = asserted
                .iter()
                .rev()
                .find(|(t, _)| *t == target)
                .map(|(_, cl)| *cl);
            asserted.push((target, &g.classes));
            let conflicts = prior.is_some_and(|p| p != g.classes.as_slice());
            match target {
                GuardTarget::Local(local) => {
                    let collapses = !g.classes.is_empty()
                        && g.classes.iter().all(|class| {
                            self.guard_collapses(local, class, g.exact, tenv, c, interner)
                        });
                    if collapses {
                        c.locals.insert(local.clone(), ClassFact::Bot);
                        continue;
                    }
                    if conflicts {
                        c.locals.remove(local);
                        continue;
                    }
                    // Sequential-guard meet: the env already carries a
                    // `Narrowed` fact for this local (an earlier statement's
                    // early-return propagation minted it, re-seeded past the
                    // join by S2), so the reference's carrier here is
                    // `Nominal[carrier]` and this guard narrows THAT, not
                    // `Dynamic` — `narrow_nominal_to_class`
                    // (`narrowing.rb:2381`), probed over the seq_* matrix in
                    // the 2026-08-08 sequential-guards note. The rule table
                    // lives on the helper, which the CHAIN arm shares
                    // unchanged. `Bot` here survives the join to SUPPRESS,
                    // where the S2 drop merely went silent.
                    if let Some(ClassFact::Narrowed(carrier)) = c.locals.get(local).cloned() {
                        let met = self.narrow_nominal_to_class(&carrier, g);
                        match met {
                            Some(f) => {
                                c.locals.insert(local.clone(), f);
                            }
                            None => {
                                c.locals.remove(local);
                            }
                        }
                        continue;
                    }
                    let mintable = g.mintable
                        && g.classes.len() == 1
                        && !coarse.contains(local)
                        && match tenv.get(local) {
                            None => true, // unbound ⇒ untyped (Dynamic[top])
                            Some(&ty) => {
                                matches!(interner.get(ty), Type::Dynamic(_) | Type::Top)
                            }
                        };
                    if !mintable {
                        continue;
                    }
                    // No existing fact for the local by here: `Bot` was
                    // consumed by the collapse test, `Narrowed` by the meet.
                    c.locals.insert(local.clone(), ClassFact::Narrowed(g.classes[0].clone()));
                }
                GuardTarget::Chain(root, method) => {
                    let addr: ChainAddr = (root.clone(), method.clone());
                    // A collapsed address stays collapsed: the reference's
                    // scope carries `Bot` into every later guard and
                    // `narrow_nominal_to_class` cannot revive it (probe
                    // `chain_third`, String→Hash→String, reference-SILENT —
                    // a live FP on master, where "absent" let the third guard
                    // re-mint). The LOCAL twin of this short-circuit lives in
                    // `guard_collapses`, which returns `true` on an incoming
                    // `Bot` fact.
                    if c.chains.get(&addr) == Some(&ClassFact::Bot) {
                        continue;
                    }
                    if conflicts {
                        c.chains.remove(&addr);
                        continue;
                    }
                    // Sequential-guard meet, the chain twin of the LOCAL arm
                    // above and the same `narrow_nominal_to_class`
                    // (`narrowing.rb:2381`): an existing fact means the
                    // reference's carrier here is `Nominal[carrier]`, not
                    // `Dynamic`. Oracle-probed over the 2026-08-09 chain_*
                    // matrix — a subclass guard REFINES (`chain_subclass` fires
                    // `for Integer`), a superclass guard is a no-op
                    // (`chain_superclass` fires `for Integer`), a disjoint pair
                    // or an all-disjoint `||` union reaches `Bot`
                    // (`chain_disjoint`, `chain_or_disjoint` — the latter a
                    // live FP on master, where the `classes.len() == 1` mint
                    // gate skipped the union guard entirely and the stale
                    // `String` fact survived to witness), and the `Unknown`
                    // split keeps a project-class carrier
                    // (`chain_projclass`/`chain_projsub`/`chain_projsub_or` all
                    // fire `for String`) while dropping an RBS-space pair our
                    // resolver cannot order (`chain_r7`).
                    if let Some(ClassFact::Narrowed(carrier)) = c.chains.get(&addr).cloned() {
                        match self.narrow_nominal_to_class(&carrier, g) {
                            Some(f) => {
                                c.chains.insert(addr, f);
                            }
                            None => {
                                c.chains.remove(&addr);
                            }
                        }
                        continue;
                    }
                    // No existing fact by here: `Bot` was consumed by the
                    // short-circuit, `Narrowed` by the meet. The MINT path is
                    // unchanged — `narrow_class_other`'s envelope, read off the
                    // CHAIN CALL.
                    let carrier_ok = g.chain_call.is_some_and(|n| {
                        let ty = self.type_of(ast, n, tenv, interner);
                        matches!(interner.get(ty), Type::Dynamic(_) | Type::Top)
                    });
                    let mintable = g.mintable && g.classes.len() == 1 && carrier_ok;
                    if !mintable {
                        continue;
                    }
                    c.chains.insert(addr, ClassFact::Narrowed(g.classes[0].clone()));
                }
            }
        }
    }

    /// The MEET of an already-`Nominal[carrier]` fact against one guard — the
    /// port of the reference's `narrow_nominal_to_class` (`narrowing.rb:2381`),
    /// shared by the LOCAL and CHAIN arms of [`Typer::apply_guards`].
    ///
    /// `Some(fact)` is the met fact, `None` a DROP (the caller removes the
    /// entry — neither witnessed nor collapsed). Same class keeps the fact
    /// (`seq_same`/`chain_same`, and `===` too — `seq_caseeq_same` fires
    /// `for String`); `instance_of?` collapses on a bare name mismatch BEFORE
    /// the hierarchy (`seq_exact_subclass`/`chain_exact_subclass` are
    /// reference-silent); a proven-disjoint pair reaches `Bot`; a SUBCLASS
    /// guard refines to the more specific class, mintable or not
    /// (`seq_subclass`/`chain_subclass` fire `for Integer`); a SUPERCLASS guard
    /// is a no-op (`seq_superclass`/`chain_superclass` fire `for Integer`, the
    /// carrier); `Unknown` splits on WHY (see the arm); a multi-class fact (an
    /// `||` union) meets per member.
    ///
    /// Extracted verbatim from the LOCAL arm by the 2026-08-09 chain-guard-meet
    /// slice — the chain family measures IDENTICALLY on the oracle across all
    /// 26 `chain_*` rows, so one implementation is the honest encoding.
    fn narrow_nominal_to_class(&self, carrier: &str, g: &GuardFact) -> Option<ClassFact> {
        match g.classes.as_slice() {
            [class] if class.as_str() == carrier => Some(ClassFact::Narrowed(carrier.to_string())),
            [_] if g.exact => Some(ClassFact::Bot),
            [class] => match self.index.class_ordering(carrier, class) {
                ClassOrdering::Equal | ClassOrdering::Subclass => {
                    Some(ClassFact::Narrowed(carrier.to_string()))
                }
                ClassOrdering::Superclass => Some(ClassFact::Narrowed(class.clone())),
                ClassOrdering::Disjoint => Some(ClassFact::Bot),
                // `Unknown` splits on WHY the ordering failed. A
                // PROJECT-declared class is unknown to the reference's RBS env
                // too — `:unknown stays conservative` (`narrowing.rb:2388`)
                // KEEPS the carrier there, even when the project hierarchy
                // would prove disjointness (probes `projsub`/`chain_projsub`:
                // `ProjKlass < Hash` after a String guard still fires
                // `for String`). But an ordering that fails on two RBS-SPACE
                // names is OUR resolver being weaker: the reference proves
                // `File::Stat` vs `URI::HTTP` disjoint and is silent (probes
                // r7/`chain_r7`), so keeping would be a live FP — drop.
                ClassOrdering::Unknown => {
                    if self.source.knows_class(class) || self.source.knows_class(carrier) {
                        Some(ClassFact::Narrowed(carrier.to_string()))
                    } else {
                        None
                    }
                }
            },
            classes => {
                // An `||` union meets PER MEMBER and unions the results
                // (`accumulate` over `analyse_or`): a disjoint member
                // contributes `Bot` (nothing), a superclass member the
                // carrier, a subclass member itself. Representable when one
                // class survives: `{Hash, String}` over a String carrier is
                // `Bot ∪ String` and the reference fires `for String` (probes
                // `seq_or_mixed`/`chain_or_mixed`); all-disjoint is `Bot`
                // (`seq_or_disjoint`/`chain_or_disjoint`). Two surviving
                // classes are a real union — drop.
                let mut survivors: Vec<&str> = Vec::new();
                let mut unresolvable = false;
                for class in classes {
                    let met: Option<&str> = if g.exact {
                        (class.as_str() == carrier).then_some(carrier)
                    } else {
                        match self.index.class_ordering(carrier, class) {
                            ClassOrdering::Disjoint => None,
                            ClassOrdering::Superclass => Some(class.as_str()),
                            ClassOrdering::Equal | ClassOrdering::Subclass => Some(carrier),
                            // Same `Unknown` split as the single-class arm: a
                            // project-class member keeps the carrier
                            // (`projsub_or`/`chain_projsub_or`); an RBS-space
                            // member our resolver cannot order poisons the
                            // whole union — drop.
                            ClassOrdering::Unknown => {
                                if self.source.knows_class(class)
                                    || self.source.knows_class(carrier)
                                {
                                    Some(carrier)
                                } else {
                                    unresolvable = true;
                                    None
                                }
                            }
                        }
                    };
                    if let Some(m) = met {
                        if !survivors.contains(&m) {
                            survivors.push(m);
                        }
                    }
                }
                match (unresolvable, survivors.as_slice()) {
                    (true, _) => None,
                    (false, []) => Some(ClassFact::Bot),
                    (false, [one]) => Some(ClassFact::Narrowed((*one).to_string())),
                    (false, _) => None,
                }
            }
        }
    }

    /// Analyse a whole predicate into its `(truthy, falsey)` guard maps — the
    /// port of the reference's `predicate_scopes` dispatch (`narrowing.rb:344`)
    /// for the three compound forms, stage 3a-1.
    ///
    /// - a class guard on a local → `([g], [])` (`analyse_class_predicate`,
    ///   `:1761`);
    /// - `!x` — a no-arg, no-block, non-safe-nav `Call` named `"!"`, which is
    ///   how prism lowers both `!` and `not` → the operand's pair SWAPPED
    ///   (`dispatch_unary_predicate`, `:1555`: `analyse(receiver)&.reverse`);
    /// - `&&` → truthy CONCATENATES (left then right, applied sequentially),
    ///   falsey JOINS (`analyse_and`, `:2631`);
    /// - `||` → truthy JOINS, falsey concatenates (`analyse_or`, `:2640`);
    /// - anything else → `None`, a whole-predicate decline.
    ///
    /// An UNRECOGNISED operand of `&&`/`||` contributes empty maps rather than
    /// killing the whole analysis — which is exactly the reference's behaviour:
    /// its truthy fallback is the other conjunct's scope (so any one recognised
    /// conjunct narrows, c1a–c1c) while its falsey JOIN against the unchanged
    /// scope yields nothing (c1g, f12, `u_and_or_falsey`). `depth` bounds the
    /// recursion; a predicate deeper than that declines.
    fn analyse_predicate(
        &self,
        ast: &LoweredAst,
        pred: NodeId,
        depth: u32,
    ) -> Option<(GuardMap, GuardMap)> {
        if depth == 0 {
            return None;
        }
        match ast.get(pred) {
            Node::Logical { left, right, is_and, .. } => {
                let (left, right, is_and) = (*left, *right, *is_and);
                // A named-capture `=~` (`/(?<v>a)/ =~ s`) BINDS `v` to `String`
                // in the reference and to nothing at all in the arena — prism's
                // `MatchWriteNode` has no lowering here, so the local reads as
                // an unbound (untyped) name and our gate would narrow it. That
                // is a measured FP (probe `matchwrite`), and the binding is
                // invisible, so the whole compound predicate declines. Only a
                // regex on the LEFT of `=~` binds; `v =~ /a/` (a local or ivar
                // receiver) does not and still narrows (`matchop_keep`).
                if regex_binding_match(ast, left, depth) || regex_binding_match(ast, right, depth)
                {
                    return None;
                }
                let a = self.analyse_predicate(ast, left, depth - 1);
                let b = self.analyse_predicate(ast, right, depth - 1);
                if a.is_none() && b.is_none() {
                    return None;
                }
                let (at, af) = a.unwrap_or_default();
                let (bt, bf) = b.unwrap_or_default();
                Some(if is_and {
                    ([at, bt].concat(), join_guards(&af, &bf))
                } else {
                    (join_guards(&at, &bt), [af, bf].concat())
                })
            }
            Node::Call { receiver: Some(r), method, args, block_body, safe_nav, .. }
                if method == "!" && args.is_empty() && block_body.is_empty() && !*safe_nav =>
            {
                let r = *r;
                self.analyse_predicate(ast, r, depth - 1).map(|(t, f)| (f, t))
            }
            _ => self.guard_predicate(ast, pred),
        }
    }

    /// Recognise ONE atomic predicate on a bare local, as the reference's
    /// `dispatch_call_simple` (`narrowing.rb:976`) does, and return its
    /// `(truthy, falsey)` guard maps.
    ///
    /// Four shapes, three of them NON-mintable — they exist so that a compound
    /// predicate knows the local was already pinned to a class, which is where
    /// the false positives live:
    /// - `local.is_a?(C)` / `kind_of?(C)` / `instance_of?(C)` — the mintable
    ///   guard (`analyse_class_predicate`, `:1761`). Bare `LocalVariableRead`
    ///   receiver, no safe-nav, no block, one `ConstantRead` argument with a
    ///   statically known, unshadowed name. Chains are stage 3a-3.
    /// - `C === local` — the reference narrows through it too (probe
    ///   `e3_case_eq_bang` fires `for String`), but minting would be new
    ///   coverage this slice does not claim. Recognising it non-mintably is
    ///   load-bearing anyway: `String === v && v.is_a?(Hash)` reaches `Bot` on
    ///   the reference and would otherwise witness `for Hash` (probe
    ///   `L_caseeq`, a measured FP), and `Hash === v` on an Array carrier feeds
    ///   the collapse (`e3_case_eq_bot`).
    /// - `local.nil?` — `analyse_nil_predicate` (`:2453`) pins `NilClass` on the
    ///   truthy edge (`narrow_nil_other`: a `Dynamic` carrier becomes
    ///   `Constant[nil]`, every precise one becomes `Bot`) and leaves the falsey
    ///   edge unchanged. Without it `v.nil? && v.is_a?(String)` witnesses
    ///   `for String` where the reference reaches `Bot` — measured FP `L_nilq`,
    ///   in BOTH conjunct orders and in the middle of a chain (`mid_nilq`).
    ///   `!v.nil? && v.is_a?(String)` keeps narrowing, because the swap puts the
    ///   `NilClass` fact on the edge the `&&` does not concatenate.
    /// - `local == nil` / `nil == local` (and `!=`, which swaps the pair) —
    ///   `analyse_equality_predicate` (`:1568`) meets the carrier with
    ///   `Constant[nil]`; on `Dynamic` that is a no-op (probe `L_eq_nil` fires,
    ///   so declining it costs coverage) but on an already-narrowed carrier it
    ///   is `Bot` (`R_eq_nil`, a measured FP). Equality against a NON-nil
    ///   literal attaches a relational fact and never pins a class (`L_eq_one`
    ///   / `R_eq_one` both fire) — not recognised, and measured safe.
    fn guard_predicate(&self, ast: &LoweredAst, pred: NodeId) -> Option<(GuardMap, GuardMap)> {
        let Node::Call { receiver: Some(r), method, args, block_body, safe_nav, span, .. } =
            ast.get(pred)
        else {
            return None;
        };
        if *safe_nav || !block_body.is_empty() {
            return None;
        }
        let nil_fact = |name: &str| {
            (GuardTarget::Local(name.to_string()), GuardFact {
                classes: vec!["NilClass".to_string()],
                exact: false,
                mintable: false,
                chain_call: None,
            })
        };
        match (method.as_str(), args.len()) {
            ("is_a?" | "kind_of?" | "instance_of?", 1) => {
                // S3: a guard on a name the PROJECT declares is not mintable
                // (this slice never narrows to a project nominal), but it must
                // still be SEEN — the reference resolves it and collapses a
                // shaped carrier against it (`v = [1, 2]` guarded by an
                // in-source `Proj::Thing` is reference-SILENT, probe r1g).
                // Declining the whole predicate, as this arm did before, left
                // that as a live false positive. `mintable: false` is the
                // existing carrier for "assert but do not narrow" (`===`,
                // `nil?`).
                let (class, mintable) = self.guard_constant(ast, args[0], *span)?;
                let exact = method == "instance_of?";
                // Stage 3a-3: the operand is EITHER a bare local (stages 1-2)
                // OR a stable single-hop chain address off a local root
                // (`analyse_class_predicate_on_chain`, `narrowing.rb:1805`).
                // Anything else — an ivar read, a two-hop chain, a hop with
                // arguments or a block, a safe-nav hop — declines, exactly as
                // `stable_chain_address` returns nil for it (probes `c7b_ivar`,
                // `m_two_hop`, `c7e_args_on_hop`, `m_block_on_hop`,
                // `m_safe_nav_hop`; the last three are reference-silent too,
                // the first two are recorded coverage gaps).
                let (target, chain_call) = match ast.get(*r) {
                    Node::LocalVariableRead { name, .. } => {
                        (GuardTarget::Local(name.clone()), None)
                    }
                    _ => {
                        let (root, m) = stable_chain_address(ast, *r)?;
                        (GuardTarget::Chain(root, m), Some(*r))
                    }
                };
                let g = (target, GuardFact {
                    classes: vec![class],
                    exact,
                    mintable,
                    chain_call,
                });
                Some((vec![g], Vec::new()))
            }
            ("===", 1) => {
                let Node::LocalVariableRead { name, .. } = ast.get(args[0]) else { return None };
                let class = self.resolved_static_constant(ast, *r, *span)?;
                let g = (GuardTarget::Local(name.clone()), GuardFact {
                    classes: vec![class],
                    exact: false,
                    mintable: false,
                    chain_call: None,
                });
                Some((vec![g], Vec::new()))
            }
            ("nil?", 0) => {
                let Node::LocalVariableRead { name, .. } = ast.get(*r) else { return None };
                Some((vec![nil_fact(name)], Vec::new()))
            }
            ("==" | "!=", 1) => {
                let name = match (ast.get(*r), ast.get(args[0])) {
                    (Node::LocalVariableRead { name, .. }, Node::NilLit { .. }) => name,
                    (Node::NilLit { .. }, Node::LocalVariableRead { name, .. }) => name,
                    _ => return None,
                };
                let g = nil_fact(name);
                Some(if method == "==" {
                    (vec![g], Vec::new())
                } else {
                    (Vec::new(), vec![g])
                })
            }
            _ => None,
        }
    }

    /// Whether guarding `local` with `class_name` yields `Bot` in the reference.
    ///
    /// Three ways, and no fourth (see [`Typer::class_narrowing_pass`] for why
    /// the `ClassOrdering::Unknown` arm is declined FOR A NOMINAL CARRIER — S3
    /// measured that a Constant/Tuple/HashShape carrier collapses on `Unknown`
    /// too, because its helper asks `subclass_of?` rather than `disjoint?`):
    /// 1. the local is ALREADY `Bot` — `narrow_class_other` / `narrow_other_class`
    ///    return `Bot` unchanged on both polarities, so a further guard cannot
    ///    revive it (probes `bot_then_match`, `bot_then_neg`);
    /// 2. `instance_of?` (`exact:`) with a name the carrier's class does not
    ///    equal — `narrow_nominal_to_class` returns `Bot` before consulting the
    ///    hierarchy at all, and `subclass_of?` degenerates to name equality for
    ///    the shape/constant carriers (`narrowing.rb:2384,2440`);
    /// 3. a PROVEN-disjoint pair. [`CoreIndex::class_ordering`] answers
    ///    `Disjoint` only when both names resolve AND both ancestor chains are
    ///    complete, so an unresolvable/project class or a truncated chain
    ///    answers `Unknown` and declines.
    ///
    /// The carrier's class comes from [`CoreIndex::class_name_of`] — the same
    /// mapping `check_call` dispatches on, so the suppression is exactly
    /// co-extensive with the witness it removes. A carrier that mapping
    /// declines (`Dynamic`, `Top`, a union, a `Singleton`) suppresses nothing.
    fn guard_collapses(
        &self,
        local: &str,
        class_name: &str,
        exact: bool,
        tenv: &TypeEnv,
        cenv: &Facts,
        interner: &Interner,
    ) -> bool {
        if cenv.locals.get(local) == Some(&ClassFact::Bot) {
            return true;
        }
        let Some(&ty) = tenv.get(local) else { return false };
        // The carrier must be one the reference's `narrow_class_dispatch`
        // (`narrowing.rb:2311`) routes to a COLLAPSING helper. Its table is
        // Constant / Nominal / Union / Tuple / HashShape / Singleton, and
        // everything else — an `IntegerRange` included — falls through to
        // `narrow_other_class`, which returns the type UNCHANGED for anything
        // that is not Dynamic/Top. `Union` (per-member) and `Singleton` (via
        // `subclass_of?("Class", …)`) are declined here as unprobed.
        if !matches!(
            interner.get(ty),
            Type::Constant(_) | Type::Nominal { .. } | Type::Tuple(_) | Type::HashShape(_)
        ) {
            return false;
        }
        let Some(carrier) = self.index.class_name_of(interner, ty) else { return false };
        if exact {
            return carrier != class_name;
        }
        // S3 (2026-08-08): the collapse condition is per-CARRIER-KIND, and it is
        // NOT the same predicate for all of them — reading `narrow_class_dispatch`
        // (`narrowing.rb:2311`) and probing the oracle both say so.
        //
        // * `narrow_shape_to_class` (`:2403`) and `narrow_constant_to_class`
        //   (`:2364`) keep the carrier iff `subclass_of?(carrier, class_name)`,
        //   i.e. iff the ordering is `Subclass`/`Equal`. `Unknown` therefore
        //   COLLAPSES: a shaped carrier does not need the guard class to resolve.
        // * `narrow_nominal_to_class` (`:2381`) is different — it PRESERVES the
        //   bound on `Subclass` and stays conservative on `Unknown`, collapsing
        //   only on `Disjoint`.
        //
        // Treating every carrier as the Nominal case (the pre-S3 code) left a
        // live false positive: `v = [1, 2]; return unless v.is_a?(File::Stat);
        // v.frobnicate_zzz` fired `for Array` where the reference is silent, and
        // it fired for an UNRESOLVABLE guard too. Measured against the pinned
        // reference on `[1, 2]` / `{a: 1}` carriers: `Enumerable`, `Object` and
        // `Array` guards all FIRE (subclass/equal ⇒ the shape survives) while
        // `File::Stat` and `Foo::Bar::Baz` are SILENT — which pins the condition
        // as `subclass_of?`, not as "any shaped carrier collapses".
        match interner.get(ty) {
            Type::Constant(_) | Type::Tuple(_) | Type::HashShape(_) => !matches!(
                self.index.class_ordering(carrier, class_name),
                ClassOrdering::Subclass | ClassOrdering::Equal
            ),
            _ => self.index.class_ordering(carrier, class_name) == ClassOrdering::Disjoint,
        }
    }

    /// The statically-known name of a `ConstantRead`/`ConstantPath` node (both
    /// lower to `ConstantRead`), resolved lexically at `use_span`'s prefix —
    /// or `None` when the node is not a static constant OR a project
    /// declaration shadows the name ([`SourceIndex::constant_shadowed`] —
    /// decline entirely; this slice never narrows to a project nominal).
    ///
    /// S2 (2026-08-08): the name is additionally resolved to a QUALIFIED KEY
    /// here, at MINT time, because this is where the use site's lexical
    /// `enclosing_prefix` is available — the class-narrowing fact then carries
    /// a resolved key rather than the verbatim source spelling. Three spellings
    /// of the same class must reach the same key (probes p3a–p3d): a relative
    /// `HTTP` inside `module URI`, a qualified `URI::HTTP`, and an absolute
    /// `::URI::HTTP` all render `for URI::HTTP` on the reference.
    fn resolved_static_constant(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        use_span: rigor_parse::Span,
    ) -> Option<String> {
        let Node::ConstantRead { name, .. } = ast.get(id) else {
            return None;
        };
        if name.is_empty() {
            return None;
        }
        let prefix = self.enclosing_prefix(use_span);
        if self.source.constant_shadowed(name, prefix) {
            return None;
        }
        Some(self.resolve_constant_as_written(name, prefix))
    }

    /// [`Typer::resolved_static_constant`] for the `is_a?`/`instance_of?` arm,
    /// which needs the SHADOWED case as a fact rather than as a decline:
    /// `(resolved qualified key, mintable)`. `mintable` is `false` exactly when
    /// the project declares the name in a lexically visible scope — the same
    /// predicate `resolved_static_constant` declines on — so the guard can
    /// collapse a precise carrier without ever narrowing TO a project nominal.
    fn guard_constant(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        use_span: rigor_parse::Span,
    ) -> Option<(String, bool)> {
        let Node::ConstantRead { name, .. } = ast.get(id) else {
            return None;
        };
        if name.is_empty() {
            return None;
        }
        let prefix = self.enclosing_prefix(use_span);
        let shadowed = self.source.constant_shadowed(name, prefix);
        Some((self.resolve_constant_as_written(name, prefix), !shadowed))
    }

    /// Resolve a guard constant's SOURCE SPELLING to the qualified key it names,
    /// by Ruby's (and RBS's) own rule: an ABSOLUTE reference (`::File::Stat`)
    /// drops its root marker and is looked up as written; a relative one is
    /// tried against each enclosing lexical scope innermost-outward, then at the
    /// root. The first scope that KNOWS the name wins — deterministic, so there
    /// is no residual ambiguity to decline on (the reference resolves the same
    /// way, probes q9/q9b).
    ///
    /// A name nothing knows is returned as written (minus the root marker): the
    /// witness gate declines it anyway (probes p2/p2b are reference-silent), and
    /// leaving it verbatim keeps `guard_collapses` seeing exactly what it saw
    /// before this slice.
    fn resolve_constant_as_written(&self, name: &str, prefix: &[String]) -> String {
        let (bare, absolute) = match name.strip_prefix("::") {
            Some(rest) => (rest, true),
            None => (name, false),
        };
        if !absolute {
            for depth in (1..=prefix.len()).rev() {
                let cand = format!("{}::{bare}", prefix[..depth].join("::"));
                if self.constant_names_a_known_class(&cand) {
                    return cand;
                }
            }
        }
        bare.to_string()
    }

    /// Whether `qname` names a class/module some AUTHORITATIVE surface knows —
    /// the bundled RBS qualified registry or the project's own `sig/`. In-source
    /// declarations are deliberately NOT consulted: the reference is silent on
    /// them for the ADR-0033 provenance reason (probes p4a/p4b/p5), so resolving
    /// to one could only ever change which name a declined witness carries.
    fn constant_names_a_known_class(&self, qname: &str) -> bool {
        self.index.knows_qualified_class(qname) || self.index.is_qualified_project_sig_class(qname)
    }

    /// Narrow through one `case`/`when` node (statement or expression
    /// position) — reference `case_when_scopes` (`narrowing.rb:374`), the
    /// strict-subset envelope:
    /// - the subject is a bare `LocalVariableRead` whose type is Dynamic/Top;
    /// - a clause narrows ONLY when it has EXACTLY ONE condition and that
    ///   condition is a static constant, resolved lexically, unshadowed
    ///   (multi-condition unions — probe a6 — decline, FP-safe);
    /// - clause bodies only; NO falsey threading between clauses (we never
    ///   narrow negative edges), no propagation past the `case`;
    /// - a `case`/`in` pattern branch (a `BeginRescue` carrier, not a `When`)
    ///   is unmodeled — no descent, matching the pre-slice behavior;
    /// - after the `case`: widen written locals, clear ALL facts (the same
    ///   conservative join every conditional gets);
    /// - **position gate** (`stmt_position`): a `case` reached as a call
    ///   receiver, as an argument, or as a `return` operand narrows NOTHING —
    ///   the reference is silent there (probes p2, p3, p7) while it fires in
    ///   statement position (p6) and on an assignment RHS (p1). This is the
    ///   same rule block bodies follow; `if`/ternary is the exception and
    ///   narrows in every position (p4, p8).
    #[allow(clippy::too_many_arguments)]
    fn class_flow_case(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        cenv: &mut Facts,
        coarse: &HashSet<String>,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut ClassNarrowing,
        stmt_position: bool,
    ) {
        let Node::Case { predicate, branches, else_body, span } = ast.get(id) else {
            return;
        };
        let (predicate, case_span) = (*predicate, *span);
        let (branches, else_body) = (branches.clone(), else_body.clone());
        // The subject evaluates first, in the current facts — EXPRESSION
        // position (it is the `case`'s operand, not a statement).
        if let Some(p) = predicate {
            self.class_flow_expr(ast, p, tenv, cenv, coarse, writes, interner, out, false);
        }
        // The narrowing subject: a bare local currently Dynamic/Top — and only
        // in statement position / on an assignment RHS (p2, p3, p7 decline).
        let subject = predicate
            .filter(|_| stmt_position)
            .and_then(|p| match ast.get(p) {
                Node::LocalVariableRead { name, .. } => Some(name.clone()),
                _ => None,
            })
            .filter(|local| !coarse.contains(local))
            .filter(|local| match tenv.get(local) {
                None => true, // unbound ⇒ untyped (Dynamic[top])
                Some(&ty) => matches!(interner.get(ty), Type::Dynamic(_) | Type::Top),
            });
        // The `Bot` subject: the same bare local under the same position gate
        // (probe `case_as_recv` — a `case` consumed as a call receiver narrows
        // NOTHING and the reference still fires there), but with the PRECISE
        // carrier the narrowing subject excludes. The per-clause collapse test
        // is applied below, per condition.
        let bot_subject = predicate.filter(|_| stmt_position).and_then(|p| match ast.get(p) {
            Node::LocalVariableRead { name, .. } => Some(name.clone()),
            _ => None,
        });
        // Join-retention slice: the pre-`case` facts, snapshot AFTER the subject
        // was evaluated (each clause clones `cenv` from exactly here).
        let pre_join = cenv.clone();
        // Were ALL branches descended? A `case`/`in` pattern clause is not, so
        // its effects are invisible to the edge evidence. Rebinds are still
        // caught by the span kill below, but an `invalidate_chain_after_call`
        // is not recorded in `writes` at all — so the chain half of the restore
        // is enabled only when every branch produced a real edge.
        let mut all_descended = true;
        let mut edges: Vec<Facts> = Vec::new();
        for br in branches {
            let Node::When { conditions, body, .. } = ast.get(br) else {
                // A `case`/`in` pattern carrier — unmodeled, no descent
                // (pre-slice behavior; the trailing clear-all covers effects).
                all_descended = false;
                continue;
            };
            let (conditions, body) = (conditions.clone(), body.clone());
            // Each clause runs on a clone of the PRE-`case` facts: clauses are
            // alternatives, and we never thread a falsey edge between them.
            let mut t = tenv.clone();
            let mut c = cenv.clone();
            // Conditions evaluate under the un-narrowed facts (they decide the
            // edge; a call condition still records its own uses).
            for &cond in &conditions {
                self.class_flow_expr(ast, cond, &mut t, &mut c, coarse, writes, interner, out, false);
            }
            if let (Some(local), [only]) = (&subject, conditions.as_slice()) {
                if let Some(class) = self.resolved_static_constant(ast, *only, case_span) {
                    // Review R3: a clause conflicting with an existing
                    // DIFFERENT-class fact for the subject drops the stale fact
                    // and never inserts (the reference's carrier is a Nominal
                    // there, out of the Dynamic-only envelope). Same class keeps.
                    let fact = ClassFact::Narrowed(class);
                    if c.locals.get(local).is_some_and(|existing| existing != &fact) {
                        c.locals.remove(local);
                    } else {
                        c.locals.insert(local.clone(), fact);
                    }
                }
            } else if let Some(local) = &bot_subject {
                // `case x when C1, C2` runs `C1 === x || C2 === x` and the
                // reference UNIONS the per-condition narrowings
                // (`accumulate_case_when_scopes`), so the clause body is `Bot`
                // only when EVERY condition collapses — `when Hash, Array` on an
                // Array keeps the carrier and still witnesses (probes
                // `case_multi_disj` vs `case_multi_mixed`). An empty clause
                // cannot occur; `all` over one condition is the single-constant
                // case, which lands here whenever `subject` declined it.
                let all_collapse = !conditions.is_empty()
                    && conditions.iter().all(|&cond| {
                        self.resolved_static_constant(ast, cond, case_span).is_some_and(|class| {
                            self.guard_collapses(local, &class, false, tenv, &c, interner)
                        })
                    });
                if all_collapse {
                    c.locals.insert(local.clone(), ClassFact::Bot);
                }
            }
            // Clause bodies INHERIT the `case`'s position: a block inside an
            // expression-position clause narrows nothing (probe x1).
            self.class_flow_scope(ast, &body, &mut t, &mut c, coarse, writes, interner, out, stmt_position);
            edges.push(c);
        }
        {
            // The `else` body is a NEGATIVE edge — never narrowed.
            let mut t = tenv.clone();
            let mut c = cenv.clone();
            self.class_flow_scope(
                ast, &else_body, &mut t, &mut c, coarse, writes, interner, out, stmt_position,
            );
            edges.push(c);
        }
        widen_flow_writes(writes, case_span, tenv, interner);
        // A `case`/`in` clause is not descended, so its rebinds are invisible to
        // the edge evidence — kill by span as well as by edge.
        join_cenv(cenv, &edges);
        // Join-retention slice: restore the pre-`case` facts the clauses left
        // untouched (`case_intervening`, reference-firing on master's silence).
        // The narrowing SUBJECT is excluded: the reference replaces its type per
        // clause and the post-`case` union is out of the Dynamic-only envelope
        // this pass models, so keeping our incoming fact there would be an
        // unprobed guess (probe `case_subject_is_target` — the reference fires,
        // we decline; coverage, never an FP).
        let subject_excl: Vec<String> = subject.iter().chain(bot_subject.iter()).cloned().collect();
        let joined_subject: Vec<(String, Option<ClassFact>)> =
            subject_excl.iter().map(|n| (n.clone(), cenv.locals.get(n).cloned())).collect();
        let predicate_locals = predicate.map(|p| locals_in_span(ast, ast.get(p).span()));
        retain_joined_facts(
            cenv,
            &pre_join,
            &edges,
            writes,
            case_span,
            predicate_locals.as_ref().filter(|_| all_descended),
        );
        for (name, fact) in joined_subject {
            match fact {
                Some(f) => cenv.locals.insert(name, f),
                None => cenv.locals.remove(&name),
            };
        }
        kill_cenv_writes(writes, case_span, cenv);
    }

    // -----------------------------------------------------------------------
    // Collection-shape receiver survival (spec
    // docs/notes/20260807-collection-shape-slice-spec.md, STAGE 1).
    //
    // Self-contained region: a parallel walker (`coll_flow_*`) rather than an
    // extension of `class_flow_*`, so this slice's env discipline (a threaded
    // `TypeEnv` joined per branch) stays independent of the narrowing pass's
    // fact env, and neither can regress the other.
    // -----------------------------------------------------------------------

    /// Compute the per-call-node collection-shape snapshot map: `call node id ->
    /// "Array" | "Hash"` for every call whose receiver is a bare local whose
    /// threaded binding is a collection carrier — a literal `Tuple`/`HashShape`
    /// seed, a `Nominal[Array|Hash]` minted by an in-place mutator that KEPT the
    /// nominal (`MutationWidening::widen_tuple`/`widen_hash_shape`,
    /// `reference/rigor/lib/rigor/inference/mutation_widening.rb:251,265`), or a
    /// `Nominal[Array|Hash]` an already-FP-gated tier fold produced (`missing =
    /// KEYS.filter { … }`). The rules layer's `check_collection_call` fires
    /// `call.undefined-method` from this map — and ONLY that rule (the class
    /// narrowing slice's pitfall 7: no wrong-arity / ATM wiring).
    ///
    /// ## FP-safety envelope (every decline load-bearing; §6 of the spec)
    ///
    /// - **Seeds only from literals or existing folds.** A mutator on a Dynamic
    ///   carrier never MINTS a binding (`widen_for_mutator` returns nil for a
    ///   non-shape carrier — probes m08/m13), which is also what keeps us out of
    ///   the reference's runtime-wrong `[]=`-on-a-String rows (bucket E, probe
    ///   c12).
    /// - **Per-shape mutator tables**, not their union: a Hash-only mutator on a
    ///   `Tuple` is a no-op, exactly as the reference's `case type` dispatch
    ///   (`mutation_widening.rb:209`).
    /// - **Branch join is identical-`TypeId` only** ([`join_flow_envs`]). This is
    ///   the load-bearing decline: a branch-contained mutation on a not-yet-
    ///   widened seed leaves `Tuple[] | Array[…]` after the reference's
    ///   `Scope#join` (`scope.rb:680`) and `receiver_descriptor` has NO
    ///   `Type::Union` arm (`rbs_dispatch.rb:200`), so the reference is SILENT
    ///   (probes m18/m20) — we widen to untyped and never model the union.
    ///   Straight-line widening BEFORE the construct makes both edges agree and
    ///   the site fires (m01/m19).
    /// - **Block bodies** mirror `widen_after_block` (`:144`): the block's
    ///   binding REPLACES the outer one, but only when it is a kept nominal —
    ///   any other outcome (a rebind, m15; a block-internal branch join) widens
    ///   to untyped. Descent only from statement position (the block-narrowing
    ///   position rule, docs/notes/20260807-block-narrowing-position-rule.md);
    ///   an expression-position block widens its contained writes instead.
    /// - **Unmodeled constructs decline**: `while`/`until` (the reference DOES
    ///   fire, probe m10 — deliberate coverage loss, its `break`/`next` join
    ///   edges are unprobed), `begin`/`rescue`, op-writes (m16), logicals,
    ///   safe-nav receivers, and every expression kind without an arm below
    ///   widen every write their span contains and bind nothing.
    /// - **Ivar carriers are never typed** (probe m09 fires in the reference —
    ///   bucket B's own future slice).
    pub fn collection_shape_snapshots(
        &self,
        ast: &LoweredAst,
        interner: &mut Interner,
    ) -> HashMap<NodeId, &'static str> {
        let mut out = HashMap::new();
        let body = match ast.get(ast.root()) {
            Node::Program { body, .. } => body.clone(),
            _ => return out,
        };
        let mut writes = collect_flow_writes(ast);
        writes.extend(indexed_flow_writes(ast, self.source));
        let rebinds = collect_rebind_writes(ast);
        let ctx = CollCtx { writes: &writes, rebinds: &rebinds };
        let mut tenv = TypeEnv::new();
        self.coll_flow_scope(ast, &body, &mut tenv, &ctx, interner, &mut out, true);
        out
    }

    /// Thread `tenv` through a scope's statements in source order.
    #[allow(clippy::too_many_arguments)]
    fn coll_flow_scope(
        &self,
        ast: &LoweredAst,
        stmts: &[NodeId],
        tenv: &mut TypeEnv,
        ctx: &CollCtx<'_>,
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
        stmt_position: bool,
    ) {
        for &s in stmts {
            self.coll_flow_stmt(ast, s, tenv, ctx, interner, out, stmt_position);
        }
    }

    /// Apply one statement's effect on `tenv` and record collection-typed uses.
    #[allow(clippy::too_many_arguments)]
    fn coll_flow_stmt(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        ctx: &CollCtx<'_>,
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
        stmt_position: bool,
    ) {
        match ast.get(id) {
            Node::Statements { body, .. } => {
                let body = body.clone();
                self.coll_flow_scope(ast, &body, tenv, ctx, interner, out, stmt_position);
            }
            Node::LocalVariableWrite { .. }
            | Node::MultiWrite { .. }
            | Node::LocalVariableOpWrite { .. }
            | Node::Call { .. } => {
                self.coll_flow_expr(ast, id, tenv, ctx, interner, out, stmt_position);
            }
            // `return E` evaluates its operands in the current bindings; the
            // operand is EXPRESSION position (no block/`case` effect leaks out).
            Node::Return { values, .. } => {
                let values = values.clone();
                for v in values {
                    self.coll_flow_expr(ast, v, tenv, ctx, interner, out, false);
                }
            }
            Node::If { .. } => {
                self.coll_flow_if(ast, id, tenv, ctx, interner, out, stmt_position);
            }
            Node::Case { .. } => {
                self.coll_flow_case(ast, id, tenv, ctx, interner, out, stmt_position);
            }
            Node::Definition { body, .. }
            | Node::ClassDef { body, .. }
            | Node::ModuleDef { body, .. } => {
                // Independent local scope: fresh env, no effect on the enclosing one.
                let body = body.clone();
                let mut t = TypeEnv::new();
                self.coll_flow_scope(ast, &body, &mut t, ctx, interner, out, true);
            }
            // Unmodeled statement (`while`/`until`, `begin`/`rescue`, ivar
            // writes, …): widen every local it writes and do NOT descend.
            other => {
                let span = other.span();
                widen_flow_writes(ctx.writes, span, tenv, interner);
            }
        }
    }

    /// Evaluate an expression: record collection-typed receiver uses, thread
    /// rebinds, apply the keep-nominal mutator widening, and widen conservatively
    /// for everything unmodeled.
    #[allow(clippy::too_many_arguments)]
    fn coll_flow_expr(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        ctx: &CollCtx<'_>,
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
        stmt_position: bool,
    ) {
        match ast.get(id) {
            Node::Call { receiver, method, args, block_body, safe_nav, span, .. } => {
                let receiver = *receiver;
                let method = method.clone();
                let args = args.clone();
                let block_body = block_body.clone();
                let safe_nav = *safe_nav;
                let call_span = *span;
                // The receiver evaluates first (a nested `a.b` in `a.b.c`) —
                // EXPRESSION position, so no block/`case` under it establishes
                // anything.
                if let Some(r) = receiver {
                    self.coll_flow_expr(ast, r, tenv, ctx, interner, out, false);
                }
                // The bare-local receiver name, if any. Safe-nav dispatch is out
                // of the envelope on both the recording and the widening side.
                let local = match (receiver, safe_nav) {
                    (Some(r), false) => match ast.get(r) {
                        Node::LocalVariableRead { name, .. } => Some(name.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                // Record the use BEFORE any of the call's own effects — the
                // receiver read happens first (`output << 'a'` reads the seed).
                if let Some(name) = &local {
                    if let Some(cls) = tenv.get(name).and_then(|&ty| self.coll_carrier(interner, ty))
                    {
                        out.insert(id, cls);
                    }
                }
                // The mutator's effect is decided by the PRE-call carrier
                // (`widen_for_mutator`, `mutation_widening.rb:209`).
                let widened = local.as_ref().and_then(|name| {
                    let ty = *tenv.get(name)?;
                    self.coll_widen_for_mutator(interner, ty, &method).map(|c| (name.clone(), c))
                });
                // Arguments are EXPRESSION position.
                for a in &args {
                    self.coll_flow_expr(ast, *a, tenv, ctx, interner, out, false);
                }
                if block_body.is_empty() {
                    // Widen every write the call span contains (the receiver-side
                    // mutator entry itself, and argument-position mutations via
                    // `indexed_flow_writes`); the modeled mutator effect is
                    // re-applied below.
                    widen_flow_writes(ctx.writes, call_span, tenv, interner);
                } else if stmt_position {
                    // The block body evaluates in a CHILD env seeded from the
                    // outer one (uses inside the block are recorded there) …
                    let pre = tenv.clone();
                    let mut btenv = tenv.clone();
                    self.coll_flow_scope(ast, &block_body, &mut btenv, ctx, interner, out, true);
                    // … then every write the call span contains widens (a block
                    // REBIND of a captured local is visible outside and kills the
                    // carrier — probe m15) …
                    widen_flow_writes(ctx.writes, call_span, tenv, interner);
                    // … and finally `widen_after_block` (`mutation_widening.rb:144`)
                    // re-applies the mutations. That routine is a SYNTACTIC walk
                    // of the block body against the OUTER scope, NOT a join of the
                    // block's evaluated scope: its own doc names `arr.push(x) if
                    // cond` as a case it catches, so a branch-contained mutation
                    // inside a block still widens the outer binding (which is why
                    // the gitlab jira-tracker / ddl-lock rows fire in the
                    // reference). We mirror it exactly, off the PRE-call carriers.
                    for (name, cls) in self.coll_block_mutations(ast, &block_body, &pre, interner) {
                        if !rebound_within(ctx.rebinds, call_span, &name) {
                            if let Some(ty) = self.coll_nominal(interner, cls) {
                                tenv.insert(name, ty);
                            }
                        }
                    }
                } else {
                    // Expression-position block: no descent (the position rule),
                    // and every contained write widens.
                    widen_flow_writes(ctx.writes, call_span, tenv, interner);
                }
                // Keep-nominal widening — unless something inside the call
                // REBOUND the same local (`output << (output = x)`).
                if let Some((name, cls)) = widened {
                    if !rebound_within(ctx.rebinds, call_span, &name) {
                        if let Some(ty) = self.coll_nominal(interner, cls) {
                            tenv.insert(name, ty);
                        }
                    }
                }
            }
            // `&&`/`||` — the RHS may not execute, so its effects are unmodeled:
            // evaluate both sides on a THROWAWAY env (uses are still recorded
            // against the pre-logical bindings) and widen every contained write.
            Node::Logical { left, right, span, .. } => {
                let (left, right, lspan) = (*left, *right, *span);
                let mut scratch = tenv.clone();
                self.coll_flow_expr(ast, left, &mut scratch, ctx, interner, out, false);
                self.coll_flow_expr(ast, right, &mut scratch, ctx, interner, out, false);
                widen_flow_writes(ctx.writes, lspan, tenv, interner);
            }
            Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                // The RHS reads the PRE-write binding; an assignment RHS keeps
                // the statement's own position (the narrowing pass's probe p1).
                self.coll_flow_expr(ast, value, tenv, ctx, interner, out, stmt_position);
                let vty = self.type_of(ast, value, tenv, interner);
                tenv.insert(name, vty);
            }
            Node::MultiWrite { targets, value, .. } => {
                let (targets, value) = (targets.clone(), *value);
                self.coll_flow_expr(ast, value, tenv, ctx, interner, out, stmt_position);
                let rhs = self.type_of(ast, value, tenv, interner);
                for (name, ty) in multi_target_binder::bind(&targets, rhs, interner) {
                    tenv.insert(name, ty);
                }
            }
            // Op-writes (`output += [1]`) are unmodeled — the reference folds
            // `Tuple + Tuple` and keeps the literal shape (probe m16); mirroring
            // that fold is coverage, not FP-safety, so the target widens.
            Node::LocalVariableOpWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                self.coll_flow_expr(ast, value, tenv, ctx, interner, out, false);
                let u = interner.untyped();
                tenv.insert(name, u);
            }
            Node::If { .. } => {
                self.coll_flow_if(ast, id, tenv, ctx, interner, out, false);
            }
            Node::Case { .. } => {
                self.coll_flow_case(ast, id, tenv, ctx, interner, out, stmt_position);
            }
            // Every other expression kind is unmodeled: bind nothing, and widen
            // every write its span contains (a mutation buried in an array
            // literal, an interpolation, a pattern, …).
            other => {
                let span = other.span();
                widen_flow_writes(ctx.writes, span, tenv, interner);
            }
        }
    }

    /// Thread one `if`/`unless`/ternary. The two edges run on clones of the
    /// pre-conditional env and are joined by IDENTICAL `TypeId` only
    /// ([`join_flow_envs`]) — the mirror of the reference's `Scope#join` union
    /// plus `receiver_descriptor`'s missing `Type::Union` arm (probes m18/m20
    /// silent, m01/m19 fire).
    #[allow(clippy::too_many_arguments)]
    fn coll_flow_if(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        ctx: &CollCtx<'_>,
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
        stmt_position: bool,
    ) {
        let Node::If { predicate, then_body, else_body, is_unless, .. } = ast.get(id) else {
            return;
        };
        let (predicate, is_unless) = (*predicate, *is_unless);
        let (then_body, else_body) = (then_body.clone(), else_body.clone());
        // The predicate evaluates first, in the pre-conditional bindings.
        self.coll_flow_expr(ast, predicate, tenv, ctx, interner, out, false);
        let (truthy, falsey) =
            if is_unless { (&else_body, &then_body) } else { (&then_body, &else_body) };
        let mut t = tenv.clone();
        self.coll_flow_scope(ast, truthy, &mut t, ctx, interner, out, stmt_position);
        let mut f = tenv.clone();
        self.coll_flow_scope(ast, falsey, &mut f, ctx, interner, out, stmt_position);
        *tenv = join_flow_envs(&t, &f, interner);
    }

    /// Thread one `case`/`when`. Every clause body runs on a clone of the
    /// pre-`case` env and all of them are joined together WITH the pre-`case` env
    /// (the implicit no-match path) — so a clause-contained mutation on a
    /// not-yet-widened seed widens to untyped (m18), while a pre-widened nominal
    /// survives every arm (m19). A `case`/`in` pattern carrier is unmodeled: the
    /// whole construct's writes widen and nothing is threaded.
    #[allow(clippy::too_many_arguments)]
    fn coll_flow_case(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        ctx: &CollCtx<'_>,
        interner: &mut Interner,
        out: &mut HashMap<NodeId, &'static str>,
        stmt_position: bool,
    ) {
        let Node::Case { predicate, branches, else_body, span } = ast.get(id) else {
            return;
        };
        let (predicate, case_span) = (*predicate, *span);
        let (branches, else_body) = (branches.clone(), else_body.clone());
        if let Some(p) = predicate {
            self.coll_flow_expr(ast, p, tenv, ctx, interner, out, false);
        }
        // The no-match path: the pre-`case` bindings, unchanged.
        let mut acc = tenv.clone();
        for br in branches {
            let Node::When { conditions, body, .. } = ast.get(br) else {
                // `case`/`in` pattern carrier — unmodeled, no descent.
                widen_flow_writes(ctx.writes, case_span, tenv, interner);
                return;
            };
            let (conditions, body) = (conditions.clone(), body.clone());
            let mut t = tenv.clone();
            for &cond in &conditions {
                self.coll_flow_expr(ast, cond, &mut t, ctx, interner, out, false);
            }
            self.coll_flow_scope(ast, &body, &mut t, ctx, interner, out, stmt_position);
            acc = join_flow_envs(&acc, &t, interner);
        }
        let mut e = tenv.clone();
        self.coll_flow_scope(ast, &else_body, &mut e, ctx, interner, out, stmt_position);
        *tenv = join_flow_envs(&acc, &e, interner);
    }

    /// The collection class a receiver carrier projects to, or `None`. Mirrors
    /// the reference's `receiver_descriptor` (`rbs_dispatch.rb:209-212`): a
    /// `Tuple` dispatches as `Array` and a `HashShape` as `Hash`, and a kept
    /// `Nominal[Array|Hash]` (mutator-widened or tier-folded) dispatches as
    /// itself. Every other carrier — including a `Union` — declines.
    fn coll_carrier(&self, interner: &Interner, ty: TypeId) -> Option<&'static str> {
        match interner.get(ty) {
            Type::Tuple(_) => Some("Array"),
            Type::HashShape(_) => Some("Hash"),
            Type::Nominal { .. } => self.coll_nominal_carrier(interner, ty),
            _ => None,
        }
    }

    /// As [`Typer::coll_carrier`], restricted to a genuine `Nominal[Array|Hash]`
    /// (what a block body may propagate outwards).
    fn coll_nominal_carrier(&self, interner: &Interner, ty: TypeId) -> Option<&'static str> {
        let Type::Nominal { class, .. } = interner.get(ty) else {
            return None;
        };
        ["Array", "Hash"].into_iter().find(|name| self.index.class_id(name) == Some(*class))
    }

    /// The reference's `MutationWidening::widen_for_mutator`
    /// (`reference/rigor/lib/rigor/inference/mutation_widening.rb:209`) as this
    /// pass needs it: the collection class the receiver local carries AFTER the
    /// mutation, or `None` when nothing survives.
    ///
    /// - `Tuple` + an ARRAY mutator ⇒ `"Array"` (`widen_tuple:251` — the nominal
    ///   is KEPT, which is the whole slice); `HashShape` + a HASH mutator ⇒
    ///   `"Hash"` (`widen_hash_shape:265`).
    /// - An already-widened `Nominal[Array|Hash]` under a mutator of ITS OWN
    ///   table has "no precision to lose" — the reference leaves the scope
    ///   untouched, so we re-assert the same nominal (the caller widens the whole
    ///   call span first, and this is what survives it: probes m01/m03/m17, where
    ///   the SECOND and later mutations run on an already-nominal carrier).
    /// - Everything else declines: the tables are per-shape and NOT their union
    ///   (a Hash-only mutator on a Tuple is a no-op), and a non-shape carrier
    ///   never MINTS a binding (probes m08/m13, and the bucket-E `[]=`-on-a-
    ///   String rows we must not mirror).
    fn coll_widen_for_mutator(
        &self,
        interner: &Interner,
        ty: TypeId,
        method: &str,
    ) -> Option<&'static str> {
        match interner.get(ty) {
            Type::Tuple(_) if ARRAY_MUTATORS.contains(&method) => Some("Array"),
            Type::HashShape(_) if HASH_MUTATORS.contains(&method) => Some("Hash"),
            Type::Nominal { .. } => match self.coll_nominal_carrier(interner, ty)? {
                "Array" if ARRAY_MUTATORS.contains(&method) => Some("Array"),
                "Hash" if HASH_MUTATORS.contains(&method) => Some("Hash"),
                _ => None,
            },
            _ => None,
        }
    }

    /// The reference's `walk_for_outer_mutations` (`mutation_widening.rb:153`)
    /// over one block body: every `local.<mutator>(…)` call in the body's SPAN —
    /// nested blocks included, exactly as the reference recurses — applied in
    /// source order against the carriers `pre` held at the call. Returns the
    /// final `local -> "Array"|"Hash"` widenings.
    ///
    /// This is a SYNTACTIC walk on purpose. The reference does not join the
    /// block's evaluated scope here; it rewrites the outer scope directly, so a
    /// branch-contained mutation inside a block (`arr.push(x) if cond`, named in
    /// its own doc comment) still widens the outer binding — unlike the
    /// branch-contained mutation of probes m18/m20, which lives in the METHOD
    /// body and does go through `Scope#join`.
    ///
    /// The one piece we cannot mirror is Prism's `depth` capture check
    /// (`widen_for_outer_receiver:175`): the lowered AST has no block-parameter
    /// list, so a block param SHADOWING an outer local (`xs.each { |output| … }`)
    /// widens the outer binding here where the reference leaves it alone. That is
    /// FP-neutral by construction — the widening only ever rewrites `Tuple` to
    /// `Nominal[Array]` (or `HashShape` to `Nominal[Hash]`), and both project to
    /// the SAME dispatch class (`receiver_descriptor:209`), so no use site can
    /// change its recorded class because of it.
    fn coll_block_mutations(
        &self,
        ast: &LoweredAst,
        block_body: &[NodeId],
        pre: &TypeEnv,
        interner: &mut Interner,
    ) -> Vec<(String, &'static str)> {
        let Some(body_span) = span_hull(ast, block_body) else {
            return Vec::new();
        };
        let mut carriers: HashMap<String, TypeId> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        let mut hits: Vec<(rigor_parse::Span, String, String)> = ast
            .iter()
            .filter_map(|(_, n)| match n {
                Node::Call { receiver: Some(r), method, safe_nav: false, span, .. }
                    if body_span.0 <= span.0 && span.1 <= body_span.1 =>
                {
                    match ast.get(*r) {
                        Node::LocalVariableRead { name, .. } => {
                            Some((*span, name.clone(), method.clone()))
                        }
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect();
        hits.sort_by_key(|(s, _, _)| *s);
        for (_, name, method) in hits {
            let Some(&cur) = carriers.get(&name).or_else(|| pre.get(&name)) else {
                continue;
            };
            let Some(cls) = self.coll_widen_for_mutator(interner, cur, &method) else {
                continue;
            };
            let Some(ty) = self.coll_nominal(interner, cls) else {
                continue;
            };
            if carriers.insert(name.clone(), ty).is_none() {
                order.push(name);
            }
        }
        order
            .into_iter()
            .filter_map(|name| {
                let ty = *carriers.get(&name)?;
                Some((name, self.coll_nominal_carrier(interner, ty)?))
            })
            .collect()
    }

    /// The bare `Nominal[C]` carrier for `"Array"` / `"Hash"`. Elements are NOT
    /// tracked (`args: vec![]`): undefined-method witnessing is a class-only
    /// lookup, and the reference's own widening fixes an empty seed's elements
    /// at `untyped` on the first mutation anyway (`widen_tuple`, `:251`).
    fn coll_nominal(&self, interner: &mut Interner, class_name: &str) -> Option<TypeId> {
        let class = self.index.class_id(class_name)?;
        Some(interner.intern(Type::Nominal { class, args: vec![] }))
    }
}

/// The per-program write tables the collection-shape walker threads (bundled to
/// keep the recursive walkers' signatures under the clippy argument limit).
struct CollCtx<'a> {
    /// [`collect_flow_writes`] + [`indexed_flow_writes`] — rebinds AND in-place
    /// mutations, the conservative widening set.
    writes: &'a [(rigor_parse::Span, String)],
    /// REBINDS only (no mutator entries) — used to veto the keep-nominal
    /// widening when the call that mutates also reassigns the same local.
    rebinds: &'a [(rigor_parse::Span, String)],
}

/// Whether `name` is REBOUND (not merely mutated) somewhere inside `span`.
fn rebound_within(
    rebinds: &[(rigor_parse::Span, String)],
    span: rigor_parse::Span,
    name: &str,
) -> bool {
    rebinds.iter().any(|(ws, n)| n == name && span.0 <= ws.0 && ws.1 <= span.1)
}

/// The smallest span covering every node in `ids`, or `None` when empty.
fn span_hull(ast: &LoweredAst, ids: &[NodeId]) -> Option<rigor_parse::Span> {
    let mut it = ids.iter().map(|&id| ast.get(id).span());
    let first = it.next()?;
    Some(it.fold(first, |acc, s| (acc.0.min(s.0), acc.1.max(s.1))))
}

/// `reference/rigor/lib/rigor/inference/mutation_widening.rb:70` verbatim.
const ARRAY_MUTATORS: &[&str] = &[
    "<<", "push", "append", "prepend", "unshift", "concat", "insert", "pop", "shift", "delete",
    "delete_at", "delete_if", "reject!", "clear", "compact!", "replace", "fill", "[]=", "map!",
    "collect!", "select!", "filter!", "keep_if", "uniq!", "flatten!", "sort!", "sort_by!",
    "reverse!", "rotate!", "shuffle!", "slice!",
];

/// `reference/rigor/lib/rigor/inference/mutation_widening.rb:82` verbatim.
const HASH_MUTATORS: &[&str] = &[
    "[]=", "store", "delete", "delete_if", "reject!", "select!", "filter!", "keep_if", "clear",
    "compact!", "merge!", "update", "transform_keys!", "transform_values!", "replace",
];

/// The REBIND half of [`collect_flow_writes`] — local assignments only, with the
/// in-place-mutation entries left out. The collection-shape pass needs the two
/// apart: a mutation KEEPS the nominal while a rebind kills it, and the merged
/// table cannot tell them apart.
fn collect_rebind_writes(ast: &LoweredAst) -> Vec<(rigor_parse::Span, String)> {
    ast.iter()
        .flat_map(|(_, n)| match n {
            Node::LocalVariableWrite { name, span, .. }
            | Node::LocalVariableOpWrite { name, span, .. } => vec![(*span, name.clone())],
            Node::MultiWrite { targets, span, .. } => targets
                .bound_names()
                .into_iter()
                .map(|(name, _)| (*span, name))
                .collect(),
            _ => Vec::new(),
        })
        .collect()
}

/// Whether a branch body's final statement EXITS the surrounding control flow
/// — a `return`, an argument-less `next`/`break`, or a receiverless `raise` (a
/// conservative approximation of the reference's
/// `branch_unconditionally_exits?`, `statement_evaluator.rb:2836`; missing a
/// termination only loses narrowing, never adds one). Descends the pure
/// statement carriers
/// (`Statements`, and a `BeginRescue` with no rescue clauses and no ensure —
/// the lowered `else`-clause / parenthesized-group shape; a real
/// `begin`/`rescue` declines, its tail `return` may be skipped by a raise
/// before it).
fn branch_terminates(ast: &LoweredAst, body: &[NodeId]) -> bool {
    match body.last() {
        Some(&last) => stmt_terminates(ast, last),
        None => false,
    }
}

fn stmt_terminates(ast: &LoweredAst, id: NodeId) -> bool {
    match ast.get(id) {
        Node::Return { .. } => true,
        // An argument-less `next`/`break` (`Node::Other`'s `jump` tag). The
        // reference accepts them unconditionally — no in-block gate, no
        // loop-body special case — and the probe matrix reproduces that:
        // `next`/`break` in a block (`p1`/`p2`/`p3`), in a `while`/`until` body
        // (`p5`/`p5b`/`p5c`), in a `lambda`/`define_method`/`loop`/`times`
        // block (`q15`/`q16`/`r2`/`q18`) all narrow past the guard, and the
        // loop-carried rebind AFTER the use (`p6`, `r11` — the shape 3b-1
        // declined loop BODIES over) still fires there. We reach a strict
        // subset of that: a `while`/`until` BODY is never descended
        // (`Node::Loop`), a fact never escapes the block (`join_cenv` keeps
        // only `Bot` — probes `p9`/`p9b`/`p13`/`q10`/`r13`, all
        // reference-silent), and a rebind BEFORE the use kills it (`q17`).
        Node::Other { jump: Some(_), .. } => true,
        Node::Call { receiver: None, method, .. } if method == "raise" => true,
        Node::BeginRescue { body, ensure_body, clauses, .. }
            if clauses.is_empty() && ensure_body.is_empty() =>
        {
            branch_terminates(ast, body)
        }
        Node::Statements { body, .. } => branch_terminates(ast, body),
        _ => false,
    }
}

/// Is `id` a binding VALUE whose carrier BOTH engines type `Dynamic`/`Top`?
///
/// The allow-list half of the carrier-fidelity fix
/// (docs/notes/20260808-narrowing-carrier-fidelity-fp.md). `narrow_class_other`
/// narrows a `Dynamic`/`Top` carrier only, so "we narrow only Dynamic" is a
/// SUBSET rule exactly while `Dynamic` means the same thing in both engines —
/// and it does not: rigor-rs collapses to `Dynamic[top]` a long tail of
/// carriers the reference types precisely (a `Logical` union, a `Range`, a
/// `Proc`, `self`, a `case`/`if` union, `defined?`, a loop's `nil`, …). On each
/// of those our gate fires where theirs declines — a live false positive.
///
/// Enumerating the coarse carriers is a losing game (`__method__`, `proc { }`,
/// `binding` and `defined?` all hide inside the same `Call`/`Statements`
/// carriers as the safe shapes), so the gate is an ALLOW-list instead, and every
/// member is oracle-measured as FIRING on the reference:
///
/// - a bare local that is itself narrowable (a parameter, or bound to a member
///   of this list) — `ctrl_param`, `ctrl_unbound`, `blockparam`, `allow_kwarg`,
///   `allow_optarg`, `allow_restarg`, `allow_block_arg`;
/// - an `@ivar` / `@@cvar` / `$gvar` read — the reference types none of them
///   (`ivar_read`, `gvar_read`, `cvar_read`);
/// - a call THROUGH such a receiver: an untyped receiver resolves no method on
///   either side, so the result is untyped on both (`plain_call_dyn`,
///   `index_read`, `safenav`, `block_call_dyn`, `call_chain`, `call_ivar_recv`,
///   `call_gvar_recv`, `allow_param_index`). `self` is deliberately NOT a
///   narrowable receiver — the reference resolves an in-source method through
///   it and gets that method's real return (`call_self_recv` is an FP), and
///   neither is a `ConstantRead` (`recv_const_float`).
///
/// Everything else declines. That costs coverage on carriers the reference does
/// narrow (`yield`, `super`, an implicit-self call, a `begin`/`ensure` value, a
/// `case` with no `else`, a constant receiver) — a strict subset, never an FP.
fn narrowable_binding(ast: &LoweredAst, id: NodeId, coarse: &HashSet<String>, depth: u32) -> bool {
    if depth == 0 {
        return false;
    }
    match ast.get(id) {
        Node::LocalVariableRead { name, .. } => !coarse.contains(name),
        Node::VariableRead { .. } => true,
        Node::Call { receiver: Some(r), .. } => {
            narrowable_binding(ast, *r, coarse, depth - 1)
        }
        _ => false,
    }
}

/// The local names in ONE scope whose binding is not a [`narrowable_binding`] —
/// the set the `is_a?`/`case-when` gate refuses to narrow.
///
/// Scope-wide rather than flow-sensitive: a name coarse at ANY binding site in
/// the scope is coarse throughout it. That is deliberately conservative (a
/// rebind never resurrects narrowability) and needs no extra env threading.
/// The scope is delimited by its statements' byte range MINUS the range of every
/// nested `def`/`class`/`module`, whose locals are their own scope's business
/// (each gets its own `coarse_locals` call).
///
/// Three binding kinds enter the set:
/// - a `LocalVariableWrite` whose value is not a narrowable carrier;
/// - EVERY `LocalVariableOpWrite` (`h ||= {}` is a union on the reference —
///   probe `logical_orassign` — and our op-write arm types it `Dynamic[top]`);
/// - every `rescue => e` capture (the reference binds the exception CLASS).
///
/// `MultiWrite` targets are deliberately absent: destructuring loses precision
/// on both sides, and all three measured shapes (`multiwrite`, `mw_from_call`,
/// `mw_from_logical`) fire on the reference.
fn coarse_locals(ast: &LoweredAst, body: &[NodeId]) -> HashSet<String> {
    let mut coarse: HashSet<String> = HashSet::new();
    let Some(lo) = body.iter().map(|&s| ast.get(s).span().0).min() else {
        return coarse;
    };
    let hi = body.iter().map(|&s| ast.get(s).span().1).max().unwrap_or(lo);
    let nested: Vec<rigor_parse::Span> = ast
        .iter()
        .filter(|(_, n)| {
            matches!(
                n,
                Node::Definition { .. } | Node::ClassDef { .. } | Node::ModuleDef { .. }
            )
        })
        .map(|(_, n)| n.span())
        .filter(|s| lo <= s.0 && s.1 <= hi)
        .collect();
    let inside = |sp: rigor_parse::Span| {
        lo <= sp.0 && sp.1 <= hi && !nested.iter().any(|n| n.0 <= sp.0 && sp.1 <= n.1)
    };
    // (name, Some(value)) — narrowable iff the value is; (name, None) —
    // unconditionally coarse.
    let mut bindings: Vec<(String, Option<NodeId>)> = Vec::new();
    for (_, n) in ast.iter() {
        match n {
            Node::LocalVariableWrite { name, value, span, .. } if inside(*span) => {
                bindings.push((name.clone(), Some(*value)));
            }
            Node::LocalVariableOpWrite { name, span, .. } if inside(*span) => {
                bindings.push((name.clone(), None));
            }
            Node::BeginRescue { clauses, span, .. } if inside(*span) => {
                for c in clauses {
                    if let Some(b) = &c.bound_name {
                        bindings.push((b.clone(), None));
                    }
                }
            }
            _ => {}
        }
    }
    // Fixpoint: a name whose value reads a name that just became coarse is
    // itself coarse. Monotone (the set only grows), so it terminates.
    loop {
        let mut changed = false;
        for (name, value) in &bindings {
            if coarse.contains(name) {
                continue;
            }
            let ok = value.is_some_and(|v| narrowable_binding(ast, v, &coarse, 32));
            if !ok {
                coarse.insert(name.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    coarse
}

/// The conservative branch join for a conditional / `case` / block call.
///
/// A [`ClassFact::Narrowed`] fact never survives a merge in this slice (the
/// reference's union of `Nominal[C]` with itself would, but the decline predates
/// this note and stays). A [`ClassFact::Bot`] fact present on ENTRY does
/// survive, because the reference's join is `Bot | Bot = Bot` (probes
/// `bot_after_inner`, `bot_after_begin`, `bot_after_block_call`) — but only
/// while EVERY edge still carries it. `edges` are the per-edge fact maps the
/// branches were walked with; an edge that REBOUND the local dropped the fact
/// from its own clone, and the reference's union of `Bot` with the new binding
/// is that binding, so the merged fact goes too (probes `bot_rebind_use`,
/// `bot_block_rebind`). An empty `edges` means the construct was not descended,
/// so there is no edge evidence and every entry `Bot` rides through — the
/// caller pairs that with a span-based [`kill_cenv_writes`].
///
/// Facts the EDGES established are not in `cenv` at all (edges walk clones), so
/// nothing minted inside a branch can leak out through here.
///
/// Stage 3a-3: EVERY chain fact dies at a join, unconditionally — a chain `Bot`
/// included, unlike the LOCAL `Bot` the retain below preserves. The reference
/// agrees that a chain narrowing established inside a branch does not escape it
/// (probe `n_escape_after_if`, reference-silent), and a `Bot` that escaped a
/// branch would SUPPRESS rather than merely go silent, so the blanket wipe is
/// the conservative side. The one place a chain fact must outlive the join is the
/// early-return propagation, which [`Typer::class_flow_if`] re-seeds explicitly
/// from a PRE-join snapshot — see its comment on the sequential-disjoint
/// hazard.
///
/// Join-retention slice (2026-08-09): the two CONDITIONAL callers
/// ([`Typer::class_flow_if`], [`Typer::class_flow_case`]) pair this wipe with
/// [`retain_joined_facts`], which puts back every pre-join fact the edges left
/// untouched. The wipe itself stays exactly as written, because the other four
/// callers pass NO edges (a construct that was not descended) and must keep the
/// blanket-clear + span-kill discipline.
fn join_cenv(cenv: &mut Facts, edges: &[Facts]) {
    cenv.locals.retain(|name, fact| {
        *fact == ClassFact::Bot
            && edges.iter().all(|edge| edge.locals.get(name) == Some(&ClassFact::Bot))
    });
    cenv.chains.clear();
}

/// Put back, after [`join_cenv`], every PRE-join fact that survived the
/// conditional untouched — the reference's `Scope#join` keeps a local's type
/// whenever both edges agree on it (`scope.rb:680`), and a fact minted BEFORE
/// the conditional is on both edges by construction. Master wiped them all, so
/// a fact died at ANY later `if`/`unless`/`case`, terminating or not, related or
/// not (the 2026-08-09 probe matrix: 10 of 14 rows diverged, every one a
/// coverage loss, plus one live FP — a disjoint re-guard AFTER an intervening
/// `if` minted against the wiped env and witnessed where the reference's meet
/// had already reached `Bot`).
///
/// A fact is restored only when EVERY edge still carries it IDENTICALLY. That
/// single test subsumes the spec's separate criteria:
///
/// - a REBIND inside a branch removed the fact from that edge's clone
///   (`branch_rebind_one_side` / `write_to_a_in_if`, both reference-silent for
///   us — the reference's real union `1 | String` is the separate widen gap);
/// - the conditional's OWN guard targets moved on at least one edge whenever the
///   guard did anything (`if a.is_a?(Hash)` after a `String` guard leaves `Bot`
///   on the truthy edge and `String` on the falsey edge), so the sequential-meet
///   and `Bot`-collapse results are never resurrected;
/// - a call on a chain ROOT inside a branch fired `invalidate_chain_after_call`
///   on that edge, which is invisible to `writes` (probe
///   `chain_call_on_root_in_branch`, reference-silent).
///
/// `writes` + `span` add the span-containment kill on top, for the rebinds an
/// edge cannot see — a `case`/`in` clause is not descended, so its rebinds are
/// invisible to the edge evidence (`case_in_rebinds_target`, reference-silent).
/// `chains` is the caller's gate for the same reason: only a construct whose
/// every branch was DESCENDED has trustworthy chain edges.
///
/// Facts the edges MINTED are absent from `pre` (edges walk clones of it), so
/// nothing established inside a branch escapes through here — the block/loop
/// escape rules (probes `n_escape_after_if`, p9/p13) are untouched.
fn retain_joined_facts(
    cenv: &mut Facts,
    pre: &Facts,
    edges: &[Facts],
    writes: &[(rigor_parse::Span, String)],
    span: rigor_parse::Span,
    chains: Option<&HashSet<String>>,
) {
    if edges.is_empty() {
        return;
    }
    let written =
        |name: &str| writes.iter().any(|(ws, n)| n == name && span.0 <= ws.0 && ws.1 <= span.1);
    for (name, fact) in &pre.locals {
        if written(name) {
            continue;
        }
        if edges.iter().all(|edge| edge.locals.get(name) == Some(fact)) {
            cenv.locals.insert(name.clone(), fact.clone());
        }
    }
    let Some(predicate_locals) = chains else { return };
    for (addr, fact) in &pre.chains {
        if written(&addr.0) || predicate_locals.contains(&addr.0) {
            continue;
        }
        if edges.iter().all(|edge| edge.chains.get(addr) == Some(fact)) {
            cenv.chains.insert(addr.clone(), fact.clone());
        }
    }
}

/// Every local NAME read or written anywhere inside `span` — used as the
/// chain-restore gate in [`retain_joined_facts`].
///
/// A conditional's PREDICATE gets no edge evidence of its own: the edges are
/// clones taken after it ran, so a predicate that narrowed a chain address in a
/// way [`Typer::analyse_predicate`] does not RECOGNISE leaves both edges
/// agreeing on the stale incoming fact. `guard_predicate` requires a bare LOCAL
/// operand, so `String === h.last` and `h.last.nil?` are not chain guards at
/// all — and `return unless String === h.last` after a disjoint `is_a?` guard is
/// reference-SILENT (row `chain_caseeq_disjoint`), so restoring there would be a
/// live false positive. Any mention of the ROOT in the predicate therefore
/// declines the restore for every address rooted at it, mirroring the
/// any-mention widening [`Facts::kill_chains_rooted_at`] already applies to
/// calls.
fn locals_in_span(ast: &LoweredAst, span: rigor_parse::Span) -> HashSet<String> {
    let mut out = HashSet::new();
    for (_, n) in ast.iter() {
        let (name, nspan) = match n {
            Node::LocalVariableRead { name, span } => (name, span),
            Node::LocalVariableWrite { name, span, .. }
            | Node::LocalVariableOpWrite { name, span, .. } => (name, span),
            _ => continue,
        };
        if span.0 <= nspan.0 && nspan.1 <= span.1 {
            out.insert(name.clone());
        }
    }
    out
}

/// The stable single-hop chain address of `id`, if it has one — the port of the
/// reference's `stable_chain_address` (`narrowing.rb:1826`) restricted to LOCAL
/// roots.
///
/// `Some((root, method))` iff `id` is a `Call` whose receiver is a bare
/// `LocalVariableRead`, with NO arguments, NO block and NO safe-nav. The
/// reference's ivar arm is DECLINED: the arena's `VariableRead` carries no name
/// (spec row `c7b`, a recorded coverage gap that needs a lowering change first).
///
/// Every other decline is measured reference-silent as well: arguments on the
/// hop (`c7e`), a block on the hop (`m_block_on_hop`), a two-hop chain
/// (`m_two_hop`). A safe-nav hop is the one exception — the reference fires
/// (`m_safe_nav_hop`) and we decline, matching `stable_chain_address`'s own
/// shape gate as ported plus the slice-wide safe-nav decline.
fn stable_chain_address(ast: &LoweredAst, id: NodeId) -> Option<ChainAddr> {
    let Node::Call { receiver: Some(r), method, args, block_body, safe_nav, .. } = ast.get(id)
    else {
        return None;
    };
    if *safe_nav || !args.is_empty() || !block_body.is_empty() {
        return None;
    }
    let Node::LocalVariableRead { name, .. } = ast.get(*r) else { return None };
    Some((name.clone(), method.clone()))
}

/// Does this predicate operand's subtree contain a `=~` whose RECEIVER is not a
/// bare variable read — i.e. the `/(?<name>…)/ =~ str` shape, which binds every
/// named capture group as a local?
///
/// Prism models it as a `MatchWriteNode`; the arena has no lowering for one, so
/// the bound locals appear only as unbound reads and the narrowing gate treats
/// them as untyped. The reference binds them to `String`, so a following
/// `v.is_a?(Hash)` reaches `Bot` there and witnesses nothing (probe
/// `matchwrite`). The binding is arena-INVISIBLE, so the only safe answer is to
/// decline the whole compound predicate. `v =~ /a/` — a variable receiver —
/// binds nothing and is deliberately excluded (`matchop_keep` fires on both
/// engines).
fn regex_binding_match(ast: &LoweredAst, id: NodeId, depth: u32) -> bool {
    if depth == 0 {
        return true; // out of budget ⇒ answer conservatively
    }
    match ast.get(id) {
        Node::Call { receiver, method, args, .. } => {
            if method == "=~"
                && !matches!(
                    receiver.map(|r| ast.get(r)),
                    Some(Node::LocalVariableRead { .. } | Node::VariableRead { .. })
                )
            {
                return true;
            }
            receiver.is_some_and(|r| regex_binding_match(ast, r, depth - 1))
                || args.iter().any(|&a| regex_binding_match(ast, a, depth - 1))
        }
        Node::Logical { left, right, .. } => {
            regex_binding_match(ast, *left, depth - 1)
                || regex_binding_match(ast, *right, depth - 1)
        }
        Node::Statements { body, .. } => {
            body.iter().any(|&s| regex_binding_match(ast, s, depth - 1))
        }
        _ => false,
    }
}

/// The JOIN of two edges' guard maps — the reference's `Scope#join`, which
/// unions the two scopes' types per local (`analyse_and`'s falsey edge,
/// `analyse_or`'s truthy edge; `narrowing.rb:2631,2640`).
///
/// A local absent from EITHER side carries its unchanged (un-narrowed) type
/// there, and a union with the unchanged type is the unchanged type — so the
/// join keeps only locals present on BOTH sides (probes `c1g`, `f12`,
/// `a_two_locals_else`, `b2_and_bang_two_locals`, `u_and_or_falsey`, all
/// reference-silent). Class names union: identical on both sides the fact
/// survives intact and MINTS (`b2_and_bang_same`, `x_or_same_class` both fire
/// on the reference); different, the reference narrows to a real union
/// (`Hash | String` — `b2_and_bang_diff`, `x_or_diff_class`) which stage 3a-4
/// would represent and this slice declines by keeping the fact un-mintable.
/// `exact` weakens to `is_a?` semantics (the STRONGER collapse requirement, so
/// the join never suppresses more than either side alone).
fn join_guards(a: &GuardMap, b: &GuardMap) -> GuardMap {
    let mut out: GuardMap = Vec::new();
    for (target, ga) in a {
        let Some((_, gb)) = b.iter().find(|(n, _)| n == target) else { continue };
        let mut classes = ga.classes.clone();
        for class in &gb.classes {
            if !classes.contains(class) {
                classes.push(class.clone());
            }
        }
        out.push((
            target.clone(),
            GuardFact {
                classes,
                exact: ga.exact && gb.exact,
                mintable: ga.mintable && gb.mintable,
                // Both sides address the SAME node when the target is a chain
                // (the join keys on the address), so either id is the carrier
                // gate's subject; `a`'s is taken for determinism.
                chain_call: ga.chain_call,
            },
        ));
    }
    out
}

/// Kill the class-narrowing fact of every local whose recorded write/mutation
/// span is contained in `span` — the `cenv` counterpart of
/// [`widen_flow_writes`].
fn kill_cenv_writes(
    writes: &[(rigor_parse::Span, String)],
    span: rigor_parse::Span,
    cenv: &mut Facts,
) {
    for (wspan, name) in writes {
        if span.0 <= wspan.0 && wspan.1 <= span.1 {
            cenv.kill_local(name);
        }
    }
}

/// [`kill_cenv_writes`] restricted to [`ClassFact::Narrowed`], for the sites
/// whose contents were DESCENDED (so a real rebind already removed the fact
/// through a write arm) and where the recorded span therefore stands for a
/// MUTATION — a `MUTATOR_METHODS` receiver, a mutated argument position.
/// A mutation widens a CARRIER, and `Bot` has no carrier to widen: the
/// reference keeps `Bot` across `h.push(3)` and stays silent afterwards (probe
/// `bot_mutator_use`), while the narrowing fact must still die there.
///
/// Stage 3a-3: a CHAIN fact rooted at the named local dies here unconditionally
/// — a chain `Bot` included (a mutated root invalidates the ADDRESS, so the
/// collapse no longer describes anything), and a mutation of the root is exactly the
/// "intervening call against the same root receiver" the reference drops on
/// (`invalidate_chain_after_call`; probe `n_root_mutator`, reference-silent).
fn kill_cenv_narrowed(
    writes: &[(rigor_parse::Span, String)],
    span: rigor_parse::Span,
    cenv: &mut Facts,
) {
    for (wspan, name) in writes {
        if span.0 <= wspan.0 && wspan.1 <= span.1 {
            cenv.kill_chains_rooted_at(name);
            if cenv.locals.get(name).is_some_and(|f| *f != ClassFact::Bot) {
                cenv.locals.remove(name);
            }
        }
    }
}

/// In-place mutator methods that invalidate a value-pinned literal-shape carrier
/// (`Tuple` / `HashShape`) bound to a local — the union of the reference's
/// `MutationWidening::ARRAY_MUTATORS` and `HASH_MUTATORS`
/// (`reference/rigor/lib/rigor/inference/mutation_widening.rb:70-87`), minus the
/// `PURE_SELF_RETURNERS` (`freeze`/`dup`/`clone`/`itself`), which never appear
/// here. A call `local.<m>(…)` for `m` in this set mutates `local`'s content, so
/// the literal arity/pair-set the shape carrier tracked is no longer justified —
/// the binding must widen (see [`collect_flow_writes`]).
const MUTATOR_METHODS: &[&str] = &[
    // ARRAY mutators
    "<<", "push", "append", "prepend", "unshift", "concat", "insert", "pop", "shift", "delete",
    "delete_at", "delete_if", "reject!", "clear", "compact!", "replace", "fill", "[]=", "map!",
    "collect!", "select!", "filter!", "keep_if", "uniq!", "flatten!", "sort!", "sort_by!",
    "reverse!", "rotate!", "shuffle!", "slice!",
    // HASH mutators not already listed above
    "store", "merge!", "update", "transform_keys!", "transform_values!",
];

/// Collect every flow-write `(span, name)` in the arena, once, for
/// span-containment widening in the flow passes. Orphan-proof: a write under a
/// lossily-lowered wrapper is still found by its span. Records two kinds:
///
/// - local-variable rebinds (`LocalVariableWrite`/`LocalVariableOpWrite`) — the
///   assignment invalidates the prior binding;
/// - **multi-write targets** (`a, (b, c), *rest = rhs`) — every local name the
///   destructure binds, keyed by the WHOLE multi-write span (the same
///   whole-statement key a single-target write uses). Before `Node::MultiWrite`
///   existed these names were absent from the arena entirely, so a multi-write
///   rebind never widened an earlier straight-line binding — a live
///   `flow.always-truthy-condition` false positive;
/// - **in-place content mutations** — a call `local.<mutator>(…)` whose receiver
///   is a bare local read and whose method is in [`MUTATOR_METHODS`], keyed by the
///   whole-call span. This is the port of the reference's `MutationWidening`
///   (`widen_after_call` + `widen_after_block`): the mutator forgets the literal
///   shape, so the containing flow construct widens `local` the same way a rebind
///   inside it would. `ast.iter()` already descends nested block/case bodies, so a
///   mutation deep inside an `each`/`case` is found and its span is contained by
///   the enclosing construct; a straight-line mutation is its own containing span
///   and widens through the catch-all/`If` arms.
pub fn collect_flow_writes(ast: &LoweredAst) -> Vec<(rigor_parse::Span, String)> {
    ast.iter()
        .flat_map(|(_, n)| match n {
            Node::LocalVariableWrite { name, span, .. }
            | Node::LocalVariableOpWrite { name, span, .. } => vec![(*span, name.clone())],
            Node::MultiWrite { targets, span, .. } => targets
                .bound_names()
                .into_iter()
                .map(|(name, _)| (*span, name))
                .collect(),
            Node::Call { receiver: Some(r), method, span, .. }
                if MUTATOR_METHODS.contains(&method.as_str()) =>
            {
                match ast.get(*r) {
                    Node::LocalVariableRead { name, .. } => vec![(*span, name.clone())],
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        })
        .collect()
}

/// The flow writes that need the project index or a cross-node lookup, appended
/// to [`collect_flow_writes`]'s per-node set. Two kinds, both pure widening:
///
/// - **argument-position mutation** — a call `f(…, local, …)` whose callee
///   mutates the matching POSITIONAL parameter in place
///   (`SourceIndex::method_mutates_param`). Keyed by the whole-call span, like
///   the receiver-side mutator entry, so the enclosing construct widens `local`.
///   This is the caller-side half of the reference's `MutationWidening`;
///   `xs = []; fill xs; if xs.length == 1` must not fold (rigor-survey
///   `rspec-core/lib/rspec/core/world.rb:179`).
/// - **block-scoped rescue binding** — a `rescue => e` clause inside a BLOCK
///   body writes `e` in the enclosing method's scope when `e` is already a local
///   there, and a block runs an unknown number of times, so the binding must
///   widen. `RescueClause::bound_name` is not a `LocalVariableWrite` node, so
///   the per-node scan cannot see it. Keyed by the enclosing BLOCK CALL's span,
///   NOT the clause's: a method-level `begin … rescue => e; end` must keep
///   folding, because the reference keeps folding it (probed both ways), and
///   keying on the clause would widen that case too — a coverage loss, not a
///   false positive, but a needless one. rigor-survey
///   `net-imap-0.6.4.1/lib/net/imap.rb:1470`.
fn indexed_flow_writes(
    ast: &LoweredAst,
    source: &SourceIndex,
) -> Vec<(rigor_parse::Span, String)> {
    let mut out = Vec::new();

    for (_, n) in ast.iter() {
        if let Node::Call { method, args, span, .. } = n {
            for (i, &arg) in args.iter().enumerate() {
                if !source.method_mutates_param(method, i) {
                    continue;
                }
                if let Node::LocalVariableRead { name, .. } = ast.get(arg) {
                    out.push((*span, name.clone()));
                }
            }
        }
    }

    // Spans of every call that carries a block, innermost-first per clause.
    let block_calls: Vec<rigor_parse::Span> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::Call { block_body, span, .. } if !block_body.is_empty() => Some(*span),
            _ => None,
        })
        .collect();
    for (_, n) in ast.iter() {
        let Node::BeginRescue { clauses, .. } = n else {
            continue;
        };
        for clause in clauses {
            let Some(name) = &clause.bound_name else {
                continue;
            };
            let enclosing = block_calls
                .iter()
                .filter(|b| b.0 <= clause.span.0 && clause.span.1 <= b.1)
                .min_by_key(|b| b.1 - b.0);
            if let Some(b) = enclosing {
                out.push((*b, name.clone()));
            }
        }
    }

    out
}

/// Extend a lexical self-qualified name with a nested class/module `name`,
/// mirroring the `SourceIndex` qualified-owner walk so the flow-eval self context
/// and the fold table agree (`Some("Gitlab")` + `"Database"` -> `Gitlab::
/// Database`). An empty enclosing prefix (top level) yields the bare name.
fn qualify_self(prefix: Option<&str>, name: &str) -> String {
    match prefix {
        Some(p) if !p.is_empty() => format!("{p}::{name}"),
        _ => name.to_string(),
    }
}

/// Widen (to `Dynamic`) every tracked local whose write span is contained in
/// `span` — the conservative invalidation a control-flow construct applies.
fn widen_flow_writes(
    writes: &[(rigor_parse::Span, String)],
    span: rigor_parse::Span,
    env: &mut TypeEnv,
    interner: &mut Interner,
) {
    let u = interner.untyped();
    for (wspan, name) in writes {
        if span.0 <= wspan.0 && wspan.1 <= span.1 {
            env.insert(name.clone(), u);
        }
    }
}

/// Drop the `Array.new`-provenance of every local whose write span is contained
/// in `span` — the `penv` counterpart of [`widen_flow_writes`] (a reassignment
/// inside `span` invalidates the "still bound to `Array.new(nominal)`" fact).
fn widen_penv_writes(
    writes: &[(rigor_parse::Span, String)],
    span: rigor_parse::Span,
    penv: &mut HashSet<String>,
) {
    for (wspan, name) in writes {
        if span.0 <= wspan.0 && wspan.1 <= span.1 {
            penv.remove(name);
        }
    }
}

/// Join two branch environments: a binding survives only when both sides map it
/// to the IDENTICAL `TypeId`; every disagreement, and every local bound in only
/// one branch, widens to `Dynamic`. This is the branch-merge that makes a
/// surviving `Type::Constant` sound to witness as always-truthy/falsey.
fn join_flow_envs(a: &TypeEnv, b: &TypeEnv, interner: &mut Interner) -> TypeEnv {
    let u = interner.untyped();
    let mut out = TypeEnv::with_capacity(a.len());
    for (k, av) in a {
        let v = match b.get(k) {
            Some(bv) if bv == av => *av,
            _ => u,
        };
        out.insert(k.clone(), v);
    }
    for k in b.keys() {
        if !a.contains_key(k) {
            out.insert(k.clone(), u);
        }
    }
    out
}

/// Unwrap a `Tuple`'s elements to their pinned scalars, or `None` if ANY element
/// is not value-pinned — membership in a set operation is undecidable the moment
/// one element is unknown.
///
/// A `NaN` element also declines. Everywhere else `Scalar`'s equality coincides
/// with Ruby's `eql?` (never across variants, floats by raw bits), which is what
/// `Array#&` / `#|` / `#-` use — but Ruby's `Float::NAN.eql?(Float::NAN)` is
/// FALSE while identical bits compare equal here, so a NaN would be the one
/// value this fold could get wrong.
fn tuple_constant_values(elems: &[TypeId], interner: &Interner) -> Option<Vec<Scalar>> {
    elems
        .iter()
        .map(|&id| match interner.get(id) {
            Type::Constant(Scalar::Float(f)) if f.is_nan() => None,
            Type::Constant(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// `a & b` — the elements of `a` that are also in `b`, de-duplicated, in `a`'s
/// order (Ruby `Array#&`).
fn set_intersection(a: &[Scalar], b: &[Scalar]) -> Vec<Scalar> {
    let mut out: Vec<Scalar> = Vec::new();
    for s in a {
        if b.contains(s) && !out.contains(s) {
            out.push(s.clone());
        }
    }
    out
}

/// `a | b` — `a` then `b`, de-duplicated, first occurrence wins (Ruby `Array#|`).
fn set_union(a: &[Scalar], b: &[Scalar]) -> Vec<Scalar> {
    let mut out: Vec<Scalar> = Vec::new();
    for s in a.iter().chain(b.iter()) {
        if !out.contains(s) {
            out.push(s.clone());
        }
    }
    out
}

/// `a - b` — the elements of `a` absent from `b`. NOT de-duplicated: Ruby's
/// `Array#-` removes every occurrence of a matching value but keeps the repeats
/// of the ones that survive (`[1, 1, 2] - [2] == [1, 1]`).
fn set_difference(a: &[Scalar], b: &[Scalar]) -> Vec<Scalar> {
    a.iter().filter(|s| !b.contains(s)).cloned().collect()
}

/// Type an owned-AST node against the current `env`. Free-function wrapper kept
/// source-compatible for callers (e.g. rigor-rules) that predate [`Typer`]; it
/// runs over an *empty* index, so a `Call` receiver types via folding only and
/// otherwise degrades to `Dynamic[top]`. Migrate to [`Typer::type_of`] (with the
/// real index) to get chained-call result typing.
///
/// - `StringLit` -> `Constant["..."]`
/// - `IntegerLit` -> `Constant[n]`
/// - `LocalVariableRead` -> the env binding, else `Dynamic[top]`
/// - anything else -> `Dynamic[top]` (`Interner::untyped`)
pub fn type_of(ast: &LoweredAst, id: NodeId, env: &TypeEnv, interner: &mut Interner) -> TypeId {
    let empty = CoreIndex::new();
    Typer::new(&empty).type_of(ast, id, env, interner)
}

/// Walk the top-level statement sequence binding each local write. Free-function
/// wrapper over an empty-index [`Typer`], kept source-compatible (see
/// [`type_of`]).
// TODO(spec): real flow-sensitive scoping + narrowing across branches (ADR-0022).
pub fn build_toplevel_env(ast: &LoweredAst, interner: &mut Interner) -> TypeEnv {
    let empty = CoreIndex::new();
    Typer::new(&empty).build_toplevel_env(ast, interner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rigor_parse::{lower, parse};

    fn lower_src(src: &[u8]) -> LoweredAst {
        lower(&parse(src))
    }

    #[test]
    fn types_string_and_integer_literals() {
        let ast = lower_src(b"\"Hello\"\n42\n");
        let mut i = Interner::new();
        let env = TypeEnv::new();
        // Locate the two literal nodes and type them.
        let str_id = ast
            .iter()
            .find_map(|(id, n)| matches!(n, Node::StringLit { .. }).then_some(id))
            .unwrap();
        let int_id = ast
            .iter()
            .find_map(|(id, n)| matches!(n, Node::IntegerLit { .. }).then_some(id))
            .unwrap();
        let str_ty = type_of(&ast, str_id, &env, &mut i);
        assert_eq!(i.get(str_ty), &Type::Constant(Scalar::Str("Hello".into())));
        let int_ty = type_of(&ast, int_id, &env, &mut i);
        assert_eq!(i.get(int_ty), &Type::Constant(Scalar::Int(42)));
    }

    /// Value-pinned Tuple projection folds: a no-arg accessor / constant index
    /// on an array literal folds to the pinned element or arity.
    #[test]
    fn tuple_projection_folds() {
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        let case = |src: &[u8], expect: Type| {
            let ast = lower_src(src);
            let mut i = Interner::new();
            let env = TypeEnv::new();
            let call_id = ast
                .iter()
                .find_map(|(id, n)| matches!(n, Node::Call { receiver: Some(_), .. }).then_some(id))
                .unwrap();
            let ty = typer.type_of(&ast, call_id, &env, &mut i);
            assert_eq!(i.get(ty), &expect, "src={}", String::from_utf8_lossy(src));
        };
        case(b"[1, 2, 3].first\n", Type::Constant(Scalar::Int(1)));
        case(b"[1, 2, 3].last\n", Type::Constant(Scalar::Int(3)));
        case(b"[1, 2, 3].size\n", Type::Constant(Scalar::Int(3)));
        case(b"[10, 20][1]\n", Type::Constant(Scalar::Int(20)));
        case(b"[10, 20][-1]\n", Type::Constant(Scalar::Int(20)));
        case(b"[1, 2].empty?\n", Type::Constant(Scalar::Bool(false)));
        case(b"[].first\n", Type::Constant(Scalar::Nil));
        case(b"[1, 2][9]\n", Type::Constant(Scalar::Nil)); // out of bounds → nil
    }

    /// Kernel `#p` / `#pp` identity typing on the implicit-self (`receiver:
    /// None`) path — the full p01–p11 probe matrix, both firing (a folded value
    /// carrier) and silent (`untyped`/Dynamic) directions. Types the LAST call
    /// in each snippet: for the firing probes that is the `p`/`pp` call whose
    /// value we assert; for the silent probes it is either the declined `p`/`pp`
    /// call or an explicit-receiver `Kernel.p` that never reaches our path.
    #[test]
    fn kernel_p_pp_identity_typing() {
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        // Type the p/pp call of interest. `nth_from_end` selects which
        // implicit-self (receiver-None) call to type, counting from the end
        // (0 = last) — needed for p07/p11 where a `def p` / method body adds
        // additional receiver-None calls we must skip past.
        let describe_ty = |src: &[u8], want_recv_none: bool| -> String {
            let ast = lower_src(src);
            let mut i = Interner::new();
            let env = TypeEnv::new();
            let call_id = ast
                .iter()
                .filter_map(|(id, n)| match n {
                    Node::Call { receiver, method, .. }
                        if receiver.is_none() == want_recv_none
                            && (method == "p" || method == "pp") =>
                    {
                        Some(id)
                    }
                    _ => None,
                })
                .last()
                .unwrap();
            let ty = typer.type_of(&ast, call_id, &env, &mut i);
            rigor_types::describe(&i, ty)
        };

        // p01: `p 42` → identity → Constant[42].
        assert_eq!(describe_ty(b"p 42\n", true), "Constant[42]");
        // p02: `p(1, "a")` → Tuple of the arg types.
        assert_eq!(describe_ty(b"p(1, \"a\")\n", true), "Tuple[Constant[1], Constant[\"a\"]]");
        // p03: bare `p` → nil (NOT declined — rigor-rs has no RBS tier on this
        // path, so the fold must carry the nil itself).
        assert_eq!(describe_ty(b"p\n", true), "nil");
        // p04: `pp 42` → identity → Constant[42].
        assert_eq!(describe_ty(b"pp 42\n", true), "Constant[42]");
        // p09: block form still folds (a block does not block the fold).
        assert_eq!(describe_ty(b"p(42) { 1 }\n", true), "Constant[42]");
        // p10: HashShape passes through the identity unchanged.
        assert_eq!(describe_ty(b"p({a: 1})\n", true), "{:a => Constant[1]}");

        // p05: `Kernel.p(42)` — the explicit `module_function` spelling folds to
        // the SAME identity as implicit-self (upstream c9d2e473), BUT only once
        // the receiver types to `Singleton[Kernel]`, which needs a populated
        // source index. This no-source harness types `Kernel` to Dynamic, so it
        // declines here; the fold is exercised in
        // `kernel_explicit_receiver_folds_like_implicit_self` (with a real source).
        assert_eq!(describe_ty(b"Kernel.p(42)\n", false), "Dynamic[top]");

        // Silent directions — decline to Dynamic[top].
        // p07: a file-wide `def p` disables the fold file-wide.
        assert_eq!(describe_ty(b"def p(*a); nil; end\np 42\n", true), "Dynamic[top]");
        // p08: a splat arg makes arity unknown → decline.
        assert_eq!(describe_ty(b"a = [1, 2]\np(*a)\n", true), "Dynamic[top]");
        // p11: a Dynamic (unknown local) arg passes through identity as Dynamic.
        assert_eq!(describe_ty(b"p some_unknown_local\n", true), "Dynamic[top]");
    }

    /// The explicit `Kernel.` module_function spelling folds like implicit-self
    /// across the whole intrinsic family (upstream c9d2e473): `Kernel.p`,
    /// `Kernel.format`/`sprintf`, `Kernel.String`/`Integer`/`Float`. A non-fold
    /// Kernel method stays Dynamic (falls through to the RBS surface).
    #[test]
    fn kernel_explicit_receiver_folds_like_implicit_self() {
        let index = CoreIndex::new();
        // A populated source index so the bare `Kernel` constant read types to
        // `Singleton[Kernel]` (the ConstantRead zero-FP gate resolves it via the
        // source registry) — the receiver shape the explicit-spelling fold keys on.
        let last_call_ty = |src: &[u8]| -> String {
            let ast = lower_src(src);
            let source = SourceIndex::build(&ast, &index);
            let typer = Typer::with_source(&index, &source);
            let mut i = Interner::new();
            let env = TypeEnv::new();
            let call_id = ast
                .iter()
                .filter_map(|(id, n)| matches!(n, Node::Call { receiver: Some(_), .. }).then_some(id))
                .last()
                .unwrap();
            let ty = typer.type_of(&ast, call_id, &env, &mut i);
            rigor_types::describe(&i, ty)
        };
        // Identity printer via the module object.
        assert_eq!(last_call_ty(b"Kernel.p(42)\n"), "Constant[42]");
        assert_eq!(last_call_ty(b"Kernel.pp(1, 2)\n"), "Tuple[Constant[1], Constant[2]]");
        // Conversion + format folds, same envelope as implicit self.
        assert_eq!(last_call_ty(b"Kernel.format(\"%d\", 1)\n"), "Constant[\"1\"]");
        assert_eq!(last_call_ty(b"Kernel.String(42)\n"), "Constant[\"42\"]");
        // A non-fold Kernel method is not a fold target → Dynamic (RBS answers).
        assert_eq!(last_call_ty(b"Kernel.puts(\"x\")\n"), "Dynamic[top]");
    }

    /// An `if`/`unless`/ternary as an expression types to the union of its
    /// branch values, with a known-polarity predicate eliding the dead branch.
    #[test]
    fn if_expression_unions_and_elides_branches() {
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        let describe = |src: &[u8]| -> String {
            let ast = lower_src(src);
            let mut i = Interner::new();
            let env = TypeEnv::new();
            let if_id = ast
                .iter()
                .find_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
                .unwrap();
            let ty = typer.type_of(&ast, if_id, &env, &mut i);
            rigor_types::describe(&i, ty)
        };
        // The internal `describe` spells constants `Constant[n]`; the point here
        // is the union/elision structure, not the user-facing rendering.
        // Unknown predicate → union of both branches (a missing else ⇒ nil).
        assert_eq!(describe(b"if c then 1 else 2 end\n"), "Constant[1] | Constant[2]");
        assert_eq!(describe(b"if c then 1 end\n"), "Constant[1] | nil");
        // Truthy constant predicate → then branch only (elided).
        assert_eq!(describe(b"if true then 1 else 2 end\n"), "Constant[1]");
        // Falsey predicate → else branch only.
        assert_eq!(describe(b"if nil then 1 else 2 end\n"), "Constant[2]");
    }

    /// A `case`/`when` expression types to the union of its branch values + the
    /// `else` value (nil when no `else`).
    #[test]
    fn case_expression_unions_branch_values() {
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        let describe = |src: &[u8]| -> String {
            let ast = lower_src(src);
            let mut i = Interner::new();
            let env = TypeEnv::new();
            let case_id = ast
                .iter()
                .find_map(|(id, n)| matches!(n, Node::Case { .. }).then_some(id))
                .unwrap();
            let ty = typer.type_of(&ast, case_id, &env, &mut i);
            rigor_types::describe(&i, ty)
        };
        assert_eq!(
            describe(b"case x\nwhen 1 then 10\nwhen 2 then 20\nelse 30\nend\n"),
            "Constant[10] | Constant[20] | Constant[30]"
        );
        // No else → nil joins the union (a non-exhaustive case returns nil).
        assert_eq!(
            describe(b"case x\nwhen 1 then 10\nend\n"),
            "Constant[10] | nil"
        );
    }

    /// The flow-constant substrate (ADR-0022) records a straight-line dominating
    /// constant for an `if` predicate.
    #[test]
    fn flow_snapshot_folds_straight_line_constant() {
        let ast = lower_src(b"x = 5\nif x\n  noop\nend\n");
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        let mut i = Interner::new();
        let snaps = typer.always_truthy_snapshots(&ast, &mut i);
        let if_id = ast
            .iter()
            .find_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
            .unwrap();
        let ty = snaps.get(&if_id).copied().expect("predicate snapshot recorded");
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(5)));
    }

    /// The branch-join keystone: a conditionally reassigned local is widened, so
    /// a later predicate reading it is NOT a constant (the zero-FP guarantee the
    /// flat env cannot provide).
    #[test]
    fn flow_snapshot_widens_conditional_reassignment() {
        let ast = lower_src(b"x = 5\nif g\n  x = f\nend\nif x\n  noop\nend\n");
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        let mut i = Interner::new();
        let snaps = typer.always_truthy_snapshots(&ast, &mut i);
        let ifs: Vec<_> = ast
            .iter()
            .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
            .collect();
        assert_eq!(ifs.len(), 2, "expected two if nodes");
        let ty2 = snaps.get(&ifs[1]).copied().expect("second if recorded");
        assert!(
            !matches!(i.get(ty2), Type::Constant(_)),
            "x must be widened to non-constant after a conditional reassignment"
        );
    }

    /// MutationWidening (parser.rb FP): a value-pinned collection local that is
    /// content-mutated by an in-place mutator call must widen, so a later
    /// `local.count`/`.size` predicate is NOT a folded constant. `true` means the
    /// predicate folds to a `Type::Constant` (the always-truthy rule WOULD fire);
    /// `false` means it was widened (declined). The predicate reads the LAST `if`.
    fn last_if_predicate_is_constant(src: &[u8]) -> bool {
        let ast = lower_src(src);
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        let mut i = Interner::new();
        let snaps = typer.always_truthy_snapshots(&ast, &mut i);
        let last_if = ast
            .iter()
            .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
            .last()
            .expect("at least one if node");
        let ty = snaps
            .get(&last_if)
            .copied()
            .expect("predicate snapshot recorded for a top-level if");
        matches!(i.get(ty), Type::Constant(_))
    }

    /// P2 rail: NO mutation ⇒ the `[]`-pinned `results.count > 1` still folds and
    /// the always-truthy rule must KEEP firing. This is the load-bearing negative
    /// control — the fix must not widen an unmutated local.
    #[test]
    fn mutation_widening_p2_no_mutation_keeps_firing() {
        assert!(last_if_predicate_is_constant(
            b"results = []\nif results.count > 1\n  noop\nend\n"
        ));
        // Both count directions fold (parser.rb fires on `> 1` and `< 1`).
        assert!(last_if_predicate_is_constant(
            b"results = []\nif results.count < 1\n  noop\nend\n"
        ));
    }

    /// A NON-mutator call on the local (`map`, a pure sibling) must NOT widen — a
    /// guard that the extension keys on the mutator set, not on any call.
    #[test]
    fn mutation_widening_non_mutator_call_keeps_firing() {
        assert!(last_if_predicate_is_constant(
            b"results = []\nresults.map { |x| x }\nif results.count > 1\n  noop\nend\n"
        ));
    }

    /// P3: a straight-line `results.push(1)` (no block) widens the local — its own
    /// call span is the containing span, resolved through the catch-all arm.
    #[test]
    fn mutation_widening_p3_straight_line_push_stops_firing() {
        assert!(!last_if_predicate_is_constant(
            b"results = []\nresults.push(1)\nif results.count > 1\n  noop\nend\n"
        ));
    }

    /// P4: a `push` under an `if` modifier widens (the then-branch mutation
    /// disagrees with the untaken else at the join).
    #[test]
    fn mutation_widening_p4_push_under_if_modifier_stops_firing() {
        assert!(!last_if_predicate_is_constant(
            b"results = []\nresults.push(1) if cond\nif results.count > 1\n  noop\nend\n"
        ));
    }

    /// P1: the parser.rb shape — `push`/`pop` inside a nested `case` in an `each`
    /// block. `ast.iter()` finds the mutation spans; the enclosing `each` call span
    /// contains them, so the catch-all arm widens `results`.
    #[test]
    fn mutation_widening_p1_block_nested_case_stops_firing() {
        let src = b"results = []\nxs.each do |t|\n  case t\n  when 1\n    results.push(t)\n  when 2\n    results.pop\n  end\nend\nif results.count > 1\n  noop\nend\n";
        assert!(!last_if_predicate_is_constant(src));
        // Same shape, `< 1` direction.
        let src_lt = b"results = []\nxs.each do |t|\n  case t\n  when 1\n    results.push(t)\n  end\nend\nif results.count < 1\n  noop\nend\n";
        assert!(!last_if_predicate_is_constant(src_lt));
    }

    /// P5: a rebind (`results = results + [x]`) inside the block widens through the
    /// pre-existing `LocalVariableWrite` arm — correct on both sides already, and
    /// still correct after the mutator extension.
    #[test]
    fn mutation_widening_p5_rebind_in_block_stops_firing() {
        assert!(!last_if_predicate_is_constant(
            b"results = []\nxs.each do |t|\n  results = results + [t]\nend\nif results.count > 1\n  noop\nend\n"
        ));
    }

    /// P7: `results << t` inside a block — `<<` is a mutator, widened via the
    /// block-containing span.
    #[test]
    fn mutation_widening_p7_shovel_in_block_stops_firing() {
        assert!(!last_if_predicate_is_constant(
            b"results = []\nxs.each do |t|\n  results << t\nend\nif results.count > 1\n  noop\nend\n"
        ));
    }

    /// ADR-0038 Slice 1: a nilable String slice bound in a NESTED block, with its
    /// receiver typed by a `String.new` in an OUTER block, fires possible-nil on
    /// the same-block use. The block-scope shape the substrate unlocks.
    #[test]
    fn nil_snapshot_fires_on_block_scope_string_slice() {
        let ast = lower_src(
            b"outer do\n  s = String.new(\"hello\")\n  inner do\n    sub = s[0..2]\n    n = sub.size\n  end\nend\n",
        );
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        let mut i = Interner::new();
        let snaps = typer.nilable_receiver_snapshots(&ast, &mut i);
        // The `sub.size` call is the nilable-receiver use; its arm is String.
        let use_id = ast
            .iter()
            .find_map(|(id, n)| match n {
                Node::Call { receiver: Some(r), method, .. }
                    if method == "size"
                        && matches!(ast.get(*r), Node::LocalVariableRead { name, .. } if name == "sub") =>
                {
                    Some(id)
                }
                _ => None,
            })
            .expect("sub.size call present");
        assert_eq!(snaps.get(&use_id).copied(), Some("String"));
    }

    /// ADR-0039 §2: an `Array.new(n > 16)` slice IS a source (the reference keeps
    /// it `Nominal[Array]`, so `arr[Range] : Array?` fires). Provenance-gated.
    #[test]
    fn nil_snapshot_array_new_large_slice_fires() {
        let ast = lower_src(b"arr = Array.new(300000) { |i| i }\nsub = arr[0..5]\nn = sub.size\n");
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        let mut i = Interner::new();
        let snaps = typer.nilable_receiver_snapshots(&ast, &mut i);
        let use_id = ast
            .iter()
            .find_map(|(id, n)| match n {
                Node::Call { receiver: Some(r), method, .. }
                    if method == "size"
                        && matches!(ast.get(*r), Node::LocalVariableRead { name, .. } if name == "sub") =>
                {
                    Some(id)
                }
                _ => None,
            })
            .expect("sub.size call present");
        assert_eq!(snaps.get(&use_id).copied(), Some("Array"));
    }

    /// The reference `Tuple`s a small `Array.new(n ≤ 16)` and every array literal
    /// (their slice is non-nil), so those slices must NOT fire — else an FP. The
    /// provenance gate (small const / literal ⇒ no provenance) keeps them silent.
    #[test]
    fn nil_snapshot_small_array_new_and_literal_slices_decline() {
        for src in [
            b"arr = Array.new(10) { |i| i }\nsub = arr[0..5]\nn = sub.size\n".as_slice(),
            b"arr = [1, 2, 3]\nsub = arr[0..1]\nn = sub.size\n".as_slice(),
            b"arr = [1, 2, 3].map { |x| x }\nsub = arr[0..1]\nn = sub.size\n".as_slice(),
        ] {
            let ast = lower_src(src);
            let index = CoreIndex::new();
            let typer = Typer::new(&index);
            let mut i = Interner::new();
            let snaps = typer.nilable_receiver_snapshots(&ast, &mut i);
            assert!(
                snaps.is_empty(),
                "small/literal/.map array slice must not mint a nilable fact: {:?}",
                std::str::from_utf8(src).unwrap()
            );
        }
    }

    /// The decline backstop: a guard (`if`) between the slice source and the use
    /// clears the fact, so no snapshot is recorded (zero-FP over recall).
    #[test]
    fn nil_snapshot_declines_on_guard_between_source_and_use() {
        let ast = lower_src(
            b"s = String.new(\"abc\")\nsub = s[0..1]\nif sub\n  noop\nend\nn = sub.size\n",
        );
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        let mut i = Interner::new();
        let snaps = typer.nilable_receiver_snapshots(&ast, &mut i);
        let use_id = ast
            .iter()
            .find_map(|(id, n)| match n {
                Node::Call { receiver: Some(r), method, .. }
                    if method == "size"
                        && matches!(ast.get(*r), Node::LocalVariableRead { name, .. } if name == "sub") =>
                {
                    Some(id)
                }
                _ => None,
            })
            .expect("sub.size call present");
        assert_eq!(snaps.get(&use_id), None, "an intervening guard must decline");
    }

    // -----------------------------------------------------------------------
    // P2 (2026-07-17) — `Regexp.last_match` optional-local nil source
    // -----------------------------------------------------------------------

    /// Snapshot arm recorded for the FIRST call whose receiver is a bare local
    /// read of `recv` and method is `method`, or `None`.
    fn last_match_use_arm(src: &[u8], recv: &str, method: &str) -> Option<&'static str> {
        let ast = lower_src(src);
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        let mut i = Interner::new();
        let snaps = typer.nilable_receiver_snapshots(&ast, &mut i);
        let use_id = ast.iter().find_map(|(id, n)| match n {
            Node::Call { receiver: Some(r), method: m, .. }
                if m == method
                    && matches!(ast.get(*r), Node::LocalVariableRead { name, .. } if name == recv) =>
            {
                Some(id)
            }
            _ => None,
        })?;
        snaps.get(&use_id).copied()
    }

    /// `Regexp.last_match(n) -> String?`: the integer-literal arg gives a
    /// concrete `String` arm, so a straight-line `content.gsub(...)` fires (the
    /// `dictionary_credentials_handler` / `hugo_transformer` gitlab cluster).
    /// Both `::Regexp` and `Regexp` lower to `ConstantRead "Regexp"`.
    #[test]
    fn p2_regexp_last_match_int_arg_is_string_source() {
        for src in [
            b"content = ::Regexp.last_match(2)\nnew = content.gsub(\"a\", \"b\")\n".as_slice(),
            b"content = Regexp.last_match(1)\nnew = content.gsub(\"a\", \"b\")\n".as_slice(),
        ] {
            assert_eq!(
                last_match_use_arm(src, "content", "gsub"),
                Some("String"),
                "Regexp.last_match(int) must mint a String|nil source: {:?}",
                std::str::from_utf8(src).unwrap()
            );
        }
    }

    /// `Regexp.last_match(name) -> String?` for a String / Symbol literal arg.
    #[test]
    fn p2_regexp_last_match_name_arg_is_string_source() {
        for src in [
            b"c = Regexp.last_match(:key)\nn = c.upcase\n".as_slice(),
            b"c = Regexp.last_match(\"key\")\nn = c.upcase\n".as_slice(),
        ] {
            assert_eq!(last_match_use_arm(src, "c", "upcase"), Some("String"));
        }
    }

    /// `Regexp.last_match() -> MatchData?`: the zero-arg form mints a `MatchData`
    /// arm, so `match[0]` / `match.begin(0)` fire (the `collection` / second
    /// `hugo_transformer` gitlab cluster).
    #[test]
    fn p2_regexp_last_match_zero_arg_is_matchdata_source() {
        let src = b"m = Regexp.last_match\nfull = m[0]\nb = m.begin(0)\n";
        assert_eq!(last_match_use_arm(src, "m", "[]"), Some("MatchData"));
        assert_eq!(last_match_use_arm(src, "m", "begin"), Some("MatchData"));
    }

    /// A NON-literal 1-arg call fires too (compat plan S2): every 1-arity
    /// overload returns `String?`, so the reference resolves BY ARITY — the arg's
    /// shape does not matter (fixture 65 `non_literal_arg`).
    #[test]
    fn p2_regexp_last_match_non_literal_arg_is_string_source() {
        assert_eq!(
            last_match_use_arm(b"i = 2\nc = Regexp.last_match(i)\nn = c.gsub(\"a\", \"b\")\n", "c", "gsub"),
            Some("String")
        );
    }

    /// Decline conditions (FP backstop): a splat / multi arg to `last_match`
    /// (arity unknown / raises), a NON-`Regexp` constant receiver, a guard
    /// between the bind and the use, and a safe-nav deref all record no snapshot.
    #[test]
    fn p2_regexp_last_match_declines() {
        // splat arg — arity statically unknown (could be the 0-arg MatchData form)
        assert_eq!(
            last_match_use_arm(b"a = [1]\nc = Regexp.last_match(*a)\nn = c.gsub(\"a\", \"b\")\n", "c", "gsub"),
            None
        );
        // multi arg — no such overload (raises at runtime)
        assert_eq!(
            last_match_use_arm(b"c = Regexp.last_match(1, 2)\nn = c.gsub(\"a\", \"b\")\n", "c", "gsub"),
            None
        );
        // a different constant named `.last_match` is not the core Regexp source
        assert_eq!(
            last_match_use_arm(b"c = Foo.last_match(2)\nn = c.gsub(\"a\", \"b\")\n", "c", "gsub"),
            None
        );
        // intervening guard clears the fact
        assert_eq!(
            last_match_use_arm(b"c = Regexp.last_match(2)\nif c\n  noop\nend\nn = c.gsub(\"a\", \"b\")\n", "c", "gsub"),
            None
        );
        // safe-nav deref is not a bug (short-circuits on nil)
        assert_eq!(
            last_match_use_arm(b"c = Regexp.last_match(2)\nn = c&.gsub(\"a\", \"b\")\n", "c", "gsub"),
            None
        );
    }

    /// A same-named block parameter must NOT inherit an outer nilable fact — the
    /// fresh-per-block `nenv` makes the shadowing FP class structurally impossible.
    #[test]
    fn nil_snapshot_block_param_shadow_does_not_leak() {
        let ast = lower_src(b"sub = String.new(\"x\")[0..2]\n[1, 2].each do |sub|\n  n = sub.size\nend\n");
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        let mut i = Interner::new();
        let snaps = typer.nilable_receiver_snapshots(&ast, &mut i);
        // Even though `sub` is nilable outside, the block's `|sub|` is a different
        // variable; the fresh block `nenv` means no snapshot leaks in.
        assert!(
            snaps.is_empty(),
            "an outer fact must not leak past a same-named block param"
        );
    }

    #[test]
    fn local_read_resolves_from_env() {
        let ast = lower_src(b"s = \"Hello\"\ns.length\n");
        let mut i = Interner::new();
        let env = build_toplevel_env(&ast, &mut i);
        assert_eq!(
            env.get("s").copied().map(|t| i.get(t).clone()),
            Some(Type::Constant(Scalar::Str("Hello".into())))
        );
    }

    #[test]
    fn unknown_receiver_is_dynamic_top() {
        // In Ruby, a bare `x` with no prior assignment parses as the
        // implicit-self call `x()`, so the receiver of `.foo` is a `Call`, not
        // a local read. Either way, an unknown carrier types as Dynamic[top],
        // which is what keeps the call rule silent (ADR-0023 tier-5).
        let ast = lower_src(b"x.foo\n");
        let mut i = Interner::new();
        let env = build_toplevel_env(&ast, &mut i);
        // The receiver node of the outer `.foo` call.
        let recv_id = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::Call { receiver: Some(r), method, .. } if method == "foo" => Some(*r),
                _ => None,
            })
            .unwrap();
        let ty = type_of(&ast, recv_id, &env, &mut i);
        assert_eq!(ty, i.untyped());
    }

    /// Find the `Call` node whose method matches `name`, returning its id.
    fn find_call(ast: &LoweredAst, name: &str) -> NodeId {
        ast.iter()
            .find_map(|(id, n)| match n {
                Node::Call { method, .. } if method == name => Some(id),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected a call to `{name}`"))
    }

    #[test]
    fn folds_integer_addition_to_constant() {
        // `1 + 2` lowers to a Call `+` on receiver `1` with positional arg `2`;
        // now that args are lowered, binary folding runs and pins Constant[3].
        let ast = lower_src(b"1 + 2\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "+");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(3)));
    }

    #[test]
    fn folds_nullary_integer_succ_to_constant() {
        // Nullary folding still works with the new arg threading.
        let ast = lower_src(b"42.succ\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "succ");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(43)));
    }

    #[test]
    fn typed_literals_pin_constants() {
        let ast = lower_src(b"nil\ntrue\nfalse\n:foo\n2.5\n");
        let mut i = Interner::new();
        let env = TypeEnv::new();
        let pick = |ast: &LoweredAst, pred: fn(&Node) -> bool| {
            ast.iter().find_map(|(id, n)| pred(n).then_some(id)).unwrap()
        };
        let nil = pick(&ast, |n| matches!(n, Node::NilLit { .. }));
        let t = pick(&ast, |n| matches!(n, Node::TrueLit { .. }));
        let f = pick(&ast, |n| matches!(n, Node::FalseLit { .. }));
        let sym = pick(&ast, |n| matches!(n, Node::SymbolLit { .. }));
        let fl = pick(&ast, |n| matches!(n, Node::FloatLit { .. }));
        let ty_of = |i: &mut Interner, id| {
            let t = type_of(&ast, id, &env, i);
            i.get(t).clone()
        };
        assert_eq!(ty_of(&mut i, nil), Type::Constant(Scalar::Nil));
        assert_eq!(ty_of(&mut i, t), Type::Constant(Scalar::Bool(true)));
        assert_eq!(ty_of(&mut i, f), Type::Constant(Scalar::Bool(false)));
        assert_eq!(ty_of(&mut i, sym), Type::Constant(Scalar::Sym("foo".into())));
        assert_eq!(ty_of(&mut i, fl), Type::Constant(Scalar::Float(2.5)));
    }

    #[test]
    fn non_pinned_argument_declines_folding() {
        // `x` is never assigned -> Dynamic, so `"a" + x` can't fold; the call
        // widens to the nominal String return rather than minting a Constant.
        let ast = lower_src(b"\"a\" + x\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "+");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        // String#+ -> String nominal (return-type path), NOT a folded Constant.
        assert_eq!(idx.class_name_of(&i, ty), Some("String"));
        assert!(!matches!(i.get(ty), Type::Constant(_)));
    }

    #[test]
    fn folds_string_upcase_to_constant() {
        // `"hi".upcase` -> Constant["HI"].
        let ast = lower_src(b"\"hi\".upcase\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "upcase");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Str("HI".into())));
    }

    #[test]
    fn folds_string_length_to_constant() {
        // `"hello".length` -> Constant[5] (value-pinned; the core folds it).
        let ast = lower_src(b"\"hello\".length\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "length");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(5)));
    }

    #[test]
    fn chained_call_result_types_to_string_nominal() {
        // `s = "Hello"; s.downcase` types to a String Nominal (folding pins the
        // value, but to exercise the return-type path we check the class
        // resolves to "String" via the index regardless). Then `.lenght` on a
        // String would be undefined.
        let ast = lower_src(b"s = \"Hello\"\ns.downcase\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "downcase");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        // Folding pins "hello"; its class still resolves to String, so a later
        // `.lenght` on the result is checkable as undefined.
        assert_eq!(idx.class_name_of(&i, ty), Some("String"));
        assert!(!idx.class_has_method("String", "lenght"));
    }

    #[test]
    fn return_type_resolves_when_receiver_not_folded() {
        // A receiver typed as a (non-constant) String Nominal exercises the
        // return-type table path: `String#downcase -> String`, and that result
        // resolves back to "String" so a chained typo is flagged.
        //
        // `s` must lower to a `LocalVariableRead` (which it does once assigned),
        // so we assign then override the env binding to a bare String Nominal
        // (no value pin) — defeating folding and forcing the return-type path.
        let ast = lower_src(b"s = \"Hello\"\ns.downcase\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let mut env = typer.build_toplevel_env(&ast, &mut i);
        let string_id = idx.class_id("String").unwrap();
        let recv = i.intern(Type::Nominal { class: string_id, args: vec![] });
        env.insert("s".into(), recv);

        let call = find_call(&ast, "downcase");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        // Not folded (receiver isn't a Constant), so we get the Nominal return.
        assert_eq!(i.get(ty), &Type::Nominal { class: string_id, args: vec![] });
        assert_eq!(idx.class_name_of(&i, ty), Some("String"));
    }

    #[test]
    fn array_literal_types_to_array_nominal() {
        // `[1, 2]` types to a bare Array Nominal so a typo (`.frist`) is
        // checkable against the real Array RBS.
        let ast = lower_src(b"[1, 2]\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let arr = ast
            .iter()
            .find_map(|(id, n)| matches!(n, Node::ArrayLit { .. }).then_some(id))
            .unwrap();
        let ty = typer.type_of(&ast, arr, &env, &mut i);
        assert_eq!(idx.class_name_of(&i, ty), Some("Array"));
        assert!(!idx.class_has_method("Array", "frist"));
    }

    #[test]
    fn interpolated_string_types_to_string_nominal() {
        // `"a#{x}b"` types to a bare String Nominal (a String *instance*), so a
        // typo'd / non-core method on it resolves against the real String RBS.
        let ast = lower_src(b"\"a#{x}b\"\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let interp = ast
            .iter()
            .find_map(|(id, n)| matches!(n, Node::InterpolatedString { .. }).then_some(id))
            .unwrap();
        let ty = typer.type_of(&ast, interp, &env, &mut i);
        assert_eq!(idx.class_name_of(&i, ty), Some("String"));
    }

    #[test]
    fn hash_literal_types_to_hash_nominal() {
        let ast = lower_src(b"{ a: 1 }\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let hash = ast
            .iter()
            .find_map(|(id, n)| matches!(n, Node::HashLit { .. }).then_some(id))
            .unwrap();
        let ty = typer.type_of(&ast, hash, &env, &mut i);
        assert_eq!(idx.class_name_of(&i, ty), Some("Hash"));
    }

    // ---------------------------------------------------------------------
    // Scalar-key HashShape (ADR-0038 slice 2). Widened key set, last-wins
    // duplicate keys, and the HashShape projection tier.
    // ---------------------------------------------------------------------

    fn find_hash(ast: &LoweredAst) -> NodeId {
        ast.iter()
            .find_map(|(id, n)| matches!(n, Node::HashLit { .. }).then_some(id))
            .expect("expected a hash literal")
    }

    fn hash_members(ty: &Type) -> &[ShapeMember] {
        match ty {
            Type::HashShape(m) => m,
            other => panic!("expected HashShape, got {other:?}"),
        }
    }

    #[test]
    fn hash_shape_pins_widened_scalar_keys() {
        // Integer / Float / true / false / nil keys now pin shape slots (the
        // reference's widened ALLOWED_KEY_CLASSES), alongside Symbol / String.
        let ast = lower_src(b"{ 1 => 2, 1.5 => 3, true => 4, false => 5, nil => 6, :s => 7, \"k\" => 8 }\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let hash = find_hash(&ast);
        let ty = typer.type_of(&ast, hash, &env, &mut i);
        let keys: Vec<ShapeKey> = hash_members(i.get(ty)).iter().map(|m| m.key.clone()).collect();
        assert_eq!(
            keys,
            vec![
                ShapeKey::Int(1),
                ShapeKey::Float(1.5f64.to_bits()),
                ShapeKey::Bool(true),
                ShapeKey::Bool(false),
                ShapeKey::Nil,
                ShapeKey::Sym("s".into()),
                ShapeKey::Str("k".into()),
            ]
        );
    }

    #[test]
    fn hash_last_wins_keeps_first_position_last_value() {
        // `{ a: 1, b: 2, a: 3 }` — `a` keeps its FIRST position but takes the
        // LAST value (runtime last-wins), so members are [a=3, b=2].
        let ast = lower_src(b"{ a: 1, b: 2, a: 3 }\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let ty = typer.type_of(&ast, find_hash(&ast), &env, &mut i);
        let m = hash_members(i.get(ty));
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].key, ShapeKey::Sym("a".into()));
        assert_eq!(i.get(m[0].value), &Type::Constant(Scalar::Int(3)));
        assert_eq!(m[1].key, ShapeKey::Sym("b".into()));
        assert_eq!(i.get(m[1].value), &Type::Constant(Scalar::Int(2)));
    }

    #[test]
    fn hash_dup_integer_key_last_wins() {
        let ast = lower_src(b"{ 1 => 1, 1 => 9 }\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let ty = typer.type_of(&ast, find_hash(&ast), &env, &mut i);
        let m = hash_members(i.get(ty));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].key, ShapeKey::Int(1));
        assert_eq!(i.get(m[0].value), &Type::Constant(Scalar::Int(9)));
    }

    #[test]
    fn hash_float_keys_collide_by_value() {
        // `1.0` and `1.00` are the same f64 → one key, last value wins.
        let ast = lower_src(b"{ 1.0 => :a, 1.00 => :b }\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let ty = typer.type_of(&ast, find_hash(&ast), &env, &mut i);
        let m = hash_members(i.get(ty));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].key, ShapeKey::Float(1.0f64.to_bits()));
        assert_eq!(i.get(m[0].value), &Type::Constant(Scalar::Sym("b".into())));
    }

    #[test]
    fn hash_int_and_float_keys_are_distinct() {
        // `1` (Int) and `1.0` (Float) are DISTINCT keys (`1.eql?(1.0)` is false).
        let ast = lower_src(b"{ 1 => :i, 1.0 => :f }\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let ty = typer.type_of(&ast, find_hash(&ast), &env, &mut i);
        let m = hash_members(i.get(ty));
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].key, ShapeKey::Int(1));
        assert_eq!(m[1].key, ShapeKey::Float(1.0f64.to_bits()));
    }

    #[test]
    fn hash_dynamic_key_degrades_to_hash_nominal() {
        // A non-literal key (a method call) can't pin a slot → bare `Hash`.
        let ast = lower_src(b"{ foo => 1 }\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let ty = typer.type_of(&ast, find_hash(&ast), &env, &mut i);
        assert_eq!(idx.class_name_of(&i, ty), Some("Hash"));
    }

    /// Type the outermost call in `src` (a `v = <hash>.<call>` line).
    fn type_of_projection(src: &[u8], method: &str) -> (Interner, TypeId) {
        let ast = lower_src(src);
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, method);
        let ty = typer.type_of(&ast, call, &env, &mut i);
        (i, ty)
    }

    #[test]
    fn hash_index_folds_present_and_missing_keys() {
        // h07: `{ a: 1, b: "s" }[:b]` → `"s"`; a missing key → `nil`.
        let (i, ty) = type_of_projection(b"v = { a: 1, b: \"s\" }[:b]\n", "[]");
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Str("s".into())));
        let (i, ty) = type_of_projection(b"v = { a: 1 }[:z]\n", "[]");
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Nil));
    }

    #[test]
    fn hash_index_on_integer_key_folds() {
        let (i, ty) = type_of_projection(b"v = { 1 => \"x\" }[1]\n", "[]");
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Str("x".into())));
    }

    #[test]
    fn hash_fetch_present_folds_missing_declines() {
        // h08: `.fetch(:a)` folds to the value; a miss DECLINES (KeyError) →
        // the RBS Hash tier answers (not a folded Constant).
        let (i, ty) = type_of_projection(b"v = { a: 1 }.fetch(:a)\n", "fetch");
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(1)));
        let (i, ty) = type_of_projection(b"v = { a: 1 }.fetch(:z)\n", "fetch");
        assert!(!matches!(i.get(ty), Type::Constant(_)), "fetch miss must not fold to a Constant");
    }

    #[test]
    fn hash_has_key_folds_to_bool() {
        // h09: `.has_key?` / aliases fold to a precise bool.
        for (src, expect) in [
            (b"v = { a: 1 }.has_key?(:a)\n".as_slice(), true),
            (b"v = { a: 1 }.has_key?(:z)\n".as_slice(), false),
        ] {
            let (i, ty) = type_of_projection(src, "has_key?");
            assert_eq!(i.get(ty), &Type::Constant(Scalar::Bool(expect)));
        }
        let (i, ty) = type_of_projection(b"v = { a: 1 }.key?(:a)\n", "key?");
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Bool(true)));
        let (i, ty) = type_of_projection(b"v = { a: 1 }.include?(:z)\n", "include?");
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Bool(false)));
    }

    #[test]
    fn hash_values_at_folds_to_tuple_in_arg_order() {
        // `{ a: 1, b: 2 }.values_at(:b, :z, :a)` → Tuple[2, nil, 1].
        let (i, ty) = type_of_projection(b"v = { a: 1, b: 2 }.values_at(:b, :z, :a)\n", "values_at");
        let Type::Tuple(elems) = i.get(ty) else { panic!("expected Tuple, got {:?}", i.get(ty)) };
        let got: Vec<Type> = elems.iter().map(|&e| i.get(e).clone()).collect();
        assert_eq!(
            got,
            vec![
                Type::Constant(Scalar::Int(2)),
                Type::Constant(Scalar::Nil),
                Type::Constant(Scalar::Int(1)),
            ]
        );
    }

    #[test]
    fn hash_slice_keeps_present_keys_in_arg_order() {
        // `{ a: 1, b: 2, c: 3 }.slice(:c, :a)` → { c: 3, a: 1 } (arg order).
        let (i, ty) = type_of_projection(b"v = { a: 1, b: 2, c: 3 }.slice(:c, :a)\n", "slice");
        let m = hash_members(i.get(ty));
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].key, ShapeKey::Sym("c".into()));
        assert_eq!(m[1].key, ShapeKey::Sym("a".into()));
    }

    #[test]
    fn hash_except_drops_keys_in_receiver_order() {
        let (i, ty) = type_of_projection(b"v = { a: 1, b: 2, c: 3 }.except(:b)\n", "except");
        let keys: Vec<ShapeKey> = hash_members(i.get(ty)).iter().map(|m| m.key.clone()).collect();
        assert_eq!(keys, vec![ShapeKey::Sym("a".into()), ShapeKey::Sym("c".into())]);
    }

    #[test]
    fn hash_invert_swaps_keys_and_values() {
        // `{ a: 1, b: 2 }.invert` → { 1 => :a, 2 => :b }.
        let (i, ty) = type_of_projection(b"v = { a: 1, b: 2 }.invert\n", "invert");
        let m = hash_members(i.get(ty));
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].key, ShapeKey::Int(1));
        assert_eq!(i.get(m[0].value), &Type::Constant(Scalar::Sym("a".into())));
        assert_eq!(m[1].key, ShapeKey::Int(2));
        assert_eq!(i.get(m[1].value), &Type::Constant(Scalar::Sym("b".into())));
    }

    #[test]
    fn hash_invert_declines_on_value_collision() {
        // A duplicate VALUE would alias under inversion → decline (falls to RBS,
        // not a folded HashShape).
        let (i, ty) = type_of_projection(b"v = { a: 1, b: 1 }.invert\n", "invert");
        assert!(!matches!(i.get(ty), Type::HashShape(_)), "collision must not fold to a HashShape");
    }

    #[test]
    fn hash_dig_folds_single_and_nested_chains() {
        let (i, ty) = type_of_projection(b"v = { a: 1 }.dig(:a)\n", "dig");
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(1)));
        let (i, ty) = type_of_projection(b"v = { a: { b: 5 } }.dig(:a, :b)\n", "dig");
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(5)));
        // A missing key mid-chain short-circuits to nil.
        let (i, ty) = type_of_projection(b"v = { a: { b: 5 } }.dig(:a, :z)\n", "dig");
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Nil));
    }

    #[test]
    fn hash_projection_declines_on_dynamic_key() {
        // A non-literal key argument declines the fold (reference gates on a
        // value-pinned Constant key), so the RBS Hash tier answers.
        let (i, ty) = type_of_projection(b"v = { a: 1 }[foo]\n", "[]");
        assert!(!matches!(i.get(ty), Type::Constant(Scalar::Int(1))));
    }

    #[test]
    fn method_param_read_is_dynamic_top() {
        // Inside `def foo(x); x.bar; end`, the receiver `x` is a param read with
        // no top-level binding -> Dynamic[top] -> the call rule stays silent.
        // This is the zero-FP keystone for lowering def bodies.
        let ast = lower_src(b"def foo(x)\n  x.bar\nend\n");
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let recv = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::Call { receiver: Some(r), method, .. } if method == "bar" => Some(*r),
                _ => None,
            })
            .unwrap();
        let ty = typer.type_of(&ast, recv, &env, &mut i);
        assert_eq!(ty, i.untyped());
    }

    #[test]
    fn ivar_and_self_and_const_reads_are_dynamic_top() {
        // `@x`, `self`, and a constant read all type to Dynamic[top] (silent).
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        for src in [b"@x.foo\n".as_slice(), b"self.foo\n".as_slice(), b"Foo.foo\n".as_slice()] {
            let ast = lower_src(src);
            let mut i = Interner::new();
            let env = typer.build_toplevel_env(&ast, &mut i);
            let recv = ast
                .iter()
                .find_map(|(_, n)| match n {
                    Node::Call { receiver: Some(r), method, .. } if method == "foo" => Some(*r),
                    _ => None,
                })
                .unwrap();
            let ty = typer.type_of(&ast, recv, &env, &mut i);
            assert_eq!(ty, i.untyped(), "receiver of {src:?} must be Dynamic[top]");
        }
    }

    #[test]
    fn non_deterministic_or_unknown_call_is_dynamic_top() {
        // `Array#sample` is non-deterministic: never folded, no modeled return
        // -> Dynamic[top]. Drive it on a value-pinned Integer receiver whose
        // unknown method has no return: `42.sample` (sample isn't on Integer).
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let ast = lower_src(b"42.sample\n");
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "sample");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        assert_eq!(ty, i.untyped());
    }

    // --- in-source class typing (ADR-0023 tier-4) ---------------------------

    #[test]
    fn source_class_new_types_to_source_instance() {
        // `class Point; def x; end; end; p = Point.new` — `Point.new` types to a
        // Nominal instance whose ClassId resolves back to "Point" via the source
        // index, and the source index witnesses `y` absent (chain complete:
        // implicit Object super, fully RBS-loaded).
        let ast = lower_src(b"class Point\n  def x\n  end\nend\np = Point.new\np.y\n");
        let idx = CoreIndex::new();
        let source = SourceIndex::build(&ast, &idx);
        let typer = Typer::with_source(&idx, &source);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        // `p` binds to the Point.new instance.
        let p_ty = *env.get("p").expect("p should be bound");
        let class = match i.get(p_ty) {
            Type::Nominal { class, .. } => *class,
            other => panic!("expected Nominal instance, got {other:?}"),
        };
        assert_eq!(source.class_name_for_id(class), Some("Point"));
        // `x` is defined, `y` is not — and the chain is complete.
        assert!(source.class_has_method(&idx, "Point", "x"));
        assert!(!source.class_has_method(&idx, "Point", "y"));
        // Inherited Object method is present (no false absence).
        assert!(source.class_has_method(&idx, "Point", "frozen?"));
    }

    #[test]
    fn unknown_superclass_makes_chain_incomplete_and_silent() {
        // `class User < ApplicationRecord; end` — ApplicationRecord is neither
        // source nor RBS ⇒ chain INCOMPLETE ⇒ any method is assumed present.
        let ast = lower_src(b"class User < ApplicationRecord\nend\nu = User.new\nu.anything\n");
        let idx = CoreIndex::new();
        let source = SourceIndex::build(&ast, &idx);
        assert!(source.knows_class("User"));
        // Even a clearly-bogus method is assumed present (zero-FP keystone).
        assert!(source.class_has_method(&idx, "User", "totally_made_up_xyz"));
        assert!(source.class_has_method(&idx, "User", "anything"));
    }

    #[test]
    fn reopened_source_class_unions_methods() {
        // Two `class C` bodies: the SourceIndex unions their methods.
        let ast = lower_src(b"class C\n  def a\n  end\nend\nclass C\n  def b\n  end\nend\n");
        let idx = CoreIndex::new();
        let source = SourceIndex::build(&ast, &idx);
        assert!(source.class_has_method(&idx, "C", "a"));
        assert!(source.class_has_method(&idx, "C", "b"));
        // A method on neither reopen is witnessed absent (complete chain).
        assert!(!source.class_has_method(&idx, "C", "c"));
    }

    #[test]
    fn source_superclass_chain_resolves_inherited_method() {
        // `class Animal; def speak; end; end; class Dog < Animal; end` —
        // Dog.new.speak is inherited (present); Dog.new.fly is absent (the whole
        // chain Dog -> Animal -> Object is known).
        let ast = lower_src(
            b"class Animal\n  def speak\n  end\nend\nclass Dog < Animal\nend\n",
        );
        let idx = CoreIndex::new();
        let source = SourceIndex::build(&ast, &idx);
        assert!(source.class_has_method(&idx, "Dog", "speak"));
        assert!(!source.class_has_method(&idx, "Dog", "fly"));
    }

    #[test]
    fn rbs_class_new_types_to_rbs_instance() {
        // `Pathname.new("a")` — Pathname is RBS-known (with the stdlib tree) but
        // outside CORE_CLASSES. The stdlib `.new` leniency now lives in the
        // TYPING (`type_dot_new` declines the mint ⇒ Dynamic): the UM witness
        // gate is `knows_class`-wide for source-range Nominals, so a minted
        // Pathname instance WOULD witness — and the reference's `.new` dispatch
        // on these classes has an intricate folding/reflection boundary
        // (fixture 38 pins `Pathname.new("x").nope` silent). The registry /
        // method-existence wiring stays intact for the paths that DO mint
        // (singleton RBS returns — `Pathname.pwd` — and project classes).
        let ast = lower_src(b"p = Pathname.new(\"a\")\np.foo\nq = Pathname.pwd\nq.foo\n");
        let idx = CoreIndex::new();
        let source = SourceIndex::build(&ast, &idx);
        if idx.knows_class("Pathname") {
            let typer = Typer::with_source(&idx, &source);
            let mut i = Interner::new();
            let env = typer.build_toplevel_env(&ast, &mut i);
            // `.new` mint declined ⇒ Dynamic (the leniency).
            let p_ty = *env.get("p").expect("p should be bound");
            assert!(
                matches!(i.get(p_ty), Type::Dynamic(_)),
                "stdlib .new must decline the mint, got {:?}",
                i.get(p_ty)
            );
            // The declaration-driven singleton return still mints the instance
            // (`def self.pwd: () -> Pathname` in core pathname.rbs).
            let q_ty = *env.get("q").expect("q should be bound");
            let class = match i.get(q_ty) {
                Type::Nominal { class, .. } => *class,
                other => panic!("expected Nominal instance from Pathname.pwd, got {other:?}"),
            };
            assert_eq!(source.class_name_for_id(class), Some("Pathname"));
            // A real Pathname method is present; a typo is absent (via RBS).
            assert!(source.class_has_method(&idx, "Pathname", "basename"));
            assert!(!source.class_has_method(&idx, "Pathname", "nonexist"));
        }
    }

    // --- block-form call result typing (recovered, RBS-derived) -------------

    #[test]
    fn block_call_return_types_to_rbs_block_overload() {
        // `arr.map { }` types to a bare Array Nominal (the block-overload
        // return), so a chained `.frist` resolves against Array and is
        // witnessable; `h.select { }` types to Hash; `x.tap { }` types to the
        // receiver's own class. Guarded on the real RBS tree (under the stub
        // fallback block returns are unmodeled ⇒ Dynamic ⇒ test is vacuous).
        let idx = CoreIndex::new();
        if !idx.knows_class("Enumerable") || !idx.class_has_method("Array", "map") {
            return;
        }
        // `a = []; a.map { |x| x }` -> Array nominal.
        let ast = lower_src(b"a = [1]\na.map { |x| x }\n");
        let typer = Typer::new(&idx);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "map");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        assert_eq!(idx.class_name_of(&i, ty), Some("Array"));

        // `h = {}; h.select { }` -> Hash nominal (so `.keys` is valid, silent).
        let ast = lower_src(b"h = { a: 1 }\nh.select { |k, v| v }\n");
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "select");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        assert_eq!(idx.class_name_of(&i, ty), Some("Hash"));

        // `s = "x"; s.tap { }` -> String nominal (self block return = receiver).
        let ast = lower_src(b"s = \"x\"\ns.tap { |x| x }\n");
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "tap");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        assert_eq!(idx.class_name_of(&i, ty), Some("String"));
    }

    #[test]
    fn block_call_on_unmodeled_or_dynamic_is_silent_dynamic() {
        let idx = CoreIndex::new();
        let typer = Typer::new(&idx);
        // A block call on a Dynamic receiver (`x` is an implicit-self call) ⇒
        // Dynamic (never guess). True under both real RBS and the stub.
        let ast = lower_src(b"x.each { |e| e }\n");
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, "each");
        let ty = typer.type_of(&ast, call, &env, &mut i);
        assert_eq!(ty, i.untyped(), "block call on Dynamic receiver must be Dynamic[top]");
    }

    #[test]
    fn unknown_constant_new_is_dynamic() {
        // `Widget.new` where Widget is neither source nor RBS ⇒ Dynamic (silent).
        let ast = lower_src(b"w = Widget.new\nw.foo\n");
        let idx = CoreIndex::new();
        let source = SourceIndex::build(&ast, &idx);
        let typer = Typer::with_source(&idx, &source);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let w_ty = *env.get("w").expect("w should be bound");
        assert_eq!(w_ty, i.untyped(), "unknown-constant .new must be Dynamic[top]");
    }

    /// ADR-0008: the tier-1 sidecar fallback. A `sidecar_foldable` call the Rust
    /// core declines (`255.to_s(16)`) routes to a wired [`folding::RubyFolder`]
    /// and interns its result as a `Constant`; with no folder it stays the nominal
    /// RBS return (the sound subset). Deterministic — no real Ruby.
    #[test]
    fn type_call_routes_sidecar_foldable_to_folder() {
        struct MockFolder(Scalar);
        impl folding::RubyFolder for MockFolder {
            fn fold(&self, _r: &Scalar, _m: &str, _a: &[Scalar]) -> Option<Scalar> {
                Some(self.0.clone())
            }
        }

        let ast = lower_src(b"255.to_s(16)\n");
        let index = CoreIndex::new();
        let source = SourceIndex::build(&ast, &index);
        let call_id = ast
            .iter()
            .find_map(|(id, n)| matches!(n, Node::Call { .. }).then_some(id))
            .expect("a call node");

        // With a folder: the declined-by-Rust base-arg `to_s` folds to the
        // folder's result.
        let mock = MockFolder(Scalar::Str("ff".into()));
        let typer = Typer::with_source_and_folder(&index, &source, Some(&mock));
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let ty = typer.type_of(&ast, call_id, &env, &mut i);
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Str("ff".into())));

        // Without a folder: the nominal `Integer#to_s -> String`, not a Constant
        // (the sound subset — no false constant).
        let typer2 = Typer::with_source(&index, &source);
        let ty2 = typer2.type_of(&ast, call_id, &env, &mut i);
        assert!(!matches!(i.get(ty2), Type::Constant(_)), "no folder ⇒ no constant");
    }

    // ------------------------------------------------------------------
    // C3a: `self.class` nominal-return tail.
    // ------------------------------------------------------------------

    /// Type the call to `method` in `src` under a source+lexical-scope typer
    /// (the full analyze wiring), returning its interned `Type`.
    fn type_c3a_call(src: &[u8], method: &str) -> Type {
        let ast = lower_src(src);
        let idx = CoreIndex::new();
        let source = SourceIndex::build(&ast, &idx);
        let scopes = crate::lexical_scopes(&ast);
        let typer = Typer::with_source(&idx, &source).with_lexical_scopes(&scopes);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let call = find_call(&ast, method);
        let ty = typer.type_of(&ast, call, &env, &mut i);
        i.get(ty).clone()
    }

    #[test]
    fn self_class_itself_is_not_witnessable_singleton() {
        // `self.class` must NOT type to a project `Singleton` — that would route
        // `self.class.<class_method>` through class-method witnessing and FP on
        // every project-defined class method. It stays Dynamic (silent).
        let ty = type_c3a_call(b"class Foo\n  def bar\n    self.class\n  end\nend\n", "class");
        assert!(!matches!(ty, Type::Singleton(_)), "self.class must stay Dynamic, got {ty:?}");
    }

    #[test]
    fn self_class_name_and_to_s_are_string() {
        // `self.class.name` / `self.class.to_s` → `Nominal[String]` (the
        // `Module#name : String?` optional is unwrapped for witnessing).
        for (src, m) in [
            (b"class Foo\n  def bar\n    self.class.name\n  end\nend\n".as_slice(), "name"),
            (b"class Foo\n  def bar\n    self.class.to_s\n  end\nend\n".as_slice(), "to_s"),
        ] {
            let ty = type_c3a_call(src, m);
            let idx = CoreIndex::new();
            let mut i = Interner::new();
            let interned = i.intern(ty.clone());
            assert_eq!(
                idx.class_name_of(&i, interned),
                Some("String"),
                "self.class.{m} must be String, got {ty:?}"
            );
        }
    }

    #[test]
    fn self_class_name_string_in_nested_class() {
        // Deeply nested enclosing class still resolves the tail to String.
        let ty = type_c3a_call(
            b"module Outer\n  class Runner\n    def k\n      self.class.name\n    end\n  end\nend\n",
            "name",
        );
        let idx = CoreIndex::new();
        let mut i = Interner::new();
        let interned = i.intern(ty.clone());
        assert_eq!(idx.class_name_of(&i, interned), Some("String"), "got {ty:?}");
    }

    #[test]
    fn self_class_at_toplevel_declines() {
        // No enclosing class ⇒ `self.class` declines to Dynamic (silent), so the
        // tail never becomes String — matches the reference's toplevel silence.
        let ty = type_c3a_call(b"self.class.name\n", "class");
        assert!(!matches!(ty, Type::Singleton(_)), "toplevel self.class must not type Singleton, got {ty:?}");
        let name_ty = type_c3a_call(b"self.class.name\n", "name");
        let idx = CoreIndex::new();
        let mut i = Interner::new();
        let interned = i.intern(name_ty.clone());
        assert_ne!(idx.class_name_of(&i, interned), Some("String"), "toplevel tail must not be String");
    }

    #[test]
    fn self_class_name_string_even_in_core_shadow_class() {
        // A nested class whose WRITTEN name shadows a core class (`Time`) still
        // resolves `self.class.name` → String (no `Singleton` is minted, so there
        // is no core-shadow witnessing hazard) — matching the reference, which
        // fires the String tail here too.
        let ty = type_c3a_call(
            b"module Shadowing\n  class Time\n    def bar\n      self.class.name\n    end\n  end\nend\n",
            "name",
        );
        let idx = CoreIndex::new();
        let mut i = Interner::new();
        let interned = i.intern(ty.clone());
        assert_eq!(idx.class_name_of(&i, interned), Some("String"), "got {ty:?}");
    }

    #[test]
    fn core_singleton_name_is_string() {
        // Bonus: `name`/`to_s` on a core-RBS `Singleton` (`Time.name`) → String.
        let ty = type_c3a_call(b"class Foo\n  def bar\n    Time.name\n  end\nend\n", "name");
        let idx = CoreIndex::new();
        let mut i = Interner::new();
        let interned = i.intern(ty.clone());
        assert_eq!(idx.class_name_of(&i, interned), Some("String"), "Time.name must be String, got {ty:?}");
    }
}



#[cfg(test)]
mod m2_go_slice_tests {
    use super::*;
    use rigor_parse::{lower, parse};

    fn ty_of_last_recv_call(src: &[u8]) -> String {
        let ast = lower(&parse(src));
        let index = CoreIndex::new();
        let source = SourceIndex::build(&ast, &index);
        let typer = Typer::with_source(&index, &source);
        let mut i = Interner::new();
        let env = TypeEnv::new();
        let call_id = ast
            .iter()
            .filter_map(|(id, n)| matches!(n, Node::Call { receiver: Some(_), .. }).then_some(id))
            .last()
            .unwrap();
        let ty = typer.type_of(&ast, call_id, &env, &mut i);
        rigor_types::describe(&i, ty)
    }

    /// Slice 4: a class-method call on a core Singleton types its unanimous RBS
    /// return (source-range Nominal for classes outside the 9-class core table).
    #[test]
    fn singleton_rbs_return_types_time_now() {
        // Core-table return resolves to the core Nominal directly
        // (`describe` renders a Nominal by id: Integer = Class<1>).
        assert_eq!(ty_of_last_recv_call(b"s = Integer.sqrt(4)\n"), "Class<1>");
        // Divergent overloads (Regexp.last_match) stay Dynamic on THIS path.
        assert_eq!(ty_of_last_recv_call(b"m = Regexp.last_match(2)\n"), "Dynamic[top]");
    }

    /// Slice 2/3: Kernel#Array folds by argument type; rand types by arity.
    #[test]
    fn kernel_array_and_rand_type() {
        let ty = |src: &[u8]| -> String {
            let ast = lower(&parse(src));
            let index = CoreIndex::new();
            let typer = Typer::new(&index);
            let mut i = Interner::new();
            let env = TypeEnv::new();
            let call_id = ast
                .iter()
                .filter_map(|(id, n)| {
                    matches!(n, Node::Call { receiver: None, .. }).then_some(id)
                })
                .last()
                .unwrap();
            let t = typer.type_of(&ast, call_id, &env, &mut i);
            rigor_types::describe(&i, t)
        };
        // Tuple identity / nil collapse / scalar wrap / nominal fallback.
        assert_eq!(ty(b"Array([1, 2])\n"), "Tuple[Constant[1], Constant[2]]");
        assert_eq!(ty(b"Array(nil)\n"), "Tuple[]");
        assert_eq!(ty(b"Array(5)\n"), "Tuple[Constant[5]]");
        // Nominal Array renders by core id (Array = Class<4>).
        assert_eq!(ty(b"def f(c)\n  Array(c)\nend\n"), "Class<4>");
        // rand: 0-arg Float (Class<2>); ANY non-Range 1-arg Integer (Class<1>,
        // the reference's measured overload pick); a Range arg declines.
        assert_eq!(ty(b"rand\n"), "Class<2>");
        assert_eq!(ty(b"rand(5)\n"), "Class<1>");
        assert_eq!(ty(b"def f(c)\n  rand(c)\nend\n"), "Class<1>");
        assert_eq!(ty(b"rand(1..5)\n"), "Dynamic[top]");
    }
}

#[cfg(test)]
mod meta_new_lift_tests {
    use super::*;
    use rigor_parse::{lower, parse};

    fn new_ty(src: &[u8]) -> String {
        let ast = lower(&parse(src));
        let index = CoreIndex::new();
        let source = SourceIndex::build(&ast, &index);
        let typer = Typer::with_source(&index, &source);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let ty = *env.get("v").expect("v bound");
        rigor_types::describe(&i, ty)
    }

    /// The reference `meta_new` constant-constructor lifts (Pathname pinned-Str,
    /// Date/DateTime all-pinned, Set empty/pinned-Tuple) produce pinned VALUE
    /// carriers rigor-rs does not model — the mint declines (Dynamic). Every
    /// other singleton `.new` mints a witnessable instance, matching the
    /// reference's `nominal_of` fallback (probed live on all of these shapes).
    #[test]
    fn curated_constructor_lifts_decline_and_others_mint() {
        // Lift shapes -> decline (Dynamic).
        assert_eq!(new_ty(b"v = Pathname.new(\"x\")\n"), "Dynamic[top]");
        assert_eq!(new_ty(b"v = Date.new(2020)\n"), "Dynamic[top]");
        assert_eq!(new_ty(b"v = Set.new\n"), "Dynamic[top]");
        assert_eq!(new_ty(b"v = Set.new([1, 2])\n"), "Dynamic[top]");
        // Non-lift shapes -> minted instance (source-range Nominal renders
        // Class<1000000+>).
        assert!(new_ty(b"v = Pathname.new(:sym)\n").starts_with("Class<"));
        assert!(new_ty(b"def f(x)\n  $g = Pathname.new(x)\nend\nv = Time.new\n").starts_with("Class<"));
        assert!(new_ty(b"v = StringIO.new\n").starts_with("Class<"));
    }
}

#[cfg(test)]
mod rbs_tuple_return_tests {
    use super::*;
    use rigor_parse::{lower, parse};

    /// The rendered type of the LAST receiver-bearing call in `src`.
    fn last_call_ty(src: &[u8]) -> String {
        let ast = lower(&parse(src));
        let index = CoreIndex::new();
        let source = SourceIndex::build(&ast, &index);
        let typer = Typer::with_source(&index, &source);
        let mut i = Interner::new();
        let env = TypeEnv::new();
        let call_id = ast
            .iter()
            .filter_map(|(id, n)| matches!(n, Node::Call { receiver: Some(_), .. }).then_some(id))
            .last()
            .unwrap();
        let ty = typer.type_of(&ast, call_id, &env, &mut i);
        rigor_types::describe(&i, ty)
    }

    /// MultiWrite substrate Slice 2: a SINGLETON RBS tuple return types
    /// per-position (`Process.wait2 : [Integer, Process::Status]`) instead of
    /// collapsing to `Dynamic[top]`. `Integer` is a core id (`Class<1>`); the
    /// RBS-only `Process::Status` carries a source-registry id (`Class<1_000_xxx>`),
    /// minted by `SourceIndex`'s tuple-element pre-registration — no source file
    /// here names `Process::Status`.
    #[test]
    fn singleton_tuple_return_types_per_position() {
        let rendered = last_call_ty(b"_pid, status = Process.wait2\n");
        assert!(
            rendered.starts_with("Tuple[Class<1>, Class<"),
            "expected a 2-element tuple, got {rendered}"
        );
    }

    /// The instance twin (`String#partition -> [String, String, String]`), and
    /// the element ids are the CORE String id.
    #[test]
    fn instance_tuple_return_types_per_position() {
        assert_eq!(
            last_call_ty(b"x = \"a-b\".partition(\"-\")\n"),
            "Tuple[Class<0>, Class<0>, Class<0>]"
        );
    }

    /// The Slice-1 binder distributes the tuple across the multi-write targets,
    /// so `status` binds to the `Process::Status` nominal — the link fixture 68
    /// needs. Compared against the SAME id the typer mints for the tuple slot.
    #[test]
    fn multi_write_binds_a_tuple_slot_to_its_rbs_class() {
        let ast = lower(&parse(b"_pid, status = Process.wait2\nstatus\n"));
        let index = CoreIndex::new();
        let source = SourceIndex::build(&ast, &index);
        let typer = Typer::with_source(&index, &source);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let status = *env.get("status").expect("status must be bound");
        let name = source.class_name_for_id_of(&i, status);
        assert_eq!(name, Some("Process::Status"), "got {:?}", i.get(status));
    }

    /// An RBS return this descriptor does not model stays exactly as before
    /// (`Dynamic[top]`): `IO.pipe`'s overloads disagree (a block overload
    /// returns the block's value), so the all-overloads-agree collapse declines.
    #[test]
    fn divergent_overloads_stay_dynamic() {
        assert_eq!(last_call_ty(b"r, w = IO.pipe\n"), "Dynamic[top]");
    }

    /// Upstream #121: an array of statically known values kept its precision
    /// through concatenation and slicing but lost it at a set operation. Each
    /// result below is the answer real Ruby gives for the same expression.
    #[test]
    fn tuple_set_operations_fold() {
        assert_eq!(last_call_ty(b"[1, 2] & [2]\n"), "Tuple[Constant[2]]");
        assert_eq!(
            last_call_ty(b"[1] | [2]\n"),
            "Tuple[Constant[1], Constant[2]]"
        );
        // `-` does NOT de-duplicate what survives: `[1, 1, 2] - [2] == [1, 1]`.
        assert_eq!(
            last_call_ty(b"[1, 1, 2] - [2]\n"),
            "Tuple[Constant[1], Constant[1]]"
        );
        // The named spellings take several arguments, reduced left to right.
        assert_eq!(
            last_call_ty(b"[1, 2].intersection([2, 3], [2])\n"),
            "Tuple[Constant[2]]"
        );
        assert_eq!(last_call_ty(b"[1, 2].intersect?([3])\n"), "Constant[false]");
        assert_eq!(last_call_ty(b"[1, 2].intersect?([2])\n"), "Constant[true]");
    }

    /// Membership is `eql?`, not `==` — `[1] & [1.0]` is EMPTY at runtime even
    /// though `1 == 1.0`. Getting this wrong is the whole reason upstream ran
    /// Ruby's own operator instead of reimplementing membership.
    #[test]
    fn tuple_set_operations_use_eql_not_equality() {
        assert_eq!(last_call_ty(b"[1] & [1.0]\n"), "Tuple[]");
        // Both survive the union because they are not `eql?` — the rendering
        // of `Constant[1.0]` as `1` is `describe`'s float formatting, not a
        // collapse (the intersection above proves they are distinct values).
        assert_eq!(
            last_call_ty(b"[1] | [1.0]\n"),
            "Tuple[Constant[1], Constant[1]]"
        );
    }

    /// `one?` (no block) counts TRUTHY elements, so `nil` / `false` do not
    /// count; `at` folds an in-range constant index.
    #[test]
    fn tuple_one_and_at_fold() {
        assert_eq!(last_call_ty(b"[nil, false, 3].one?\n"), "Constant[true]");
        assert_eq!(last_call_ty(b"[nil, 2, 3].one?\n"), "Constant[false]");
        assert_eq!(last_call_ty(b"[1, 2, 3].at(1)\n"), "Constant[2]");
        assert_eq!(last_call_ty(b"[1, 2, 3].at(-1)\n"), "Constant[3]");
    }

    /// The declines. An out-of-range `at` does NOT fold to nil — proving nil on
    /// a receiver the RBS tier calls optional would newly SURFACE diagnostics,
    /// a different decision from removing a Dynamic. An argument that is not a
    /// pinned Tuple, or an element that is not pinned, leaves the RBS tier to
    /// widen.
    #[test]
    fn tuple_set_operations_decline_when_undecidable() {
        assert_eq!(last_call_ty(b"[1, 2, 3].at(9)\n"), "Dynamic[top]");
        // `Class<4>` is core `Array` — the RBS tier's widened answer.
        assert_eq!(last_call_ty(b"[1, 2] & unknown_thing\n"), "Class<4>");
        assert_eq!(last_call_ty(b"[1, unknown_thing] & [1]\n"), "Class<4>");
        // No argument at all is not a set operation; the RBS tier answers.
        assert_eq!(last_call_ty(b"[1, 2].intersection\n"), "Class<4>");
    }
}

// ---------------------------------------------------------------------------
// `is_a?` / `case-when` class narrowing (census mechanism 1) — the oracle
// probe matrix a1–a6 + the load-bearing declines
// ---------------------------------------------------------------------------

#[cfg(test)]
mod class_narrowing_tests {
    use super::*;
    use rigor_parse::{lower, parse};

    fn lower_src(src: &[u8]) -> LoweredAst {
        lower(&parse(src))
    }

    /// The class-narrowing snapshot map for `src`, wired exactly as the analyze
    /// pass wires it (per-file source index + lexical scopes, so the shadow
    /// gate is live).
    fn class_snaps(src: &[u8]) -> (LoweredAst, HashMap<NodeId, String>) {
        let ast = lower_src(src);
        let index = CoreIndex::new();
        let source = SourceIndex::build(&ast, &index);
        let scopes = lexical_scopes(&ast);
        let typer = Typer::with_source(&index, &source).with_lexical_scopes(&scopes);
        let mut i = Interner::new();
        let snaps = typer.class_narrowing_snapshots(&ast, &mut i);
        (ast, snaps)
    }

    /// The node id of the first call named `method`, or panic.
    fn call_named(ast: &LoweredAst, method: &str) -> NodeId {
        ast.iter()
            .find_map(|(id, n)| match n {
                Node::Call { method: m, .. } if m == method => Some(id),
                _ => None,
            })
            .unwrap_or_else(|| panic!("call `{method}` present"))
    }

    /// a1: `if value.is_a?(Hash)` narrows the branch use; the use AFTER the
    /// `if` (no terminating opposite branch) stays un-narrowed.
    #[test]
    fn class_narrowing_if_branch_narrows_use_after_does_not() {
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  if value.is_a?(Hash)\n    value.frobnicate_zzz\n  end\n  value.after_zzz\nend\n",
        );
        assert_eq!(snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str), Some("Hash"));
        assert!(!snaps.contains_key(&call_named(&ast, "after_zzz")));
    }

    /// a2: a ternary narrows its truthy arm only (the falsey arm is UNCHANGED).
    #[test]
    fn class_narrowing_ternary_truthy_arm_only() {
        let (ast, snaps) = class_snaps(
            b"def f(rule)\n  rule.is_a?(Hash) ? rule.frobnicate_zzz : rule.other_zzz\nend\n",
        );
        assert_eq!(snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str), Some("Hash"));
        assert!(!snaps.contains_key(&call_named(&ast, "other_zzz")));
    }

    /// a4: rebinding the local inside the branch invalidates the narrowing for
    /// subsequent uses (`scope.rb:194`).
    #[test]
    fn class_narrowing_rebind_in_branch_invalidates() {
        let (ast, snaps) = class_snaps(
            b"def f(value, other)\n  if value.is_a?(Hash)\n    value = other\n    value.frobnicate_zzz\n  end\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    }

    /// a5: a TERMINATING opposite branch propagates the truthy edge past the
    /// guard (`eval_if:481` early-return narrowing) — both the `return` and the
    /// `raise` idiom.
    #[test]
    fn class_narrowing_early_return_propagates() {
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  return unless value.is_a?(Hash)\n  value.frobnicate_zzz\nend\n",
        );
        assert_eq!(snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str), Some("Hash"));

        let (ast, snaps) = class_snaps(
            b"def f(value)\n  raise ArgumentError unless value.is_a?(Hash)\n  value.frobnicate_zzz\nend\n",
        );
        assert_eq!(snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str), Some("Hash"));
    }

    /// a5 counterpart: a rebind AFTER the propagated guard kills the fact.
    #[test]
    fn class_narrowing_rebind_after_guard_invalidates() {
        let (ast, snaps) = class_snaps(
            b"def f(value, other)\n  return unless value.is_a?(Hash)\n  value = other\n  value.frobnicate_zzz\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    }

    /// `kind_of?` and `instance_of?` route identically (`narrowing.rb:979`).
    #[test]
    fn class_narrowing_kind_of_and_instance_of_narrow() {
        for method in ["kind_of?", "instance_of?"] {
            let src = format!(
                "def f(value)\n  if value.{method}(Hash)\n    value.frobnicate_zzz\n  end\nend\n"
            );
            let (ast, snaps) = class_snaps(src.as_bytes());
            assert_eq!(
                snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str),
                Some("Hash"),
                "{method} must narrow"
            );
        }
    }

    /// Decline: facts do NOT enter a block body (ADR-0038 §3) — but the
    /// receiver of the block-bearing call itself, OUTSIDE the block, records.
    #[test]
    fn class_narrowing_block_body_declines_receiver_outside_records() {
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  if value.is_a?(Hash)\n    value.deep_frobnicate_zzz { |k| value.inner_zzz }\n  end\nend\n",
        );
        assert_eq!(
            snaps.get(&call_named(&ast, "deep_frobnicate_zzz")).map(String::as_str),
            Some("Hash")
        );
        assert!(!snaps.contains_key(&call_named(&ast, "inner_zzz")));
    }

    /// Stage 3a-1 REPLACED the blanket `&&`/`||` decline: one recognised
    /// conjunct now narrows the truthy edge (probe c1a, reference fires), while
    /// the falsey edge of the same predicate still narrows nothing (c1g). The
    /// full matrix lives in
    /// [`class_narrowing_tests::class_narrowing_stage3a1_compound_predicate_matrix`].
    #[test]
    fn class_narrowing_logical_predicate_narrows_truthy_only() {
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  if value.is_a?(Hash) && value.foo\n    value.frobnicate_zzz\n  end\nend\n",
        );
        assert_eq!(
            snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str),
            Some("Hash")
        );
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  if value.is_a?(Hash) && value.foo\n    1\n  else\n    value.frobnicate_zzz\n  end\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    }

    /// Decline: a project declaration shadowing the constant kills the
    /// narrowing entirely (never narrow to the project nominal in this slice).
    #[test]
    fn class_narrowing_shadowed_constant_declines() {
        let (ast, snaps) = class_snaps(
            b"class Hash\nend\ndef f(value)\n  if value.is_a?(Hash)\n    value.frobnicate_zzz\n  end\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    }

    /// Decline: a local with a concrete (non-Dynamic/Top) carrier is untouched
    /// (`narrow_class_other` narrows Dynamic/Top ONLY).
    #[test]
    fn class_narrowing_non_dynamic_local_declines() {
        let (ast, snaps) = class_snaps(
            b"value = \"str\"\nif value.is_a?(Hash)\n  value.frobnicate_zzz\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    }

    /// Decline: a mutator call on the local kills the fact for SUBSEQUENT uses
    /// (the mutator call itself still records — its receiver read precedes the
    /// mutation).
    #[test]
    fn class_narrowing_mutator_call_invalidates_subsequent_uses() {
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  if value.is_a?(Hash)\n    value.merge!(a: 1)\n    value.frobnicate_zzz\n  end\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    }

    /// R1 decline: an expression-position rebind in an argument list threads
    /// IMMEDIATELY — `f(value = x, value.frobnicate_zzz)` inside an `is_a?`
    /// branch must record nothing (Ruby evaluates arguments left-to-right, so
    /// the second argument reads the rebound local).
    #[test]
    fn class_narrowing_arg_position_rebind_invalidates_sibling_use() {
        let (ast, snaps) = class_snaps(
            b"def f(value, x)\n  if value.is_a?(Hash)\n    g(value = x, value.frobnicate_zzz)\n  end\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    }

    /// R2 decline: early-return propagation is STATEMENT-position only — an
    /// expression-position conditional with a terminating falsey arm
    /// (`f(value.is_a?(Hash) ? value : raise)`) must NOT narrow the statements
    /// after it (unprobed oracle behavior). The statement-position a5 idiom
    /// (its own test above) keeps propagating.
    #[test]
    fn class_narrowing_expression_position_if_never_propagates() {
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  g(value.is_a?(Hash) ? value : raise)\n  value.frobnicate_zzz\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
        // Assignment-RHS position is expression position too.
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  y = value.is_a?(Hash) ? value : raise\n  value.frobnicate_zzz\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    }

    /// R3 decline: a nested guard CONFLICTING with the outer fact
    /// (`if v.is_a?(Hash)` then inner `if v.is_a?(String)`) narrows nothing —
    /// the reference's carrier is `Nominal[Hash]` at the inner guard (Bot on a
    /// disjoint re-narrow), out of the Dynamic-only envelope. A SAME-class
    /// re-guard keeps the fact.
    #[test]
    fn class_narrowing_nested_conflicting_guard_declines() {
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  if value.is_a?(Hash)\n    if value.is_a?(String)\n      value.frobnicate_zzz\n    end\n  end\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
        // Same-class re-guard: the fact survives (a no-op re-narrowing).
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  if value.is_a?(Hash)\n    if value.is_a?(Hash)\n      value.frobnicate_zzz\n    end\n  end\nend\n",
        );
        assert_eq!(
            snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str),
            Some("Hash")
        );
    }

    /// The POSITION matrix (docs/notes/20260807-block-narrowing-position-rule
    /// .md): a block body and a `case`/`when` clause narrow ONLY from statement
    /// position or an assignment RHS; a receiver, an argument or a `return`
    /// operand narrows nothing. `if`/ternary is the exception (p4, p8 narrow in
    /// every position). Every row was measured against the pinned reference —
    /// `Some(c)` means the reference FIRES and rigor-rs must record `c`, `None`
    /// means the reference is SILENT and rigor-rs must record nothing.
    ///
    /// Safe-nav is NOT the axis: s3/s4 (`h&.transform_values { … }` in
    /// statement position) fire on both engines, which is why PR #63's
    /// `if !safe_nav` block decline was wrong.
    #[test]
    fn class_narrowing_position_matrix() {
        // (row, source, expected narrowed class of the `frobnicate_zzz` call)
        let rows: &[(&str, &[u8], Option<&str>)] = &[
            // --- block bodies: statement position / assignment RHS -> narrows
            ("s1", b"def f(h)\n  h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\nend\n", Some("String")),
            ("s2", b"def f(h)\n  h.transform_values do |v|\n    v.is_a?(String) ? v.frobnicate_zzz : v\n  end\nend\n", Some("String")),
            ("s3", b"def f(h)\n  h&.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\nend\n", Some("String")),
            ("s4", b"def f(h)\n  h&.transform_values do |v|\n    v.is_a?(String) ? v.frobnicate_zzz : v\n  end\nend\n", Some("String")),
            ("s8", b"def f(h)\n  x = h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\n  x\nend\n", Some("String")),
            ("s10", b"def f(h)\n  h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\n  nil\nend\n", Some("String")),
            // --- block bodies: receiver / argument / `return` -> declines
            ("s5", b"def f(h)\n  h&.transform_values do |v|\n    v.is_a?(String) ? v.frobnicate_zzz : v\n  end&.compact\nend\n", None),
            ("s6", b"def f(h)\n  h&.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }&.compact\nend\n", None),
            ("s7", b"def f(h)\n  h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }.compact\nend\n", None),
            ("s9", b"def g(y)\n  y\nend\n\ndef f(h)\n  g(h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v })\nend\n", None),
            ("s11", b"def f(h)\n  h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }.compact.to_a\nend\n", None),
            ("s12", b"def f(h)\n  return h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\nend\n", None),
            ("s13", b"def g(y)\n  y\nend\n\ndef f(h)\n  x = g(h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v })\n  x\nend\n", None),
            // --- `case`/`when`: the same positional rule
            ("p6", b"def f(v)\n  case v\n  when Hash\n    v.frobnicate_zzz\n  end\nend\n", Some("Hash")),
            ("p1", b"def f(v)\n  x = case v\n      when Hash\n        v.frobnicate_zzz\n      end\n  x\nend\n", Some("Hash")),
            ("p2", b"def g(y)\n  y\nend\n\ndef f(v)\n  g(case v\n    when Hash\n      v.frobnicate_zzz\n    end)\nend\n", None),
            ("p3", b"def f(v)\n  (case v\n   when Hash\n     v.frobnicate_zzz\n   end).to_s\nend\n", None),
            ("p7", b"def f(v)\n  return case v\n         when Hash\n           v.frobnicate_zzz\n         end\nend\n", None),
            // --- `if`/ternary: the EXCEPTION — narrows in every position
            ("p4", b"def g(y)\n  y\nend\n\ndef f(v)\n  g(v.is_a?(Hash) ? v.frobnicate_zzz : v)\nend\n", Some("Hash")),
            ("p8", b"def f(v)\n  (v.is_a?(Hash) ? v.frobnicate_zzz : v).to_s\nend\n", Some("Hash")),
            // --- nesting / carrier corners (all oracle-measured)
            ("p5", b"def f(h)\n  h.each do |a|\n    a.each do |v|\n      v.is_a?(String) ? v.frobnicate_zzz : v\n    end\n  end\nend\n", Some("String")),
            ("x1", b"def g(y)\n  y\nend\n\ndef f(h, k)\n  g(case k\n    when Integer\n      h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\n    end)\nend\n", None),
            ("x2", b"def g(y)\n  y\nend\n\ndef f(h, k)\n  g(k ? h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v } : nil)\nend\n", None),
            ("x3", b"def f(h)\n  a, b = h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\n  [a, b]\nend\n", Some("String")),
            ("x4", b"def f(h, x)\n  x ||= h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\n  x\nend\n", Some("String")),
            ("x5", b"def f(h, k)\n  if k\n    h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\n  end\nend\n", Some("String")),
        ];
        for (row, src, expected) in rows {
            let (ast, snaps) = class_snaps(src);
            let got = snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str);
            assert_eq!(got, *expected, "position matrix row {row}");
        }
    }

    /// CARRIER FIDELITY (docs/notes/20260808-narrowing-carrier-fidelity-fp.md):
    /// the `narrow_class_other` Dynamic-only gate is a SUBSET rule only over
    /// carriers both engines type `Dynamic`/`Top`. rigor-rs collapses a long
    /// tail of carriers to `Dynamic[top]` that the reference types precisely, so
    /// on those our gate fires where theirs declines. [`coarse_locals`] +
    /// [`narrowable_binding`] turn the gate into an ALLOW-list; this matrix pins
    /// every measured member and every measured decline.
    ///
    /// Every row is oracle-measured against the pinned reference from a fresh
    /// cwd with `--no-cache`. `Some(c)` — the reference FIRES and rigor-rs must
    /// record `c`. `None` — rigor-rs must record NOTHING, either because the
    /// reference is SILENT (a would-be false positive: the `fp*` rows) or
    /// because the decline costs coverage the reference has (the `cost*` rows,
    /// a strict subset).
    #[test]
    fn class_narrowing_carrier_fidelity_matrix() {
        // The guard/use tail every row shares. `if`/ternary narrows in every
        // position (p4/p8), so the row's whole point is the local's BINDING.
        const G: &str = "  h.is_a?(Hash) ? h.frobnicate_zzz : h\n";
        let bound = |expr: &str| format!("def f(spec, list, cond)\n  h = {expr}\n{G}end\n");
        let rows: Vec<(&str, String, Option<&str>)> = vec![
            // ---- ALLOW-list: measured Dynamic on BOTH engines ---------------
            // A method / block / keyword / optional / rest / block parameter.
            ("ok_param", format!("def f(h)\n{G}end\n"), Some("Hash")),
            ("ok_kwarg", format!("def f(k: nil)\n  h = k\n{G}end\n"), Some("Hash")),
            ("ok_optarg", format!("def f(o = nil)\n  h = o\n{G}end\n"), Some("Hash")),
            ("ok_restarg", format!("def f(*a)\n  h = a\n{G}end\n"), Some("Hash")),
            ("ok_blockarg", format!("def f(&blk)\n  h = blk\n{G}end\n"), Some("Hash")),
            // `@ivar` / `$gvar` / `@@cvar` reads — the reference types none.
            ("ok_ivar", bound("@x"), Some("Hash")),
            ("ok_gvar", bound("$gx"), Some("Hash")),
            (
                "ok_cvar",
                format!("class C\n  def f\n    h = @@cx\n  {G}  end\nend\n"),
                Some("Hash"),
            ),
            // A call THROUGH a narrowable receiver (an untyped receiver resolves
            // no method on either side), incl. safe-nav, a block, `[]`, chains.
            ("ok_call", bound("spec.unknown_zzz"), Some("Hash")),
            ("ok_call_chain", bound("spec.foo_zzz.bar_zzz"), Some("Hash")),
            ("ok_call_index", bound("spec[0]"), Some("Hash")),
            ("ok_call_safenav", bound("spec&.dup"), Some("Hash")),
            ("ok_call_block", bound("spec.map { |x| x }"), Some("Hash")),
            ("ok_call_ivar_recv", bound("@obj.foo_zzz"), Some("Hash")),
            ("ok_call_gvar_recv", bound("$gobj.foo_zzz"), Some("Hash")),
            // Destructuring loses precision on BOTH sides — even from a Logical.
            ("ok_multiwrite", format!("def f(spec)\n  a, h = spec\n{G}end\n"), Some("Hash")),
            (
                "ok_multiwrite_logical",
                format!("def f(spec)\n  a, h = (spec || {{}})\n{G}end\n"),
                Some("Hash"),
            ),
            // ---- DECLINES that close a live FP (reference SILENT) -----------
            // `Logical`: the measured archetype. `analyse_or` builds a UNION.
            ("fp_or", bound("spec || {}"), None),
            ("fp_and", bound("spec && {}"), None),
            ("fp_or_nested", bound("(spec || other_zzz) || {}"), None),
            ("fp_paren", bound("(spec || {})"), None),
            ("fp_opwrite", format!("def f(spec)\n  h = spec\n  h ||= {{}}\n{G}end\n"), None),
            // A project method whose return TAIL is a `Logical` — reached
            // through an implicit-self call and through `self.`.
            (
                "fp_insource_logical",
                format!("def mk\n  unknown_zzz || {{}}\nend\n\ndef f\n  h = mk\n{G}end\n"),
                None,
            ),
            (
                "fp_self_insource_logical",
                format!(
                    "class C\n  def f\n    h = self.mk\n  {G}  end\n\n  def mk\n    unknown_zzz || {{}}\n  end\nend\n"
                ),
                None,
            ),
            (
                "fp_recv_insource_logical",
                format!(
                    "class D\n  def mk\n    unknown_zzz || {{}}\n  end\nend\n\ndef f\n  d = D.new\n  h = d.mk\n{G}end\n"
                ),
                None,
            ),
            // A loop's value (`nil` on the reference).
            ("fp_while", format!("def f(cond)\n  h = while cond\n    break({{}})\n  end\n{G}end\n"), None),
            ("fp_for", format!("def f(list)\n  h = for i in list\n    nil\n  end\n{G}end\n"), None),
            // `begin`/`rescue` and the `rescue` modifier — a UNION.
            (
                "fp_beginrescue",
                format!("def f(spec)\n  h = begin\n    spec\n  rescue StandardError\n    {{}}\n  end\n{G}end\n"),
                None,
            ),
            ("fp_rescue_mod", bound("(spec rescue {})"), None),
            // A `rescue => e` capture: the reference binds the exception CLASS.
            (
                "fp_rescue_bind",
                format!("def f\n  begin\n    nil\n  rescue StandardError => h\n  {G}  end\nend\n"),
                None,
            ),
            // `case`/`in` and `if`/ternary AS EXPRESSIONS — a UNION the
            // reference keeps and our `Algebra::join` collapses into Dynamic.
            (
                "fp_case",
                format!("def f(cond, spec)\n  h = case cond\n  when 1 then spec\n  else {{}}\n  end\n{G}end\n"),
                None,
            ),
            (
                "fp_case_in",
                format!("def f(cond, spec)\n  h = case cond\n  in Integer then spec\n  else {{}}\n  end\n{G}end\n"),
                None,
            ),
            ("fp_ternary", bound("cond ? spec : {}"), None),
            (
                "fp_if",
                format!("def f(cond, spec)\n  h = if cond\n    spec\n  else\n    {{}}\n  end\n{G}end\n"),
                None,
            ),
            // `Range`, `self`, a lambda/proc — all precisely typed by the
            // reference, all `Dynamic[top]` here.
            ("fp_range_lit", bound("(1..2)"), None),
            ("fp_range_dyn", bound("(spec..spec)"), None),
            ("fp_self", format!("class C\n  def f\n    h = self\n  {G}  end\nend\n"), None),
            ("fp_lambda", bound("->(x) { x }"), None),
            ("fp_proc", bound("proc { |x| x }"), None),
            // Kernel methods with a precise RBS return, reached receiverless —
            // the reason an implicit-self call cannot be allow-listed.
            ("fp_method_ref", bound("__method__"), None),
            ("fp_binding", bound("binding"), None),
            ("fp_caller", bound("caller"), None),
            ("fp_block_given", bound("block_given?"), None),
            // `defined?` (`String?`), a `*splat` (`Array`), a `return` operand.
            ("fp_defined", bound("defined?(spec)"), None),
            ("fp_splat", bound("*spec"), None),
            ("fp_return", bound("(return {} if cond)"), None),
            // A receiver the reference types precisely enough to resolve the
            // method on: `self`, a constant.
            ("fp_const_recv", bound("Float::INFINITY.abs"), None),
            // ---- DECLINES that COST coverage (reference FIRES) --------------
            ("cost_yield", format!("def f\n  h = yield\n{G}end\n"), None),
            ("cost_super", format!("class C\n  def f\n    h = super\n  {G}  end\nend\n"), None),
            ("cost_implicit_self", bound("unknown_zzz"), None),
            (
                "cost_implicit_self_insource",
                format!("def mk\n  unknown_zzz\nend\n\ndef f\n  h = mk\n{G}end\n"),
                None,
            ),
            ("cost_const_read", bound("XCONST_ZZZ"), None),
            ("cost_const_recv", bound("File.foo_zzz"), None),
            ("cost_str_recv", bound("\"s\".foo_zzz"), None),
            ("cost_self_recv", format!("class C\n  def f\n    h = self.unknown_zzz\n  {G}  end\nend\n"), None),
            (
                "cost_call_on_coarse",
                format!("def f(spec)\n  a = spec || {{}}\n  h = a.dup\n{G}end\n"),
                None,
            ),
            (
                "cost_case_noelse",
                format!("def f(cond, spec)\n  h = case cond\n  when 1 then spec\n  end\n{G}end\n"),
                None,
            ),
            (
                "cost_begin_ensure",
                format!("def f(spec)\n  h = begin\n    spec\n  ensure\n    nil\n  end\n{G}end\n"),
                None,
            ),
        ];
        for (row, src, expected) in &rows {
            let (ast, snaps) = class_snaps(src.as_bytes());
            let got = snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str);
            assert_eq!(got, *expected, "carrier-fidelity row {row}");
        }
    }

    /// The coarse-carrier decline is SCOPED: a name made coarse in one `def`
    /// must not disable narrowing for the same name in another `def` (a
    /// whole-file set would silence common names like `h`/`value` project-wide).
    #[test]
    fn class_narrowing_coarse_set_is_per_scope() {
        let src = b"def a(spec)\n  h = spec || {}\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n\ndef b(h)\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n";
        let (ast, snaps) = class_snaps(src);
        let calls: Vec<NodeId> = ast
            .iter()
            .filter_map(|(id, n)| match n {
                Node::Call { method, .. } if method == "frobnicate_zzz" => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(snaps.get(&calls[0]), None, "the Logical-bound `h` must decline");
        assert_eq!(
            snaps.get(&calls[1]).map(String::as_str),
            Some("Hash"),
            "the parameter `h` in another def must still narrow"
        );
    }

    /// STAGE 3b-1 (docs/notes/20260807-narrowing-stage3-spec.md § "3b-1"): the
    /// unmodeled-statement-form decision table. Every row was measured against
    /// the pinned reference from a fresh cwd with `--no-cache` — `Some(c)` means
    /// the reference FIRES and rigor-rs must record `c`, `None` means rigor-rs
    /// must record NOTHING (either the reference is silent, or the row is a
    /// deliberate coverage decline).
    ///
    /// The guard is always `return unless v.is_a?(String)` and the witnessed
    /// call is always `v.frobnicate_zzz`, so a row's whole point is the
    /// STATEMENT FORM the use sits in. 3b-1 mints no facts: every `Some` row is
    /// a use recorded under a fact stage 1-2 already established.
    #[test]
    fn class_narrowing_stage3b1_statement_form_matrix() {
        const G: &str = "  return unless v.is_a?(String)\n";
        let guarded = |body: &str| format!("def f(v, cache, obj, cond, list)\n{G}{body}end\n");
        // (row, source, expected narrowed class of the `frobnicate_zzz` call)
        let mut rows: Vec<(&str, String, Option<&str>)> = vec![
            // --- d4-d7: ivar / gvar / cvar / constant write VALUES -----------
            // DECLINED (see the arm's comment): the reference FIRES on all
            // four, but descending this one arm surfaced a pre-existing
            // carrier-fidelity FP over the standing sweep. The e-family rows
            // below pin the half that DID ship — facts survive these writes.
            ("d4", guarded("  @x = v.frobnicate_zzz\n"), None),
            ("d5", guarded("  $gx = v.frobnicate_zzz\n"), None),
            ("d6", guarded("  @@cx = v.frobnicate_zzz\n"), None),
            (
                "d7",
                // A constant write is only legal at top level / in a class body.
                "v = $stdin\nif v.is_a?(String)\n  XCONST_ZZZ = v.frobnicate_zzz\nend\n".to_string(),
                None,
            ),
            // The two SWEEP FPs that forced the d4-d7 decline, reduced. Both
            // are `narrow_class_other` carrier gaps: rigor-rs typed a `Logical`
            // (fp1) and a project-method return ending in one (fp2) as
            // `Dynamic[top]` where the reference produces a union, so the
            // reference's Dynamic-only gate declined and ours did not.
            //
            // The gap is CLOSED as of the carrier-fidelity fix
            // (docs/notes/20260808-narrowing-carrier-fidelity-fp.md): both
            // shapes now decline on the CARRIER, at the guard, in every
            // statement form — see `class_narrowing_carrier_fidelity_matrix`
            // and the rules-layer `coarse_carrier_narrowing_is_silent_end_to_end`.
            // These rows stay pinned here as the d4-d7 regression tripwire;
            // re-enabling that arm is its own slice and its own gate run.
            (
                "fp1",
                "def f(spec)\n  h = spec || {}\n  @spec = h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"
                    .to_string(),
                None,
            ),
            (
                "fp2",
                "class C\n  def config\n    c = mk\n    raise ArgumentError unless c.is_a?(Hash)\n\n    @config = c.frobnicate_zzz\n  end\n\n  def mk\n    unknown_zzz || {}\n  end\nend\n"
                    .to_string(),
                None,
            ),
            // --- d1/d10/d11: recovered op-assign carriers (bare local reads) --
            ("d1", guarded("  cache[v] ||= v.frobnicate_zzz\n"), Some("String")),
            ("d10a", guarded("  cache[v] += v.frobnicate_zzz\n"), Some("String")),
            ("d10b", guarded("  cache[v] &&= v.frobnicate_zzz\n"), Some("String")),
            ("d11", guarded("  obj.attr ||= v.frobnicate_zzz\n"), Some("String")),
            // d2 — the mastodon archetype: an op-assign whose RHS is a nested
            // conditional. The recovered carrier flattens the `if`, but the use
            // still records under the OUTER fact.
            (
                "d2",
                guarded("  cache[v] ||= if cond\n    v\n  else\n    v.frobnicate_zzz\n  end\n"),
                Some("String"),
            ),
            // --- d25/g6: a recovered carrier / `rescue` modifier as an RHS ----
            ("d25", guarded("  x = *v.frobnicate_zzz\n  x\n"), Some("String")),
            ("g6", guarded("  x = (v.frobnicate_zzz rescue nil)\n  x\n"), Some("String")),
            // --- d14-d17: literal containers ---------------------------------
            ("d14", guarded("  a, b = v.frobnicate_zzz, 1\n  [a, b]\n"), Some("String")),
            ("d15", guarded("  x = [v.frobnicate_zzz]\n  x\n"), Some("String")),
            ("d16", guarded("  x = { k: v.frobnicate_zzz }\n  x\n"), Some("String")),
            ("d17", guarded("  x = \"#{v.frobnicate_zzz}\"\n  x\n"), Some("String")),
            ("d17b", guarded("  x = :\"#{v.frobnicate_zzz}\"\n  x\n"), Some("String")),
            // --- d19/d20/f7/f8: begin/rescue/else/ensure ---------------------
            (
                "d19",
                guarded("  begin\n    v.frobnicate_zzz\n  rescue StandardError\n    nil\n  end\n"),
                Some("String"),
            ),
            (
                "d20",
                guarded(
                    "  x = begin\n    v.frobnicate_zzz\n  rescue StandardError\n    nil\n  end\n  x\n",
                ),
                Some("String"),
            ),
            (
                "f7",
                guarded("  begin\n    nil\n  rescue StandardError\n    v.frobnicate_zzz\n  end\n"),
                Some("String"),
            ),
            ("f8", guarded("  begin\n    nil\n  ensure\n    v.frobnicate_zzz\n  end\n"), Some("String")),
            // --- g1: loop PREDICATE (`while`/`until`/`for` collection) -------
            ("g1", guarded("  while v.frobnicate_zzz\n    break\n  end\n"), Some("String")),
            ("g1d", guarded("  until v.frobnicate_zzz\n    break\n  end\n"), Some("String")),
            // g1b/g1c: the `for` COLLECTION is evaluated ONCE, before the index
            // rebind — the reference fires even when the index is the narrowed
            // local itself.
            ("g1b", guarded("  for i in v.frobnicate_zzz\n    nil\n  end\n"), Some("String")),
            ("g1c", guarded("  for v in v.frobnicate_zzz\n    nil\n  end\n"), Some("String")),
            // --- f1-f4b: `&&`/`||` no longer clear ---------------------------
            ("f1", guarded("  cond && v.frobnicate_zzz\n"), Some("String")),
            ("f2", guarded("  cond || v.frobnicate_zzz\n"), Some("String")),
            ("f3", guarded("  g(cond && v.frobnicate_zzz)\n"), Some("String")),
            ("f4", guarded("  x = cond && v.frobnicate_zzz\n  x\n"), Some("String")),
            ("f4b", guarded("  cond && 1\n  v.frobnicate_zzz\n"), Some("String")),
            // --- e-family: fact SURVIVAL past the new statement arms ---------
            ("e1", guarded("  @x = 1\n  v.frobnicate_zzz\n"), Some("String")),
            ("e6", guarded("  $gx = 1\n  v.frobnicate_zzz\n"), Some("String")),
            ("e7", guarded("  @@cx = 1\n  v.frobnicate_zzz\n"), Some("String")),
            ("e3", guarded("  cache[:k] ||= 1\n  v.frobnicate_zzz\n"), Some("String")),
            ("e9", guarded("  cache\n  v.frobnicate_zzz\n"), Some("String")),
            // --- ALREADY CLOSED ON MASTER: assert they STAY closed -----------
            // A regression in any of these is otherwise silent (the spec's
            // "Where the evidence note's reading was wrong" correction).
            ("d8", guarded("  cache[v] = v.frobnicate_zzz\n"), Some("String")),
            ("d9", guarded("  obj.attr = v.frobnicate_zzz\n"), Some("String")),
            ("d12a", guarded("  @x ||= v.frobnicate_zzz\n"), Some("String")),
            ("d12b", guarded("  @x += v.frobnicate_zzz\n"), Some("String")),
            ("d13", guarded("  $gx ||= v.frobnicate_zzz\n"), Some("String")),
            ("d23", guarded("  yield v.frobnicate_zzz\n"), Some("String")),
            // d24 (`defined?(v.frobnicate_zzz)`) is RETIRED at the `v0.3.4` pin.
            // Upstream #318 stopped every engine walk descending into a
            // `defined?` operand — the call is not reachable code on either side
            // any more, so rigor-rs no longer lowers one and there is no node
            // left to carry a narrowing fact. The behaviour it used to pin now
            // lives in `rigor-parse`'s `defined_operand_drops_calls_but_keeps_
            // local_reads` and harness fixture 97.
            ("g2", guarded("  super(v.frobnicate_zzz)\n"), Some("String")),
            ("g5", guarded("  v.frobnicate_zzz rescue nil\n"), Some("String")),
            // --- DECLINES (each load-bearing) --------------------------------
            // f10a: the `for` index rebind is INVISIBLE in the arena and the
            // reference is SILENT here. This row is why `Loop` bodies are not
            // descended at all — `Node::Loop` cannot tell `for` from `while`.
            ("f10a", guarded("  for v in list\n    v.frobnicate_zzz\n  end\n"), None),
            // f10b/d21/g3: `for` with a distinct index, a `while` body and a
            // `break` operand — the reference FIRES on all three; declined as
            // collateral of the f10a decline. Stage 3b-2.
            ("f10b", guarded("  for i in list\n    v.frobnicate_zzz\n  end\n"), None),
            ("d21", guarded("  while cond\n    v.frobnicate_zzz\n  end\n"), None),
            ("g3", guarded("  while cond\n    break v.frobnicate_zzz\n  end\n"), None),
            // post1/post2: the reference KEEPS the fact past a `begin`/loop;
            // we clear (unprobed at spec time ⇒ decline, a strict subset).
            (
                "post1",
                guarded("  begin\n    nil\n  rescue StandardError\n    nil\n  end\n  v.frobnicate_zzz\n"),
                None,
            ),
            ("post2", guarded("  while cond\n    nil\n  end\n  v.frobnicate_zzz\n"), None),
            // rescuebind: `rescue => v` REBINDS the narrowed local with no
            // `LocalVariableWrite` node. The reference narrows it to the
            // EXCEPTION class (`for StandardError`), so keeping the `String`
            // fact would be a live FP.
            (
                "rescuebind",
                guarded("  begin\n    nil\n  rescue StandardError => v\n    v.frobnicate_zzz\n  end\n"),
                None,
            ),
            // c5: a `while` PREDICATE mints nothing for the body (reference
            // silent — evidence note).
            ("c5", "def f(v)\n  while v.is_a?(String)\n    v.frobnicate_zzz\n  end\nend\n".to_string(), None),
            // c2a/c2c: `Logical` MINTING stays out of slice (stage 3a-2) in
            // statement AND argument position.
            ("c2a", "def f(v)\n  v.is_a?(String) && v.frobnicate_zzz\nend\n".to_string(), None),
            ("c2c", "def f(v)\n  g(v.is_a?(String) && v.frobnicate_zzz)\nend\n".to_string(), None),
            // blk4/blk6: a BLOCK inside a literal container or a loop predicate
            // is not descended (container elements and a loop predicate are
            // EXPRESSION position). The reference does narrow there — recorded
            // coverage gaps, not FPs.
            (
                "blk4",
                "def f(v)\n  x = [[1].map { v.is_a?(String) ? v.frobnicate_zzz : v }]\n  x\nend\n"
                    .to_string(),
                None,
            ),
            (
                "blk6",
                "def f(v)\n  while [1].map { v.is_a?(String) ? v.frobnicate_zzz : v }\n    break\n  end\nend\n"
                    .to_string(),
                None,
            ),
            // --- INVALIDATION through the new arms ---------------------------
            // A rebind nested in an ivar-write value threads IMMEDIATELY.
            ("inv1", guarded("  @x = (v = cond)\n  v.frobnicate_zzz\n"), None),
            // A conditionally-executed rebind in a statement `&&` kills by span.
            ("inv2", guarded("  cond && (v = cache)\n  v.frobnicate_zzz\n"), None),
            // A rebind in an earlier array element kills the later sibling.
            ("inv3", guarded("  x = [v = cache, v.frobnicate_zzz]\n  x\n"), None),
            // A mutator inside a `begin` body kills the following use.
            (
                "inv4",
                guarded("  begin\n    v.merge!(a: 1)\n    v.frobnicate_zzz\n  rescue StandardError\n    nil\n  end\n"),
                None,
            ),
        ];
        rows.sort_by_key(|(row, _, _)| *row);
        for (row, src, expected) in &rows {
            let (ast, snaps) = class_snaps(src.as_bytes());
            let got = snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str);
            assert_eq!(got, *expected, "stage 3b-1 matrix row {row}\n--- source ---\n{src}");
        }
    }

    /// STAGE 3a-1 — compound predicate analysis (`&&` / `||` / `!`) and the
    /// both-direction termination propagation
    /// (docs/notes/20260807-narrowing-stage3-spec.md).
    ///
    /// Every row is oracle-measured against the pinned reference `v0.3.1` from
    /// a fresh cwd with `--no-cache` and the checkout plugin path pinned.
    /// `Some(c)` — the reference FIRES and rigor-rs must record `c`. `None` —
    /// rigor-rs must record nothing, either because the reference is SILENT
    /// (a would-be false positive — the control rows) or because the decline
    /// costs coverage the reference has (a strict subset). The row name is the
    /// probe name; see the note's "3a-1 BUILT" outcome table.
    #[test]
    fn class_narrowing_stage3a1_compound_predicate_matrix() {
        const USE: &str = "v.frobnicate_zzz";
        const G: &str = "v.is_a?(String)";
        let f = |body: &str| format!("def f(v, w, a, b)\n{body}\nend\n");
        let mut rows: Vec<(&str, String, Option<&str>)> = vec![
            // ---- c1: one recognised conjunct narrows the TRUTHY edge --------
            ("c1a", f(&format!("  {USE} if {G} && v.length > 2")), Some("String")),
            ("c1b", f(&format!("  if v.frozen? && {G}\n    {USE}\n  end")), Some("String")),
            (
                "c1c",
                f(&format!("  if v.frozen? && {G} && v.length > 2\n    {USE}\n  end")),
                Some("String"),
            ),
            ("x_and_kw", f(&format!("  if {G} and v.length > 2\n    {USE}\n  end")), Some("String")),
            // c1g control: the falsey edge of a plain `&&` stays UNNARROWED.
            (
                "c1g",
                f(&format!("  if {G} && v.length > 2\n    1\n  else\n    {USE}\n  end")),
                None,
            ),
            // f12 control: an unrecognised `||` disjunct kills the truthy join.
            ("f12", f(&format!("  if {G} || {G} || v.nil?\n    {USE}\n  end")), None),
            // ---- c4: `!` swaps the edges ------------------------------------
            ("c4d", f(&format!("  if !{G}\n    1\n  else\n    {USE}\n  end")), Some("String")),
            ("c4f", f(&format!("  unless !{G}\n    {USE}\n  end")), Some("String")),
            ("x_not_kw", f(&format!("  if not {G}\n    1\n  else\n    {USE}\n  end")), Some("String")),
            ("x_double_bang", f(&format!("  if !!{G}\n    {USE}\n  end")), Some("String")),
            (
                "x_bang_of_and",
                f(&format!("  if !({G} && v.length > 2)\n    1\n  else\n    {USE}\n  end")),
                Some("String"),
            ),
            ("u_bang_nonguard", f(&format!("  if !v.nil?\n    {USE}\n  end")), None),
            // ---- termination propagation, BOTH directions -------------------
            ("c4a", f(&format!("  return if !{G}\n  {USE}")), Some("String")),
            ("c4b", f(&format!("  if !{G}\n    return\n  end\n  {USE}")), Some("String")),
            ("f22", f(&format!("  raise \"x\" if !{G}\n  {USE}")), Some("String")),
            ("f16", f(&format!("  if {G}\n    1\n  else\n    return\n  end\n  {USE}")), Some("String")),
            ("t_c1d", f(&format!("  return unless {G} && v.length > 2\n  {USE}")), Some("String")),
            ("t_c1d_or", f(&format!("  return if !{G} || v.nil?\n  {USE}")), Some("String")),
            (
                "t_unless_and",
                f(&format!("  unless {G} && v.length > 2\n    return\n  end\n  {USE}")),
                Some("String"),
            ),
            // CONTROL, measured reference-silent: both branches terminate, so
            // the statements after are unreachable. Propagating either map here
            // would be a live FP.
            ("t_both_terminate", f(&format!("  if !{G}\n    return\n  else\n    return\n  end\n  {USE}")), None),
            // A write inside the conditional's span declines the propagation
            // (the reference still narrows — coverage cost, not an FP).
            ("t_write_in_span", f(&format!("  if !{G}\n    v = 1\n    return\n  end\n  {USE}")), None),
            // A rebind AFTER the propagated guard kills the fact (the reference
            // fires a DIFFERENT diagnostic there, `for 1`).
            ("t_write_after_guard", f(&format!("  return if !{G}\n  v = 1\n  {USE}")), None),
            ("t_elsif", f(&format!("  if v.nil?\n    1\n  elsif !{G}\n    2\n  else\n    {USE}\n  end")), Some("String")),
            // ---- (a) `&&` falsey JOIN with two guards -----------------------
            (
                "a_same_local_disjoint_else",
                f(&format!("  if {G} && v.is_a?(Hash)\n    1\n  else\n    {USE}\n  end")),
                None,
            ),
            (
                "a_two_locals_else",
                f(&format!("  if {G} && w.is_a?(Hash)\n    1\n  else\n    {USE}\n  end")),
                None,
            ),
            // The SPEC CORRECTION: "b wins a same-local collision" would fire
            // `for Hash` here; the reference reaches `Bot` and is SILENT.
            (
                "a_same_local_disjoint_then",
                f(&format!("  if {G} && v.is_a?(Hash)\n    {USE}\n  end")),
                None,
            ),
            // ---- (b) `&&` falsey join, SAME class both sides ----------------
            (
                "b2_and_bang_same",
                f(&format!("  if !{G} && !{G}\n    1\n  else\n    {USE}\n  end")),
                Some("String"),
            ),
            // Different classes join to a UNION in the reference (`Hash |
            // String`) — declined until 3a-4, coverage cost only.
            (
                "b2_and_bang_diff",
                f(&format!("  if !{G} && !v.is_a?(Hash)\n    1\n  else\n    {USE}\n  end")),
                None,
            ),
            (
                "b2_and_bang_two_locals",
                f(&format!("  if !{G} && !w.is_a?(Hash)\n    1\n  else\n    {USE}\n  end")),
                None,
            ),
            ("b_same_class_then", f(&format!("  if {G} && {G}\n    {USE}\n  end")), Some("String")),
            ("b_same_class_else", f(&format!("  if {G} && {G}\n    1\n  else\n    {USE}\n  end")), None),
            // ---- `||` edge algebra ------------------------------------------
            ("x_or_same_class", f(&format!("  if {G} || {G}\n    {USE}\n  end")), Some("String")),
            // A `||` of DIFFERENT classes is a union in the reference — 3a-4.
            ("x_or_diff_class", f(&format!("  if {G} || v.is_a?(Hash)\n    {USE}\n  end")), None),
            (
                "x_or_falsey_bang",
                f(&format!("  if !{G} || v.nil?\n    1\n  else\n    {USE}\n  end")),
                Some("String"),
            ),
            (
                "o_or_none_then_bang",
                f(&format!("  if v.nil? || !{G}\n    1\n  else\n    {USE}\n  end")),
                Some("String"),
            ),
            (
                "o_or_bang_bang_diff",
                f(&format!("  if !{G} || !v.is_a?(Hash)\n    1\n  else\n    {USE}\n  end")),
                None,
            ),
            (
                "o_and_bang_then_cond",
                f(&format!("  if !{G} && v.nil?\n    1\n  else\n    {USE}\n  end")),
                None,
            ),
            (
                "u_and_nested_or",
                f(&format!("  if ({G} || v.is_a?(Hash)) && v.length > 2\n    {USE}\n  end")),
                None,
            ),
            ("u_or_nested_and", f(&format!("  if ({G} && v.length > 2) || v.nil?\n    {USE}\n  end")), None),
            (
                "u_and_or_falsey",
                f(&format!("  if !{G} && (v.nil? || v.frozen?)\n    1\n  else\n    {USE}\n  end")),
                None,
            ),
            ("n_unless_bang", f(&format!("  unless !{G}\n    {USE}\n  end")), Some("String")),
            (
                "n_unless_and_else",
                f(&format!("  unless {G} && v.length > 2\n    1\n  else\n    {USE}\n  end")),
                Some("String"),
            ),
            (
                "n_unless_or_bang",
                f(&format!("  unless !{G} || v.nil?\n    1\n  else\n    {USE}\n  end")),
                None,
            ),
            // Two locals, both narrowed by the same compound predicate.
            (
                "a_two_locals_then",
                f(&format!("  if {G} && w.is_a?(Hash)\n    {USE}\n    w.other_zzz\n  end")),
                Some("String"),
            ),
            // ---- position: `if`/ternary narrows in EVERY position -----------
            ("q_ternary_bang", f(&format!("  g(!{G} ? 1 : {USE})")), Some("String")),
            ("q_ternary_and", f(&format!("  g({G} && v.length > 2 ? {USE} : 1)")), Some("String")),
            // ---- DECLINES, each measured reference-FIRING (coverage cost) ---
            // `===` narrows in the reference; this slice keeps it Bot-only.
            ("e3_case_eq_bang", f(&format!("  if !(String === v)\n    1\n  else\n    {USE}\n  end")), None),
            // A same-local `&&` collision in a SUBCLASS relation: the reference
            // keeps the more specific class; review R3 drops the fact.
            ("s_num_then_int", f("  if v.is_a?(Numeric) && v.is_a?(Integer)\n    v.frobnicate_zzz\n  end"), None),
            ("s_int_then_num", f("  if v.is_a?(Integer) && v.is_a?(Numeric)\n    v.frobnicate_zzz\n  end"), None),
            // (d) the carrier ALLOW-list gate stays in force PER LOCAL: a local
            // bound from a `Logical` declines even on the new falsey edge.
            (
                "d_logical_carrier",
                f("  v2 = a || b\n  if !v2.is_a?(String)\n    1\n  else\n    v2.frobnicate_zzz\n  end"),
                None,
            ),
            // …and its control: the same shape on a PARAMETER narrows.
            ("d_param_control", f(&format!("  if !{G}\n    1\n  else\n    {USE}\n  end")), Some("String")),
            // A conjunct narrowing only the OTHER local leaves this one alone.
            (
                "x_and_two_locals_falsey_only_v",
                f(&format!("  if {G} && w.is_a?(String)\n    1\n  else\n    {USE}\n  end")),
                None,
            ),
            // ---- the CONJUNCT-INTERFERENCE battery -------------------------
            // 54 measured `X && guard` / `guard && X` rows (probes4/probes5)
            // found exactly three FP mechanisms; everything else is inert. The
            // inert representatives are pinned as POSITIVES so a future
            // over-broad interference rule cannot quietly delete them.
            ("L_len_cmp", f(&format!("  if v.length > 2 && {G}\n    {USE}\n  end")), Some("String")),
            ("L_frozen", f(&format!("  if v.frozen? && {G}\n    {USE}\n  end")), Some("String")),
            ("R_frozen", f(&format!("  if {G} && v.frozen?\n    {USE}\n  end")), Some("String")),
            ("L_bare_v", f(&format!("  if v && {G}\n    {USE}\n  end")), Some("String")),
            ("R_bare_v", f(&format!("  if {G} && v\n    {USE}\n  end")), Some("String")),
            ("L_bare_w", f(&format!("  if w && {G}\n    {USE}\n  end")), Some("String")),
            ("L_respond", f(&format!("  if v.respond_to?(:foo) && {G}\n    {USE}\n  end")), Some("String")),
            ("R_respond", f(&format!("  if {G} && v.respond_to?(:foo)\n    {USE}\n  end")), Some("String")),
            ("L_empty", f(&format!("  if v.empty? && {G}\n    {USE}\n  end")), Some("String")),
            ("L_cmp_ge", f(&format!("  if v >= 2 && {G}\n    {USE}\n  end")), Some("String")),
            ("R_cmp_ge", f(&format!("  if {G} && v >= 2\n    {USE}\n  end")), Some("String")),
            ("L_between", f(&format!("  if v.between?(1, 3) && {G}\n    {USE}\n  end")), Some("String")),
            ("L_startwith", f(&format!("  if v.start_with?(\"a\") && {G}\n    {USE}\n  end")), Some("String")),
            ("L_call_arg", f(&format!("  if w.include?(v) && {G}\n    {USE}\n  end")), Some("String")),
            ("L_eq_one", f(&format!("  if v == 1 && {G}\n    {USE}\n  end")), Some("String")),
            ("R_eq_one", f(&format!("  if {G} && v == 1\n    {USE}\n  end")), Some("String")),
            ("L_bang_nilq", f(&format!("  if !v.nil? && {G}\n    {USE}\n  end")), Some("String")),
            ("R_bang_nilq", f(&format!("  if {G} && !v.nil?\n    {USE}\n  end")), Some("String")),
            ("L_neq_nil", f(&format!("  if v != nil && {G}\n    {USE}\n  end")), Some("String")),
            ("R_neq_nil", f(&format!("  if {G} && v != nil\n    {USE}\n  end")), Some("String")),
            ("other_nilq", f(&format!("  if w.nil? && {G}\n    {USE}\n  end")), Some("String")),
            ("matchop_keep", f(&format!("  if v =~ /a/ && {G}\n    {USE}\n  end")), Some("String")),
            ("R_caseeq_same", f(&format!("  if String === v && {G}\n    {USE}\n  end")), Some("String")),
            // …and the three FP mechanisms, each measured reference-SILENT.
            ("L_nilq", f(&format!("  if v.nil? && {G}\n    {USE}\n  end")), None),
            ("R_nilq", f(&format!("  if {G} && v.nil?\n    {USE}\n  end")), None),
            ("mid_nilq", f(&format!("  if v.frozen? && v.nil? && {G}\n    {USE}\n  end")), None),
            ("R_eq_nil", f(&format!("  if {G} && v == nil\n    {USE}\n  end")), None),
            // `== nil` on the LEFT is reference-FIRING; declining it is the
            // coverage price of the one rule that closes `R_eq_nil`.
            ("L_eq_nil", f(&format!("  if v == nil && {G}\n    {USE}\n  end")), None),
            ("L_caseeq", f(&format!("  if String === v && v.is_a?(Hash)\n    {USE}\n  end")), None),
            ("R_caseeq", f(&format!("  if v.is_a?(Hash) && String === v\n    {USE}\n  end")), None),
            // A named-capture `=~` binds `v` invisibly: decline the predicate.
            (
                "matchwrite",
                "def f(s)\n  if /(?<v>a)/ =~ s && v.is_a?(Hash)\n    v.frobnicate_zzz\n  end\nend\n".to_string(),
                None,
            ),
            // …including when the reference AGREES with the narrowing — the
            // decline is uniform because the binding is arena-invisible.
            (
                "matchwrite_str",
                "def f(s)\n  if /(?<v>a)/ =~ s && v.is_a?(String)\n    v.frobnicate_zzz\n  end\nend\n".to_string(),
                None,
            ),
            // A `||` whose disjuncts pin different classes joins to a union.
            ("or_nilq", f(&format!("  if v.nil? || {G}\n    {USE}\n  end")), None),
            (
                "bang_nilq_falsey",
                f(&format!("  if !v.nil? && !{G}\n    1\n  else\n    {USE}\n  end")),
                None,
            ),
            (
                "nilq_falsey",
                f(&format!("  if v.nil? && !{G}\n    1\n  else\n    {USE}\n  end")),
                None,
            ),
        ];
        rows.sort_by_key(|(row, _, _)| *row);
        for (row, src, expected) in &rows {
            let (ast, snaps) = class_snaps(src.as_bytes());
            let got = snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str);
            assert_eq!(got, *expected, "stage 3a-1 matrix row {row}\n--- source ---\n{src}");
        }
    }

    /// STAGE 3a-1 follow-up — `branch_terminates` recognising `next` / `break`
    /// (docs/notes/20260807-narrowing-stage3-spec.md, the 2026-08-08 section).
    ///
    /// Same convention as the 3a-1 matrix: `Some(c)` — the reference FIRES and
    /// rigor-rs must record `c`; `None` — rigor-rs must record nothing, either
    /// because the reference is SILENT (a would-be false positive) or because
    /// the decline costs coverage the reference has (a strict subset). Every row
    /// is oracle-measured against the pinned reference `v0.3.1` from a fresh cwd
    /// with `--no-cache` and the checkout plugin path pinned; the row name is
    /// the probe name in the note's table.
    #[test]
    fn class_narrowing_next_break_termination_matrix() {
        const USE: &str = "v.frobnicate_zzz";
        const G: &str = "v.is_a?(String)";
        // A method wrapper whose body runs INSIDE an `each` block, which is
        // where `next`/`break` are legal Ruby.
        let blk = |body: &str| format!("def f(v, w, xs)\n  xs.each do |x|\n{body}\n  end\nend\n");
        let mut rows: Vec<(&str, String, Option<&str>)> = vec![
            // ---- the archetype, both jumps, both carriers -------------------
            ("p1_next_block", blk(&format!("    next unless {G}\n    {USE}")), Some("String")),
            ("p2_break_block", blk(&format!("    break unless {G}\n    {USE}")), Some("String")),
            (
                "p3_next_blockparam",
                "def f(xs)\n  xs.each do |x|\n    next unless x.is_a?(String)\n    x.frobnicate_zzz\n  end\nend\n".to_string(),
                Some("String"),
            ),
            (
                "p3b_break_blockparam",
                "def f(xs)\n  xs.each do |x|\n    break unless x.is_a?(String)\n    x.frobnicate_zzz\n  end\nend\n".to_string(),
                Some("String"),
            ),
            // `!` swap: `next if !guard` carries the falsey map.
            ("p11_bang_next_if", blk(&format!("    next if !{G}\n    {USE}")), Some("String")),
            // The compound census shape (`next unless job && x.is_a?(Hash)`).
            ("p17_next_compound", blk(&format!("    next unless w && {G}\n    {USE}")), Some("String")),
            ("q13_next_or_guard", blk(&format!("    next if !{G} || v.empty?\n    {USE}")), Some("String")),
            ("r9_kind_of", blk("    next unless v.kind_of?(String)\n    v.frobnicate_zzz"), Some("String")),
            ("r10_instance_of", blk("    next unless v.instance_of?(String)\n    v.frobnicate_zzz"), Some("String")),
            // The jump need only be the branch's LAST statement …
            (
                "q6_next_not_last",
                blk(&format!("    unless {G}\n      w.warn('x')\n      next\n    end\n    {USE}")),
                Some("String"),
            ),
            // … and a jump followed by dead code does NOT terminate the branch
            // (`.last` is not a `next`) — the reference is silent too.
            (
                "q7_next_first_not_last",
                blk(&format!("    unless {G}\n      next\n      w.warn('x')\n    end\n    {USE}")),
                None,
            ),
            // The block need not be a loop at all — the recognition is syntactic
            // on the reference and here.
            (
                "q16_define_method",
                "class K\n  define_method(:f) do |v|\n    next unless v.is_a?(String)\n    v.frobnicate_zzz\n  end\nend\n".to_string(),
                Some("String"),
            ),
            (
                "q15_lambda_rhs",
                "def f(v)\n  g = lambda do |x|\n    next unless v.is_a?(String)\n    v.frobnicate_zzz\n  end\n  g\nend\n".to_string(),
                Some("String"),
            ),
            // ---- CONTROLS: reference-SILENT, so recording would be an FP ----
            // The use BEFORE the guard.
            ("p4_use_before", blk(&format!("    {USE}\n    next unless {G}")), None),
            // `next if guard` — the truthy edge terminates and the falsey map of
            // an atomic class guard is EMPTY.
            ("p10_next_if_positive", blk(&format!("    next if {G}\n    {USE}")), None),
            // A rebind inside the conditional's span, and one after the guard.
            ("q3_write_in_span", blk(&format!("    next unless (v = w).is_a?(String)\n    {USE}")), None),
            ("q17_rebind_then_use", blk(&format!("    next unless {G}\n    v = w\n    {USE}")), None),
            // A fact minted inside a block NEVER escapes it (`join_cenv` keeps
            // only `Bot`) — for `next`, for `break`, and out of a NESTED block.
            (
                "p9_after_block",
                format!("def f(v, xs)\n  xs.each do |x|\n    next unless {G}\n  end\n  {USE}\nend\n"),
                None,
            ),
            (
                "p9b_after_block_break",
                format!("def f(v, xs)\n  xs.each do |x|\n    break unless {G}\n  end\n  {USE}\nend\n"),
                None,
            ),
            (
                "p13_nested_block",
                format!("def f(v, xs, ys)\n  xs.each do |x|\n    ys.each do |y|\n      next unless {G}\n    end\n    {USE}\n  end\nend\n"),
                None,
            ),
            // …nor past an inner `if` inside the block (the join clears it).
            ("q10_after_nested_if", blk(&format!("    if w\n      next unless {G}\n    end\n    {USE}")), None),
            // A block in ARGUMENT position is not descended at all.
            (
                "r7_block_arg_position",
                format!("def f(v, xs, sink)\n  sink.push(xs.each do |x|\n    next unless {G}\n    {USE}\n  end)\nend\n"),
                None,
            ),
            // ---- DECLINES: the reference fires, we do not (strict subset) ----
            // A `while`/`until` BODY is never descended (stage 3b-2).
            (
                "p5_next_in_while",
                format!("def f(v, n)\n  while n > 0\n    next unless {G}\n    {USE}\n  end\nend\n"),
                None,
            ),
            // `next`/`break` WITH a value keeps the recovered-children carrier,
            // so it is not tagged as a jump.
            (
                "p16_next_with_value",
                format!("def f(v, xs)\n  xs.map do |x|\n    next 0 unless {G}\n    {USE}\n  end\nend\n"),
                None,
            ),
            // `throw` / `exit` / `abort` / `fail` / `redo` all terminate on the
            // reference; only `raise` is ported (out of this slice).
            ("p15_throw", blk(&format!("    throw :done unless {G}\n    {USE}")), None),
            ("p8_redo", blk(&format!("    redo unless {G}\n    {USE}")), None),
            // BOTH branches jumping: the reference propagates the TRUTHY map
            // (`eval_if:495` needs only a present then-branch), we decline —
            // `truthy_terminates != falsey_terminates` is the subset rule.
            (
                "q19_break_both_terminate",
                blk(&format!("    if {G}\n      break\n    else\n      break\n    end\n    {USE}")),
                None,
            ),
            // A `case`/`when` clause ending in `next`: `class_flow_case` has no
            // termination propagation (stage 3a-4).
            (
                "q11_next_case_when",
                blk(&format!("    case v\n    when Integer then nil\n    else next\n    end\n    {USE}")),
                None,
            ),
            // The carrier ALLOW-list still declines per local (PR #72).
            (
                "q4_coarse_carrier",
                format!("def f(xs, a, b)\n  v = a || b\n  xs.each do |x|\n    next unless {G}\n    {USE}\n  end\nend\n"),
                None,
            ),
        ];
        rows.sort_by_key(|(row, _, _)| *row);
        for (row, src, expected) in &rows {
            let (ast, snaps) = class_snaps(src.as_bytes());
            let got = snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str);
            assert_eq!(got, *expected, "next/break termination row {row}\n--- source ---\n{src}");
        }
    }

    /// STAGE 3a-1 × PR #73: the disjoint-guard `Bot` collapse composes with the
    /// new compound-predicate edges. `true` — the call site must be DEAD (the
    /// reference is silent because its carrier collapsed to `Bot`); `false` —
    /// it must stay live (the reference FIRES, so suppressing would lose a real
    /// diagnostic). Every row is a live false positive on master except the
    /// `must_fire_*` controls, which pin the anti-over-suppression half.
    #[test]
    fn class_narrowing_stage3a1_bot_composition_matrix() {
        let rows: &[(&str, &str, bool)] = &[
            // `!guard` + termination: the falsey map carries the guard past the
            // `return`, and the Array carrier collapses against Hash.
            ("c_bang_return", "def f\n  v = [1, 2]\n  return if !v.is_a?(Hash)\n  v.frobnicate_zzz\nend\n", true),
            // `===` under `!`, same shape.
            ("e3_case_eq_bot", "def f\n  v = [1, 2]\n  return if !(Hash === v)\n  v.frobnicate_zzz\nend\n", true),
            // A `&&` whose recognised conjunct collapses.
            ("k_bot_and_cond", "def f\n  v = [1, 2]\n  if v.is_a?(Hash) && v.frozen?\n    v.frobnicate_zzz\n  end\nend\n", true),
            // An `||` truthy JOIN where BOTH disjuncts collapse — the union
            // `Hash | String` is `Bot | Bot`.
            ("k_bot_or_bot", "def f\n  v = [1, 2]\n  if v.is_a?(Hash) || v.is_a?(String)\n    v.frobnicate_zzz\n  end\nend\n", true),
            ("k_bot_or_same", "def f\n  v = [1, 2]\n  if v.is_a?(Hash) || v.is_a?(Hash)\n    v.frobnicate_zzz\n  end\nend\n", true),
            // A same-local `&&` collision on a PRECISE carrier: the second
            // conjunct collapses what the first left alone, in either order.
            ("p_and_collide_precise", "def f\n  v = [1, 2]\n  if v.is_a?(Array) && v.is_a?(Hash)\n    v.frobnicate_zzz\n  end\nend\n", true),
            ("p_and_collide_precise2", "def f\n  v = [1, 2]\n  if v.is_a?(Hash) && v.is_a?(Array)\n    v.frobnicate_zzz\n  end\nend\n", true),
            ("p_bang_and_precise", "def f\n  v = [1, 2]\n  return if !(v.is_a?(Hash) && v.nil?)\n  v.frobnicate_zzz\nend\n", true),
            // ---- must-still-fire controls (reference FIRES) -----------------
            // An unrecognised disjunct empties the `||` truthy join.
            ("must_fire_or_cond", "def f\n  v = [1, 2]\n  if v.is_a?(Hash) || v.frozen?\n    v.frobnicate_zzz\n  end\nend\n", false),
            // The truthy edge of `!guard` carries NOTHING.
            ("must_fire_bang_then", "def f\n  v = [1, 2]\n  if !v.is_a?(Hash)\n    v.frobnicate_zzz\n  end\nend\n", false),
            // The falsey edge of a plain `&&` carries nothing either.
            ("must_fire_else_of_and", "def f\n  v = [1, 2]\n  if v.is_a?(Hash) && v.frozen?\n    1\n  else\n    v.frobnicate_zzz\n  end\nend\n", false),
            // An `&&` falsey join that drops leaves the else edge un-collapsed.
            ("must_fire_bang_and_else", "def f\n  v = [1, 2]\n  if !v.is_a?(Hash) && v.frozen?\n    1\n  else\n    v.frobnicate_zzz\n  end\nend\n", false),
            // ---- the `nil?` / `== nil` collapse -----------------------------
            // `narrow_nil` (`narrowing.rb:90`) sends every precise carrier to
            // `Bot`, so a nil test on an Array-literal local silences its calls.
            ("nilq_bot_then", "def f\n  v = [1, 2]\n  if v.nil?\n    v.frobnicate_zzz\n  end\nend\n", true),
            ("nilq_bot_return", "def f\n  v = [1, 2]\n  return unless v.nil?\n  v.frobnicate_zzz\nend\n", true),
            ("eqnil_bot_then", "def f\n  v = [1, 2]\n  if v == nil\n    v.frobnicate_zzz\n  end\nend\n", true),
            ("nilq_bot_and", "def f\n  v = [1, 2]\n  if v.nil? && v.frozen?\n    v.frobnicate_zzz\n  end\nend\n", true),
            // …and its must-still-fire twin: the FALSEY edge of `nil?` is
            // `narrow_non_nil`, which leaves a precise carrier alone.
            ("must_fire_nilq_else", "def f\n  v = [1, 2]\n  if v.nil?\n    1\n  else\n    v.frobnicate_zzz\n  end\nend\n", false),
        ];
        for (row, src, dead) in rows {
            let ast = lower_src(src.as_bytes());
            let index = CoreIndex::new();
            let source = SourceIndex::build(&ast, &index);
            let scopes = lexical_scopes(&ast);
            let typer = Typer::with_source(&index, &source).with_lexical_scopes(&scopes);
            let mut i = Interner::new();
            let pass = typer.class_narrowing_pass(&ast, &mut i);
            let call = call_named(&ast, "frobnicate_zzz");
            assert_eq!(pass.dead.contains(&call), *dead, "3a-1 Bot row {row}");
        }
    }

    /// SEQUENTIAL disjoint/refining guards — the [`Typer::apply_guards`]
    /// sequential-guard meet plus the pre-join propagation in
    /// [`Typer::class_flow_if`]. Every row is oracle-measured (pin `v0.3.1`,
    /// 2026-08-08 `seqprobe` matrix): `Some(class)` — the reference fires
    /// `for <class>` and the snapshot must carry it; `(None, true)` — the
    /// reference's meet reached `Bot` and the call site must be DEAD (every
    /// `dead: true` row except the harness-shape controls was a live FP on
    /// master); `(None, false)` — a recorded DECLINE: the reference fires but
    /// we drop the fact (never an FP, only coverage).
    #[test]
    fn class_narrowing_sequential_guard_meet_matrix() {
        let rows: &[(&str, &str, Option<&str>, bool)] = &[
            // ---- the FP family: disjoint sequential pairs reach Bot ---------
            ("seq_disjoint", "def f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(Hash)\n  v.frobnicate_zzz\nend\n", None, true),
            ("seq_raise", "def f(v)\n  raise ArgumentError unless v.is_a?(String)\n  raise ArgumentError unless v.is_a?(Hash)\n  v.frobnicate_zzz\nend\n", None, true),
            ("seq_bang", "def f(v)\n  return unless v.is_a?(String)\n  return if !v.is_a?(Hash)\n  v.frobnicate_zzz\nend\n", None, true),
            // A third guard cannot revive the collapsed local.
            ("seq_third", "def f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(Hash)\n  return unless v.is_a?(String)\n  v.frobnicate_zzz\nend\n", None, true),
            // `instance_of?` collapses on a bare name mismatch — even a
            // SUBCLASS name (the reference tests `context.exact` before the
            // hierarchy; both probes are reference-silent).
            ("seq_exact_disjoint", "def f(v)\n  return unless v.is_a?(String)\n  return unless v.instance_of?(Hash)\n  v.frobnicate_zzz\nend\n", None, true),
            ("seq_exact_subclass", "def f(v)\n  return unless v.is_a?(Numeric)\n  return unless v.instance_of?(Integer)\n  v.frobnicate_zzz\nend\n", None, true),
            // Non-mintable second guards feed the same meet: `===`, `nil?`.
            ("seq_caseeq_disjoint", "def f(v)\n  return unless v.is_a?(String)\n  return unless Hash === v\n  v.frobnicate_zzz\nend\n", None, true),
            ("seq_nilq", "def f(v)\n  return unless v.is_a?(String)\n  return unless v.nil?\n  v.frobnicate_zzz\nend\n", None, true),
            // An `||` union whose EVERY member is disjoint.
            ("seq_or_disjoint", "def f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(Hash) || v.is_a?(Array)\n  v.frobnicate_zzz\nend\n", None, true),
            // The `next` spelling inside a block (the r8 shape).
            ("blk_next_disjoint", "def f(xs)\n  xs.each do |v|\n    next unless v.is_a?(String)\n    next unless v.is_a?(Hash)\n    v.frobnicate_zzz\n  end\nend\n", None, true),
            // A NON-terminating second conditional: the branch-edge meet.
            ("br_disjoint", "def f(v)\n  return unless v.is_a?(String)\n  if v.is_a?(Hash)\n    v.frobnicate_zzz\n  end\nend\n", None, true),
            ("br_exact_disjoint", "def f(v)\n  return unless v.is_a?(String)\n  if v.instance_of?(Hash)\n    v.frobnicate_zzz\n  end\nend\n", None, true),
            // A use between the guards fires; the use AFTER the collapse is
            // dead (the reference reports only the first).
            ("seq_use_between", "def f(v)\n  return unless v.is_a?(String)\n  v.frobnicate_yyy\n  return unless v.is_a?(Hash)\n  v.frobnicate_zzz\nend\n", None, true),
            // ---- refinement / no-op: the reference FIRES and so must we -----
            // A subclass guard refines to the MORE SPECIFIC class …
            ("seq_subclass", "def f(v)\n  return unless v.is_a?(Numeric)\n  return unless v.is_a?(Integer)\n  v.frobnicate_zzz\nend\n", Some("Integer"), false),
            // … a superclass guard is a no-op (the carrier stays) …
            ("seq_superclass", "def f(v)\n  return unless v.is_a?(Integer)\n  return unless v.is_a?(Numeric)\n  v.frobnicate_zzz\nend\n", Some("Integer"), false),
            // … a same-class re-guard keeps, `===` included …
            ("seq_same", "def f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(String)\n  v.frobnicate_zzz\nend\n", Some("String"), false),
            ("seq_caseeq_same", "def f(v)\n  return unless v.is_a?(String)\n  return unless String === v\n  v.frobnicate_zzz\nend\n", Some("String"), false),
            // … and `===` refines too (updating an existing fact is not
            // minting — the reference fires `for Integer`).
            ("seq_caseeq_subclass", "def f(v)\n  return unless v.is_a?(Numeric)\n  return unless Integer === v\n  v.frobnicate_zzz\nend\n", Some("Integer"), false),
            ("blk_next_subclass", "def f(xs)\n  xs.each do |v|\n    next unless v.is_a?(Numeric)\n    next unless v.is_a?(Integer)\n    v.frobnicate_zzz\n  end\nend\n", Some("Integer"), false),
            // Refinement through a branch edge (non-terminating conditional).
            ("br_subclass", "def f(v)\n  return unless v.is_a?(Numeric)\n  if v.is_a?(Integer)\n    v.frobnicate_zzz\n  end\nend\n", Some("Integer"), false),
            ("br_superclass", "def f(v)\n  return unless v.is_a?(Integer)\n  if v.is_a?(Numeric)\n    v.frobnicate_zzz\n  end\nend\n", Some("Integer"), false),
            // The ELSE edge of a disjoint branch keeps the incoming fact.
            ("br_else_keeps", "def f(v)\n  return unless v.is_a?(String)\n  if v.is_a?(Hash)\n    1\n  else\n    v.frobnicate_zzz\n  end\nend\n", Some("String"), false),
            // ---- must-still-fire controls -----------------------------------
            ("ctrl_single", "def f(v)\n  return unless v.is_a?(String)\n  v.frobnicate_zzz\nend\n", Some("String"), false),
            // A rebind between the guards resets the meet: the second guard
            // mints fresh (the reference fires `for Hash`).
            ("ctrl_write_between", "def f(v, w)\n  return unless v.is_a?(String)\n  v = w\n  return unless v.is_a?(Hash)\n  v.frobnicate_zzz\nend\n", Some("Hash"), false),
            // ---- the keep family: `:unknown stays conservative` + unions ----
            // An `||` union with a LIVE member meets per member: `Bot ∪
            // String` is the carrier and the reference fires `for String`.
            ("seq_or_mixed", "def f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(Hash) || v.is_a?(String)\n  v.frobnicate_zzz\nend\n", Some("String"), false),
            // An unresolvable ordering (a project class): `:unknown stays
            // conservative` — the reference keeps the carrier and fires
            // `for String`, even when the project hierarchy (`< Hash`) would
            // prove disjointness (probe `projsub`).
            ("seq_projclass", "class ProjKlass; end\n\ndef f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(ProjKlass)\n  v.frobnicate_zzz\nend\n", Some("String"), false),
            ("seq_projsub", "class ProjKlass < Hash; end\n\ndef f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(ProjKlass)\n  v.frobnicate_zzz\nend\n", Some("String"), false),
            // The OTHER `Unknown`: two RBS-space names our resolver cannot
            // order. The reference proves them disjoint and is silent (the S2
            // probe r7), so the fact DROPS — neither witnessed nor `Bot`.
            ("seq_ns_unknown_drop", "def f(v)\n  return unless v.is_a?(File::Stat)\n  return unless v.is_a?(URI::HTTP)\n  v.frobnicate_zzz\nend\n", None, false),
        ];
        for (row, src, expected, dead) in rows {
            let ast = lower_src(src.as_bytes());
            let index = CoreIndex::new();
            let source = SourceIndex::build(&ast, &index);
            let scopes = lexical_scopes(&ast);
            let typer = Typer::with_source(&index, &source).with_lexical_scopes(&scopes);
            let mut i = Interner::new();
            let pass = typer.class_narrowing_pass(&ast, &mut i);
            let call = call_named(&ast, "frobnicate_zzz");
            assert_eq!(
                pass.calls.get(&call).map(String::as_str),
                *expected,
                "sequential-guard row {row} snapshot\n--- source ---\n{src}"
            );
            assert_eq!(pass.dead.contains(&call), *dead, "sequential-guard row {row} dead");
            if *row == "seq_use_between" {
                let first = call_named(&ast, "frobnicate_yyy");
                assert_eq!(
                    pass.calls.get(&first).map(String::as_str),
                    Some("String"),
                    "sequential-guard row {row}: the use BETWEEN the guards fires"
                );
                assert!(!pass.dead.contains(&first), "row {row}: first use stays live");
            }
        }
    }

    /// SEQUENTIAL guards on a stage-3a-3 CHAIN address — the chain twin of
    /// [`class_narrowing_sequential_guard_meet_matrix`]. Every row was measured
    /// against the pinned oracle (`v0.3.1`, 2026-08-09 `chain_*` probe matrix,
    /// one FRESH cwd per scenario, `--no-cache`): `Some(class)` — the reference
    /// fires `for <class>` and the snapshot must carry it; `(None, true)` — the
    /// meet reached `Bot` and the call site must be DEAD; `(None, false)` — a
    /// recorded DECLINE (the reference may fire; we drop the fact — coverage,
    /// never an FP).
    ///
    /// `chain_or_disjoint` and `chain_third` were LIVE false positives on
    /// master: the union guard skipped the `classes.len() == 1` mint gate and
    /// the disjoint re-guard could only REMOVE (never collapse), so a stale or
    /// re-minted fact witnessed `for String` where the reference is silent.
    #[test]
    fn class_narrowing_chain_guard_meet_matrix() {
        let rows: &[(&str, &str, Option<&str>, bool)] = &[
            // ---- the FP family: disjoint sequential pairs reach Bot ---------
            ("chain_disjoint", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(Hash)\n  h.last.frobnicate_zzz\nend\n", None, true),
            // A THIRD guard cannot revive the collapsed address (live FP).
            ("chain_third", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(Hash)\n  return unless h.last.is_a?(String)\n  h.last.frobnicate_zzz\nend\n", None, true),
            ("chain_bang", "def f(h)\n  return unless h.last.is_a?(String)\n  return if !h.last.is_a?(Hash)\n  h.last.frobnicate_zzz\nend\n", None, true),
            // An `||` union whose EVERY member is disjoint (live FP: the mint
            // gate skipped the 2-class guard and the stale fact survived).
            ("chain_or_disjoint", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(Hash) || h.last.is_a?(Array)\n  h.last.frobnicate_zzz\nend\n", None, true),
            // `instance_of?` collapses on a bare name mismatch BEFORE the
            // hierarchy — a SUBCLASS name included.
            ("chain_exact_disjoint", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.instance_of?(Hash)\n  h.last.frobnicate_zzz\nend\n", None, true),
            ("chain_exact_subclass", "def f(h)\n  return unless h.last.is_a?(Numeric)\n  return unless h.last.instance_of?(Integer)\n  h.last.frobnicate_zzz\nend\n", None, true),
            // A NON-terminating second conditional: the branch-edge meet.
            ("chain_br_disjoint", "def f(h)\n  return unless h.last.is_a?(String)\n  if h.last.is_a?(Hash)\n    h.last.frobnicate_zzz\n  end\nend\n", None, true),
            // A use between the guards fires; the use after the collapse is dead.
            ("chain_ctrl_use_between", "def f(h)\n  return unless h.last.is_a?(String)\n  h.last.frobnicate_yyy\n  return unless h.last.is_a?(Hash)\n  h.last.frobnicate_zzz\nend\n", None, true),
            // ---- refinement / no-op: the reference FIRES and so must we -----
            ("chain_subclass", "def f(h)\n  return unless h.last.is_a?(Numeric)\n  return unless h.last.is_a?(Integer)\n  h.last.frobnicate_zzz\nend\n", Some("Integer"), false),
            ("chain_superclass", "def f(h)\n  return unless h.last.is_a?(Integer)\n  return unless h.last.is_a?(Numeric)\n  h.last.frobnicate_zzz\nend\n", Some("Integer"), false),
            ("chain_same", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(String)\n  h.last.frobnicate_zzz\nend\n", Some("String"), false),
            ("chain_br_subclass", "def f(h)\n  return unless h.last.is_a?(Numeric)\n  if h.last.is_a?(Integer)\n    h.last.frobnicate_zzz\n  end\nend\n", Some("Integer"), false),
            ("chain_br_superclass", "def f(h)\n  return unless h.last.is_a?(Integer)\n  if h.last.is_a?(Numeric)\n    h.last.frobnicate_zzz\n  end\nend\n", Some("Integer"), false),
            // The ELSE edge of a disjoint branch keeps the incoming fact.
            ("chain_br_else_keeps", "def f(h)\n  return unless h.last.is_a?(String)\n  if h.last.is_a?(Hash)\n    1\n  else\n    h.last.frobnicate_zzz\n  end\nend\n", Some("String"), false),
            // ---- the keep family: `:unknown stays conservative` + unions ----
            // `Bot ∪ String` is the carrier.
            ("chain_or_mixed", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(Hash) || h.last.is_a?(String)\n  h.last.frobnicate_zzz\nend\n", Some("String"), false),
            // A PROJECT class: `:unknown stays conservative` keeps the carrier
            // even when the project hierarchy would prove disjointness. These
            // rows passed before this slice only because a project-class guard
            // is non-mintable and skipped; the explicit `Unknown` split in
            // `narrow_nominal_to_class` is what keeps them passing now.
            ("chain_projclass", "class ProjKlass; end\n\ndef f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(ProjKlass)\n  h.last.frobnicate_zzz\nend\n", Some("String"), false),
            ("chain_projsub", "class ProjKlass < Hash; end\n\ndef f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(ProjKlass)\n  h.last.frobnicate_zzz\nend\n", Some("String"), false),
            ("chain_projsub_or", "class ProjKlass < Hash; end\n\ndef f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(Hash) || h.last.is_a?(ProjKlass)\n  h.last.frobnicate_zzz\nend\n", Some("String"), false),
            // ---- must-still-fire controls -----------------------------------
            ("chain_ctrl_single", "def f(h)\n  return unless h.last.is_a?(String)\n  h.last.frobnicate_zzz\nend\n", Some("String"), false),
            // A rebind of the ROOT resets the address: the second guard mints.
            ("chain_ctrl_rebind", "def f(h, w)\n  return unless h.last.is_a?(String)\n  h = w\n  return unless h.last.is_a?(Hash)\n  h.last.frobnicate_zzz\nend\n", Some("Hash"), false),
            // A call ON the root invalidates the address (the existing
            // `invalidate_chain_after_call` port), so the second guard mints.
            ("chain_ctrl_pop_between", "def f(h)\n  return unless h.last.is_a?(String)\n  h.pop\n  return unless h.last.is_a?(Hash)\n  h.last.frobnicate_zzz\nend\n", Some("Hash"), false),
            // ---- declines that STAY -----------------------------------------
            // Two RBS-space names our resolver cannot order: the reference
            // proves them disjoint and is silent, so the fact DROPS.
            ("chain_r7", "def f(h)\n  return unless h.last.is_a?(File::Stat)\n  return unless h.last.is_a?(URI::HTTP)\n  h.last.frobnicate_zzz\nend\n", None, false),
            // RECOGNITION gap (not this slice): `guard_predicate` requires a
            // bare LOCAL operand, so `===` / `nil?` on a chain receiver is
            // never a chain guard at all and the incoming fact dies at the
            // join. The reference fires on the first three — pure coverage.
            ("chain_caseeq_same", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless String === h.last\n  h.last.frobnicate_zzz\nend\n", None, false),
            ("chain_caseeq_subclass", "def f(h)\n  return unless h.last.is_a?(Numeric)\n  return unless Integer === h.last\n  h.last.frobnicate_zzz\nend\n", None, false),
            ("chain_nilq", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.nil?\n  h.last.frobnicate_zzz\nend\n", None, false),
            ("chain_caseeq_disjoint", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless Hash === h.last\n  h.last.frobnicate_zzz\nend\n", None, false),
        ];
        assert_eq!(rows.len(), 26, "the chain probe matrix has 26 oracle-measured rows");
        for (row, src, expected, dead) in rows {
            let ast = lower_src(src.as_bytes());
            let index = CoreIndex::new();
            let source = SourceIndex::build(&ast, &index);
            let scopes = lexical_scopes(&ast);
            let typer = Typer::with_source(&index, &source).with_lexical_scopes(&scopes);
            let mut i = Interner::new();
            let pass = typer.class_narrowing_pass(&ast, &mut i);
            let call = call_named(&ast, "frobnicate_zzz");
            assert_eq!(
                pass.calls.get(&call).map(String::as_str),
                *expected,
                "chain-guard row {row} snapshot\n--- source ---\n{src}"
            );
            assert_eq!(pass.dead.contains(&call), *dead, "chain-guard row {row} dead");
            if *row == "chain_ctrl_use_between" {
                let first = call_named(&ast, "frobnicate_yyy");
                assert_eq!(
                    pass.calls.get(&first).map(String::as_str),
                    Some("String"),
                    "chain-guard row {row}: the use BETWEEN the guards fires"
                );
                assert!(!pass.dead.contains(&first), "row {row}: first use stays live");
            }
        }
    }

    /// JOIN RETENTION — a fact minted BEFORE a conditional survives it
    /// ([`retain_joined_facts`]). Master blanket-wiped every `Narrowed` local
    /// and every chain fact at each `if`/`unless`/`case` merge, so a fact died
    /// at ANY later intervening conditional, terminating or not, related or not.
    ///
    /// Every row was measured against the PINNED oracle (`v0.3.2`/`c6b91b9e`,
    /// 2026-08-09, one fresh temp cwd per case, `--no-cache`, both reference
    /// libs on `-I`). `Some(class)` — the reference fires `for <class>` and the
    /// snapshot must carry it; `(None, true)` — the site must be DEAD; `(None,
    /// false)` — no fact (either the reference is silent too, or a recorded
    /// DECLINE, marked per row).
    #[test]
    fn class_narrowing_join_retention_matrix() {
        let rows: &[(&str, &str, Option<&str>, bool)] = &[
            // ---- the retention family: master silent, reference FIRES --------
            ("baseline_single_guard", "def f(a)\n  return unless a.is_a?(String)\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            // A guard on a DIFFERENT local killed the first one's fact.
            ("double_guard", "def f(a, b)\n  return unless a.is_a?(String)\n  return unless b.is_a?(Hash)\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            // A wholly unrelated NON-terminating `if`, with and without `else`.
            ("unrelated_nonterm_if", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    x = 1\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            ("unrelated_nonterm_if_else", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    x = 1\n  else\n    x = 2\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            ("unless_intervening", "def f(a, b)\n  return unless a.is_a?(String)\n  unless b\n    x = 1\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            ("modifier_if_intervening", "def f(a, b)\n  return unless a.is_a?(String)\n  x = 1 if b\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            ("nested_intervening_if", "def f(a, b, c)\n  return unless a.is_a?(String)\n  if b\n    if c\n      x = 1\n    end\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            ("two_intervening_ifs", "def f(a, b, c)\n  return unless a.is_a?(String)\n  if b\n    x = 1\n  end\n  if c\n    y = 1\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            // The reference does NOT prune the statements after a conditional
            // whose branches BOTH terminate — the fact rides through.
            ("both_branches_terminate", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    return\n  else\n    return\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            // …and one that terminates on a single edge whose guard map is empty
            // (the propagation carries nothing; only the retention fires here).
            ("single_terminating_unrelated", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    return\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            // EXPRESSION position: the reference retains there too, so the
            // restore is NOT gated on `stmt_position` (unlike the propagation).
            ("expr_position_ternary", "def f(a, b)\n  return unless a.is_a?(String)\n  x = b ? 1 : 2\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            ("expr_position_if", "def f(a, b)\n  return unless a.is_a?(String)\n  x = if b\n    1\n  else\n    2\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            // `case`: an unrelated subject, a `when` clause and an `in` clause.
            ("case_intervening", "def f(a, b)\n  return unless a.is_a?(String)\n  case b\n  when Integer\n    x = 1\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            ("case_in_intervening", "def f(a, b)\n  return unless a.is_a?(String)\n  case b\n  in Integer\n    x = 1\n  else\n    x = 2\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            // A guard on a CHAIN of another local (`b.length`), and the chain
            // twin of the whole family (a chain fact across an intervening if).
            ("three_guard_chain", "def f(a, b)\n  return unless a.is_a?(String)\n  return unless b.is_a?(String)\n  return unless b.length.is_a?(Integer)\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            ("chain_intervening_if", "def f(h, b)\n  return unless h.last.is_a?(String)\n  if b\n    x = 1\n  end\n  h.last.frobnicate_zzz\nend\n", Some("String"), false),
            // The CENSUS row, reduced from gitlab-foss
            // `lib/bulk_imports/object_counter.rb:52`: a non-narrowing guard on
            // the SAME local (`empty?` / `key?` are not class guards, so they
            // contribute no guard map and the fact must simply survive).
            ("object_counter_reduced", "def f(x)\n  return unless x.is_a?(Hash)\n  return if x.empty?\n  x.frobnicate_zzz\nend\n", Some("Hash"), false),
            ("object_counter_block_form", "def f(x)\n  return unless x.is_a?(Hash)\n  if x.empty?\n    return\n  end\n  x.frobnicate_zzz\nend\n", Some("Hash"), false),
            ("nonnarrowing_guard_same_var", "def f(x)\n  return unless x.is_a?(Hash)\n  return if x.key?(:a)\n  x.frobnicate_zzz\nend\n", Some("Hash"), false),
            // The `if` with an `else` lowers its else clause to a clause-less
            // `BeginRescue` carrier; unwrapping it is what makes the `_else`
            // rows above pass (an else body of ANY shape used to wipe the edge).
            ("else_body_is_a_bare_literal", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    nil\n  else\n    nil\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            // ---- rows that already matched, and must not regress ------------
            ("unrelated_if_before_guard", "def f(a, b)\n  if b\n    x = 1\n  end\n  return unless a.is_a?(String)\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            ("intervening_method_call", "def f(a, b)\n  return unless a.is_a?(String)\n  b.to_s\n  a.frobnicate_zzz\nend\n", Some("String"), false),
            ("use_inside_nonterm_branch", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    a.frobnicate_zzz\n  end\nend\n", Some("String"), false),
            // ---- FP HAZARDS: every one measured reference-SILENT -------------
            // 1. A rebind of the target inside ONE branch. The reference fires a
            //    real union (`for 1 | String`) — the separate widen gap — so we
            //    must stay silent rather than witness `for String`.
            ("write_to_a_in_if", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    a = 1\n  end\n  a.frobnicate_zzz\nend\n", None, false),
            ("branch_rebind_one_side", "def f(a, b, w)\n  return unless a.is_a?(String)\n  if b\n    a = w\n  end\n  a.frobnicate_zzz\nend\n", None, false),
            ("rebind_in_else_only", "def f(a, b, w)\n  return unless a.is_a?(String)\n  if b\n    x = 1\n  else\n    a = w\n  end\n  a.frobnicate_zzz\nend\n", None, false),
            ("ternary_rebinds_target", "def f(a, b, w)\n  return unless a.is_a?(String)\n  x = b ? (a = w) : 2\n  a.frobnicate_zzz\nend\n", None, false),
            // 4. A `case`/`in` pattern clause is NOT descended, so its rebind is
            //    invisible to the edge evidence — the span kill is what holds.
            ("case_in_rebinds_target", "def f(a, b)\n  return unless a.is_a?(String)\n  case b\n  in Integer\n    a = 1\n  else\n    a = 2\n  end\n  a.frobnicate_zzz\nend\n", None, false),
            // A REBIND of a chain ROOT inside a branch kills the address.
            ("chain_root_rebind_in_if", "def f(h, b, w)\n  return unless h.last.is_a?(String)\n  if b\n    h = w\n  end\n  h.last.frobnicate_zzz\nend\n", None, false),
            // A plain CALL on the chain root inside a branch invalidates the
            // address (`invalidate_chain_after_call`) — invisible to `writes`,
            // caught only by the edge disagreement.
            ("chain_call_on_root_in_branch", "def f(h, b)\n  return unless h.last.is_a?(String)\n  if b\n    h.size\n  end\n  h.last.frobnicate_zzz\nend\n", None, false),
            ("chain_mutator_on_root_in_branch", "def f(h, b)\n  return unless h.last.is_a?(String)\n  if b\n    h.pop\n  end\n  h.last.frobnicate_zzz\nend\n", None, false),
            // 2. The conditional's OWN guard targets: a disjoint re-guard must
            //    still reach `Bot` and the use must be DEAD, both inside the
            //    branch and — the row that was a LIVE FP on master — after a
            //    later guard whose meet now sees the RESTORED incoming fact.
            ("own_guard_disjoint_after", "def f(a)\n  return unless a.is_a?(String)\n  if a.is_a?(Hash)\n    a.frobnicate_zzz\n  end\nend\n", None, true),
            ("guard_then_if_then_disjoint_guard", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    x = 1\n  end\n  return unless a.is_a?(Hash)\n  a.frobnicate_zzz\nend\n", None, true),
            // …and the refining twin still fires `for Integer`.
            ("guard_then_if_then_subclass_guard", "def f(a, b)\n  return unless a.is_a?(Numeric)\n  if b\n    x = 1\n  end\n  return unless a.is_a?(Integer)\n  a.frobnicate_zzz\nend\n", Some("Integer"), false),
            // 5. The guard's OWN `if` with BOTH branches terminating stays
            //    silent (reference-measured) — there is no PRE-join fact to put
            //    back, so the retention cannot resurrect the declined
            //    propagation. Its positive-guard twin is a DECLINE below.
            ("t_both_terminate_negated", "def f(a)\n  if !a.is_a?(String)\n    return\n  else\n    return\n  end\n  a.frobnicate_zzz\nend\n", None, false),
            // A `Bot` fact riding through an intervening `if` still suppresses.
            ("bot_intervening_if", "def f(b)\n  v = [1, 2]\n  return unless v.is_a?(Hash)\n  if b\n    x = 1\n  end\n  v.frobnicate_zzz\nend\n", None, true),
            // ---- DECLINES: the reference FIRES, we stay silent (coverage) ----
            // The conditional's own guard target after a NON-terminating merge:
            // the reference unions the edges back to the incoming class. We
            // exclude every target the edges disagree on, which is the whole
            // FP-safety of hazard 2 — this is the price.
            ("d_own_guard_target_after_join", "def f(a)\n  return unless a.is_a?(String)\n  if a.is_a?(Hash)\n    x = 1\n  end\n  a.frobnicate_zzz\nend\n", None, false),
            // The guard's own `if`, both branches terminating, guard on the
            // TRUTHY edge: the reference fires `for String`, we have no pre-join
            // fact and the propagation declines when both branches terminate.
            ("d_t_both_terminate_positive", "def f(a)\n  if a.is_a?(String)\n    return\n  else\n    return\n  end\n  a.frobnicate_zzz\nend\n", None, false),
            // The `case` SUBJECT is excluded from the restore by construction.
            ("d_case_subject_is_target", "def f(a)\n  return unless a.is_a?(String)\n  case a\n  when Integer\n    x = 1\n  end\n  a.frobnicate_zzz\nend\n", None, false),
            // A MUTATION of the target inside a branch drops the fact
            // (`kill_cenv_narrowed`); the reference keeps `Array` and fires.
            ("d_mutator_in_branch", "def f(a, b)\n  return unless a.is_a?(Array)\n  if b\n    a.push(1)\n  end\n  a.frobnicate_zzz\nend\n", None, false),
            // A `Narrowed` fact still does not enter a BLOCK body, nor survive a
            // block CALL — the block-boundary rules (`n_escape_after_if`, the
            // next/break matrix p9/p13) are deliberately untouched by this slice.
            ("d_use_in_block_after_if", "def f(a, b, xs)\n  return unless a.is_a?(String)\n  if b\n    x = 1\n  end\n  xs.each do |y|\n    a.frobnicate_zzz\n  end\nend\n", None, false),
            ("d_join_inside_block_use_outside", "def f(a, b, xs)\n  return unless a.is_a?(String)\n  xs.each do |i|\n    if b\n      x = 1\n    end\n  end\n  a.frobnicate_zzz\nend\n", None, false),
        ];
        assert_eq!(rows.len(), 42, "the join-retention matrix has 42 oracle-measured rows");
        for (row, src, expected, dead) in rows {
            let ast = lower_src(src.as_bytes());
            let index = CoreIndex::new();
            let source = SourceIndex::build(&ast, &index);
            let scopes = lexical_scopes(&ast);
            let typer = Typer::with_source(&index, &source).with_lexical_scopes(&scopes);
            let mut i = Interner::new();
            let pass = typer.class_narrowing_pass(&ast, &mut i);
            let call = call_named(&ast, "frobnicate_zzz");
            assert_eq!(
                pass.calls.get(&call).map(String::as_str),
                *expected,
                "join-retention row {row} snapshot\n--- source ---\n{src}"
            );
            assert_eq!(pass.dead.contains(&call), *dead, "join-retention row {row} dead");
        }
    }

    /// Decline: safe-nav dispatch on the narrowed local never records.
    #[test]
    fn class_narrowing_safe_nav_declines() {
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  if value.is_a?(Hash)\n    value&.frobnicate_zzz\n  end\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    }

    /// `elsif` chains narrow each truthy arm independently (the chained `If`
    /// lowers into the else branch).
    #[test]
    fn class_narrowing_elsif_arms_narrow_independently() {
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  if value.is_a?(Hash)\n    value.frobnicate_zzz\n  elsif value.is_a?(String)\n    value.other_zzz\n  end\nend\n",
        );
        assert_eq!(snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str), Some("Hash"));
        assert_eq!(snaps.get(&call_named(&ast, "other_zzz")).map(String::as_str), Some("String"));
    }

    /// a3: `case value / when Hash / when String` narrows per clause; the
    /// `else` body (a negative edge) is never narrowed.
    #[test]
    fn class_narrowing_case_when_narrows_per_clause() {
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  case value\n  when Hash\n    value.frobnicate_zzz\n  when String\n    value.frobnicate_yyy\n  else\n    value.else_zzz\n  end\nend\n",
        );
        assert_eq!(snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str), Some("Hash"));
        assert_eq!(
            snaps.get(&call_named(&ast, "frobnicate_yyy")).map(String::as_str),
            Some("String")
        );
        assert!(!snaps.contains_key(&call_named(&ast, "else_zzz")));
    }

    /// a6 decline: a multi-condition clause (`when Hash, String` — a union in
    /// the reference) narrows NOTHING in this slice.
    #[test]
    fn class_narrowing_multi_condition_when_declines() {
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  case value\n  when Hash, String\n    value.frobnicate_zzz\n  end\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    }

    /// Decline: a non-local subject, a non-constant condition, and a rebind
    /// inside the clause body all narrow nothing.
    #[test]
    fn class_narrowing_case_declines() {
        // Subject is a call, not a bare local.
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  case value.foo\n  when Hash\n    value.frobnicate_zzz\n  end\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
        // Condition is a literal, not a static constant.
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  case value\n  when 1\n    value.frobnicate_zzz\n  end\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
        // Rebind inside the clause body invalidates subsequent uses.
        let (ast, snaps) = class_snaps(
            b"def f(value, other)\n  case value\n  when Hash\n    value = other\n    value.frobnicate_zzz\n  end\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    }

    /// Decline: a shadowed constant in a `when` condition narrows nothing, and
    /// facts do not enter a block body inside a clause.
    #[test]
    fn class_narrowing_case_shadow_and_block_declines() {
        let (ast, snaps) = class_snaps(
            b"class Hash\nend\ndef f(value)\n  case value\n  when Hash\n    value.frobnicate_zzz\n  end\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
        let (ast, snaps) = class_snaps(
            b"def f(value)\n  case value\n  when Hash\n    [1].each { |_i| value.frobnicate_zzz }\n  end\nend\n",
        );
        assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    }

    /// STAGE 3a-3 — single-hop chain guards, LOCAL roots
    /// (docs/notes/20260807-narrowing-stage3-spec.md, "3a-3 BUILT").
    ///
    /// Same convention as the 3a-1 and `next`/`break` matrices: `Some(c)` — the
    /// reference FIRES and rigor-rs must record `c`; `None` — rigor-rs records
    /// nothing, either because the reference is SILENT (recording would be a
    /// live false positive) or because a decline costs coverage the reference
    /// has (a strict subset, never an FP). Every row is oracle-measured against
    /// the pinned reference `v0.3.1` from a FRESH temp cwd with `--no-cache`
    /// and the checkout plugin path pinned; the row name is the probe name in
    /// the note's tables.
    ///
    /// The expectation is on the FIRST `frobnicate_zzz` call unless the row
    /// name says otherwise (`f11` asserts both).
    #[test]
    fn class_narrowing_stage3a3_chain_guard_matrix() {
        const USE: &str = "h.last.frobnicate_zzz";
        const G: &str = "h.last.is_a?(String)";
        let f = |body: &str| format!("def f(h, g, xs, cond)\n{body}\nend\n");
        let rows: Vec<(&str, String, Option<&str>)> = vec![
            // ---- e: the spec's own c7 matrix, reproduced --------------------
            ("c7a", f(&format!("  {USE} if {G}")), Some("String")),
            // c7b: an IVAR root. The arena's `VariableRead` is NAMELESS, so
            // `stable_chain_address` cannot key it — a recorded coverage gap
            // (the reference fires `for String`).
            (
                "c7b_ivar",
                "class K\n  def f\n    @h.last.frobnicate_zzz if @h.last.is_a?(String)\n  end\nend\n"
                    .to_string(),
                None,
            ),
            // c7c/f23: the reference KEEPS the fact through an argument-position
            // mention of the root; we kill on ANY mention. Coverage only.
            ("c7c_arg_mention", f(&format!("  if {G}\n    g(h)\n    {USE}\n  end")), None),
            ("f23_push", f(&format!("  if {G}\n    xs.push(h)\n    {USE}\n  end")), None),
            // c7d: a call whose RECEIVER is the root invalidates — reference
            // silent, so keeping the fact would be a live FP.
            ("c7d_pop", f(&format!("  if {G}\n    h.pop\n    {USE}\n  end")), None),
            // c7e: arguments on the hop ⇒ no stable address, on both engines.
            (
                "c7e_args_on_hop",
                f("  h.fetch(0).frobnicate_zzz if h.fetch(0).is_a?(String)"),
                None,
            ),
            // c7g: the REBIND control. The reference fires here, but with a
            // DIFFERENT diagnostic (`for nil`, folding the rebound `[].last`),
            // so the write-kill must make us silent.
            ("c7g_rebind", f(&format!("  if {G}\n    h = []\n    {USE}\n  end")), None),
            ("c7h_inert", f(&format!("  if {G}\n    x = 1\n    {USE}\n  end")), Some("String")),
            ("h1_return_unless", f(&format!("  return unless {G}\n  {USE}")), Some("String")),
            // ---- a: the chain guard as a CONJUNCT ---------------------------
            ("a_conj_right_then", f(&format!("  if cond && {G}\n    {USE}\n  end")), Some("String")),
            ("a_conj_left_then", f(&format!("  if {G} && cond\n    {USE}\n  end")), Some("String")),
            (
                "a_conj_elsif",
                f(&format!("  if g\n    1\n  elsif cond && {G}\n    {USE}\n  end")),
                Some("String"),
            ),
            // The `&&` FALSEY edge of an atomic chain guard is empty — the
            // reference is silent in the `else`, so narrowing there would be an FP.
            ("a_conj_else_ctl", f(&format!("  if cond && {G}\n    1\n  else\n    {USE}\n  end")), None),
            ("a_conj_mid", f(&format!("  if g && {G} && cond\n    {USE}\n  end")), Some("String")),
            // An `||` with an unrecognised disjunct joins in the un-narrowed
            // scope ⇒ nothing, on both engines.
            ("a_or_disjunct_ctl", f(&format!("  if {G} || cond\n    {USE}\n  end")), None),
            // A LOCAL guard and a CHAIN guard in one predicate are independent
            // targets — both apply.
            (
                "a_conj_localguard_mix",
                "def f(h, v)\n  if v.is_a?(Integer) && h.last.is_a?(String)\n    h.last.frobnicate_zzz\n  end\nend\n"
                    .to_string(),
                Some("String"),
            ),
            // ---- b: the `!` swap and falsey-edge termination ----------------
            ("b_bang_else", f(&format!("  if !{G}\n    1\n  else\n    {USE}\n  end")), Some("String")),
            ("b_bang_then_ctl", f(&format!("  if !{G}\n    {USE}\n  end")), None),
            ("b_return_if_bang", f(&format!("  return if !{G}\n  {USE}")), Some("String")),
            (
                "b_unless_stmt",
                f(&format!("  unless {G}\n    1\n  else\n    {USE}\n  end")),
                Some("String"),
            ),
            (
                "b_return_unless_compound",
                f(&format!("  return unless cond && {G}\n  {USE}")),
                Some("String"),
            ),
            ("b_raise_unless", f(&format!("  raise 'x' unless {G}\n  {USE}")), Some("String")),
            // The `next`/`break` termination slice composes with chain facts.
            (
                "b_next_unless",
                f(&format!("  xs.each do |_x|\n    next unless {G}\n    {USE}\n  end")),
                Some("String"),
            ),
            // ---- c: disjoint / `Bot` interaction ----------------------------
            // A PRECISE chain carrier: the reference collapses `h.last`
            // (`Integer`) to `Bot` under a `String` guard and is SILENT. Our
            // carrier gate reads the SAME node's type and declines the mint —
            // the same silence by a different route, and no chain `Bot` fact.
            ("c_bot_precise_root", f("  h = [1, 2]\n  h.last.frobnicate_zzz if h.last.is_a?(String)"), None),
            // The must-still-fire twin of the row above.
            ("c_must_still_fire", f(&format!("  {USE} if {G}")), Some("String")),
            // A LOCAL collapsed to `Bot` beside a chain guard does NOT suppress
            // the chain witness — the reference fires (`out.dead` keys on the
            // local's own calls, and the chain call's receiver is not that local).
            (
                "c_chain_and_local_bot",
                "def f(h)\n  v = [1, 2]\n  if v.is_a?(String) && h.last.is_a?(String)\n    h.last.frobnicate_zzz\n  end\nend\n"
                    .to_string(),
                Some("String"),
            ),
            // ---- d: the same address re-guarded in sequence -----------------
            // The sequential-disjoint hazard. The reference carries `String`
            // into the second guard and collapses to `Bot`; without the
            // pre-join re-seed in `class_flow_if` we would witness `for Hash`.
            (
                "d_seq_two_returns_disjoint",
                f(&format!("  return unless {G}\n  return unless h.last.is_a?(Hash)\n  {USE}")),
                None,
            ),
            (
                "d_seq_and_disjoint",
                f(&format!("  if {G} && h.last.is_a?(Hash)\n    {USE}\n  end")),
                None,
            ),
            // A SUBCLASS re-guard REFINES to the more specific class. Was a
            // recorded decline (the blind R3 drop) until the 2026-08-09
            // chain-guard meet; the reference fires `for Integer` (probe
            // `chain_subclass`) and now so do we — see
            // `class_narrowing_chain_guard_meet_matrix`.
            (
                "d_seq_subclass",
                f("  return unless h.last.is_a?(Numeric)\n  return unless h.last.is_a?(Integer)\n  h.last.frobnicate_zzz"),
                Some("Integer"),
            ),
            (
                "d_seq_same_class",
                f(&format!("  return unless {G}\n  return unless {G}\n  {USE}")),
                Some("String"),
            ),
            ("d_nested_reguard", f(&format!("  if {G}\n    if h.last.is_a?(Hash)\n      {USE}\n    end\n  end")), None),
            // ---- carrier hazards on the ADDRESS -----------------------------
            // A `||`-bound ROOT still narrows: the PR #72 carrier ALLOW-LIST is
            // a per-LOCAL rule and must NOT be applied to a chain address (the
            // reference fires — applying it would be pure coverage loss).
            ("k_root_or_union", "def f(a, b)\n  h = a || b\n  h.last.frobnicate_zzz if h.last.is_a?(String)\nend\n".to_string(), Some("String")),
            ("k_root_splat", "def f(spec)\n  h = *spec\n  h.last.frobnicate_zzz if h.last.is_a?(String)\nend\n".to_string(), Some("String")),
            ("k_root_from_call", "def f(x)\n  h = x.fetch(:a)\n  h.last.frobnicate_zzz if h.last.is_a?(String)\nend\n".to_string(), Some("String")),
            ("k_root_kwarg", "def f(h: nil)\n  h.last.frobnicate_zzz if h.last.is_a?(String)\nend\n".to_string(), Some("String")),
            // Precise carriers the reference collapses — all reference-SILENT,
            // all declined by the Dynamic/Top carrier gate.
            ("k_root_hash_lit", "def f\n  h = { a: 1 }\n  h.size.frobnicate_zzz if h.size.is_a?(String)\nend\n".to_string(), None),
            ("k_root_str_lit", "def f\n  h = 'abc'\n  h.upcase.frobnicate_zzz if h.upcase.is_a?(Hash)\nend\n".to_string(), None),
            ("k_root_int_lit", "def f\n  h = 3\n  h.succ.frobnicate_zzz if h.succ.is_a?(String)\nend\n".to_string(), None),
            // ---- guard family / shape variants ------------------------------
            ("m_kind_of", f("  h.last.frobnicate_zzz if h.last.kind_of?(String)"), Some("String")),
            ("m_instance_of", f("  h.last.frobnicate_zzz if h.last.instance_of?(String)"), Some("String")),
            // DECLINE: `===` is non-mintable (3a-1's own finding) and never
            // produces a chain target. The reference narrows through it.
            ("m_case_eq", f("  h.last.frobnicate_zzz if String === h.last"), None),
            // DECLINE: safe-nav, on the hop or on the use. Reference fires.
            ("m_safe_nav_hop", f("  h&.last.frobnicate_zzz if h&.last.is_a?(String)"), None),
            ("m_safe_nav_use", f(&format!("  h.last&.frobnicate_zzz if {G}")), None),
            // A block on the hop: no stable address, reference silent too.
            ("m_block_on_hop", f("  h.map { |x| x }.frobnicate_zzz if h.map { |x| x }.is_a?(String)"), None),
            // The OUTER call's own arguments and block are irrelevant — the
            // reference narrows the RECEIVER expression, so both fire.
            ("m_use_with_args", f(&format!("  h.last.frobnicate_zzz(1) if {G}")), Some("String")),
            ("m_use_with_block", f(&format!("  h.last.frobnicate_zzz {{ |x| x }} if {G}")), Some("String")),
            // Single-hop only; a different method or root is a different address.
            ("m_two_hop", f("  h.first.last.frobnicate_zzz if h.first.last.is_a?(String)"), None),
            ("m_different_method", f(&format!("  h.first.frobnicate_zzz if {G}")), None),
            ("m_different_root", f(&format!("  g.last.frobnicate_zzz if {G}")), None),
            // DECLINE: a project declaration shadowing the guard class declines
            // the whole guard (shared with stages 1-2). Reference fires.
            (
                "m_shadowed_const",
                "class String\nend\ndef f(h)\n  h.last.frobnicate_zzz if h.last.is_a?(String)\nend\n".to_string(),
                None,
            ),
            ("m_dynamic_const", "def f(h, c)\n  h.last.frobnicate_zzz if h.last.is_a?(c)\nend\n".to_string(), None),
            // ---- invalidation -----------------------------------------------
            ("n_root_mutator", f(&format!("  if {G}\n    h << 1\n    {USE}\n  end")), None),
            // A call whose receiver is the ADDRESS (not the root) does NOT
            // invalidate, on either engine.
            ("n_call_on_address", f(&format!("  if {G}\n    h.last.strip\n    {USE}\n  end")), Some("String")),
            ("n_address_receiver_call", f(&format!("  if {G}\n    h.last << g\n    {USE}\n  end")), Some("String")),
            ("n_root_write_after", f(&format!("  if {G}\n    {USE}\n    h = xs\n  end")), Some("String")),
            ("n_root_opwrite", f(&format!("  if {G}\n    h += xs\n    {USE}\n  end")), None),
            // DECLINE: chain facts do not cross a block boundary, in or out.
            ("n_into_block", f(&format!("  if {G}\n    xs.each {{ |_x| {USE} }}\n  end")), None),
            ("n_after_block", f(&format!("  if {G}\n    xs.each {{ |_x| 1 }}\n    {USE}\n  end")), None),
            // A fact minted in a branch does NOT escape it — reference-silent.
            ("n_escape_after_if", f(&format!("  if {G}\n    1\n  end\n  {USE}")), None),
            ("n_in_nested_if", f(&format!("  if {G}\n    if cond\n      {USE}\n    end\n  end")), Some("String")),
            ("n_root_as_arg_to_mutator", f(&format!("  if {G}\n    xs.fill(h)\n    {USE}\n  end")), None),
            ("n_use_before_guard", f(&format!("  {USE}\n  return unless {G}")), None),
            // ---- the three verified corpus shapes, reduced ------------------
            // (over a TOP-LEVEL guard class: the corpus rows themselves name
            // `Bundler::Source::Git`, and `check_narrowed_call`'s
            // `knows_toplevel_class` gate cannot witness a NAMESPACED class —
            // a pre-existing consumption limit this slice does not change, and
            // the reason the gap diff is 0. See the note's "BUILT" section.)
            (
                "w1_elsif_conj",
                "def f(dep, defn_dep, cond)\n  if dep.nil?\n    1\n  elsif cond && defn_dep.source.is_a?(String)\n    defn_dep.source.frobnicate_zzz\n  end\nend\n".to_string(),
                Some("String"),
            ),
            (
                "w2_index_write_if_mod",
                "def f(dep)\n  details = {}\n  details[:commit_sha] = dep.source.frobnicate_zzz if dep.source.instance_of?(String)\n  details\nend\n".to_string(),
                Some("String"),
            ),
            (
                "w3_return_unless",
                "def f(dep)\n  return unless dep.source.is_a?(String)\n\n  dep.source.frobnicate_zzz\nend\n".to_string(),
                Some("String"),
            ),
            (
                "w4_return_if_mod",
                "def f(spec)\n  return spec.source.frobnicate_zzz if spec.source.instance_of?(String)\n\n  nil\nend\n".to_string(),
                Some("String"),
            ),
            // ---- residual composition --------------------------------------
            ("x_two_addresses_one_root", f("  if h.first.is_a?(String) && h.last.is_a?(Hash)\n    h.first.frobnicate_zzz\n  end"), Some("String")),
            ("x_same_addr_two_roots", f(&format!("  if {G} && g.last.is_a?(Hash)\n    {USE}\n  end")), Some("String")),
            ("x_chain_in_begin", f(&format!("  return unless {G}\n\n  begin\n    {USE}\n  rescue StandardError\n    nil\n  end")), Some("String")),
            // DECLINE (carried from 3b-1): survival PAST a `begin` — a
            // `begin`/`rescue` is not a conditional join, so the join-retention
            // slice does not reach it.
            ("x_chain_after_begin", f(&format!("  return unless {G}\n\n  begin\n    1\n  rescue StandardError\n    nil\n  end\n  {USE}")), None),
            // …but survival past a `case` is CLOSED by the join-retention slice
            // (2026-08-09): the subject is an unrelated local, every clause was
            // descended and left the address alone, so the pre-`case` fact comes
            // back. Re-measured against the v0.3.2 oracle: the reference fires
            // `for String` here.
            ("x_chain_after_case", f(&format!("  return unless {G}\n\n  case cond\n  when 1 then 2\n  end\n  {USE}")), Some("String")),
            ("x_chain_in_loop_pred", f(&format!("  return unless {G}\n\n  while {USE}\n    break\n  end")), Some("String")),
            ("x_chain_in_case_clause", f(&format!("  return unless {G}\n\n  case cond\n  when 1 then {USE}\n  end")), Some("String")),
            ("x_chain_in_array_lit", f(&format!("  return unless {G}\n\n  x = [{USE}]\n  x")), Some("String")),
            ("x_chain_ternary", f(&format!("  return unless {G}\n\n  cond ? {USE} : 1")), Some("String")),
            // DECLINE: 3a-2 (`Logical` statement minting) is DEFERRED, so
            // `guard or raise` mints nothing. The reference fires.
            ("x_chain_or_raise", f(&format!("  {G} or raise 'no'\n  {USE}")), None),
            ("x_chain_guard_root_also_local", f(&format!("  if h.is_a?(Array) && {G}\n    {USE}\n  end")), Some("String")),
            // A rebind of the root inside the conditional's span declines the
            // propagation; a rebind before the use kills the fact.
            ("x_root_rebind_in_span", f(&format!("  if {G}\n    h = xs\n  end\n  {USE}")), None),
            ("x_root_rebind_then_return", f(&format!("  return unless {G}\n\n  h = xs\n  {USE}")), None),
            ("x_chain_multiwrite_root", f(&format!("  if {G}\n    h, _y = xs, 1\n    {USE}\n  end")), None),
            // DECLINE: an `||` of DIFFERENT classes is a real union (3a-4).
            ("x_chain_reguard_or", f(&format!("  if {G} || h.last.is_a?(Hash)\n    {USE}\n  end")), None),
            ("x_chain_bang_or", f(&format!("  return if !{G} || h.last.nil?\n\n  {USE}")), Some("String")),
            // A nested `def` is an independent scope — no fact crosses in.
            ("x_chain_nested_def", f(&format!("  return unless {G}\n\n  def q(h)\n    {USE}\n  end")), None),
            ("x_chain_use_as_arg", f(&format!("  return unless {G}\n\n  g({USE})")), Some("String")),
            ("x_chain_use_as_return", f(&format!("  return unless {G}\n\n  return {USE}")), Some("String")),
        ];
        for (row, src, expected) in &rows {
            let (ast, snaps) = class_snaps(src.as_bytes());
            let got = snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str);
            assert_eq!(got, *expected, "stage 3a-3 matrix row {row}\n--- source ---\n{src}");
        }
    }

    /// f11 — the fact SURVIVES its own re-read: BOTH chain uses in one branch
    /// are recorded (the reference fires twice). Split out of the matrix
    /// because `call_named` only reaches the first call of a name.
    #[test]
    fn class_narrowing_stage3a3_chain_fact_survives_its_own_reread() {
        let src = b"def f(h)\n  if h.last.is_a?(String)\n    h.last.frobnicate_zzz\n    h.last.frobnicate_zzz\n  end\nend\n";
        let (ast, snaps) = class_snaps(src);
        let uses: Vec<_> = ast
            .iter()
            .filter_map(|(id, n)| match n {
                Node::Call { method, .. } if method == "frobnicate_zzz" => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(uses.len(), 2, "two chain uses");
        for id in uses {
            assert_eq!(snaps.get(&id).map(String::as_str), Some("String"), "f11: both uses record");
        }
    }
}

// ---------------------------------------------------------------------------
// Collection-shape receiver survival (stage 1) — the oracle probe matrix
// m01-m20 of docs/notes/20260807-collection-shape-slice-spec.md. The SILENT
// rows are the FP-safety envelope, not coverage bookkeeping: each one is a
// shape the reference itself declines on (m04/m05/m08/m13/m15/m18/m20) or a
// deliberate coverage give-up (m16 op-writes, `while`, ivars, safe-nav).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod collection_shape_tests {
    use super::*;
    use rigor_parse::{lower, parse};

    /// The collection-shape snapshot map for `src`, wired exactly as the analyze
    /// pass wires it (per-file source index + lexical scopes).
    fn coll_snaps(src: &[u8]) -> (LoweredAst, HashMap<NodeId, &'static str>) {
        let ast = lower(&parse(src));
        let index = CoreIndex::new();
        let source = SourceIndex::build(&ast, &index);
        let scopes = lexical_scopes(&ast);
        let typer = Typer::with_source(&index, &source).with_lexical_scopes(&scopes);
        let mut i = Interner::new();
        let snaps = typer.collection_shape_snapshots(&ast, &mut i);
        (ast, snaps)
    }

    /// The node id of the first call named `method`, or panic.
    fn call_named(ast: &LoweredAst, method: &str) -> NodeId {
        ast.iter()
            .find_map(|(id, n)| match n {
                Node::Call { method: m, .. } if m == method => Some(id),
                _ => None,
            })
            .unwrap_or_else(|| panic!("call `{method}` present"))
    }

    fn snap(src: &[u8], method: &str) -> Option<&'static str> {
        let (ast, snaps) = coll_snaps(src);
        snaps.get(&call_named(&ast, method)).copied()
    }

    // --- FIRES ------------------------------------------------------------

    /// m01: straight-line `<<` widens the seed to `Array`, and the branch-
    /// contained mutation that follows joins IDENTICALLY (both edges already
    /// `Nominal[Array]`), so the use after the `if` still dispatches on Array.
    #[test]
    fn coll_m01_straight_line_then_branch_mutation_fires() {
        assert_eq!(
            snap(
                b"def f(c)\n  output = []\n  output << 'a'\n  output << 'b' if c\n  output.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            Some("Array"),
        );
    }

    /// m02: a block-contained `<<` on a captured local REPLACES the outer
    /// binding (`widen_after_block`) — an unmutated seed is enough.
    #[test]
    fn coll_m02_each_block_mutation_fires() {
        assert_eq!(
            snap(
                b"def f(xs)\n  output = []\n  xs.each do |x|\n    output << x\n  end\n  output.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            Some("Array"),
        );
    }

    /// m02b: the mutation inside the block is itself BRANCH-contained. The
    /// reference's `widen_after_block` is a syntactic walk against the outer
    /// scope (its doc names `arr.push(x) if cond`), so this still fires — unlike
    /// the same shape in the METHOD body (m20), which goes through `Scope#join`.
    /// The gitlab jira-tracker / ddl-lock survey rows have exactly this shape.
    #[test]
    fn coll_m02b_branch_contained_block_mutation_fires() {
        assert_eq!(
            snap(
                b"def f(xs, c)\n  output = []\n  xs.each do |x|\n    if c\n      output << x\n    end\n  end\n  output.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            Some("Array"),
        );
        // …and the modifier form behind a `next` guard (the ddl-lock row).
        assert_eq!(
            snap(
                b"def f(xs)\n  output = []\n  xs.each do |x|\n    next if x.nil?\n\n    output << x if x\n  end\n  output.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            Some("Array"),
        );
    }

    /// A block-local seed must not leak outwards: only a local ALREADY carrying
    /// a collection at the call can widen.
    #[test]
    fn coll_block_mutation_needs_outer_carrier() {
        assert_eq!(
            snap(
                b"def f(xs)\n  xs.each do |x|\n    inner = []\n    inner << x\n  end\n  inner.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            None,
        );
    }

    /// m03: `[]=` on a `{}` seed widens to `Hash` and STAYS Hash across further
    /// index assignments (the already-nominal carrier re-asserts itself).
    #[test]
    fn coll_m03_hash_index_assign_fires() {
        assert_eq!(
            snap(
                b"def f(v)\n  project = {}\n  project[:a] = v\n  project[:b] = v\n  project.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            Some("Hash"),
        );
    }

    /// m06: no alias tracking — `b = a; b << 1` widens only `b`; `a` keeps its
    /// `Tuple[]`, which dispatches as Array all the same
    /// (`receiver_descriptor:209`). BOTH uses fire.
    #[test]
    fn coll_m06_alias_both_locals_fire() {
        let (ast, snaps) =
            coll_snaps(b"def f\n  a = []\n  b = a\n  b << 1\n  a.first_zzz\n  b.second_zzz\nend\n");
        assert_eq!(snaps.get(&call_named(&ast, "first_zzz")).copied(), Some("Array"));
        assert_eq!(snaps.get(&call_named(&ast, "second_zzz")).copied(), Some("Array"));
    }

    /// m07: an escape into an UNRESOLVED callee does not widen (the reference
    /// does not model unknown-callee mutation either).
    #[test]
    fn coll_m07_unknown_callee_escape_fires() {
        assert_eq!(
            snap(
                b"def f\n  output = []\n  output << 'a'\n  helper_zzz(output)\n  output.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            Some("Array"),
        );
    }

    /// m14: a multi-write seeds each target with its own `Tuple[]`; mutating one
    /// leaves the other a bare Tuple. Both dispatch as Array.
    #[test]
    fn coll_m14_multi_write_seeds_both_fire() {
        let (ast, snaps) =
            coll_snaps(b"def f\n  a, b = [], []\n  a << 1\n  a.first_zzz\n  b.second_zzz\nend\n");
        assert_eq!(snaps.get(&call_named(&ast, "first_zzz")).copied(), Some("Array"));
        assert_eq!(snaps.get(&call_named(&ast, "second_zzz")).copied(), Some("Array"));
    }

    /// m17: `push` then `concat` — a chain of mutators all keeps the nominal.
    #[test]
    fn coll_m17_push_then_concat_fires() {
        assert_eq!(
            snap(
                b"def f(xs)\n  output = []\n  output.push('a')\n  output.concat(xs)\n  output.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            Some("Array"),
        );
    }

    /// m19: straight-line widening BEFORE a `case` makes every clause's join
    /// edge agree, so the use after the `case` still dispatches on Array. This
    /// is the exact contrast with m18 below.
    #[test]
    fn coll_m19_prewidened_case_mutation_fires() {
        assert_eq!(
            snap(
                b"def f(x)\n  output = []\n  output << 'a'\n  case x\n  when 1 then output << 'b'\n  when 2 then output << 'c'\n  end\n  output.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            Some("Array"),
        );
    }

    /// A plain literal seed with NO mutation at all still dispatches as Array /
    /// Hash inside a `def` body (`Tuple`/`HashShape` project to the collection
    /// descriptor) — the base case the mutation rows build on.
    #[test]
    fn coll_bare_literal_seed_fires() {
        assert_eq!(
            snap(b"def f\n  output = [1, 2]\n  output.frobnicate_zzz\nend\n", "frobnicate_zzz"),
            Some("Array"),
        );
        assert_eq!(
            snap(b"def f\n  h = { a: 1 }\n  h.frobnicate_zzz\nend\n", "frobnicate_zzz"),
            Some("Hash"),
        );
    }

    // --- SILENT (the FP-safety envelope) ----------------------------------

    /// m04: a straight-line rebind kills the carrier.
    #[test]
    fn coll_m04_rebind_silent() {
        assert_eq!(
            snap(
                b"def f(x)\n  output = []\n  output << 'a'\n  output = x\n  output.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            None,
        );
    }

    /// m05: a BRANCH rebind kills it too — the join sees two different carriers.
    #[test]
    fn coll_m05_branch_rebind_silent() {
        assert_eq!(
            snap(
                b"def f(x, c)\n  output = []\n  output << 'a'\n  output = x if c\n  output.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            None,
        );
    }

    /// m08: a Dynamic (parameter) seed is never MINTED into a collection by a
    /// mutation — the decline that keeps us out of the reference's own
    /// runtime-wrong `[]=`-on-a-String rows (bucket E, probe c12).
    #[test]
    fn coll_m08_param_seed_silent() {
        assert_eq!(snap(b"def f(a)\n  a << 1\n  a.frobnicate_zzz\nend\n", "frobnicate_zzz"), None);
    }

    /// m13: same, for a local bound to a Dynamic value.
    #[test]
    fn coll_m13_dynamic_carrier_mutation_silent() {
        assert_eq!(
            snap(
                b"def f(x)\n  output = x\n  output << 1\n  output.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            None,
        );
    }

    /// m15: a REBIND inside a block body kills the outer carrier (only a kept
    /// nominal ever propagates out of a block).
    #[test]
    fn coll_m15_block_rebind_silent() {
        assert_eq!(
            snap(
                b"def f(xs)\n  output = []\n  output << 'a'\n  xs.each { |_x| output = nil }\n  output.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            None,
        );
    }

    /// m18 — LOAD-BEARING: a `case`-contained mutation on a NOT-yet-widened seed
    /// leaves `Tuple[] | Array[…]` after the reference's `Scope#join`, and
    /// `receiver_descriptor` has no `Type::Union` arm, so the reference is
    /// SILENT. We must never model that union.
    #[test]
    fn coll_m18_unwidened_case_mutation_silent() {
        assert_eq!(
            snap(
                b"def f(x)\n  output = []\n  case x\n  when 1 then output << 'a'\n  when 2 then output << 'b'\n  end\n  output.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            None,
        );
    }

    /// m20 — LOAD-BEARING, the `if` twin of m18.
    #[test]
    fn coll_m20_unwidened_if_mutation_silent() {
        assert_eq!(
            snap(
                b"def f(c)\n  output = []\n  output << 'a' if c\n  output.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            None,
        );
    }

    /// m16: an op-write (`output += [1]`) FIRES in the reference (the `Tuple +
    /// Tuple` fold keeps the literal shape) — a deliberate coverage give-up
    /// here, per §5.5 of the spec. Pinned so the give-up stays visible.
    #[test]
    fn coll_m16_op_write_silent_coverage_giveup() {
        assert_eq!(
            snap(
                b"def f\n  output = []\n  output += [1]\n  output.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            None,
        );
    }

    /// m09 / bucket B: an IVAR carrier is never typed by this slice (the
    /// reference DOES fire cross-method; that substrate is its own future
    /// slice).
    #[test]
    fn coll_ivar_carrier_silent() {
        assert_eq!(
            snap(
                b"class K\n  def g\n    @h = {}\n    @h[:a] = 1\n    @h.frobnicate_zzz\n  end\nend\n",
                "frobnicate_zzz",
            ),
            None,
        );
    }

    /// Safe-nav dispatch is outside the envelope on BOTH sides: a `&.` mutation
    /// widens nothing and a `&.` use records nothing.
    #[test]
    fn coll_safe_nav_silent() {
        assert_eq!(
            snap(
                b"def f\n  output = []\n  output << 'a'\n  output&.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            None,
        );
    }

    /// `while`/`until` bodies are unmodeled (the reference fires — probe m10 —
    /// but its `break`/`next` join edges are unprobed, so stage 1 declines).
    #[test]
    fn coll_while_loop_silent() {
        assert_eq!(
            snap(
                b"def f(c)\n  output = []\n  while c\n    output << 1\n  end\n  output.frobnicate_zzz\nend\n",
                "frobnicate_zzz",
            ),
            None,
        );
    }

    /// A `def` body is an INDEPENDENT local scope: a top-level seed never leaks
    /// into it.
    #[test]
    fn coll_def_body_scope_isolation_silent() {
        assert_eq!(
            snap(b"output = []\ndef f\n  output.frobnicate_zzz\nend\n", "frobnicate_zzz"),
            None,
        );
    }
}

/// Collection-shape **stage 2** — the chain ROOTS. Each micro-slice gets a
/// fire + decline pair, mirroring the oracle probes recorded in
/// `docs/notes/20260807-collection-shape-slice-spec.md` §1.
#[cfg(test)]
mod collection_shape_stage2_tests {
    use super::*;
    use rigor_parse::{lower, parse};

    /// The type of the LAST receiver-bearing call in `src`, rendered. Wired like
    /// the analyze pass (source index + lexical scopes) so the shadow/lexical
    /// gates the stage-2 arms consult are live.
    fn ty_of_last_recv_call(src: &[u8]) -> String {
        let ast = lower(&parse(src));
        let index = CoreIndex::new();
        let source = SourceIndex::build(&ast, &index);
        let scopes = lexical_scopes(&ast);
        let typer = Typer::with_source(&index, &source).with_lexical_scopes(&scopes);
        let mut i = Interner::new();
        let env = TypeEnv::new();
        let call_id = ast
            .iter()
            .filter_map(|(id, n)| matches!(n, Node::Call { receiver: Some(_), .. }).then_some(id))
            .last()
            .unwrap();
        let ty = typer.type_of(&ast, call_id, &env, &mut i);
        let name = index.class_name_of(&i, ty).map(str::to_string);
        name.unwrap_or_else(|| rigor_types::describe(&i, ty))
    }

    // --- 2a: `Dir.glob` / `Dir.[]` -----------------------------------------

    /// FIRE (oracle c02): `Dir.glob(...)` types `Array` on the BLOCK-FREE call
    /// path even though its block overload returns `nil`. `Dir[...]` (single
    /// overload, oracle c01) is the untouched control that already worked.
    #[test]
    fn s2a_dir_glob_block_free_types_array() {
        assert_eq!(ty_of_last_recv_call(b"x = Dir.glob('*.rb')\n"), "Array");
        assert_eq!(ty_of_last_recv_call(b"x = Dir['*.rb']\n"), "Array");
    }

    /// DECLINE: a singleton whose overloads diverge for a reason OTHER than a
    /// block (`Regexp.last_match`: `MatchData?` vs `String?`) is unchanged — the
    /// block-free slot is empty for it, so the arm cannot invent a return.
    /// A BLOCK-bearing `Dir.glob { }` also stays Dynamic (it routes to
    /// `type_block_call`, which reads `block_returns`, not this slot).
    #[test]
    fn s2a_divergent_overloads_and_block_form_decline() {
        assert_eq!(ty_of_last_recv_call(b"m = Regexp.last_match(2)\n"), "Dynamic[top]");
        assert_eq!(
            ty_of_last_recv_call(b"x = Dir.glob('*.rb') { |f| f }\n"),
            "Dynamic[top]"
        );
    }

    // --- 2b: `ENV` ----------------------------------------------------------

    /// FIRE (oracle c03): `ENV` is declared `ENV: RBS::Unnamed::ENVClass` in
    /// core RBS, so `ENV.keys` types `Array`.
    #[test]
    fn s2b_env_object_constant_types_its_declared_return() {
        assert_eq!(ty_of_last_recv_call(b"def f\n  x = ENV.keys\nend\n"), "Array");
        assert_eq!(ty_of_last_recv_call(b"def f\n  x = ENV.to_hash\nend\n"), "Hash");
    }

    /// DECLINE: a PROJECT `ENV` constant makes the core declaration the wrong
    /// surface. Probed at the pin: with `ENV = Object.new` the reference reports
    /// `undefined method 'keys' for Object` — a different diagnostic — so typing
    /// the chain as `Array` here was an oracle FP. A project `module ENV`
    /// declines through the same lexical shadow gate, and a method the declared
    /// class does not define declines for lack of a return.
    #[test]
    fn s2b_project_env_shadow_declines() {
        assert_eq!(
            ty_of_last_recv_call(b"ENV = Object.new\ndef f\n  x = ENV.keys\nend\n"),
            "Dynamic[top]"
        );
        assert_eq!(
            ty_of_last_recv_call(b"module ENV\nend\ndef f\n  x = ENV.keys\nend\n"),
            "Dynamic[top]"
        );
        assert_eq!(
            ty_of_last_recv_call(b"def f\n  x = ENV.frobnicate_zzz\nend\n"),
            "Dynamic[top]"
        );
    }

    // --- 2c: block-free INSTANCE returns (`String#split`) -------------------

    /// FIRE (oracle c08b): `String#split` declares `(…) { … } -> self` beside
    /// `(…) -> Array[String]`; the block-free call site types `Array`.
    #[test]
    fn s2c_string_split_block_free_types_array() {
        assert_eq!(ty_of_last_recv_call(b"x = 'a:b'.split(':', 2)\n"), "Array");
    }

    /// DECLINE: the BLOCK form of the same method does not read the block-free
    /// slot (`'a'.split(':') { }` types through `block_returns`, which records
    /// the `self` return ⇒ String), and a method with no block overload is
    /// unchanged.
    #[test]
    fn s2c_block_form_and_plain_methods_unchanged() {
        assert_eq!(ty_of_last_recv_call(b"x = 'a:b'.split(':') { |p| p }\n"), "String");
        assert_eq!(ty_of_last_recv_call(b"x = 'a'.upcase\n"), "String");
        assert_eq!(ty_of_last_recv_call(b"x = 'a'.frobnicate_zzz\n"), "Dynamic[top]");
    }

    // --- 2e: `::`-qualified constant paths ----------------------------------

    /// FIRE (oracle u1): the SAME C5 literal constant reached by a fully
    /// qualified path types identically to the lexical read — the reference
    /// resolves all three spellings to `[:high, :low]`.
    const U1: &[u8] = b"module A\n  module B\n    class C\n      PR = { high: 1, low: 2 }.freeze\n\n      def lexical\n        x = PR.keys\n      end\n\n      def qualified\n        x = ::A::B::C::PR.keys\n      end\n    end\n  end\nend\n";

    #[test]
    fn s2e_qualified_constant_path_resolves() {
        // The last receiver-bearing call is the qualified spelling's `.keys`.
        assert_eq!(ty_of_last_recv_call(U1), "Array");
    }

    /// DECLINE: an AMBIGUOUS path — two DIFFERENT qualified constants that the
    /// use site's lexical candidates both reach — resolves to nothing rather
    /// than guessing which one Ruby would pick.
    #[test]
    fn s2e_ambiguous_qualified_path_declines() {
        // `B::C::PR` at a use site inside `module A` matches BOTH the top-level
        // `B::C::PR` and `A::B::C::PR`.
        let src = b"module B\n  module C\n    PR = { top: 1 }.freeze\n  end\nend\n\nmodule A\n  module B\n    module C\n      PR = { nested: 1 }.freeze\n    end\n  end\n\n  class Use\n    def f\n      x = B::C::PR.keys\n    end\n  end\nend\n";
        assert_eq!(ty_of_last_recv_call(src), "Dynamic[top]");
        // An unknown path declines too.
        assert_eq!(
            ty_of_last_recv_call(b"def f\n  x = ::No::Such::CONST_ZZZ.keys\nend\n"),
            "Dynamic[top]"
        );
    }

    /// DECLINE (measured on the sweep): a resolved path whose constant is NOT
    /// lexically visible from the use site stays untyped, exactly like the bare
    /// spelling. Gitlab's `Gitlab::GitalyClient::DiffBlob::ATTRS` read from a
    /// SIBLING class `…::DiffBlobsStitcher` is the shape; the reference is
    /// silent there, and folding it was an oracle FP on the first cut.
    #[test]
    fn s2e_cross_namespace_path_declines() {
        let src = b"module G\n  module Client\n    class Blob\n      ATTRS = { a: 1 }.freeze\n    end\n\n    class Stitcher\n      def f\n        x = G::Client::Blob::ATTRS.keys\n      end\n    end\n  end\nend\n";
        assert_eq!(ty_of_last_recv_call(src), "Dynamic[top]");
    }

    /// DECLINE (measured on the sweep): a NILABLE declared return on the object
    /// constant's class. `ENVClass#[]` is `(String) -> String?`; the reference
    /// carries `String | nil` and declines dispatch, so the chain must stay
    /// untyped rather than become a bare `String`. `ENV.keys` (non-nilable) is
    /// the positive control in `s2b_env_object_constant_types_its_declared_return`.
    #[test]
    fn s2b_nilable_object_constant_return_declines() {
        assert_eq!(ty_of_last_recv_call(b"def f\n  x = ENV['HOME']\nend\n"), "Dynamic[top]");
    }
}
