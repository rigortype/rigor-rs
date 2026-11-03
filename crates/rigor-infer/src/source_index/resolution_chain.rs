//! Upstream v0.4.1 (ADR-119 / ADR-24 amended; rigortype/rigor#1578, #1594,
//! #1605, #1606, #1623, #1631) — Ruby's ancestor order for a project class,
//! and the conservative half of `DefinerResolution`: the one question "which
//! project definer does Ruby call for `name`?", answered only where the
//! reference's answer is `Known`, and declined (`None`) everywhere else.
//!
//! The reference builds a `ResolutionChain` (CRuby's `include_modules_at`
//! insertion rule replayed over the discovery tables) and reads the first
//! definer off it, declining (`UNKNOWN`) where:
//!
//! * a node on the chain carries an unpositioned mixin edge (a conditional,
//!   block, hook, receiver-form or unrecordable edge) that per-name
//!   `Relevance` cannot discharge, or is declared in several files with
//!   several edges ([`AncestryIndex`] holds the port's census of those);
//! * the replay met a FORK (a module reaching the chain by a second route —
//!   the reference settles one include-side fork through a retro world; the
//!   port declines on every fork, which only declines more);
//! * an external ancestor ahead of the definer may answer first (one RBS does
//!   not know, or knows and declares the name in);
//! * the definer rests on a `possible` def or a contested visibility (the
//!   reference then asks the next candidate, and two candidates with
//!   different owners never agree);
//! * the chain was cut at [`CHAIN_LIMIT`] project entries.
//!
//! Every gap between this and the reference is on the declining side: a fork
//! the reference settles, a named mark whose closure holds a node the port
//! cannot classify, or an entry the port's `known_user` reads as external.

use std::collections::{HashMap, HashSet};

use rigor_index::CoreIndex;
use rigor_parse::{AncestryFacts, MixinSide, MIXIN_WILDCARD};

use super::{DefKind, SourceIndex};

/// `ResolutionChain::LIMIT` — the project entries a chain may hold.
const CHAIN_LIMIT: usize = 100;

/// The merged per-owner ancestry facts ([`AncestryFacts`] across files).
#[derive(Default)]
pub(crate) struct AncestryIndex {
    include_listed: HashMap<String, Vec<String>>,
    extend_listed: HashMap<String, Vec<String>>,
    possible: HashSet<(String, String)>,
    contested: HashSet<(String, String)>,
    dirty: HashSet<String>,
    prepends: HashMap<String, Vec<String>>,
    decl_files: HashMap<String, usize>,
    def_owners: HashSet<String>,
}

impl AncestryIndex {
    pub(crate) fn ingest<'a>(
        &mut self,
        facts: &AncestryFacts,
        declared: impl Iterator<Item = &'a str>,
    ) {
        for u in &facts.unpositioned {
            let table = match u.side {
                MixinSide::Include => &mut self.include_listed,
                MixinSide::Extend => &mut self.extend_listed,
            };
            for o in &u.owners {
                let list = table.entry(o.clone()).or_default();
                for n in &u.names {
                    if !list.contains(n) {
                        list.push(n.clone());
                    }
                }
            }
        }
        self.possible.extend(facts.possible_methods.iter().cloned());
        self.contested.extend(facts.contested_visibilities.iter().cloned());
        self.dirty.extend(facts.dirty_owners.iter().cloned());
        self.def_owners.extend(facts.def_owners.iter().cloned());
        for (owner, raw) in &facts.direct_prepends {
            let list = self.prepends.entry(owner.clone()).or_default();
            if !list.contains(raw) {
                list.push(raw.clone());
            }
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for q in declared {
            if seen.insert(q) {
                *self.decl_files.entry(q.to_string()).or_insert(0) += 1;
            }
        }
    }

    fn wildcard_include(&self, owner: &str) -> bool {
        self.include_listed
            .get(owner)
            .is_some_and(|l| l.iter().any(|n| n == MIXIN_WILDCARD))
    }
}

/// One ancestor on a chain: a project class/module, or an ancestor the
/// project does not declare (its spelling and the node that wrote it, for
/// lexical resolution against RBS).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChainEntry {
    Project(String),
    External { raw: String, from: String },
}

/// A node whose mixin order is not a fact: its listed names (or the wildcard)
/// and whether the multi-file rule fired.
struct Mark {
    node: String,
    listed: Vec<String>,
    multi_file: bool,
}

pub(crate) struct Chain {
    pub(crate) entries: Vec<ChainEntry>,
    marks: Vec<Mark>,
}

struct Builder<'a> {
    idx: &'a SourceIndex,
    nodes: Vec<String>,
    stack: Vec<String>,
    failed: bool,
}

impl Builder<'_> {
    fn project_count(entries: &[ChainEntry]) -> usize {
        entries.iter().filter(|e| matches!(e, ChainEntry::Project(_))).count()
    }

    /// The chain of the project class/module `name`: its prepend region, the
    /// class, its includes (latest statement nearest), then its superclass's
    /// chain. Any fork, cycle or cut sets `failed`.
    fn lin(&mut self, name: &str) -> Vec<ChainEntry> {
        if self.failed || self.stack.iter().any(|s| s == name) {
            self.failed = true;
            return Vec::new();
        }
        if !self.nodes.iter().any(|n| n == name) {
            self.nodes.push(name.to_string());
        }
        let Some(entry) = self.idx.override_classes.get(name) else {
            return vec![ChainEntry::Project(name.to_string())];
        };
        self.stack.push(name.to_string());
        let mut entries = vec![ChainEntry::Project(name.to_string())];
        if let Some(sup) = &entry.superclass {
            let sub = self.sub_lin(name, sup);
            entries.extend(sub);
        }
        let prepends: Vec<String> =
            self.idx.ancestry.prepends.get(name).cloned().unwrap_or_default();
        let includes: Vec<String> = entry
            .includes
            .iter()
            .filter(|raw| !prepends.contains(raw))
            .cloned()
            .collect();
        let mut origin = 0usize;
        for raw in &prepends {
            let sub = self.sub_lin(name, raw);
            if Self::forks(&entries, &sub) {
                self.failed = true;
            }
            origin += sub.len();
            entries.splice(0..0, sub);
        }
        for raw in &includes {
            let sub = self.sub_lin(name, raw);
            if Self::forks(&entries, &sub) {
                self.failed = true;
            }
            entries.splice(origin + 1..origin + 1, sub);
        }
        if Self::project_count(&entries) > CHAIN_LIMIT {
            self.failed = true;
        }
        self.stack.pop();
        entries
    }

    fn forks(entries: &[ChainEntry], sub: &[ChainEntry]) -> bool {
        sub.iter()
            .any(|e| matches!(e, ChainEntry::Project(_)) && entries.contains(e))
    }

    fn sub_lin(&mut self, owner: &str, raw: &str) -> Vec<ChainEntry> {
        match self.idx.resolve_override_ancestor(owner, raw) {
            Some(p) if self.idx.known_user(&p) => self.lin(&p),
            _ => vec![ChainEntry::External {
                raw: raw.strip_prefix("::").unwrap_or(raw).to_string(),
                from: owner.to_string(),
            }],
        }
    }
}

impl SourceIndex {
    /// The reference's `known_user_class?` under the `:methods` flavor, as far
    /// as the port's tables show it: a project entry that records a
    /// superclass, a method, a visibility, a mixin edge (direct or not) or a
    /// `def` of either kind. Anything else (an empty or constants-only module)
    /// is an EXTERNAL entry on a chain.
    pub(crate) fn known_user(&self, name: &str) -> bool {
        self.override_classes.get(name).is_some_and(|e| {
            e.superclass.is_some()
                || !e.methods.is_empty()
                || !e.includes.is_empty()
                || !e.method_visibilities.is_empty()
                || self.ancestry.def_owners.contains(name)
                || self.ancestry.include_listed.contains_key(name)
        })
    }

    /// The instance-side chain of the project class `class`, or `None` where
    /// the port cannot vouch for Ruby's order (a fork, a cycle, a cut).
    pub(crate) fn instance_chain(&self, class: &str) -> Option<Chain> {
        if !self.override_classes.contains_key(class) {
            return None;
        }
        let mut b = Builder { idx: self, nodes: Vec::new(), stack: Vec::new(), failed: false };
        let entries = b.lin(class);
        if b.failed {
            return None;
        }
        let marks = b
            .nodes
            .iter()
            .filter_map(|node| self.mark_of(node))
            .collect();
        Some(Chain { entries, marks })
    }

    fn mark_of(&self, node: &str) -> Option<Mark> {
        let listed = self.ancestry.include_listed.get(node).cloned().unwrap_or_default();
        let edges = self.override_classes.get(node).map_or(0, |e| e.includes.len())
            + listed.iter().filter(|n| *n != MIXIN_WILDCARD).count();
        let multi_file =
            edges >= 2 && self.ancestry.decl_files.get(node).copied().unwrap_or(0) >= 2;
        if listed.is_empty() && !multi_file {
            return None;
        }
        Some(Mark { node: node.to_string(), listed, multi_file })
    }

    /// `Relevance.discharged?` — a mark does not move the first definer of
    /// `method` when the closure of every module it names cannot answer it.
    fn discharged(&self, core: &CoreIndex, mark: &Mark, method: &str) -> bool {
        if mark.listed.iter().any(|n| n == MIXIN_WILDCARD) {
            return false;
        }
        if !mark.listed.iter().all(|raw| self.closure_clean(core, &mark.node, raw, method)) {
            return false;
        }
        if mark.multi_file {
            let mut edges: Vec<String> = self
                .override_classes
                .get(&mark.node)
                .map(|e| e.includes.clone())
                .unwrap_or_default();
            edges.extend(mark.listed.iter().cloned());
            let dirty = edges
                .iter()
                .filter(|raw| !self.closure_clean(core, &mark.node, raw, method))
                .count();
            if dirty > 1 {
                return false;
            }
        }
        true
    }

    fn closure_clean(&self, core: &CoreIndex, node: &str, raw: &str, method: &str) -> bool {
        match self.resolve_override_ancestor(node, raw) {
            Some(p) if self.known_user(&p) => {
                let Some(chain) = self.instance_chain(&p) else {
                    return false;
                };
                chain.entries.iter().all(|e| match e {
                    ChainEntry::External { raw, from } => external_lacks(core, from, raw, method),
                    ChainEntry::Project(q) => {
                        !self.ancestry.wildcard_include(q)
                            && !self.ancestry.dirty.contains(q)
                            && !self.records_name(q, method)
                    }
                })
            }
            _ => external_lacks(core, node, raw, method),
        }
    }

    /// Whether the project entry `q` records `method` in any table.
    fn records_name(&self, q: &str, method: &str) -> bool {
        let key = (q.to_string(), method.to_string());
        self.override_classes.get(q).is_some_and(|e| {
            e.methods.contains(method) || e.method_visibilities.contains_key(method)
        }) || self.ancestry.possible.contains(&key)
            || self.ancestry.contested.contains(&key)
            || self.owner_defines(q, method, DefKind::Instance)
            || self.owner_defines(q, method, DefKind::Singleton)
            || self.discovered_methods.get(q).is_some_and(|m| m.contains(method))
    }

    /// Whether `owner`'s `method` rests on a fact the port cannot vouch for:
    /// a `possible` def or a contested visibility slot.
    fn uncertain_definer(&self, owner: &str, method: &str) -> bool {
        let key = (owner.to_string(), method.to_string());
        self.ancestry.possible.contains(&key) || self.ancestry.contested.contains(&key)
    }

    /// Upstream v0.4.1 (#1594, ADR-119 C2-b1): whether a call of `method` on
    /// an instance of the project class `name` (the collapsed name the
    /// typer's registry holds) may be typed from that class's OWN `def`:
    /// every qualified class the name can denote that defines `method` must
    /// read itself as the `Known` definer on its Ruby-order chain. A class the
    /// override index does not hold says nothing (the caller's own gates
    /// stand).
    pub fn own_def_typing_settled(&self, core: &CoreIndex, name: &str, method: &str) -> bool {
        let suffix = format!("::{name}");
        self.override_classes
            .keys()
            .filter(|k| *k == name || k.ends_with(&suffix))
            .filter(|k| self.owner_defines(k, method, DefKind::Instance))
            .all(|k| {
                self.settled_definer(core, k, method, false, |n| {
                    self.owner_defines(n, method, DefKind::Instance)
                })
                .is_some_and(|d| d == *k)
            })
    }

    /// The `Known` owner `DefinerResolution` reads for `method` on the
    /// instance side of the project class `class`, or `None` (`UNKNOWN` /
    /// `ABSENT`, or a case the port cannot prove). `skip_self` reads from the
    /// entry after the class (the override lints' `from:`); otherwise from
    /// the chain's head (the typing read). `defines` is the reader's own
    /// definer predicate.
    pub(crate) fn settled_definer(
        &self,
        core: &CoreIndex,
        class: &str,
        method: &str,
        skip_self: bool,
        defines: impl Fn(&str) -> bool,
    ) -> Option<String> {
        let chain = self.instance_chain(class)?;
        if !chain.marks.iter().all(|m| self.discharged(core, m, method)) {
            return None;
        }
        let start = if skip_self {
            chain
                .entries
                .iter()
                .position(|e| matches!(e, ChainEntry::Project(n) if n == class))?
                + 1
        } else {
            0
        };
        for e in &chain.entries[start..] {
            match e {
                ChainEntry::External { raw, from } => {
                    if !external_lacks(core, from, raw, method) {
                        return None;
                    }
                }
                ChainEntry::Project(n) => {
                    if self.uncertain_definer(n, method) {
                        return None;
                    }
                    if defines(n) {
                        return Some(n.clone());
                    }
                }
            }
        }
        None
    }
}

/// `external_lacks?` — true only for an ancestor RBS knows whose declaration
/// lacks `method`. The spelling resolves against `from`'s lexical nesting,
/// most qualified first; a name RBS does not know may answer.
fn external_lacks(core: &CoreIndex, from: &str, raw: &str, method: &str) -> bool {
    let segments: Vec<&str> = from.split("::").collect();
    for i in (0..segments.len()).rev() {
        let cand = if i == 0 {
            raw.to_string()
        } else {
            format!("{}::{}", segments[..i].join("::"), raw)
        };
        if core.knows_qualified_class(&cand) {
            return !core.qualified_class_has_method(&cand, method);
        }
        if !cand.contains("::") && core.knows_toplevel_class(&cand) {
            return !core.class_has_method(&cand, method);
        }
    }
    false
}
