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
    /// Per owner, its direct mixin statements in order: `(prepend?, group)`.
    groups: HashMap<String, Vec<(bool, Vec<String>)>>,
    decl_files: HashMap<String, usize>,
    def_owners: HashSet<String>,
    /// Upstream v0.4.2 (#1706): every file's top-level `include` names
    /// ([`AncestryFacts::toplevel_includes`]), project-wide, deduplicated.
    toplevel_includes: Vec<String>,
    /// Upstream v0.4.2 (#1701): the gated vendored features some analysed
    /// file requires ([`AncestryFacts::required_features`]).
    required_features: HashSet<String>,
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
        self.required_features.extend(facts.required_features.iter().cloned());
        for raw in &facts.toplevel_includes {
            if !self.toplevel_includes.contains(raw) {
                self.toplevel_includes.push(raw.clone());
            }
        }
        for (owner, prepend, names) in &facts.direct_mixins {
            self.groups.entry(owner.clone()).or_default().push((*prepend, names.clone()));
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
    /// The world in which every insertion was made (Ruby's order when the
    /// statements ran with the forked module's other route LATER).
    pub(crate) entries: Vec<ChainEntry>,
    /// With exactly one include-side fork, the RETRO world: the same chain
    /// with the forked insertion skipped (the other route ran first, so Ruby
    /// skipped the `include`). A reader stands only where both agree.
    retro: Option<Vec<ChainEntry>>,
    marks: Vec<Mark>,
}

/// A chain entry and whether the one forked `include` inserted it.
type Tagged = (ChainEntry, bool);

struct Builder<'a> {
    idx: &'a SourceIndex,
    nodes: Vec<String>,
    stack: Vec<String>,
    failed: bool,
    forks: usize,
}

impl Builder<'_> {
    fn project_count(entries: &[Tagged]) -> usize {
        entries.iter().filter(|(e, _)| matches!(e, ChainEntry::Project(_))).count()
    }

    /// The chain of the project class/module `name`: its prepend region, the
    /// class, its includes (latest statement nearest), then its superclass's
    /// chain. A cycle, a cut, a prepend fork or a second fork sets `failed`;
    /// ONE include-side skip — the last entry of the inserted sub-chain is
    /// already on the chain (#1570) — is inserted tagged, so the retro world
    /// can drop it.
    fn lin(&mut self, name: &str) -> Vec<Tagged> {
        if self.failed || self.stack.iter().any(|s| s == name) {
            self.failed = true;
            return Vec::new();
        }
        if !self.nodes.iter().any(|n| n == name) {
            self.nodes.push(name.to_string());
        }
        let Some(entry) = self.idx.override_classes.get(name) else {
            return vec![(ChainEntry::Project(name.to_string()), false)];
        };
        self.stack.push(name.to_string());
        let mut entries = vec![(ChainEntry::Project(name.to_string()), false)];
        if let Some(sup) = &entry.superclass {
            let sub = self.sub_lin(name, sup);
            entries.extend(sub);
        }
        // Each statement's group in statement order, its arguments LAST-FIRST
        // (Ruby's `include`/`prepend` with several modules appends them in
        // reverse, so `include B, A` is `[C, B, A]`). A repeated prepend is a
        // no-op (the first statement wins); a repeated include keeps its
        // first position, as the flat `includes` list always did.
        let groups: Vec<(bool, Vec<String>)> =
            self.idx.ancestry.groups.get(name).cloned().unwrap_or_default();
        let mut prepends: Vec<String> = Vec::new();
        let mut includes: Vec<String> = Vec::new();
        for (prepend, names) in &groups {
            for raw in names.iter().rev() {
                let list = if *prepend { &mut prepends } else { &mut includes };
                if !list.contains(raw) {
                    list.push(raw.clone());
                }
            }
        }
        includes.retain(|raw| !prepends.contains(raw));
        // Every edge the harvest holds must come from a statement the census
        // saw; anything else has no known order.
        if entry
            .includes
            .iter()
            .any(|raw| !prepends.contains(raw) && !includes.contains(raw))
        {
            self.failed = true;
        }
        let mut origin = 0usize;
        for raw in &prepends {
            let sub = self.sub_lin(name, raw);
            // A direct prepend of a module is always inserted, even where the
            // superclass's chain carries it (`[W, C, B, W]`): only the module
            // already in this class's prepend region, or another entry of its
            // own chain the chain already carries, is a fork.
            let module = self
                .idx
                .resolve_override_ancestor(name, raw)
                .map(ChainEntry::Project);
            let in_region = module
                .as_ref()
                .is_some_and(|m| entries[..origin].iter().any(|(x, _)| x == m));
            let others: Vec<Tagged> =
                sub.iter().filter(|(e, _)| Some(e) != module.as_ref()).cloned().collect();
            if in_region || Self::forks(&entries, &others) {
                self.failed = true;
            }
            origin += sub.len();
            entries.splice(0..0, sub);
        }
        for raw in &includes {
            let mut sub = self.sub_lin(name, raw);
            if Self::forks(&entries, &sub) {
                // The one fork the reference settles: an include-side skip
                // on the LAST entry of the sub-chain being inserted (#1570:
                // the module already sits further down the chain).
                let present: Vec<usize> = sub
                    .iter()
                    .enumerate()
                    .filter(|(_, (e, _))| {
                        matches!(e, ChainEntry::Project(_)) && entries.iter().any(|(x, _)| x == e)
                    })
                    .map(|(i, _)| i)
                    .collect();
                let last = sub.len() - 1;
                if self.forks == 0 && present == [last] && !sub.iter().any(|(_, t)| *t) {
                    self.forks = 1;
                    sub[last].1 = true;
                } else {
                    self.failed = true;
                }
            }
            entries.splice(origin + 1..origin + 1, sub);
        }
        if Self::project_count(&entries) > CHAIN_LIMIT {
            self.failed = true;
        }
        self.stack.pop();
        entries
    }

    fn forks(entries: &[Tagged], sub: &[Tagged]) -> bool {
        sub.iter().any(|(e, _)| {
            matches!(e, ChainEntry::Project(_)) && entries.iter().any(|(x, _)| x == e)
        })
    }

    fn sub_lin(&mut self, owner: &str, raw: &str) -> Vec<Tagged> {
        match self.idx.resolve_override_ancestor(owner, raw) {
            Some(p) if self.idx.known_user(&p) => self.lin(&p),
            _ => vec![(
                ChainEntry::External {
                    raw: raw.strip_prefix("::").unwrap_or(raw).to_string(),
                    from: owner.to_string(),
                },
                false,
            )],
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
        let mut b =
            Builder { idx: self, nodes: Vec::new(), stack: Vec::new(), failed: false, forks: 0 };
        let tagged = b.lin(class);
        if b.failed {
            return None;
        }
        let retro = (b.forks == 1).then(|| {
            tagged.iter().filter(|(_, t)| !t).map(|(e, _)| e.clone()).collect()
        });
        let entries = tagged.into_iter().map(|(e, _)| e).collect();
        let marks = b
            .nodes
            .iter()
            .filter_map(|node| self.mark_of(node))
            .collect();
        Some(Chain { entries, retro, marks })
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
        let answer = first_definer(self, core, &chain.entries, class, method, skip_self, &defines)?;
        match &chain.retro {
            None => Some(answer),
            Some(retro) => {
                let other = first_definer(self, core, retro, class, method, skip_self, &defines)?;
                (other == answer).then_some(answer)
            }
        }
    }
}

/// The first definer of `method` on one world of a chain, read from the
/// class's own entry (`skip_self`: the entry after it), or `None` where a
/// definer Ruby may not reach could answer first.
fn first_definer(
    idx: &SourceIndex,
    core: &CoreIndex,
    entries: &[ChainEntry],
    class: &str,
    method: &str,
    skip_self: bool,
    defines: &impl Fn(&str) -> bool,
) -> Option<String> {
    let start = if skip_self {
        entries
            .iter()
            .position(|e| matches!(e, ChainEntry::Project(n) if n == class))?
            + 1
    } else {
        0
    };
    for e in &entries[start..] {
        match e {
            ChainEntry::External { raw, from } => {
                if !external_lacks(core, from, raw, method) {
                    return None;
                }
            }
            ChainEntry::Project(n) => {
                if idx.uncertain_definer(n, method) {
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

/// `external_lacks?` — true only for an ancestor RBS knows whose declaration
/// lacks `method`. The spelling resolves against `from`'s lexical nesting,
/// most qualified first; a name RBS does not know may answer.
/// Upstream v0.4.2 (#1706) — the [`SourceIndex::object_mixin_may_define`]
/// answer for an external entry `raw` written from `from`: the first
/// candidate the bundled RBS declares answers by its method table (a bare
/// call also reaches a module's singleton function, `include Math; sqrt(2)`);
/// an entry no candidate declares is OPAQUE, which answers a bare call and
/// not a call with a receiver (the reference's `EXPLICIT_RECEIVER_SKIP`).
fn external_may_define(core: &CoreIndex, from: &str, raw: &str, method: &str, explicit: bool) -> bool {
    let raw = raw.strip_prefix("::").unwrap_or(raw);
    let segments: Vec<&str> = from.split("::").collect();
    for i in (0..segments.len()).rev() {
        let cand = if i == 0 {
            raw.to_string()
        } else {
            format!("{}::{}", segments[..i].join("::"), raw)
        };
        let known = if core.knows_qualified_class(&cand) {
            Some(core.qualified_class_has_method(&cand, method))
        } else if !cand.contains("::") && core.knows_toplevel_class(&cand) {
            Some(core.class_has_method(&cand, method))
        } else {
            None
        };
        if let Some(has) = known {
            // `class_has_singleton_method` also answers `Class`/`Module`
            // methods the reference's own-declaration test excludes: a
            // superset, so it only declines more.
            return has || (!explicit && core.class_has_singleton_method(&cand, method));
        }
    }
    !explicit
}

impl SourceIndex {
    /// Upstream v0.4.2 (#1701, `Environment::RequiredFeatures`): whether some
    /// analysed file requires the gated vendored `feature` (`"prime"`), which
    /// the reference answers by loading that gem's signatures.
    pub fn requires_vendored_feature(&self, feature: &str) -> bool {
        self.ancestry.required_features.contains(feature)
    }

    /// Upstream v0.4.2 (#1706 / issue #1697; `Inference::ObjectMixins.may_define?`)
    /// — whether a module the project mixes into `Object` may define `method`:
    /// a top-level `include M` in any analysed file (`main.include`, i.e.
    /// `Object.include`), or an `include M` in a project `class Object`, or
    /// `Object`'s own project definitions. `explicit` is the receiver form
    /// (`call.undefined-method`): it counts only a declared definition, while a
    /// bare top-level call (`call.unresolved-toplevel`) is also answered by an
    /// unresolvable module, whose surface cannot be enumerated.
    ///
    /// A pure SILENCER, conservative on every uncertainty: a project module the
    /// port cannot chain, a wildcard or hook-dirty entry, and a private or
    /// singleton-only RBS declaration all answer `true` where the reference may
    /// not, which only declines more.
    pub fn object_mixin_may_define(&self, core: &CoreIndex, method: &str, explicit: bool) -> bool {
        let mut edges: Vec<(&str, &str)> =
            self.ancestry.toplevel_includes.iter().map(|r| ("", r.as_str())).collect();
        if let Some(obj) = self.override_classes.get("Object") {
            edges.extend(obj.includes.iter().map(|r| ("Object", r.as_str())));
            if self.known_user("Object") && self.records_name("Object", method) {
                return true;
            }
        }
        if let Some(listed) = self.ancestry.include_listed.get("Object") {
            edges.extend(listed.iter().map(|r| ("Object", r.as_str())));
        }
        edges.into_iter().any(|(owner, raw)| self.mixin_edge_may_define(core, owner, raw, method, explicit))
    }

    fn mixin_edge_may_define(
        &self,
        core: &CoreIndex,
        owner: &str,
        raw: &str,
        method: &str,
        explicit: bool,
    ) -> bool {
        if raw == MIXIN_WILDCARD {
            return !explicit;
        }
        match self.resolve_override_ancestor(owner, raw) {
            Some(p) if self.known_user(&p) => {
                let Some(chain) = self.instance_chain(&p) else {
                    return true;
                };
                chain.entries.iter().any(|e| match e {
                    ChainEntry::External { raw, from } => {
                        external_may_define(core, from, raw, method, explicit)
                    }
                    ChainEntry::Project(q) => {
                        self.ancestry.wildcard_include(q)
                            || self.ancestry.dirty.contains(q)
                            || self.records_name(q, method)
                    }
                })
            }
            _ => external_may_define(core, owner, raw, method, explicit),
        }
    }
}

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
