//! rigor-rs#405 — a class guard on a local invalidates the indexed
//! narrowings (`h[k] ||= v` records) rooted at it.
//!
//! The reference narrows a guard's subject local through `Scope#with_local`
//! (`narrowing.rb` `class_slot_scopes` → `narrow_guarded_slot` →
//! `narrow_slot`), and `with_local` is a rebind: `bind_local` drops every
//! `(local, name, key)` indexed narrowing through
//! `drop_indexed_narrowings_for` (`scope.rb`). It does so on BOTH edges and
//! whatever the narrowed type is — an `is_a?(Hash)` guard on a `Hash`
//! binding still rebinds it — and `Scope#join` keeps only the records both
//! sides hold (`join_bindings`), so the drop also survives every join the
//! guard's edges reach: the arms, the code after the construct, and the
//! right operand of a `&&` / `||`. The port's edge engines leave a binding
//! whose edge is `Keep` untouched, records included, so a read of `h[k]`
//! inside or after the guard served the stale record (`h[:a] ||= "s"; if
//! h.is_a?(Hash); h[:a].frobnicate` fired `for "s" | 1`).
//!
//! Which guards rebind (probed on the pinned reference):
//! - `L.is_a?(C)` / `kind_of?` / `instance_of?` and `C === L`, `C` a static
//!   constant spelling — a relative name drops even when nothing declares it
//!   (`resolve_class_name_lexically` falls back to the bare name), a rooted
//!   `::C` only when `C` is known (`rooted_class_predicate_name`). A
//!   non-constant operand (`is_a?(k)`) narrows nothing and keeps the record.
//!   `Regexp === L` / range `=== L` rebind too (`analyse_case_equality_receiver`).
//!   The atom counts wherever the predicate analyser reaches it: through
//!   `!`, `&&` / `||` and a parenthesised last statement.
//! - the predicate of `if` / `unless` / `elsif` / ternary / modifier and of
//!   `while` / `until`; the left operand of any `&&` / `||` (its right
//!   operand runs on the left's edge); a subjectless `case`'s `when` tests.
//! - `case L` with any `when` clause (`case_when_scopes` rebinds the
//!   subject's slot for every clause, even `when 1`), and `case L` / `in C`
//!   for a bare class pattern only (`in {a: Integer}` keeps the record).
//!
//! The port applies this as a per-site overlay on the check env: a site
//! after the guard's narrowing point, in the guard's `def` / closure and
//! before the local's next rebind, reads every record a slot write BEFORE
//! the guard may have left as `untyped`. Untyped, not removed: the record's
//! replacement on the oracle is the binding read, which the port's binding
//! need not match, and an untyped read is the decline side. A record only
//! slot writes AFTER the guard produce (`h = {}; if h.is_a?(Hash); h[:a] ||=
//! "t"; h[:a].frobnicate` fires `for "t"` on both) is computed from the
//! post-guard scope already and is kept.

use std::borrow::Cow;

use rigor_parse::{LoweredAst, Node, NodeId, Span};
use rigor_types::Interner;

use crate::flow_writes::{indexed_narrowing_key, IndexedFlow};
use crate::{TypeEnv, Typer};

/// One local a guard rebinds, with the extent its record drop covers.
pub(crate) struct RecordGuard {
    /// The construct's own arms a site reads the guard's edge in, wherever
    /// they sit in the source — a modifier `x if h.is_a?(C)` / `x while …`
    /// writes its arm BEFORE the predicate.
    regions: Vec<Span>,
    /// The offset from which every later site reads the joined edges: the
    /// construct's end, or the case subject / bare class pattern's end.
    after: usize,
    /// The innermost `def` around the guard — a site outside it reads a
    /// different local scope.
    def: Option<Span>,
    /// The innermost block / `->` body around the guard: its edges narrow
    /// that closure's scope only.
    closure: Option<Span>,
    /// The first rebind of the local after the guard atom, in the guard's
    /// own scope; a site past it reads the rebind's (record-free) binding.
    next: Option<usize>,
    /// The record keys a slot write before the guard atom may have left.
    keys: Vec<String>,
}

/// `(local, arm regions, after, atom end)` — one guard before the scope
/// census [`Typer::collect_record_guards`] completes it with.
type RawGuard = (String, Vec<Span>, usize, usize);

impl<'i> Typer<'i> {
    /// Every [`RecordGuard`] in the file. Empty when the file holds no
    /// `h[k] ||= v` record at all — the common case pays one scan.
    pub(crate) fn collect_record_guards(
        &self,
        ast: &LoweredAst,
        indexed: &IndexedFlow,
    ) -> Vec<RecordGuard> {
        if indexed.slot_writes.is_empty() {
            return Vec::new();
        }
        let mut raw: Vec<RawGuard> = Vec::new();
        for (_, n) in ast.iter() {
            match n {
                Node::If { predicate, then_body, else_body, span, .. } => {
                    let atom_end = ast.get(*predicate).span().1;
                    let regions: Vec<Span> = [then_body, else_body]
                        .into_iter()
                        .filter_map(|b| crate::dead::body_hull(ast, b))
                        .collect();
                    for name in self.guarded_locals(ast, *predicate) {
                        raw.push((name, regions.clone(), span.1, atom_end));
                    }
                }
                Node::Loop { predicate: Some(p), body, span, .. } => {
                    let atom_end = ast.get(*p).span().1;
                    let regions: Vec<Span> = crate::dead::body_hull(ast, body).into_iter().collect();
                    for name in self.guarded_locals(ast, *p) {
                        raw.push((name, regions.clone(), span.1, atom_end));
                    }
                }
                Node::Logical { left, right, span, .. } => {
                    let atom_end = ast.get(*left).span().1;
                    let regions = vec![ast.get(*right).span()];
                    for name in self.guarded_locals(ast, *left) {
                        raw.push((name, regions.clone(), span.1, atom_end));
                    }
                }
                Node::Case { predicate, branches, .. } => {
                    let subject = predicate.and_then(|p| match ast.get(p) {
                        Node::LocalVariableRead { name, span } => Some((name.clone(), span.1)),
                        _ => None,
                    });
                    match subject {
                        Some((name, subject_end)) => {
                            let from = branches.iter().find_map(|&b| match ast.get(b) {
                                Node::When { .. } => Some(subject_end),
                                // A `case`/`in` branch is a `BeginRescue`
                                // carrier whose first body node is the
                                // pattern; a bare class pattern (`in C`,
                                // `in C => x`) lowers to the constant.
                                Node::BeginRescue { body, .. } => body
                                    .first()
                                    .filter(|&&p| self.static_guard_class(ast, p))
                                    .map(|&p| ast.get(p).span().1),
                                _ => None,
                            });
                            if let Some(from) = from {
                                raw.push((name, Vec::new(), from, from));
                            }
                        }
                        None if predicate.is_none() => {
                            for &b in branches {
                                let Node::When { conditions, .. } = ast.get(b) else {
                                    continue;
                                };
                                for &c in conditions {
                                    let from = ast.get(c).span().1;
                                    for name in self.guarded_locals(ast, c) {
                                        raw.push((name, Vec::new(), from, from));
                                    }
                                }
                            }
                        }
                        None => {}
                    }
                }
                _ => {}
            }
        }
        if raw.is_empty() {
            return Vec::new();
        }
        let defs = crate::method_body_spans(ast);
        let closures: Vec<Span> =
            ast.iter().filter_map(|(_, n)| crate::loop_edges::closure_extent(n)).collect();
        let innermost = |spans: &[Span], at: usize| {
            spans
                .iter()
                .filter(|s| s.0 <= at && at <= s.1)
                .min_by_key(|s| s.1 - s.0)
                .copied()
        };
        // A local's real rebinds — writes, `op=`, multi-write and `for`
        // targets. Content mutations (`h[k] ||= v`, `h << x`) are not:
        // they keep the binding, so the guard's drop still covers them.
        let rebinds: Vec<(usize, String)> = ast
            .iter()
            .flat_map(|(_, n)| match n {
                Node::LocalVariableWrite { name, span, .. }
                | Node::LocalVariableOpWrite { name, span, .. } => vec![(span.0, name.clone())],
                Node::MultiWrite { targets, span, .. } => {
                    targets.bound_names().into_iter().map(|(name, _)| (span.0, name)).collect()
                }
                Node::Loop { index, .. } => index.iter().map(|(name, s)| (s.0, name.clone())).collect(),
                _ => Vec::new(),
            })
            .collect();
        let mut out = Vec::new();
        for (name, regions, after, atom_end) in raw {
            let mut keys: Vec<String> = indexed
                .slot_writes
                .iter()
                .filter(|w| w.name == name && w.span.0 < atom_end)
                .map(|w| indexed_narrowing_key(&w.name, &w.key))
                .collect();
            if keys.is_empty() {
                continue;
            }
            keys.sort();
            keys.dedup();
            let def = innermost(&defs, atom_end);
            let closure = innermost(&closures, atom_end);
            // Only a rebind in the guard's own scope ends the cover; one in
            // a nested block may bind a block-local of the same name. Not
            // counting a rebind only widens the cover — the decline side.
            let next = rebinds
                .iter()
                .filter(|(w, n)| {
                    *n == name
                        && *w >= atom_end
                        && innermost(&defs, *w) == def
                        && innermost(&closures, *w) == closure
                })
                .map(|(w, _)| *w)
                .min();
            out.push(RecordGuard { regions, after, def, closure, next, keys });
        }
        out
    }

    /// The locals a predicate's class-guard atoms rebind on its edges — the
    /// shapes `Narrowing.analyse` reaches: `!`, `&&` / `||`, a
    /// parenthesised group's last statement, and the class atoms.
    fn guarded_locals(&self, ast: &LoweredAst, pred: NodeId) -> Vec<String> {
        let mut out = Vec::new();
        self.guarded_locals_into(ast, pred, &mut out);
        out
    }

    fn guarded_locals_into(&self, ast: &LoweredAst, pred: NodeId, out: &mut Vec<String>) {
        let local = |id: NodeId| match ast.get(id) {
            Node::LocalVariableRead { name, .. } => Some(name.clone()),
            _ => None,
        };
        match ast.get(pred) {
            Node::Statements { body, .. } => {
                if let Some(&last) = body.last() {
                    self.guarded_locals_into(ast, last, out);
                }
            }
            Node::Logical { left, right, .. } => {
                self.guarded_locals_into(ast, *left, out);
                self.guarded_locals_into(ast, *right, out);
            }
            Node::Call { receiver: Some(r), method, args, .. } => match (method.as_str(), args.as_slice()) {
                ("!", []) => self.guarded_locals_into(ast, *r, out),
                ("is_a?" | "kind_of?" | "instance_of?", [c]) => {
                    if let Some(name) = local(*r) {
                        if self.static_guard_class(ast, *c) {
                            out.push(name);
                        }
                    }
                }
                ("===", [a]) => {
                    if let Some(name) = local(*a) {
                        let receiver_guards = self.static_guard_class(ast, *r)
                            || matches!(ast.get(*r), Node::RegexpLit { .. } | Node::Range { .. });
                        if receiver_guards {
                            out.push(name);
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Whether `id` is a class operand the reference's class guard narrows
    /// through: a static constant spelling (`static_class_name`), and for a
    /// rooted `::C` one whose `C` is known (`rooted_class_predicate_name`).
    fn static_guard_class(&self, ast: &LoweredAst, id: NodeId) -> bool {
        let Node::ConstantRead { name, dynamic_base, self_anchored, rooted, .. } = ast.get(id) else {
            return false;
        };
        if name.is_empty() || *dynamic_base || *self_anchored {
            return false;
        }
        let bare = name.strip_prefix("::").unwrap_or(name);
        if *rooted || name.starts_with("::") {
            return self.constant_names_a_known_class(bare) || self.source.knows_class(bare);
        }
        true
    }

    /// [`RecordGuard`]s applied to the env a site reads: every record a
    /// pre-guard slot write may have left reads `untyped`.
    pub(crate) fn record_guard_overlay(
        &self,
        guards: &[RecordGuard],
        site: Span,
        env: &mut Cow<'_, TypeEnv>,
        interner: &mut Interner,
    ) {
        for g in guards {
            let in_arm = g.regions.iter().any(|r| r.0 <= site.0 && site.1 <= r.1);
            if !(in_arm || site.0 >= g.after) || g.next.is_some_and(|w| w <= site.0) {
                continue;
            }
            let outside = |c: Option<Span>| c.is_some_and(|c| !(c.0 <= site.0 && site.1 <= c.1));
            if outside(g.def) || outside(g.closure) {
                continue;
            }
            for key in &g.keys {
                if env.contains_key(key.as_str()) {
                    let u = interner.untyped();
                    env.to_mut().insert(key.clone(), u);
                }
            }
        }
    }
}
