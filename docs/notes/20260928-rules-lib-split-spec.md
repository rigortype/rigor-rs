# rigor-rules lib.rs module split — handoff spec

2026-09-28. `crates/rigor-rules/src/lib.rs` is **4,497 lines** at `origin/master`
`b05f7f3`. Its three test modules already left in #224. This spec splits the
production code into per-family modules. It is a **move-only refactor**: no
behaviour change, no renames, and visibility changes only where rustc asks.
The review checks "same code, new files", not parity.

**Precedent:** #204 (`20260927-lib-rs-module-split-spec.md`, outcome in
`20260927-lib-rs-split-outcome.md`), with the tooling in `harness/split/`.

**Why now:** no open PR touches `crates/rigor-rules/`. #232 (tooling) has
since merged and closed #221; its latent limit #233 does not apply here (no
attributed impl in the crate). `lib.rs` has had 29 commits since
2026-08-01 and 90 in total, and every rule PR edits it.

**Execution shape:** one PR per module, leaf-first, after one small prep PR
(P0). File an umbrella issue with this spec as the agent brief, as #204 was.

Line numbers below are at `b05f7f3`. P0 shifts them by a few lines.
Selectors are by name, so the shift does not matter to the tools.

## 1. File shape (verified against `b05f7f3`)

**Header, lines 1–18:**
- `//!` docs, then `#![allow(dead_code)]` (line 5).
- Four `use` lines (7–10): `rigor_index::{CoreIndex, OverloadSignature, RetainedParamType}`,
  `rigor_infer::Typer`, `rigor_parse::{HashKeyTag, LoweredAst, Node, NodeId}` and
  `rigor_types::{Interner, Scalar, Type}`.
- `mod shadowed_rescue;` + `pub use …shadowed_rescue_diagnostics` (12–13).
- `pub mod dead_version_guard;` + `pub use dead_version_guard::{filter_dead_version_guard_arms, filter_dead_version_guard_arms_with, RubyRuntime}` (15–18).

**Items, 19–4484, in banner-delimited sections:**

| lines | section (the file's own banners) | items |
|---|---|---|
| 19–111 | Severity enum / Diagnostic struct | `Severity`+impl, `NO_RULE`, `Diagnostic`+impl |
| 112–284 | Rule catalogue | `RuleEntry`, `catalog` |
| 285–491 | Rule IDs | 19 `pub const` rule ids |
| 492–497 | (no banner) | `INTEGER_RAISING_OPERATORS` (belongs to `flow.always-raises`) |
| 498–864 | analyze() | `analyze`, `analyze_with_source`, `analyze_with_source_and_folder` (332 lines, all ten passes) |
| 865–1106 | (no banner) | `call.unresolved-toplevel`: 3 consts + 4 fns |
| 1107–1256 | (no banner) | `flow.always-truthy-condition`, `flow.unreachable-branch` |
| 1257–1385 | (no banner) | `qualified_class_names`/`walk_qualified`, then `def.override-visibility-reduced` |
| 1386–2118 | Rule implementations | `call.undefined-method` (4 check fns), `call.wrong-arity` |
| 2119–2525 | call.argument-type-mismatch | 14 items |
| 2526–2629 | (no banner) | `flow.always-raises` |
| 2630–2951 | call.raise-non-exception | 11 items, incl. the shared `concrete_class_name`/`constant_class_name`/`resolve_class_name` |
| 2952–3010 | (no banner) | `call.possible-nil-receiver` (`check_nil_receiver`) |
| 3011–3284 | Helpers (empty) + flow.dead-assignment | `dead_assignments_in_def`, `span_within`, `ScopedEnv`+impl, `trailing_statement`, `descend_trailing` |
| 3285–3573 | def.ivar-write-mismatch | 5 items, then the shared `render_receiver`, `render_scalar` |
| 3574–3618 | flow.duplicate-hash-key | 1 fn |
| 3619–3782 | flow.return-in-ensure | 1 const + 3 fns |
| 3783–3894 | static.value-use.void | 2 fns |
| 3895–4139 | suppression.unknown-rule / suppression.empty | 14 fns |
| 4140–4484 | In-source diagnostic suppression | a **mid-file** `use std::collections::{HashMap, HashSet};` at 4145, then 15 items + `impl SuppressSet` |
| 4485–4497 | Tests | three `#[cfg(test)] mod …;` |

**Public surface.** There are 34 `pub` items:
- `Severity`, `NO_RULE`, `Diagnostic`, `RuleEntry` and `catalog`;
- the 19 rule-id consts;
- `analyze`, `analyze_with_source` and `analyze_with_source_and_folder`;
- `void_value_use_diagnostics` and `suppression_marker_diagnostics`;
- `implemented_rules`, `is_inert_builtin_token` and `known_suppression_token`;
- `SuppressSet` and `filter_suppressed`;
- the pub methods `Severity::as_str`, `Diagnostic::qualified_rule`,
  `SuppressSet::from_tokens` and `SuppressSet::suppresses`.

The re-exports listed in the header are also public.

**The only external caller is rigor-cli** (13 files: `main.rs`, `lsp.rs`,
`lsp/tests.rs`, `mcp.rs`, `baseline.rs`, `config.rs`, `config_audit.rs`,
`diagnostic_formats.rs`, `diff.rs`, `doctor.rs`, `triage.rs`, `ruby_mode.rs`
(a doc mention only) and `tests/check.rs`). Names it uses, with the number of
files:

- `Diagnostic` 8, `Severity` 5;
- 3 each: `catalog`, `filter_suppressed`, `SuppressSet`,
  `suppression_marker_diagnostics`, `shadowed_rescue_diagnostics` and
  `filter_dead_version_guard_arms`;
- 2 each: `analyze_with_source_and_folder`, `void_value_use_diagnostics` and
  `STATIC_VALUE_USE_VOID`;
- 1 each: `analyze` and `CALL_UNDEFINED_METHOD` (both `tests/check.rs`),
  `analyze_with_source` (`mcp.rs`), `NO_RULE` (`main.rs`), `implemented_rules`
  (`doctor.rs`) and `is_inert_builtin_token` (`config_audit.rs`).

No other crate names `rigor_rules`. Everything stays reachable at the same
root path through `pub use`. No new module is `pub`.

**What `lib.rs` keeps (end state, about 60–70 lines):**
- the `//!` docs, plus an optional module map written by hand in the last PR;
- `#![allow(dead_code)]`. It must stay: `render_scalar` (0 callers) and
  `SuppressSet::is_empty` (0 callers) depend on it;
- the existing `mod shadowed_rescue` / `pub mod dead_version_guard` lines and
  their re-exports;
- 13 new `mod X;` lines;
- the `pub use` re-exports (the public surface);
- the `pub(crate) use X::*;` globs that siblings or tests need;
- the three `#[cfg(test)] mod …;` declarations, unchanged. Their paths stay
  the same, so the test names do not change.

## 2. Target layout

**Existing modules stay as they are:** `shadowed_rescue.rs` (642 lines,
`flow.shadowed-rescue-clause`), `dead_version_guard.rs` (871), and the test
files `tests.rs`, `void_value_use_tests.rs` and `rbs_tuple_witness_tests.rs`.

The rows are in extraction order, leaf-first:
- first, rule modules that only the driver calls;
- next, the providers they share (`call_raise`, `suppression`, `scope`);
- then the driver;
- last, the catalogue and the `Diagnostic` type, which every module references.
  This mirrors `typer.rs` coming last in #204.

"Moved" counts split_mod's lines, gaps included. Selectors are listed in
file order. Every `impl:` selector must be listed explicitly (see hazard 3.7).

| # | module | selectors | moved | ranges |
|---|---|---|---|---|
| P0 | (prep, not move-only) | see §4 | — | — |
| 1 | `call_toplevel.rs` | `RUNTIME_KERNEL_TOPLEVEL unresolved_toplevel_diagnostics META_NEW_SELECTORS meta_new_block_body_spans RECEIVER_EVAL_CALL_NAMES receiver_eval_block_spans span_contained_in_any` | 242 | 865–1106 |
| 2 | `void_value_use.rs` | `void_value_use_diagnostics check_void_value_use` | 112 | 3783–3894 |
| 3 | `suppression_markers.rs` | `suppression_marker_diagnostics validate_suppression_tokens diagnose_bare_suppression_marker is_word_or_hyphen_byte starts_word_or_hyphen word_or_hyphen_run_len is_suppression_ws_or_comma match_bare_disable parse_unknown_marker is_rule_list_shaped rigor_marker_tails unknown_suppression_rule_diagnostic unknown_suppression_marker_diagnostic empty_suppression_diagnostic` | 245 | 3895–4139 |
| 4 | `call_receiver.rs` | `METACLASS_ARMS unenumerable_metaclass_receiver unenumerable_instance_receiver check_call check_union_call check_narrowed_call check_collection_call check_nil_receiver` | 685 | 1386–2011, 2952–3010 |
| 5 | `call_arguments.rs` | `check_wrong_arity is_universal_equality_method is_coerce_dispatch_method argument_check_eligible is_faithful_param render_retained_param expected_label_multi arg_is_pure_nil arg_is_dynamic_or_top single_concrete_arg_class faithful_param_rejects_arg AtmMismatch single_overload_mismatch multi_overload_mismatch check_argument_type_mismatch` | 514 | 2012–2525 |
| 6 | `flow.rs` | `INTEGER_RAISING_OPERATORS DEFENSIVE_PREDICATES check_always_truthy constant_polarity literal_predicate_truthy check_unreachable_branch check_always_raises is_integer_rooted dead_assignments_in_def trailing_statement descend_trailing duplicate_hash_key_diagnostics FRAME_BARRIER_CALL_NAMES return_in_ensure_diagnostics gather_returns_in_ensure node_children` | 667 | 492–497, 1107–1256, 2526–2629, 3011–3170, 3247–3284, 3574–3782 |
| 7 | `def.rs` | `visibility_rank visibility_word check_override_visibility IvarWrite RescueBinding collect_rescue_bindings ivar_write_class ivar_write_mismatch_diagnostics` | 335 | 1302–1385, 3285–3535 |
| 8 | `call_raise.rs` | `RAISE_METHOD_NAMES RAISE_UNEXACT_INSTANCE_CLASSES RaiseVerdict raise_non_exception_diagnostics raise_operand_verdict raise_class_operand_verdict raise_instance_operand_verdict concrete_class_name constant_class_name resolve_class_name raise_redefined_in_scope` | 322 | 2630–2951 |
| 9 | `suppression.rs` | `INTERNAL_ERROR_RULE RULE_FAMILIES IMPLEMENTED_RULES implemented_rules ALL_CANONICAL_RULES is_inert_builtin_token legacy_alias NON_CHECK_DIAGNOSTIC_FAMILIES NON_CHECK_DIAGNOSTIC_IDS known_suppression_token SuppressSet impl:SuppressSet filter_suppressed parse_suppression_comments match_directive absorb_tokens` | 339 (343 after P0: the banner joins the gap) | 4146–4484 |
| 10 | `scope.rs` | `qualified_class_names walk_qualified span_within ScopedEnv impl:ScopedEnv` | 121 | 1257–1301, 3171–3246 |
| 11 | `driver.rs` | `analyze analyze_with_source analyze_with_source_and_folder` | 367 | 498–864 |
| 12 | `rule_catalog.rs` | `RuleEntry catalog` + the 19 ids `CALL_UNDEFINED_METHOD CALL_WRONG_ARITY CALL_ARGUMENT_TYPE_MISMATCH CALL_POSSIBLE_NIL_RECEIVER CALL_UNRESOLVED_TOPLEVEL FLOW_DEAD_ASSIGNMENT DEF_OVERRIDE_VISIBILITY_REDUCED FLOW_ALWAYS_RAISES FLOW_UNREACHABLE_BRANCH FLOW_ALWAYS_TRUTHY_CONDITION FLOW_DUPLICATE_HASH_KEY FLOW_RETURN_IN_ENSURE SUPPRESSION_UNKNOWN_RULE SUPPRESSION_EMPTY SUPPRESSION_UNKNOWN_MARKER STATIC_VALUE_USE_VOID DEF_IVAR_WRITE_MISMATCH CALL_RAISE_NON_EXCEPTION FLOW_SHADOWED_RESCUE_CLAUSE` | 380 | 112–491 |
| 13 | `diagnostic.rs` | `Severity impl:Severity NO_RULE Diagnostic impl:Diagnostic render_receiver render_scalar` | 131 | 19–111, 3536–3573 |

The 13 rows hold 133 items (all of them) and move 4,460 of the 4,497 lines.

**Why these seams:**
- **`call_receiver`** holds the receiver rules of the one-per-site precedence
  chain. **`call_arguments`** holds the two rules that read RBS signatures:
  arity counts them and ATM types them. The planned singleton-arity work
  (`20260909-singleton-arity-mini-spec.md`) copies ATM's `Singleton` arm into
  `check_wrong_arity`, so those two change together.
- **`scope`** takes `qualified_class_names`: the def rules and the driver's
  override pass both use it.
- **`diagnostic`** takes `render_receiver`, a message-rendering helper that
  three call modules share.
- **`call_raise`** keeps `concrete_class_name`, which the reference names as a
  raise helper. ATM imports it.
- **The module is `rule_catalog`, not `catalog`.** A module and a fn both named
  `catalog` would compile, but a root doc link [`catalog`] would then be
  ambiguous.

**Proposed `//!` first lines** (humans write the headers):

1. "`call.unresolved-toplevel` (ref ADR-34), and the class-body regions (meta-new and receiver-eval blocks) that are not toplevel."
2. "`static.value-use.void` (ADR-100), run by the CLI only under `use-of-void-value`."
3. "The `suppression.*` rules: surveillance over the `# rigor:` markers themselves, emitted before `filter_suppressed`."
4. "`call.undefined-method` (scalar, union, class-narrowed, collection-shape receivers) and `call.possible-nil-receiver`."
5. "`call.wrong-arity` and `call.argument-type-mismatch` (ADR-64): the rules that read a method's RBS parameters."
6. "The `flow.*` rules except `flow.shadowed-rescue-clause` (in `shadowed_rescue`)."
7. "The `def.*` rules: override-visibility-reduced (ADR-35) and ivar-write-mismatch."
8. "`call.raise-non-exception`, and the operand class resolution the argument-type rule shares."
9. "In-source suppression (`# rigor:disable[-file]`), the `disable:` set, and the rule-token tables both read."
10. "The per-use-site local env (`ScopedEnv`), span containment, lexical class qualification."
11. "The single converged walk (ADR-0005): `analyze*` builds the typer and envs once and runs every pass."
12. "Every rule id and its catalogue entry (severity, evidence tier, documentation URL)."
13. "The `Diagnostic` type (ADR-0014), `Severity`, the `NO_RULE` sentinel, receiver rendering."

**Lighter split (10 PRs):** merge `diagnostic` with `rule_catalog` (~511
lines), `scope` into `driver` (~488), and `suppression_markers` with
`suppression` (~584). The default keeps them apart for two reasons:
- hot vs. cold: every new rule edits `driver` and `rule_catalog`, while
  `scope` and `diagnostic` rarely change;
- `suppression.*` is a rule family, but the filter is infrastructure.

`scope` (121 lines), `diagnostic` (131) and `void_value_use` (112) are below
the ~200-line target.

## 3. Coupling hazards (each verified by reading the code at `b05f7f3`)

**3.1 Cross-module edges → `pub(crate)` (30 in total; the crate has 0 today).**
fixvis adds each one. The reviewer checks that the count per PR matches.

| PR | new `pub(crate)` | because |
|---|---|---|
| 1 | `unresolved_toplevel_diagnostics` | driver |
| 2, 3 | none | their only external entries are already `pub` |
| 4 | `check_call check_narrowed_call check_collection_call check_nil_receiver` | driver |
| 5 | `check_wrong_arity check_argument_type_mismatch arg_is_pure_nil` | driver (`arg_is_pure_nil` is the `nil&.m` skip at line 643) |
| 6 | `check_always_truthy check_unreachable_branch check_always_raises dead_assignments_in_def duplicate_hash_key_diagnostics return_in_ensure_diagnostics` | driver |
| 7 | `check_override_visibility ivar_write_mismatch_diagnostics` | driver |
| 8 | `raise_non_exception_diagnostics` (driver), `concrete_class_name` (`call_arguments`), `raise_operand_verdict RaiseVerdict` (tests) | |
| 9 | `match_directive` (`suppression_markers`), `INTERNAL_ERROR_RULE parse_suppression_comments` (tests) | |
| 10 | `ScopedEnv` plus the methods `build at gate_at` (E0624), `span_within`, `qualified_class_names` | driver, `call_raise`, `void_value_use`, `flow`, `def` |
| 11, 12 | none | all `pub` |
| 13 | `render_receiver` | `call_receiver`, `call_arguments`, `call_raise` |

No struct field crosses a module boundary: the fields of `AtmMismatch`,
`IvarWrite`, `RescueBinding`, `ScopedEnv` and `SuppressSet` are read only in
their own module. `ScopedEnv::in_method_body` and `SuppressSet::absorb_token`
stay private. Mutual module dependencies (driver ↔ rules) are fine in Rust.

**3.2 Names the tests reach through `use super::*`.**
- `tests.rs` reaches 35 lib.rs names:
  - 26 `pub` ones, which the root `pub use` covers;
  - 4 private ones, which need the root glob plus `pub(crate)`:
    `INTERNAL_ERROR_RULE` and `parse_suppression_comments` → `suppression`
    (tests at lines 2049/2056/2087), `raise_operand_verdict` and `RaiseVerdict`
    → `call_raise` (line 2958);
  - 5 root imports: `CoreIndex`, `Interner`, `LoweredAst` (line 2139),
    `Scalar` and `Type` (2954–2955).
- `void_value_use_tests.rs` reaches `Diagnostic`, `STATIC_VALUE_USE_VOID`,
  `void_value_use_diagnostics`, `CoreIndex` and `Interner`.
- `rbs_tuple_witness_tests.rs` reaches `CALL_UNDEFINED_METHOD`, `Diagnostic`,
  `analyze`, `CoreIndex` and `Interner`.
- None of the test files uses a `crate::`/`super::`-qualified lib item.

**3.3 Test-only root imports.** fixvis reports these as "unused in one build
only". For each, delete the name from lib.rs's `use`, then run `testimports.py`.
Do not add `#[cfg(test)]` imports at the root.

| PR | test-only import | `testimports.py` arguments | files that gain the import |
|---|---|---|---|
| 8 | `Type` | `Type=rigor_types` | `tests.rs` |
| 11 | `LoweredAst` | `LoweredAst=rigor_parse` | `tests.rs` (merged into `use rigor_parse::{lower, parse}`) |
| 13 | `CoreIndex`, `Interner`, `Scalar` | `CoreIndex=rigor_index Interner=rigor_types Scalar=rigor_types` | `tests.rs`; also the other two test files for `CoreIndex` and `Interner` |

Unused in both builds, so fixvis prunes them itself:
- #5: `OverloadSignature`, `RetainedParamType`;
- #6: `HashKeyTag`;
- #8: `NodeId`;
- #9: `HashMap`, `HashSet`;
- #11: `Typer`, `Node`.

After #13 the root has no `use rigor_*` line.

**3.4 Globs.**
- fixvis should drop 4 globs as unused, because an explicit `pub use` covers
  every name they carry: `void_value_use` (#2), `suppression_markers` (#3),
  `driver` (#11) and `rule_catalog` (#12).
- 9 globs stay load-bearing: `call_toplevel`, `call_receiver`,
  `call_arguments`, `flow`, `def`, `call_raise`, `suppression`, `scope` and
  `diagnostic`. The reason is that fixvis imports siblings as `crate::NAME`.
- Glob shadowing: every item name in lib.rs is unique, and no root import
  shares a name with a moved item. Re-check this in every PR, since a later
  root item would shadow a glob silently.

**3.5 Consts and statics.**
- There are 35 consts and 0 statics.
- Shared across families: only the 19 `pub` rule ids (read by the rule
  modules, `catalog`, `IMPLEMENTED_RULES` and `legacy_alias`), `NO_RULE`
  (`diagnostic`), and `INTERNAL_ERROR_RULE` (`suppression` + tests).
- Each of the 14 private tables is read only inside its own module:
  - `call_toplevel`: `RUNTIME_KERNEL_TOPLEVEL`, `META_NEW_SELECTORS`, `RECEIVER_EVAL_CALL_NAMES`;
  - `flow`: `INTEGER_RAISING_OPERATORS`, `DEFENSIVE_PREDICATES`, `FRAME_BARRIER_CALL_NAMES`;
  - `call_receiver`: `METACLASS_ARMS`;
  - `call_raise`: `RAISE_METHOD_NAMES`, `RAISE_UNEXACT_INSTANCE_CLASSES`;
  - `suppression`: `RULE_FAMILIES`, `IMPLEMENTED_RULES`, `ALL_CANONICAL_RULES`,
    `NON_CHECK_DIAGNOSTIC_FAMILIES`, `NON_CHECK_DIAGNOSTIC_IDS`.
- `INTEGER_RAISING_OPERATORS` sits physically among the rule ids (493). It
  moves to `flow`.
- The one nested item is `fn strip` inside `render_retained_param`; it moves
  with its parent.

**3.6 Absent shapes (grepped).**
- Absent in lib.rs: `macro_rules!`, `self::`/`super::`/`crate::` paths,
  `line!`/`file!`/`column!`/`module_path!`/`include*!`, `#[path]`, and trait
  imports. Every root `use` names a type, so no method call depends on a
  trait being in scope.
- Absent in the whole crate: #221's shapes. The 4 impls (`Severity` 33,
  `Diagnostic` 100, `ScopedEnv` 3212, `SuppressSet` 4337) carry no
  attributes and have plain `impl X {` headers. There is no one-line impl and
  no `use …::{self`. `verify_move.py HEAD crates/rigor-rules/src` runs clean
  on today's tooling.
- None of the 13 target paths is git-ignored (`git check-ignore`; the global
  excludes file is `~/.gitexclude`).
- #232 has landed: its git-ignore warning replaces the manual check.

**3.7 Impls left behind compile.** An inherent `impl Severity` that stays in
lib.rs while `Severity` moves still compiles, and `verify_move` passes. Only
the selection review catches it. List `impl:Severity`, `impl:Diagnostic`,
`impl:ScopedEnv` and `impl:SuppressSet` in the same PR as their type.

**3.8 Misplaced doc and orphaned banners (P0).**
- Lines 866–875 document `unresolved_toplevel_diagnostics` ("Emit
  `call.unresolved-toplevel` for every toplevel implicit-self call…"), but
  they sit glued to `RUNTIME_KERNEL_TOPLEVEL`, whose own doc is 876–881. The
  fn at 884 has no doc. This is the only misplaced doc: every item's doc
  openers were scanned.
- The mid-file `use` at 4145 is not movable by `split_mod`, because `use` is
  not a selectable kind. Its banner (4141–4143) would stay orphaned in
  lib.rs. Hoisting the `use` makes the banner `INTERNAL_ERROR_RULE`'s gap.
- Cosmetic banners travel with the next item:
  - the empty `// Helpers` banner (3012–3014) → `flow`;
  - `// Rule implementations` (1387–1389) → the top of `call_receiver`.

**3.9 Rustdoc links.**
- Baseline: 7 warnings with `--document-private-items`:
  - 5 are "public documentation … links to private item": 303→`check_argument_type_mismatch`,
    482→`shadowed_rescue`, and three inside the `suppression` family;
  - 2 are pre-existing unresolved links: `SourceIndex` at 518 and
    `SourceIndex::nearest_ancestor_defining` at 1331. Both stay unresolved and
    keep the same text.
- Four links would break when their doc leaves the target's scope:
  - 303 `check_argument_type_mismatch`, 425 `void_value_use_diagnostics` and
    482 `shadowed_rescue`, all in `rule_catalog` (#12);
  - 1954 `ScopedEnv`, in `call_receiver` (#4).
- Measured on a scratch crate: a reference-style target makes rustdoc print
  the *target path* in the private-link warning (`links to private item
  `crate::f``). Adding the targets during #12 would therefore reword two
  warning lines. Adding all four in P0 moves that rewording into P0. After
  P0, every move PR should show an identical warning set with no
  `doclinks.py` step. Keep `doclinks.py` as the fallback for anything this
  scan missed.

**3.10 split_mod's root wiring.**
- It inserts `mod X;` after the last `mod` line above the first body item, so
  the lines land between `pub mod dead_version_guard;` and its `pub use {…}`
  block. That is cosmetic. Reordering `mod`/`use` lines is scaffold to
  `verify_move`.
- In **#13**, no body item remains, so `first_body` finds none and
  `mod diagnostic;` is appended after `mod rbs_tuple_witness_tests;` at the end
  of the file. That compiles, but move the line up by hand.

**3.11 Adjacent findings (out of scope; file separately, probe first).**
- `IMPLEMENTED_RULES` (18 ids) omits `CALL_UNRESOLVED_TOPLEVEL`, which both
  `catalog()` and `ALL_CANONICAL_RULES` include. So the `call` family token
  (`# rigor:disable call`, `disable: [call]`) and `rigor doctor` skip
  `call.unresolved-toplevel`. This is unverified against the reference.
- Dead code: `render_scalar` and `SuppressSet::is_empty`.
- Stale text:
  - the comment at 835 names `check_unresolved_toplevel`, which does not
    exist;
  - `catalog`'s doc says "three rules";
  - `render_receiver`'s doc opens with a superseded sentence (3537–3538).

## 4. Per-PR process

**P0 (a small PR, not move-only, landed first):**
1. Move the doc at 866–875 onto `fn unresolved_toplevel_diagnostics`.
2. Hoist `use std::collections::{HashMap, HashSet};` into the header block
   (7–10), and drop the doubled blank line it leaves behind.
3. Run `python3 harness/split/doclinks.py crates/rigor-rules/src/lib.rs check_argument_type_mismatch=crate::check_argument_type_mismatch void_value_use_diagnostics=crate::void_value_use_diagnostics shadowed_rescue=crate::shadowed_rescue ScopedEnv=crate::ScopedEnv`.
   Each shortcut link occurs exactly once in the file.
4. Optionally, the cosmetic fixes in 3.8 and 3.11.

Gates for P0:
- the test list is identical;
- rustdoc still has 7 warnings, with exactly the two private-link lines
  reworded to `crate::…`;
- `harness/gate.sh` passes.

**Each move PR (#1–#13).**
Set up a fresh worktree on `claude/issue-N-<slug>` from its base (`origin/master`,
or the previous stacked branch; pass that base to every step that takes one),
then run `git submodule update --init reference/rigor`.
`S` is a scratch directory.

```sh
# baselines, on the base commit
cargo test -q --locked -p rigor-rules -- --list 2>/dev/null | grep ': test$' | sort > $S/tests.before   # 270
cargo doc -q -p rigor-rules --no-deps --document-private-items 2>&1 | grep '^warning' | sort > $S/doc.before
rm -rf target/doc/rigor_rules && cargo doc -q -p rigor-rules --no-deps \
  && find target/doc/rigor_rules -maxdepth 1 -name '*.html' | sort > $S/pub.before   # public root pages
# move (sel = the row's selectors, one per line; doc = the //! header)
python3 harness/split/split_mod.py crates/rigor-rules/src/lib.rs MOD $S/MOD.sel $S/MOD.doc   # read every printed gap line
python3 harness/split/fixvis.py --crate rigor-rules crates/rigor-rules/src/MOD.rs --prune; echo $?  # must be 0
#   if it reports a test-only root import (3.3): delete it from lib.rs's `use`, then
python3 harness/split/testimports.py --crate rigor-rules NAME=PATH ...
#   #13 only: move the appended `mod diagnostic;` up (3.10)
# proofs (run each bare and read its exit code)
git check-ignore -v crates/rigor-rules/src/MOD.rs    # must print nothing
git add -A crates/rigor-rules/src
python3 harness/split/verify_move.py BASE crates/rigor-rules/src   # 0 UNEXPECTED; read every scaffold row
diff $S/tests.before <(cargo test -q --locked -p rigor-rules -- --list 2>/dev/null | grep ': test$' | sort)
diff $S/doc.before <(cargo doc -q -p rigor-rules --no-deps --document-private-items 2>&1 | grep '^warning' | sort)
rm -rf target/doc/rigor_rules && cargo doc -q -p rigor-rules --no-deps && \
  diff $S/pub.before <(find target/doc/rigor_rules -maxdepth 1 -name '*.html' | sort)
cargo check -q --locked -p rigor-cli --all-targets      # the only external caller, incl. tests/check.rs
harness/gate.sh --base BASE; echo $?
```

If the rustdoc diff is not empty, run `doclinks.py crates/rigor-rules/src/MOD.rs X=crate::X`
for each new "unresolved link" and re-diff.

Then:
1. Commit, and push with an explicit refspec:
   `git push origin HEAD:refs/heads/claude/issue-N-<slug>`.
2. Open a draft PR with `Closes`/`Part of #umbrella` in the body.
3. Read `gh pr checks`. CI's clippy on 1.88 is the authority.
4. Before marking ready, run `cargo build --release` and
   `python3 harness/fp_audit.py --gaps --sweep`. It must show 0 FP, with
   per-corpus counts equal to master's.
5. Get the Opus review, then `gh pr ready`.

In the PR body, record:
- the moved-line count;
- `pub(crate) added: N` against 3.1;
- the tests (270) and rustdoc (7) results;
- `verify_move`'s result;
- the sweep numbers.

**What the reviewer checks:**
1. The selection equals this spec's row. No other item moved. The gap lines
   split_mod printed hold only that family's banners.
2. No `impl` was left behind (3.7).
3. `verify_move` shows 0 UNEXPECTED rows. Read every scaffold row:
   - each `use crate::{…}` name must point at the moved-away item;
   - the `mod` / `pub use` / glob lines;
   - the new `//!` header.
4. `pub(crate) added` equals 3.1, and each one is on the listed item.
5. Every moved `pub` item has a root `pub use`. The public root page list and
   `cargo check -p rigor-cli --all-targets` are unchanged.
6. The test list and the rustdoc warning set are identical.
7. No `#[cfg(test)]` import was added at the root. Test-only imports went into
   the test files, as 3.3 predicts.
8. Glob shadowing: grep the new module's item names in lib.rs.
9. The moved range contains none of the shapes in 3.6.
10. CI is green, and the sweep is equal to master's.

**After the last PR:** write `docs/notes/<date>-rules-lib-rs-split-outcome.md`
(end state, measured per-PR numbers, any hazard this spec missed). Add one
ledger line to `docs/CURRENT_WORK.md`.

## 5. Measured facts (`b05f7f3`, 2026-09-28)

**lib.rs:**
- 4,497 lines.
- rsitems lists 145 top-level rows: 85 fn, 35 const, 7 struct, 2 enum,
  4 impl (10 methods), 7 use and 5 mod. That is 133 movable items.
- 34 `pub` items. The crate has 0 `pub(crate)` today.
- `lib.rs` history: 90 commits, 29 of them since 2026-08-01.

**Tests:** 270 names:
- `tests` 228;
- `shadowed_rescue::tests` 23;
- `dead_version_guard::tests` 12;
- `rbs_tuple_witness_tests` 4;
- `void_value_use_tests` 3.

**Rustdoc** (`--document-private-items`): 7 warnings (the list is in 3.9).

**This layout:**
- 13 modules, 4,460 lines moved, and 37 lines left before the hand edits
  (counted at `b05f7f3`, without P0):
  - 18 lines of header, imports and mod lines;
  - 6 lines of the mid-file banner and `use`;
  - 13 lines of the tests banner and test mods.
- Moved lines per module: `call_toplevel` 242, `void_value_use` 112,
  `suppression_markers` 245, `call_receiver` 685, `call_arguments` 514,
  `flow` 667, `def` 335, `call_raise` 322, `suppression` 339, `scope` 121,
  `driver` 367, `rule_catalog` 380, `diagnostic` 131.
- Expected `pub(crate)` added, per PR: 1, 0, 0, 4, 3, 6, 2, 4, 3, 6, 0, 0, 1
  (30 in total).
- Expected globs at the end: 9. Expected `pub use` lines at the end: 6 new
  plus the 2 existing.

**Tooling and paths:**
- `verify_move.py HEAD crates/rigor-rules/src`: pass, with `pub(crate) added: 0`.
- 13 target paths: none git-ignored.
- Open PRs touching `crates/rigor-rules`: 0.
- #232 (tooling) merged, closing #221. No #221 shape occurs in this crate.
