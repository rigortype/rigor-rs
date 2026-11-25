//! rigor-rs#972 — which receiver reads inside a retrying `begin` the retry
//! edge can reach.
//!
//! The reference evaluates a `begin` whose rescue chain holds a `retry` twice
//! (`eval_retried_begin`): the second pass re-runs the protected body, the
//! clauses and the `else` over `widen_entry_for_retry`'s entry, the original
//! entry joined with every scope on the edge. Those are the scope after each
//! statement of the body (`raise_scopes`) and each `retry`'s scope. A read
//! anywhere in the `begin` can therefore see a write the body or a retrying
//! clause makes LATER in source order (`begin; c.nope; c << 2; rescue;
//! retry; end` and `rescue; c.nope; c = [2]; retry` are silent on the
//! oracle), or one on an exclusive sibling path (another clause, the other
//! arm of an `if`).
//!
//! The port's walkers replay a `begin` once, in source order, so they miss
//! those writes. This table does not model the join's literal/envelope
//! convergence. It names the reads the edge can change, and the walkers
//! decline them. A read keeps its walker's answer when every edge write to
//! its local runs before it on every path from the `begin`'s entry, because
//! the write lands in an earlier statement of a statement list that also
//! holds the read. The exception is a write that rebinds the local from
//! something other than a fresh value (`c = c + [1]`, `c += [1]`): the
//! widened entry flows through it. A plain rebind that does not read the
//! local (`c = [2]`) kills the edge for the reads after it.

use std::collections::{HashMap, HashSet};

use rigor_parse::{LoweredAst, Node, NodeId, Span};

use crate::Typer;

fn within(inner: Span, outer: Span) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}

/// One retrying `begin`: its span, the edge writes per local and the
/// statement lists inside it.
struct RetryBegin {
    span: Span,
    /// Per local, the edge writes: every write in the protected body, a
    /// retrying clause or the `ensure` body. A write inside a nested
    /// `def`/`class`/`module` body is another scope's and is left out.
    writes: Vec<(String, EdgeWrites)>,
    lists: Vec<StmtList>,
}

/// One local's edge writes in a [`RetryBegin`].
struct EdgeWrites {
    /// Start offsets, ascending.
    starts: Vec<usize>,
    /// Whether any of them is a rebind (not a mutation).
    rebind: bool,
}

/// A statement list inside a [`RetryBegin`].
struct StmtList {
    /// Element spans, in source order.
    elems: Vec<Span>,
    /// Per local, the index of the first element that kills it: a plain
    /// `x = v` statement whose value does not read `x`.
    first_kill: HashMap<String, usize>,
}

/// The file's retrying `begin`s ([`RetryBegin`]).
#[derive(Default)]
pub(crate) struct RetryEdges {
    begins: Vec<RetryBegin>,
}

impl RetryEdges {
    /// Every retrying `begin` the `keep` filter admits, with the edge writes
    /// taken from `writes` (rebinds and mutations, span-keyed). A write is a
    /// rebind when `rebinds` holds its span and name.
    pub(crate) fn build<'w>(
        typer: &Typer<'_>,
        ast: &LoweredAst,
        writes: impl Iterator<Item = (Span, &'w str)> + Clone,
        rebinds: &[(Span, String)],
        keep: impl Fn(Span) -> bool,
    ) -> Self {
        let idx = typer.reach_index(ast);
        let rebind_set: HashSet<(Span, &str)> =
            rebinds.iter().map(|(s, n)| (*s, n.as_str())).collect();
        let mut begins = Vec::new();
        let scope_bodies: Vec<Span> = ast
            .iter()
            .filter_map(|(_, n)| match n {
                Node::Definition { span, .. }
                | Node::ClassDef { span, .. }
                | Node::ModuleDef { span, .. } => Some(*span),
                _ => None,
            })
            .collect();
        for (id, n) in ast.iter() {
            let Node::BeginRescue { span, main_body, ensure_body, clauses, .. } = n else {
                continue;
            };
            if clauses.is_empty() || !keep(*span) {
                continue;
            }
            let mut regions: Vec<Span> = clauses
                .iter()
                .filter(|c| idx.clause_retries(typer, ast, c.span))
                .map(|c| c.span)
                .collect();
            if regions.is_empty() {
                continue;
            }
            regions.extend(hull(ast, main_body));
            regions.extend(hull(ast, ensure_body));
            // A nested scope body inside this `begin` is another local scope.
            let nested: Vec<Span> = scope_bodies
                .iter()
                .copied()
                .filter(|&s| within(s, *span) && s != *span)
                .collect();
            let mut seen: HashSet<(Span, &str)> = HashSet::new();
            let mut by_name: HashMap<&str, EdgeWrites> = HashMap::new();
            for (w, name) in writes.clone() {
                if regions.iter().any(|&r| within(w, r))
                    && !nested.iter().any(|&s| within(w, s))
                    && seen.insert((w, name))
                {
                    let e = by_name
                        .entry(name)
                        .or_insert_with(|| EdgeWrites { starts: Vec::new(), rebind: false });
                    e.starts.push(w.0);
                    e.rebind |= rebind_set.contains(&(w, name));
                }
            }
            if by_name.is_empty() {
                continue;
            }
            let mut writes: Vec<(String, EdgeWrites)> = by_name
                .into_iter()
                .map(|(n, mut e)| {
                    e.starts.sort_unstable();
                    (n.to_string(), e)
                })
                .collect();
            writes.sort_by(|a, b| a.0.cmp(&b.0));
            let lists = lists_within(ast, &idx, id, *span);
            begins.push(RetryBegin { span: *span, writes, lists });
        }
        RetryEdges { begins }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.begins.is_empty()
    }

    /// Whether `site` sits inside a retrying `begin` of the table.
    pub(crate) fn holds(&self, site: Span) -> bool {
        self.begins.iter().any(|b| within(site, b.span))
    }

    /// The locals [`Self::declines`] declines at `site`.
    pub(crate) fn declined_names(&self, site: Span) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for b in self.begins.iter().filter(|b| within(site, b.span)) {
            let chain = b.chain(site);
            for (name, w) in &b.writes {
                if !out.contains(&name.as_str()) && chain.reaches(site, name, w) {
                    out.push(name);
                }
            }
        }
        out
    }

    /// Whether a read of `name` at `site` sits in a retrying `begin` whose
    /// edge can carry a write of `name` that the read's own walk does not see
    /// in order (the module doc).
    pub(crate) fn declines(&self, site: Span, name: &str) -> bool {
        self.begins.iter().filter(|b| within(site, b.span)).any(|b| {
            let Ok(i) = b.writes.binary_search_by(|(n, _)| n.as_str().cmp(name)) else {
                return false;
            };
            b.chain(site).reaches(site, name, &b.writes[i].1)
        })
    }
}

impl RetryBegin {
    /// Per statement list holding `site`, the list and the index of the
    /// element holding it: the lists along the chain from the `begin` down
    /// to `site`.
    fn chain(&self, site: Span) -> Chain<'_> {
        let links = self
            .lists
            .iter()
            .filter_map(|l| {
                let k = l.elems.partition_point(|s| s.0 <= site.0);
                (k > 0 && within(site, l.elems[k - 1])).then(|| (l, k - 1))
            })
            .collect();
        Chain { links }
    }
}

/// [`RetryBegin::chain`].
struct Chain<'a> {
    links: Vec<(&'a StmtList, usize)>,
}

impl Chain<'_> {
    /// Whether an edge write of `name` reaches the read at `site`: no
    /// element before the read's kills `name`, and some write is a rebind,
    /// or lies in no element before the read's in a list of the chain.
    fn reaches(&self, site: Span, name: &str, w: &EdgeWrites) -> bool {
        let killed = self
            .links
            .iter()
            .any(|(l, j)| l.first_kill.get(name).is_some_and(|&k| k < *j));
        if killed {
            return false;
        }
        if w.rebind || w.starts.last().is_some_and(|&s| s >= site.0) {
            return true;
        }
        // Every write starts before the read. Each list's prefix covers one
        // offset range, and the ranges along the chain are disjoint (an
        // inner list sits inside the outer list's element holding the read),
        // so the writes are all in order exactly when the prefixes hold all
        // of them.
        let count = |from: usize, to: usize| {
            w.starts.partition_point(|&s| s < to) - w.starts.partition_point(|&s| s < from)
        };
        let ordered: usize = self
            .links
            .iter()
            .filter(|(_, j)| *j > 0)
            .map(|(l, j)| count(l.elems[0].0, l.elems[j - 1].1))
            .sum();
        ordered < w.starts.len()
    }
}

/// The span covering a statement list, or `None` for an empty one.
fn hull(ast: &LoweredAst, list: &[NodeId]) -> Option<Span> {
    let (&f, &l) = (list.first()?, list.last()?);
    Some((ast.get(f).span().0, ast.get(l).span().1))
}

/// Every statement list under the `begin` node `begin` (span `span`), itself
/// included, as [`StmtList`]s. A list kind this misses only makes a write
/// look out of order, which declines.
fn lists_within(
    ast: &LoweredAst,
    idx: &crate::reach_index::ReachIndex,
    begin: NodeId,
    span: Span,
) -> Vec<StmtList> {
    let mut raw: Vec<Vec<NodeId>> = Vec::new();
    let ids: Vec<NodeId> =
        std::iter::once(begin).chain(idx.nodes_within(ast, span).filter(|&i| i != begin)).collect();
    for id in ids {
        match ast.get(id) {
            Node::Statements { body, kind: rigor_parse::StatementsKind::Sequence, .. }
            | Node::When { body, .. }
            | Node::Loop { body, .. }
            | Node::Lambda { body, .. }
            | Node::Definition { body, .. }
            | Node::ClassDef { body, .. }
            | Node::ModuleDef { body, .. } => raw.push(body.clone()),
            Node::If { then_body, else_body, .. } => {
                raw.push(then_body.clone());
                raw.push(else_body.clone());
            }
            Node::Case { else_body, .. } => raw.push(else_body.clone()),
            Node::Call { block_body, .. } => raw.push(block_body.clone()),
            Node::BeginRescue { body, main_body, ensure_body, clauses, .. } => {
                raw.push(main_body.clone());
                raw.push(ensure_body.clone());
                raw.extend(clauses.iter().map(|c| c.body.clone()));
                // The `else` clause: the `body` ids nothing else claims.
                let mut covered: HashSet<NodeId> = main_body.iter().copied().collect();
                covered.extend(ensure_body.iter().copied());
                for c in clauses {
                    covered.extend(c.exceptions.iter().copied());
                    covered.extend(c.body.iter().copied());
                }
                raw.push(body.iter().copied().filter(|s| !covered.contains(s)).collect());
            }
            _ => {}
        }
    }
    raw.into_iter()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut first_kill = HashMap::new();
            for (i, &s) in l.iter().enumerate() {
                if let Some(name) = kills(ast, idx, s) {
                    first_kill.entry(name).or_insert(i);
                }
            }
            StmtList { elems: l.iter().map(|&s| ast.get(s).span()).collect(), first_kill }
        })
        .collect()
}

/// The local the statement `id` rebinds to a fresh value: `x = v` whose `v`
/// reads no `x`.
fn kills(ast: &LoweredAst, idx: &crate::reach_index::ReachIndex, id: NodeId) -> Option<String> {
    let Node::LocalVariableWrite { name, value, .. } = ast.get(id) else {
        return None;
    };
    let vspan = ast.get(*value).span();
    let reads = idx
        .nodes_within(ast, vspan)
        .any(|n| matches!(ast.get(n), Node::LocalVariableRead { name: r, .. } if r == name));
    (!reads).then(|| name.clone())
}
