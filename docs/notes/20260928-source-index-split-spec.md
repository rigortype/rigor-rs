# rigor-infer source_index.rs module split — handoff spec

**Tooling note (2026-09-28):** #257 and #259 (merged) close the two tooling gaps this spec measured. The 4 `private_interfaces` bumps that §3.2–§4 call "hand bumps" are now made by fixvis itself: the replays of PRs 3, 4 and 6 bump `HarvestedConstWrite`, `FoldSite`, `HarvestedFoldDef` and `HarvestedOverrideClass` with no hand step. split_mod refuses to leave an impl empty, so PR 7 must use `impl:SourceIndex`. Read "by hand" below as "by fixvis"; the counts do not change.

2026-09-28. `crates/rigor-infer/src/source_index.rs` is **4,650 lines** at
`origin/master` `4bec095`. Its three test modules left in #220 (to
`src/source_index/{tests,probes_s92,probes_s94}.rs`). This spec splits the
production code into per-pass child modules. It is a **move-only refactor**:
no behaviour change, no renames, and visibility changes only where rustc asks
for them (as an error or as a `private_interfaces` warning). The review checks
"same code, new files", not parity.

**Precedents:** #204 (`20260927-lib-rs-split-outcome.md`) and #234
(`20260928-rules-lib-split-spec.md`, whose format this follows). Tooling:
`harness/split/`.

**How the numbers were measured.** Unlike the #234 spec, every per-PR number
below comes from a **dry run**, not from reading the code. The dry run executed
P0 and then both layouts with the real tools on a scratch copy of `4bec095`:
the eight-PR variant (§2) first, then the seven-PR default, branched after PR 4.
After every step it checked:
- `verify_move` passed;
- `cargo test -p rigor-infer -- --list` was identical (322);
- the rustdoc warning set was identical (28);
- the public page list was unchanged, with the exception noted in 3.9;
- `cargo check` of the dependents was clean;
- `cargo +1.88.0 clippy -p rigor-infer --all-targets -- -D warnings` exited 0.

At both end states, `cargo test -p rigor-infer` passed (322),
`cargo check --workspace --all-targets` was clean, and workspace clippy 1.88
with `-D warnings` exited 0.

Two gates were **not** run: `gate.sh`'s `run_snapshot.rb` and the release
sweep. The scratch copy had no `reference/rigor`, and neither gate is
meaningful before the code is on a branch. The logs are not committed.
Re-running the sequence reproduces them.

**Why now:**
- `source_index.rs` has 41 commits in total, 24 of them since 2026-08-01.
- None of the three open PRs (#249, #251, #254) touches `crates/rigor-infer/`.
- #233 does not apply (3.7).

**Execution shape:** P0, a small doc-only PR, first. Then one PR per module, in
the order below. File an umbrella issue with this spec as the agent brief, as
#204 and #234 did.

Line numbers are at `4bec095`. P0 adds 16 lines inside lines 1–465, so later
numbers shift by up to 16. Selectors are by name, so the shift does not matter
to the tools.

## 1. File shape (verified against `4bec095`)

`rsitems` lists 79 top-level rows:
- 5 `use`, 3 `mod`, and 71 items: 40 fn, 13 struct, 7 const, 6 enum,
  3 type and 2 impl;
- 46 impl children: 45 `SourceIndex` methods and `DefCx::self_prefix`.

There are 0 statics, 0 `macro_rules!`, and 0 nested items (only closures).
There is no `pub(crate)` anywhere in the file today.

| lines | section | items |
|---|---|---|
| 1–38 | `//!` docs (no inner attributes) | — |
| 40–45 | imports: `std::borrow::Borrow`; `std::collections::{HashMap, HashSet, VecDeque}`; `rigor_index::CoreIndex`; `rigor_parse::{FileKey, LoweredAst, MethodBody, Node, NodeId, Span, Visibility}`; `rigor_types::{ClassId, Interner, Scalar, ShapeKey}` | 5 `use` |
| 47–657 | **data model** | `ConstLit`, `SOURCE_CLASS_BASE`, `OVERRIDE_ANCESTOR_WALK_LIMIT`, `FOLD_DEPTH_CAP`, `DefKind`, `FoldSite`, `FoldDefs`, `FoldTail`, `FoldExpr`, `SourceClass`, `OverrideClass`, `HarvestedConst`, `AncestorClosures`, `HarvestedClass`, `HarvestedOverrideClass`, `HarvestedConstWrite`, `HarvestedFoldDef`, `FileDefs`, `Harvest` (12 private fields), `SourceIndex` (20 private fields), `ParamBoundReturn` |
| 659–2250 | `impl SourceIndex` (1,592 lines) | 45 methods, 30 `pub`. Groups: build/harvest/merge 660–1188; def and existence queries 1190–1289; constant queries 1291–1450; class registry 1452–1579; returns and fold lookups 1581–1659; ADR-35 ancestry 1661–1855; the Pass-4b fold (banner at 1857) 1857–2185; `class_has_method` 2187–2249 |
| 2252–2483 | tier-4b inference | `infer_method_returns`, `infer_one_return`, `infer_one_param_bound` |
| 2485–2656 | ADR-35 walk, qualification, lexical scopes | `collect_override_classes`, `qualify`, `def_names`, `split_qualified`, `lexical_scopes` (pub), `method_body_spans` (pub), `collect_lexical_scopes` |
| 2658–4215 | def attribution (banner 2658–2673) | 4 consts, `DefsSide`, `DefCx` and `impl DefCx`, `def_root_cx`, `DefTables`, 12 walk and filing fns, `DefFrame`, `file_orphan_defs`, `def_walk_children`, and the census (`collect_declared_names`, `add_declared`, `collect_declared_names_at`). 29 items |
| 4216–4422 | C5 constant harvest (4216–4218 are three blank lines) | `const_shape_key_of`, `collect_literal_constants`, `widen_if_mutated`, `mutated_constant_names`, `const_lit_of` |
| 4424–4583 | fold capture | `scalar_truthy`, `walk_fold_defs`, `capture_fold_tail`, `invert_definers` |
| 4585–4598 | tier-4b gate | `is_branch_carrier` |
| 4600–4650 | tests | three `#[cfg(test)] mod …;`, and two banners above `probes_s92` and `probes_s94` |

**Public surface.**
- 10 `pub` top-level items: `ConstLit`, `SOURCE_CLASS_BASE`,
  `OVERRIDE_ANCESTOR_WALK_LIMIT`, `DefKind`, `FileDefs`, `Harvest`,
  `SourceIndex`, `ParamBoundReturn`, `lexical_scopes` and `method_body_spans`.
- 30 `pub` methods on `SourceIndex`.
- `rigor-infer/src/lib.rs:68` re-exports 8 of the items:
  `pub use source_index::{lexical_scopes, method_body_spans, ConstLit, DefKind, Harvest, ParamBoundReturn, SourceIndex, SOURCE_CLASS_BASE}`.
- `FileDefs` and `OVERRIDE_ANCESTOR_WALK_LIMIT` are reachable only at
  `rigor_infer::source_index::…`, and 0 files outside `source_index*` name
  them.

**External callers.** These are files outside `source_index*` that name each
item. Other crates use only root paths (`rigor_infer::SourceIndex`,
`rigor_infer::Harvest`, `rigor_infer::lexical_scopes`, …).

| item | files | by crate |
|---|---|---|
| `SourceIndex` | 36 | rigor-infer 13, rigor-rules 11, rigor-cli 10, rigor-parse 2 (comments only) |
| `lexical_scopes` | 9 | rigor-infer 6, rigor-rules 2, rigor-cli 1 |
| `Harvest` | 6 | rigor-cli 4, rigor-infer 1 (`lib.rs`), rigor-parse 1 (comment) |
| `method_body_spans` | 2 | `lib.rs`, rigor-rules `scope.rs` |
| `ConstLit` | 2 | `lib.rs`, `expr_type.rs` |
| `DefKind` | 2 | `lib.rs`, `flow_eval.rs` |
| `ParamBoundReturn` | 2 | `lib.rs`, `call_dispatch.rs` |
| `SOURCE_CLASS_BASE` | 1 | `lib.rs` |

The only `source_index::` paths outside the file are `lib.rs:68` and a doc link
in `typer.rs:91` (`crate::source_index::lexical_scopes`). Both stay valid.
Methods are reached by method-call syntax, which does not depend on the module
that holds the `impl`.

**What the parent keeps (end state: 668 lines, measured):**
- the `//!` docs, plus P0's three `//!` link targets;
- the imports `std::collections::{HashMap, HashSet}`,
  `rigor_parse::{FileKey, Visibility}` and `rigor_types::{Scalar, ShapeKey}`;
- 7 `mod X;` lines, 5 `pub(crate) use X::*;` globs and
  `pub use override_index::{lexical_scopes, method_body_spans};`;
- the **data model**, 17 items: `ConstLit`, `SOURCE_CLASS_BASE`,
  `OVERRIDE_ANCESTOR_WALK_LIMIT`, `DefKind`, `FoldSite`, `FoldDefs`,
  `SourceClass`, `OverrideClass`, `HarvestedConst`, `HarvestedClass`,
  `HarvestedOverrideClass`, `HarvestedConstWrite`, `HarvestedFoldDef`,
  `FileDefs`, `Harvest`, `SourceIndex` and `ParamBoundReturn`;
- the three `#[cfg(test)] mod …;` declarations with their banners, unchanged.
  Test paths do not change, so the test names stay the same.

At `4bec095` these are lines 1–120, 130–162, 208–267, 283–657 and 4599–4650:
640 lines.

**Why types stay in the parent.** The parent's private fields are visible to
all its descendants. Every child module, and every test module, can therefore
read and write the 20 `SourceIndex` fields and the 12 `Harvest` fields without
any `pub(crate)`.

Measured at the end state, the children touch these private `SourceIndex`
fields:

| module | `SourceIndex` fields |
|---|---|
| `harvest` | 19 (all but `names`) |
| `registry` | 9 |
| `constants` | 6 |
| `literal_fold` | 2 |
| `method_returns` | 2 |
| `override_index` | 1 |
| `probes_s92` | 19 |

`harvest` also touches all 12 `Harvest` fields, and `probes_s92` touches 5.
None of these accesses needs a `pub(crate)`.

Types move only when they are private to one family: the fold's captured
tail (`FoldTail`, `FoldExpr`, `FOLD_DEPTH_CAP`), its memo (`AncestorClosures`),
and the def walk's context and accumulators (`DefsSide`, `DefCx`, `DefTables`,
`DefFrame`).

## 2. Target layout

The existing files stay as they are: `tests.rs`, `probes_s92.rs` and
`probes_s94.rs`. No new name clashes with them, with an item, or with an
intra-doc link (grepped: no `[`harvest`]`-style bare link to a module name
exists).

The rows are in extraction order, leaf-first:
- first, the passes only `harvest`/`merge` call (`method_returns`, `registry`,
  `constants`, `literal_fold`);
- then the def walk;
- then the shared provider `override_index`. Its `qualify`, `split_qualified`
  and `override_ancestor_names` are called by `constants`, `literal_fold` and
  `registry`;
- last, the hub `harvest`, which calls every module.

"Moved" is split_mod's own count, with gaps included. "Ranges" are the
gap-inclusive line ranges at `4bec095`. Every number is measured.

| # | module | selectors (in file order) | moved | ranges | file |
|---|---|---|---|---|---|
| P0 | (prep, doc-only) | see §4 | +16 | — | — |
| 1 | `method_returns.rs` | `SourceIndex::method_return SourceIndex::param_bound_return infer_method_returns infer_one_return infer_one_param_bound is_branch_carrier` | 273 | 1580–1590, 1629–1642, 2251–2483, 4584–4598 | 285 |
| 2 | `registry.rs` | `SourceIndex::project_declares_method SourceIndex::method_mutates_param SourceIndex::is_toplevel_def SourceIndex::project_defines_method_name SourceIndex::register SourceIndex::add_source SourceIndex::knows_class SourceIndex::discovered_superclass SourceIndex::is_registered SourceIndex::is_declaration_only_class SourceIndex::class_id SourceIndex::class_name_for_id SourceIndex::class_name_for_id_of SourceIndex::project_declares_method_through_ancestors SourceIndex::class_has_method` | 316 | 1189–1289, 1451–1467, 1505–1579, 1643–1659, 1735–1776, 2186–2249 | 328 |
| 3 | `constants.rs` | `SourceIndex::literal_constant SourceIndex::literal_constant_visible_any_file SourceIndex::project_writes_constant SourceIndex::qualified_literal_constant SourceIndex::constant_shadowed SourceIndex::constant_defined_anywhere const_shape_key_of collect_literal_constants widen_if_mutated mutated_constant_names const_lit_of` | 368 | 1290–1450, 4216–4422 | 377 |
| 4 | `literal_fold.rs` | `FOLD_DEPTH_CAP FoldTail FoldExpr AncestorClosures SourceIndex::const_singleton_literal SourceIndex::implicit_self_literal SourceIndex::compute_literal_returns SourceIndex::resolve_fold_key SourceIndex::fold_key_sites SourceIndex::fold_tail SourceIndex::resolve_instance_owner SourceIndex::owner_defines SourceIndex::overridden_in_project SourceIndex::ancestor_closure SourceIndex::build_ancestor_closure SourceIndex::related_to_owner scalar_truthy walk_fold_defs capture_fold_tail invert_definers` | 598 | 121–129, 163–207, 268–282, 1591–1628, 1856–2185, 4423–4583 | 612 |
| 5 | `def_attribution.rs` | `RECEIVER_EVAL_METHODS INSTANCE_EVAL_METHODS META_NEW_SELECTORS ANONYMOUS_META_OWNER DefsSide DefCx impl:DefCx def_root_cx DefTables decl_body_cx walk_defs file_def file_alias module_attr_shape literal_method_name file_call_methods singleton_operand_prefix eval_receiver_prefix eval_const_prefix collapse_object_owner is_meta_new_call meta_new_rvalue qualify_vec DefFrame file_orphan_defs def_walk_children collect_declared_names add_declared collect_declared_names_at` | 1,559 | 2657–4215 | 1,566 |
| 6 | `override_index.rs` | `SourceIndex::namespace_children SourceIndex::method_visibility SourceIndex::nearest_ancestor_defining SourceIndex::override_ancestor_names SourceIndex::resolve_override_ancestor SourceIndex::ingest_override_class collect_override_classes qualify split_qualified lexical_scopes method_body_spans collect_lexical_scopes` | 352 | 1468–1504, 1660–1734, 1777–1855, 2484–2572, 2585–2656 | 362 |
| 7 | `harvest.rs` | `impl:SourceIndex def_names` (by then the impl holds only `build`, `build_project`, `harvest` and `merge`; see 3.8) | 544 | 658–1188, 2250, 2573–2584 | 559 |

The seven rows move 54 top-level items and all 45 methods: 4,010 of 4,650
lines. The other 17 top-level items are the data model the parent keeps.

**Why these seams**

Churn below counts the 22 non-mechanical commits since 2026-08-01 that touched
each future file.

- **`harvest`** holds the #92 pair `harvest`/`merge` together with
  `build`/`build_project`. A new pass edits both halves, so keeping them in one
  file means one file per such PR. It is the hottest behaviour file (11
  commits).
- **`registry`** holds the `ClassId` registry together with every
  method-existence gate the rules consult. That includes
  `project_declares_method_through_ancestors`, which walks override ancestors
  but answers an existence question.
- **`constants`** puts the C5 harvest writers next to their C1/C5/stage-2
  readers (9 commits).
- **`literal_fold`** owns the #113 captured-tail IR and the #94 closure memo,
  so both land in one file.
  - It is not named `fold`, to avoid confusion with `crate::folding::fold`.
- **`override_index`** takes the per-file lexical scopes.
  - `collect_lexical_scopes` mirrors `collect_override_classes`.
  - Keeping `lexical_scopes`' public doc and its link to the private
    `collect_override_classes` in one module keeps that existing
    `rustdoc::private_intra_doc_links` warning byte-identical.
- **`def_attribution`** takes the declared-constant census (see the alternative
  below). In 2 of 2 commits that touched the census, #141's `9b87b58` and
  `4d41936`, the same commit also touched the walk.

**Eight-PR alternative (also measured):** split the census
(`collect_declared_names add_declared collect_declared_names_at`, 153 lines,
file 161) into `declared_names.rs`, extracted before `def_attribution`.

`def_attribution` then shrinks to 1,406 moved lines (file 1,412), which is
under 1,500. The cost is:
- one more PR;
- 4 more `pub(crate)`: `def_walk_children`, `is_meta_new_call`,
  `meta_new_rvalue` and `qualify_vec`, because the census calls them across
  the boundary;
- the default's census-and-walk co-change becomes a two-file PR.

Per-PR `pub(crate) added` (verify_move's count, which includes glob lines) is
2, 2, 6, 16, 2, 16, 7, 1, for 52 in total. The census PR adds 1 bump plus its
glob, and `def_attribution` adds 15.

**Proposed `//!` first lines** (humans write the headers):
1. "ADR-0023 tier-4b: project method RETURN inference (Pass 3) and the call-site parameter-binding descriptors (Pass 3b), with the lookups the call hook reads."
2. "The per-run instance-class registry (`ClassId` <-> name) and the method-existence gates `call.undefined-method` and `call.unresolved-toplevel` consult."
3. "Project constants: the C5 literal-constant harvest and its lexical, per-file lookups, the stage-2 write census, and the C1 constant-shadow gate."
4. "ADR-0038 interprocedural literal-tail fold: the Pass-4a tail capture (issue #113), the Pass-4b fold with its overridable degrade (#94), and its lookups."
5. "Passes 1c/1d: the def-attribution walk (issue #141), which files every `def`-family name under the owner that binds it, its orphan post-pass, and the file's declared-constant census the walk resolves eval receivers against."
6. "ADR-35 slice 1: the lexically-qualified override index (Pass 1b, its merge replay and MRO ancestor walks), and the per-file lexical scopes that mirror it."
7. "Issue #92: `build` / `build_project`, the per-file `harvest` (parallel) and the serial `merge` that replays it and runs every cross-file pass."

## 3. Coupling hazards (each verified by the dry run)

**3.1 Cross-module edges → `pub(crate)`.**

There are 42 edges in total: 38 added by fixvis and 4 added by hand (3.2). The
crate root's `#![allow(dead_code)]` silences dead-code warnings, so a bump used
only by tests never warns.

| PR | fixvis adds `pub(crate)` on | needed by |
|---|---|---|
| 1 | `infer_method_returns` | `merge`, `probes_s92` |
| 2 | methods `register`, `add_source` (E0624) | `merge`; `probes_s92`'s `build_project_legacy` |
| 3 | `collect_literal_constants`, `mutated_constant_names` | `harvest` (and `probes_s92` for the second) |
| 3 | `const_lit_of`, `widen_if_mutated` | `probes_s92` only |
| 4 | `FoldTail` | the parent's `HarvestedFoldDef.tail` |
| 4 | `FoldExpr` | `private_interfaces`, via `FoldTail::Expr` |
| 4 | `walk_fold_defs` | `harvest` |
| 4 | `invert_definers`, method `compute_literal_returns` | `merge` |
| 4 | `AncestorClosures` | `probes_s92`, `probes_s94` |
| 4 | `FOLD_DEPTH_CAP`, `scalar_truthy`, methods `overridden_in_project`, `resolve_instance_owner` | `probes_s92` |
| 4 | methods `ancestor_closure`, `build_ancestor_closure`, `related_to_owner` | `probes_s94` |
| 5 | `DefTables` and **6 fields** (`toplevel`, `macro_methods`, `def_names`, `file_methods`, `file_toplevel`, `pending_aliases`) | `harvest` reads all six (lines 777–797), and so does `probes_s92`'s `build_project_legacy` (lines 518–558) |
| 5 | `def_root_cx`, `walk_defs`, `file_orphan_defs`, `collect_declared_names` | `harvest`, `probes_s92` |
| 5 | `DefCx` | `private_interfaces`: the return type of `def_root_cx` and a parameter of `walk_defs` |
| 6 | `qualify`, `split_qualified` | `constants`, `literal_fold`, `probes_s92` |
| 6 | `collect_override_classes` | `harvest` |
| 6 | method `override_ancestor_names` | `registry`, `literal_fold` |
| 6 | method `ingest_override_class` | `merge`, `probes_s92` |
| 7 | `def_names` | `probes_s92` |

Per PR, fixvis adds 1, 2, 4, 13, 12, 5 and 1 bumps.

- Test-only bumps (no production caller across a boundary): 11.
- Field bumps: exactly the 6 on `DefTables`. `DefCx` fields stay private,
  because only `def_attribution` reads them.
- Mutual module dependencies (`harvest` ↔ everyone) are fine in Rust.

**3.2 NEW hazard (fixed in tooling by #259; with it, fixvis bumps these itself): `private_interfaces` on a parent type. fixvis does not fix it
and exits 0.**

When a moved `pub(crate)` fn's signature names a type that is still private in
the parent, rustc warns `private_interfaces`. fixvis bumps a type only when it
is defined in MODFILE. For a parent type it prints the WARNING and still exits
0. `cargo test`, and so `gate.sh`, only warns. **CI's clippy 1.88 with
`-D warnings` fails.** This was measured on PR 3 with the hand bump reverted:
`error: type source_index::HarvestedConstWrite is more private than the item …`.

Fix: add `pub(crate) ` to the struct in `source_index.rs` by hand, then re-run
`cargo check`. Four bumps are needed, all on the parent:

| PR | parent type | named by |
|---|---|---|
| 3 | `struct HarvestedConstWrite` | `collect_literal_constants(…, out: &mut Vec<HarvestedConstWrite>, …)` |
| 4 | `struct FoldSite<'a>` | `FoldDefs<'_>` in `compute_literal_returns` and `invert_definers`. The alias is looked through: the warning names `FoldSite<'_>`, not `FoldDefs` |
| 4 | `struct HarvestedFoldDef` | `walk_fold_defs(…, out: &mut Vec<HarvestedFoldDef>)` |
| 6 | `struct HarvestedOverrideClass` | `collect_override_classes(…, out: &mut Vec<HarvestedOverrideClass>)` |

verify_move normalises `pub(crate) ` away, so these bumps show only in its
`pub(crate) added` count. The reviewer checks them in the diff.

Pre-bumping the four types in P0 would make every move PR tool-only. It is not
recommended: it would be a visibility change rustc has not asked for yet.

**3.3 Names the tests reach through `use super::*`.**

- **`tests.rs`:**
  - parent items `SourceIndex`, `ConstLit`, `DefKind`, `OverrideClass` and
    `ParamBoundReturn`;
  - `lexical_scopes` and `method_body_spans`, through the parent's `pub use`;
  - parent imports `HashMap`, `Scalar` and `Visibility`, which stay in the
    parent;
  - `CoreIndex`, `Interner` and `LoweredAst`, which become test-only (3.4).
  - It reaches no moved private item.
- **`probes_s92.rs`:**
  - the 20 bumped names marked `probes_s92` in 3.1 (it does not name
    `collect_literal_constants` or `walk_fold_defs`: its legacy oracle carries
    its own copies);
  - all six `DefTables` fields;
  - the private fields of `SourceIndex` and `Harvest`, through its
    `fingerprint` (they stay in the parent, so no bump is needed);
  - imports `FileKey`, `HashMap`, `HashSet`, `Scalar` and `Visibility`, which
    stay;
  - imports `Interner`, `NodeId`, `CoreIndex`, `LoweredAst` and `Node`, which
    become test-only.
  - It names `ClassId` only as `rigor_types::ClassId` (line 417), so that
    import is not test-only.
- **`probes_s94.rs`:**
  - `SourceIndex` and `OVERRIDE_ANCESTOR_WALK_LIMIT` (parent);
  - `AncestorClosures` and three methods (bumped in PR 4);
  - `CoreIndex` and `LoweredAst`, which become test-only.
- Qualified paths in the tests: `crate::MUTATOR_METHODS` and
  `crate::folding::fold` in `probes_s92`, and `super::probes_s92::…` in
  `probes_s94`. The split affects none of them.

**3.4 Test-only parent imports.**

fixvis reports these as "unused in one build only". For each, delete the name
from the parent's `use`, then run `testimports.py`. Do not add `#[cfg(test)]`
imports to the parent.

| PR | test-only | `testimports.py --crate rigor-infer …` | lands in |
|---|---|---|---|
| 2 | `Interner` | `Interner=rigor_types` | `tests.rs`, `probes_s92.rs` (a new `use rigor_types::Interner;`) |
| 6 | `NodeId` | `NodeId=rigor_parse` | `probes_s92.rs` (merged into `use rigor_parse::{lower, parse, NodeId}`) |
| 7 | `CoreIndex`, `LoweredAst`, `Node`, and the glob (3.5) | `CoreIndex=rigor_index LoweredAst=rigor_parse Node=rigor_parse def_names=super::harvest` | `CoreIndex` → all 3 files; `LoweredAst` → all 3 (merged); `Node` → `probes_s92`; `use super::harvest::def_names;` → `probes_s92` |

Some imports are unused in every target, so fixvis prunes them from the parent
itself:
- #1: `MethodBody`;
- #2: `ClassId`;
- #4: `VecDeque`;
- #5: `Span`;
- #7: `Borrow`.

Measured test-file growth: `tests.rs` +2, `probes_s92.rs` +3, `probes_s94.rs`
+1 lines.

**3.5 Globs.**

| PR | glob | reason |
|---|---|---|
| 1, 3, 4, 5, 6 | kept (5 globs) | siblings and `harvest` import through them as `super::NAME` |
| 2 | dropped by fixvis | `registry` moves only methods, so there is nothing to import |
| 7 | **test-only** | fixvis keeps it with the hint. Delete `pub(crate) use harvest::*;`, then `testimports.py … def_names=super::harvest` (3.4) |

`testimports.py` works for a glob too. The test module is a descendant of
`source_index`, so it can name the private sibling module `super::harvest`.

Glob shadowing:
- the 54 moved top-level names are unique;
- none equals a parent item or a remaining parent import (`HashMap`,
  `HashSet`, `FileKey`, `Visibility`, `Scalar`, `ShapeKey`);
- children use explicit `use super::{…}` lists, never globs.

Re-check this in every PR: a later parent item would shadow a glob silently.

**3.6 Consts, type aliases, traits.**
- There are 7 consts.
  - `SOURCE_CLASS_BASE` and `OVERRIDE_ANCESTOR_WALK_LIMIT` are `pub` and stay.
    The second is read by `registry`, `literal_fold`, `override_index` and
    `probes_s94`.
  - `FOLD_DEPTH_CAP` → `literal_fold`, bumped for `probes_s92`.
  - The four def consts → `def_attribution`, private.
- There are 3 type aliases.
  - `FoldDefs` and `HarvestedConst` stay: `harvest` and a parent field name
    them.
  - `AncestorClosures` → `literal_fold`.
- The only trait import is `std::borrow::Borrow`, used by `merge`'s
  `h.borrow()`. split_mod copies every parent `use` into the new file, and
  fixvis keeps it in `harvest.rs`. A missing trait import would be a compile
  error, not a silent change.
- The moved code has no `self::`/`super::`/`crate::`-relative ambiguity. Its
  `crate::` paths are absolute: `crate::MUTATOR_METHODS`,
  `crate::is_shape_mutator`, `crate::folding::fold`, `crate::Typer`,
  `crate::TypeEnv`.

**3.7 #233 and #221 shapes: absent.**
- `impl SourceIndex` (659) and `impl DefCx` (2756) carry no outer attributes.
  Their headers are one plain `impl X {` line with nothing after the `{`.
- Across `crates/`, 0 of 133 top-level impls carry an attribute (re-scanned
  with `rsitems`). The only one-line impl is `impl Eq for Scalar {}` in
  rigor-types, a trait impl that is never split.
- **#233 item 1 (`impl-attr` counts lines) therefore cannot fire.**
- The attributes that do exist are on methods and fns, and each travels with
  its item: `#[cfg(test)]` on `related_to_owner` (2162),
  `#[allow(clippy::too_many_arguments)]` on `fold_tail` (1955) and
  `#[allow(clippy::type_complexity)]` on `infer_method_returns` (2315).
- There is no `use …::{self`, no mid-file `use`, no `macro_rules!`, and none of
  `line!`/`file!`/`column!`/`module_path!`/`include*!`/`#[path]`.
- None of the 7 target paths (or the eight-PR `declared_names.rs`) is
  git-ignored (`git check-ignore -v`, global excludes `~/.gitexclude`).

**3.8 Leftover empty impl (NEW, measured).**
- If PR 7 selects `SourceIndex::build … SourceIndex::merge`, split_mod leaves
  `impl SourceIndex {\n}` in the parent.
- It compiles, and clippy 1.88 `-D warnings` passes.
- verify_move shows only `+1 impl-frame 'impl SourceIndex {'` and
  `+1 impl-frame '}'`, which read as the wrapper's ordinary scaffold.
- **Use `impl:SourceIndex` in PR 7.** The original block then moves whole,
  verify_move prints no impl-frame row, and `grep -c '^impl ' source_index.rs`
  is 0 afterwards.
- `impl:DefCx` in PR 5 moves `DefCx`'s only impl with its type.

**3.9 Rustdoc links (P0).**

Baseline: 28 warnings with `--document-private-items`, 11 of them in
`source_index.rs`:
- 461 and 495: unresolved `build_project`;
- 645: `ParamBoundReturn` links to private `SourceIndex::param_bound_returns`;
- 1197, 1229, 1257, 1340 and 1741: public method docs linking private
  `Self::…` items;
- 2254 and 2255: unresolved `TypeEnv` and `Typer`;
- 2595: `lexical_scopes` links to private `collect_override_classes`.

Each of the 11 moves with its doc, and its text is unchanged at every step
(measured).

Without P0, the moves break 9 links that name imports the parent loses.
Measured in a first dry run:
- **PR 2 adds 5 warnings:** `unresolved link to ClassId` four times (lines 12,
  110, 287 and 447) and `Interner` once (line 50).
- **PR 7 adds 4 warnings:** `CoreIndex` (364), `CoreIndex::class_id` (464),
  and `Node::ClassDef` and `Node::ModuleDef` (9).

Three of the nine are in the `//!` header, which `doclinks.py` does not scan:
it reads `///` blocks only. P0 adds all nine targets up front (§4).

After P0, all seven PRs show an **identical warning set**, with no
`doclinks.py` step.

Links from the parent into moved private items would also break: `FoldTail`
and `FOLD_DEPTH_CAP` link `capture_fold_tail` at lines 123, 171 and 182. Those
links move into `literal_fold` together with their items, which is one reason
`FOLD_DEPTH_CAP`, `FoldTail` and `FoldExpr` move.

**Public pages.** The public page list does not change, with one exception:
- 38 pages under `target/doc/rigor_infer/`, of which 11 are directly under
  `source_index/`;
- after PR 6, rustdoc also writes two **redirect** pages,
  `source_index/override_index/fn.lexical_scopes.html` and
  `fn.method_body_spans.html`, for the re-exported `pub` fns;
- compare with `-maxdepth 1` under `source_index/`, and expect exactly those two
  new files in the full tree.

**3.10 Misplaced docs and banners.**

Every item's and method's doc opener was scanned, as was the gap above each
item. **No misplaced doc exists**; #216 fixed the last one (`SourceIndex`,
`Harvest`).

Only two non-blank gaps travel, and each belongs to its item:
- PR 4: the `ADR-0038 — interprocedural literal-tail return folding` banner
  (1857–1859), with `compute_literal_returns`;
- PR 5: the `Pass 1c/1d — def-attribution walk` banner (2658–2673), with
  `RECEIVER_EVAL_METHODS`.

The two test banners stay with their `mod probes_s9x;` lines. The triple blank
at 4216–4218 disappears in PR 3, because split_mod strips leading blanks from a
moved chunk. That is a whitespace-only change, and verify_move reads it as
blank scaffold.

**3.11 split_mod's parent wiring (cosmetic).**
- The parent has no `mod` line above its first item, so PR 1 inserts
  `mod method_returns;` plus a blank line *above* the `use` block. Later PRs
  append after it.
- Globs and the `pub use` are appended right after the last `use` line, with no
  blank line between them.
- Both compile. rustfmt is not enforced (`ci.yml`). An optional last docs PR
  can regroup the wiring and add a module map to the `//!` header, as #249 did
  for rigor-rules.

**3.12 Adjacent findings (out of scope; file separately).**
- Stale text:
  - `SourceIndex`'s doc says "Built once per file." It is built once per run,
    project-wide.
  - `harvest`'s Pass 4a comment (896–897) still says the merge "stamps the
    slice position" onto `FoldSite`. That stopped being true in #113.
  - `FoldTail`'s doc calls the retired `SourceIndex::fold_expr` the
    `probes_s92` oracle. The oracle is `legacy_fold_expr`.
  - The probes_s92 banner (4608) says "all 17 fields". The struct has 20.
- The four pre-existing unresolved links (`build_project` ×2, `TypeEnv`,
  `Typer`) should read `Self::build_project`, `crate::TypeEnv` and
  `crate::Typer`. Fixing them changes the baseline to 24, so do it outside the
  split.
- Tooling, for #233:
  - fixvis should bump a `private_interfaces` type defined in the *parent*
    file, or at least exit non-zero (3.2);
  - split_mod could warn when it empties an impl (3.8);
  - the README's page check should say `-maxdepth 1` or expect redirect pages
    (3.9).

## 4. Per-PR process

**P0 (a small PR, doc comments only, landed first).**

Add the 9 reference-style targets below by hand. `doclinks.py` would also touch
the method docs that later move, and it skips `//!`. The diff is exactly +16
lines:

| block | add |
|---|---|
| `//!` header (end, after line 38) | `//!`, then `//! [`ClassId`]: rigor_types::ClassId`, `//! [`Node::ClassDef`]: rigor_parse::Node::ClassDef`, `//! [`Node::ModuleDef`]: rigor_parse::Node::ModuleDef` |
| `ConstLit` doc (47–53) | `///`, `/// [`Interner`]: rigor_types::Interner` |
| `SOURCE_CLASS_BASE` doc (110–113) | `///`, `/// [`ClassId`]: rigor_types::ClassId` |
| `HarvestedClass` doc (284–287) | `///`, `/// [`ClassId`]: rigor_types::ClassId` |
| `Harvest` doc (363–391) | `///`, `/// [`CoreIndex`]: rigor_index::CoreIndex` |
| `SourceIndex.names` field doc (446–448, indented) | `///`, `/// [`ClassId`]: rigor_types::ClassId` |
| `SourceIndex.method_returns` field doc (459–465, indented) | `///`, `/// [`CoreIndex::class_id`]: rigor_index::CoreIndex::class_id` |

Measured gates for P0:
- tests: 322, identical;
- rustdoc: 28, **identical**, because every link already resolved and none is
  public-to-private;
- `verify_move.py 4bec095 crates/rigor-infer/src`: exit 0, showing only
  `doclink:code` and `inner-doc` rows and `pub(crate) added: 0`;
- `gate.sh` must pass. It was not run in the dry run, which had no
  `reference/rigor` for `run_snapshot.rb`, and neither was the sweep.

**Each move PR (#1–#7).**

Set up a fresh worktree on `claude/issue-N-<slug>` from its base, which is
`origin/master` or the previous stacked branch. Pass that base to every step
that takes one. Then run `git submodule update --init reference/rigor`. `S` is
a scratch directory.

```sh
# baselines, on BASE
cargo test -q --locked -p rigor-infer -- --list 2>/dev/null | grep ': test$' | sort > $S/tests.before   # 322
cargo doc -q -p rigor-infer --no-deps --document-private-items 2>&1 | grep '^warning' | sort > $S/doc.before   # 28
rm -rf target/doc/rigor_infer && cargo doc -q -p rigor-infer --no-deps \
  && find target/doc/rigor_infer -name '*.html' | sort > $S/pub.before   # 38 (40 after PR 6)
# move (sel = the row's selectors, one per line; doc = the //! header)
python3 harness/split/split_mod.py crates/rigor-infer/src/source_index.rs MOD $S/MOD.sel $S/MOD.doc   # read every printed gap (3.10)
python3 harness/split/fixvis.py --crate rigor-infer crates/rigor-infer/src/source_index/MOD.rs --prune; echo $?   # must be 0
#   exit 0 is NOT enough. Read every WARNING line fixvis prints last:
#   - private_interfaces naming a parent type → hand `pub(crate) ` on it in source_index.rs (3.2), re-run cargo check
#   - unused_imports "unused in one build only" in source_index.rs → delete it from the parent's use (or the glob, PR 7), then
python3 harness/split/testimports.py --crate rigor-infer NAME=PATH ...   # arguments per 3.4
# proofs (run each bare and read its exit code)
git check-ignore -v crates/rigor-infer/src/source_index/MOD.rs    # must print nothing
git add -A crates/rigor-infer/src
python3 harness/split/verify_move.py BASE crates/rigor-infer/src   # 0 UNEXPECTED; read every scaffold row
diff $S/tests.before <(cargo test -q --locked -p rigor-infer -- --list 2>/dev/null | grep ': test$' | sort)
diff $S/doc.before <(cargo doc -q -p rigor-infer --no-deps --document-private-items 2>&1 | grep '^warning' | sort)
rm -rf target/doc/rigor_infer && cargo doc -q -p rigor-infer --no-deps && \
  diff $S/pub.before <(find target/doc/rigor_infer -name '*.html' | sort)   # PR 6: exactly the 2 redirect pages
cargo check -q --locked --workspace --all-targets          # dependents: rigor-rules, rigor-cli, rigor-effects …
CARGO_TARGET_DIR=$S/t188 cargo +1.88.0 clippy -q --locked -p rigor-infer --all-targets -- -D warnings; echo $?   # catches a missed 3.2 bump; gate.sh does not
harness/gate.sh --base BASE; echo $?
```

Then:
1. Commit, and push with an explicit refspec:
   `git push origin HEAD:refs/heads/claude/issue-N-<slug>`.
2. Open a draft PR with `Part of #umbrella` in the body.
3. Read `gh pr checks`. CI's clippy 1.88 is the authority.
4. Before marking ready, run `cargo build --release` and
   `python3 harness/fp_audit.py --gaps --sweep`. It must show 0 FP, with
   per-corpus counts equal to master's.
5. Get the review (`docs/agents/review.md`), then `gh pr ready`.

In the PR body, record:
- the moved-line count;
- `pub(crate) added: N` against the table below;
- the hand bumps;
- the test-only imports;
- the tests (322) and rustdoc (28) results;
- `verify_move`'s result;
- the sweep numbers.

**Predicted per PR (measured on the dry run):**

| PR | fixvis bumps | hand bumps (3.2) | glob | test-only (3.4) | verify_move `pub(crate) added` |
|---|---|---|---|---|---|
| 1 method_returns | 1 | — | kept | — | 2 |
| 2 registry | 2 | — | dropped by fixvis | `Interner` | 2 |
| 3 constants | 4 | `HarvestedConstWrite` | kept | — | 6 |
| 4 literal_fold | 13 | `FoldSite`, `HarvestedFoldDef` | kept | — | 16 |
| 5 def_attribution | 12 (6 of them fields) | — | kept | — | 13 |
| 6 override_index | 5 | `HarvestedOverrideClass` | kept, plus `pub use {lexical_scopes, method_body_spans}` | `NodeId` | 7 |
| 7 harvest (`impl:SourceIndex`) | 1 | — | test-only: delete it and import in `probes_s92` | `CoreIndex`, `LoweredAst`, `Node`, `def_names` | 1 |

The total is 47: 38 fixvis bumps, 4 hand bumps and 5 kept globs.

**What the reviewer checks:**
1. The selection equals this spec's row, and no other item moved.
   - Only PRs 4 and 5 print a non-blank gap, and it is their own banner.
2. PR 5 lists `impl:DefCx`.
   - PR 7 uses `impl:SourceIndex`, and no `impl` remains in `source_index.rs`
     afterwards (3.8).
3. `verify_move` shows 0 UNEXPECTED rows. Read every scaffold row:
   - each `use super::{…}` name must resolve to the moved-away item or a parent
     item;
   - the changed test-file `use` lines match 3.4;
   - the `mod` / glob / `pub use` lines;
   - the new `//!` header.
4. `pub(crate) added` equals the table, and each bump is on the listed item.
   Check the hand bumps in the diff, because verify_move normalises them away.
5. `lib.rs` is untouched.
   - `pub use` appears only in PR 6.
   - The page tree is identical, except PR 6's two redirect pages.
   - `cargo check --workspace --all-targets` is clean.
6. The test list (322) and the rustdoc warning set (28) are identical.
7. No `#[cfg(test)]` import was added to the parent. Test-only imports went into
   the test files, as 3.4 predicts.
8. Glob shadowing: grep the new module's item names in `source_index.rs`.
9. The moved range contains none of the shapes in 3.7.
10. clippy 1.88 exits 0 locally and in CI, and the sweep is equal to master's.

**After the last PR:**
1. Write `docs/notes/<date>-source-index-split-outcome.md`: the end state,
   measured per-PR numbers, and any hazard this spec missed.
2. Add one ledger line to `docs/CURRENT_WORK.md`.
3. Optionally, a docs PR adds a module map to the parent's `//!` header and
   tidies the wiring (3.11).

## 5. Measured facts (`4bec095`, 2026-09-28)

**`source_index.rs`:**
- 4,650 lines.
- `rsitems` lists 79 top-level rows and 46 impl children (see §1).
- 10 `pub` items and 30 `pub` methods. 0 `pub(crate)` today.
- 41 commits in total, 24 since 2026-08-01. Over the 22 non-mechanical ones
  (`9cf22f8` #218 and `20b1a85` #204-docs excluded), the future files were
  touched this many times:

  | file | commits |
  |---|---|
  | test files | 15 |
  | parent (data model) | 12 |
  | `harvest` | 11 |
  | `constants` | 9 |
  | `def_attribution` | 4 |
  | `registry` | 4 |
  | `literal_fold` | 4 |
  | `override_index` | 3 |
  | `method_returns` | 3 |

  The census was touched twice, both times together with the walk.

**Tests (`cargo test -p rigor-infer -- --list`): 322 names.**
- 123 are under `source_index`: `tests` 93, `probes_s92` 26, `probes_s94` 4.
- The rest: `tests` 86, `class_narrowing_tests` 32, `collection_shape_tests`
  23, `folding` 12, `multi_target_binder` 11, `kernel_fold` 11,
  `collection_shape_stage2_tests` 10, `rbs_tuple_return_tests` 8,
  `m2_go_slice_tests` 5, `meta_new_lift_tests` 1.

**Rustdoc** (`--document-private-items`): 28 warnings, 11 of them in
`source_index.rs` (the list is in 3.9).

**Public pages:**
- 11 directly under `target/doc/rigor_infer/source_index/`:
  - `index`;
  - constants `OVERRIDE_ANCESTOR_WALK_LIMIT` and `SOURCE_CLASS_BASE`;
  - enums `ConstLit` and `DefKind`;
  - fns `lexical_scopes` and `method_body_spans`;
  - structs `FileDefs`, `Harvest`, `ParamBoundReturn` and `SourceIndex`.
- 38 pages in the whole `rigor_infer` tree, and 40 at the end (the two PR-6
  redirects).

**Tooling:**
- `verify_move.py HEAD crates/rigor-infer/src` passes today: exit 0,
  `pub(crate) added: 0`.
- `rsitems` builds with
  `cargo build --offline --release --locked --manifest-path harness/split/rsitems/Cargo.toml --target-dir target/split-tools`.
- rigor-infer has 17 impls, none attributed. `crates/` has 133 top-level
  impls, 0 attributed.
- #221 is CLOSED (by #232). #233 is OPEN, and its item 1 does not apply (3.7).
- 0 open PRs touch `crates/rigor-infer`.

**End state (default, 7 PRs), measured:**

| file | lines |
|---|---|
| parent | 668 |
| `method_returns` | 285 |
| `registry` | 328 |
| `constants` | 377 |
| `literal_fold` | 612 |
| `def_attribution` | 1,566 |
| `override_index` | 362 |
| `harvest` | 559 |

At the end:
- 42 `pub(crate)` items and fields, and 5 globs;
- 322 tests pass;
- `cargo check --workspace --all-targets` is clean, and workspace clippy 1.88
  `-D warnings` exits 0.

The eight-PR variant has the same end state, except that `def_attribution` is
1,412 lines, `declared_names` is 161, and there are 46 bumps and 6 globs.
