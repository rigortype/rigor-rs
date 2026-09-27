//! Project constants: the C5 literal-constant harvest and its lexical, per-file lookups, the
//! stage-2 write census, and the C1 constant-shadow gate.

use std::collections::{HashMap, HashSet};

use rigor_parse::{FileKey, LoweredAst, Node, NodeId};
use rigor_types::{Scalar, ShapeKey};

use super::{qualify, split_qualified, ConstLit, HarvestedConst, HarvestedConstWrite, SourceIndex};

impl SourceIndex {
    /// C5: the harvested fully-literal value of constant `name` VISIBLE at a use
    /// site with lexical prefix `use_prefix`, or `None`. A recorded entry applies
    /// iff its defining namespace is an initial segment run of `use_prefix` (Ruby
    /// lexical lookup: toplevel is visible everywhere, a nested constant only
    /// within its namespace); among visible entries the LONGEST-namespace
    /// (innermost) wins. The `ConstantRead` arm consults this BEFORE the
    /// singleton gate and re-interns the value via `Typer::intern_const_lit`.
    ///
    /// `use_file` is the [`rigor_parse::FileKey`] of the file the use site is in,
    /// and an entry only applies when it matches the file that ASSIGNED the
    /// constant. The reference's constant-value table is per-file (see the field
    /// docs); before this gate a same-namespace cross-file read folded here and
    /// was silent in the oracle.
    pub fn literal_constant(
        &self,
        name: &str,
        use_prefix: &[String],
        use_file: &FileKey,
    ) -> Option<&ConstLit> {
        self.literal_constants
            .get(name)?
            .iter()
            .filter(|(_, file, _)| file == use_file)
            .filter(|(ns, _, _)| ns.len() <= use_prefix.len() && use_prefix[..ns.len()] == ns[..])
            .max_by_key(|(ns, _, _)| ns.len())
            .map(|(_, _, lit)| lit)
    }

    /// Collection-shape stage 2b's negative check: whether SOME harvested
    /// literal constant named `name` is lexically visible at `use_prefix`,
    /// **ignoring which file assigned it**.
    ///
    /// Deliberately file-AGNOSTIC, and deliberately separate from
    /// [`Self::literal_constant`]. That method types a use site, so slice A's
    /// per-file gate belongs there; this one is a DECLINE predicate whose job is
    /// to be maximally conservative, and narrowing it would be the one way slice
    /// A could make the `ENV` arm fire somewhere it previously stayed silent.
    /// (In practice `project_writes_constant` already subsumes it — that set is
    /// built from every constant write before the literal gates — but the
    /// redundancy is the point.)
    pub fn literal_constant_visible_any_file(&self, name: &str, use_prefix: &[String]) -> bool {
        self.literal_constants.get(name).is_some_and(|entries| {
            entries.iter().any(|(ns, _, _)| {
                ns.len() <= use_prefix.len() && use_prefix[..ns.len()] == ns[..]
            })
        })
    }

    /// Collection-shape stage 2b: whether the project ASSIGNS a constant with
    /// this bare name anywhere (see [`Self::project_constant_write_names`]).
    pub fn project_writes_constant(&self, name: &str) -> bool {
        self.project_constant_write_names.contains(name)
    }

    /// Collection-shape stage 2e: the harvested value of a constant named by a
    /// QUALIFIED path (`::A::B::C::CONST` / `A::B::C::CONST` — both lower to one
    /// `ConstantRead` whose `name` is `"A::B::C::CONST"`, the leading `::` is not
    /// preserved). `None` for a bare name (the C5 map owns those).
    ///
    /// Resolution mirrors PR #64's precedent for qualified REFERENCES: the path
    /// is taken AS WRITTEN and tried against the use site's lexical contexts —
    /// every initial segment run of `use_prefix` (innermost first) plus the
    /// top-level reading. **Ambiguity DECLINES**: if two distinct candidate keys
    /// both name a harvested constant, we return `None` rather than guess which
    /// one Ruby's lookup would reach (a strict under-emit; the reference resolves
    /// it precisely, so this can only lose recall, never fire wrongly).
    ///
    /// A resolved key must then pass the SAME lexical-visibility filter
    /// [`Self::literal_constant`] applies to the bare spelling: the constant's
    /// DEFINING namespace has to be an initial segment run of `use_prefix`. This
    /// makes stage 2e a pure SPELLING extension of the already-shipped C5 gate
    /// rather than a wider resolution reach — load-bearing, and measured: without
    /// it, a cross-namespace path (gitlab
    /// `Gitlab::GitalyClient::DiffBlob::ATTRS` read from
    /// `…::DiffBlobsStitcher`) folded here while the reference stayed silent, an
    /// oracle FP on the sweep.
    ///
    /// `use_file` applies the SAME per-file consumption gate as
    /// [`Self::literal_constant`] — a qualified path is only a SPELLING of the
    /// same harvest, so it inherits the same restriction.
    pub fn qualified_literal_constant(
        &self,
        name: &str,
        use_prefix: &[String],
        use_file: &FileKey,
    ) -> Option<&ConstLit> {
        if !name.contains("::") {
            return None;
        }
        let mut hit: Option<(&String, &HarvestedConst)> = None;
        // Candidate keys: `<prefix[..i]>::<name>` for every i (the lexical
        // nesting runs), plus `name` itself (i == 0 yields exactly that).
        for i in 0..=use_prefix.len() {
            let key = if i == 0 {
                name.to_string()
            } else {
                format!("{}::{}", use_prefix[..i].join("::"), name)
            };
            if let Some((k, v)) = self.qualified_literal_constants.get_key_value(&key) {
                match hit {
                    // The same constant reached by two spellings is not an
                    // ambiguity; two DIFFERENT keys are.
                    Some((prev, _)) if prev != k => return None,
                    Some(_) => {}
                    None => hit = Some((k, v)),
                }
            }
        }
        let (_, (ns, file, lit)) = hit?;
        (file == use_file && ns.len() <= use_prefix.len() && use_prefix[..ns.len()] == ns[..])
            .then_some(lit)
    }

    /// C1 (constant-shadow gate): whether a BARE read of constant `name` at a use
    /// site with lexical prefix `use_prefix` (the enclosing class/module segment
    /// vector, empty at toplevel) is SHADOWED by a project definition — i.e. the
    /// project name resolves in Ruby's lexical lookup, so the core-RBS singleton
    /// must NOT be witnessed. This REPLACES the pre-C1 bare-name project-wide
    /// `!knows_class(name)` suppression with a lexically precise one, matching the
    /// reference's `lexical_constant_candidates` walk:
    ///
    ///   * a TOPLEVEL project definition shadows everywhere;
    ///   * a NESTED definition `N::name` shadows only where `N` is an initial
    ///     segment run of `use_prefix` (`N` ∈ `Module.nesting` of the use site);
    ///   * a name known as a project class but placed by the qualified walk at
    ///     neither position (def-nested / walk gap) falls back to the pre-C1
    ///     blanket suppression — ambiguity resolves to silent (never an FP).
    ///
    /// FP-safe by construction: the only behavior change vs the old gate is that a
    /// nested-only definition STOPS suppressing at use sites it is not lexically
    /// visible from — a strict relaxation whose every new firing the reference
    /// (which resolves identically-lexically) confirms.
    pub fn constant_shadowed(&self, name: &str, use_prefix: &[String]) -> bool {
        if self.toplevel_constants.contains(name) {
            return true;
        }
        match self.nested_constant_namespaces.get(name) {
            Some(namespaces) => namespaces.iter().any(|ns| {
                ns.len() <= use_prefix.len() && use_prefix[..ns.len()] == ns[..]
            }),
            // Not seen by the qualified walk at all: preserve pre-C1 behavior for
            // any project class the walk did not qualify (def-nested / walk gap).
            None => self.classes.contains_key(name),
        }
    }

    /// Whether the project defines a constant named `name` ANYWHERE (toplevel,
    /// nested, or as a discovered class/module) — the scope-INDEPENDENT
    /// companion to [`Self::constant_shadowed`]. Used by `type_dot_new`'s
    /// stdlib-mint decline: a project-defined name colliding with a loaded-RBS
    /// short key (`Selector = Data.define(...)` vs an RBS `Selector`) keeps its
    /// project mint regardless of the caller's lexical-scope attachment
    /// (callers without `with_lexical_scopes` have an empty prefix, which would
    /// make the lexical predicate miss a nested definition). Conservative
    /// toward KEEPING the mint — the pre-existing behavior.
    pub fn constant_defined_anywhere(&self, name: &str) -> bool {
        self.toplevel_constants.contains(name)
            || self.nested_constant_namespaces.contains_key(name)
            || self.classes.contains_key(name)
    }
}

/// C5: the static scalar key a hash-key NODE denotes, or `None` when dynamic.
/// Mirrors the Typer's `static_shape_key_of_node` (the reference's
/// `HashShape::ALLOWED_KEY_CLASSES`) so a harvested hash pins the same slots.
fn const_shape_key_of(node: &Node) -> Option<ShapeKey> {
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

/// C5a: recursively collect ONE FILE's lexically-qualified `CONST = <literal>`
/// writes from `ast` under lexical `prefix`, in walk order. The first write of a
/// qualified name appends `(qualified, defining namespace, harvested value,
/// writes: 1)` to `out`; every repeat bumps that entry's `writes` (and never
/// re-harvests the value — the FIRST write wins, as before). Only
/// class/module/program BODIES are walked (a def-nested constant is out of
/// scope), mirroring the C1 override / fold discovery inclusion rule.
///
/// `seen` maps a qualified name to its position in `out` — a per-file scratch
/// map the caller owns so the recursion stays cheap.
///
/// The project-wide single-assignment gate is NOT here: [`SourceIndex::merge`]
/// sums the counts across files (Σ ≥ 2 ⇒ declined) and takes the first file's
/// value, which is exactly what one shared `first`/`multi` pair used to do.
pub(crate) fn collect_literal_constants(
    ast: &LoweredAst,
    node: NodeId,
    prefix: &[String],
    out: &mut Vec<HarvestedConstWrite>,
    seen: &mut HashMap<String, usize>,
    mutated: &HashSet<String>,
) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                collect_literal_constants(ast, child, prefix, out, seen, mutated);
            }
        }
        Node::ClassDef { name, body, .. } | Node::ModuleDef { name, body, .. } => {
            if name.is_empty() {
                return;
            }
            let child_prefix = split_qualified(&qualify(prefix, name));
            for &child in body {
                collect_literal_constants(ast, child, &child_prefix, out, seen, mutated);
            }
        }
        Node::ConstantWrite { name, value, .. } => {
            let qualified = qualify(prefix, name);
            match seen.get(&qualified) {
                Some(&at) => out[at].writes += 1,
                None => {
                    // Issue #540: a constant this file MUTATES is harvested
                    // Dynamic-wrapped, so a read no longer folds through the
                    // literal shape. Applied at insert time rather than as a
                    // second pass over the accumulator — the census is a pure
                    // function of the same file, so the result is identical and
                    // the merge stays untouched.
                    let lit = const_lit_of(ast, *value)
                        .map(|l| widen_if_mutated(&qualified, l, mutated));
                    seen.insert(qualified.clone(), out.len());
                    out.push(HarvestedConstWrite {
                        qualified,
                        namespace: prefix.to_vec(),
                        lit,
                        writes: 1,
                    });
                }
            }
        }
        _ => {}
    }
}

/// Wrap a harvested constant value in [`ConstLit::Widened`] when the file's own
/// mutation census names it (reference `widen_mutated_constants`).
pub(crate) fn widen_if_mutated(qualified: &str, lit: ConstLit, mutated: &HashSet<String>) -> ConstLit {
    if mutated.contains(qualified) {
        ConstLit::Widened(Box::new(lit))
    } else {
        lit
    }
}

/// Upstream #540 (`fc3b8b42`) — the qualified constant names THIS FILE mutates,
/// from the lowering's raw census ([`rigor_parse::ConstMutation`]).
///
/// A site counts when it is an `Index{Or,And,Operator}Write` / attribute-or-index
/// writer (`method: None`) or when its method is in upstream's `SHAPE_MUTATORS`
/// ([`crate::is_shape_mutator`]) — the reference's `mutating_receiver_of`,
/// which read `ARRAY_MUTATORS` ∪ `HASH_MUTATORS` alone until pin `e59b7b89`
/// added the String and `HashLookupMutation` tables. A BARE receiver
/// name contributes EVERY lexical-resolution candidate (`A::B` + `C` yields
/// `A::B::C` and `C`, mirroring how the reads resolve); a `A::B` PATH receiver
/// contributes only the name as written.
///
/// Same-file scope: the census is built per file, so a mutation in ANOTHER file
/// never widens this file's fold — which is also what the reference's per-file
/// `in_source_constants` table gives.
pub(crate) fn mutated_constant_names(ast: &LoweredAst) -> HashSet<String> {
    let mut out = HashSet::new();
    for m in ast.const_mutations() {
        if let Some(method) = &m.method {
            if !crate::is_shape_mutator(method) {
                continue;
            }
        }
        if m.receiver.is_empty() {
            continue;
        }
        if m.receiver_is_path {
            out.insert(m.receiver.clone());
            continue;
        }
        // `constant_mutation_candidates`: `[A, B]` + `C` -> `A::B::C`, `A::C`, `C`.
        for keep in 0..=m.prefix.len() {
            let prefix = &m.prefix[..m.prefix.len() - keep];
            out.insert(if prefix.is_empty() {
                m.receiver.clone()
            } else {
                format!("{}::{}", prefix.join("::"), m.receiver)
            });
        }
    }
    out
}

/// C5: harvest a `ConstLit` from a constant's RHS `node`, or `None` when the RHS
/// is not FULLY literal (declining the whole constant). Recurses into array /
/// hash elements — any non-literal element declines the entire structure (a
/// splat / dynamic key / non-literal value ⇒ `None`), so a recorded value is
/// always exactly the carrier the Typer builds for the same inline literal.
pub(crate) fn const_lit_of(ast: &LoweredAst, node: NodeId) -> Option<ConstLit> {
    match ast.get(node) {
        Node::IntegerLit { value, .. } => value.map(|v| ConstLit::Scalar(Scalar::Int(v))),
        Node::FloatLit { value, .. } => Some(ConstLit::Scalar(Scalar::Float(*value))),
        Node::StringLit { value, .. } => Some(ConstLit::Scalar(Scalar::Str(value.clone()))),
        Node::SymbolLit { value, .. } => Some(ConstLit::Scalar(Scalar::Sym(value.clone()))),
        Node::TrueLit { .. } => Some(ConstLit::Scalar(Scalar::Bool(true))),
        Node::FalseLit { .. } => Some(ConstLit::Scalar(Scalar::Bool(false))),
        Node::NilLit { .. } => Some(ConstLit::Scalar(Scalar::Nil)),
        Node::ArrayLit { elements, .. } => {
            let mut elems = Vec::with_capacity(elements.len());
            for &e in elements {
                // Slice B: one non-literal element no longer declines the whole
                // constant — it degrades to the projection-inert bare nominal.
                // The reference never declines here either (it types the hole
                // and keeps a Tuple), so this is a spelling of the SAME
                // constant, one precision tier lower.
                match const_lit_of(ast, e) {
                    Some(l) => elems.push(l),
                    None => return Some(ConstLit::BareArray),
                }
            }
            Some(ConstLit::Tuple(elems))
        }
        Node::HashLit { elements, all_assoc, .. } => {
            if !*all_assoc {
                // A `**` splat / non-assoc element. The reference degrades to a
                // widened `Hash[K, V]` (probes p3b/p3b2), never declines.
                return Some(ConstLit::BareHash);
            }
            let mut members: Vec<(ShapeKey, ConstLit)> = Vec::with_capacity(elements.len() / 2);
            let mut i = 0;
            while i + 1 < elements.len() {
                // A dynamic key (probe p3c) or a non-literal value (p1, the
                // lambda-hash shape) degrades the container, not the constant.
                let Some(key) = const_shape_key_of(ast.get(elements[i])) else {
                    return Some(ConstLit::BareHash);
                };
                let Some(value) = const_lit_of(ast, elements[i + 1]) else {
                    return Some(ConstLit::BareHash);
                };
                // Last-wins on a duplicate key (mirrors `hash_shape_or_hash`).
                if let Some(m) = members.iter_mut().find(|m| m.0 == key) {
                    m.1 = value;
                } else {
                    members.push((key, value));
                }
                i += 2;
            }
            Some(ConstLit::Hash(members))
        }
        Node::Range { .. } => Some(ConstLit::Range),
        // `.freeze` is identity on the literal (M2-GO slice 1): the ubiquitous
        // `CONST = %w[...].freeze` / `{...}.freeze` spelling (RuboCop's
        // Style/MutableConstant autocorrect) harvests as the literal underneath.
        // Zero-arg, block-free `freeze` only; recursion makes nested
        // `["a".freeze].freeze` work at any depth. The reference folds the same
        // way (probed: `A = %w[a b].freeze; A.exclude?("c")` fires there).
        Node::Call { receiver: Some(r), method, args, block_body, .. }
            if method == "freeze" && args.is_empty() && block_body.is_empty() =>
        {
            const_lit_of(ast, *r)
        }
        _ => None,
    }
}
