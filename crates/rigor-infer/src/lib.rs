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
pub mod source_index;

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use rigor_index::CoreIndex;
use rigor_parse::{LoweredAst, Node, NodeId};
use rigor_types::{Interner, Scalar, ShapeKey, ShapeMember, Type, TypeId};

pub use folding::RubyFolder;
pub use source_index::{
    lexical_scopes, ConstLit, DefKind, ParamBoundReturn, SourceIndex, SOURCE_CLASS_BASE,
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
        }
    }

    /// C1: the use-site lexical prefix (enclosing class/module qualified segments)
    /// for a node at `span` — the INNERMOST enclosing scope by span containment,
    /// or an empty slice at toplevel / when no scopes are attached.
    fn enclosing_prefix(&self, span: rigor_parse::Span) -> &[String] {
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
                if let Some(lit) = self.source.literal_constant(name, prefix) {
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
            Node::LocalVariableWrite { value, .. }
            | Node::LocalVariableOpWrite { value, .. }
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
                if let Some(ret) = self.index.singleton_method_return(class_name, method) {
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
            if let Some(ret_class) = self.index.method_return(class_name, method) {
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

    /// Build a SCOPED method-body env for ONE `def`, used by
    /// `call.possible-nil-receiver` to type a method-local's nil-source RHS
    /// receiver (`s = String.new; s.byteslice(..)`). Starts from `base` (the
    /// top-level env) and binds every plain `LocalVariableWrite` whose span lies
    /// within `def_span`, in arena (source) order — so `s` is typed before
    /// `x = s.byteslice`. Span-scan (not structural) is orphan-proof, matching
    /// the dead-assignment collector.
    ///
    /// Deliberately NON-flow-sensitive and SCOPED to this rule's call path: it
    /// does NOT mutate the shared top-level env and is never consumed by the
    /// undefined-method / arity / chaining rules, so existing behaviour and the
    /// corpus baseline are unperturbed (ADR-0022 full scoping is deferred).
    pub fn build_method_body_env(
        &self,
        ast: &LoweredAst,
        def_span: rigor_parse::Span,
        base: &TypeEnv,
        interner: &mut Interner,
    ) -> TypeEnv {
        let mut env = base.clone();
        // Collect writes in arena/source order so earlier binds feed later RHS.
        let writes: Vec<(String, NodeId)> = ast
            .iter()
            .filter_map(|(_, n)| match n {
                Node::LocalVariableWrite { name, value, span, .. }
                    if def_span.0 <= span.0 && span.1 <= def_span.1 =>
                {
                    Some((name.clone(), *value))
                }
                _ => None,
            })
            .collect();
        for (name, value) in writes {
            let ty = self.type_of(ast, value, &env, interner);
            env.insert(name, ty);
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
    /// [`Self::build_method_body_env`] and the dead-assignment collector.
    pub fn always_truthy_snapshots(
        &self,
        ast: &LoweredAst,
        interner: &mut Interner,
    ) -> HashMap<NodeId, TypeId> {
        let mut out = HashMap::new();
        let writes = collect_flow_writes(ast);
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
        let writes = collect_flow_writes(ast);
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
        .filter_map(|(_, n)| match n {
            Node::LocalVariableWrite { name, span, .. }
            | Node::LocalVariableOpWrite { name, span, .. } => Some((*span, name.clone())),
            Node::Call { receiver: Some(r), method, span, .. }
                if MUTATOR_METHODS.contains(&method.as_str()) =>
            {
                match ast.get(*r) {
                    Node::LocalVariableRead { name, .. } => Some((*span, name.clone())),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
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
