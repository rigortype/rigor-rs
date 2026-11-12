//! The regex match globals (`$~` and its `$N` captures) across an early-exit
//! guard, for the nil-receiver walker's `Regexp.last_match` source
//! (rigor-rs#449).
//!
//! The reference keeps `$~` / `$1..$N` as scope globals. A `=~` / `!~`
//! predicate's match edge binds `$~` to `MatchData` and every UNCONDITIONAL
//! capture group's `$N` to `String` (`Narrowing#regex_match_predicate_scopes`),
//! and `RegexpFolding.fold_last_match` reads them: `Regexp.last_match` is
//! `MatchData`, and `Regexp.last_match(N)` is `String` when `$N` is bound,
//! instead of RBS's `MatchData?` / `String?`. An early-exit guard
//! (`return unless s =~ /(x)/`, `s =~ /(x)/ or return`, `return if s !~ re`)
//! leaves the statements after it on the match edge, so the read there is
//! non-nil. A later statement that may run a match forgets the globals
//! (`StatementEvaluator#rebinds_match_globals?`).
//!
//! The walker keeps that state in reserved [`TypeEnv`] keys (no local or
//! global name starts with `\u{1f}`), so it follows the walker's scopes: a
//! `def` body starts with none.
//!
//! **Direction.** A recorded match only silences `call.possible-nil-receiver`
//! on a `Regexp.last_match` source. Forgetting is the pre-#449 behaviour, so
//! the port forgets on every statement it cannot prove match-free. That set
//! is wider than the reference's, which costs only coverage. It records a
//! match only where the reference binds one: a literal-pattern `=~` / `!~` on
//! a String / Symbol / nil / untyped receiver, or on a regexp literal.

use std::collections::BTreeSet;

use rigor_parse::{LoweredAst, Node, NodeId, StatementsKind};
use rigor_types::{Interner, Scalar, Type};

use crate::{TypeEnv, Typer};

/// The reserved key for a proven `$~` (`Nominal[MatchData]`).
const MATCH_KEY: &str = "\u{1f}$~";

/// The reserved key for a proven unconditional `$N` (`String`).
fn group_key(n: i64) -> String {
    format!("\u{1f}${n}")
}

/// `MatchRebinding::Calls::ALWAYS_MATCHING` (`match_rebinding/calls.rb`),
/// plus the calls that run code by name or run a block parameter. A call
/// with one of these names may rebind the frame's `$~`, so the walker forgets
/// the match after it. The reference forgets on a subset of the extra names;
/// the rest only cost coverage.
const MAY_MATCH_CALLS: &[&str] = &[
    "=~", "!~", "match", "sub", "sub!", "gsub", "gsub!", "scan", "~", "===", "call", "yield",
    "send", "public_send", "__send__", "eval", "instance_eval", "instance_exec", "class_eval",
    "module_eval", "class_exec", "module_exec", "binding", "method",
];

/// `Calls::LOOKUPS` (and `[]=`, read by its index): these rebind `$~` only
/// with a Regexp argument, so they keep the match when every argument is a
/// non-Regexp literal (`Calls.forgets_by_name?`). `split` with no separator
/// splits on `$;`, which may hold a Regexp, so it forgets.
const LOOKUP_CALLS: &[&str] = &[
    "[]", "slice", "slice!", "index", "rindex", "byteindex", "byterindex", "partition",
    "rpartition", "split", "start_with?", "any?", "all?", "none?", "one?", "[]=",
];

/// Drop the recorded match: `Scope#forget_match_globals`.
pub(crate) fn forget(tenv: &mut TypeEnv) {
    tenv.retain(|k, _| !k.starts_with("\u{1f}$"));
}

/// `$~` is proven a `MatchData` (`RegexpFolding#proven_match?`).
pub(crate) fn proven(tenv: &TypeEnv) -> bool {
    tenv.contains_key(MATCH_KEY)
}


fn record(tenv: &mut TypeEnv, groups: &BTreeSet<i64>, interner: &mut Interner) {
    forget(tenv);
    let u = interner.untyped();
    tenv.insert(MATCH_KEY.to_string(), u);
    for &n in groups {
        tenv.insert(group_key(n), u);
    }
}

impl<'i> Typer<'i> {
    /// `RegexpFolding.fold_last_match_group`: `Regexp.last_match(arg)` is
    /// `String` when `$~` is proven, `arg` types to a positive Integer
    /// constant N and `$N` is bound, meaning group N is unconditional.
    pub(crate) fn last_match_group_proven(
        &self,
        ast: &LoweredAst,
        arg: NodeId,
        tenv: &TypeEnv,
        interner: &mut Interner,
    ) -> bool {
        if !proven(tenv) {
            return false;
        }
        let ty = self.type_of(ast, arg, tenv, interner);
        matches!(interner.get(ty), Type::Constant(Scalar::Int(n))
            if *n > 0 && tenv.contains_key(&group_key(*n)))
    }

    /// Update the match state after statement `id` ran.
    ///
    /// An early-exit guard whose fall-through is the match edge of a
    /// recognised predicate records that match. Any other statement keeps the
    /// state only when it cannot run a match ([`match_free`]).
    pub(crate) fn match_globals_after_stmt(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &mut TypeEnv,
        interner: &mut Interner,
    ) {
        if let Some(groups) = self.guard_match_edge(ast, id, tenv, interner) {
            record(tenv, &groups, interner);
        } else if !match_free(ast, id) {
            forget(tenv);
        }
    }

    /// The unconditional groups of the match an early-exit guard leaves its
    /// fall-through on, or `None` when the fall-through is not a proven match.
    ///
    /// - `return unless P` / `return if P` (an `if`/`unless` whose one arm
    ///   exits and whose other arm is empty): the fall-through is `P`'s
    ///   truthy edge under `unless`, its falsey edge under `if`.
    /// - `P or return` / `P and return`: truthy under `or`, falsey under `and`.
    fn guard_match_edge(
        &self,
        ast: &LoweredAst,
        id: NodeId,
        tenv: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<BTreeSet<i64>> {
        let (predicate, falls_through_truthy) = match ast.get(id) {
            Node::If { predicate, then_body, else_body, is_unless, .. } => {
                let mut seen = Vec::new();
                if else_body.is_empty() && self.arm_exits(ast, then_body, &mut seen) {
                    (*predicate, *is_unless)
                } else if then_body.is_empty() && self.arm_exits(ast, else_body, &mut seen) {
                    (*predicate, !*is_unless)
                } else {
                    return None;
                }
            }
            Node::Logical { left, right, is_and, .. } => {
                if !self.stmt_exits(ast, *right, &mut Vec::new()) {
                    return None;
                }
                (*left, !*is_and)
            }
            _ => return None,
        };
        let (match_is_truthy, groups) = self.regex_match_edge(ast, predicate, tenv, interner)?;
        (match_is_truthy == falls_through_truthy).then_some(groups)
    }

    /// `Narrowing#gated_regex_match_edges`, reduced to what the guard needs:
    /// `(match edge is the truthy edge, unconditional groups)` for a
    /// `recv =~ pattern` (truthy) or `recv !~ pattern` (falsey) predicate
    /// whose one regexp-literal operand is not `//x`, on a receiver the
    /// reference keeps the match edge for.
    fn regex_match_edge(
        &self,
        ast: &LoweredAst,
        pred: NodeId,
        tenv: &TypeEnv,
        interner: &mut Interner,
    ) -> Option<(bool, BTreeSet<i64>)> {
        let pred = unwrap_parens(ast, pred);
        let Node::Call {
            receiver: Some(recv),
            method,
            args,
            block_body,
            block_span: None,
            safe_nav: false,
            ..
        } = ast.get(pred)
        else {
            return None;
        };
        let match_is_truthy = match method.as_str() {
            "=~" => true,
            "!~" => false,
            _ => return None,
        };
        let [arg] = args.as_slice() else { return None };
        if !block_body.is_empty() {
            return None;
        }
        // `regex_match_pattern`: exactly one operand is a regexp literal.
        let pattern = match (ast.get(*recv), ast.get(*arg)) {
            (Node::RegexpLit { .. }, Node::RegexpLit { .. }) => return None,
            (Node::RegexpLit { source, extended, .. }, _)
            | (_, Node::RegexpLit { source, extended, .. }) => {
                if *extended {
                    return None;
                }
                source
            }
            _ => return None,
        };
        // `match_receiver_edges`: a regexp receiver keeps both edges; any
        // other must be String / Symbol / nil / untyped for `!~`'s match edge
        // (`not_match_operator_edges`). The port applies that gate to `=~`
        // too, whose reference gate is looser; that only costs coverage.
        if !matches!(ast.get(*recv), Node::RegexpLit { .. }) {
            let rty = self.type_of(ast, *recv, tenv, interner);
            if !self.match_receiver_kind_ok(rty, interner) {
                return None;
            }
        }
        Some((match_is_truthy, unconditional_capture_groups(pattern)))
    }

    /// Every member of `ty` is a String, a Symbol, nil, or untyped.
    fn match_receiver_kind_ok(&self, ty: rigor_types::TypeId, interner: &Interner) -> bool {
        match interner.get(ty) {
            Type::Union(members) => {
                let members = members.clone();
                members.iter().all(|&m| self.match_receiver_kind_ok(m, interner))
            }
            Type::Dynamic(f) => matches!(interner.get(*f), Type::Top),
            Type::Constant(Scalar::Str(_) | Scalar::Sym(_) | Scalar::Nil) => true,
            Type::Nominal { .. } => matches!(
                self.index.class_name_of(interner, ty),
                Some("String" | "Symbol" | "NilClass")
            ),
            _ => false,
        }
    }
}

/// `id` cannot run a regex match in this frame: it is built only from
/// local reads and writes, literals, constant reads, own-frame bodies
/// (`def`, `class`, `module`), `if`s and `&&`/`||` over such parts, and
/// block-less calls that `Calls.forgets_by_name?` clears: a name outside
/// [`MAY_MATCH_CALLS`], and a [`LOOKUP_CALLS`] name only on non-Regexp
/// literal arguments. Anything else answers `false` (forget), the
/// pre-#449 behaviour.
fn match_free(ast: &LoweredAst, id: NodeId) -> bool {
    let all = |ids: &[NodeId]| ids.iter().all(|&i| match_free(ast, i));
    match ast.get(id) {
        Node::LocalVariableRead { .. }
        | Node::VariableRead { .. }
        | Node::ConstantRead { .. }
        | Node::SelfExpr { .. }
        | Node::StringLit { .. }
        | Node::IntegerLit { .. }
        | Node::FloatLit { .. }
        | Node::RationalLit { .. }
        | Node::ImaginaryLit { .. }
        | Node::SymbolLit { .. }
        | Node::NilLit { .. }
        | Node::TrueLit { .. }
        | Node::FalseLit { .. }
        | Node::RegexpLit { .. } => true,
        // `MatchRebinding::OWN_FRAME_NODES`: the body runs in a frame of
        // its own.
        Node::Definition { .. } | Node::ClassDef { .. } | Node::ModuleDef { .. } => true,
        Node::LocalVariableWrite { value, .. } => match_free(ast, *value),
        // An exit leaves the fall-through unchanged.
        Node::Return { values, .. } => all(values),
        Node::Other { jump: Some(_), .. } => true,
        Node::InterpolatedString { parts, .. } | Node::InterpolatedSymbol { parts, .. } => {
            all(parts)
        }
        Node::ArrayLit { elements, .. } => all(elements),
        Node::Statements { body, kind: StatementsKind::Sequence, .. } => all(body),
        Node::If { predicate, then_body, else_body, .. } => {
            match_free(ast, *predicate) && all(then_body) && all(else_body)
        }
        Node::Logical { left, right, .. } => {
            match_free(ast, *left) && match_free(ast, *right)
        }
        Node::Call {
            receiver,
            method,
            args,
            block_body,
            block_span: None,
            ..
        } => {
            let name = method.as_str();
            let args_ok = if LOOKUP_CALLS.contains(&name) {
                let checked = if name == "[]=" {
                    &args[..args.len().saturating_sub(1)]
                } else {
                    &args[..]
                };
                !(name == "split" && args.is_empty())
                    && checked.iter().all(|&a| non_regexp_literal(ast.get(a)))
                    && all(args)
            } else {
                all(args)
            };
            block_body.is_empty()
                && !MAY_MATCH_CALLS.contains(&name)
                && args_ok
                && receiver.is_none_or(|r| match_free(ast, r))
        }
        _ => false,
    }
}

/// `Operands.keep_literal?`, narrowed: a literal that is not a Regexp.
fn non_regexp_literal(node: &Node) -> bool {
    matches!(
        node,
        Node::StringLit { .. }
            | Node::IntegerLit { .. }
            | Node::FloatLit { .. }
            | Node::SymbolLit { .. }
            | Node::NilLit { .. }
            | Node::TrueLit { .. }
            | Node::FalseLit { .. }
    )
}

/// A parenthesised single expression `(P)` is `P`.
fn unwrap_parens(ast: &LoweredAst, mut id: NodeId) -> NodeId {
    while let Node::Statements { body, kind: StatementsKind::Sequence, .. } = ast.get(id) {
        let [only] = body.as_slice() else { break };
        id = *only;
    }
    id
}

/// `Narrowing#unconditional_capture_groups`: the 1-based indices of the
/// capture groups present on every successful match of `source` — no
/// optional quantifier (`?`, `*`, `{0…}`) on the group or an ancestor, and
/// not inside an alternation branch. Once the pattern has a named group only
/// named groups capture. A char scan, ported as the reference writes it.
pub(crate) fn unconditional_capture_groups(source: &str) -> BTreeSet<i64> {
    let src: Vec<char> = source.chars().collect();
    let named = named_groups(&src);
    let mut unconditional: BTreeSet<i64> = BTreeSet::new();
    // One frame per open group, plus a virtual root:
    // (group index, descendant indices, alternated).
    let mut stack: Vec<(Option<i64>, Vec<i64>, bool)> = vec![(None, Vec::new(), false)];
    let mut group_index = 0;
    each_structural_char(&src, |pos, chr| match chr {
        '(' => {
            let mut idx = None;
            if capturing_group(&src, pos, named) {
                group_index += 1;
                idx = Some(group_index);
                unconditional.insert(group_index);
            }
            stack.push((idx, Vec::new(), false));
        }
        ')' => {
            let optional = next_quantifier_optional(&src, pos + 1);
            let frame = stack.pop();
            finalize_frame(&mut unconditional, &mut stack, frame, optional);
        }
        '|' => {
            if let Some(top) = stack.last_mut() {
                top.2 = true;
            }
        }
        _ => {}
    });
    let root = stack.pop();
    finalize_frame(&mut unconditional, &mut stack, root, false);
    unconditional
}

fn finalize_frame(
    unconditional: &mut BTreeSet<i64>,
    stack: &mut [(Option<i64>, Vec<i64>, bool)],
    frame: Option<(Option<i64>, Vec<i64>, bool)>,
    optional: bool,
) {
    let Some((idx, descendants, alternated)) = frame else { return };
    let mut subtree = descendants.clone();
    subtree.extend(idx);
    if optional {
        for i in &subtree {
            unconditional.remove(i);
        }
    }
    if alternated {
        for i in &descendants {
            unconditional.remove(i);
        }
    }
    if let Some(parent) = stack.last_mut() {
        parent.1.extend(subtree);
    }
}

/// Each position outside a backslash escape and a character class.
fn each_structural_char(src: &[char], mut f: impl FnMut(usize, char)) {
    let mut pos = 0;
    while pos < src.len() {
        let chr = src[pos];
        if chr == '\\' {
            pos += 2;
            continue;
        }
        if chr == '[' {
            pos = skip_char_class(src, pos) + 1;
            continue;
        }
        f(pos, chr);
        pos += 1;
    }
}

fn named_groups(src: &[char]) -> bool {
    let mut found = false;
    each_structural_char(src, |pos, chr| {
        if chr == '(' && named_group(src, pos) {
            found = true;
        }
    });
    found
}

fn next_quantifier_optional(src: &[char], pos: usize) -> bool {
    match src.get(pos) {
        Some('?' | '*') => true,
        Some('{') => src.get(pos + 1) == Some(&'0'),
        _ => false,
    }
}

fn capturing_group(src: &[char], pos: usize, named: bool) -> bool {
    if src.get(pos + 1) != Some(&'?') {
        return !named;
    }
    named_group(src, pos)
}

/// `(?<name>…)` / `(?'name'…)`; not lookbehind `(?<=…)` / `(?<!…)`.
fn named_group(src: &[char], pos: usize) -> bool {
    if src.get(pos + 1) != Some(&'?') {
        return false;
    }
    if src.get(pos + 2) == Some(&'\'') {
        return true;
    }
    src.get(pos + 2) == Some(&'<')
        && src.get(pos + 3) != Some(&'=')
        && src.get(pos + 3) != Some(&'!')
}

fn skip_char_class(src: &[char], start: usize) -> usize {
    let mut pos = start + 1;
    if src.get(pos) == Some(&'^') {
        pos += 1;
    }
    if src.get(pos) == Some(&']') {
        pos += 1;
    }
    while pos < src.len() {
        if src[pos] == ']' {
            return pos;
        }
        pos += if src[pos] == '\\' { 2 } else { 1 };
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::unconditional_capture_groups as groups;

    fn set(v: &[i64]) -> std::collections::BTreeSet<i64> {
        v.iter().copied().collect()
    }

    /// The reference's `unconditional_capture_groups` on its documented
    /// shapes: optional quantifiers, alternation, non-capturing and
    /// lookaround groups, character classes and named-group numbering.
    #[test]
    fn unconditional_capture_groups_match_the_reference_walker() {
        assert_eq!(groups(r"(\w+)"), set(&[1]));
        assert_eq!(groups(r"(\w+)?"), set(&[]));
        assert_eq!(groups(r"(a)*(b)+(c){0,2}(d){1,2}"), set(&[2, 4]));
        assert_eq!(groups(r"(a)|(b)"), set(&[]));
        assert_eq!(groups(r"((a)|(b))"), set(&[1]));
        assert_eq!(groups(r"((\w)+)?(z)"), set(&[3]));
        assert_eq!(groups(r"(?:a)(b)(?=c)(?<=d)(?!e)(?<!f)"), set(&[1]));
        assert_eq!(groups(r"[(](\w+)\("), set(&[1]));
        assert_eq!(groups(r"(?<n>a)(b)"), set(&[1]));
        assert_eq!(groups(r"(?'n'a)(?<m>b)?"), set(&[1]));
    }
}
