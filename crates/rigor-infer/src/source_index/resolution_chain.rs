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
    /// Upstream v0.4.6 (#2264): the first segment of every feature some
    /// analysed file requires ([`AncestryFacts::required_segments`]).
    required_segments: HashSet<String>,
    /// Upstream v0.4.6 (#2269): per owner candidate, its singleton-side
    /// mixins ([`AncestryFacts::singleton_mixins`]), the wildcard included.
    singleton_listed: HashMap<String, Vec<String>>,
    /// Upstream v0.4.6 (#2094): `(owner, name)` macro-named slots
    /// ([`AncestryFacts::macro_names`]).
    macro_names: HashSet<(String, String)>,
    /// Upstream v0.4.5 (#2086): per owner candidate, its receiver-form
    /// instance mixins ([`AncestryFacts::receiver_mixins`]).
    receiver_listed: HashMap<String, Vec<String>>,
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
        self.required_segments.extend(facts.required_segments.iter().cloned());
        self.macro_names.extend(facts.macro_names.iter().cloned());
        for (owners, names) in &facts.singleton_mixins {
            for o in owners {
                let list = self.singleton_listed.entry(o.clone()).or_default();
                for n in names {
                    if !list.contains(n) {
                        list.push(n.clone());
                    }
                }
            }
        }
        for raw in &facts.toplevel_includes {
            if !self.toplevel_includes.contains(raw) {
                self.toplevel_includes.push(raw.clone());
            }
        }
        for (owners, names) in &facts.receiver_mixins {
            for o in owners {
                let list = self.receiver_listed.entry(o.clone()).or_default();
                for n in names {
                    if !list.contains(n) {
                        list.push(n.clone());
                    }
                }
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
            || self.contested_slot(q, method)
            || self.owner_defines(q, method, DefKind::Instance)
            || self.owner_defines(q, method, DefKind::Singleton)
            || self.discovered_methods.get(q).is_some_and(|m| m.contains(method))
    }

    /// Whether `owner`'s `method` rests on a fact the port cannot vouch for:
    /// a `possible` def or a contested visibility slot.
    fn uncertain_definer(&self, owner: &str, method: &str) -> bool {
        let key = (owner.to_string(), method.to_string());
        self.ancestry.possible.contains(&key) || self.contested_slot(owner, method)
    }

    /// Whether `owner`'s `method` slot is contested, by name or by the
    /// wildcard a `private(*names)` call writes (v0.4.4 `contest_class`).
    fn contested_slot(&self, owner: &str, method: &str) -> bool {
        let contested = |name: &str| {
            self.ancestry.contested.contains(&(owner.to_string(), name.to_string()))
        };
        contested(method) || contested(MIXIN_WILDCARD)
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

    /// Upstream v0.4.6 (#2264 / issue #2174, `RbsLoader#ruby_library_reopen?`)
    /// — whether `class_name`'s every RBS declaration is the project's own,
    /// its first `::` segment is the root of a library Ruby ships, and some
    /// analysed file requires a feature that loads that root. Such a project
    /// declaration reopens a library class and lists only the methods the
    /// project added, so the reference treats the class's surface as open.
    /// It does NOT ask whether the library's own signatures loaded: a
    /// project-only `Digest::MakerZzz` declines beside `require "digest"`
    /// while `Digest::SHA256` (bundled RBS) keeps firing.
    ///
    /// "Every declaration is the project's own" is read as "introduced by
    /// project `sig/` ingestion" (`is_qualified_project_sig_class`), which also
    /// counts an rbs-collection-only class: a superset, so it only declines
    /// more.
    pub fn ruby_library_reopen(&self, core: &CoreIndex, class_name: &str) -> bool {
        let name = class_name.strip_prefix("::").unwrap_or(class_name);
        if self.ancestry.required_segments.is_empty() || !core.is_qualified_project_sig_class(name) {
            return false;
        }
        let root = name.split("::").next().unwrap_or(name);
        super::ruby_library_roots::segments_of(root)
            .is_some_and(|segs| segs.iter().any(|s| self.ancestry.required_segments.contains(*s)))
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
        // The question is a union over the module's ancestry, not "which
        // definer does Ruby call", so the closure is walked directly: an order
        // the port cannot vouch for (a fork, a multi-file mark) does not matter
        // here, and declining on it silenced every receiver in a project whose
        // included module merely had one (concurrent-ruby's `Concurrent`).
        let mut queue: Vec<(String, String)> = vec![(owner.to_string(), raw.to_string())];
        let mut seen: HashSet<String> = HashSet::new();
        while let Some((from, raw)) = queue.pop() {
            if raw == MIXIN_WILDCARD {
                if !explicit {
                    return true;
                }
                continue;
            }
            match self.resolve_override_ancestor(&from, &raw) {
                Some(q) if self.known_user(&q) => {
                    if !seen.insert(q.clone()) {
                        continue;
                    }
                    // `ResolutionChain::LIMIT`: a cut chain is uncertainty.
                    if seen.len() > CHAIN_LIMIT {
                        return true;
                    }
                    // A wildcard edge (an include the walk cannot name) or a
                    // hook may hide a definer; only a bare call counts that,
                    // as the reference's chain lists neither as an entry and
                    // a receiver call skips every unknown module (an
                    // `include('x')` matcher in a spec's `module Concurrent`
                    // body would otherwise silence the whole project).
                    if (!explicit
                        && (self.ancestry.wildcard_include(&q) || self.ancestry.dirty.contains(&q)))
                        || self.records_name(&q, method)
                        || self.macro_named(&q, method)
                    {
                        return true;
                    }
                    let mut edges: Vec<String> = self
                        .override_classes
                        .get(&q)
                        .map(|e| e.includes.clone())
                        .unwrap_or_default();
                    if let Some(listed) = self.ancestry.include_listed.get(&q) {
                        edges.extend(listed.iter().cloned());
                    }
                    for (_, names) in self.ancestry.groups.get(&q).into_iter().flatten() {
                        edges.extend(names.iter().cloned());
                    }
                    for e in edges {
                        queue.push((q.clone(), e));
                    }
                }
                other => {
                    // #2094: a declared module's class-body macro may name it.
                    if other.is_some_and(|q| self.macro_named(&q, method))
                        || external_may_define(core, &from, &raw, method, explicit)
                    {
                        return true;
                    }
                }
            }
        }
        false
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

/// `RbsReceiverSurface::MISSING_HOOK`.
const MISSING_HOOK: &str = "method_missing";
/// `RbsReceiverSurface::OBJECT_ROOTS` — the roots where only a module that
/// defines the name answers (#1697's reading), never an open-surface one.
const OBJECT_ROOTS: &[&str] = &["Object", "Kernel", "BasicObject"];
/// The instance roots every class object is an instance of (the reference's
/// `singleton_ancestors_for` tail).
const CLASS_OBJECT_INSTANCE_ROOTS: &[&str] = &["Class", "Module", "Object", "Kernel", "BasicObject"];
/// `SingletonHookDecline::CONCERN`.
const CONCERN: &str = "ActiveSupport::Concern";

/// The RBS ancestors of `class_name`, the class itself first; an incomplete
/// chain reads as the class alone, as the reference's empty answer does.
fn rbs_ancestors(core: &CoreIndex, class_name: &str) -> Vec<String> {
    let mut names: Vec<String> = core
        .ancestor_names(class_name)
        .map(|v| v.into_iter().map(str::to_string).collect())
        .unwrap_or_default();
    if !names.iter().any(|n| n == class_name) {
        names.insert(0, class_name.to_string());
    }
    names
}

/// Whether some lexical candidate of `raw` (written from `from`) is a class or
/// module the bundled RBS declares.
fn rbs_knows_candidate(core: &CoreIndex, from: &str, raw: &str) -> bool {
    let raw = raw.strip_prefix("::").unwrap_or(raw);
    let segments: Vec<&str> = from.split("::").collect();
    (0..segments.len()).rev().any(|i| {
        let cand = if i == 0 {
            raw.to_string()
        } else {
            format!("{}::{}", segments[..i].join("::"), raw)
        };
        core.knows_qualified_class(&cand) || (!cand.contains("::") && core.knows_toplevel_class(&cand))
    })
}

impl SourceIndex {
    /// The reference's `project_defines?` for the instance side, read from
    /// every table the port holds, across files: a `def` (recorded or
    /// `possible`), a visibility slot, or a discovered method.
    fn instance_records(&self, q: &str, method: &str) -> bool {
        let key = (q.to_string(), method.to_string());
        self.override_classes.get(q).is_some_and(|e| {
            e.methods.contains(method) || e.method_visibilities.contains_key(method)
        }) || self.ancestry.possible.contains(&key)
            || self.owner_defines(q, method, DefKind::Instance)
            || self.project_declares_method(None, q, method)
    }

    /// Every instance mixin edge of `q` the port holds, as `(from, raw)`:
    /// direct statements, listed (unpositioned) edges and receiver-form ones.
    fn instance_mixin_edges(&self, q: &str) -> Vec<(String, String)> {
        let mut edges: Vec<(String, String)> = Vec::new();
        if let Some(e) = self.override_classes.get(q) {
            edges.extend(e.includes.iter().map(|r| (q.to_string(), r.clone())));
        }
        if let Some(listed) = self.ancestry.include_listed.get(q) {
            edges.extend(listed.iter().map(|r| (q.to_string(), r.clone())));
        }
        for (_, names) in self.ancestry.groups.get(q).into_iter().flatten() {
            edges.extend(names.iter().map(|r| (q.to_string(), r.clone())));
        }
        if let Some(listed) = self.ancestry.receiver_listed.get(q) {
            edges.extend(listed.iter().map(|r| (q.to_string(), r.clone())));
        }
        edges
    }

    /// `SingletonHookDecline.entry_hook_capable?` for a project module: it
    /// records a hook (or a `method_missing`), lists the wildcard on either
    /// side, or extends `ActiveSupport::Concern`.
    fn hook_capable(&self, q: &str) -> bool {
        self.ancestry.dirty.contains(q)
            || self.ancestry.wildcard_include(q)
            || self
                .ancestry
                .extend_listed
                .get(q)
                .is_some_and(|l| l.iter().any(|n| n == MIXIN_WILDCARD))
            || self.override_classes.get(q).is_some_and(|e| {
                e.extends.iter().any(|x| x.strip_prefix("::").unwrap_or(x) == CONCERN)
            })
    }

    /// `chain_answers?` over the mixin closure of the RBS ancestor `a` (its
    /// own wildcard taint is no entry of the reference's chain): a project
    /// module that defines `method` or a `method_missing`, an open-surface
    /// module when `open`, or an external RBS declares `method` on (or, when
    /// `open`, one RBS does not know). `hook_only` is `hook_on_chain?`: only
    /// a project `method_missing` answers. The closure is walked as a union,
    /// so an order the port cannot vouch for does not matter.
    fn mixin_closure_may_answer(
        &self,
        core: &CoreIndex,
        a: &str,
        method: &str,
        open: bool,
        hook_only: bool,
    ) -> bool {
        let mut queue: Vec<(String, String)> = self
            .instance_mixin_edges(a)
            .into_iter()
            .filter(|(_, raw)| raw != MIXIN_WILDCARD)
            .collect();
        let mut seen: HashSet<String> = HashSet::new();
        while let Some((from, raw)) = queue.pop() {
            if raw == MIXIN_WILDCARD {
                if open && !hook_only {
                    return true;
                }
                continue;
            }
            match self.resolve_override_ancestor(&from, &raw) {
                Some(q) if self.known_user(&q) => {
                    if !seen.insert(q.clone()) {
                        continue;
                    }
                    if seen.len() > CHAIN_LIMIT {
                        return true;
                    }
                    if self.instance_records(&q, MISSING_HOOK) {
                        return true;
                    }
                    if !hook_only {
                        let rbs_known = core.knows_qualified_class(&q)
                            || (!q.contains("::") && core.knows_toplevel_class(&q));
                        let answers = if rbs_known {
                            // ADR-17: a cross-file `def` on an RBS-declared
                            // module stays the monkey-patch report.
                            self.discovered_methods.get(&q).is_some_and(|m| m.contains(method))
                                || core.qualified_class_has_method(&q, method)
                        } else {
                            self.instance_records(&q, method)
                                || self.macro_named(&q, method)
                                || (open && (self.hook_capable(&q) || self.open_extend(core, &q)))
                        };
                        if answers {
                            return true;
                        }
                    }
                    queue.extend(self.instance_mixin_edges(&q));
                }
                // A module the project declares with nothing in it answers
                // only through RBS.
                Some(q) => {
                    if !hook_only
                        && (((core.knows_qualified_class(&q) || core.knows_toplevel_class(&q))
                            && core.qualified_class_has_method(&q, method))
                            // #2094: `external_may_answer?`'s `macro_names?` /
                            // `open_extend?` arms for a declared candidate.
                            || self.macro_named(&q, method)
                            || (open && self.open_extend(core, &q)))
                    {
                        return true;
                    }
                }
                None => {
                    if !hook_only
                        && (external_may_define(core, &from, &raw, method, true)
                            || (open && !rbs_knows_candidate(core, &from, &raw)))
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Upstream v0.4.6 (#2094, `ObjectMixins.macro_named?`).
    fn macro_named(&self, q: &str, method: &str) -> bool {
        self.ancestry.macro_names.contains(&(q.to_string(), method.to_string()))
    }

    /// Every singleton-side mixin spelling of `q` the port holds: the
    /// singleton table (wildcard included), the listed `extend`s and the
    /// harvested class-body `extend`s.
    fn singleton_raws(&self, q: &str) -> Vec<String> {
        let mut out: Vec<String> = self.ancestry.singleton_listed.get(q).cloned().unwrap_or_default();
        let listed = self.ancestry.extend_listed.get(q).into_iter().flatten();
        let harvested = self.override_classes.get(q).into_iter().flat_map(|e| e.extends.iter());
        for raw in listed.chain(harvested) {
            if raw != MIXIN_WILDCARD && !out.contains(raw) {
                out.push(raw.clone());
            }
        }
        out
    }

    /// `chain.truncated?` of `q`'s singleton chain: its extended modules and
    /// their include closures hold more than [`CHAIN_LIMIT`] project modules.
    fn singleton_truncated(&self, q: &str) -> bool {
        let mut queue: Vec<(String, String)> = self
            .singleton_raws(q)
            .into_iter()
            .filter(|r| r != MIXIN_WILDCARD)
            .map(|r| (q.to_string(), r))
            .collect();
        let mut seen: HashSet<String> = HashSet::new();
        while let Some((from, raw)) = queue.pop() {
            if raw == MIXIN_WILDCARD {
                continue;
            }
            if let Some(m) = self.resolve_override_ancestor(&from, &raw) {
                if !seen.insert(m.clone()) {
                    continue;
                }
                if seen.len() > CHAIN_LIMIT {
                    return true;
                }
                queue.extend(self.instance_mixin_edges(&m));
            }
        }
        false
    }

    /// `SingletonHookDecline.entry_hook_capable?` for an entry written `raw`
    /// from `from`: a declared project module by [`Self::hook_capable`], an
    /// undeclared one when no candidate is a class RBS knows.
    fn entry_hook_capable(&self, core: &CoreIndex, from: &str, raw: &str) -> bool {
        match self.resolve_override_ancestor(from, raw) {
            Some(q) => self.hook_capable(&q),
            None => !rbs_knows_candidate(core, from, raw),
        }
    }

    /// Upstream v0.4.6 (#2094, `RbsReceiverSurface.open_extend?`) — `owner`
    /// extends a module whose surface is open (hook-capable, or one neither
    /// RBS nor the project declares), or its singleton chain was cut.
    fn open_extend(&self, core: &CoreIndex, owner: &str) -> bool {
        self.singleton_truncated(owner)
            || self
                .singleton_raws(owner)
                .iter()
                .filter(|r| *r != MIXIN_WILDCARD)
                .any(|raw| self.entry_hook_capable(core, owner, raw))
    }

    /// `external_may_answer?` for an entry written `raw` from `from`.
    fn external_may_answer(&self, core: &CoreIndex, from: &str, raw: &str, method: &str, open: bool) -> bool {
        if external_may_define(core, from, raw, method, true) {
            return true;
        }
        if let Some(q) = self.resolve_override_ancestor(from, raw) {
            return self.macro_named(&q, method) || (open && self.open_extend(core, &q));
        }
        open && !rbs_knows_candidate(core, from, raw)
    }

    /// Upstream v0.4.6 (#2269 / #2094, `RbsReceiverSurface.singleton_mixin_may_answer?`)
    /// — whether a singleton-side mixin of the class object `class_name` or
    /// of an RBS superclass may answer `method`: the side carries a wildcard
    /// (an unrecordable spelling, e.g. `IO.singleton_class.send(:define_method,
    /// …)`, which silences `File.zzz`) outside `Object` / `Kernel` /
    /// `BasicObject`, its chain was cut, or an extended module defines the
    /// name, macro-names it, or is open. Every extended module's include
    /// closure is not read past the cut test: the reference reads level 0.
    /// A pure SILENCER.
    pub fn core_singleton_mixin_may_answer(&self, core: &CoreIndex, class_name: &str, method: &str) -> bool {
        rbs_ancestors(core, class_name)
            .iter()
            .filter(|a| a.as_str() == class_name || !core.is_module(a))
            .any(|n| {
                let open = !OBJECT_ROOTS.contains(&n.as_str());
                let raws = self.singleton_raws(n);
                if self.singleton_truncated(n) || (open && raws.iter().any(|r| r == MIXIN_WILDCARD)) {
                    return true;
                }
                raws.iter().filter(|r| *r != MIXIN_WILDCARD).any(|raw| {
                    match self.resolve_override_ancestor(n, raw) {
                        Some(q) if self.known_user(&q) => {
                            if &q == n {
                                return false;
                            }
                            let rbs_known = core.knows_qualified_class(&q)
                                || (!q.contains("::") && core.knows_toplevel_class(&q));
                            if rbs_known {
                                self.discovered_methods.get(&q).is_some_and(|m| m.contains(method))
                                    || core.qualified_class_has_method(&q, method)
                            } else {
                                self.instance_records(&q, method)
                                    || self.macro_named(&q, method)
                                    || (open && (self.hook_capable(&q) || self.open_extend(core, &q)))
                            }
                        }
                        _ => self.external_may_answer(core, n, raw, method, open),
                    }
                })
            })
    }

    /// Upstream v0.4.5 (#2086 / issue #1722, `RbsReceiverSurface.ancestor_may_answer?`)
    /// — whether the project may answer `method` on an instance of the RBS
    /// class `class_name` through one of its RBS ancestors: a `method_missing`
    /// on the ancestor, or a project mixin of it (`Integer.include(Ext)`
    /// wherever written, `module Comparable; include Ext`). A pure SILENCER.
    pub fn core_ancestor_may_answer(&self, core: &CoreIndex, class_name: &str, method: &str) -> bool {
        rbs_ancestors(core, class_name).iter().any(|a| {
            self.instance_records(a, MISSING_HOOK)
                || self.mixin_closure_may_answer(core, a, method, !OBJECT_ROOTS.contains(&a.as_str()), false)
        })
    }

    /// Upstream v0.4.5 (#2086, `RbsReceiverSurface.singleton_hook?`) — whether
    /// the class object `class_name` may reach a project `method_missing`: a
    /// `def self.method_missing` on the class or an RBS ancestor, or an
    /// instance `method_missing` on `Class`, `Module`, `Object`, `Kernel` or
    /// `BasicObject` (or a project module mixed into one). A superset: every
    /// RBS ancestor's singleton side is read, modules included.
    pub fn core_singleton_hook(&self, core: &CoreIndex, class_name: &str) -> bool {
        rbs_ancestors(core, class_name)
            .iter()
            .any(|a| self.owner_defines(a, MISSING_HOOK, DefKind::Singleton))
            || CLASS_OBJECT_INSTANCE_ROOTS.iter().any(|r| {
                self.instance_records(r, MISSING_HOOK)
                    || self.mixin_closure_may_answer(core, r, MISSING_HOOK, false, true)
            })
    }

    /// Upstream v0.4.5 (#2086, `RbsReceiverSurface.subclass_may_define?`) —
    /// whether a project subclass of `class_name`, at any depth, defines
    /// `method` (through its own ancestry) or a `method_missing`. A parent is
    /// read as written and resolved the way the class header resolves it:
    /// the first project class the spelling can denote, else the top-level
    /// name.
    pub fn project_subclass_may_define(&self, class_name: &str, method: &str) -> bool {
        let mut children: HashMap<String, Vec<&str>> = HashMap::new();
        for (child, entry) in &self.override_classes {
            let Some(raw) = entry.superclass.as_deref() else { continue };
            let raw = raw.strip_prefix("::").unwrap_or(raw);
            let segments: Vec<&str> = child.split("::").collect();
            let parent = (0..segments.len())
                .rev()
                .map(|i| {
                    if i == 0 {
                        raw.to_string()
                    } else {
                        format!("{}::{}", segments[..i].join("::"), raw)
                    }
                })
                .find(|c| self.known_user(c))
                .unwrap_or_else(|| raw.to_string());
            children.entry(parent).or_default().push(child.as_str());
        }
        let mut queue: Vec<&str> = children.get(class_name).cloned().unwrap_or_default();
        let mut seen: HashSet<&str> = HashSet::new();
        while let Some(sub) = queue.pop() {
            if !seen.insert(sub) {
                continue;
            }
            if self.instance_records(sub, method)
                || self.instance_records(sub, MISSING_HOOK)
                || self.project_declares_method_through_ancestors(None, sub, method)
            {
                return true;
            }
            queue.extend(children.get(sub).into_iter().flatten().copied());
        }
        false
    }
}
