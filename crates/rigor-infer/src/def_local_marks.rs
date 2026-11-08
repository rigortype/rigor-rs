//! The marks behind the def-local env: what the straight-line replay
//! ([`Typer::bind_statements_into`]) does NOT model, so it declines
//! (rigor-rs#546 reviews).
//!
//! The replay binds a local to the literal carrier it was written with
//! (`r = []` → `Tuple[]`, `s = "a"` → `Constant["a"]`) and joins the arms of a
//! few conditional shapes. The reference's flow evaluator does far more, so
//! every place the two can disagree must make the replay give up — bind the
//! local to `untyped`, which a tail read turns into a skipped method (a
//! coverage loss, never a wrong signature). The tables here record those
//! places by SPAN (orphan-proof: a node under a lossily lowered wrapper is
//! still found):
//!
//! - `mutations` / `escapes` / `rebinds` — in-place calls (`r << 1`,
//!   `s.upcase!`, `h[:a] = 1`), locals handed to a callee, closure writes: the
//!   reference's `MutationWidening` widens the binding at the call site, the
//!   replay widens a stale-able carrier to `untyped` after the statement.
//! - `writes` — every local write position (including `for` indexes,
//!   `rescue => e`, pattern bindings): a write at a position the replay does not
//!   walk (a predicate, an argument, an interpolation, an `else` clause) is stale
//!   ([`Typer::widen_shell`]).
//! - `decisions` — `if` / `case` / loop predicates, `&&` / `||` lefts, `&.`
//!   receivers: the reference narrows and constant-folds on a local a decision
//!   reads, the replay does not, so a statement or tail holding a decision on a
//!   typed local is declined ([`Typer::typed_decision_reads`]).
//! - `exits` — `return` / `break` / `next` / `redo` / `retry` / `raise`: the
//!   reference drops a terminating arm from a join, rejects a loop that
//!   `break`s from the zero-or-more model, and re-enters a `begin` on `retry`.
//! - `wild` — a named-capture `=~` binds locals with unknown names.
//! - `scopes` — nested `def` / `class` / `module` bodies are another scope.

use std::rc::Rc;

use rigor_parse::{JumpKind, LoweredAst, Node, Span, StatementsKind};
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
pub struct DefLocalMarks {
    mutations: Vec<(Span, Vec<String>)>,
    /// Locals handed to a call as an argument (see [`passes_argument`]):
    /// the callee may mutate the object in place. Applied like `mutations`,
    /// but kept apart — a block call whose body merely passes its own
    /// parameter on (`a.each { |x| puts(x) }`) does not make the receiver's
    /// locals candidates (the reference keeps `a` there).
    escapes: Vec<(Span, Vec<String>)>,
    rebinds: Vec<(Span, Vec<String>)>,
    /// Every local read `(start, end, name)` and write `(start, name)`,
    /// sorted by start — the loop-carried-return scan asks what a span
    /// reads and what it writes.
    reads: Vec<(usize, usize, String)>,
    writes: Vec<(usize, String)>,
    /// Decision positions, sorted by start: an `if`/`unless`/ternary
    /// predicate, a `case` subject / `when` condition / `in` pattern, a loop
    /// predicate, the left operand of `&&` / `||`, the receiver of `&.`.
    /// The reference NARROWS (and constant-folds) the locals a decision reads;
    /// the replay does not, so a construct whose decision reads a typed local
    /// is declined (`Typer::typed_decision_reads`).
    decisions: Vec<(usize, usize)>,
    /// Control-flow exits `(start, kind)`, sorted: `return`, `break`, `next`,
    /// `redo`, `retry`, and calls that never return (`raise`, `exit`, ...).
    exits: Vec<(usize, ExitKind)>,
    /// Spans of nested `def` / `class` / `module` bodies: a different local
    /// scope, so a write inside one binds nothing in the enclosing def.
    scopes: Vec<(usize, usize)>,
    /// Starts of `=~` calls whose receiver is a regexp literal (or a lossily
    /// lowered carrier): a named capture binds locals Prism models as a
    /// `MatchWriteNode` and the arena has no node for, so the names are
    /// unknown — every typed local is stale after one.
    wild: Vec<usize>,
    /// `(position, local)` of every type test on a local — `x.nil?`,
    /// `x.is_a?(K)`, `x == v`, `x =~ r`, `case x` — sorted by position.
    narrow_tests: Vec<(usize, String)>,
}

/// A call the reference narrows its receiver on (`x.nil?`, `x.is_a?(K)`,
/// `x == v`, ...).
fn narrowing_test(method: &str) -> bool {
    matches!(
        method,
        "nil?" | "is_a?" | "kind_of?" | "instance_of?" | "==" | "!=" | "===" | "=~" | "!~"
            | "equal?" | "eql?" | "respond_to?"
    )
}

fn jump_exit(k: JumpKind) -> ExitKind {
    match k {
        JumpKind::Next => ExitKind::Next,
        JumpKind::Break => ExitKind::Break,
        JumpKind::Redo => ExitKind::Redo,
        JumpKind::Retry => ExitKind::Retry,
    }
}

/// What kind of control-flow exit an [`DefLocalMarks::exits`] entry is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExitKind {
    Return,
    Next,
    Break,
    Redo,
    Retry,
    /// A call that never returns: `raise` / `fail` / `exit` / `exit!` /
    /// `abort` / `throw`.
    Raise,
}

/// The call names the reference types `bot` (RBS) and its terminating-branch
/// detection recognises.
fn exit_call(method: &str) -> bool {
    matches!(method, "raise" | "fail" | "exit" | "exit!" | "abort" | "throw")
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

/// Whether a call named `method` hands its arguments to code that may
/// mutate them in place. The reference's callee-mutated-parameter analysis
/// (`StatementEvaluator#argument_mutating_callees`) widens a local passed to
/// a project def (self-call, `Const.m`, `Const.new.m`), to the opaque sinks
/// (`call`, `.()`, `send`, `instance_exec`, ...) and through `&blk`; the
/// marks do not resolve callees, so EVERY call passing a local counts —
/// except an arithmetic / comparison operator, which no callee is expected
/// to use for an in-place change of its operand. Over-declining a local
/// passed to a core method (`puts(a)`) is a coverage loss, never a wrong
/// signature.
fn passes_argument(method: &str) -> bool {
    !matches!(
        method,
        "+" | "-" | "*" | "/" | "%" | "**" | "==" | "!=" | "<" | ">" | "<=" | ">=" | "<=>" | "==="
            | "=~" | "!~" | "&" | "|" | "^" | ">>" | "!" | "-@" | "+@" | "~"
    )
}

/// The local names an argument expression hands over BY REFERENCE: a bare
/// local read, looking through the carriers an argument position lowers
/// splats / parentheses / keyword hashes into. A nested call (`f(a.dup)`,
/// `f(g(a))`) is judged on its own arguments; a container literal
/// (`f([a])`) is not followed, as in the reference.
fn argument_locals(ast: &LoweredAst, id: rigor_parse::NodeId, out: &mut Vec<String>) {
    match ast.get(id) {
        Node::LocalVariableRead { name, .. } => out.push(name.clone()),
        Node::Statements { body, .. } => {
            for &b in body {
                argument_locals(ast, b, out);
            }
        }
        Node::HashLit { elements, .. } => {
            for &e in elements {
                argument_locals(ast, e, out);
            }
        }
        _ => {}
    }
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
        let mut escapes: Vec<(Span, Vec<String>)> = Vec::new();
        let mut closures: Vec<Span> = Vec::new();
        let mut block_calls: Vec<Span> = Vec::new();
        let mut decisions: Vec<(usize, usize)> = Vec::new();
        let mut exits: Vec<(usize, ExitKind)> = Vec::new();
        let mut scopes: Vec<(usize, usize)> = Vec::new();
        let mut wild: Vec<usize> = Vec::new();
        let mut narrow_tests: Vec<(usize, String)> = Vec::new();
        for (_, n) in ast.iter() {
            // Decision positions, exits and nested scopes (see the field docs).
            match n {
                Node::If { predicate, .. } => decisions.push(ast.get(*predicate).span()),
                Node::Case { predicate, branches, .. } => {
                    if let Some(p) = predicate {
                        decisions.push(ast.get(*p).span());
                        if let Node::LocalVariableRead { name, span } = ast.get(*p) {
                            narrow_tests.push((span.0, name.clone()));
                        }
                    }
                    for &b in branches {
                        match ast.get(b) {
                            Node::When { conditions, .. } => {
                                decisions.extend(conditions.iter().map(|&c| ast.get(c).span()));
                            }
                            // An `in` branch: the pattern leads the carrier's body.
                            Node::BeginRescue { body, .. } => {
                                decisions.extend(body.first().map(|&p| ast.get(p).span()));
                            }
                            other => decisions.push(other.span()),
                        }
                    }
                }
                Node::Loop { predicate: Some(p), .. } => decisions.push(ast.get(*p).span()),
                Node::Logical { left, .. } => decisions.push(ast.get(*left).span()),
                Node::Return { span, .. } => exits.push((span.0, ExitKind::Return)),
                Node::Other { span, jump: Some(k) }
                | Node::Statements { span, kind: StatementsKind::Jump(k), .. } => {
                    exits.push((span.0, jump_exit(*k)));
                }
                Node::Definition { span, .. }
                | Node::ClassDef { span, .. }
                | Node::ModuleDef { span, .. } => scopes.push(*span),
                Node::Call { receiver, method, safe_nav, span, .. } => {
                    if exit_call(method) {
                        exits.push((span.0, ExitKind::Raise));
                    }
                    if narrowing_test(method) {
                        if let Some(r) = receiver {
                            if let Node::LocalVariableRead { name, .. } = ast.get(*r) {
                                narrow_tests.push((span.0, name.clone()));
                            }
                        }
                    }
                    if *safe_nav {
                        if let Some(r) = receiver {
                            decisions.push(ast.get(*r).span());
                        }
                    }
                    if method == "=~" {
                        if let Some(r) = receiver {
                            if matches!(
                                ast.get(*r),
                                Node::RegexpLit { .. }
                                    | Node::Statements { .. }
                                    | Node::Other { .. }
                            ) {
                                wild.push(span.0);
                            }
                        }
                    }
                }
                // `for x in c` / `for a, b in c` write the index names; a
                // `rescue => e` writes its reference.
                Node::BeginRescue { clauses, .. } => {
                    for c in clauses {
                        if let Some(name) = &c.bound_name {
                            all_writes.push((c.span.0, name.clone()));
                        }
                    }
                }
                _ => {}
            }
            if let Node::Loop { index, .. } = n {
                for (name, sp) in index {
                    all_writes.push((sp.0, name.clone()));
                }
            }
            match n {
                Node::Call { receiver, method, args, block_body, span, block_span, .. } => {
                    if passes_argument(method) {
                        let mut names: Vec<String> = Vec::new();
                        for &a in args {
                            argument_locals(ast, a, &mut names);
                        }
                        // `&expr` block-pass: the expression rides `block_body`
                        // of a call with no literal block.
                        if block_span.is_none() {
                            for &b in block_body {
                                argument_locals(ast, b, &mut names);
                            }
                        }
                        if !names.is_empty() {
                            names.sort();
                            names.dedup();
                            escapes.push((*span, names));
                        }
                    }
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
        escapes.sort_by_key(|m| m.0 .0);

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
        decisions.sort_unstable();
        exits.sort_by_key(|e| e.0);
        scopes.sort_unstable();
        wild.sort_unstable();
        narrow_tests.sort();
        DefLocalMarks { mutations, escapes, rebinds, reads, writes: all_writes, decisions, exits, scopes, wild, narrow_tests }
    }

    /// The names written (rebound) or possibly mutated inside `span`.
    pub fn changed_within(&self, span: Span) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let i = self.writes.partition_point(|w| w.0 < span.0);
        out.extend(self.writes[i..].iter().take_while(|w| w.0 < span.1).map(|w| w.1.clone()));
        for names in Self::within(&self.mutations, span)
            .chain(Self::within(&self.escapes, span))
            .chain(Self::within(&self.rebinds, span))
        {
            out.extend(names.iter().cloned());
        }
        out.sort();
        out.dedup();
        out
    }

    /// The local names WRITTEN inside `span` (nested `def` / `class` /
    /// `module` scopes excluded) — no mutation, escape or closure marks.
    pub fn written_within(&self, span: Span) -> Vec<String> {
        self.written_outside(span, &[], None)
    }

    /// The local names read inside `span`.
    pub fn reads_within(&self, span: Span) -> Vec<&str> {
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

    /// The local names read inside a decision position contained in `span`.
    pub(crate) fn decision_reads_within(&self, span: Span) -> Vec<&str> {
        let i = self.decisions.partition_point(|d| d.0 < span.0);
        let mut out: Vec<&str> = Vec::new();
        for d in self.decisions[i..].iter().take_while(|d| d.0 < span.1).filter(|d| d.1 <= span.1) {
            out.extend(self.reads_within(*d));
        }
        out
    }

    /// Whether `name`, within the def at `scope`, may be a Dynamic local the
    /// reference has narrowed to a foldable type: a type test on it AND at
    /// least two decision positions reading it (`raise unless c.is_a?(Integer)`
    /// then `a = 2 if c` — the second decision constant-folds).
    pub fn narrowing_hot(&self, scope: Span, name: &str) -> bool {
        let i = self.narrow_tests.partition_point(|t| t.0 < scope.0);
        let tested = self.narrow_tests[i..]
            .iter()
            .take_while(|t| t.0 < scope.1)
            .any(|t| t.1 == name);
        let first = self.decisions.partition_point(|d| d.0 < scope.0);
        tested
            && self.decisions[first..]
                .iter()
                .take_while(|d| d.0 < scope.1)
                .filter(|d| d.1 <= scope.1)
                .filter(|d| self.reads_within((d.0, d.1)).contains(&name))
                .take(2)
                .count()
                >= 2
    }

    /// Whether a decision position inside `span` reads one of `names`.
    pub(crate) fn reads_any_within_decisions(&self, span: Span, names: &[String]) -> bool {
        self.decision_reads_within(span).iter().any(|r| names.iter().any(|n| n == r))
    }

    /// Whether `span` holds a control-flow exit of one of `kinds`.
    pub fn has_exit_within(&self, span: Span, kinds: &[ExitKind]) -> bool {
        let i = self.exits.partition_point(|e| e.0 < span.0);
        self.exits[i..]
            .iter()
            .take_while(|e| e.0 < span.1)
            .any(|e| kinds.contains(&e.1))
    }

    /// Whether `span` holds an `=~` that may bind named captures.
    pub(crate) fn wild_within(&self, span: Span) -> bool {
        let i = self.wild.partition_point(|&w| w < span.0);
        self.wild.get(i).is_some_and(|&w| w < span.1)
    }

    /// The local names written inside `span` at a position that is not inside
    /// one of `bodies` (the sub-spans the replay itself walks) and not inside
    /// a nested `def` / `class` / `module`, and not at `skip_pos` (the
    /// statement's own modelled target). These writes are the ones the replay
    /// does not see — a predicate, an argument, an interpolation, a value
    /// operand, a pattern, an `else` clause.
    pub(crate) fn written_outside(
        &self,
        span: Span,
        bodies: &[Span],
        skip_pos: Option<usize>,
    ) -> Vec<String> {
        let nested: Vec<(usize, usize)> = {
            let i = self.scopes.partition_point(|s| s.0 < span.0);
            self.scopes[i..]
                .iter()
                .take_while(|s| s.0 < span.1)
                .filter(|s| s.1 <= span.1 && **s != span)
                .copied()
                .collect()
        };
        let i = self.writes.partition_point(|w| w.0 < span.0);
        let mut out: Vec<String> = self.writes[i..]
            .iter()
            .take_while(|w| w.0 < span.1)
            .filter(|w| Some(w.0) != skip_pos)
            .filter(|w| !bodies.iter().any(|b| b.0 <= w.0 && w.0 < b.1))
            .filter(|w| !nested.iter().any(|b| b.0 <= w.0 && w.0 < b.1))
            .map(|w| w.1.clone())
            .collect();
        out.sort();
        out.dedup();
        out
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
    pub fn def_local_marks(&self, ast: &LoweredAst) -> Rc<DefLocalMarks> {
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
        for names in
            DefLocalMarks::within(&marks.mutations, span).chain(DefLocalMarks::within(&marks.escapes, span))
        {
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

/// The span covering a statement list: first start to last end.
pub fn body_span(ast: &LoweredAst, stmts: &[rigor_parse::NodeId]) -> Option<Span> {
    let lo = stmts.iter().map(|&s| ast.get(s).span().0).min()?;
    let hi = stmts.iter().map(|&s| ast.get(s).span().1).max()?;
    Some((lo, hi))
}

/// Call names that conventionally return (never `bot`) — a receiver-less
/// call to anything else as an arm's last statement may be a helper the
/// reference types `bot` (a terminating arm).
fn benign_call(method: &str) -> bool {
    matches!(
        method,
        "puts" | "print" | "p" | "pp" | "printf" | "putc" | "warn" | "require" | "require_relative"
            | "sleep" | "format" | "sprintf" | "rand" | "srand" | "gets" | "binding" | "lambda"
            | "proc" | "block_given?" | "attr_reader" | "attr_writer" | "attr_accessor"
    )
}

impl<'i> Typer<'i> {
    /// The typed locals (any binding but `untyped`) read in a decision
    /// position inside `span`. The reference narrows / constant-folds on a
    /// decision; the replay does not, so a statement or tail holding one is
    /// declined (`Typer::replay_opaque`, `Typer::tail_env`).
    pub(crate) fn typed_decision_reads(
        &self,
        marks: &DefLocalMarks,
        env: &TypeEnv,
        span: Span,
        interner: &Interner,
    ) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let scope = self.def_scope.get();
        for n in marks.decision_reads_within(span) {
            if out.iter().any(|o| o == n) {
                continue;
            }
            let typed = env.get(n).is_some_and(|&t| !matches!(interner.get(t), Type::Dynamic(_)));
            // A Dynamic local the reference may have narrowed to a type
            // (`raise unless c.is_a?(Integer)`) folds a second decision on it.
            let hot = !typed && scope.is_some_and(|sc| marks.narrowing_hot(sc, n));
            if typed || hot {
                out.push(n.to_string());
            }
        }
        out
    }

    /// Bind every name in `names` to `untyped` (present or not: a name a
    /// non-replayed position wrote is bound there, to something unknown).
    pub fn widen_names(env: &mut TypeEnv, names: &[String], interner: &mut Interner) {
        let untyped = interner.untyped();
        for n in names {
            env.insert(n.clone(), untyped);
        }
    }

    /// Bind every local in `env` to `untyped`.
    pub(crate) fn widen_all(env: &mut TypeEnv, interner: &mut Interner) {
        let untyped = interner.untyped();
        for t in env.values_mut() {
            *t = untyped;
        }
    }

    /// The spans of the bodies the replay itself walks inside statement
    /// `id`. Everything else in the statement — a predicate, an argument, an
    /// interpolation, a pattern, an `else` clause — is a "shell" the replay
    /// does not execute: a local written there is stale
    /// ([`Self::widen_shell`]).
    pub(crate) fn stmt_bodies(&self, ast: &LoweredAst, id: rigor_parse::NodeId) -> Vec<Span> {
        let mut out: Vec<Span> = Vec::new();
        match ast.get(id) {
            Node::If { then_body, else_body, .. } => {
                out.extend(body_span(ast, then_body));
                out.extend(body_span(ast, else_body));
            }
            Node::Logical { right, .. } => out.push(ast.get(*right).span()),
            Node::BeginRescue { main_body, body, clauses, ensure_body, has_else, .. } => {
                if !*has_else {
                    if clauses.is_empty() && ensure_body.is_empty() {
                        out.extend(body_span(ast, body));
                    } else {
                        out.extend(body_span(ast, main_body));
                        for c in clauses {
                            out.extend(body_span(ast, &c.body));
                        }
                        out.extend(body_span(ast, ensure_body));
                    }
                }
            }
            Node::Case { branches, else_body, .. } => {
                if branches.iter().all(|&b| matches!(ast.get(b), Node::When { .. })) {
                    for &b in branches {
                        if let Node::When { body, .. } = ast.get(b) {
                            out.extend(body_span(ast, body));
                        }
                    }
                    out.extend(body_span(ast, else_body));
                }
            }
            Node::Loop { body, .. } => out.extend(body_span(ast, body)),
            Node::Call { block_body, block_span: Some(_), .. } => out.extend(body_span(ast, block_body)),
            Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
                out.extend(body_span(ast, body));
            }
            _ => {}
        }
        out
    }

    /// Bind to `untyped` every local written inside statement `id` at a
    /// position the replay does not walk (see [`Self::stmt_bodies`]).
    pub(crate) fn widen_shell(
        &self,
        ast: &LoweredAst,
        marks: &DefLocalMarks,
        env: &mut TypeEnv,
        id: rigor_parse::NodeId,
        interner: &mut Interner,
    ) {
        let span = ast.get(id).span();
        // A nested `def` / `class` / `module` is another local scope.
        if matches!(ast.get(id), Node::Definition { .. } | Node::ClassDef { .. } | Node::ModuleDef { .. }) {
            return;
        }
        let skip = match ast.get(id) {
            Node::LocalVariableWrite { .. } | Node::MultiWrite { .. } => Some(span.0),
            _ => None,
        };
        let bodies = self.stmt_bodies(ast, id);
        let names = marks.written_outside(span, &bodies, skip);
        Self::widen_names(env, &names, interner);
    }

    /// The env a tail node is typed under, or `None` to decline it: a tail
    /// holding a decision on a typed local, or a named-capture `=~`, is not
    /// modelled; a write the replay does not walk inside it is widened.
    pub fn tail_env(
        &self,
        ast: &LoweredAst,
        env: &TypeEnv,
        id: rigor_parse::NodeId,
        interner: &mut Interner,
    ) -> Option<TypeEnv> {
        let marks = self.def_local_marks(ast);
        let span = ast.get(id).span();
        if marks.wild_within(span) || !self.typed_decision_reads(&marks, env, span, interner).is_empty()
        {
            return None;
        }
        let mut out = env.clone();
        self.widen_shell(ast, &marks, &mut out, id, interner);
        Some(out)
    }

    /// Whether the statement list `arm` may leave the construct other than
    /// by falling off its end — a `return` / `break` / `next` / `redo` /
    /// `retry` / `raise` anywhere in it, or a last statement that is a call
    /// the reference may type `bot` (a project helper that always raises).
    /// The reference drops such an arm from the join; the replay does not
    /// model that, so the names the arm writes are declined.
    pub(crate) fn arm_suspect(&self, ast: &LoweredAst, arm: &[rigor_parse::NodeId]) -> bool {
        let Some(sp) = body_span(ast, arm) else { return false };
        let marks = self.def_local_marks(ast);
        if marks.has_exit_within(
            sp,
            &[ExitKind::Return, ExitKind::Next, ExitKind::Break, ExitKind::Redo, ExitKind::Retry, ExitKind::Raise],
        ) {
            return true;
        }
        let Some(&last) = arm.last() else { return false };
        if let Node::Call { receiver, method, .. } = ast.get(last) {
            let guessable = match receiver {
                None => true,
                Some(r) => matches!(ast.get(*r), Node::SelfExpr { .. } | Node::ConstantRead { .. }),
            };
            if guessable && !benign_call(method) {
                return true;
            }
        }
        false
    }
}

/// Whether `id` is built from literals alone — no local, ivar or
/// receiver-less call: the reference constant-folds such a decision
/// (`if true`, `if 1 > 2`, `if [1]`, `true && false`), so a replay that
/// cannot decide it must decline rather than join both arms.
pub fn literal_like(ast: &LoweredAst, id: rigor_parse::NodeId) -> bool {
    match ast.get(id) {
        Node::NilLit { .. }
        | Node::TrueLit { .. }
        | Node::FalseLit { .. }
        | Node::IntegerLit { .. }
        | Node::FloatLit { .. }
        | Node::StringLit { .. }
        | Node::SymbolLit { .. }
        | Node::RegexpLit { .. }
        | Node::RationalLit { .. }
        | Node::ImaginaryLit { .. }
        | Node::SelfExpr { .. }
        | Node::ConstantRead { .. } => true,
        Node::ArrayLit { elements, .. } | Node::HashLit { elements, .. } => {
            elements.iter().all(|&e| literal_like(ast, e))
        }
        Node::Range { left, right, .. } => {
            left.iter().chain(right.iter()).all(|&e| literal_like(ast, e))
        }
        Node::Logical { left, right, .. } => literal_like(ast, *left) && literal_like(ast, *right),
        Node::Call { receiver: Some(r), args, block_body, .. } => {
            block_body.is_empty() && literal_like(ast, *r) && args.iter().all(|&a| literal_like(ast, a))
        }
        // A sequence / parenthesised group is decided by its LAST statement;
        // a write evaluates to its value.
        Node::Statements { body, kind: StatementsKind::Sequence, .. } => {
            body.last().is_some_and(|&b| literal_like(ast, b))
        }
        Node::BeginRescue { body, clauses, ensure_body, has_else: false, .. }
            if clauses.is_empty() && ensure_body.is_empty() =>
        {
            body.last().is_some_and(|&b| literal_like(ast, b))
        }
        Node::LocalVariableWrite { value, .. }
        | Node::VariableWrite { value, .. }
        | Node::InstanceVariableWrite { value, .. }
        | Node::ConstantWrite { value, .. } => literal_like(ast, *value),
        _ => false,
    }
}
