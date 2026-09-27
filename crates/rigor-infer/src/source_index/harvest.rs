//! Issue #92: `build` / `build_project`, the per-file `harvest` (parallel) and the serial `merge`
//! that replays it and runs every cross-file pass.

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};

use rigor_index::CoreIndex;
use rigor_parse::{FileKey, LoweredAst, Node};

use super::{
    collect_declared_names, collect_literal_constants, collect_override_classes, def_root_cx,
    file_orphan_defs, infer_method_returns, invert_definers, mutated_constant_names, walk_defs,
    walk_fold_defs, ConstLit, DefTables, FileDefs, FoldDefs, FoldSite, Harvest, HarvestedClass,
    HarvestedFoldDef, SourceIndex,
};

impl SourceIndex {
    /// Build from a lowered AST against the core (RBS) index. Collects every
    /// `ClassDef`/`ModuleDef` (source structure) and registers an instance-class
    /// id for every class we may type an instance of: each source class, and
    /// each `X.new` receiver constant whose `X` is RBS-known (so a `Pathname.new`
    /// instance carries identity even though `Pathname` is outside `CORE_CLASSES`).
    pub fn build(ast: &LoweredAst, core: &CoreIndex) -> Self {
        Self::build_project(&[ast], core)
    }

    /// Build a PROJECT-WIDE index from EVERY analyzed file's lowered AST. Class /
    /// module names are harvested from all `asts`, so [`knows_class`] answers
    /// project-wide — this is what lets the rules layer refuse to singleton-type a
    /// bare constant that the project itself defines elsewhere (e.g. a Rails model
    /// `Group`/`Report`), keeping cross-file constant typing false-positive-free.
    ///
    /// Constant registration is also project-wide and generalized: EVERY
    /// `Node::ConstantRead { name }` whose `name` is RBS-known (and not already a
    /// source class) gets a registry id, so `Time`/`Array`/... round-trip via
    /// [`class_id`]/[`class_name_for_id`] for singleton rendering. The original
    /// `X.new` registration is subsumed by this (its receiver is a `ConstantRead`).
    ///
    /// [`knows_class`]: SourceIndex::knows_class
    /// [`class_id`]: SourceIndex::class_id
    /// [`class_name_for_id`]: SourceIndex::class_name_for_id
    pub fn build_project(asts: &[&LoweredAst], core: &CoreIndex) -> Self {
        let files: Vec<(Harvest, &LoweredAst)> =
            asts.iter().map(|ast| (Self::harvest(ast, core), *ast)).collect();
        Self::merge(&files, core)
    }

    /// **Issue #92 — the PARALLEL half.** Everything one file contributes to a
    /// project index that is derivable from `(that file's AST, the FROZEN
    /// [`CoreIndex`])` alone: passes 1, 1b, 1c, 1d, 1e, C5a, 2 and the 4a walk.
    ///
    /// Reads no other file's state and no accumulated [`SourceIndex`] state, so
    /// it is safe to run inside the CLI's stage-1 rayon closure beside
    /// parse+lower. Nothing here decides anything: the ordered fields are
    /// REPLAYED by [`Self::merge`] in the caller's file order, and every
    /// cross-file gate (the constant single-assignment gate, the declaration-only
    /// set, tier-4b returns, the definers inversion, the interprocedural fold)
    /// runs there.
    pub fn harvest(ast: &LoweredAst, core: &CoreIndex) -> Harvest {
        let mut h = Harvest::default();

        // Pass 1: source class/module structure, in `ast.iter()` order (which IS
        // the registration ⇒ ClassId order once the merge replays it).
        for (_, node) in ast.iter() {
            match node {
                Node::ClassDef { name, superclass, methods, .. } => {
                    if name.is_empty() {
                        continue; // un-namable (dynamic constant) ⇒ skip.
                    }
                    h.source_classes.push(HarvestedClass {
                        name: name.clone(),
                        superclass: superclass.clone(),
                        methods: methods.to_vec(),
                    });
                }
                Node::ModuleDef { name, methods, .. } => {
                    if name.is_empty() {
                        continue;
                    }
                    // A module has no super.
                    h.source_classes.push(HarvestedClass {
                        name: name.clone(),
                        superclass: None,
                        methods: methods.to_vec(),
                    });
                }
                _ => {}
            }
        }

        // Pass 1b (ADR-35 slice 1): the LEXICALLY-QUALIFIED override index, by a
        // recursive walk with a nesting stack, so a nested `module Params` is
        // keyed `Outer::Params` (not the collapsed `Params`). This is what keeps
        // the override-visibility rule free of the name-collision false
        // positives. Kept entirely separate from the collapsed `classes` map —
        // no other rule is affected.
        collect_override_classes(ast, ast.root(), &[], &mut h.override_classes);

        // Passes 1c + 1d (ADR-34; upstream `fb781023` / rigortype/rigor#1135 /
        // issue #141): PROJECT-WIDE toplevel method names (`toplevel_defs`,
        // for `call.unresolved-toplevel`) and per-class instance-method
        // declarations (`discovered_methods`) in ONE def-attribution walk —
        // the port of `walk_methods_and_def_nodes`. A `def`'s owner is not
        // always its lexical enclosure: inside `Recv.class_eval { def m }`
        // `m` belongs to `Recv` (it is NOT a toplevel def), inside
        // `K = Class.new { def m }` it belongs to `K`, inside an anonymous
        // factory block it stays toplevel (#319), and only an `Object` owner
        // collapses back to bare-callable. The merge still unions across
        // files so a `def` in one file resolves a call in another.
        let declared = collect_declared_names(ast);
        let mut tables = DefTables::default();
        let mut visited = HashSet::new();
        walk_defs(
            ast,
            &declared,
            &mut tables,
            &mut visited,
            ast.root(),
            &def_root_cx(),
        );
        // Orphan positions: defs lowered into the arena WITHOUT a child edge
        // `walk_defs` follows (range endpoints, def receivers, parameter
        // defaults, dynamic constant-path parents). The reference's
        // Prism-child walk reaches them; the port mirrors it span-wise.
        file_orphan_defs(ast, &declared, &visited, &mut tables);
        // `apply_alias_def_nodes`: an `alias_method :new, :old` whose `old`
        // names a `def` hands the new name `old`'s def node — which the
        // merge's `subtract_def_methods` then strips cross-file. Resolving
        // against the COMPLETE per-file def table keeps it order-free.
        // `adopted` remembers each def-backed alias so Pass 4a's fold table
        // can adopt `old`'s captured tail for `new` below — the reference
        // folds an alias through the shared def node, so `T.new.cb_al` types
        // `upcase`'s return, not Dynamic (issue #187).
        let mut adopted: Vec<(String, String, String)> = Vec::new();
        for (key, new_name, old_name) in std::mem::take(&mut tables.pending_aliases) {
            if tables.def_names.get(&key).is_some_and(|defs| defs.contains(&old_name)) {
                tables.def_names.entry(key.clone()).or_default().insert(new_name.clone());
                tables
                    .file_methods
                    .entry(key.clone())
                    .or_default()
                    .insert(new_name.clone());
                if key == "Object" {
                    tables.file_toplevel.insert(new_name.clone());
                }
                adopted.push((key, new_name, old_name));
            }
        }
        h.toplevel_defs = std::mem::take(&mut tables.toplevel);
        h.macro_methods = std::mem::take(&mut tables.macro_methods);
        h.def_names = std::mem::take(&mut tables.def_names);
        h.file_defs = FileDefs {
            toplevel: std::mem::take(&mut tables.file_toplevel),
            methods: std::mem::take(&mut tables.file_methods),
        };

        // rigor-rs#140: EVERY def name this file defines in any `def` form —
        // instance, `def self.x`, receiver-bearing — flattened with no
        // owner; the reference's `project_defines_anywhere?` union
        // (`block_call_timing.rb`).
        for (_, node) in ast.iter() {
            if matches!(node, Node::Definition { .. }) {
                h.defined_method_names.extend(def_names(node));
            }
        }

        // Pass 1e: the caller-side half of `MutationWidening` — which positional
        // parameter of each project method the method mutates in place. See
        // `mutated_params`.
        for (_, node) in ast.iter() {
            let Node::Definition { params: Some(names), span, .. } = node else {
                continue;
            };
            if names.is_empty() {
                continue;
            }
            for (_, inner) in ast.iter() {
                let Node::Call { receiver: Some(r), method, span: cspan, .. } = inner else {
                    continue;
                };
                if !(span.0 <= cspan.0 && cspan.1 <= span.1) {
                    continue; // not inside this def.
                }
                if !crate::MUTATOR_METHODS.contains(&method.as_str()) {
                    continue;
                }
                let Node::LocalVariableRead { name: recv_name, .. } = ast.get(*r) else {
                    continue;
                };
                if let Some(i) = names.iter().position(|p| p == recv_name) {
                    // The def's NAME: a receiver-bearing def carries it in
                    // `receiver_def_name` / `singleton_name` instead.
                    for key in def_names(node) {
                        h.mutated_params.entry(key).or_default().insert(i);
                    }
                }
            }
        }

        // C5a: this file's lexically-qualified `CONST = <literal>` writes, in walk
        // order, with a per-file write COUNT per qualified name. The project-wide
        // single-assignment gate is the merge's job (see [`Self::merge`]); the
        // count is what lets it see an INTRA-file duplicate too.
        let mut seen_writes: HashMap<String, usize> = HashMap::new();
        // Issue #540 — the file's OWN mutation census, consulted as each write is
        // recorded (see `mutated_constant_names`).
        let mutated = mutated_constant_names(ast);
        collect_literal_constants(
            ast,
            ast.root(),
            &[],
            &mut h.constant_writes,
            &mut seen_writes,
            &mutated,
        );
        // Stage 2b: every constant this file ASSIGNS, by bare name — recorded
        // BEFORE the merge's C5 gates drop the non-literal / multiply-assigned
        // ones, because the RBS-object-constant arm must decline on those too.
        for w in &h.constant_writes {
            let bare = w.qualified.rsplit("::").next().unwrap_or(&w.qualified).to_string();
            h.constant_write_bare_names.insert(bare);
        }

        // Pass 2: every `ConstantRead` whose `name` the FROZEN core knows, so the
        // merge can register an instance-class id for it. This lets both
        // `Pathname.new(...)` instances AND bare singleton constants (`Time`,
        // `Array`, ...) carry a registry identity that round-trips for rendering.
        //
        // ADR-0042 Slice 2: a QUALIFIED RBS-known constant read (`ERB::Util`)
        // counts too, so it carries a registry id that round-trips for
        // `Singleton` rendering. `knows_class` (short key) covers top-level and
        // the merged composite; the added `knows_qualified_class` covers a
        // namespaced name the short map lacks.
        //
        // Today's third term — `!idx.classes.contains_key(name)` — is NOT
        // reproduced, and cannot change the result: a source class was already
        // registered by Pass 1, and `register` is idempotent, so the skipped call
        // was a no-op (issue #92 §2.1, pinned by `register_is_idempotent`).
        let mut seen_names: HashSet<&str> = HashSet::new();
        for (_, node) in ast.iter() {
            if let Node::ConstantRead { name, .. } = node {
                if !name.is_empty()
                    && (core.knows_class(name) || core.knows_qualified_class(name))
                    && seen_names.insert(name.as_str())
                {
                    h.rbs_constant_names.push(name.clone());
                }
            }
        }

        // Pass 4a (ADR-0038): every project instance + singleton `def` body by
        // QUALIFIED owner name (the same lexical walk, so `module Gitlab; module
        // Database` keys `Gitlab::Database` — matching a
        // `Gitlab::Database.read_only?` receiver). FILE-RELATIVE: the merge
        // stamps the slice position on to build each `FoldSite`.
        walk_fold_defs(ast, ast.root(), &[], &mut h.fold_defs);
        // A def-backed `alias`/`alias_method` folds the TARGET's tail (the
        // reference hands the new name the same DefNode): clone `old`'s
        // captured site under `new`. The site clone is per-file — a reopen
        // disagreement between two files still declines in `fold_key_sites`.
        for (key, new_name, old_name) in &adopted {
            let adoptions: Vec<HarvestedFoldDef> = h
                .fold_defs
                .iter()
                .filter(|d| d.owner == *key && d.method == *old_name)
                .map(|d| HarvestedFoldDef {
                    owner: d.owner.clone(),
                    method: new_name.clone(),
                    kind: d.kind,
                    tail: d.tail.clone(),
                    has_explicit_return: d.has_explicit_return,
                })
                .collect();
            h.fold_defs.extend(adoptions);
        }

        h
    }

    /// **Issue #92 — the SERIAL half.** Fold per-file [`Harvest`]es into one
    /// project index, then run every genuinely cross-file pass over the complete
    /// state. `files` pairs each harvest with ITS OWN AST, in the caller's file
    /// order.
    ///
    /// ## The order is normative — never sort `files`
    ///
    /// Today's order is `expand_check_paths`' (each directory argument expands to
    /// its recursive `**/*.rb` SORTED, arguments concatenated in ARGUMENT order),
    /// and it reaches diagnostics twice: `method_visibilities` is first-write-wins
    /// and `includes` is an ordered append, so `rigor check a.rb b.rb` and
    /// `rigor check b.rb a.rb` legitimately differ (issue #92 §3.2/§3.5).
    /// Normalising the order here would be a behaviour change, not a cleanup.
    ///
    /// ## Three phases, in this order
    ///
    /// * **M1 — ordered replay.** Each ordered harvest field, replayed pass by
    ///   pass across all files (pass by pass, NOT file by file: `names` is
    ///   appended by Pass 1 and Pass 2 both, so the ClassId order is the pass
    ///   order interleaved with the file order).
    /// * **M2 — barrier aggregates.** Cheap, need the complete replayed state:
    ///   the C1 constant-shadow tables, the C5b literal-constant gates, the Pass
    ///   2b tuple-element registry + declaration-only set.
    /// * **M3 — AST-consuming passes.** Pass 3 (tier-4b returns, typed against
    ///   the complete index) and Pass 4 (the definers inversion + the
    ///   interprocedural literal-tail fold, which resolves calls into OTHER
    ///   files' bodies). These are why the merge still takes the ASTs — issue #92
    ///   §5: harvest-then-evict is NOT unblocked by this decomposition.
    ///
    /// ## Why the harvest is BORROWED (`H: Borrow<Harvest>`)
    ///
    /// The merge only ever READS each harvest, so the parameter is generic over
    /// anything that lends one out: `check` passes the owned `Harvest`es its
    /// stage-1 rayon closure just produced (`H = Harvest`), while the LSP passes
    /// `&Harvest` borrowed from the per-file harvests tier 1 HOLDS across
    /// keystrokes (`Arc<Harvest>`, so a context swap stays a pointer copy). An
    /// owned-only parameter would force the LSP to re-harvest every project file
    /// on every dispatch — which is exactly the cost the held table removes. No
    /// behaviour rides on this: `H` is erased before the first read.
    pub fn merge<H: Borrow<Harvest>>(files: &[(H, &LoweredAst)], core: &CoreIndex) -> Self {
        let mut idx = SourceIndex::default();

        // === M1: ordered replay ============================================

        // Pass 1: source class/module structure, across ALL files in order.
        for (h, _) in files {
            let h = h.borrow();
            for c in &h.source_classes {
                idx.add_source(&c.name, c.superclass.clone(), &c.methods);
            }
        }

        // Pass 1b: the lexically-qualified override index. First-write-wins on
        // superclass + visibility, ordered append-with-dedup on includes — so
        // this replay is exactly today's call sequence.
        for (h, _) in files {
            let h = h.borrow();
            for oc in &h.override_classes {
                idx.ingest_override_class(
                    &oc.qualified,
                    oc.superclass.clone(),
                    &oc.methods,
                    &oc.method_visibilities,
                    &oc.includes,
                    oc.is_module,
                );
            }
        }

        // Passes 1c / 1d / 1e + stage 2b's bare-name set: unions — the two
        // def-attribution halves need the COMPLETE project census before the
        // `subtract_def_methods` barrier, so the union collects both tables
        // first and subtracts once (order-free either way).
        let mut union_def_names: HashMap<String, HashSet<String>> = HashMap::new();
        for (i, (h, ast)) in files.iter().enumerate() {
            let h = h.borrow();
            idx.file_index.insert(ast.file_key().clone(), i);
            idx.toplevel_defs.extend(h.toplevel_defs.iter().cloned());
            idx.defined_method_names.extend(h.defined_method_names.iter().cloned());
            for (owner, methods) in &h.macro_methods {
                idx.discovered_methods
                    .entry(owner.clone())
                    .or_default()
                    .extend(methods.iter().cloned());
            }
            for (owner, defs) in &h.def_names {
                union_def_names
                    .entry(owner.clone())
                    .or_default()
                    .extend(defs.iter().cloned());
            }
            idx.file_defs.push(h.file_defs.clone());
            for (method, indices) in &h.mutated_params {
                idx.mutated_params
                    .entry(method.clone())
                    .or_default()
                    .extend(indices.iter().copied());
            }
            idx.project_constant_write_names.extend(h.constant_write_bare_names.iter().cloned());
        }
        // `subtract_def_methods` (`finalize_def_index`): cross-file method
        // suppression is for the project's OWN accessors/aliases — NOT plain
        // `def`s, a cross-file `def` being the ADR-17 monkey-patch case the
        // check surfaces. The `Object` slice feeds `toplevel_defs` under the
        // same rule — an `Object.class_eval { attr_reader :a }` name is
        // bare-callable in EVERY file, while an `Object.class_eval { def a }`
        // name is per-file only (it lives in `file_defs.toplevel`).
        for (owner, defs) in &union_def_names {
            if let Some(methods) = idx.discovered_methods.get_mut(owner) {
                methods.retain(|m| !defs.contains(m));
            }
        }
        let object_macros: Vec<String> = idx
            .discovered_methods
            .get("Object")
            .map(|ms| ms.iter().cloned().collect())
            .unwrap_or_default();
        idx.toplevel_defs.extend(object_macros);

        // Pass 2: register the RBS-known constant reads. Runs AFTER Pass 1's
        // registrations, exactly as before — the two share the `names` vector, so
        // this is the ClassId order.
        for (h, _) in files {
            let h = h.borrow();
            for name in &h.rbs_constant_names {
                idx.register(name);
            }
        }

        // === M2: barrier aggregates ========================================

        // C1: derive the constant-shadow tables from the lexically-qualified
        // override index built above (the same class/module set Ruby's lexical
        // constant lookup sees). A key with no `::` is a TOPLEVEL definition
        // (shadows everywhere); a namespaced key contributes its containing
        // namespace under the constant's last segment (shadows only where
        // lexically visible). Collected keys first to satisfy the borrow checker.
        let qualified_defs: Vec<String> = idx.override_classes.keys().cloned().collect();
        for qualified in &qualified_defs {
            let segs: Vec<&str> = qualified.split("::").collect();
            let Some((name, ns)) = segs.split_last() else { continue };
            if ns.is_empty() {
                idx.toplevel_constants.insert((*name).to_string());
            } else {
                let ns_vec: Vec<String> = ns.iter().map(|s| (*s).to_string()).collect();
                let entry = idx.nested_constant_namespaces.entry((*name).to_string()).or_default();
                if !entry.contains(&ns_vec) {
                    entry.push(ns_vec);
                }
            }
        }

        // C5b: the project-wide constant gates. A QUALIFIED name qualifies iff it
        // is assigned EXACTLY ONCE project-wide, its RHS harvested to a
        // `ConstLit` (fully literal), and its bare name does NOT also name a
        // class/module. Ambiguity (multiple writes to the same qualified name, a
        // non-literal RHS, a class-name collision) declines. The recorded value
        // is keyed by BARE name + DEFINING NAMESPACE so the use-site consults it
        // lexically — a constant only visible in its defining namespace never
        // folds at an unrelated use site (the app/models concern-constant FP).
        //
        // `lit_first` keeps the FIRST write in file-then-walk order and
        // `lit_writes` sums the per-file counts, so a duplicate ACROSS files and
        // a duplicate WITHIN one file decline identically — which is what today's
        // single shared `lit_first`/`lit_multi` pair does.
        //
        // The `file` stamp comes from the paired AST, never from the harvest —
        // see the note on `HarvestedConst`.
        let mut lit_first: HashMap<String, (Vec<String>, FileKey, Option<ConstLit>)> =
            HashMap::new();
        let mut lit_writes: HashMap<String, usize> = HashMap::new();
        for (h, ast) in files {
            let h = h.borrow();
            for w in &h.constant_writes {
                *lit_writes.entry(w.qualified.clone()).or_insert(0) += w.writes;
                lit_first
                    .entry(w.qualified.clone())
                    .or_insert_with(|| (w.namespace.clone(), ast.file_key().clone(), w.lit.clone()));
            }
        }
        for (qualified, (namespace, file, lit)) in lit_first {
            if lit_writes.get(&qualified).is_some_and(|n| *n >= 2) {
                continue;
            }
            let bare = qualified.rsplit("::").next().unwrap_or(&qualified).to_string();
            // A constant is never a class/module: a name collision (the qualified
            // name names an override class, or the bare name a source class)
            // declines — the singleton / source-class path owns that name.
            if idx.override_classes.contains_key(&qualified) || idx.classes.contains_key(&bare) {
                continue;
            }
            if let Some(l) = lit {
                // Stage 2e: the qualified twin, keyed by the full path so a
                // `::A::B::C::CONST` read resolves. Same entry set, same gates.
                idx.qualified_literal_constants
                    .insert(qualified, (namespace.clone(), file.clone(), l.clone()));
                idx.literal_constants.entry(bare).or_default().push((namespace, file, l));
            }
        }

        // Pass 2b (MultiWrite substrate Slice 2): register an id for every class
        // an RBS TUPLE return names as an element. Pass 2 above can only see
        // classes the SOURCE mentions, but a tuple element is reached THROUGH a
        // call — `Process.wait2 : [Integer, Process::Status]` names
        // `Process::Status` in no source file — so without this the element has
        // no registry identity and its `Nominal` cannot be minted (the slot would
        // silently degrade to `Dynamic[top]`).
        //
        // Declaration-driven, not name-driven: the set is whatever the loaded RBS
        // declares (see `CoreIndex::tuple_return_class_names`), so no class name
        // is special-cased here. A name that is already a source class keeps the
        // source registration (the project's own class wins, as everywhere else),
        // and an element the loaded RBS does not model is skipped — an
        // unregistered name simply leaves that slot `Dynamic[top]` (silent).
        //
        // CROSS-FILE by construction: `!name_to_id.contains_key` asks "did NO
        // analyzed file name this class?", which no per-file harvest can answer.
        for name in core.tuple_return_class_names() {
            if !idx.classes.contains_key(name)
                && (core.knows_class(name) || core.knows_qualified_class(name))
            {
                // A name the source ALREADY registered (a class it declares, or
                // a constant it reads) is not declaration-only — see
                // `is_declaration_only_class`.
                if !idx.name_to_id.contains_key(name) {
                    idx.declaration_only_classes.insert(name.to_string());
                }
                idx.register(name);
            }
        }

        // === M3: AST-consuming passes ======================================

        let asts: Vec<&LoweredAst> = files.iter().map(|(_, ast)| *ast).collect();

        // Pass 3 (ADR-0023 tier-4b): infer per-method RETURN types. Runs AFTER the
        // source/registry maps are complete (so a Typer over `&idx` sees every
        // project class), and produces a fresh map that is then assigned — we must
        // NOT mutate `idx.method_returns` while `&idx` is immutably borrowed for
        // typing, so the inference returns a value.
        let (returns, param_bound) = infer_method_returns(&idx, core, &asts);
        idx.method_returns = returns;
        idx.param_bound_returns = param_bound;

        // Pass 4 (ADR-0038): interprocedural literal-tail return folding. Runs
        // AFTER Pass 1b (`override_classes`, the ancestry the degrade + implicit-
        // self resolution walk) and needs no `core`/typing state. Joins the
        // harvested def sites by key, inverts to a definers index, then folds each
        // method's CAPTURED tail to a scalar literal (resolving nested project
        // calls — into other files' harvests — and applying the overridable
        // degrade).
        //
        // Issue #113: the site borrows the mini-tree its harvest owns, so this
        // pass reads no AST at all. `asts` above is Pass 3's alone.
        let mut defs: FoldDefs<'_> = FoldDefs::new();
        for (h, _) in files {
            for d in &h.borrow().fold_defs {
                defs.entry((d.owner.clone(), d.method.clone(), d.kind)).or_default().push(
                    FoldSite { tail: &d.tail, has_explicit_return: d.has_explicit_return },
                );
            }
        }
        idx.definers = invert_definers(&defs);
        idx.literal_returns = idx.compute_literal_returns(&defs);

        idx
    }
}

/// Every name a `Definition` node is callable under: its instance name, or the
/// method name of a `def self.x` / `def Recv.x` singleton. Empty for anything
/// else (a `class << X` body).
pub(crate) fn def_names(node: &Node) -> Vec<String> {
    match node {
        Node::Definition { name, singleton_name, receiver_def_name, .. } => {
            [name, singleton_name, receiver_def_name].into_iter().flatten().cloned().collect()
        }
        _ => Vec::new(),
    }
}
