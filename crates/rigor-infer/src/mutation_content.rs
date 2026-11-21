//! The content-evidence half of `MutationWidening.widen_for_mutator`
//! (`reference/rigor/lib/rigor/inference/mutation_widening.rb`,
//! `content_join.rb`, `mutation_rejoin.rb`, `rewrite_mutation.rb`).
//!
//! The flat check env used to mint a BARE `Nominal[Array|Hash]` when a
//! literal `Tuple`/`HashShape` binding took an in-place mutation — the
//! reference's `widen_tuple` / `widen_hash_shape` mint carriers WITH the
//! seed's content (`Array[1]`, `Hash[Symbol, 1]`), and the content-adder
//! joins (`join_added_elements` / `join_added_pairs`) grow them with the
//! call's typed arguments plus the `gradual_floor` `Dynamic[top]` arm. The
//! bare carrier only ever cost message spelling (`for Hash` where the
//! reference says `for Hash[Dynamic[top] | Symbol, Dynamic[top] |
//! Integer]` — rigor-rs#771); minting the reference's args is also what
//! two branch edges with different stored content need to diverge the way
//! the reference's own carriers do.
//!
//! `arg_types` mirrors the reference's `arg_types:` keyword: `Option`
//! members carry `index_store_form`'s nil-splat mark (`a[i, *xs] = v`),
//! and an EMPTY slice is `NO_ARG_TYPES` — the joins then reproduce the
//! pre-join widening exactly, which is what every path that cannot type
//! the call's arguments (`widen_mutated_slot`, the nilable replay) asks
//! for.

use rigor_types::{Algebra, Interner, ShapeKey, ShapeMember, Type, TypeId};

use crate::folding::scalar_class;
use crate::Typer;

/// `MutationWidening::VALUE_REWRITING_MUTATORS` (`mutation_widening.rb:405`)
/// — mutators that can land a NEW value in an EXISTING slot, so a surviving
/// value pin is falsified along with the shape.
const VALUE_REWRITING_MUTATORS: &[&str] = &[
    "[]=",
    "fill",
    "map!",
    "collect!",
    "replace",
    "store",
    "merge!",
    "update",
    "transform_values!",
];

/// `ContentJoin::ARRAY_CONTENT_ADDERS` (`content_join.rb:30`) — the
/// element-INTRODUCING Array mutators.
const ARRAY_CONTENT_ADDERS: &[&str] = &[
    "<<", "push", "append", "prepend", "unshift", "concat", "insert", "[]=", "fill", "replace",
];

/// `ContentJoin::HASH_CONTENT_ADDERS` (`content_join.rb:36`) — `[]=` and
/// `store` store a key→value pair.
const HASH_CONTENT_ADDERS: &[&str] = &["[]=", "store"];

/// `UnknownStoreWidening::VALUE_PRESERVING`
/// (`unknown_store_widening.rb:58`) — mutators that only remove or reorder
/// what the receiver already holds. Read by the alias floor's
/// `gradual_content` fallback.
pub(crate) const VALUE_PRESERVING_MUTATORS: &[&str] = &[
    "pop",
    "shift",
    "delete",
    "delete_at",
    "delete_if",
    "reject!",
    "select!",
    "filter!",
    "keep_if",
    "clear",
    "compact!",
    "uniq!",
    "slice!",
    "sort!",
    "sort_by!",
    "reverse!",
    "rotate!",
    "shuffle!",
];

/// `RewriteMutation::REPLACED` (`rewrite_mutation.rb:29`): mutators that
/// rewrite EVERY value at the listed type-argument positions — the position
/// becomes `Dynamic[top]` outright, the seed's evidence gone.
fn rewrite_replaced(class_name: &str, method: &str) -> &'static [usize] {
    match (class_name, method) {
        ("Array", "map!" | "collect!" | "flatten!" | "replace") => &[0],
        ("Set", "map!" | "collect!" | "flatten!" | "replace") => &[0],
        ("Hash", "transform_keys!") => &[0],
        ("Hash", "transform_values!") => &[1],
        ("Hash", "replace") => &[0, 1],
        _ => &[],
    }
}

/// `RewriteMutation::JOINED` (`rewrite_mutation.rb:38`): mutators that
/// store values nothing describes BESIDE values they may keep — the
/// position joins `Dynamic[top]` and keeps the seed's own arms.
fn rewrite_joined(class_name: &str, method: &str) -> &'static [usize] {
    match (class_name, method) {
        ("Array", "fill") => &[0],
        ("Hash", "merge!" | "update") => &[0, 1],
        _ => &[],
    }
}

/// `RewriteMutation.values_mode` (`rewrite_mutation.rb:152`): `:keep` when
/// `method` rewrites the carrier's VALUE position (an Array's element, a
/// Hash's value) — a closed claim beside the gradual arm is exactly what a
/// hand-written signature rejects, so the pinning survives the widening.
fn rewrite_keeps_values(class_name: &str, method: &str) -> bool {
    let pos = if class_name == "Hash" { 1 } else { 0 };
    rewrite_replaced(class_name, method).contains(&pos)
        || rewrite_joined(class_name, method).contains(&pos)
}

/// How `a[…] = v` stores its value (`ContentJoin.index_store_form`).
pub(crate) enum IndexStoreForm {
    /// `a[i, n] = v` / `a[range] = v` — the value's ELEMENTS land.
    Splice,
    /// `a[i] = v` — the value itself is one stored element.
    Element,
    /// The store may be either form (unclassifiable index, splat).
    Either,
}

impl Typer<'_> {
    /// `Type::Combinator.nominal_of` through the core index.
    pub(crate) fn nominal_args(
        &self,
        interner: &mut Interner,
        class_name: &str,
        args: Vec<TypeId>,
    ) -> Option<TypeId> {
        let class = self.index.class_id(class_name)?;
        Some(interner.intern(Type::Nominal { class, args }))
    }

    /// `Combinator.union` over a non-empty member list — `union(*types)`.
    pub(crate) fn content_union(&self, interner: &mut Interner, members: &[TypeId]) -> TypeId {
        Algebra::union(interner, members)
    }

    /// `Combinator.untyped` — `Dynamic[top]`.
    fn content_untyped(&self, interner: &mut Interner) -> TypeId {
        interner.untyped()
    }

    // ----------------------------------------------------------------
    // widen_tuple / widen_hash_shape — the literal-carrier mints.
    // ----------------------------------------------------------------

    /// `MutationWidening.widen_tuple` (`mutation_widening.rb:709`):
    /// `Tuple[A, B]` → `Nominal[Array, [union(A, B)]]`; under `values:
    /// :widen` each element's value pin widens to its class nominal; an
    /// empty tuple carries the `untyped` element bound.
    fn widen_tuple_args(
        &self,
        interner: &mut Interner,
        elems: &[TypeId],
        keep_values: bool,
    ) -> Vec<TypeId> {
        let element = if elems.is_empty() {
            self.content_untyped(interner)
        } else {
            let mapped: Vec<TypeId> = elems
                .iter()
                .map(|&e| {
                    if keep_values {
                        e
                    } else {
                        self.widen_value_pinned(e, interner)
                    }
                })
                .collect();
            self.content_union(interner, &mapped)
        };
        vec![element]
    }

    /// `MutationWidening.widen_hash_shape` (`mutation_widening.rb:729`):
    /// closed `HashShape` → `Nominal[Hash, [Kunion, Vunion]]`. An empty
    /// shape degrades to a fully-untyped `Hash`. (The reference's `open?`
    /// arm has no port counterpart — the interner only mints closed
    /// shapes.) Values widen their pinning exactly as
    /// [`Self::widen_tuple_args`]' elements do.
    fn widen_hash_shape_args(
        &self,
        interner: &mut Interner,
        members: &[ShapeMember],
        keep_values: bool,
    ) -> Vec<TypeId> {
        if members.is_empty() {
            let u = self.content_untyped(interner);
            return vec![u, u];
        }
        let keys: Vec<ShapeKey> = members.iter().map(|m| m.key.clone()).collect();
        let key_t = self.key_union_for(interner, &keys);
        let values: Vec<TypeId> = members
            .iter()
            .map(|m| {
                if keep_values {
                    m.value
                } else {
                    self.widen_value_pinned(m.value, interner)
                }
            })
            .collect();
        let value_t = self.content_union(interner, &values);
        vec![key_t, value_t]
    }

    /// `ContentJoin.key_union_for` (`content_join.rb:510`): Symbol /
    /// String / Integer / Float keys widen to their class nominal; `true` /
    /// `false` / `nil` keep their constant carrier. A `ShapeKey::Other`
    /// names nothing — the reference maps `key.class.name`, and this seam
    /// cannot name the key's runtime class either.
    pub(crate) fn key_union_for(
        &self,
        interner: &mut Interner,
        keys: &[ShapeKey],
    ) -> TypeId {
        let mut carriers: Vec<TypeId> = Vec::new();
        for key in keys {
            let t = match key {
                ShapeKey::Bool(true) => Some(interner.true_()),
                ShapeKey::Bool(false) => Some(interner.false_()),
                ShapeKey::Nil => Some(interner.nil()),
                ShapeKey::Sym(_) => self
                    .index
                    .class_id("Symbol")
                    .map(|class| interner.intern(Type::Nominal { class, args: vec![] })),
                ShapeKey::Str(_) => self
                    .index
                    .class_id("String")
                    .map(|class| interner.intern(Type::Nominal { class, args: vec![] })),
                ShapeKey::Int(_) => self
                    .index
                    .class_id("Integer")
                    .map(|class| interner.intern(Type::Nominal { class, args: vec![] })),
                ShapeKey::Float(_) => self
                    .index
                    .class_id("Float")
                    .map(|class| interner.intern(Type::Nominal { class, args: vec![] })),
                ShapeKey::Other => None,
            };
            if let Some(t) = t {
                if !carriers.contains(&t) {
                    carriers.push(t);
                }
            }
        }
        self.content_union(interner, &carriers)
    }

    /// One `ShapeKey` as its `key_union_for` singleton carrier — the key
    /// half a `[]=` store's `arg_types` pair mints when the target's
    /// literal key is known.
    pub(crate) fn shape_key_carrier(
        &self,
        interner: &mut Interner,
        key: &ShapeKey,
    ) -> TypeId {
        self.key_union_for(interner, std::slice::from_ref(key))
    }

    // ----------------------------------------------------------------
    // Content evidence — the types a call's arguments put in.
    // ----------------------------------------------------------------

    /// `MutationWidening.value_pin_widened` (`mutation_widening.rb:678`):
    /// `widen_value_pinned` memberwise, then `shape_erased` — a stored
    /// literal collection joins as `Array[untyped]` / `Hash[untyped,
    /// untyped]`, not its shape.
    fn value_pin_widened_list(
        &self,
        interner: &mut Interner,
        types: &[TypeId],
    ) -> Vec<TypeId> {
        types
            .iter()
            .map(|&t| {
                let w = self.widen_value_pinned(t, interner);
                self.shape_erased(w, interner)
            })
            .collect()
    }

    /// `MutationWidening.shape_erased` (`mutation_widening.rb:695`).
    fn shape_erased(&self, ty: TypeId, interner: &mut Interner) -> TypeId {
        match interner.get(ty) {
            Type::Tuple(elems) => {
                let args = self.widen_tuple_args(interner, &elems.clone(), false);
                self.nominal_args(interner, "Array", args)
                    .unwrap_or(ty)
            }
            Type::HashShape(members) => {
                let args = self.widen_hash_shape_args(interner, &members.clone(), false);
                self.nominal_args(interner, "Hash", args).unwrap_or(ty)
            }
            _ => ty,
        }
    }

    /// `ContentJoin.array_added_elements` (`content_join.rb:60`): the
    /// element types `method`'s `arg_types` introduce into an Array.
    fn array_added_elements(
        &self,
        interner: &mut Interner,
        method: &str,
        arg_types: &[Option<TypeId>],
    ) -> Vec<TypeId> {
        if arg_types.is_empty() {
            return Vec::new();
        }
        match method {
            "concat" | "replace" => arg_types
                .iter()
                .flatten()
                .flat_map(|&t| self.collection_element_types(interner, t))
                .collect(),
            "insert" => arg_types.iter().skip(1).flatten().copied().collect(),
            "[]=" => match self.index_store_form(interner, arg_types) {
                IndexStoreForm::Splice => arg_types
                    .last()
                    .copied().flatten()
                    .map(|v| self.splice_stored_elements(interner, v))
                    .unwrap_or_default(),
                IndexStoreForm::Either => {
                    let mut out: Vec<TypeId> = arg_types.last().copied().flatten().into_iter().collect();
                    if let Some(v) = arg_types.last().copied().flatten() {
                        out.extend(self.splice_stored_elements(interner, v));
                    }
                    out
                }
                IndexStoreForm::Element => {
                    arg_types.last().copied().flatten().into_iter().collect()
                }
            },
            "fill" => {
                if arg_types.len() == 1 {
                    arg_types.iter().flatten().copied().collect()
                } else {
                    Vec::new()
                }
            }
            "<<" | "push" | "append" | "prepend" | "unshift" => {
                arg_types.iter().flatten().copied().collect()
            }
            _ => Vec::new(),
        }
    }

    /// `ContentJoin.index_store_form` (`content_join.rb:320`) over typed
    /// `[]=` arguments — the leading (index) positions carry the reference's
    /// splat `nil` mark as `None`.
    fn index_store_form(
        &self,
        interner: &Interner,
        arg_types: &[Option<TypeId>],
    ) -> IndexStoreForm {
        let leading = &arg_types[..arg_types.len().saturating_sub(1)];
        let definite: Vec<TypeId> = leading.iter().flatten().copied().collect();
        if definite.len() >= 2 {
            return IndexStoreForm::Splice;
        }
        if definite.is_empty() {
            return IndexStoreForm::Either;
        }
        if definite.len() == leading.len() {
            return self.single_index_form(interner, definite[0]);
        }
        if self.coll_range_index(interner, definite[0]) {
            IndexStoreForm::Splice
        } else {
            IndexStoreForm::Either
        }
    }

    /// `ContentJoin.single_index_form` (`content_join.rb:346`).
    fn single_index_form(&self, interner: &Interner, index_ty: TypeId) -> IndexStoreForm {
        let members = Self::content_members(interner, index_ty);
        if members
            .iter()
            .all(|&m| self.coll_definite_range(interner, m))
        {
            return IndexStoreForm::Splice;
        }
        if members
            .iter()
            .all(|&m| !self.coll_could_be_range(interner, m))
        {
            return IndexStoreForm::Element;
        }
        IndexStoreForm::Either
    }

    /// `ContentJoin.splice_stored_elements` (`content_join.rb:411`) — the
    /// element types a splice RHS puts in: a `Tuple`'s elements, an
    /// `Array` nominal's type args (a bare `Array`'s unknown elements),
    /// a definite Array subclass's unknown elements, or the member itself
    /// beside a `Dynamic[top]` arm for the `to_ary` coercion nothing can
    /// rule out.
    fn splice_stored_elements(&self, interner: &mut Interner, value_ty: TypeId) -> Vec<TypeId> {
        let mut out = Vec::new();
        for member in Self::content_members(interner, value_ty) {
            let mut base = member;
            while let Type::Refined { base: b, .. } | Type::Difference { base: b, .. } =
                interner.get(base)
            {
                base = *b;
            }
            match interner.get(base) {
                Type::Tuple(elems) => out.extend(elems.clone()),
                Type::Nominal { class, args } => {
                    if self.index.class_name_for_id(*class) == Some("Array") {
                        if args.is_empty() {
                            out.push(self.content_untyped(interner));
                        } else {
                            out.extend(args.clone());
                        }
                    } else if self.is_array_subclass(*class) {
                        out.push(self.content_untyped(interner));
                    } else {
                        out.push(member);
                        out.push(self.content_untyped(interner));
                    }
                }
                _ => {
                    out.push(member);
                    out.push(self.content_untyped(interner));
                }
            }
        }
        out
    }

    /// `ContentJoin.collection_element_types` (`content_join.rb:454`).
    fn collection_element_types(&self, interner: &Interner, ty: TypeId) -> Vec<TypeId> {
        match interner.get(ty) {
            Type::Tuple(elems) => elems.clone(),
            Type::Nominal { class, args }
                if self.index.class_name_for_id(*class) == Some("Array") =>
            {
                args.clone()
            }
            Type::Union(members) => members
                .clone()
                .iter()
                .flat_map(|&m| self.collection_element_types(interner, m))
                .collect(),
            Type::Difference { base, .. } => {
                let base = *base;
                self.collection_element_types(interner, base)
            }
            _ => Vec::new(),
        }
    }

    /// `ContentJoin.hash_shape_key_values` (`content_join.rb:471`):
    /// `[keys, values]` evidence from a Hash-ish carrier. (No `open` arm —
    /// the port mints only closed shapes.)
    fn hash_shape_key_values(&self, interner: &mut Interner, ty: TypeId) -> (Vec<TypeId>, Vec<TypeId>) {
        match interner.get(ty) {
            Type::HashShape(members) => {
                if members.is_empty() {
                    (Vec::new(), Vec::new())
                } else {
                    let members = members.clone();
                    let keys: Vec<ShapeKey> = members.iter().map(|m| m.key.clone()).collect();
                    let k = self.key_union_for(interner, &keys);
                    (vec![k], members.iter().map(|m| m.value).collect())
                }
            }
            Type::Nominal { class, args }
                if self.index.class_name_for_id(*class) == Some("Hash")
                    && args.len() == 2 =>
            {
                (vec![args[0]], vec![args[1]])
            }
            Type::Union(members) => {
                let mut ks = Vec::new();
                let mut vs = Vec::new();
                for &m in &members.clone() {
                    let (mk, mv) = self.hash_shape_key_values(interner, m);
                    ks.extend(mk);
                    vs.extend(mv);
                }
                (ks, vs)
            }
            Type::Difference { base, .. } => {
                let base = *base;
                self.hash_shape_key_values(interner, base)
            }
            _ => (Vec::new(), Vec::new()),
        }
    }

    /// `ContentJoin.union_members` — the union's arms or the type itself.
    fn content_members(interner: &Interner, ty: TypeId) -> Vec<TypeId> {
        match interner.get(ty) {
            Type::Union(members) => members.clone(),
            _ => vec![ty],
        }
    }

    // ----------------------------------------------------------------
    // Seed-admissibility — the straight-line join's signature gate.
    // ----------------------------------------------------------------

    /// `ContentJoin.admissible_evidence` (`content_join.rb:277`): `added`
    /// with every member the seed's class set does not admit replaced by
    /// `Dynamic[top]`. A seed carrying nothing to contradict — empty, or a
    /// `Dynamic` member — admits everything.
    fn admissible_evidence(
        &self,
        interner: &mut Interner,
        seed_members: &[TypeId],
        added: &[TypeId],
    ) -> Vec<TypeId> {
        let members: Vec<TypeId> = seed_members
            .iter()
            .flat_map(|&m| Self::content_members(interner, m))
            .collect();
        if members.is_empty()
            || members
                .iter()
                .any(|&m| matches!(interner.get(m), Type::Dynamic(_)))
        {
            return added.to_vec();
        }
        let classes: Vec<String> = members
            .iter()
            .filter_map(|&m| self.evidence_class(interner, m).map(str::to_string))
            .collect();
        added
            .iter()
            .flat_map(|&ty| {
                Self::content_members(interner, ty)
                    .into_iter()
                    .map(|m| match self.evidence_class(interner, m) {
                        Some(c) if classes.iter().any(|k| k == c) => m,
                        _ => self.content_untyped(interner),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// `ContentJoin.evidence_class` (`content_join.rb:301`): the class name
    /// a carrier commits its values to — `nil` when it commits to none,
    /// which is never admitted.
    fn evidence_class<'a>(&'a self, interner: &'a Interner, ty: TypeId) -> Option<&'a str> {
        match interner.get(ty) {
            Type::Nominal { class, .. } => self.index.class_name_for_id(*class),
            Type::Constant(s) => Some(scalar_class(s)),
            Type::Tuple(_) => Some("Array"),
            Type::HashShape(_) => Some("Hash"),
            _ => None,
        }
    }

    // ----------------------------------------------------------------
    // The joins — join_added_elements / join_added_pairs.
    // ----------------------------------------------------------------

    /// `MutationWidening.gradual_floor` (`mutation_widening.rb:578`): a
    /// straight-line join may never CLOSE the parameter it contributes to —
    /// every admissible list takes a `Dynamic[top]` arm.
    fn gradual_floor(&self, interner: &mut Interner, mut types: Vec<TypeId>) -> Vec<TypeId> {
        types.push(self.content_untyped(interner));
        types
    }

    /// `ContentJoin.join_array_content` (`content_join.rb:116`): the
    /// continuation `Array` carrier from the widened pre-state and the
    /// appended element types. The `with_residue` half is elided — every
    /// caller here passes a `Nominal[Array]` pre-state, whose residue is
    /// empty by construction.
    fn join_array_content(
        &self,
        interner: &mut Interner,
        pre_state: TypeId,
        added_elements: &[TypeId],
    ) -> TypeId {
        let mut elements = self.collection_element_types(interner, pre_state);
        elements.extend(added_elements.iter().copied());
        let element_t = if elements.is_empty() {
            self.content_untyped(interner)
        } else {
            self.content_union(interner, &elements)
        };
        self.nominal_args(interner, "Array", vec![element_t])
            .unwrap_or(pre_state)
    }

    /// `ContentJoin.join_hash_content` (`content_join.rb:127`): the
    /// continuation `Hash[K, V]` carrier — the seed's key and value arms
    /// survive beside the stored pairs'.
    fn join_hash_content(
        &self,
        interner: &mut Interner,
        pre_state: TypeId,
        added_pairs: &[(TypeId, TypeId)],
    ) -> TypeId {
        let (mut keys, mut values) = self.hash_shape_key_values(interner, pre_state);
        keys.extend(added_pairs.iter().map(|p| p.0));
        values.extend(added_pairs.iter().map(|p| p.1));
        let key_t = if keys.is_empty() {
            self.content_untyped(interner)
        } else {
            self.content_union(interner, &keys)
        };
        let value_t = if values.is_empty() {
            self.content_untyped(interner)
        } else {
            self.content_union(interner, &values)
        };
        self.nominal_args(interner, "Hash", vec![key_t, value_t])
            .unwrap_or(pre_state)
    }

    /// `RewriteMutation.arm` (`rewrite_mutation.rb:54`): `carrier` with
    /// each position `method` rewrites made gradual — replaced for a
    /// `REPLACED` name, joined for a `JOINED` one.
    fn rewrite_arm(&self, interner: &mut Interner, carrier: TypeId, method: &str) -> TypeId {
        let Type::Nominal { class, args } = interner.get(carrier) else {
            return carrier;
        };
        if args.is_empty() {
            return carrier;
        }
        let Some(name) = self.index.class_name_for_id(*class) else {
            return carrier;
        };
        let replaced = rewrite_replaced(name, method);
        let joined = rewrite_joined(name, method);
        if replaced.is_empty() && joined.is_empty() {
            return carrier;
        }
        let class = *class;
        let args = args.clone();
        let u = self.content_untyped(interner);
        let new_args: Vec<TypeId> = args
            .iter()
            .enumerate()
            .map(|(i, &a)| {
                if replaced.contains(&i) {
                    u
                } else if joined.contains(&i) {
                    self.content_union(interner, &[a, u])
                } else {
                    a
                }
            })
            .collect();
        interner.intern(Type::Nominal {
            class,
            args: new_args,
        })
    }

    /// `MutationWidening.join_added_elements` (`mutation_widening.rb:539`):
    /// join the mutator's own argument evidence into the already-widened
    /// Array carrier — value-pin widened, seed-admitted, gradual-floored.
    ///
    /// `seed_elements` is the PRE-STATE literal's element list, not the
    /// widened carrier's: `[]` and `[x]` on untyped `x` must widen to the
    /// same `Array[Dynamic[top]]`, and only the literal distinguishes "no
    /// evidence" from "one slot holds something unknown".
    fn join_added_elements(
        &self,
        interner: &mut Interner,
        widened: TypeId,
        method: &str,
        arg_types: &[Option<TypeId>],
        seed_elements: &[TypeId],
    ) -> TypeId {
        let widened = self.rewrite_arm(interner, widened, method);
        if !ARRAY_CONTENT_ADDERS.contains(&method) {
            return widened;
        }
        let raw_added = self.array_added_elements(interner, method, arg_types);
        let added = self.value_pin_widened_list(interner, &raw_added);
        if added.is_empty() && arg_types.is_empty() {
            return widened;
        }
        let admitted = if added.is_empty() {
            Vec::new()
        } else {
            self.admissible_evidence(interner, seed_elements, &added)
        };
        let floored = self.gradual_floor(interner, admitted);
        self.join_array_content(interner, widened, &floored)
    }

    /// `MutationWidening.join_added_pairs` (`mutation_widening.rb:585`): the
    /// Hash-side twin — `[]=`/`store` join the stored key and value into
    /// `Hash[K, V]`, each admitted against its OWN side's seed evidence.
    ///
    /// `admitted_union` (`mutation_widening.rb:610`): a `Hash` pair has a
    /// single slot per side, so the admissible list folds to a union and
    /// takes the same `gradual_floor` the element side takes.
    fn join_added_pairs(
        &self,
        interner: &mut Interner,
        widened: TypeId,
        method: &str,
        arg_types: &[Option<TypeId>],
        seed_keys: &[TypeId],
        seed_values: &[TypeId],
    ) -> TypeId {
        let widened = self.rewrite_arm(interner, widened, method);
        if !HASH_CONTENT_ADDERS.contains(&method) || arg_types.len() < 2 {
            return widened;
        }
        let key_arg = arg_types[0].unwrap_or_else(|| self.content_untyped(interner));
        let value_arg = arg_types[arg_types.len() - 1].unwrap_or_else(|| self.content_untyped(interner));
        let pair = self.value_pin_widened_list(interner, &[key_arg, value_arg]);
        if pair.len() != 2 {
            return widened;
        }
        let key_admitted = self.admissible_evidence(interner, seed_keys, &[pair[0]]);
        let key_floored = self.gradual_floor(interner, key_admitted);
        let key = self.content_union(interner, &key_floored);
        let value_admitted = self.admissible_evidence(interner, seed_values, &[pair[1]]);
        let value_floored = self.gradual_floor(interner, value_admitted);
        let value = self.content_union(interner, &value_floored);
        self.join_hash_content(interner, widened, &[(key, value)])
    }

    // ----------------------------------------------------------------
    // MutationRejoin — the already-widened carrier's second store.
    // ----------------------------------------------------------------

    /// `MutationRejoin.gradual_parameter?` (`mutation_rejoin.rb:274`): a
    /// parameter carrying a `Dynamic` arm — the carrier this seam (or a
    /// declaration) already opened.
    fn gradual_parameter(&self, interner: &Interner, ty: TypeId) -> bool {
        Self::content_members(interner, ty)
            .iter()
            .any(|&m| matches!(interner.get(m), Type::Dynamic(_)))
    }

    /// `MutationRejoin.regrowable_carrier?` (`mutation_rejoin.rb:131`): an
    /// `Array`/`Hash` nominal whose parameter already carries a gradual
    /// arm — a PRECISE nominal is refused (the `widen_precise_array` /
    /// `:synthesised` halves the port declines: `Array[E]` under a store
    /// keeps `E` here where the reference re-grows it with a floor — a
    /// message-and-coverage residual, never a wrong claim).
    fn regrowable_carrier(&self, interner: &Interner, ty: TypeId) -> bool {
        let Type::Nominal { class, args } = interner.get(ty) else {
            return false;
        };
        if !matches!(self.index.class_name_for_id(*class), Some("Array" | "Hash")) {
            return false;
        }
        args.iter().any(|&a| self.gradual_parameter(interner, a))
    }

    /// `MutationRejoin.keep_precise_parameters` (`mutation_rejoin.rb:64`):
    /// re-grow only the parameters that were already gradual — a `Hash`
    /// can carry one gradual and one PROVED parameter (`Hash.new(0)`'s
    /// `Hash[Dynamic[top], Integer]`), and the precise side stands.
    fn keep_precise_parameters(
        &self,
        interner: &mut Interner,
        pre_state: TypeId,
        rejoined: TypeId,
    ) -> TypeId {
        let (
            Type::Nominal {
                class: pre_class,
                args: pre_args,
            },
            Type::Nominal {
                class: re_class,
                args: re_args,
            },
        ) = (interner.get(pre_state), interner.get(rejoined))
        else {
            return rejoined;
        };
        if pre_args.len() != re_args.len() {
            return rejoined;
        }
        let pre_args = pre_args.clone();
        let re_args = re_args.clone();
        let class = *re_class;
        let _ = pre_class;
        let args: Vec<TypeId> = pre_args
            .iter()
            .zip(re_args.iter())
            .map(|(&prior, &re)| {
                if self.gradual_parameter(interner, prior) {
                    re
                } else {
                    prior
                }
            })
            .collect();
        interner.intern(Type::Nominal { class, args })
    }

    /// `MutationRejoin.pinned_evidence` (`mutation_rejoin.rb:113`): the
    /// carrier's VALUE-PINNED arms — the claim a literal seed made, which
    /// `admissible_evidence` gates added members against on a re-opened
    /// carrier.
    fn pinned_evidence(&self, interner: &Interner, members: &[TypeId]) -> Vec<TypeId> {
        members
            .iter()
            .flat_map(|&m| Self::content_members(interner, m))
            .filter(|&m| matches!(interner.get(m), Type::Constant(_)))
            .collect()
    }

    /// `MutationRejoin.widen_nominal` (`mutation_rejoin.rb:34`) — the
    /// ordinary (non-synthesised, non-precise) arm: re-open a carrier this
    /// seam already widened once so the SECOND and later stores into the
    /// same collection are seen. `nil` when the re-join changed nothing, so
    /// a non-adder on an already-widened carrier does not rebind.
    pub(crate) fn rejoin_nominal(
        &self,
        interner: &mut Interner,
        ty: TypeId,
        method: &str,
        arg_types: &[Option<TypeId>],
    ) -> Option<TypeId> {
        let Type::Nominal { class, .. } = interner.get(ty) else {
            return None;
        };
        let class_name = self.index.class_name_for_id(*class)?;
        if !matches!(class_name, "Array" | "Hash") || !self.regrowable_carrier(interner, ty) {
            return None;
        }
        let keep = !VALUE_REWRITING_MUTATORS.contains(&method)
            || rewrite_keeps_values(class_name, method);
        let pre_state = if keep {
            ty
        } else {
            self.value_pin_widened_args(interner, ty)
        };
        let rejoined = match class_name {
            "Array" => {
                if !crate::ARRAY_MUTATORS.contains(&method) {
                    return None;
                }
                let seed = self.pinned_evidence(
                    interner,
                    &self.collection_element_types(interner, pre_state),
                );
                self.join_added_elements(interner, pre_state, method, arg_types, &seed)
            }
            "Hash" => {
                if !crate::HASH_MUTATORS.contains(&method) {
                    return None;
                }
                let (sk, sv) = self.hash_shape_key_values(interner, pre_state);
                let seed_k = self.pinned_evidence(interner, &sk);
                let seed_v = self.pinned_evidence(interner, &sv);
                self.join_added_pairs(interner, pre_state, method, arg_types, &seed_k, &seed_v)
            }
            _ => return None,
        };
        let rejoined = self.keep_precise_parameters(interner, pre_state, rejoined);
        let rejoined = self.rewrite_arm(interner, rejoined, method);
        (rejoined != ty).then_some(rejoined)
    }

    /// `MutationRejoin.value_pin_widened_args` (`mutation_rejoin.rb:357`):
    /// the nominal with every type argument `widen_value_pinned` — the
    /// re-opened-carrier half of `widen_tuple`'s `values: :widen`.
    fn value_pin_widened_args(&self, interner: &mut Interner, ty: TypeId) -> TypeId {
        let Type::Nominal { class, args } = interner.get(ty) else {
            return ty;
        };
        let class = *class;
        let args = args.clone();
        let new_args: Vec<TypeId> = args
            .iter()
            .map(|&a| self.widen_value_pinned(a, interner))
            .collect();
        interner.intern(Type::Nominal {
            class,
            args: new_args,
        })
    }

    /// The nominal a literal carrier widens to under `method` — the port of
    /// the `Tuple` / `HashShape` arms of `widen_carrier`, content args
    /// included. `keep` is the resolved `values:` mode (`:keep` under
    /// `RewriteMutation.values_mode`).
    pub(crate) fn widened_shape_nominal(
        &self,
        interner: &mut Interner,
        ty: TypeId,
        method: &str,
    ) -> Option<TypeId> {
        match interner.get(ty) {
            Type::Tuple(elems) if crate::ARRAY_MUTATORS.contains(&method) => {
                let elems = elems.clone();
                let keep = !VALUE_REWRITING_MUTATORS.contains(&method)
                    || rewrite_keeps_values("Array", method);
                let args = self.widen_tuple_args(interner, &elems, keep);
                self.nominal_args(interner, "Array", args)
            }
            Type::HashShape(members) if crate::HASH_MUTATORS.contains(&method) => {
                let members = members.clone();
                let keep = !VALUE_REWRITING_MUTATORS.contains(&method)
                    || rewrite_keeps_values("Hash", method);
                let args = self.widen_hash_shape_args(interner, &members, keep);
                self.nominal_args(interner, "Hash", args)
            }
            _ => None,
        }
    }

    /// The seed-and-store join for a literal carrier — the
    /// `join_added_elements` / `join_added_pairs` call `widen_carrier`
    /// wraps the widened mint in.
    pub(crate) fn join_mutation_content(
        &self,
        interner: &mut Interner,
        pre_ty: TypeId,
        widened: TypeId,
        method: &str,
        arg_types: &[Option<TypeId>],
    ) -> TypeId {
        match interner.get(pre_ty) {
            Type::Tuple(elems) => {
                let elems = elems.clone();
                self.join_added_elements(interner, widened, method, arg_types, &elems)
            }
            Type::HashShape(_) => {
                let (sk, sv) = self.hash_shape_key_values(interner, pre_ty);
                self.join_added_pairs(interner, widened, method, arg_types, &sk, &sv)
            }
            _ => widened,
        }
    }

    /// `UnknownStoreWidening.gradual_content`
    /// (`unknown_store_widening.rb:283`): every type argument of an
    /// `Array`/`Hash` nominal joined with `Dynamic[top]`, member-wise
    /// through a `Union` and a `Difference`'s base — the alias floor's
    /// unknown-store finish.
    pub(crate) fn gradual_content(&self, interner: &mut Interner, ty: TypeId) -> TypeId {
        match interner.get(ty) {
            Type::Union(members) => {
                let mapped: Vec<TypeId> = members
                    .clone()
                    .iter()
                    .map(|&m| self.gradual_content(interner, m))
                    .collect();
                self.content_union(interner, &mapped)
            }
            Type::Difference { base, removed } => {
                let (base, removed) = (*base, *removed);
                let b = self.gradual_content(interner, base);
                interner.intern(Type::Difference {
                    base: b,
                    removed,
                })
            }
            Type::Nominal { class, args }
                if matches!(
                    self.index.class_name_for_id(*class),
                    Some("Array" | "Hash")
                ) && !args.is_empty() =>
            {
                let class = *class;
                let args = args.clone();
                let u = self.content_untyped(interner);
                let new_args: Vec<TypeId> = args
                    .iter()
                    .map(|&a| self.content_union(interner, &[a, u]))
                    .collect();
                interner.intern(Type::Nominal {
                    class,
                    args: new_args,
                })
            }
            _ => ty,
        }
    }

    /// `UnknownStoreWidening.literal_carrier?`
    /// (`unknown_store_widening.rb:236`): a carrier that still records its
    /// slots or keys — a `Tuple`/`HashShape`, alone or in a `Union`.
    pub(crate) fn literal_carrier(interner: &Interner, ty: TypeId) -> bool {
        match interner.get(ty) {
            Type::Tuple(_) | Type::HashShape(_) => true,
            Type::Union(members) => members.iter().any(|&m| Self::literal_carrier(interner, m)),
            _ => false,
        }
    }
}
