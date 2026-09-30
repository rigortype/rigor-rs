//! Expression typing: [`Typer::type_of`], the dispatch-by-node-variant core
//! (ADR-0023), with the statement/branch value types, `.new` typing, tuple
//! and hash-shape projection folds, implicit-self calls, and the shape-key and
//! literal-set helpers they share.

use rigor_parse::{IndexCompound, LoweredAst, Node, NodeId};
use rigor_types::{Interner, Scalar, ShapeKey, ShapeMember, Type, TypeId};

use crate::{kernel_fold, ConstLit, TypeEnv, Typer};

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
        // A Bignum key is a real Ruby key but `ShapeKey::Int` holds `i64`;
        // `Other` keeps the shape honest — lookups keyed on it decline rather
        // than pin a wrong identity (rigor-rs#194).
        Scalar::BigInt(_) => ShapeKey::Other,
        Scalar::Float(f) => ShapeKey::Float(f.to_bits()),
        Scalar::Bool(b) => ShapeKey::Bool(*b),
        Scalar::Nil => ShapeKey::Nil,
    }
}

/// The [`Scalar`] a [`ShapeKey`] denotes — the inverse of [`scalar_to_shape_key`],
/// used by `HashShape#invert` to turn an original key back into a `Constant`
/// value. `None` for the `Other` fallback (never built from a literal), so a
/// projection that reaches it declines.
pub(crate) fn shape_key_to_scalar(k: &ShapeKey) -> Option<Scalar> {
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

/// Constants whose `.new`/`.define` returns a CLASS, not a plain instance of the
/// named class: `Struct.new(...)` and `Data.define(...)` build an anonymous
/// SUBCLASS; `Class.new` builds a `Class`. Their result must NOT be typed as an
/// instance of the receiver — doing so would witness a chained class-method call
/// (e.g. the second `.new` in `Struct.new(:a).new(1)`) falsely absent. We can't
/// model the anonymous class, so the result stays Dynamic (silent).
const CLASS_RETURNING_NEW: &[&str] = &["Struct", "Data", "Class"];

impl<'i> Typer<'i> {
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
            // A Bignum pins its exact decimal spelling (`Scalar::BigInt`) —
            // the reference's `Constant[99999999999999999999]` — so witnesses
            // render the literal (rigor-rs#194). Arithmetic folds decline it
            // (no i64), the zero-FP-safe side. A `digits: None` bigint keeps
            // the pre-pin nominal answer.
            Node::IntegerLit { digits: Some(digits), .. } => {
                interner.intern(Type::Constant(Scalar::BigInt(digits.clone())))
            }
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
                block_locals,
                block_params,
                explicit_arg_list,
                safe_nav,
                ..
            } => {
                let (
                    r,
                    method,
                    block_body,
                    block_span,
                    block_locals,
                    block_params,
                    explicit_arg_list,
                    safe_nav,
                ) = (
                    *r,
                    method.clone(),
                    block_body.clone(),
                    *block_span,
                    block_locals.clone(),
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
                        &block_locals,
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
            // `h[k] ||= v` / `h[k] &&= v` / `h[k] op= v` evaluate to the value
            // they STORE — `index_write_stored_type` (statement_evaluator.rb:828):
            // `||=` → `narrow_truthy(h[k]) | v`, `&&=` → `narrow_falsey(h[k]) |
            // v`. `op=` would be the dispatched `h[k] op v`; this slice declines
            // it (`Dynamic[top]` — the stored-type path only ever produces a
            // witnessable constant from `||=`/`&&=` anyway). Only an
            // `operand`-flagged write keeps a value — a joined-position write
            // lowered for its `[]=` widening alone types `Dynamic[top]`.
            Node::IndexWrite {
                receiver: Some(r),
                indices,
                value,
                compound,
                operand: true,
                ..
            } => {
                let (r, indices, value, compound) =
                    (*r, indices.clone(), *value, compound.clone());
                self.index_write_value_type(ast, r, &indices, value, &compound, env, interner)
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

    /// The value a compound index write stores (`index_write_stored_type`,
    /// `statement_evaluator.rb:828`): `union(narrow_truthy(current), rhs)` for
    /// `||=`, `union(narrow_falsey(current), rhs)` for `&&=` — `current`
    /// being the `receiver[k]` read, which itself consults the recorded
    /// indexed narrowing through `type_call`'s `[]` interception
    /// (`index_read_type` does the same). The `op=` form declines —
    /// dispatching `h[k] op v` here is the reference's `MethodDispatcher`
    /// tier this slice does not carry.
    #[allow(clippy::too_many_arguments)]
    fn index_write_value_type(
        &self,
        ast: &LoweredAst,
        receiver: NodeId,
        indices: &[NodeId],
        value: NodeId,
        compound: &IndexCompound,
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> TypeId {
        let current = self.type_call(ast, receiver, "[]", indices, env, interner);
        let rhs = self.type_of(ast, value, env, interner);
        match compound {
            IndexCompound::Or => {
                let truthy = self.narrow_truthy(current, interner);
                rigor_types::Algebra::join(interner, truthy, rhs)
            }
            IndexCompound::And => {
                let falsey = self.narrow_falsey(current, interner);
                rigor_types::Algebra::join(interner, falsey, rhs)
            }
            IndexCompound::Op(_) => interner.untyped(),
        }
    }

    /// `index_write_stored_type` for a recorded `h[k] ||= v`
    /// (`statement_evaluator.rb:828`): `union(narrow_truthy(current), rhs)`
    /// where `current` is the `index_read_type` — the `[]` dispatch, which
    /// consults the recorded narrowing ahead of the receiver binding
    /// (statement_evaluator.rb:871). `None` declines the record: a receiver
    /// holding `Dynamic`/`Top` (`fully_tracked_receiver_type?`, upstream
    /// issue #544) could carry a caller-supplied slot value `||=` keeps, so
    /// the recorded default would invent a fact.
    pub(crate) fn slot_stored_type(
        &self,
        ast: &LoweredAst,
        w: &crate::flow_writes::SlotWrite,
        env: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        let pre = env.get(w.name.as_str())?;
        if !self.fully_tracked_type(*pre, interner) {
            return None;
        }
        let current = self.type_call(ast, w.receiver, "[]", &[w.key_node], env, interner);
        let rhs = self.type_of(ast, w.value, env, interner);
        let truthy = self.narrow_truthy(current, interner);
        Some(rigor_types::Algebra::join(interner, truthy, rhs))
    }

    /// `IndexedNarrowing.fully_tracked_receiver_type?`
    /// (indexed_narrowing.rb:92): a `Dynamic`/`Top` constituent means the
    /// collection can hold a caller-supplied slot value `||=` keeps.
    #[allow(clippy::only_used_in_recursion)]
    fn fully_tracked_type(&self, ty: TypeId, interner: &Interner) -> bool {
        match interner.get(ty) {
            Type::Dynamic(_) | Type::Top => false,
            Type::Union(members) => members
                .iter()
                .all(|&m| self.fully_tracked_type(m, interner)),
            _ => true,
        }
    }

    /// `IndexedNarrowing.string_slot_floor` (indexed_narrowing.rb:184): a
    /// String mutator `widen_for_mutator` declines on an all-String slot
    /// still leaves the same object in it — the record is kept, floored to
    /// `String`.
    pub(crate) fn string_slot_floor(
        &self,
        recorded: TypeId,
        method: &str,
        interner: &mut Interner,
    ) -> Option<TypeId> {
        if !crate::STRING_MUTATORS.contains(&method) {
            return None;
        }
        let members: Vec<TypeId> = match interner.get(recorded) {
            Type::Union(m) => m.clone(),
            _ => vec![recorded],
        };
        if members
            .iter()
            .all(|&m| self.index.class_name_of(interner, m) == Some("String"))
        {
            Some(self.nominal_or_untyped("String", interner))
        } else {
            None
        }
    }

    /// `Narrowing.narrow_truthy` (`narrowing.rb:73`): the truthy fragment —
    /// `nil`/`false` constants and `NilClass`/`FalseClass` nominals collapse
    /// to `Bot`; a union maps memberwise; every other carrier passes
    /// through unchanged.
    pub(crate) fn narrow_truthy(&self, ty: TypeId, interner: &mut Interner) -> TypeId {
        match interner.get(ty) {
            Type::Constant(Scalar::Nil) | Type::Constant(Scalar::Bool(false)) => {
                interner.intern(Type::Bottom)
            }
            Type::Union(members) => {
                let members = members.clone();
                self.union_members(
                    members
                        .iter()
                        .map(|&m| self.narrow_truthy(m, interner))
                        .collect(),
                    interner,
                )
            }
            Type::Nominal { class, .. }
                if matches!(
                    self.index.class_name_for_id(*class),
                    Some("NilClass" | "FalseClass")
                ) =>
            {
                interner.intern(Type::Bottom)
            }
            _ => ty,
        }
    }

    /// `Narrowing.narrow_falsey` (`narrowing.rb:88`): the falsey fragment —
    /// `nil`/`false` constants and `NilClass`/`FalseClass` nominals pass
    /// through; `Singleton`/`Tuple`/`HashShape` and every other constant or
    /// nominal collapses to `Bot`; `Dynamic`/`Top`/`Bot` stay.
    pub(crate) fn narrow_falsey(&self, ty: TypeId, interner: &mut Interner) -> TypeId {
        match interner.get(ty) {
            Type::Constant(Scalar::Nil) | Type::Constant(Scalar::Bool(false)) => ty,
            Type::Constant(_) => interner.intern(Type::Bottom),
            Type::Nominal { class, .. } => {
                if matches!(
                    self.index.class_name_for_id(*class),
                    Some("NilClass" | "FalseClass")
                ) {
                    ty
                } else {
                    interner.intern(Type::Bottom)
                }
            }
            Type::Union(members) => {
                let members = members.clone();
                self.union_members(
                    members
                        .iter()
                        .map(|&m| self.narrow_falsey(m, interner))
                        .collect(),
                    interner,
                )
            }
            Type::Singleton(_) | Type::Tuple(_) | Type::HashShape(_) => {
                interner.intern(Type::Bottom)
            }
            _ => ty,
        }
    }

    /// `Type::Combinator.union` over a member list — `Bot` folds away
    /// through `Algebra::join`.
    fn union_members(&self, members: Vec<TypeId>, interner: &mut Interner) -> TypeId {
        let mut it = members.into_iter();
        let Some(first) = it.next() else {
            return interner.intern(Type::Bottom);
        };
        it.fold(first, |acc, m| {
            rigor_types::Algebra::join(interner, acc, m)
        })
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
    pub(crate) fn stmt_value_type(
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
            // `h[k] ||= v` / `h[k] &&= v` / `h[k] op= v` evaluate to the value
            // they STORE — `index_write_stored_type` (statement_evaluator.rb:828):
            // `||=` → `narrow_truthy(h[k]) | v`, `&&=` → `narrow_falsey(h[k]) |
            // v`, `op=` → the dispatched `h[k] op v` (declined here — `op=`
            // never reaches a witnessable constant in this slice). Only an
            // `operand`-flagged write keeps a value: a joined-position write
            // lowered for its `[]=` widening alone has no meaningful
            // expression type either (`Dynamic[top]`, the `_` arm's answer).
            Node::IndexWrite {
                receiver: Some(r),
                indices,
                value,
                compound,
                operand: true,
                ..
            } => {
                let (r, indices, value, compound) =
                    (*r, indices.clone(), *value, compound.clone());
                self.index_write_value_type(ast, r, &indices, value, &compound, env, interner)
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
    pub(crate) fn nominal_or_untyped(&self, class_name: &str, interner: &mut Interner) -> TypeId {
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
    ///
    /// [`SourceIndex`]: crate::SourceIndex
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
    pub(crate) fn intern_rbs_tuple(
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
    pub(crate) fn type_dot_new(
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
    pub(crate) fn fold_tuple_projection(
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
    pub(crate) fn fold_hash_shape_projection(
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
    pub(crate) fn type_implicit_self_call(
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

    /// Type each argument and, if *every* one is a value-pinned `Constant`,
    /// return the owned scalars in order — the input [`folding::fold`] needs to
    /// compute a byte-exact result. Returns `None` the moment any argument is
    /// not a pinned `Constant` (Dynamic / Nominal / unknown), so the caller
    /// declines to fold rather than guessing (ADR-0008 zero-FP).
    ///
    /// [`folding::fold`]: crate::folding::fold
    pub(crate) fn pin_arg_scalars(
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
