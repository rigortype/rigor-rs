//! ADR-35 slice 1: the lexically-qualified override index (Pass 1b, its merge replay and MRO
//! ancestor walks), and the per-file lexical scopes that mirror it.

use std::collections::HashSet;

use rigor_parse::{LoweredAst, Node, NodeId, Visibility};

use super::{HarvestedOverrideClass, OverrideClass, SourceIndex, OVERRIDE_ANCESTOR_WALK_LIMIT};

impl SourceIndex {
    /// The IMMEDIATE class/module children the analysed SOURCE declares under
    /// the namespace `parent_fqn`, as `(leaf name, is_module)` — the project-side
    /// twin of [`CoreIndex::namespace_children`], which answers the same question
    /// over the RBS qualified registry.
    ///
    /// Contract, mirrored on that method deliberately so the LSP's `Foo::`
    /// completion can union the two without reconciling two shapes:
    ///
    /// * **Immediate children only** — a grandchild (`Foo::Bar::Baz` under
    ///   `Foo`) is skipped, exactly as the RBS side skips it and as the
    ///   reference's `enumerate_constant_children` does.
    /// * **`is_module` is the DECLARED kind** (`module Foo` ⇒ `true`), taken
    ///   first-write-wins from the first declaration the merge replayed.
    /// * **Deterministic, name-sorted order** — a `BTreeMap` collect, the same
    ///   ordering the RBS path already produces.
    ///
    /// Read-only and read by NOBODY else: it enumerates the ADR-35 lexically
    /// qualified override registry, which is the only project-wide table keyed
    /// by FULLY-QUALIFIED name (the collapsed `classes` map cannot answer a
    /// namespace question — `Foo::Bar` and `Baz::Bar` share the key `Bar`).
    ///
    /// [`CoreIndex::namespace_children`]: rigor_index::CoreIndex::namespace_children
    pub fn namespace_children(&self, parent_fqn: &str) -> Vec<(&str, bool)> {
        let prefix = format!("{parent_fqn}::");
        let mut set: std::collections::BTreeMap<&str, bool> = std::collections::BTreeMap::new();
        for (qual, entry) in &self.override_classes {
            let Some(leaf) = qual.strip_prefix(prefix.as_str()) else {
                continue;
            };
            if leaf.is_empty() || leaf.contains("::") {
                continue;
            }
            set.insert(leaf, entry.is_module);
        }
        set.into_iter().collect()
    }

    /// ADR-35 slice 1: the discovered instance-method VISIBILITY of `method` on
    /// the QUALIFIED project class `class` (its OWN table only — not inherited).
    /// `None` when `class` is not in the override index or does not record
    /// `method`.
    pub fn method_visibility(&self, class: &str, method: &str) -> Option<Visibility> {
        self.override_classes
            .get(class)
            .and_then(|c| c.method_visibilities.get(method).copied())
    }

    /// ADR-35 slice 1: the NEAREST project ancestor of the QUALIFIED class
    /// `class` that DEFINES the instance method `method`, paired with that
    /// ancestor's discovered visibility for `method` (`None` when the ancestor
    /// defines the method but its visibility is UNKNOWN — e.g. `private def` /
    /// dynamic form).
    ///
    /// MRO-ordered breadth-first walk over the LEXICALLY-QUALIFIED override index:
    /// included / prepended modules FIRST, then the superclass (Ruby's MRO
    /// ordering). Each ancestor name is resolved against the subclass's lexical
    /// nesting (the reference's `resolve_override_ancestor_name`) and dropped if
    /// it names no PROJECT class (RBS / third-party ancestors are NOT walked —
    /// slice-1 carve-out). Cycle-guarded and capped at
    /// [`OVERRIDE_ANCESTOR_WALK_LIMIT`] visited nodes (returns `None` past the cap
    /// — a missed witness, never an FP).
    ///
    /// An ancestor DEFINES `method` when it appears in that ancestor's own
    /// `methods` set OR its `method_visibilities` table; the walk STOPS at the
    /// first such ancestor.
    ///
    /// ## The zero-FP keystones (do NOT weaken)
    ///
    /// 1. **Lexical qualification.** The index is keyed by FULL qualified name, so
    ///    a nested `module Params` in `IssuableFinder` is `IssuableFinder::Params`
    ///    — it never merges with `Groups::Params`. Collapsing them invented
    ///    phantom ancestors / methods (the gitlab-foss FP cluster).
    /// 2. **Never synthesize Public.** The returned visibility is the ancestor's
    ///    RECORDED entry or `None`. The caller must treat `None` as "cannot prove
    ///    a reduction" and STAY SILENT — never fabricate `Public` from a missing
    ///    entry (the reference's Mastodon 160 → 35 cluster).
    pub fn nearest_ancestor_defining(
        &self,
        class: &str,
        method: &str,
    ) -> Option<(String, Option<Visibility>)> {
        let mut queue: Vec<String> = self.override_ancestor_names(class);
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(class.to_string());
        let mut visited = 0usize;

        while !queue.is_empty() {
            let current = queue.remove(0);
            if !seen.insert(current.clone()) {
                continue;
            }
            visited += 1;
            if visited > OVERRIDE_ANCESTOR_WALK_LIMIT {
                return None; // cap exceeded ⇒ decline (never an FP).
            }
            if let Some(entry) = self.override_classes.get(&current) {
                let defines = entry.methods.contains(method)
                    || entry.method_visibilities.contains_key(method);
                if defines {
                    // Stop at the nearest defining ancestor; its visibility may be
                    // None (unknown) — the caller treats unknown as "cannot prove".
                    return Some((current.clone(), entry.method_visibilities.get(method).copied()));
                }
                // Not defined here ⇒ enqueue this ancestor's own ancestors.
                for next in self.override_ancestor_names(&current) {
                    queue.push(next);
                }
            }
        }
        None
    }

    /// The direct PROJECT ancestors of the QUALIFIED `class`, resolved + ordered:
    /// each `include` / `prepend` (in source order) FIRST, then the `superclass`
    /// — Ruby's MRO ordering. Names that resolve to no project class (RBS /
    /// third-party) are dropped (slice-1 carve-out).
    pub(crate) fn override_ancestor_names(&self, class: &str) -> Vec<String> {
        let Some(entry) = self.override_classes.get(class) else {
            return Vec::new();
        };
        let mut names = Vec::new();
        for inc in &entry.includes {
            if let Some(resolved) = self.resolve_override_ancestor(class, inc) {
                names.push(resolved);
            }
        }
        if let Some(sup) = &entry.superclass {
            if let Some(resolved) = self.resolve_override_ancestor(class, sup) {
                names.push(resolved);
            }
        }
        names
    }

    /// Resolve an as-written ancestor name against the subclass's lexical
    /// nesting, returning the QUALIFIED project class name it names, or `None` if
    /// it names no project class. Mirrors the reference's
    /// `resolve_override_ancestor_name`: try `<prefix>::<raw>` for each enclosing
    /// scope of the subclass, longest-prefix first, falling back to the bare name.
    /// A leading `::` on the raw name is stripped (a top-level absolute path).
    fn resolve_override_ancestor(&self, subclass: &str, raw: &str) -> Option<String> {
        let raw = raw.strip_prefix("::").unwrap_or(raw);
        let segments: Vec<&str> = subclass.split("::").collect();
        // Drop the subclass's own last segment; try its enclosing scopes
        // longest-first, then the top level (bare `raw`).
        for i in (0..segments.len()).rev() {
            let candidate = if i == 0 {
                raw.to_string()
            } else {
                format!("{}::{}", segments[..i].join("::"), raw)
            };
            if self.override_classes.contains_key(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    /// Fold one (re)definition of a QUALIFIED override class into the index.
    pub(crate) fn ingest_override_class(
        &mut self,
        qualified: &str,
        superclass: Option<String>,
        methods: &[String],
        method_visibilities: &[(String, Visibility)],
        includes: &[String],
        is_module: bool,
    ) {
        // `or_insert_with` (not `or_default`) so `is_module` is FIRST-WRITE-WINS
        // like `superclass`: a reopen never re-decides the kind.
        let entry = self
            .override_classes
            .entry(qualified.to_string())
            .or_insert_with(|| OverrideClass { is_module, ..OverrideClass::default() });
        if entry.superclass.is_none() {
            entry.superclass = superclass;
        }
        for m in methods {
            entry.methods.insert(m.clone());
        }
        // First-write-wins per method name (stable cross-file view).
        for (m, vis) in method_visibilities {
            entry.method_visibilities.entry(m.clone()).or_insert(*vis);
        }
        for inc in includes {
            if !entry.includes.contains(inc) {
                entry.includes.push(inc.clone());
            }
        }
    }
}

/// ADR-35 slice 1: recursively collect the LEXICALLY-QUALIFIED override classes
/// from `ast`, starting at `node` under the lexical `prefix` (the enclosing
/// class/module name segments). A `ClassDef`/`ModuleDef` appends a
/// [`HarvestedOverrideClass`] keyed by `prefix + name`, then recurses into its
/// body with the extended prefix so a nested class/module is fully qualified.
/// Other nodes recurse over their direct children only enough to reach nested
/// class/module bodies (handled via the explicit body lists below).
///
/// Reads ONE file and accumulates nothing: the first-write-wins semantics for
/// visibilities + superclass and the ordered append-with-dedup for includes are
/// the MERGE's ([`SourceIndex::ingest_override_class`]), replaying `out` in file
/// order. That is why `out` must stay in walk order.
pub(crate) fn collect_override_classes(
    ast: &LoweredAst,
    node: NodeId,
    prefix: &[String],
    out: &mut Vec<HarvestedOverrideClass>,
) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                collect_override_classes(ast, child, prefix, out);
            }
        }
        Node::ClassDef {
            name,
            superclass_path,
            methods,
            method_visibilities,
            includes,
            body,
            ..
        } => {
            if name.is_empty() {
                return;
            }
            let qualified = qualify(prefix, name);
            out.push(HarvestedOverrideClass {
                qualified: qualified.clone(),
                superclass: superclass_path.clone(),
                methods: methods.to_vec(),
                method_visibilities: method_visibilities.to_vec(),
                includes: includes.to_vec(),
                is_module: false,
            });
            let child_prefix = split_qualified(&qualified);
            for &child in body {
                collect_override_classes(ast, child, &child_prefix, out);
            }
        }
        Node::ModuleDef { name, methods, method_visibilities, includes, body, .. } => {
            if name.is_empty() {
                return;
            }
            let qualified = qualify(prefix, name);
            out.push(HarvestedOverrideClass {
                qualified: qualified.clone(),
                superclass: None,
                methods: methods.to_vec(),
                method_visibilities: method_visibilities.to_vec(),
                includes: includes.to_vec(),
                is_module: true,
            });
            let child_prefix = split_qualified(&qualified);
            for &child in body {
                collect_override_classes(ast, child, &child_prefix, out);
            }
        }
        // Any other node: a nested class/module only appears as a DIRECT body
        // statement of a class/module/program (mirroring the reference's
        // `record_def_visibility`/qualification, which only qualifies through
        // class/module bodies). We deliberately do NOT descend into method
        // bodies / control flow — a def-nested class is out of slice-1 scope.
        _ => {}
    }
}

/// ADR-35 slice 1: join a lexical `prefix` and a (possibly already-namespaced)
/// declaration `name` into a fully-qualified name. A `name` that is itself a
/// path (`Foo::Bar` declared inside `Outer`) qualifies to `Outer::Foo::Bar`,
/// matching Ruby's lexical constant resolution for the declaration head.
pub(crate) fn qualify(prefix: &[String], name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{}::{}", prefix.join("::"), name)
    }
}

/// Split a qualified name into its segment vector (`"A::B" -> ["A", "B"]`), used
/// as the child lexical prefix when recursing into a class/module body.
pub(crate) fn split_qualified(qualified: &str) -> Vec<String> {
    qualified.split("::").map(|s| s.to_string()).collect()
}

/// C1: the per-file lexical class/module SCOPES — each `(span, qualified segment
/// vector)` — so a `ConstantRead`'s use-site lexical prefix can be recovered by
/// span containment (the innermost enclosing scope). Mirrors the qualification
/// walk of [`collect_override_classes`]; computed once per analyzed
/// file and threaded into the [`Typer`] so its `ConstantRead` arm can consult
/// [`SourceIndex::constant_shadowed`] with the correct lexical prefix.
///
/// [`Typer`]: crate::Typer
pub fn lexical_scopes(ast: &LoweredAst) -> Vec<(rigor_parse::Span, Vec<String>)> {
    let mut out = Vec::new();
    collect_lexical_scopes(ast, ast.root(), &[], &mut out);
    out
}

/// The span of every METHOD body in the file (`def x` / `def self.x`), excluding
/// `class << X` bodies (which are class scopes, not method scopes).
///
/// A Ruby method body is an independent LOCAL scope: it never sees the enclosing
/// file's locals. Prism already encodes that for a bare name — `s` inside a `def`
/// lowers to a CALL, not a `LocalVariableRead`, when the def does not bind `s` —
/// so the only reads that survive into a body are its own parameters and writes.
/// The flat top-level env is keyed by NAME alone, though, so a parameter that
/// happens to share a name with a top-level local (`s = 'a'` at file scope,
/// `def go(s)` below it) used to read the top-level local's TYPE. That is what
/// produced the `wrong-arity`/`undefined-method` FPs on rigor-survey
/// `Ruby/data_structures/hash_table/anagram_checker.rb`.
///
/// Callers use these spans to withhold the top-level env from a use site inside
/// a method body. Span-containment (not a structural walk) is orphan-proof — the
/// same discipline as [`lexical_scopes`] and the dead-assignment collector.
pub fn method_body_spans(ast: &LoweredAst) -> Vec<rigor_parse::Span> {
    ast.iter()
        .filter_map(|(_, n)| match n {
            Node::Definition { is_singleton_class: false, span, .. } => Some(*span),
            _ => None,
        })
        .collect()
}

fn collect_lexical_scopes(
    ast: &LoweredAst,
    node: NodeId,
    prefix: &[String],
    out: &mut Vec<(rigor_parse::Span, Vec<String>)>,
) {
    match ast.get(node) {
        Node::Program { body, .. } | Node::Statements { body, .. } => {
            for &child in body {
                collect_lexical_scopes(ast, child, prefix, out);
            }
        }
        Node::ClassDef { name, body, span, .. } | Node::ModuleDef { name, body, span, .. } => {
            if name.is_empty() {
                return; // un-namable (dynamic constant / `class << self`) ⇒ skip.
            }
            let qualified = qualify(prefix, name);
            let segs = split_qualified(&qualified);
            out.push((*span, segs.clone()));
            for &child in body {
                collect_lexical_scopes(ast, child, &segs, out);
            }
        }
        _ => {}
    }
}
