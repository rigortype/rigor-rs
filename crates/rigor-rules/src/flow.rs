//! The `flow.*` rules, except `flow.shadowed-rescue-clause` (in
//! `shadowed_rescue`): always-truthy condition, unreachable branch, always
//! raises, dead assignment, duplicate hash key and return in ensure.

use std::collections::{HashMap, HashSet};

use rigor_index::CoreIndex;
use rigor_infer::Typer;
use rigor_parse::{HashKeyTag, LoweredAst, Node, NodeId};
use rigor_types::{Interner, Scalar, Type};

use crate::{
    catalog, span_within, Diagnostic, Severity, FLOW_ALWAYS_RAISES, FLOW_ALWAYS_TRUTHY_CONDITION,
    FLOW_DEAD_ASSIGNMENT, FLOW_DUPLICATE_HASH_KEY, FLOW_RETURN_IN_ENSURE, FLOW_UNREACHABLE_BRANCH,
};

/// The Integer division/modulo operators that raise `ZeroDivisionError` on a
/// zero Integer divisor — verbatim the reference's `INTEGER_RAISING_OPERATORS`
/// (`%i[/ % div modulo divmod]`). The op set is closed: Float `/` returns
/// `Infinity` (no raise), and other methods are not modeled here.
const INTEGER_RAISING_OPERATORS: &[&str] = &["/", "%", "div", "modulo", "divmod"];

/// The defensive predicate selectors the reference's
/// `AlwaysTruthyConditionCollector` skips: a predicate call to one of these reads
/// like an explicit runtime check the (strict-on-returns) type system disagrees
/// with — skipping them keeps the rule on genuine logic errors, not defensive
/// code. Verbatim the reference's `DEFENSIVE_PREDICATES`.
const DEFENSIVE_PREDICATES: &[&str] =
    &["nil?", "empty?", "zero?", "any?", "none?", "all?", "respond_to?"];

/// Build the `flow.always-truthy-condition` diagnostic for one `Node::If`, or
/// `None` (a DECLINE — never a false positive). Fires iff the predicate folds to
/// a `Type::Constant` in the recorded flow snapshot AND is not in the reference's
/// skip envelope:
///   - a SYNTACTIC literal predicate (owned by `flow.unreachable-branch`) →
///     declined here so the two rules never double-fire;
///   - a defensive predicate call (`nil?`/`empty?`/…) → declined;
///   - a loop/block-nested predicate → already absent from `snapshots`.
///
/// The diagnostic anchors on the predicate node span (the reference's
/// `Diagnostic.from_node(predicate_node)`).
pub(crate) fn check_always_truthy(
    ast: &LoweredAst,
    if_id: rigor_parse::NodeId,
    predicate: rigor_parse::NodeId,
    snapshots: &std::collections::HashMap<rigor_parse::NodeId, rigor_types::TypeId>,
    interner: &Interner,
) -> Option<Diagnostic> {
    // Skip syntactic literals (unreachable-branch's domain) and defensive calls.
    if literal_predicate_truthy(ast, predicate).is_some() {
        return None;
    }
    if matches!(ast.get(predicate), Node::Call { method, .. } if DEFENSIVE_PREDICATES.contains(&method.as_str()))
    {
        return None;
    }
    let ty = *snapshots.get(&if_id)?;
    let polarity = constant_polarity(interner, ty)?;

    let span = ast.get(predicate).span();
    let severity = catalog(FLOW_ALWAYS_TRUTHY_CONDITION)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Warning);

    Some(Diagnostic {
        rule_id: FLOW_ALWAYS_TRUTHY_CONDITION,
        start_offset: span.0,
        end_offset: span.1,
        message: format!(
            "condition is always {polarity} (the surrounding flow proves it folds to a constant)"
        ),
        severity,
        source_family: "builtin",
        receiver_type: None,
        method_name: None,
    })
}

/// The polarity word for a constant predicate, or `None` if `ty` is not a
/// `Type::Constant`. Mirrors the reference exactly: a `nil` or `false` constant
/// is `falsey`, every other constant (Integer/Float/String/Symbol/`true`) is
/// `truthy` (in Ruby only `nil`/`false` are falsey).
fn constant_polarity(interner: &Interner, ty: rigor_types::TypeId) -> Option<&'static str> {
    match interner.get(ty) {
        Type::Constant(Scalar::Nil) | Type::Constant(Scalar::Bool(false)) => Some("falsey"),
        Type::Constant(_) => Some("truthy"),
        _ => None,
    }
}

/// `:truthy` / `:falsey` polarity of a SYNTACTICALLY-LITERAL predicate, or `None`
/// for anything else (a variable, constant, call, interpolated string, …). In
/// Ruby every value except `false`/`nil` is truthy — so `true`/Integer/Float/
/// String/Symbol literals are truthy, and only `false`/`nil` are falsey. This
/// mirrors the reference's `TRUTHY_LITERAL_NODES`/`FALSEY_LITERAL_NODES` exactly,
/// with two parity notes carried from the oracle:
///   - An INTERPOLATED string (`"a#{x}"`, a `Node::InterpolatedString`) is NOT a
///     literal here — the reference matches `StringNode` only, not
///     `InterpolatedStringNode` — so it is declined.
///   - A bare-regexp predicate (`if /re/`) is a `MatchLastLineNode` in Prism, not
///     a `RegularExpressionNode`, so the reference does not flag it; rigor-rs has
///     no regexp-literal node at all, so the case is naturally absent.
fn literal_predicate_truthy(ast: &LoweredAst, predicate: rigor_parse::NodeId) -> Option<bool> {
    match ast.get(predicate) {
        Node::TrueLit { .. }
        | Node::IntegerLit { .. }
        | Node::FloatLit { .. }
        | Node::StringLit { .. }
        | Node::SymbolLit { .. } => Some(true),
        Node::FalseLit { .. } | Node::NilLit { .. } => Some(false),
        _ => None,
    }
}

/// Build the `flow.unreachable-branch` diagnostic for one `Node::If`, or `None`
/// (a DECLINE — never a false positive) when the predicate is not a literal or
/// the dead branch is empty/absent. The keyword-inversion is the keystone: for an
/// `if`, a truthy predicate kills the ELSE branch and a falsey one kills the THEN
/// branch; an `unless` INVERTS both. The diagnostic anchors on the DEAD branch:
///   - THEN dead → the then-body's first statement (the reference anchors on the
///     `StatementsNode`, whose start is its first statement — col matches).
///   - ELSE dead → the lowered `else`/subsequent node, whose span starts at the
///     `else` keyword (matching the reference's `from_node(node.subsequent)` /
///     `from_node(node.else_clause)`).
pub(crate) fn check_unreachable_branch(
    ast: &LoweredAst,
    predicate: rigor_parse::NodeId,
    then_body: &[rigor_parse::NodeId],
    else_body: &[rigor_parse::NodeId],
    is_unless: bool,
) -> Option<Diagnostic> {
    let truthy = literal_predicate_truthy(ast, predicate)?;

    // Which branch is dead, accounting for the keyword. For `if`: truthy ⇒ else
    // dead, falsey ⇒ then dead. `unless` inverts (truthy ⇒ then dead, falsey ⇒
    // else dead). `then_dead == truthy` for `unless`, `!truthy` for `if`.
    let then_dead = if is_unless { truthy } else { !truthy };

    // Resolve the dead branch's anchor span. A then-branch is a `Vec` of
    // statements — anchor first-statement-start to last-statement-end (the
    // reference's StatementsNode span). An else-branch is a single lowered node
    // whose span already starts at the `else` keyword. Empty/absent ⇒ DECLINE.
    let span = if then_dead {
        let first = *then_body.first()?;
        let last = *then_body.last()?;
        (ast.get(first).span().0, ast.get(last).span().1)
    } else {
        let dead = *else_body.first()?;
        let s = ast.get(dead).span();
        (s.0, s.1)
    };

    // Byte-exact polarity word (verified against the oracle):
    //   "unreachable branch: literal predicate is always <truthy|falsey>".
    let polarity = if truthy { "truthy" } else { "falsey" };

    let severity = catalog(FLOW_UNREACHABLE_BRANCH)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Warning);

    Some(Diagnostic {
        rule_id: FLOW_UNREACHABLE_BRANCH,
        start_offset: span.0,
        end_offset: span.1,
        message: format!("unreachable branch: literal predicate is always {polarity}"),
        severity,
        source_family: "builtin",
        receiver_type: None,
        method_name: None,
    })
}

/// Apply `flow.always-raises` to a single call with a receiver — a provable
/// Integer `ZeroDivisionError` (the reference's `integer_zero_division?`).
///
/// Zero-false-positive gate (ADR-0023), mirroring the reference exactly. Fire
/// iff ALL hold:
///   1. the method is one of [`INTEGER_RAISING_OPERATORS`] (`/ % div modulo
///      divmod`),
///   2. NO block is attached (a block changes dispatch — decline),
///   3. exactly ONE positional argument is present (the divisor),
///   4. the receiver types to a provably Integer-rooted type — a
///      `Constant[Integer]`, an `IntegerRange`, or `Nominal[Integer]` with no
///      type args (the reference's `integer_rooted_for_diagnostic?`), AND
///   5. that one argument types to a constant Integer `0`
///      (`Constant[Int(0)]`).
///
/// Any other case DECLINES (returns `None`): a Float receiver (`5.0 / 0` —
/// Float division by zero is `Infinity`, not an error), a Float / non-zero /
/// non-constant divisor (`5 / 0.0`, `5 / 2`, `x / y`), a Dynamic/unknown
/// receiver, a block-bearing call, or a multi-arg call. This is the error-
/// severity zero-FP keystone: an FP here would be an ERROR on correct code.
// too_many_arguments: a rule-check fn threading the full typing context (ast, receiver,
// args, span, env, typer, interner, index); bundling into a struct would obscure the call sites.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_always_raises(
    ast: &LoweredAst,
    receiver: rigor_parse::NodeId,
    method: &str,
    args: &[rigor_parse::NodeId],
    has_block: bool,
    message_span: (usize, usize),
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
) -> Option<Diagnostic> {
    // (1) op set, (2) no block, (3) exactly one positional arg.
    if !INTEGER_RAISING_OPERATORS.contains(&method) {
        return None;
    }
    if has_block {
        return None;
    }
    let [arg] = args else {
        return None; // not exactly one positional arg ⇒ decline.
    };

    // (4) receiver provably Integer-rooted — mirrors the reference's
    // `integer_rooted_for_diagnostic?` (Constant<Integer> | IntegerRange |
    // Nominal[Integer] with no type args). Any other carrier (Float, Dynamic,
    // unknown, a generic Integer subtype application) ⇒ decline.
    let recv_ty = typer.type_of(ast, receiver, env, interner);
    if !is_integer_rooted(interner, index, recv_ty) {
        return None;
    }

    // (5) the divisor types to a constant Integer zero — `Constant[Int(0)]`.
    // A Float `0.0`, a non-zero constant, or any non-constant ⇒ decline.
    let arg_ty = typer.type_of(ast, *arg, env, interner);
    if !matches!(interner.get(arg_ty), Type::Constant(Scalar::Int(0))) {
        return None;
    }

    let message =
        format!("always raises ZeroDivisionError: `{method}' by zero on Integer receiver");
    let severity = catalog(FLOW_ALWAYS_RAISES)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Error);

    Some(Diagnostic {
        rule_id: FLOW_ALWAYS_RAISES,
        start_offset: message_span.0,
        end_offset: message_span.1,
        message,
        severity,
        source_family: "builtin",
        // Not a dispatch-typo rule; the receiver render / method fields are
        // carried for parity with the other call-family diagnostics.
        receiver_type: Some("Integer".to_string()),
        method_name: Some(method.to_string()),
    })
}

/// Whether `ty` is provably Integer-rooted for `flow.always-raises` — the
/// reference's `integer_rooted_for_diagnostic?`: a `Constant` pinned to an
/// Integer value, any `IntegerRange`, or `Nominal[Integer]` with NO type args.
/// Everything else (Float, Dynamic, unknown, applied generics) is NOT
/// Integer-rooted ⇒ the caller declines.
fn is_integer_rooted(interner: &Interner, index: &CoreIndex, ty: rigor_types::TypeId) -> bool {
    match interner.get(ty) {
        // A value-pinned Integer literal (`Constant[Int(5)]`).
        Type::Constant(Scalar::Int(_)) => true,
        // Any bounded Integer range is Integer-rooted (the reference fires on
        // `Type::IntegerRange` unconditionally).
        Type::IntegerRange { .. } => true,
        // `Nominal[Integer]` with NO type args — resolve the class name through
        // the core index (the same surface `class_name_of` uses), so this stays
        // robust to the class id's interning.
        Type::Nominal { class, args } => {
            args.is_empty() && index.class_name_for_id(*class) == Some("Integer")
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// flow.dead-assignment (ADR-0030) — pure AST/structural, no typer/index
// ---------------------------------------------------------------------------
//
// Faithful port of `DeadAssignmentCollector` (the reference firing logic) +
// `build_dead_assignment_diagnostic` (the message/severity/name-loc). For one
// method body:
//   1. Gather READ names `R`: every `LocalVariableRead.name`, PLUS every
//      `LocalVariableOpWrite.name` (an op-write reads-then-writes its target —
//      reference `reading_assignment?`), anywhere in the body subtree INCLUDING
//      blocks and string interpolation. Reads do NOT stop at nested defs for the
//      reference (`gather_read_names` has no def barrier) — but a write does, and
//      since we only ever fire on a write found OUTSIDE a nested def, and a name
//      read only inside a nested def cannot suppress an OUTER write that the
//      nested def can't see... we mirror the reference precisely: reads are
//      gathered with NO def barrier (so an inner-def read of an outer local
//      counts as a read — closure capture), writes ARE gathered with a def
//      barrier.
//   2. Gather WRITE candidates `W`: every plain `LocalVariableWrite`, WITHOUT
//      descending into a nested `Definition`/`ClassDef`/`ModuleDef`. Op-writes
//      and multi-writes (lowered to `Other`) are never candidates.
//   3. Trailing statement: the last node of the body list, descending through a
//      `BeginRescue` wrapper's last statement (the reference's
//      `trailing_statement`, which unwraps `StatementsNode`/`BeginNode`).
//   4. Fire iff the write is NOT the trailing statement, its name does NOT start
//      with `_`, and its name is NOT in `R`.

/// Collect every `flow.dead-assignment` diagnostic for one named method body.
///
/// ## Why reads/writes are gathered by SPAN, not structural recursion
///
/// The reference's `gather_read_names`/`gather_write_nodes` recurse the real
/// Prism tree via `compact_child_nodes` — a complete parent->child link. The
/// rigor-rs owned arena is a *lossy* lowering: several Prism nodes (a `return`,
/// `super`, `yield`, a `*splat` arg, …) lower to `Node::Other` and DISCARD their
/// lowered children, orphaning any `LocalVariableRead` underneath. A structural
/// child-walk would miss those reads and FALSELY flag a write that the reference
/// sees as read (a confirmed FP class: `return [entries, policy]`,
/// `super(head: frozen_head)`, `[*rest.map { … }]`).
///
/// The faithful, orphan-proof equivalent: every read/write node STILL lands in
/// the flat arena (lowering is total — only the *link* is lost, not the node),
/// and its byte span lies within the enclosing `def`'s span. So we scan the arena
/// for reads/writes whose span is contained in this def's span. This is exactly
/// the reference's "any read anywhere in the def subtree" set, because the def
/// span delimits precisely that subtree.
///
/// * Reads have NO def barrier in the reference (a read of an outer local inside
///   a nested `def` is a closure capture and counts) — span-containment naturally
///   includes nested-def reads, matching that.
/// * Writes DO have a def barrier (a nested def's writes are its own unit) — so a
///   write is a candidate here only if it is NOT inside any nested
///   def/class/module span that itself sits within this def.
pub(crate) fn dead_assignments_in_def(
    ast: &LoweredAst,
    def_id: rigor_parse::NodeId,
    def_name: &str,
    body: &[rigor_parse::NodeId],
    def_span: rigor_parse::Span,
    param_span: Option<rigor_parse::Span>,
    out: &mut Vec<Diagnostic>,
) {
    // Spans of nested definition units WITHIN this def (the write barrier). A
    // nested def/class/module is one whose span is strictly inside `def_span`
    // (i.e. not this def itself). A write inside any of these belongs to that
    // inner unit, not this one.
    let nested_spans: Vec<rigor_parse::Span> = ast
        .iter()
        .filter_map(|(id, n)| {
            if id == def_id {
                return None;
            }
            match n {
                Node::Definition { span, .. }
                | Node::ClassDef { span, .. }
                | Node::ModuleDef { span, .. }
                    if span_within(*span, def_span) =>
                {
                    Some(*span)
                }
                _ => None,
            }
        })
        .collect();

    // (1) read names — every read/op-write target whose span is within this def
    // (no def barrier). Orphan-proof: the node is in the arena regardless of link.
    let mut reads: HashSet<String> = HashSet::new();
    // (2) write candidates — plain LocalVariableWrites within this def but NOT
    // inside a nested unit.
    let mut writes: Vec<rigor_parse::NodeId> = Vec::new();
    for (id, n) in ast.iter() {
        match n {
            Node::LocalVariableRead { name, span } if span_within(*span, def_span) => {
                reads.insert(name.clone());
            }
            Node::LocalVariableOpWrite { name, span, .. } if span_within(*span, def_span) => {
                // An op-write READS its target (reference `reading_assignment?`).
                reads.insert(name.clone());
            }
            // The PARAMETER LIST is excluded: the reference gathers writes from
            // `def_node.body` only, so a write in a default value (`def f(a, b =
            // (not_set = true))`) is not a candidate there. Parameter defaults
            // are lowered into the arena for the call rules, which is what puts
            // them inside `def_span` at all. Reads are NOT excluded — keeping the
            // extra read names only suppresses more, which stays inside the
            // reference's witness set.
            Node::LocalVariableWrite { span, .. }
                if span_within(*span, def_span)
                    && !param_span.is_some_and(|ps| span_within(*span, ps))
                    && !nested_spans.iter().any(|ns| span_within(*span, *ns)) =>
            {
                writes.push(id);
            }
            _ => {}
        }
    }

    // (3) trailing statement (implicit return — its write is intentional).
    let trailing = trailing_statement(ast, body);

    let severity = catalog(FLOW_DEAD_ASSIGNMENT)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Warning);

    // Emit in source order (writes were collected in arena/source order already).
    for wid in writes {
        let Node::LocalVariableWrite {
            name, name_span, ..
        } = ast.get(wid)
        else {
            continue;
        };
        // (4) the gate.
        if Some(wid) == trailing {
            continue;
        }
        if name.starts_with('_') {
            continue;
        }
        if reads.contains(name) {
            continue;
        }
        out.push(Diagnostic {
            rule_id: FLOW_DEAD_ASSIGNMENT,
            start_offset: name_span.0,
            end_offset: name_span.1,
            message: format!("local `{name}' assigned in `{def_name}' but never read"),
            severity,
            source_family: "builtin",
            receiver_type: None,
            method_name: None,
        });
    }
}

/// The trailing statement of a method body: the last id in `body`, descending
/// through a `BeginRescue` / `Statements` wrapper's last statement (mirrors the
/// reference's `trailing_statement`, which unwraps `StatementsNode`/`BeginNode`).
/// `None` for an empty body. A write that IS the trailing statement is an
/// implicit return and is skipped.
fn trailing_statement(ast: &LoweredAst, body: &[rigor_parse::NodeId]) -> Option<rigor_parse::NodeId> {
    let &last = body.last()?;
    descend_trailing(ast, last)
}

fn descend_trailing(ast: &LoweredAst, id: rigor_parse::NodeId) -> Option<rigor_parse::NodeId> {
    match ast.get(id) {
        // A `begin ... end` — its trailing node is the last statement of the
        // protected/rescue/else region, NOT the ensure tail: an `ensure` clause's
        // value is discarded (the reference treats the protected-body tail as the
        // implicit return even when an `ensure` follows it in `body`, where the
        // lowering appends the ensure statements).
        Node::BeginRescue {
            body, ensure_body, ..
        } => match body.iter().rev().find(|id| !ensure_body.contains(id)) {
            Some(&inner) => descend_trailing(ast, inner),
            None => Some(id),
        },
        // The lowered Statements wrapper — its last statement is the real
        // trailing node.
        Node::Statements { body, .. } => match body.last() {
            Some(&inner) => descend_trailing(ast, inner),
            None => Some(id),
        },
        // An explicit `return E` is NOT descended: the reference FIRES
        // `flow.dead-assignment` on `return (x = 5)` (the local binding is
        // pointless even though its value is returned — oracle-probed
        // 2026-07-10), so a write inside a return must NOT get the
        // implicit-return trailing-write skip.
        _ => Some(id),
    }
}

// ---------------------------------------------------------------------------
// flow.duplicate-hash-key (v0.3.0) — reference `DuplicateHashKeyCollector`
// ---------------------------------------------------------------------------

/// Emit `flow.duplicate-hash-key` for every LATER occurrence of a repeated
/// value-pinned literal key within one Hash literal (braced or bare kwargs). Walks
/// each `HashLit`'s precomputed `dup_keys` (source order); a `seen` map keyed by
/// the collision tag records the FIRST occurrence, and each later hit fires
/// pointing at the repeat, naming the first's line. The `seen` entry is NOT
/// updated on a hit, so with N≥2 duplicates every later occurrence references the
/// SAME original first occurrence (reference semantics). Each literal is its own
/// scope — nested literals never cross-compare (they are distinct arena nodes).
pub(crate) fn duplicate_hash_key_diagnostics(ast: &LoweredAst, out: &mut Vec<Diagnostic>) {
    for (_id, node) in ast.iter() {
        let Node::HashLit { dup_keys, .. } = node else {
            continue;
        };
        if dup_keys.len() < 2 {
            continue;
        }
        let mut seen: HashMap<&HashKeyTag, u32> = HashMap::new();
        for key in dup_keys {
            match seen.get(&key.tag) {
                Some(&first_line) => out.push(Diagnostic {
                    rule_id: FLOW_DUPLICATE_HASH_KEY,
                    start_offset: key.anchor.0,
                    end_offset: key.anchor.1,
                    message: format!(
                        "duplicate hash key `{}' in the same literal; this entry \
                         overwrites the value first set at line {first_line}",
                        key.label
                    ),
                    severity: Severity::Warning,
                    source_family: "builtin",
                    receiver_type: None,
                    method_name: None,
                }),
                None => {
                    seen.insert(&key.tag, key.line);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// flow.return-in-ensure (v0.3.0) — reference `ReturnInEnsureCollector`
// ---------------------------------------------------------------------------

/// Receiver-less calls whose attached BLOCK opens a new return frame: a `return`
/// inside their block exits the lambda / defined method, not the method whose
/// `ensure` is scanned. `proc` is deliberately ABSENT — `return` inside a `Proc`
/// block returns from the enclosing method, so it stays in scope. Reference
/// `FRAME_BARRIER_CALL_NAMES`.
const FRAME_BARRIER_CALL_NAMES: &[&str] = &["lambda", "define_method"];

/// Emit `flow.return-in-ensure` for every explicit `return` lexically inside an
/// `ensure` clause body. Dispatches on every `BeginRescue` carrying a non-empty
/// `ensure_body` and gathers returns from it with a frame-aware envelope.
pub(crate) fn return_in_ensure_diagnostics(ast: &LoweredAst, out: &mut Vec<Diagnostic>) {
    for (_id, node) in ast.iter() {
        let Node::BeginRescue { ensure_body, .. } = node else {
            continue;
        };
        for &stmt in ensure_body {
            gather_returns_in_ensure(ast, stmt, out);
        }
    }
}

/// Recursively collect offending `return`s under `id`, stopping at frame
/// barriers. Port of the reference `gather_returns` + `gather_returns_around_barrier_block`.
fn gather_returns_in_ensure(ast: &LoweredAst, id: NodeId, out: &mut Vec<Diagnostic>) {
    match ast.get(id) {
        Node::Return { values, span } => {
            out.push(Diagnostic {
                rule_id: FLOW_RETURN_IN_ENSURE,
                start_offset: span.0,
                end_offset: span.1,
                message: "`return' inside `ensure' discards the method's in-flight \
                          return value and swallows any in-flight exception"
                    .to_string(),
                severity: Severity::Warning,
                source_family: "builtin",
                receiver_type: None,
                method_name: None,
            });
            // The reference falls through to descend the return's children.
            for &v in values {
                gather_returns_in_ensure(ast, v, out);
            }
        }
        // A nested `def` / lambda opens a new return frame — a `return` below it
        // exits that inner frame, not the one whose `ensure` we scan.
        Node::Definition { .. } | Node::Lambda { .. } => {}
        // A nested `begin/ensure`: descend the protected/rescue/else statements
        // but NOT its own `ensure` clause — that inner ensure is scanned when its
        // OWN `BeginRescue` is dispatched, so descending here would double-count.
        // The ensure statements also live (duplicated) in `body`, so exclude them.
        Node::BeginRescue { body, ensure_body, .. } => {
            for &child in body {
                if !ensure_body.contains(&child) {
                    gather_returns_in_ensure(ast, child, out);
                }
            }
        }
        // A receiver-less `lambda`/`define_method` call with a block is a barrier:
        // its receiver + args stay in the current frame (and are descended), only
        // the block opens a new one. Every other call (incl. `proc`, plain blocks)
        // is fully descended.
        Node::Call { receiver, method, args, block_body, .. } => {
            let is_barrier = receiver.is_none()
                && FRAME_BARRIER_CALL_NAMES.contains(&method.as_str())
                && !block_body.is_empty();
            if let Some(r) = receiver {
                gather_returns_in_ensure(ast, *r, out);
            }
            for &a in args {
                gather_returns_in_ensure(ast, a, out);
            }
            if !is_barrier {
                for &b in block_body {
                    gather_returns_in_ensure(ast, b, out);
                }
            }
        }
        other => {
            for child in node_children(other) {
                gather_returns_in_ensure(ast, child, out);
            }
        }
    }
}

/// The child node ids of a node (for the generic descent in the return-in-ensure
/// walk). Covers every variant carrying child ids; the barrier/special variants
/// (`Call`/`BeginRescue`/`Return`/`Definition`/`Lambda`) are handled by the caller
/// and never routed here.
fn node_children(node: &Node) -> Vec<NodeId> {
    let mut out = Vec::new();
    match node {
        Node::Program { body, .. }
        | Node::Statements { body, .. }
        | Node::ClassDef { body, .. }
        | Node::ModuleDef { body, .. }
        | Node::Definition { body, .. }
        | Node::Lambda { body, .. }
        | Node::BeginRescue { body, .. } => out.extend(body.iter().copied()),
        Node::LocalVariableWrite { value, .. }
        | Node::LocalVariableOpWrite { value, .. }
        | Node::VariableWrite { value, .. }
        | Node::InstanceVariableWrite { value, .. }
        | Node::ConstantWrite { value, .. } => out.push(*value),
        // A multi-write descends into its RHS *and* the expressions embedded in
        // non-local targets (`obj.attr, b = (return 1), 2` — the `return` can
        // hide in either). Correct descent regardless of whether a probe
        // currently reaches it.
        Node::MultiWrite { value, target_exprs, .. } => {
            out.push(*value);
            out.extend(target_exprs.iter().copied());
        }
        Node::InterpolatedString { parts, .. } | Node::InterpolatedSymbol { parts, .. } => {
            out.extend(parts.iter().copied())
        }
        Node::Call { receiver, args, block_body, .. } => {
            if let Some(r) = receiver {
                out.push(*r);
            }
            out.extend(args.iter().copied());
            out.extend(block_body.iter().copied());
        }
        Node::If { predicate, then_body, else_body, .. } => {
            out.push(*predicate);
            out.extend(then_body.iter().copied());
            out.extend(else_body.iter().copied());
        }
        Node::Case { predicate, branches, else_body, .. } => {
            if let Some(p) = predicate {
                out.push(*p);
            }
            out.extend(branches.iter().copied());
            out.extend(else_body.iter().copied());
        }
        // A `when` clause's children are its conditions then its body — the
        // same id set (and order) the pre-split `BeginRescue` carrier held
        // concatenated in `body`.
        Node::When { conditions, body, .. } => {
            out.extend(conditions.iter().copied());
            out.extend(body.iter().copied());
        }
        Node::Loop { predicate, body, .. } => {
            if let Some(p) = predicate {
                out.push(*p);
            }
            out.extend(body.iter().copied());
        }
        Node::Logical { left, right, .. } => {
            out.push(*left);
            out.push(*right);
        }
        Node::ArrayLit { elements, .. } | Node::HashLit { elements, .. } => {
            out.extend(elements.iter().copied());
        }
        Node::Return { values, .. } => out.extend(values.iter().copied()),
        _ => {}
    }
    out
}
