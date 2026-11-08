//! The in-place-mutation / closure-write marks behind the def-local env
//! (rigor-rs#546 review).
//!
//! The straight-line replay ([`Typer::bind_statements_into`]) binds a local to
//! the literal carrier it was written with (`r = []` → `Tuple[]`, `s = "a"` →
//! `Constant["a"]`). A later in-place call on that local — `r << 1`,
//! `s.upcase!`, `h[:a] = 1`, a mutation inside an `each` block or a lambda —
//! makes the carrier stale: the reference's `MutationWidening` widens the
//! binding at the call site (`mutation_widening.rb`, `widen_after_call` /
//! `widen_after_block`), but the replay does not model that widening, so
//! without this table a signature would carry the pre-mutation literal
//! (`-> []` for an accumulator that grows) — an unsound emit.
//!
//! The marks are the DECLINE half of the reference rule: every local a
//! mutation or a closure write may touch is recorded by SPAN (orphan-proof:
//! a node under a lossily lowered wrapper is still found), and the replay
//! widens each such binding that holds a stale-able carrier to `untyped`
//! after the statement that contains the mark — a coverage loss against the
//! reference's precise widened type (`Array[untyped]`, `String`), never a
//! wrong signature.

use std::rc::Rc;

use rigor_parse::{LoweredAst, Node, Span};
use rigor_types::{Interner, Scalar, Type, TypeId};

use crate::flow_writes::MUTATOR_METHODS;
use crate::typer::Typer;
use crate::TypeEnv;

/// Span-sorted marks, two kinds. `mutations`: locals an in-place call /
/// index write / element mutation may change (stale only for a carrier —
/// see [`mutation_stable`]). `rebinds`: locals written inside a block or
/// lambda body (any binding goes stale — the closure may run later, or
/// never, or many times).
#[derive(Default)]
pub(crate) struct DefLocalMarks {
    mutations: Vec<(Span, Vec<String>)>,
    rebinds: Vec<(Span, Vec<String>)>,
    /// Every local read `(start, end, name)` and write `(start, name)`,
    /// sorted by start — the loop-carried-return scan asks what a span
    /// reads and what it writes.
    reads: Vec<(usize, usize, String)>,
    writes: Vec<(usize, String)>,
}

/// The [`Typer`]'s memo of [`DefLocalMarks`], keyed by `(ast identity, arena length)`.
pub(crate) type DefMarksCache = std::cell::RefCell<Option<((usize, usize), Rc<DefLocalMarks>)>>;

/// A call name that may change its receiver in place: the reference's
/// `SHAPE_MUTATORS` tables (the port's [`MUTATOR_METHODS`] plus the hash
/// lookup mutators) and every bang method — a user- or core-defined `foo!`
/// conventionally mutates, and declining is the safe side.
fn mutating_call(method: &str) -> bool {
    MUTATOR_METHODS.contains(&method)
        || rigor_parse::mutators::is_shape_mutator(method)
        || (method.len() > 1 && method.ends_with('!'))
}

impl DefLocalMarks {
    pub(crate) fn build(ast: &LoweredAst) -> Self {
        // Every local read, by start offset: the names a receiver expression
        // can evaluate to are the reads inside its span.
        let mut reads: Vec<(usize, usize, &str)> = Vec::new();
        // Every local write (name by start offset), for the closure scan.
        let mut all_writes: Vec<(usize, String)> = Vec::new();
        for (_, n) in ast.iter() {
            match n {
                Node::LocalVariableRead { name, span } => reads.push((span.0, span.1, name)),
                Node::LocalVariableWrite { name, span, .. }
                | Node::LocalVariableOpWrite { name, span, .. } => {
                    all_writes.push((span.0, name.clone()));
                }
                Node::MultiWrite { targets, span, .. } => {
                    for (name, _) in targets.bound_names() {
                        all_writes.push((span.0, name));
                    }
                }
                Node::UnmodeledWrite { span, names } => {
                    for name in names {
                        all_writes.push((span.0, name.clone()));
                    }
                }
                _ => {}
            }
        }
        reads.sort_by_key(|r| r.0);

        let reads_in = |lo: usize, hi: usize| -> Vec<String> {
            let i = reads.partition_point(|r| r.0 < lo);
            reads[i..]
                .iter()
                .take_while(|r| r.0 < hi)
                .filter(|r| r.1 <= hi)
                .map(|r| r.2.to_string())
                .collect()
        };

        let mut mutations: Vec<(Span, Vec<String>)> = Vec::new();
        let mut closures: Vec<Span> = Vec::new();
        let mut block_calls: Vec<Span> = Vec::new();
        for (_, n) in ast.iter() {
            match n {
                Node::Call { receiver, method, span, block_span, .. } => {
                    if let Some(r) = receiver {
                        if mutating_call(method) {
                            let rs = ast.get(*r).span();
                            mutations.push((*span, reads_in(rs.0, rs.1)));
                        }
                    }
                    if let Some(bs) = block_span {
                        closures.push(*bs);
                        block_calls.push(*span);
                    }
                }
                // `h[k] op= v` / `h.attr op= v`: stores through `[]=` /
                // `attr=` on the receiver.
                Node::IndexWrite { receiver: Some(r), span, .. }
                | Node::AttrWrite { receiver: Some(r), span, .. } => {
                    let rs = ast.get(*r).span();
                    mutations.push((*span, reads_in(rs.0, rs.1)));
                }
                // `h[k], z = …` stores through `[]=` on `h`; an attribute
                // target (`h.a, z = …`) is not modelled, so every read left
                // of the `=` is a possible mutation target.
                Node::MultiWrite { targets, value, span, .. } => {
                    let names: Vec<String> =
                        targets.index_writes().into_iter().map(|(n, _, _)| n).collect();
                    if !names.is_empty() {
                        mutations.push((*span, names));
                    }
                    let vs = ast.get(*value).span();
                    let mut left = reads_in(span.0, vs.0.max(span.0));
                    if !left.is_empty() {
                        left.sort();
                        left.dedup();
                        mutations.push((*span, left));
                    }
                }
                Node::Loop { index_writes, span, .. } => {
                    let names: Vec<String> =
                        index_writes.iter().map(|(n, _, _)| n.clone()).collect();
                    if !names.is_empty() {
                        mutations.push((*span, names));
                    }
                }
                Node::BeginRescue { clauses, span, .. } => {
                    let names: Vec<String> = clauses
                        .iter()
                        .flat_map(|c| c.index_writes.iter().map(|(n, _, _)| n.clone()))
                        .collect();
                    if !names.is_empty() {
                        mutations.push((*span, names));
                    }
                }
                Node::Lambda { span, .. } => closures.push(*span),
                _ => {}
            }
        }
        // `DeepErasure`: a block call / `for` whose body mutates an element
        // in place drops the shapes its receiver's locals carry.
        for (span, names) in ast.element_mutations() {
            mutations.push((*span, names.clone()));
        }
        mutations.sort_by_key(|m| m.0 .0);

        // A block call whose body holds ANY mutation (on a block parameter,
        // a captured local, anything) may mutate what its receiver and
        // arguments hand it — every local read inside the call is a
        // candidate. Coarser than `widen_element_aliases`, never unsound.
        let mut extra: Vec<(Span, Vec<String>)> = Vec::new();
        for call_span in &block_calls {
            let has = {
                let i = mutations.partition_point(|m| m.0 .0 < call_span.0);
                mutations[i..]
                    .iter()
                    .take_while(|m| m.0 .0 < call_span.1)
                    .any(|m| m.0 .1 <= call_span.1 && m.0 != *call_span)
            };
            if has {
                extra.push((*call_span, reads_in(call_span.0, call_span.1)));
            }
        }
        mutations.extend(extra);
        mutations.sort_by_key(|m| m.0 .0);

        // Closure writes: every local written inside a block / lambda body.
        all_writes.sort_by_key(|w| w.0);
        let mut rebinds: Vec<(Span, Vec<String>)> = Vec::new();
        for cs in closures {
            let i = all_writes.partition_point(|w| w.0 < cs.0);
            let names: Vec<String> = all_writes[i..]
                .iter()
                .take_while(|w| w.0 < cs.1)
                .map(|w| w.1.clone())
                .collect();
            if !names.is_empty() {
                rebinds.push((cs, names));
            }
        }
        rebinds.sort_by_key(|m| m.0 .0);
        let reads = reads.into_iter().map(|(a, b, n)| (a, b, n.to_string())).collect();
        DefLocalMarks { mutations, rebinds, reads, writes: all_writes }
    }

    /// The names written (rebound) or possibly mutated inside `span`.
    pub(crate) fn changed_within(&self, span: Span) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let i = self.writes.partition_point(|w| w.0 < span.0);
        out.extend(self.writes[i..].iter().take_while(|w| w.0 < span.1).map(|w| w.1.clone()));
        for names in Self::within(&self.mutations, span).chain(Self::within(&self.rebinds, span)) {
            out.extend(names.iter().cloned());
        }
        out.sort();
        out.dedup();
        out
    }

    /// The local names read inside `span`.
    pub(crate) fn reads_within(&self, span: Span) -> Vec<&str> {
        let i = self.reads.partition_point(|r| r.0 < span.0);
        self.reads[i..]
            .iter()
            .take_while(|r| r.0 < span.1)
            .filter(|r| r.1 <= span.1)
            .map(|r| r.2.as_str())
            .collect()
    }

    /// Whether any local read inside `span` names one of `names`.
    pub(crate) fn reads_any_within(&self, span: Span, names: &[String]) -> bool {
        let i = self.reads.partition_point(|r| r.0 < span.0);
        self.reads[i..]
            .iter()
            .take_while(|r| r.0 < span.1)
            .any(|r| r.1 <= span.1 && names.contains(&r.2))
    }

    fn within(marks: &[(Span, Vec<String>)], (lo, hi): Span) -> impl Iterator<Item = &Vec<String>> {
        let i = marks.partition_point(|m| m.0 .0 < lo);
        marks[i..]
            .iter()
            .take_while(move |m| m.0 .0 < hi)
            .filter(move |m| m.0 .1 <= hi)
            .map(|m| &m.1)
    }
}

/// Whether `ty` survives an in-place call unchanged: an immutable scalar, an
/// integer range, a class object, or a nominal with no type arguments (a
/// bare `String` / `Foo` the call cannot make more precise or less true).
/// Everything else — a `Tuple`, a `HashShape`, a `Constant` String, a
/// generic `Array[T]`, a refinement, a union holding one — carries a literal
/// fact the call may falsify.
fn mutation_stable(interner: &Interner, ty: TypeId) -> bool {
    match interner.get(ty) {
        Type::Constant(Scalar::Str(_)) => false,
        Type::Constant(_)
        | Type::IntegerRange { .. }
        | Type::Singleton(_)
        | Type::Bottom
        | Type::Top
        | Type::Dynamic(_) => true,
        Type::Nominal { args, .. } => args.is_empty(),
        Type::Union(ms) => ms.iter().all(|&m| mutation_stable(interner, m)),
        _ => false,
    }
}

impl<'i> Typer<'i> {
    /// The marks of `ast`, built on first use and memoized per `ast`
    /// identity (the `reach_index` keying).
    pub(crate) fn def_local_marks(&self, ast: &LoweredAst) -> Rc<DefLocalMarks> {
        let key = (ast as *const LoweredAst as usize, ast.len());
        if let Some((cached_key, marks)) = &*self.def_marks.borrow() {
            if *cached_key == key {
                return marks.clone();
            }
        }
        let marks = Rc::new(DefLocalMarks::build(ast));
        *self.def_marks.borrow_mut() = Some((key, marks.clone()));
        marks
    }

    /// Widen to `untyped` every binding in `env` that a mutation or closure
    /// write inside `span` may have made stale (see the module docs).
    pub(crate) fn apply_def_local_marks(
        &self,
        ast: &LoweredAst,
        env: &mut TypeEnv,
        span: Span,
        interner: &mut Interner,
    ) {
        let marks = self.def_local_marks(ast);
        let untyped = interner.untyped();
        for names in DefLocalMarks::within(&marks.mutations, span) {
            for name in names {
                if let Some(&ty) = env.get(name) {
                    if !mutation_stable(interner, ty) {
                        env.insert(name.clone(), untyped);
                    }
                }
            }
        }
        for names in DefLocalMarks::within(&marks.rebinds, span) {
            for name in names {
                if env.get(name).is_some() {
                    env.insert(name.clone(), untyped);
                }
            }
        }
    }
}
