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
use rigor_parse::{AncestryFacts, ConstWrite, MixinSide, MIXIN_UNNAMED, MIXIN_WILDCARD};

/// `?` ([`MIXIN_UNNAMED`]) rides beside `*` ([`MIXIN_WILDCARD`]) in every list
/// the census writes — `UNNAMED_LIST` is `["*", "?"]` — so a name-reader that
/// treated `?` as a resolvable name would answer wrongly wherever the
/// reference's `includes_of` / `discovered_extends` hold no such name. Every
/// marker check reads both.
fn mixin_marker(n: &str) -> bool {
    n == MIXIN_WILDCARD || n == MIXIN_UNNAMED
}

use super::{qualify, DefKind, SourceIndex};

/// `ResolutionChain::LIMIT` — the project entries a chain may hold.
const CHAIN_LIMIT: usize = 100;

/// The method-independent half of `object_mixin_may_define` (rigor-rs#464):
/// `Object`'s mixin closure collected once per index instead of re-walked per
/// diagnostic. Every member below is a SET the per-edge walks of the removed
/// `mixin_edge_may_define` would have unioned over — the per-method questions
/// (`records_name`, `macro_named`, `external_may_define`) are asked per call.
#[derive(Default)]
pub(crate) struct ObjectMixins {
    /// A walk's per-owner `seen` passed [`CHAIN_LIMIT`]: uncertainty — every
    /// method may be defined, unconditionally.
    cut: bool,
    /// Some edge spells no name (`include m`, `include(*MODS)`): an unknown
    /// surface. Answers a bare call only.
    unnamed: bool,
    /// Some reached module carries a wildcard include or a hook-dirty edge.
    /// Answers a bare call only.
    tainted: bool,
    /// `known_user("Object")` — `Object`'s own `records_name` is asked per
    /// method.
    object_records: bool,
    /// The reachable `known_user` project modules, deduped across seeds.
    modules: Vec<String>,
    module_set: HashSet<String>,
    /// Edges resolving to nothing or to a non-`known_user` project module:
    /// `(from, raw, declared)`. Each asks `macro_named` on `declared` and
    /// `external_may_define` on `(from, raw)` per method.
    opaque: Vec<(String, String, Option<String>)>,
    opaque_set: HashSet<(String, String)>,
}

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
    /// Per owner, EVERY recorded instance mixin edge in write order —
    /// [`AncestryFacts::written_mixins`]'s fold: positioned statements,
    /// unpositioned named edges and receiver-form edges interleaved, the
    /// `MixinAccumulator` list `discovered_includes` freezes.
    written: HashMap<String, Vec<WrittenEdge>>,
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
    /// Upstream v0.4.8 (#2991): owners defining an `extend`-triggered hook
    /// ([`AncestryFacts::extend_hook_owners`]) — `extend_reshapes_instance?`'s
    /// `defines_any?` half.
    extend_hook_owners: HashSet<String>,
}

/// One [`AncestryIndex::written`] edge, filed under each of its owner
/// candidates at ingest. `owners` survives the fold so the query can still
/// resolve an eval-owner edge (`X.class_eval { include M }`): its
/// `resolved_write_namespace` oracle asks the project's known namespaces,
/// which ingest does not yet see whole. An already-resolved edge — a
/// declaration-frame `self` owner or a receiver-form call the ingest
/// anchored — carries one name.
struct WrittenEdge {
    owners: Vec<String>,
    prepend: bool,
    names: Vec<String>,
}

/// `add_declared_name` — every enclosing prefix of a declared name joins the
/// oracle: a file declaring `S::A::B` necessarily has `S` and `S::A` to
/// declare it under.
fn expand_declared_name(set: &mut HashSet<String>, name: &str) {
    let mut prefix = String::new();
    for seg in name.split("::") {
        if !prefix.is_empty() {
            prefix.push_str("::");
        }
        prefix.push_str(seg);
        set.insert(prefix.clone());
    }
}

/// The `entry::head` probe `eval_constant_receiver_prefix` makes of the
/// file's declared names: `candidate` minus the receiver path's `tail`
/// segments — `NS::A::B` for `A::B.include(M)` probes `NS::A`.
fn head_probe(candidate: &str, tail: usize) -> &str {
    let mut s = candidate;
    for _ in 0..tail {
        s = s.rsplit_once("::").map(|(p, _)| p).unwrap_or(s);
    }
    s
}

impl AncestryIndex {
    pub(crate) fn ingest<'a>(
        &mut self,
        facts: &AncestryFacts,
        declared: impl Iterator<Item = &'a str>,
        writes: &[ConstWrite],
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
        self.extend_hook_owners.extend(facts.extend_hook_owners.iter().cloned());
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
        // `eval_file_declared_names` — the oracle `eval_constant_receiver_prefix`
        // resolves a receiver-form owner against: every class/module
        // declaration plus constant write THE FILE contains, expanded to
        // every enclosing prefix. `declared_constant_write_prefix` files a
        // path write as written (`S2::C = 2` inside `class S` keys `S2::C`)
        // and a bare write under its lexical prefix.
        let mut file_declared: HashSet<String> = HashSet::new();
        let declared: Vec<&str> = declared.collect();
        for q in &declared {
            expand_declared_name(&mut file_declared, q);
        }
        for w in writes {
            let key = if w.is_path {
                w.name.clone()
            } else {
                qualify(&w.prefix, &w.name)
            };
            expand_declared_name(&mut file_declared, &key);
        }
        for m in &facts.written_mixins {
            if m.receiver_form {
                // `eval_constant_receiver_prefix`: the receiver resolves to
                // the first lexical candidate this FILE declares — the probe
                // is `entry::head` (the path's head segment only) — else the
                // bare spelling.
                let bare = m.owners.last().expect("receiver candidates");
                let tail = bare.split("::").count() - 1;
                let resolved = m
                    .owners
                    .iter()
                    .find(|o| file_declared.contains(head_probe(o, tail)))
                    .unwrap_or(bare);
                self.written.entry(resolved.clone()).or_default().push(WrittenEdge {
                    owners: vec![resolved.clone()],
                    prepend: m.prepend,
                    names: m.names.clone(),
                });
            } else {
                for o in &m.owners {
                    self.written.entry(o.clone()).or_default().push(WrittenEdge {
                        owners: m.owners.clone(),
                        prepend: m.prepend,
                        names: m.names.clone(),
                    });
                }
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
    /// The chain's flavor: `false` is the methods walks' `:methods` (project
    /// entries are `known_user` classes); `true` is the constant ladder's
    /// `:constants` (project entries are the namespaces
    /// [`SourceIndex::resolve_constant_scope`] resolves — the reference's
    /// `known_project_namespace?`).
    constants: bool,
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
            let module = if self.constants {
                self.idx
                    .resolve_constant_scope(name, raw)
                    .map(ChainEntry::Project)
            } else {
                self.idx
                    .resolve_override_ancestor(name, raw)
                    .map(ChainEntry::Project)
            };
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
                // The one fork the reference's `include_all` settles: an
                // include-side skip on the LAST entry of the sub-chain being
                // inserted, its earlier occurrence sitting AT OR AFTER the
                // class itself — `found >= origin && index == sub.size - 1`
                // (#1570). A duplicate inside the prepend region
                // (`found < origin`) is a fork the retro world cannot
                // repeat: the chain settles to master.
                let last = sub.len() - 1;
                let present: Vec<(usize, usize)> = sub
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (e, _))| {
                        if matches!(e, ChainEntry::Project(_)) {
                            entries.iter().position(|(x, _)| x == e).map(|p| (i, p))
                        } else {
                            None
                        }
                    })
                    .collect();
                if self.forks == 0
                    && present.len() == 1
                    && present[0].0 == last
                    && present[0].1 >= origin
                    && !sub.iter().any(|(_, t)| *t)
                {
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
        let resolved = if self.constants {
            self.idx.resolve_constant_scope(owner, raw)
        } else {
            self.idx
                .resolve_override_ancestor(owner, raw)
                .filter(|p| self.idx.known_user(p))
        };
        match resolved {
            Some(p) => self.lin(&p),
            None => vec![(
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
        let mut b = Builder {
            idx: self,
            nodes: Vec::new(),
            stack: Vec::new(),
            failed: false,
            forks: 0,
            constants: false,
        };
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

    /// `ResolutionChain::Builder#mark` — the mark a node carries on its
    /// instance side: the names `include`/`prepend` wrote (the `["*", "?"]`
    /// markers among them), plus [`SourceIndex::extend_hook_taint`]. Neither
    /// marker counts toward the `multi_file` edge count (the reference's
    /// `!multi_file` reads `includes_of` — names — against `*`).
    fn mark_of(&self, node: &str) -> Option<Mark> {
        let mut listed = self.ancestry.include_listed.get(node).cloned().unwrap_or_default();
        let mut taint = self.extend_hook_taint(node);
        let edges = self.override_classes.get(node).map_or(0, |e| e.includes.len())
            + listed
                .iter()
                .filter(|n| *n != MIXIN_WILDCARD && *n != MIXIN_UNNAMED)
                .count();
        let multi_file =
            edges >= 2 && self.ancestry.decl_files.get(node).copied().unwrap_or(0) >= 2;
        if listed.is_empty() && taint.is_empty() && !multi_file {
            return None;
        }
        listed.append(&mut taint);
        Some(Mark { node: node.to_string(), listed, multi_file })
    }

    /// Upstream v0.4.8 (#2991/#3006, `DiscoveryIndex#extend_hook_taint` +
    /// `extend_reshapes_instance?`): a module `name` extends whose hook may
    /// mix modules into the extender's instance side — its mark also lists
    /// `["*", "?"]` when such a module's own include side names a module the
    /// walk cannot name, `["*"]` when it only taints order, nothing when no
    /// extended module reshapes.
    fn extend_hook_taint(&self, name: &str) -> Vec<String> {
        let mut found = false;
        for raw in self.singleton_raws(name) {
            let Some(resolved) = self.resolve_override_ancestor(name, &raw) else {
                continue;
            };
            if !self.extend_reshapes_instance(&resolved) {
                continue;
            }
            if self
                .ancestry
                .include_listed
                .get(&resolved)
                .is_some_and(|l| l.iter().any(|n| n == MIXIN_UNNAMED))
            {
                return vec![MIXIN_WILDCARD.to_string(), MIXIN_UNNAMED.to_string()];
            }
            found = true;
        }
        if found { vec![MIXIN_WILDCARD.to_string()] } else { Vec::new() }
    }

    /// `extend_reshapes_instance?` — a `*` on the module's own include side
    /// plus a hook `extend` runs on the extended module (`extended` /
    /// `extend_object` on its singleton, `included` / `append_features` /
    /// `prepended` / `prepend_features` / `inherited` on its instances — the
    /// census's [`AncestryFacts::extend_hook_owners`]).
    fn extend_reshapes_instance(&self, resolved: &str) -> bool {
        self.ancestry.wildcard_include(resolved)
            && self.ancestry.extend_hook_owners.contains(resolved)
    }

    /// Upstream v0.4.8 (#2991/#3006, `ResolutionChain#unnamed_mixin?` +
    /// `DiscoveryIndex.unnamed_mixin_on_chain?`): whether a class on
    /// `class_name`'s instance-side chain lists a mixin no name spells — a
    /// computed or unrecorded `include` (`include(*MODS)`, `include m`,
    /// `include M, h`, a computed `send` that may be a mixin, a hook's
    /// `base.include M`) opens the class: the unnamed module may define any
    /// name, so a `def`-side reader declines. The marks are the ones the
    /// chain build reaches — a chain the port cannot settle keeps the marks
    /// it gathered, as the reference's own build does.
    pub fn unnamed_mixin_on_chain(&self, class_name: &str) -> bool {
        self.chain_marks(class_name)
            .iter()
            .any(|m| m.listed.iter().any(|n| n == MIXIN_UNNAMED))
    }

    /// `ResolutionChain::Builder#marks` over `class`'s chain — collected even
    /// for a chain the build cannot settle (`unnamed_mixin?` reads them).
    fn chain_marks(&self, class: &str) -> Vec<Mark> {
        if !self.override_classes.contains_key(class) {
            // A class with no project declaration still carries the marks its
            // receiver-form edges wrote (`X.include(*M)` on a sig-only class).
            return self.mark_of(class).into_iter().collect();
        }
        let mut b = Builder {
            idx: self,
            nodes: Vec::new(),
            stack: Vec::new(),
            failed: false,
            forks: 0,
            constants: false,
        };
        let _ = b.lin(class);
        b.nodes.iter().filter_map(|n| self.mark_of(n)).collect()
    }

    /// rigor-rs#905 — the instance-side chain under the `:constants` flavor
    /// ([`Builder::constants`]), paired with whether the chain carries an
    /// ORDER mark, or `None` where the port cannot vouch for Ruby's order.
    /// Unlike [`Self::instance_chain`] the ROOT need not be a `known_user`
    /// class: `constant_for_name`'s step 2 builds it for the innermost
    /// lexical scope of a constant read, which the project may hold only as
    /// a `module` whose own edges all resolve elsewhere (or not at all —
    /// [`Builder::lin`] on an undiscovered name contributes the name alone,
    /// which [`Self::constant_scope_names`] drops as `class` itself).
    fn constant_chain(&self, class: &str) -> Option<(Chain, bool)> {
        let mut b = Builder {
            idx: self,
            nodes: Vec::new(),
            stack: Vec::new(),
            failed: false,
            forks: 0,
            constants: true,
        };
        let tagged = b.lin(class);
        if b.failed {
            return None;
        }
        let retro = (b.forks == 1).then(|| {
            tagged.iter().filter(|(_, t)| !t).map(|(e, _)| e.clone()).collect()
        });
        let entries = tagged.into_iter().map(|(e, _)| e).collect();
        // `order_marks` — the marks a constants reader settles on. Only the
        // include-kind marks count: the `extend_hook` taint [`Self::mark_of`]
        // also folds in is a HOOK_MARK_KIND the reference's `order_marks`
        // rejects, and reading it as unsettled would settle a chain the
        // reference stands to master's different order.
        let order_marked = b.nodes.iter().any(|n| self.constant_order_mark(n));
        Some((Chain { entries, retro, marks: Vec::new() }, order_marked))
    }

    /// `ResolutionChain`'s `:include`-kind order mark on `node`
    /// (`mark_unsettled`): an unpositioned include-side mixin edge (a
    /// conditional-, method-, block- or hook-written `include`, or one no
    /// table records, `"*"`/`"?"` among the listed), or a mixin edge set of
    /// two or more on a class declared in several files, whose order across
    /// the files is load order. The `extend_hook` mark kind is deliberately
    /// NOT here — `order_marks` excludes it, so a hook-tainted node still
    /// settles its constants chain.
    fn constant_order_mark(&self, node: &str) -> bool {
        self.ancestry
            .include_listed
            .get(node)
            .is_some_and(|l| !l.is_empty())
            || (self
                .override_classes
                .get(node)
                .map_or(0, |e| e.includes.len())
                >= 2
                && self.ancestry.decl_files.get(node).copied().unwrap_or(0) >= 2)
    }

    /// `constant_scopes_of`: the chain's project entries in Ruby's order,
    /// each once, without `class` itself (the step-1 rungs cover it). An
    /// external entry — a mixin the project does not declare — yields no
    /// name here: the reference's `external_constant_owner` RBS arm
    /// (`include AcLibraryRb` putting `AcLibraryRb::Segtree` on the walk,
    /// upstream #1698) is a residual, and its `project_written_candidate?`
    /// stop lands for free since a written-only spelling resolves to no
    /// project entry and contributes nothing either way.
    fn constant_scope_names(entries: &[ChainEntry], class: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for entry in entries {
            let ChainEntry::Project(name) = entry else {
                continue;
            };
            if name != class && !out.contains(name) {
                out.push(name.clone());
            }
        }
        out
    }

    /// rigor-rs#905 — the worlds `constant_for_name`'s ancestor rung
    /// consults, `agreed_ancestor_hit`'s shape: the CHAIN's scope order
    /// where the port can vouch for it (no order mark — an unpositioned
    /// mixin edge, a multi-file edge set — and no failed fork/cycle/cut),
    /// plus the retro world's scopes when the chain's one include-side
    /// skip needs settling; [`ConstantScopeWorlds::Master`] —
    /// `MasterOrder.breadth_first`'s order — everywhere else, exactly as
    /// `chain.settle` answers `:master` there.
    pub(crate) fn constant_scope_worlds(&self, class: &str) -> ConstantScopeWorlds {
        match self.constant_chain(class) {
            Some((chain, order_marked)) if !order_marked => {
                ConstantScopeWorlds::Settled {
                    scopes: Self::constant_scope_names(&chain.entries, class),
                    retro: chain
                        .retro
                        .map(|entries| Self::constant_scope_names(&entries, class)),
                }
            }
            _ => ConstantScopeWorlds::Master(self.master_constant_scopes(class)),
        }
    }

    /// `MasterOrder.breadth_first` under `:constants`: every resolved
    /// project ancestor of `class`, nearer first — each class's mixin
    /// spellings (WRITTEN order) and superclass queued ahead of the
    /// expanded node's own edges. The order `agreed_ancestor_hit` falls
    /// back to when the chain cannot settle — and the pre-rigor-rs#905
    /// walk's one order.
    pub(crate) fn master_constant_scopes(&self, class: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = [class.to_string()].into_iter().collect();
        let mut queue: std::collections::VecDeque<String> =
            self.constant_direct_edges(class).into_iter().collect();
        while let Some(current) = queue.pop_front() {
            if !seen.insert(current.clone()) {
                continue;
            }
            out.push(current.clone());
            if out.len() > CHAIN_LIMIT {
                break;
            }
            queue.extend(self.constant_direct_edges(&current));
        }
        out
    }

    /// `MasterOrder.direct_edges` under `:constants`: the class's
    /// `discovered_includes` — the mixin spellings in ancestor
    /// (nearest-first) order, prepends ahead of includes — then its
    /// superclass, each resolved to the project namespace it names.
    fn constant_direct_edges(&self, class: &str) -> Vec<String> {
        let mut raws = self.constant_mixin_edges(class);
        let entry = self.override_classes.get(class);
        // `discovered_includes` also carries edges [`Self::constant_mixin_edges`]
        // cannot order — ones the statement census did not see (an `include`
        // attributed from outside the body). They ride last, in written
        // order: the master walk is what a class with such an edge settles
        // to anyway, and a name Ruby may place nearer still answers, just
        // later — the declining side.
        if let Some(entry) = entry {
            for raw in &entry.includes {
                if !raws.contains(raw) {
                    raws.push(raw.clone());
                }
            }
            if let Some(sup) = &entry.superclass {
                raws.push(sup.clone());
            }
        }
        raws.into_iter()
            .filter_map(|raw| self.resolve_constant_scope(class, &raw))
            .collect()
    }

    /// `DiscoveryIndex#discovered_includes`'s order for `class` (upstream
    /// #1173): `freeze_mixin_lists` answers `prepends ++ includes`, uniq'd
    /// (first occurrence wins), and each bucket keeps every edge
    /// `write_mixin_targets` recorded — a DIRECT statement, a conditional
    /// or `def`- or block-written `include`, and the receiver form
    /// `C.include(M)` alike — in search order: each edge's argument group
    /// unshifts as one unit (`include A, B` searches `A` first), a repeated
    /// include target keeps its first position, and a re-`prepend` is kept
    /// (the `uniq` takes its earlier position). This is ANCESTOR
    /// (nearest-first) order, not written order: `include N; include M`
    /// lists `[M, N]`.
    ///
    /// A name appearing in both kinds (`include M; prepend M`) is a body
    /// the tables cannot represent and is tainted — the chain carrying it
    /// is marked and settles to master, so the list below never orders
    /// such a node.
    fn constant_mixin_edges(&self, class: &str) -> Vec<String> {
        let mut prepends: Vec<String> = Vec::new();
        let mut includes: Vec<String> = Vec::new();
        for edge in self.ancestry.written.get(class).into_iter().flatten() {
            // An eval-owner edge (`X.class_eval { include M }`) still carries
            // its lexical candidates; `resolved_write_namespace` answers ONE
            // owner — the first candidate `known_namespace?` knows (read as
            // [`Self::knows_discovered_class`], whose `discovered_classes`
            // covers the RBS-known half only through a project singleton), or
            // the bare spelling — and the edge belongs to that name alone. An
            // already-resolved edge holds one owner and matches the key it
            // filed under.
            let resolved = edge
                .owners
                .iter()
                .find(|o| self.knows_discovered_class(o))
                .or_else(|| edge.owners.last())
                .expect("owners");
            if resolved.as_str() != class {
                continue;
            }
            let (prepend, names) = (edge.prepend, &edge.names);
            if prepend {
                // `write_mixin_targets`: a prepend statement's group
                // unshifts whole, repeats kept.
                prepends.splice(0..0, names.iter().cloned());
            } else {
                // A repeated include target keeps its first position —
                // only names the bucket does not already carry unshift.
                let fresh: Vec<String> = names
                    .iter()
                    .filter(|n| !includes.contains(*n))
                    .cloned()
                    .collect();
                includes.splice(0..0, fresh);
            }
        }
        let mut out: Vec<String> = Vec::new();
        for name in prepends.into_iter().chain(includes) {
            if !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }

    /// rigor-rs#905 — the `:constants` flavor's `Resolver#resolve_one`: the
    /// first lexical candidate of `raw` written from `owner` that the
    /// project knows as a namespace — `known_project_namespace?`, read as
    /// [`Self::knows_discovered_class`] (whose `discovered_classes` covers
    /// the synthesized prefixes and meta-new writes). A `::`-rooted
    /// spelling is anchored at the top level, as
    /// `ancestor_name_candidates` anchors it.
    ///
    /// Candidates the project only WRITES are skipped — the reference's
    /// resolver asks `project?` alone and is blind to constant writes — so
    /// a `class Base` + `Base = 1` pair resolves `Base` to the DECLARED
    /// class and `class Sub < Base; K` reads `Base::K` (the declared name
    /// wins over the write, rigor-rs#905's `Base = 1` edge). A spelling
    /// with no declared candidate resolves to no project entry: whether a
    /// written constant binds it or nobody does, `constant_scopes_of`
    /// contributes no name for it under the port.
    fn resolve_constant_scope(&self, owner: &str, raw: &str) -> Option<String> {
        if let Some(rooted) = raw.strip_prefix("::") {
            return self
                .knows_discovered_class(rooted)
                .then(|| rooted.to_string());
        }
        let segments: Vec<&str> = owner.split("::").collect();
        for i in (0..segments.len()).rev() {
            let candidate = if i == 0 {
                raw.to_string()
            } else {
                format!("{}::{}", segments[..i].join("::"), raw)
            };
            if self.knows_discovered_class(&candidate) {
                return Some(candidate);
            }
        }
        None
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
    /// a `possible` def, a contested visibility slot, or — when the reader
    /// consults visibility — a def-node slot a non-`def` definer superseded
    /// (`supersede_def_nodes`) or a name a `define_method` / `alias` left
    /// replaced without a later certain visibility write
    /// (`replaced_visibility_mark`, upstream #2013).
    fn uncertain_definer(&self, owner: &str, method: &str, reads_visibility: bool) -> bool {
        let key = (owner.to_string(), method.to_string());
        if self.ancestry.possible.contains(&key) || self.contested_slot(owner, method) {
            return true;
        }
        let seg = owner.rsplit("::").next().unwrap_or(owner);
        if self.def_node_superseded(seg, method) {
            return true;
        }
        reads_visibility && self.visibility_replaced(seg, method)
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
                self.settled_definer(core, k, method, false, false, |n| {
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
    /// `reads_visibility` (rigor-rs#492): the reader's answer comes from the
    /// reference's def-node / visibility slot, so the supersede and
    /// replaced-mark declines join the `possible?` ones.
    pub(crate) fn settled_definer(
        &self,
        core: &CoreIndex,
        class: &str,
        method: &str,
        skip_self: bool,
        reads_visibility: bool,
        defines: impl Fn(&str) -> bool,
    ) -> Option<String> {
        let chain = self.instance_chain(class)?;
        if !chain.marks.iter().all(|m| self.discharged(core, m, method)) {
            return None;
        }
        let answer = first_definer(
            self,
            core,
            &chain.entries,
            class,
            method,
            skip_self,
            reads_visibility,
            &defines,
        )?;
        match &chain.retro {
            None => Some(answer),
            Some(retro) => {
                let other = first_definer(
                    self,
                    core,
                    retro,
                    class,
                    method,
                    skip_self,
                    reads_visibility,
                    &defines,
                )?;
                (other == answer).then_some(answer)
            }
        }
    }
}

/// The first definer of `method` on one world of a chain, read from the
/// class's own entry (`skip_self`: the entry after it), or `None` where a
/// definer Ruby may not reach could answer first.
#[allow(clippy::too_many_arguments)] // the env context is one logical read
fn first_definer(
    idx: &SourceIndex,
    core: &CoreIndex,
    entries: &[ChainEntry],
    class: &str,
    method: &str,
    skip_self: bool,
    reads_visibility: bool,
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
                if idx.uncertain_definer(n, method, reads_visibility) {
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

    /// A gated vendored feature the project CONFIG names — `libraries:
    /// [prime]` activates exactly like a source `require "prime"`
    /// (`RequiredFeatures.tokens(features, configured_libraries)`,
    /// rigor-rs#463).
    pub fn add_required_feature(&mut self, feature: &str) {
        self.ancestry.required_features.insert(feature.to_string());
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
    ///
    /// The mixin closure itself is method-independent, so it is collected ONCE
    /// per index ([`Self::collect_object_mixins`], memoised in
    /// `object_mixins`) and each call only asks the per-method questions of the
    /// collected sets — the same union the per-edge walks of
    /// `mixin_edge_may_define` produced (rigor-rs#464).
    pub fn object_mixin_may_define(&self, core: &CoreIndex, method: &str, explicit: bool) -> bool {
        let m = self.object_mixins.get_or_init(|| self.collect_object_mixins());
        if m.cut
            || (!explicit && (m.unnamed || m.tainted))
            || (m.object_records && self.records_name("Object", method))
        {
            return true;
        }
        m.modules
            .iter()
            .any(|q| self.records_name(q, method) || self.macro_named(q, method))
            || m.opaque.iter().any(|(from, raw, declared)| {
                declared.as_deref().is_some_and(|q| self.macro_named(q, method))
                    || external_may_define(core, from, raw, method, explicit)
            })
    }

    /// The one-time half of [`Self::object_mixin_may_define`]: `Object`'s
    /// mixin side walked as the reference's two instance chains — the
    /// `answer_on_chain` calls `Inference::ObjectMixins` makes on its
    /// `EDGE_OWNERS`, one `ResolutionChain` rooted at the
    /// `<toplevel-include>` pseudo-owner (every file's top-level `include`s
    /// file under it) and one at `Object` (`include M` in `class Object`
    /// plus its listed edges). Each owner's edges therefore share ONE
    /// [`CHAIN_LIMIT`] budget, and the chain's root entry counts toward it —
    /// the reference counts every project entry, its root included
    /// (rigor-rs#637).
    fn collect_object_mixins(&self) -> ObjectMixins {
        let mut m = ObjectMixins::default();
        let mut groups: Vec<(String, Vec<String>)> = Vec::new();
        let mut raws: Vec<String> = Vec::new();
        for r in &self.ancestry.toplevel_includes {
            if !raws.contains(r) {
                raws.push(r.clone());
            }
        }
        if !raws.is_empty() {
            groups.push((String::new(), std::mem::take(&mut raws)));
        }
        if let Some(obj) = self.override_classes.get("Object") {
            for r in &obj.includes {
                if !raws.contains(r) {
                    raws.push(r.clone());
                }
            }
            m.object_records = self.known_user("Object");
        }
        if let Some(listed) = self.ancestry.include_listed.get("Object") {
            for r in listed {
                if !raws.contains(r) {
                    raws.push(r.clone());
                }
            }
        }
        if !raws.is_empty() {
            groups.push(("Object".to_string(), raws));
        }
        for (owner, raws) in groups {
            if m.cut {
                break;
            }
            self.collect_owner_edges(&owner, raws, &mut m);
        }
        m
    }

    /// One owner chain's closure walk for [`Self::collect_object_mixins`].
    /// `seen` starts holding `owner` — the chain's root entry counts against
    /// [`CHAIN_LIMIT`] exactly as `ResolutionChain`'s `cut_position` counts it
    /// (`""` stands in for the `<toplevel-include>` pseudo-entry, which names
    /// no module the walk can reach). Every method-dependent question is
    /// deferred: a marker edge sets `unnamed`, a wildcard/hook-dirty module
    /// sets `tainted`, a known-user module joins `modules`, and an edge
    /// resolving to nothing or to a non-`known_user` project module joins
    /// `opaque` (which keeps the `(from, raw)` pair — the per-method
    /// `external_may_define` needs the writer's lexical scope).
    fn collect_owner_edges(&self, owner: &str, raws: Vec<String>, m: &mut ObjectMixins) {
        // The question is a union over the modules' ancestry, not "which
        // definer does Ruby call", so the closure is walked directly: an order
        // the port cannot vouch for (a fork, a multi-file mark) does not matter
        // here, and declining on it silenced every receiver in a project whose
        // included module merely had one (concurrent-ruby's `Concurrent`).
        let mut queue: Vec<(String, String)> =
            raws.into_iter().map(|r| (owner.to_string(), r)).collect();
        let mut seen: HashSet<String> = [owner.to_string()].into_iter().collect();
        while let Some((from, raw)) = queue.pop() {
            if mixin_marker(&raw) {
                m.unnamed = true;
                continue;
            }
            match self.resolve_override_ancestor(&from, &raw) {
                Some(q) if self.known_user(&q) => {
                    if !seen.insert(q.clone()) {
                        continue;
                    }
                    // `ResolutionChain::LIMIT`: a cut chain is uncertainty —
                    // the answer is `true` for every method, so collecting
                    // can stop.
                    if seen.len() > CHAIN_LIMIT {
                        m.cut = true;
                        return;
                    }
                    // A wildcard edge (an include the walk cannot name) or a
                    // hook may hide a definer; only a bare call counts that,
                    // as the reference's chain lists neither as an entry and
                    // a receiver call skips every unknown module (an
                    // `include('x')` matcher in a spec's `module Concurrent`
                    // body would otherwise silence the whole project).
                    if self.ancestry.wildcard_include(&q) || self.ancestry.dirty.contains(&q) {
                        m.tainted = true;
                    }
                    if m.module_set.insert(q.clone()) {
                        m.modules.push(q.clone());
                    }
                    for e in self.object_mixin_edges(&q) {
                        queue.push((q.clone(), e));
                    }
                }
                // A declared module's class-body macro may name it (#2094) —
                // the `Some` half — and any raw spelling may be an ancestor
                // RBS knows (`external_may_define`). Both are per-method
                // questions; the edge itself is method-independent.
                other => {
                    if m.opaque_set.insert((from.clone(), raw.clone())) {
                        m.opaque.push((from, raw, other));
                    }
                }
            }
        }
    }

    /// The mixin edges `mixin_edge_may_define` enqueued for `q`: its direct
    /// `include`s, its listed (unpositioned) edges and its statement groups.
    fn object_mixin_edges(&self, q: &str) -> Vec<String> {
        let mut edges: Vec<String> = self
            .override_classes
            .get(q)
            .map(|e| e.includes.clone())
            .unwrap_or_default();
        if let Some(listed) = self.ancestry.include_listed.get(q) {
            edges.extend(listed.iter().cloned());
        }
        for (_, names) in self.ancestry.groups.get(q).into_iter().flatten() {
            edges.extend(names.iter().cloned());
        }
        edges
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

/// rigor-rs#905 — the ancestor-scope worlds [`SourceIndex::constant_scope_worlds`]
/// hands `constant_for_name`'s step 2: the chain's order where the port
/// can vouch for it, its retro world when the chain's one settle-able
/// fork needs the comparison, or the master (breadth-first) order.
pub(crate) enum ConstantScopeWorlds {
    /// `agreed_ancestor_hit`'s chain world plus its retro world when the
    /// build tagged one include-side skip (`Some` ⇒ the caller must
    /// compare the first answering OWNER across the two before the
    /// chain's answer may stand).
    Settled { scopes: Vec<String>, retro: Option<Vec<String>> },
    /// `MasterOrder.breadth_first` — the fallback `chain.settle` answers
    /// for an order mark, a multi-fork or a chain the build cannot place.
    Master(Vec<String>),
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
/// The reference's `OPENING_HOOKS` — the hooks whose presence on a class
/// leaves its singleton surface open for `open_project_singleton?`.
const OPENING_HOOKS: &[&str] = &[
    "inherited",
    "included",
    "extended",
    "prepended",
    "method_missing",
    "respond_to_missing?",
];
/// The reference's `CLOSED_ROOTS` — the two superclass names the
/// open-surface chain treats as closed.
const CLOSED_ROOTS: &[&str] = &["Object", "BasicObject"];

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
            .filter(|(_, raw)| !mixin_marker(raw))
            .collect();
        // `chain_answers?`'s `ResolutionChain` root is a project entry itself,
        // so the walk's [`CHAIN_LIMIT`] budget starts one entry spent
        // (rigor-rs#637).
        let mut seen: HashSet<String> = [a.to_string()].into_iter().collect();
        while let Some((from, raw)) = queue.pop() {
            if mixin_marker(&raw) {
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
            if !mixin_marker(raw) && !out.contains(raw) {
                out.push(raw.clone());
            }
        }
        out
    }

    /// `chain.truncated?` of `q`'s singleton chain: its extended modules and
    /// their include closures hold more than [`CHAIN_LIMIT`] project modules.
    fn singleton_truncated(&self, core: &CoreIndex, q: &str) -> bool {
        let mut queue: Vec<(String, String)> = self
            .singleton_raws(q)
            .into_iter()
            .filter(|r| !mixin_marker(r))
            .map(|r| (q.to_string(), r))
            .collect();
        // `q`'s singleton chain counts its root entry — the singleton class
        // object — toward [`CHAIN_LIMIT`] (rigor-rs#637). The root is NOT
        // pre-seeded into `seen`: entries intern PER SIDE, so a self-extend
        // (`extend Comparable`, `Integer.extend(Integer)`) inserts
        // `instance_lin(q)`'s head — a SECOND, instance-side entry of `q` —
        // and the walk must descend `q`'s instance closure from it. Seeding
        // by name made the self-extend edge hit the dedup and the walk could
        // never truncate at all (rigor-rs#667). The `+ 1` below is the
        // root's own entry; every reached name counts once, `q` included.
        let mut seen: HashSet<String> = HashSet::new();
        while let Some((from, raw)) = queue.pop() {
            if mixin_marker(&raw) {
                continue;
            }
            // The reference's resolver answers an RBS-known constant too
            // (`each_mixin_chain` yields `instance_lin(name)`): a mixin the
            // project never declares still contributes its own entry plus
            // its receiver-form includes — `Integer.extend(Integer)`'s
            // self-edge resolves to Integer itself.
            let m = match self.resolve_override_ancestor(&from, &raw) {
                Some(m) => m,
                None => {
                    let name = raw.strip_prefix("::").unwrap_or(&raw);
                    let rbs_known = core.knows_qualified_class(name)
                        || (!name.contains("::") && core.knows_toplevel_class(name));
                    if rbs_known {
                        name.to_string()
                    } else {
                        continue;
                    }
                }
            };
            if !seen.insert(m.clone()) {
                continue;
            }
            if seen.len() + 1 > CHAIN_LIMIT {
                return true;
            }
            queue.extend(self.instance_mixin_edges(&m));
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
        self.singleton_truncated(core, owner)
            || self
                .singleton_raws(owner)
                .iter()
                .filter(|r| !mixin_marker(r))
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
    /// name, macro-names it, or is open, or a module of its include/prepend
    /// closure (level 0 of the reference's singleton chain) may answer.
    /// A pure SILENCER.
    pub fn core_singleton_mixin_may_answer(&self, core: &CoreIndex, class_name: &str, method: &str) -> bool {
        rbs_ancestors(core, class_name)
            .iter()
            .filter(|a| a.as_str() == class_name || !core.is_module(a))
            .any(|n| {
                let open = !OBJECT_ROOTS.contains(&n.as_str());
                let raws = self.singleton_raws(n);
                if self.singleton_truncated(core, n)
                    || (open && raws.iter().any(|r| mixin_marker(r)))
                {
                    return true;
                }
                raws.iter().filter(|r| !mixin_marker(r)).any(|raw| {
                    // v0.4.6 review: level 0 of the reference's singleton
                    // chain holds the extended module's own include/prepend
                    // closure too (`module Ext; include Inner`), read as the
                    // instance-side closure is. A union, so a superset.
                    if let Some(q) = self.resolve_override_ancestor(n, raw) {
                        if &q != n && self.mixin_closure_may_answer(core, &q, method, open, false) {
                            return true;
                        }
                    }
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

    /// upstream #2438 — `open_source_class?` (the source half of
    /// `open_project_singleton?`): whether `name`'s singleton surface may
    /// hold methods its declarations do not show. A class is OPEN when,
    /// for itself or a project class up its superclass chain:
    ///   * no `class`/`module` body exists inside `paths:` at all — a
    ///     `sig/`-only name, a synthesized namespace prefix (`class Foo::Bar`
    ///     names `Foo` without a body — [`Self::knows_discovered_decl`]
    ///     answers `false`), or a meta-new write the chain cannot model a
    ///     superclass for (`Foo = Struct.new(...)` — the reference's
    ///     `chain.opaque_tail?`), or
    ///   * a `def` records an `inherited`, `included`, `extended`,
    ///     `prepended`, `method_missing` or `respond_to_missing?` hook on
    ///     either method side (`OPENING_HOOKS`), or
    ///   * an `extend` adds a singleton-side mixin entry (`singleton_raws`
    ///     — any spelling, the wildcard markers included), or
    ///   * a written superclass reaches no project class — an external
    ///     superclass edge; `Object`/`BasicObject` (`CLOSED_ROOTS`) are the
    ///     closed roots the chain ends on, or
    ///   * the member-less project declaration's own written superclass —
    ///     consulted when the source chain wrote none (`open_tail?`) —
    ///     names no project class.
    ///
    /// An instance-side `include`/`prepend` never opens it (they add
    /// instance methods only), and a chain walk past [`CHAIN_LIMIT`] reads
    /// open, mirroring `truncated?`.
    ///
    /// Reached only for project-sig classes whose receiver still minted
    /// `Type::Singleton` — today a source `class`/`module` decl shadows
    /// `singleton_for_name` (the mint reads `Instance`/`Dynamic`), so the
    /// deeper arms sit dormant until that divergence closes; the
    /// `knows_discovered_*` half carries the live verdicts.
    pub fn open_source_class(&self, name: &str, index: &CoreIndex) -> bool {
        if !self.knows_discovered_class(name) || !self.knows_discovered_decl(name) {
            return true;
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut last: Option<String> = None;
        let mut current = Some(name.to_string());
        while let Some(c) = current {
            if !seen.insert(c.clone()) {
                break;
            }
            if seen.len() > CHAIN_LIMIT {
                return true;
            }
            for hook in OPENING_HOOKS {
                if self.project_declares_method(None, &c, hook)
                    || self.owner_defines(&c, hook, DefKind::Instance)
                    || self.owner_defines(&c, hook, DefKind::Singleton)
                {
                    return true;
                }
            }
            if !self.singleton_raws(&c).is_empty() {
                return true;
            }
            last = Some(c.clone());
            current = match self.override_classes.get(&c).and_then(|e| e.superclass.as_ref()) {
                Some(raw) => {
                    let stripped = raw.strip_prefix("::").unwrap_or(raw);
                    if CLOSED_ROOTS.contains(&stripped) {
                        None
                    } else {
                        match self.resolve_override_ancestor(&c, raw) {
                            Some(q) => Some(q),
                            // The written superclass reaches no project
                            // class — an external superclass edge leaves
                            // the singleton surface open.
                            None => return true,
                        }
                    }
                }
                None => None,
            };
        }
        // `open_tail?` — the member-less project declaration's own written
        // superclass of the LAST chain class, consulted when the source
        // wrote none: not a project class means the class's singleton side
        // keeps answering through whatever that name resolves to at
        // runtime.
        if let Some(last) = last {
            if let Some(sup) = index.memberless_project_superclass(&last) {
                let stripped = sup.strip_prefix("::").unwrap_or(sup);
                if !CLOSED_ROOTS.contains(&stripped) && !self.knows_discovered_class(stripped) {
                    return true;
                }
            }
        }
        false
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
