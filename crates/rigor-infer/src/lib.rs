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
pub mod source_index;

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use rigor_index::CoreIndex;
use rigor_parse::{LoweredAst, Node, NodeId};
use rigor_types::{Interner, Scalar, ShapeKey, ShapeMember, Type, TypeId};

pub use folding::RubyFolder;
pub use source_index::{ParamBoundReturn, SourceIndex, SOURCE_CLASS_BASE};

/// A process-wide empty [`SourceIndex`], used as the default `source` for a
/// [`Typer`] built via [`Typer::new`] (callers that predate in-source typing).
/// Sharing one empty index keeps `Typer::new` allocation-free and infallible.
fn empty_source() -> &'static SourceIndex {
    static EMPTY: OnceLock<SourceIndex> = OnceLock::new();
    EMPTY.get_or_init(SourceIndex::default)
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
}

impl<'i> Typer<'i> {
    /// Build a typer over a borrowed core index, with an EMPTY source index
    /// (no in-source typing). Kept for callers that predate tier-4.
    pub fn new(index: &'i CoreIndex) -> Self {
        Typer { index, source: empty_source(), folder: None }
    }

    /// Build a typer over a borrowed core index AND a per-run [`SourceIndex`],
    /// enabling `X.new` instance typing and in-source method resolution.
    pub fn with_source(index: &'i CoreIndex, source: &'i SourceIndex) -> Self {
        Typer { index, source, folder: None }
    }

    /// As [`Typer::with_source`], plus the ADR-0008 real-Ruby folder for
    /// sidecar-routed constant folds. `None` is byte-identical to
    /// [`Typer::with_source`] (the sound subset).
    pub fn with_source_and_folder(
        index: &'i CoreIndex,
        source: &'i SourceIndex,
        folder: Option<&'i (dyn folding::RubyFolder + Sync)>,
    ) -> Self {
        Typer { index, source, folder }
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
            Node::ConstantRead { name, .. } => {
                if !name.is_empty()
                    && self.index.knows_toplevel_class(name)
                    && !self.source.knows_class(name)
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
    /// `static_hash_shape_for`: every key must be a static Symbol/String literal
    /// and no key may repeat; a non-static or duplicate key degrades to `Hash`.
    /// The empty list yields the empty `HashShape{}` (`{}`).
    fn hash_shape_or_hash(
        &self,
        ast: &LoweredAst,
        elem_ids: &[NodeId],
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> TypeId {
        let mut members: Vec<ShapeMember> = Vec::with_capacity(elem_ids.len() / 2);
        let mut seen: std::collections::HashSet<ShapeKey> = std::collections::HashSet::new();
        let mut i = 0;
        while i + 1 < elem_ids.len() {
            let key = match ast.get(elem_ids[i]) {
                Node::SymbolLit { value, .. } => ShapeKey::Sym(value.clone()),
                Node::StringLit { value, .. } => ShapeKey::Str(value.clone()),
                // A dynamic / non-Symbol-or-String key can't pin a shape slot.
                _ => return self.nominal_or_untyped("Hash", interner),
            };
            if !seen.insert(key.clone()) {
                return self.nominal_or_untyped("Hash", interner);
            }
            let value = self.type_of(ast, elem_ids[i + 1], env, interner);
            members.push(ShapeMember { key, value, optional: false });
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
        interner: &mut Interner,
    ) -> Option<TypeId> {
        let Node::ConstantRead { name, .. } = ast.get(receiver) else {
            return None;
        };
        if name.is_empty() || CLASS_RETURNING_NEW.contains(&name.as_str()) {
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

    /// Implicit-self (`receiver: None`) dispatch entry — the shared home for
    /// receiverless Kernel folds (ADR-0038 inference-cluster spec). Returns
    /// `Some(ty)` when a fold applies, `None` to decline (the caller falls to
    /// `Dynamic[top]`, silent). This slice folds ONLY Kernel `#p` / `#pp`
    /// identity; future Kernel folds (`format`/`String()`/`Integer()`/…) extend
    /// this same entry.
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
    /// Guards (decline ⇒ Dynamic, silent), matching the reference's FP envelope:
    /// - an explicit foreign receiver never reaches here — this path is
    ///   `receiver: None` only, so `Kernel.p(42)` stays unfolded automatically
    ///   (probe p05);
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
        // Only `p` / `pp` are folded in this slice; every other implicit-self
        // call declines (one string compare on the hot path).
        if method != "p" && method != "pp" {
            return None;
        }
        // User-redefinition guard (conservative file-wide substitute for the
        // reference's scope-aware check).
        if self.file_defines_method(ast, method) {
            return None;
        }
        // Splat / forwarding guard: rigor-parse lowers a `*a` splat and a `...`
        // forwarding arg to `Node::Other` (no owned variant). Any such arg means
        // the runtime arity is unknown, so we cannot choose identity vs `Tuple`.
        if args.iter().any(|&a| matches!(ast.get(a), Node::Other { .. })) {
            return None;
        }
        Some(match args {
            [] => interner.intern(Type::Constant(Scalar::Nil)),
            [only] => self.type_of(ast, *only, env, interner),
            many => {
                let elems: Vec<TypeId> =
                    many.iter().map(|&a| self.type_of(ast, a, env, interner)).collect();
                interner.intern(Type::Tuple(elems))
            }
        })
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
            if let Some(ty) = self.type_dot_new(ast, receiver, interner) {
                return ty;
            }
            // Not a typeable `.new` (metaclass constructor / unknown constant) ⇒
            // fall through to the folding / RBS-return cascade below.
        }

        let recv_ty = self.type_of(ast, receiver, env, interner);

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
            if let Some(ty) = self.type_dot_new(ast, receiver, interner) {
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
        self.flow_eval_scope(ast, &body, &mut env, false, &writes, interner, &mut out);
        out
    }

    /// Thread `env` through a scope's statements in source order.
    #[allow(clippy::too_many_arguments)]
    fn flow_eval_scope(
        &self,
        ast: &LoweredAst,
        stmts: &[NodeId],
        env: &mut TypeEnv,
        in_loop_or_block: bool,
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut HashMap<NodeId, TypeId>,
    ) {
        for &s in stmts {
            self.flow_eval_stmt(ast, s, env, in_loop_or_block, writes, interner, out);
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
        writes: &[(rigor_parse::Span, String)],
        interner: &mut Interner,
        out: &mut HashMap<NodeId, TypeId>,
    ) {
        match ast.get(id) {
            Node::Statements { body, .. } => {
                let body = body.clone();
                self.flow_eval_scope(ast, &body, env, in_loop_or_block, writes, interner, out);
            }
            Node::LocalVariableWrite { name, value, .. } => {
                let (name, value) = (name.clone(), *value);
                // A value expression may itself write OTHER locals (`x = (y = 5)`)
                // or capture-write via a block — widen those first, then bind.
                let vspan = ast.get(value).span();
                widen_flow_writes(writes, vspan, env, interner);
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
                    let pty = self.type_of(ast, predicate, env, interner);
                    out.insert(id, pty);
                }
                // Independently evaluate each branch from the dominating env, then
                // join: a binding survives only if both branches agree exactly.
                let mut then_env = env.clone();
                self.flow_eval_scope(
                    ast, &then_body, &mut then_env, in_loop_or_block, writes, interner, out,
                );
                let mut else_env = env.clone();
                self.flow_eval_scope(
                    ast, &else_body, &mut else_env, in_loop_or_block, writes, interner, out,
                );
                *env = join_flow_envs(&then_env, &else_env, interner);
                // A predicate may contain a write (`if (x = f)`); widen post-join.
                let pspan = ast.get(predicate).span();
                widen_flow_writes(writes, pspan, env, interner);
            }
            Node::Definition { body, .. }
            | Node::ClassDef { body, .. }
            | Node::ModuleDef { body, .. } => {
                // Independent scope: fresh local env, inherited suppression flag,
                // no effect on the enclosing env.
                let body = body.clone();
                let mut fresh = TypeEnv::new();
                self.flow_eval_scope(
                    ast, &body, &mut fresh, in_loop_or_block, writes, interner, out,
                );
            }
            // Loop / case / begin-rescue / logical / call(+block) / any other node:
            // widen every local written in the span, do not descend for snapshots.
            other => {
                widen_flow_writes(writes, other.span(), env, interner);
            }
        }
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
fn collect_flow_writes(ast: &LoweredAst) -> Vec<(rigor_parse::Span, String)> {
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

        // Silent directions — decline to Dynamic[top].
        // p05: `Kernel.p(42)` — explicit receiver, never on our path.
        assert_eq!(describe_ty(b"Kernel.p(42)\n", false), "Dynamic[top]");
        // p07: a file-wide `def p` disables the fold file-wide.
        assert_eq!(describe_ty(b"def p(*a); nil; end\np 42\n", true), "Dynamic[top]");
        // p08: a splat arg makes arity unknown → decline.
        assert_eq!(describe_ty(b"a = [1, 2]\np(*a)\n", true), "Dynamic[top]");
        // p11: a Dynamic (unknown local) arg passes through identity as Dynamic.
        assert_eq!(describe_ty(b"p some_unknown_local\n", true), "Dynamic[top]");
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
        // outside CORE_CLASSES; the source registry carries its instance id and
        // method existence defers to RBS. Under the stub fallback Pathname is not
        // registered, so this test only asserts the registry/typing wiring when
        // Pathname is actually loaded.
        let ast = lower_src(b"p = Pathname.new(\"a\")\np.foo\n");
        let idx = CoreIndex::new();
        let source = SourceIndex::build(&ast, &idx);
        if idx.knows_class("Pathname") {
            let typer = Typer::with_source(&idx, &source);
            let mut i = Interner::new();
            let env = typer.build_toplevel_env(&ast, &mut i);
            let p_ty = *env.get("p").expect("p should be bound");
            let class = match i.get(p_ty) {
                Type::Nominal { class, .. } => *class,
                other => panic!("expected Nominal instance, got {other:?}"),
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
}
