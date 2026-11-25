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
//!
//! rigor-rs#987 — two more shapes reach a read in any order:
//!
//! - An index write (`d[0] = v`, `d.[]=(0, v)`, `d[0] ||= v`, `h.store(k,
//!   v)`). The second pass re-runs it over the widened entry, and when its
//!   value's class is not the carrier's element class the re-run widens the
//!   entry again, so the reference falls through to the envelope pass
//!   (`d = ["a"]; begin; d[0] = 9; d.zork; rescue; retry; end` is silent).
//!   The port does not model which re-runs converge, so it treats every one
//!   as reaching.
//! - A derived write: a write whose value reads a local or ivar the edge
//!   reaches at that read (`d = c`, `@iv = c`, `d = c.first`). The second
//!   pass binds it from the widened entry. It counts as an edge write of its
//!   target that reaches in any order, and it kills nothing. Derivation is a
//!   fixpoint, so `e = d` after `d = c` is derived too.
//!
//! rigor-rs#1000 — a kill protects the reads after it only until the next
//! derived write to the same local: `begin; d = [0]; d = c; d.nope; rescue;
//! c << 2; retry; end` re-binds `d` from the widened entry at `d = c` on the
//! second pass, so `d.nope` declines even though `d = [0]` killed the edge
//! for it. A kill still holds when every derived write to the local runs
//! before it (`d = c; d = [0]; d.nope` keeps firing) or past the read's
//! holder element (`x ? (d = c) : (d.nope)` fires — the `d = c` arm never
//! runs on the read's path).

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
    /// The derived writes' spans (the module doc): they kill nothing.
    derived: HashSet<Span>,
}

/// One local's edge writes in a [`RetryBegin`].
struct EdgeWrites {
    /// Start offsets, ascending.
    starts: Vec<usize>,
    /// Whether one of them reaches every read in any order: a rebind (not a
    /// mutation), an index write or a derived write (the module doc).
    rebind: bool,
    /// No statement list in the `begin` holds a non-derived kill of this
    /// local, so [`Chain::reaches`] can skip the kill scan. Computed once
    /// [`RetryBegin::derive`] has found every derived write; `false` until
    /// then, which takes the checked path.
    kill_free: bool,
    /// rigor-rs#1000 — the start offsets of this local's derived writes (the
    /// module doc), in discovery order. A kill of this local protects the
    /// reads after it only until the next derived write: the second pass
    /// binds the derived write's target from the widened entry, so the edge
    /// reaches a read the earlier kill once covered.
    derived_starts: Vec<usize>,
}

/// A statement list inside a [`RetryBegin`].
struct StmtList {
    /// Element spans, in source order.
    elems: Vec<Span>,
    /// Per local, the elements that kill it, in index order: a plain `x = v`
    /// statement whose value does not read `x` ([`kills`]), with its span.
    kills: HashMap<String, Vec<(usize, Span)>>,
}

impl StmtList {
    /// The span of the last element before `holder` that kills `name`,
    /// skipping the derived writes.
    fn last_kill_before(&self, name: &str, holder: usize, derived: &HashSet<Span>) -> Option<Span> {
        self.kills
            .get(name)?
            .iter()
            .rev()
            .find(|(i, s)| *i < holder && !derived.contains(s))
            .map(|&(_, s)| s)
    }
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
            // rigor-rs#987 — an index write on the edge reaches in any order.
            for n in idx.nodes_within(ast, *span) {
                let w = ast.get(n).span();
                if let Some(name) = index_write_local(ast, n) {
                    if regions.iter().any(|&r| within(w, r)) && !nested.iter().any(|&s| within(w, s)) {
                        let e = by_name.entry(name).or_insert_with(|| EdgeWrites {
                            starts: Vec::new(),
                            rebind: false,
                            kill_free: false,
                            derived_starts: Vec::new(),
                        });
                        e.starts.push(w.0);
                        e.rebind = true;
                    }
                }
            }
            for (w, name) in writes.clone() {
                if regions.iter().any(|&r| within(w, r))
                    && !nested.iter().any(|&s| within(w, s))
                    && seen.insert((w, name))
                {
                    let e = by_name.entry(name).or_insert_with(|| EdgeWrites {
                        starts: Vec::new(),
                        rebind: false,
                        kill_free: false,
                        derived_starts: Vec::new(),
                    });
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
            let mut begin = RetryBegin { span: *span, writes, lists, derived: HashSet::new() };
            begin.derive(ast, &idx, &nested);
            // With `derived` final, a local whose every kill is derived can
            // never be killed ([`StmtList::last_kill_before`] skips derived
            // spans): mark it so `reaches` answers without the kill scan
            // (rigor-rs#1003).
            for (name, w) in &mut begin.writes {
                w.kill_free = begin.lists.iter().all(|l| {
                    l.kills.get(name.as_str()).is_none_or(|ks| {
                        ks.iter().all(|(_, s)| begin.derived.contains(s))
                    })
                });
            }
            begins.push(begin);
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
        // One `begin` holding the site is the common case, and its `writes`
        // are already unique — skip the dedup set entirely. A nested
        // retrying `begin` can name the same local, so several holders dedup
        // through `seen` (a linear `out.contains` scan was quadratic on a
        // `begin` with hundreds of edge writes — rigor-rs#1003).
        let mut holders = self.begins.iter().filter(|b| within(site, b.span));
        let Some(first) = holders.next() else {
            return Vec::new();
        };
        if holders.next().is_none() {
            let chain = first.chain(site);
            return first
                .writes
                .iter()
                .filter(|(name, w)| chain.reaches(site, name, w, &first.derived))
                .map(|(name, _)| name.as_str())
                .collect();
        }
        let mut out: Vec<&str> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for b in self.begins.iter().filter(|b| within(site, b.span)) {
            let chain = b.chain(site);
            for (name, w) in &b.writes {
                if !seen.contains(name.as_str()) && chain.reaches(site, name, w, &b.derived) {
                    seen.insert(name.as_str());
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
        self.begins.iter().filter(|b| within(site, b.span)).any(|b| b.declines(site, name))
    }
}

impl RetryBegin {
    /// [`RetryEdges::declines`] for this `begin`.
    fn declines(&self, site: Span, name: &str) -> bool {
        let Ok(i) = self.writes.binary_search_by(|(n, _)| n.as_str().cmp(name)) else {
            return false;
        };
        self.chain(site).reaches(site, name, &self.writes[i].1, &self.derived)
    }

    /// rigor-rs#987 — add the derived writes (the module doc) to the edge
    /// writes, to a fixpoint. A candidate is a write in the `begin`, outside
    /// the `nested` scope bodies, whose value reads a local or ivar. It is
    /// derived once one of those reads declines.
    fn derive(&mut self, ast: &LoweredAst, idx: &crate::reach_index::ReachIndex, nested: &[Span]) {
        let mut cands: Vec<Derivable> = Vec::new();
        for n in idx.nodes_within(ast, self.span) {
            let node = ast.get(n);
            let w = node.span();
            if nested.iter().any(|&s| within(w, s)) {
                continue;
            }
            let (targets, value) = match node {
                Node::LocalVariableWrite { name, value, .. }
                | Node::LocalVariableOpWrite { name, value, .. }
                | Node::InstanceVariableWrite { name, value, .. } => (vec![name.clone()], *value),
                Node::MultiWrite { targets, value, .. } => {
                    let mut bound = Vec::new();
                    targets.collect_bound_names(&mut bound);
                    (bound.into_iter().map(|(n, _)| n).collect(), *value)
                }
                _ => continue,
            };
            let reads: Vec<(String, Span)> = idx
                .nodes_within(ast, ast.get(value).span())
                .filter_map(|r| match ast.get(r) {
                    Node::LocalVariableRead { name, span } => Some((name.clone(), *span)),
                    Node::VariableRead { name, span } if crate::def_local_marks::is_ivar(name) => {
                        Some((name.clone(), *span))
                    }
                    _ => None,
                })
                .collect();
            if !targets.is_empty() && !reads.is_empty() {
                cands.push(Derivable { span: w, targets, reads });
            }
        }
        // A worklist over names: deriving a write changes only its targets'
        // edge writes and kills, so only the candidates reading a changed
        // name need another look. A full re-scan per round was quadratic in
        // a chain `v1 = v0; v2 = v1; …` (a 2000-link chain took 25s).
        let mut readers: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, c) in cands.iter().enumerate() {
            for (n, _) in &c.reads {
                let list = readers.entry(n.as_str()).or_default();
                if list.last() != Some(&i) {
                    list.push(i);
                }
            }
        }
        let mut done = vec![false; cands.len()];
        // Each item is a candidate and the one name whose reads to look at
        // (`None`: all of them, the first look).
        let mut work: Vec<(usize, Option<&str>)> = (0..cands.len()).rev().map(|i| (i, None)).collect();
        while let Some((i, only)) = work.pop() {
            if done[i] {
                continue;
            }
            let Derivable { span: w, targets, reads } = &cands[i];
            let hit = reads
                .iter()
                .filter(|(n, _)| only.is_none_or(|o| o == n))
                .any(|(n, s)| self.declines(*s, n));
            if !hit {
                continue;
            }
            done[i] = true;
            self.derived.insert(*w);
            for t in targets {
                self.add_reaching_write(t, w.0);
                if let Some((name, list)) = readers.get_key_value(t.as_str()) {
                    work.extend(list.iter().filter(|&&j| !done[j]).map(|&j| (j, Some(*name))));
                }
            }
        }
    }

    /// Record a derived write of `name` at `start` that reaches every read
    /// in any order ([`EdgeWrites::rebind`]).
    fn add_reaching_write(&mut self, name: &str, start: usize) {
        let i = match self.writes.binary_search_by(|(n, _)| n.as_str().cmp(name)) {
            Ok(i) => i,
            Err(i) => {
                let fresh = EdgeWrites {
                    starts: Vec::new(),
                    rebind: false,
                    kill_free: false,
                    derived_starts: Vec::new(),
                };
                self.writes.insert(i, (name.to_string(), fresh));
                i
            }
        };
        let e = &mut self.writes[i].1;
        if let Err(k) = e.starts.binary_search(&start) {
            e.starts.insert(k, start);
        }
        e.derived_starts.push(start);
        e.rebind = true;
    }

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

/// A candidate derived write ([`RetryBegin::derive`]): its span, the names
/// it writes and the local/ivar reads in its value.
struct Derivable {
    span: Span,
    targets: Vec<String>,
    reads: Vec<(String, Span)>,
}

/// [`RetryBegin::chain`].
struct Chain<'a> {
    links: Vec<(&'a StmtList, usize)>,
}

impl Chain<'_> {
    /// Whether an edge write of `name` reaches the read at `site`: no
    /// element before the read's kills `name`, and some write is a rebind,
    /// or lies in no element before the read's in a list of the chain.
    fn reaches(&self, site: Span, name: &str, w: &EdgeWrites, derived: &HashSet<Span>) -> bool {
        if !w.kill_free {
            let killed = self.links.iter().any(|(l, j)| {
                let Some(kill) = l.last_kill_before(name, *j, derived) else {
                    return false;
                };
                // rigor-rs#1000 — the kill holds only until a derived write
                // of `name` runs between it and the read: the second pass
                // binds that write's target from the widened entry, so the
                // edge reaches after all. `begin; d = [0]; d = c; d.nope;
                // rescue; c << 2; retry; end` is silent on the oracle.
                !w.derived_starts.iter().any(|&d| d >= kill.1 && self.before_holder(d))
            });
            if killed {
                return false;
            }
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

    /// Whether `start` lies inside an element strictly before the holder of
    /// a chain link — a statement every path to the read runs before it.
    /// Writes inside the holder element itself are not covered: a derived
    /// write in a sibling arm (`x ? (d = c) : (d.nope)`) does not run on the
    /// read's path (rigor-rs#1000).
    fn before_holder(&self, start: usize) -> bool {
        self.links
            .iter()
            .any(|(l, j)| *j > 0 && l.elems[0].0 <= start && start <= l.elems[j - 1].1)
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
            let mut kills: HashMap<String, Vec<(usize, Span)>> = HashMap::new();
            for (i, &s) in l.iter().enumerate() {
                if let Some(name) = self::kills(ast, idx, s) {
                    kills.entry(name).or_default().push((i, ast.get(s).span()));
                }
            }
            StmtList { elems: l.iter().map(|&s| ast.get(s).span()).collect(), kills }
        })
        .collect()
}

/// rigor-rs#987 — the locals (not ivars: the collection walk never types
/// one) the retrying `begin` `id` stores through an index write on its edge
/// ([`index_write_local`]), sorted and deduplicated.
/// The reference's binding past the `begin` is the join of the retry passes,
/// and a re-run index write can push it to the envelope pass (the module
/// doc). The port does not model that, so the walkers widen these locals past
/// the `begin`. The result is empty for a `begin` with no retrying clause.
pub(crate) fn edge_index_write_locals(typer: &Typer<'_>, ast: &LoweredAst, id: NodeId) -> Vec<String> {
    let Node::BeginRescue { span, main_body, ensure_body, clauses, .. } = ast.get(id) else {
        return Vec::new();
    };
    let idx = typer.reach_index(ast);
    let mut regions: Vec<Span> = clauses
        .iter()
        .filter(|c| idx.clause_retries(typer, ast, c.span))
        .map(|c| c.span)
        .collect();
    if regions.is_empty() {
        return Vec::new();
    }
    regions.extend(hull(ast, main_body));
    regions.extend(hull(ast, ensure_body));
    let nested: Vec<Span> = idx
        .nodes_within(ast, *span)
        .filter_map(|n| match ast.get(n) {
            Node::Definition { span: s, .. }
            | Node::ClassDef { span: s, .. }
            | Node::ModuleDef { span: s, .. } => Some(*s),
            _ => None,
        })
        .collect();
    let mut out: Vec<String> = idx
        .nodes_within(ast, *span)
        .filter_map(|n| {
            let w = ast.get(n).span();
            let name = index_write_local(ast, n).filter(|n| !crate::def_local_marks::is_ivar(n))?;
            (regions.iter().any(|&r| within(w, r)) && !nested.iter().any(|&s| within(w, s)))
                .then(|| name.to_string())
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The local or ivar an index write `id` stores through: `[]=` or `store`
/// called on a bare local or ivar (`d[0] = v`, `d.[]=(0, v)`, `h.store(k,
/// v)`, `@d[0] = v`), or a compound index write on one (`d[0] ||= v`).
fn index_write_local(ast: &LoweredAst, id: NodeId) -> Option<&str> {
    let receiver = match ast.get(id) {
        Node::Call { receiver: Some(r), method, .. } if method == "[]=" || method == "store" => *r,
        Node::IndexWrite { receiver: Some(r), .. } => *r,
        _ => return None,
    };
    match ast.get(receiver) {
        Node::LocalVariableRead { name, .. } => Some(name),
        Node::VariableRead { name, .. } if crate::def_local_marks::is_ivar(name) => Some(name),
        _ => None,
    }
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
