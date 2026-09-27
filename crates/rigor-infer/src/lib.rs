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
mod flow_writes;
mod class_narrowing;

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use rigor_index::CoreIndex;
use rigor_parse::{BlockParamKind, JumpKind, LoweredAst, Node, NodeId, StatementsKind};
use rigor_types::{Interner, Scalar, ShapeKey, ShapeMember, Type, TypeId};

pub use folding::RubyFolder;
pub use source_index::{
    lexical_scopes, method_body_spans, ConstLit, DefKind, Harvest, ParamBoundReturn, SourceIndex,
    SOURCE_CLASS_BASE,
};
pub use flow_writes::collect_flow_writes;
pub(crate) use flow_writes::*;
pub use class_narrowing::ClassNarrowing;

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
        Node::IntegerLit { value, .. } => value.map(ShapeKey::Int),
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
    /// The analyzed file's [`rigor_parse::FileKey`] — the per-file
    /// def-attribution overlay index (`SourceIndex::project_declares_method`
    /// / `is_toplevel_def` consult `file_defs` through it). `None` for callers
    /// that do not set it ⇒ the union-over-all-files answer.
    file_key: Option<&'i rigor_parse::FileKey>,
}

/// A shared empty lexical-scope slice — the default `lexical_scopes` for a
/// [`Typer`] built without the C1 per-file scopes.
const EMPTY_LEXICAL_SCOPES: &[(rigor_parse::Span, Vec<String>)] = &[];

impl<'i> Typer<'i> {
    /// Build a typer over a borrowed core index, with an EMPTY source index
    /// (no in-source typing). Kept for callers that predate tier-4.
    pub fn new(index: &'i CoreIndex) -> Self {
        Typer { index, source: empty_source(), folder: None, lexical_scopes: EMPTY_LEXICAL_SCOPES, file_key: None }
    }

    /// Build a typer over a borrowed core index AND a per-run [`SourceIndex`],
    /// enabling `X.new` instance typing and in-source method resolution.
    pub fn with_source(index: &'i CoreIndex, source: &'i SourceIndex) -> Self {
        Typer { index, source, folder: None, lexical_scopes: EMPTY_LEXICAL_SCOPES, file_key: None }
    }

    /// As [`Typer::with_source`], plus the ADR-0008 real-Ruby folder for
    /// sidecar-routed constant folds. `None` is byte-identical to
    /// [`Typer::with_source`] (the sound subset).
    pub fn with_source_and_folder(
        index: &'i CoreIndex,
        source: &'i SourceIndex,
        folder: Option<&'i (dyn folding::RubyFolder + Sync)>,
    ) -> Self {
        Typer { index, source, folder, lexical_scopes: EMPTY_LEXICAL_SCOPES, file_key: None }
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

    /// Attach the analyzed file's [`rigor_parse::FileKey`] so the
    /// source-index def queries resolve its per-file overlay.
    pub fn with_file_key(mut self, key: &'i rigor_parse::FileKey) -> Self {
        self.file_key = Some(key);
        self
    }

    /// The analyzed file's [`rigor_parse::FileKey`], `None` when unset.
    pub fn file_key(&self) -> Option<&rigor_parse::FileKey> {
        self.file_key
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
            // Issue #540 (`fc3b8b42`) — a literal shape the file itself mutates.
            // `Type::Combinator.dynamic(literal)` in the reference: the class
            // survives for dispatch, the SHAPE stops licensing the negative
            // rules (a `Dynamic[Tuple]` projects to nothing, so `if LN[0]` no
            // longer folds to a truthy constant).
            ConstLit::Widened(inner) => {
                let t = self.intern_const_lit(inner, interner);
                interner.intern(Type::Dynamic(t))
            }
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
            Node::IntegerLit { value: Some(value), .. } => {
                interner.intern(Type::Constant(Scalar::Int(*value)))
            }
            // A Bignum: the reference pins it, but no `i64` scalar can.
            Node::IntegerLit { value: None, .. } => self.nominal_or_untyped("Integer", interner),
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
            Node::Call {
                receiver: Some(r),
                method,
                args,
                block_body,
                block_span,
                block_params,
                explicit_arg_list,
                safe_nav,
                ..
            } => {
                let (r, method, block_body, block_span, block_params, explicit_arg_list, safe_nav) = (
                    *r,
                    method.clone(),
                    block_body.clone(),
                    *block_span,
                    block_params.clone(),
                    *explicit_arg_list,
                    *safe_nav,
                );
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
                    self.type_block_call(
                        ast,
                        r,
                        &method,
                        &block_body,
                        block_span,
                        &block_params,
                        explicit_arg_list,
                        safe_nav,
                        env,
                        interner,
                    )
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
                if let Some(lit) = self.source.literal_constant(name, prefix, ast.file_key()) {
                    return self.intern_const_lit(lit, interner);
                }
                // Collection-shape stage 2e: the same C5 value reached by a
                // FULLY-QUALIFIED path (`::A::B::C::CONST`), which arrives as one
                // `ConstantRead` whose `name` is the whole path and so misses the
                // bare-name map above. Ambiguity declines (see
                // `SourceIndex::qualified_literal_constant`).
                if let Some(lit) =
                    self.source.qualified_literal_constant(name, prefix, ast.file_key())
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

        // #521 (upstream `3d5dddbb`, the `v0.3.4 → v0.3.8` re-pin): an UNTYPED
        // argument cannot discriminate between a method's overloads, so a
        // conversion whose overloads DISAGREE on their return must not pin one.
        // Upstream's strict and alias passes now decline on an untyped argument,
        // the gradual pass answers EVERY arity-compatible overload, and the
        // dispatch joins their returns as `Dynamic[union]` — on which no negative
        // rule fires. Reproduced here as a DECLINE for the four folds whose
        // Kernel overloads disagree, measured at the pin (`ffb456b0`):
        // `Float(u)`, `Integer(u)`, `Integer(u, 16)`, `Array(u)` and `rand(u)`
        // are all reference-SILENT (rows a1/a2/a17/a4/a5), and so is the
        // `def.ivar-write-mismatch` the pinned `Float` used to license (fixture
        // 60 row a18).
        //
        // `String` and `format`/`sprintf` are NOT on the list and keep their
        // answers: each has a single matching overload, so upstream's join is
        // that one return and both engines still fire (rows a3/a9/a32/a33).
        // `Hash` is not on it either — it already declines for a non-shape
        // argument, and the reference's `Hash(u)` answer is a recorded coverage
        // gap (row a26), not an FP.
        //
        // WHY THE GATE IS NOT UPSTREAM'S BARE TYPE TEST. Upstream asks
        // `untyped_arg?(t)` — "is this exactly `Dynamic[Top]`". rigor-rs CANNOT
        // ask that here: a use site inside a method body reads an EMPTY
        // `TypeEnv` (rules `ScopedEnv::at` — a def body is an independent local
        // scope and the flat top-level env types the wrong value there), so
        // EVERY def-body local read answers `Dynamic[top]` whether or not the
        // reference knows its type. Measured at the pin: with the bare type test
        // in place, `s = "x"; Float(s)`, `s = "x" if s.nil?; Array(s)`,
        // `return unless s.is_a?(String); Integer(s, 16)` and
        // `return unless s.is_a?(Integer); rand(s)` all went SILENT while the
        // reference keeps firing (rows q4/a20/a25/a31) — four must-still-fire
        // controls, silenced by a decline that "does strictly less".
        //
        // A syntactic REACH analysis instead ([`Typer::arg_reach`]): which values
        // can reach the argument's root on the reference, and is one of them the
        // untyped carrier? An unwritten, unguarded parameter is; an arbitrary
        // call chain over an untyped root (`kwargs[:upload_duration]`, fixture
        // 60) is too.
        //
        // #1021 (upstream `5496acd6`, the `v0.3.8 -> e59b7b89` re-pin) widened
        // upstream's test from `untyped_arg?` to `imprecise_arg?` — the bare
        // carrier OR a union with an untyped member — so a CONDITIONAL rebind
        // that leaves the parameter reachable (`s = "x" if s.nil?`, the
        // reference's `Dynamic[top] | "x"`) now declines too (rows a7/a20/q3),
        // while an unconditional one (`s = "x"`, row q4) still fires. `rand` is
        // the one conversion whose join a union's precise members can still pin
        // (see [`Reach`]), so it asks [`Reach::declines_rand`] instead.
        //
        // The decline ANSWERS `Dynamic[top]` rather than returning `None`,
        // because the explicit `Kernel.Float(u)` spelling routes here too and a
        // `None` there falls through to the singleton-RBS tier, which would
        // re-pin the very return this declines (row a19, reference-silent).
        if matches!(method, "Float" | "Integer" | "Array" | "rand") {
            let untyped = interner.untyped();
            let declines = args.iter().any(|&a| {
                self.type_of(ast, a, env, interner) == untyped && {
                    let reach = self.arg_reach(ast, a);
                    if method == "rand" {
                        reach.declines_rand()
                    } else {
                        reach.untyped
                    }
                }
            });
            if declines {
                return Some(untyped);
            }
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
    fn arg_reach(&self, ast: &LoweredAst, arg: NodeId) -> Reach {
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
    fn local_reach(
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
                // A String lookup can answer `nil`, so a stale value flips the
                // result's CLASS. The top-level flat env keeps a local's first
                // literal across `<<`, `+=`, branch and block writes, so a lookup
                // that reads a local declines to the RBS answer (#149 review:
                // `buf = ""; buf << "x"; buf[0].upcase` fired `for nil`).
                let stale_risk = folding::is_str_lookup(&scalar, method)
                    && std::iter::once(receiver)
                        .chain(args.iter().copied())
                        .any(|id| ast.reads_local_within(ast.get(id).span()));
                if let Some(folded) =
                    (!stale_risk).then(|| folding::fold(&scalar, method, &arg_scalars)).flatten()
                {
                    return interner.intern(Type::Constant(folded));
                }
                // ADR-0008 sidecar fallback: the Rust core declined, but if this
                // is a `sidecar_foldable` pure call and a real-Ruby folder is
                // wired (full-fidelity mode), execute it there. A declined /
                // absent folder leaves the value widened (sound subset).
                if let Some(folder) = self.folder {
                    if folding::sidecar_foldable(folding::scalar_class(&scalar), method)
                        && !folding::sidecar_blows_up(method, &arg_scalars)
                    {
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
                // #521 in the GENERIC dispatch (issue #118). The flat return
                // slot answers a BARE `Nominal[C]`; the reference answers what
                // the surviving overloads actually join to. When those differ —
                // a nilable `C?`, or two candidates the erased head hid — a
                // REFERENCE-UNTYPED argument means the reference cannot fold the
                // call to a value either, so its carrier is the union and no
                // negative rule fires on it. Decline to `Dynamic[top]` there.
                let untyped_arg = self.rbs_dispatch_declines_on_untyped_arg(
                    class_name, method, ast, args, env, interner,
                );
                if untyped_arg {
                    return interner.untyped();
                }
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

    /// Whether the tier-3 flat-slot answer for `class_name#method` must be
    /// WITHHELD at this call site — the generic-dispatch half of upstream #521
    /// (`3d5dddbb`, PR #537), issue #118.
    ///
    /// ## What the reference does, and where the flat slot diverges
    ///
    /// The reference selects the overloads that match the call's arity and block
    /// shape and joins their translated returns (`join_candidate_returns`): one
    /// candidate answers its own return, several with the SAME return answer it,
    /// several with distinct returns answer `Dynamic[union]` — a carrier no
    /// negative rule fires on. rigor-rs has no RBS type translator, so tier 3
    /// reads a per-method flat slot instead, which is right only when the
    /// reference's join is a single BARE nominal. Two measured ways it is not
    /// (both oracle-measured at pin `ffb456b0`):
    ///
    /// 1. **A nilable return.** `String#[]`'s four overloads all return
    ///    `String?`; the reference answers `String | nil` and stays silent,
    ///    while `method_return` drops the nil bit and hands tier 3 a bare
    ///    `String` that `call.undefined-method` witnesses on — issue #118's
    ///    `"abc"[u].frobnicate` (rows a15, and the `slice` / `byteslice` /
    ///    `index` / `rindex` / `getbyte` / `byteindex` / `assoc` / `rassoc` /
    ///    `Float#<=>` twins).
    /// 2. **Returns that agree only after ERASURE.** `method_signature` compares
    ///    the head class NAME, so `Array[[E, X]]` and `Array[Array[E | U]]`
    ///    "agree" on `Array`; the reference joins them to `Dynamic[union]` and
    ///    stays silent. This is #521's own `[true] * n` class of defect — here
    ///    `Array#product`, `Array#zip` and `String#scan`, whose candidate sets
    ///    this predicate reproduces exactly.
    ///
    /// ## The gate
    ///
    /// Withhold only when an argument is REFERENCE-IMPRECISE — untyped, or
    /// since #1021 (`5496acd6`) a union with an untyped member
    /// ([`Typer::arg_reach`], the same analysis the Kernel folds use:
    /// `s = 1 if s.nil?; "abc"[s]` and `"abc".index(s)` are reference-silent
    /// now, rows r01/r11). An imprecise argument is what makes the reference
    /// unable to value-fold the call, which is what leaves the union standing:
    /// with a LITERAL argument the reference folds `"abc"[0]` to `"a"` and
    /// fires, and rigor-rs's bare `String` matches that row — so gating on
    /// imprecision keeps every such row (`"abc"[0]`, `"abc"[1..]`,
    /// `"abc".byteslice(1)`, `"abc".index("b")`, `"abc".slice(1)`, all measured
    /// BOTH before and after). A union's precise members can narrow the
    /// reference's candidate set back to one agreeing return, which this
    /// declines on anyway — a coverage loss, never a false positive.
    ///
    /// The index test runs FIRST and the arena walk only if it says the answer
    /// is at risk: `arg_reach` is two arena scans per root visited (see its own
    /// COST note), and tier 3 is the hot dispatch path.
    ///
    /// A class-GUARDED parameter is refused by that allow-list (the reference
    /// types it precisely), but for a NILABLE return it declines anyway through
    /// [`Typer::arg_is_guarded_parameter`] (issue #121): a guarded parameter is
    /// never a value the reference can fold, so its join keeps the nil arm —
    /// `return unless u.is_a?(Integer); "abc"[u].typo` is reference-silent.
    ///
    /// Known withholdings, recorded not chased: an argument the reference types
    /// but rigor-rs cannot see as such stays firing (a `case`/`when` or
    /// `C === u` guard, a chain over a guarded root, a guard after a
    /// conditional rebind — see fixture 106's trailer), because the allow-list
    /// is deliberately syntactic; and
    /// an overload written as an untyped function (`(?) -> untyped`) is not
    /// retained in `method_overloads` at all, so it cannot contribute a
    /// disagreement (18 occurrences in the vendored RBS, none of them a
    /// multi-overload method whose flat slot answers).
    fn rbs_dispatch_declines_on_untyped_arg(
        &self,
        class_name: &str,
        method: &str,
        ast: &LoweredAst,
        args: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> bool {
        if args.is_empty() || self.rbs_join_is_one_bare_nominal(class_name, method, args.len()) {
            return false;
        }
        let nilable = matches!(self.index.method_return_nilable(class_name, method), Some((_, true)));
        let untyped = interner.untyped();
        args.iter().any(|&a| {
            self.type_of(ast, a, env, interner) == untyped
                && (self.arg_reach(ast, a).untyped
                    || (nilable && self.arg_is_guarded_parameter(ast, a)))
        })
    }

    /// Issue #121 — the NILABLE half of tier 3's decline, for the argument the
    /// untyped allow-list deliberately refuses: a bare local that a class guard
    /// narrows (`return unless u.is_a?(Integer); "abc"[u]`).
    ///
    /// The reference types that argument as the guard's `Nominal` (or leaves it
    /// `Dynamic` where the guard does not dominate the read) — never a value it
    /// can constant-fold — so the call cannot fold and the carrier stays the RBS
    /// join. When every overload returns `C?` that join is `C | nil`, on which
    /// no negative rule fires (`"abc"[u].frobnicate` and `"abc"[u].upcase` are
    /// both reference-silent). The flat slot's bare `C` is what fired here.
    ///
    /// The test is "only the untyped carrier reaches the root once the class
    /// guards are stepped over": a parameter the region never rebinds. Any
    /// precise write reaching the read (`u = 1; return unless u.is_a?(Integer)`)
    /// could leave the reference a `Constant` to fold, and the rows a25/a31 of
    /// the Kernel-fold arc ride exactly that — so it does not qualify, and
    /// neither does a chain over the root (`u <=> 1` can answer a union of
    /// literals the reference does fold). Only the nilable case asks: a guarded
    /// argument can narrow the erasure family (`Array#product`) back to one
    /// overload, and the reference then fires.
    fn arg_is_guarded_parameter(&self, ast: &LoweredAst, arg: NodeId) -> bool {
        let Node::LocalVariableRead { name, span, .. } = ast.get(arg) else {
            return false;
        };
        let reach = self.local_reach(ast, name, *span, &mut vec![format!("{name}@{}", span.0)], true);
        reach.untyped && !reach.precise
    }

    /// The index half of [`Typer::rbs_dispatch_declines_on_untyped_arg`]:
    /// whether the reference's join over the overloads that match `argc` and a
    /// BLOCK-FREE call site is a single bare nominal — the only shape tier 3's
    /// flat slot can spell faithfully.
    ///
    /// `true` (keep the flat answer) when the return is non-nilable AND every
    /// candidate declares the same verbatim return. `true` also when the method
    /// retains no overload shapes (nothing to contradict the slot) or when no
    /// candidate matches the arity — the reference then falls back to a SINGLE
    /// overload (`overloads.find { !requires_block } || overloads.first`) and
    /// pins it, exactly as the flat slot does.
    ///
    /// Candidate selection is deliberately PERMISSIVE where the retained shapes
    /// are coarse (a trailing positional does not raise the minimum arity, and
    /// no per-argument type filtering is applied): admitting an extra candidate
    /// can only turn agreement into disagreement, i.e. make the port answer
    /// LESS, which is FP-safe.
    fn rbs_join_is_one_bare_nominal(&self, class_name: &str, method: &str, argc: usize) -> bool {
        // (1) A nilable return — the reference's carrier is `C | nil`.
        if matches!(self.index.method_return_nilable(class_name, method), Some((_, true))) {
            return false;
        }
        // (2) The candidates the reference would join.
        let Some(overloads) = self.index.method_overloads(class_name, method) else {
            return true;
        };
        let mut agreed: Option<&str> = None;
        for ov in overloads {
            // A block-less call site never engages a block-REQUIRING overload,
            // and the reference skips an overload with a required keyword
            // (`rejects_keyword_required?`) because no keyword is passed here.
            if ov.block_required || ov.has_required_keywords {
                continue;
            }
            if argc < ov.required_positionals.len() {
                continue;
            }
            if !ov.has_rest_positionals
                && !ov.has_trailing_positionals
                && argc > ov.required_positionals.len() + ov.optional_positionals.len()
            {
                continue;
            }
            match agreed {
                None => agreed = Some(ov.return_form.as_str()),
                Some(prev) if prev == ov.return_form => {}
                Some(_) => return false,
            }
        }
        true
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
    // The `explicit_arg_list`/`block_params`/`safe_nav` additions took this
    // past clippy's limit; all are call-site descriptors, so bundling them
    // would be ceremony for a single caller (matches
    // `exactly_once_block_call`'s allow above).
    #[allow(clippy::too_many_arguments)]
    fn type_block_call(
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

    /// [`Self::build_toplevel_env`] for the `check` rules: the same straight-line
    /// binder, except that every statement it does not bind WIDENS (to
    /// `Dynamic`) each top-level local rebound inside it, in statement order.
    ///
    /// The flat binder sees only a top-level `x = …` / `a, b = …`; a rebind
    /// nested in an `if`, a loop, a block, a `begin`, or an `x += …` is
    /// invisible to it, so the local kept its FIRST type past the construct.
    /// The reference joins every path that leaves the construct — including a
    /// `next` / `break` path, since upstream #1248 (loops) and #1215 (blocks) —
    /// so `w = String.new; while …; w = i; next; end; w.even?` and
    /// `n = String.new; xs.each { |e| n = e; next }; n.even?` are silent there
    /// while the flat env still fired `for String` (rigor-rs#133). The port has
    /// no flow evaluator behind this env to join into, so it declines instead:
    /// a strict loss of information, which cannot add a diagnostic. A local
    /// the construct does not rebind keeps its type, so the no-rebind controls
    /// still fire.
    ///
    /// A write inside a `def` / `class` / `module` body is a different local
    /// scope and widens nothing. A write in a `->` or block body does widen: a
    /// closure may run and rebind a CAPTURED local. A write to a name the
    /// block/lambda binds itself (a parameter or `;`-declared block-local —
    /// [`Node::Call::block_locals`], rigor-rs#166) shadows the top-level name
    /// and widens nothing either.
    pub fn build_toplevel_check_env(&self, ast: &LoweredAst, interner: &mut Interner) -> TypeEnv {
        let mut env = TypeEnv::new();
        let body = match ast.get(ast.root()) {
            Node::Program { body, .. } => body.clone(),
            _ => return env,
        };
        let rebinds = toplevel_rebinds(ast);
        for stmt in body {
            self.bind_check_statement(ast, stmt, &mut env, &rebinds, interner);
        }
        env
    }

    /// One statement of [`Self::build_toplevel_check_env`]: a direct write binds
    /// as [`Self::bind_statement`] does, after widening the rebinds nested in
    /// its value (`x = xs.each { |e| w = e }`); any other statement widens every
    /// rebind inside it.
    fn bind_check_statement(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        env: &mut TypeEnv,
        rebinds: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
    ) {
        match ast.get(id) {
            Node::LocalVariableWrite { value, .. } | Node::MultiWrite { value, .. } => {
                let vspan = ast.get(*value).span();
                widen_flow_writes(rebinds, vspan, env, interner);
                self.bind_statement(ast, id, env, interner);
            }
            // Only a real statement sequence is straight-line code. A recovery
            // carrier (a `rescue` modifier, `super(…)`, …) runs its writes
            // conditionally or out of order, so it widens; an inert one
            // (`defined?`, `END`, `BEGIN`) has no writes in `rebinds` and so
            // changes nothing (rigor-rs#153).
            Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
                for s in body.clone() {
                    self.bind_check_statement(ast, s, env, rebinds, interner);
                }
            }
            other => widen_flow_writes(rebinds, other.span(), env, interner),
        }
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
            // A recovery / inert carrier is not straight-line code: it goes to
            // the widening arm below (rigor-rs#153). An inert carrier's writes
            // are not in `writes`, so it leaves `env` as the reference does.
            Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
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
            // A write under `defined?` / `END` / `BEGIN` / `super` / `yield`
            // never reaches the scope on the reference (rigor-rs#153), so the
            // flat env does not bind it either (`type-of` on the later read
            // says the earlier type, as the reference does). A recovery
            // carrier is still bound as before: this env has no widening.
            Node::Statements { kind: StatementsKind::Inert, .. } => {}
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
            // A carrier is descended so the uses in it are recorded, as before;
            // what differs by kind is the state it leaves behind (rigor-rs#153).
            Node::Statements { body, kind, span } => {
                let (body, kind, span) = (body.clone(), *kind, *span);
                match kind {
                    StatementsKind::Sequence => {
                        self.nil_flow_scope(ast, &body, tenv, nenv, penv, writes, interner, out);
                    }
                    // Its writes may not run, or not in this order: widen them
                    // and drop their facts after the descent. A `Jump` carrier
                    // holds the jump's VALUE expressions (`break (x = 1)`), so
                    // it reads exactly like `Recovered` here — the write runs
                    // only when the jump does, which nothing upstream proves.
                    StatementsKind::Recovered | StatementsKind::Jump(_) => {
                        self.nil_flow_scope(ast, &body, tenv, nenv, penv, writes, interner, out);
                        widen_flow_writes(writes, span, tenv, interner);
                        widen_penv_writes(writes, span, penv);
                        for (w, name) in writes {
                            if span.0 <= w.0 && w.1 <= span.1 {
                                nenv.remove(name);
                            }
                        }
                    }
                    // Its writes never reach the scope: record the uses in a
                    // written value, bind nothing.
                    StatementsKind::Inert => {
                        for s in body {
                            match ast.get(s) {
                                Node::LocalVariableWrite { value, .. }
                                | Node::LocalVariableOpWrite { value, .. }
                                | Node::MultiWrite { value, .. } => {
                                    let value = *value;
                                    self.nil_flow_expr(ast, value, tenv, nenv, penv, writes, interner, out);
                                }
                                _ => self.nil_flow_stmt(ast, s, tenv, nenv, penv, writes, interner, out),
                            }
                        }
                    }
                }
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
            // An inert carrier leaves the reference's scope unchanged
            // (rigor-rs#153): a write in it does not bind, but the uses in it
            // are still recorded, as before.
            Node::Statements { body, kind: StatementsKind::Inert, .. } => {
                for s in body.clone() {
                    match ast.get(s) {
                        Node::LocalVariableWrite { value, .. }
                        | Node::LocalVariableOpWrite { value, .. }
                        | Node::MultiWrite { value, .. } => {
                            let value = *value;
                            self.coll_flow_expr(ast, value, tenv, ctx, interner, out, stmt_position);
                        }
                        _ => self.coll_flow_stmt(ast, s, tenv, ctx, interner, out, stmt_position),
                    }
                }
            }
            Node::Statements { body, kind, span } => {
                let (body, kind, span) = (body.clone(), *kind, *span);
                self.coll_flow_scope(ast, &body, tenv, ctx, interner, out, stmt_position);
                // A recovery carrier's writes may not run, or not in this order:
                // the uses in it are recorded as before, then its writes widen.
                if kind == StatementsKind::Recovered {
                    widen_flow_writes(ctx.writes, span, tenv, interner);
                }
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
                let widened = match local.as_ref().and_then(|name| {
                    let ty = *tenv.get(name)?;
                    self.coll_widen_for_mutator(interner, ty, &method)
                        .map(|c| (name.clone(), c, ty))
                }) {
                    None => None,
                    // `Inference::MutationRejoin` (`1ad7351e`, #580, `v0.3.9`):
                    // the later store grows the carrier's value side instead of
                    // being swallowed by the already-widened nominal. Read off
                    // the PRE-call carrier, like the widening itself.
                    Some((name, cls, pre_ty)) => {
                        let mut added: Vec<TypeId> = Vec::new();
                        for &a in Typer::coll_store_value_args(&method, &args) {
                            for m in self.coll_store_value_classes(ast, a, tenv, interner) {
                                if !added.contains(&m) {
                                    added.push(m);
                                }
                            }
                        }
                        // Canonical order: the member set is compared only by the
                        // interned `TypeId` two branch edges end up with, so the
                        // order stores happened in must not separate them.
                        let grown = |base: &[TypeId]| {
                            let mut members = base.to_vec();
                            for &m in &added {
                                if !members.contains(&m) {
                                    members.push(m);
                                }
                            }
                            members.sort_unstable();
                            members
                        };
                        match interner.get(pre_ty) {
                            // `widen_union` (mutation_widening.rb:316) widens
                            // EACH arm and `Combinator.union` re-joins, deduping
                            // only structurally identical arms — the edges
                            // converge iff every arm grows to the same set.
                            // Collapsing to one merged carrier fired where the
                            // oracle kept the union (probe r2).
                            Type::Union(arms) => {
                                let arms = arms.clone();
                                let mut minted = Vec::with_capacity(arms.len());
                                for arm in arms {
                                    let members =
                                        grown(&Typer::coll_value_members(interner, arm));
                                    if let Some(t) =
                                        self.coll_nominal_with(interner, cls, &members)
                                    {
                                        minted.push(t);
                                    }
                                }
                                minted.sort_unstable();
                                minted.dedup();
                                match minted.as_slice() {
                                    [] => None,
                                    [only] => Some((name, *only)),
                                    _ => Some((name, interner.intern(Type::Union(minted)))),
                                }
                            }
                            _ => self
                                .coll_nominal_with(
                                    interner,
                                    cls,
                                    &grown(&Typer::coll_value_members(interner, pre_ty)),
                                )
                                .map(|ty| (name, ty)),
                        }
                    }
                };
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
                if let Some((name, ty)) = widened {
                    if !rebound_within(ctx.rebinds, call_span, &name) {
                        tenv.insert(name, ty);
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
        *tenv = self.coll_join_envs(&t, &f, interner);
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
            acc = self.coll_join_envs(&acc, &t, interner);
        }
        let mut e = tenv.clone();
        self.coll_flow_scope(ast, &else_body, &mut e, ctx, interner, out, stmt_position);
        *tenv = self.coll_join_envs(&acc, &e, interner);
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
            // A union [`Typer::coll_join_envs`] minted — every member a carrier
            // of the SAME collection class. A store widens EACH arm and the
            // union re-joins only where the arms converge (`widen_union`,
            // mutation_widening.rb:316; `MutationRejoin` grows an
            // already-widened carrier — upstream's `joinable_receiver?` had to
            // stop skipping an already-nominal receiver for exactly this
            // reason). The union is not a Dynamic carrier, so minting here does
            // not breach this pass's "never mint from Dynamic" envelope: every
            // member was minted from a literal seed already.
            Type::Union(members) => {
                let members = members.clone();
                let mut carrier: Option<&'static str> = None;
                for m in members {
                    let c = self.coll_nominal_carrier(interner, m)?;
                    match carrier {
                        None => carrier = Some(c),
                        Some(prev) if prev == c => {}
                        Some(_) => return None,
                    }
                }
                match carrier? {
                    "Array" if ARRAY_MUTATORS.contains(&method) => Some("Array"),
                    "Hash" if HASH_MUTATORS.contains(&method) => Some("Hash"),
                    _ => None,
                }
            }
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

    /// The same carrier, tagged with the accumulated set of erased STORE VALUE
    /// classes — upstream's `Inference::MutationRejoin` (`1ad7351e`, #580,
    /// `v0.3.9`) as this pass needs it.
    ///
    /// Before `v0.3.9` mutation widening was a one-way door: the first content
    /// mutation replaced the literal shape with a bare nominal and every later
    /// store was invisible, so both edges of a branch carried the SAME
    /// `Nominal[Hash]` and the join kept it. The re-join grows the carrier's
    /// value side with each later store, so two edges that stored DIFFERENT
    /// value classes no longer carry the same instantiation — the reference
    /// joins them into a `Type::Union`, and `receiver_descriptor` has no union
    /// arm, so the receiver stops witnessing entirely.
    ///
    /// This pass models exactly that difference and nothing else: the member set
    /// rides in the nominal's `args`, so [`join_flow_envs`]'s identical-`TypeId`
    /// test — this slice's standing decline — does the union for us. The members
    /// themselves are never read; only their EQUALITY is. A store whose value
    /// class this pass cannot name contributes nothing, which is the same thing
    /// the reference's own `Dynamic[top]` seed does (it is in every instantiation
    /// and so never separates two of them).
    ///
    /// The member set is an APPROXIMATION of the reference's model: the
    /// reference carries REAL element types and joins them (`Tuple[1]` and
    /// `Tuple[2]` are different members; `String` and `Dynamic[String]` are too),
    /// while this pass compares ERASED class sets for equality. Two values of
    /// the same erased class therefore agree here where the reference may still
    /// separate them — a residual that can witness where the oracle is silent,
    /// and the reason a faithful port of the content-join model is its own arc.
    fn coll_nominal_with(
        &self,
        interner: &mut Interner,
        class_name: &str,
        members: &[TypeId],
    ) -> Option<TypeId> {
        let class = self.index.class_id(class_name)?;
        let args = match members.len() {
            0 => vec![],
            1 => vec![members[0]],
            _ => vec![interner.intern(Type::Union(members.to_vec()))],
        };
        Some(interner.intern(Type::Nominal { class, args }))
    }

    /// The store-value classes a carrier has accumulated so far, in canonical
    /// order (the `args` of [`Typer::coll_nominal_with`], unwrapped).
    ///
    /// A free function rather than a method: it reads nothing but the interner,
    /// and clippy 1.88 — the version CI pins — flags a `self` that only the
    /// recursive call uses (`only_used_in_recursion`).
    fn coll_value_members(interner: &Interner, ty: TypeId) -> Vec<TypeId> {
        let args = match interner.get(ty) {
            Type::Nominal { args, .. } => args,
            // Defensive: the store path handles a `Nominal[C] | Nominal[C]`
            // union arm-by-arm before reaching here (`widen_union`), so this
            // arm only merges member sets if a union arrives some other way.
            Type::Union(members) => {
                let mut out: Vec<TypeId> = Vec::new();
                for &m in members {
                    for v in Typer::coll_value_members(interner, m) {
                        if !out.contains(&v) {
                            out.push(v);
                        }
                    }
                }
                out.sort_unstable();
                return out;
            }
            _ => return Vec::new(),
        };
        match args.first() {
            None => Vec::new(),
            Some(&a) => match interner.get(a) {
                Type::Union(ms) => ms.clone(),
                _ => vec![a],
            },
        }
    }

    /// The erased classes one stored value contributes to the carrier's value
    /// side — the erased class of the value expression's TYPED answer, not a
    /// syntactic read of its shape (issue #128). The reference names the store
    /// by typing the argument; the syntactic classifier this replaced named
    /// strictly less, and the gap was symmetric: `h['a'] = 1.to_s` lost a row
    /// (the reference's two edges agreed where ours could not) and
    /// `h['b'] = 'y'.to_i` false-positived (our two edges agreed where the
    /// reference's did not).
    ///
    /// The env the value is typed against is the pass's OWN threaded `tenv` —
    /// the same sparse env the local-assignment arm already calls `type_of`
    /// with — never the rules layer's `ScopedEnv` (top-level only, and empty
    /// inside a `def` body, which is where mutation code lives). A sparse env
    /// is safe: an unbound local types `Dynamic[top]`, which contributes
    /// nothing — the same thing the reference's own `Dynamic[top]` seed does
    /// (it is in every instantiation and so never separates two of them).
    fn coll_store_value_classes(
        &self,
        ast: &LoweredAst,
        node: NodeId,
        tenv: &TypeEnv,
        interner: &mut Interner,
    ) -> Vec<TypeId> {
        let ty = self.stmt_value_type(ast, node, tenv, interner);
        let mut out = Vec::new();
        self.coll_erased_store_member(interner, ty, &mut out);
        out
    }

    /// Pushes the erased member classes of one typed store value onto `out`.
    ///
    /// The answer is ERASED before it becomes a member: the member set is
    /// compared by interned `TypeId` equality and never read, so a precise type
    /// (`Constant["a"]` for `'a'`) would separate two edges the reference joins.
    /// Members stay bare class nominals with no args.
    ///
    /// It is a SET rather than one class because a union value contributes every
    /// member the reference's join puts in the element set: `out << (flag ? 'a'
    /// : 1)` types `String | Integer`, both go in, and a later `String` store
    /// then adds nothing (row r15). A `Dynamic` member — `Dynamic[top]` or any
    /// faceted dynamic — names nothing: the erased class of a dynamic is
    /// `untyped`, not a class.
    fn coll_erased_store_member(&self, interner: &mut Interner, ty: TypeId, out: &mut Vec<TypeId>) {
        match interner.get(ty) {
            Type::Union(members) => {
                let members = members.clone();
                for m in members {
                    self.coll_erased_store_member(interner, m, out);
                }
            }
            // Erasing through the base (or an intersection's first member)
            // matches `erase_to_rbs_named`; the store path's own normalizer
            // (`widen_value_pinned`) would keep these wrappers whole, so this
            // is the merge-direction residual — unreachable via `type_of`
            // today since no expression types to them here.
            Type::Refined { base, .. } | Type::Difference { base, .. } => {
                let base = *base;
                self.coll_erased_store_member(interner, base, out);
            }
            Type::Intersection(members) => {
                if let Some(&m) = members.first() {
                    self.coll_erased_store_member(interner, m, out);
                }
            }
            // The erased class of a nominal is itself minus the args — interned
            // straight off the `ClassId`, so a PROJECT-class nominal names too
            // (`CoreIndex::class_name_of` can only spell the nine CORE_CLASSES).
            Type::Nominal { class, .. } | Type::DataInstance { class, .. } => {
                let class = *class;
                Self::push_store_member(interner, out, Type::Nominal { class, args: vec![] });
            }
            // A class object (`Time`, `Array`) contributes its singleton
            // carrier — already erased-level, and injective, so it separates
            // exactly the edges the reference's `singleton(C)` member does.
            Type::Singleton(class) => {
                let class = *class;
                Self::push_store_member(interner, out, Type::Singleton(class));
            }
            // `Constants`, `Tuple`, `HashShape`, `IntegerRange`: the index's
            // type→class-name erasure (`"a"` -> `String`, `[1]` -> `Array`,
            // `1..3` -> `Integer`). `Dynamic`/`Top`/anything else names
            // nothing.
            _ => {
                if let Some(name) = self.index.class_name_of(interner, ty) {
                    if let Some(class) = self.index.class_id(name) {
                        Self::push_store_member(
                            interner,
                            out,
                            Type::Nominal { class, args: vec![] },
                        );
                    }
                }
            }
        }
    }

    /// Interns `member` and pushes it unless already present — member sets are
    /// sets, so two arms of a union erasing to the same class contribute once.
    fn push_store_member(interner: &mut Interner, out: &mut Vec<TypeId>, member: Type) {
        let m = interner.intern(member);
        if !out.contains(&m) {
            out.push(m);
        }
    }

    /// [`join_flow_envs`] for this pass: two carriers of the SAME collection
    /// class that differ only in the store-value members they accumulated join
    /// to a `Type::Union` of the two, which is what the reference's `Scope#join`
    /// leaves behind once `MutationRejoin` lets the edges diverge.
    ///
    /// Keeping the union rather than collapsing to untyped matters in BOTH
    /// directions. A use site sees no witness either way ([`Typer::coll_carrier`]
    /// has no union arm, exactly as `receiver_descriptor` has none). But a later
    /// STORE re-joins the union into one carrier, and mastodon's
    /// `application_helper.rb:180` needs that: a branch push makes the edges
    /// diverge, the unconditional push after it re-joins them, and the reference
    /// fires on the `compact_blank` that follows. Collapsing to untyped loses
    /// that row, because a mutator on a Dynamic carrier never mints.
    ///
    /// Every other disagreement — a shape against a nominal, two different
    /// collection classes, a non-carrier — still widens to untyped, which is this
    /// pass's standing decline (probes m18/m20).
    fn coll_join_envs(&self, a: &TypeEnv, b: &TypeEnv, interner: &mut Interner) -> TypeEnv {
        let u = interner.untyped();
        let mut out = TypeEnv::with_capacity(a.len());
        for (k, av) in a {
            let v = match b.get(k) {
                Some(bv) if bv == av => *av,
                Some(bv) => self.coll_union_carrier(interner, *av, *bv).unwrap_or(u),
                None => u,
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

    /// The union of two collection carriers of the same class, or `None` when
    /// they are not both that. Members are flattened and sorted so the result is
    /// a function of the SET — two edges that reach the same set must intern to
    /// the same `TypeId` or the next join separates them for no reason.
    fn coll_union_carrier(
        &self,
        interner: &mut Interner,
        a: TypeId,
        b: TypeId,
    ) -> Option<TypeId> {
        let ca = self.coll_union_class(interner, a)?;
        let cb = self.coll_union_class(interner, b)?;
        if ca != cb {
            return None;
        }
        let mut members: Vec<TypeId> = Vec::new();
        for side in [a, b] {
            match interner.get(side) {
                Type::Union(ms) => {
                    for &m in ms {
                        if !members.contains(&m) {
                            members.push(m);
                        }
                    }
                }
                _ => {
                    if !members.contains(&side) {
                        members.push(side);
                    }
                }
            }
        }
        members.sort_unstable();
        match members.len() {
            0 => None,
            1 => Some(members[0]),
            _ => Some(interner.intern(Type::Union(members))),
        }
    }

    /// The single collection class a carrier — or a union of carriers — stands
    /// for, or `None` when it is neither.
    fn coll_union_class(&self, interner: &Interner, ty: TypeId) -> Option<&'static str> {
        match interner.get(ty) {
            Type::Nominal { .. } => self.coll_nominal_carrier(interner, ty),
            Type::Union(members) => {
                let mut cls: Option<&'static str> = None;
                for &m in members {
                    let c = self.coll_nominal_carrier(interner, m)?;
                    match cls {
                        None => cls = Some(c),
                        Some(prev) if prev == c => {}
                        Some(_) => return None,
                    }
                }
                cls
            }
            _ => None,
        }
    }

    /// The argument positions of `method` that STORE a value into the receiver —
    /// the only ones `MutationRejoin` grows the value side from. Every other
    /// mutator (`delete`, `clear`, `sort!`, …) leaves the member set alone, and
    /// the reference's remaining content adders (`concat`/`insert`/`fill`/
    /// `replace`) stay out of scope for this pass. A Hash `[]=`/`store` also
    /// joins the KEY's type into the reference's K side (`join_added_pairs`) —
    /// this pass's flat member set is value-side only, a documented residual.
    fn coll_store_value_args<'a>(method: &str, args: &'a [NodeId]) -> &'a [NodeId] {
        match method {
            "[]=" | "store" => args.last().map(std::slice::from_ref).unwrap_or(&[]),
            "<<" | "push" | "append" | "unshift" | "prepend" => args,
            _ => &[],
        }
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
struct Reach {
    /// A `Dynamic[Top]` value may reach.
    untyped: bool,
    /// A precisely-typed value may reach.
    precise: bool,
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
    fn declines_rand(self) -> bool {
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
            rigor_parse::MultiTarget::Ignored { .. } => true,
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
mod tests;

#[cfg(test)]
mod m2_go_slice_tests;

#[cfg(test)]
mod meta_new_lift_tests;

#[cfg(test)]
mod rbs_tuple_return_tests;

// ---------------------------------------------------------------------------
// `is_a?` / `case-when` class narrowing (census mechanism 1) — the oracle
// probe matrix a1–a6 + the load-bearing declines
// ---------------------------------------------------------------------------

#[cfg(test)]
mod class_narrowing_tests;

// ---------------------------------------------------------------------------
// Collection-shape receiver survival (stage 1) — the oracle probe matrix
// m01-m20 of docs/notes/20260807-collection-shape-slice-spec.md. The SILENT
// rows are the FP-safety envelope, not coverage bookkeeping: each one is a
// shape the reference itself declines on (m04/m05/m08/m13/m15/m18/m20) or a
// deliberate coverage give-up (m16 op-writes, `while`, ivars, safe-nav).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod collection_shape_tests;

/// Collection-shape **stage 2** — the chain ROOTS. Each micro-slice gets a
/// fire + decline pair, mirroring the oracle probes recorded in
/// `docs/notes/20260807-collection-shape-slice-spec.md` §1.
#[cfg(test)]
mod collection_shape_stage2_tests;
