# lib.rs module split — handoff spec

**Landed 2026-09-27** (PRs #205–#216). The line numbers below are pre-split; for the end state, the
recipe and the hazards this spec missed, see [the outcome note](20260927-lib-rs-split-outcome.md).

2026-09-27. `crates/rigor-infer/src/lib.rs` is **14,084 lines / 736 KB**, one
file holding every inference pass. Split it into per-pass modules. This is a
**move-only refactor**: no behaviour change, no new machinery, no renames
beyond visibility. The review gate verifies "same code, new files" — not
parity.

**Why now:** the infer-PR stream (#177/#183/#184) is abandoned; zero open PRs
touch this file. The window will not stay open — land this before the next
inference issue is claimed.

**Execution shape:** land incrementally, one module extraction per PR, leaf
modules first. Each PR is a small mechanical move — small iterations are the
priority; do NOT attempt the whole split in one PR.

## File shape (verified against `2f37ba0`)

- Lines 1–36: docs, `pub mod folding/kernel_fold/multi_target_binder/source_index`, `use`, `pub use` re-exports.
- Lines 38–9444: production code — ONE `impl<'i> Typer<'i>` block at 369–7963 (~7.6k lines) + ~1.5k lines of free helpers physically interleaved below it (7965–9420).
- Lines 9446–14084: six `#[cfg(test)]` mods (165 tests): `tests` 9446–11304, `m2_go_slice_tests`, `meta_new_lift_tests`, `rbs_tuple_return_tests`, `class_narrowing_tests` 11730–13591, `collection_shape_tests` 13592–13932, `collection_shape_stage2_tests` 13933–14084.

## Target layout (10 files, leaf-first extraction order)

| # | Module | Moves | ~Lines |
|---|---|---|---|
| 1 | `flow_writes.rs` | mutator tables + `is_shape_mutator` 7992–8045, `collect_rebind_writes`/`for_index_rebinds`/`drop_inert_writes` 8047–8084, `rebound_within`/`span_hull` 7976–7990, `MUTATOR_METHODS` 8976, `collect_flow_writes` (**stays `pub`**), `toplevel_rebinds`, `descendants_of`, `node_child_ids`, `indexed_flow_writes`, `widen_flow_writes`, `widen_penv_writes`, `join_flow_envs`, `qualify_self` 8976–9370 | ~560 |
| 2 | `class_narrowing.rs` | types 169–337 (`ClassFact`/`GuardFact`/`GuardTarget`/`GuardMap`/`ChainAddr`/`Facts`/`ClassNarrowing` — `ClassNarrowing` stays `pub`); helpers 8086–8473 (`branch_terminates`/`stmt_terminates`/`narrowable_binding`/`coarse_locals`/`join_cenv`/`propagate_widened`/`retain_joined_facts`/`locals_in_span`); chain helpers 8803–8975 (`stable_chain_address`/`regex_binding_match`/`join_guards`/`kill_cenv_*`); impl methods 5267–7098 (`class_narrowing_pass`→`class_flow_*`, `apply_guards`, `analyse_predicate`, `guard_*`, `resolve_constant_as_written` 6824–6868, `constant_names_a_known_class`) | ~2,450 |
| 3 | `collection_shape.rs` | `CollCtx` 7965–7974 (sits AFTER the impl — easy to orphan); impl methods 7099–7963 (`collection_shape_snapshots` + all `coll_*`) | ~875 |
| 4 | `nilable.rs` | `ARRAY_NEW_TUPLE_LIMIT` 157; impl methods 4744–5266 (`nilable_receiver_snapshots`, `nil_flow_*`, `array_new_nominal_provenance`, `nilable_source_class`) | ~530 |
| 5 | `block_call.rs` | consts/types 102–143 (`EXACTLY_ONCE_*`, `NON_RETURNING_*`, `SplatArm`, `BlockJump`); impl methods 2979–4335 (`type_block_call`, `block_call_result`, `exactly_once_*`, `block_never_completes`/`stmt_never_completes`/`call_never_returns`/`call_declares_bot` + `*_value_bot` family, `kernel_spelled_receiver`, `block_level_jumps`, `block_break_arm_types`, `narrow_non_nil`, `block_self_type`, `block_self_member_type`, `block_entry_env`, `block_splat_table`, `splat_member_arm`, `span_on_dead_branch`, `deep_widen_is_top`) | ~1,410 |
| 6 | `reach.rs` | types + fns 8474–8801 (`Reach`, `LocalWrite`, `latest_definite_assignment`, `statement_sections`, `ends_in_return`, `definitely_assigns`, `UntypedRoot`, `untyped_expr_root`, `proc_like_block`, `has_non_local_target` incl. nested `any_ignored`, `IvarScope`); impl methods 1733–2362 (`*_reach`, `const_is_reference_untyped`, `class_ivar_scope`) | ~960 |
| 7 | `call_dispatch.rs` | impl methods 2396–2978 (`type_call`, `rbs_dispatch_declines_on_untyped_arg`, `arg_is_guarded_parameter`, `rbs_join_is_one_bare_nominal`, `resolve_param_bound`) | ~590 |
| 8 | `expr_type.rs` | fns 52–95 (shape-key fns), `CLASS_RETURNING_NEW` 151, set ops 9372–9420; impl methods: `intern_const_lit` 422–464, `type_of` 503–797, `branch_value_type`…`hash_shape_or_hash` 798–1007, `type_dot_new`…`fold_hash_shape_projection` 1008–1454, `type_implicit_self_call` 1455–1732, `fold_kernel_hash`/`file_defines_method` 2363–2395, `pin_arg_scalars` 4336–4361 | ~1,600 |
| 9 | `flow_eval.rs` | impl methods 4362–4743 (`build_toplevel_env`, `build_toplevel_check_env`, `bind_check_statement`, `always_truthy_snapshots`, `flow_eval_*`, `bind_statement`) | ~385 |
| 10 | `typer.rs` | `empty_source` 41–44 (incl. nested `static EMPTY`), `Typer` 339–368 + `EMPTY_LEXICAL_SCOPES`, impl ctors/accessors 372–502 | ~110 |
| — | `lib.rs` root | docs, `mod`/`pub mod` decls, `pub use` re-exports, `pub type TypeEnv` 100, free shims `type_of`/`build_toplevel_env` 9422–9444, all six test mods 9446–14084 | ~4,750 (mostly tests) |

Optional lighter split: merge `call_dispatch`+`reach` into `expr_type` → 7
files. `flow_eval` may later fold into a tests/ move — the six test mods can
also be relocated to `tests/` files if `use super::*` is adjusted, but that is
NOT required.

## Coupling hazards (verified — read before moving anything)

1. **`impl Typer` methods are module-private.** Splitting the impl across
   sibling modules requires `pub(crate)` on every method called cross-module.
   The compiler will find them; for speed, `rg 'self\.\w+\(' per extracted
   block before moving. Known cross-cluster edges: `stmt_value_type`→coll,
   `branch_value_type`/`nominal_or_untyped`/`type_dot_new`/`shape_key_to_scalar`→block,
   `type_block_call`/`block_call_result`/`narrow_non_nil`↔`type_of`,
   `local_reach`/`arg_reach`/`expr_reach`→dispatch/implicit-self,
   `block_entry_env`/`span_on_dead_branch`→bot family,
   `resolve_constant_as_written`/`constant_names_a_known_class`→reach (defined
   inside the class-flow range — keep in `class_narrowing.rs` as `pub(crate)`),
   `class_ivar_scope`→ivar/cvar reach, `file_defines_method`→implicit-self.
   Mutual recursion between modules is fine in Rust — no strict layering.
2. **`crate::MUTATOR_METHODS`** is a private const read by `source_index.rs`
   (lines ~826, ~6963). Moving it requires `pub(crate)` + the same-path
   re-export (see 3).
3. **Preserve bare-name resolution for test mods** (`use super::*`) and
   `crate::NAME` paths from siblings: at root add
   `pub(crate) use flow_writes::*;` style re-exports for every moved item
   tests or siblings touch. Matches the existing `pub use` pattern.
4. **Nested items move with parents**: `static EMPTY` in `empty_source`;
   `fn any_ignored` in `has_non_local_target`; `const MAX_SET_OPERATION_*`
   in `tuple_set_operation`.
5. **`#[allow(dead_code)]`** at crate level (line 18) covers new modules free.
6. **Each module needs a hand-built `use` list** — sibling pattern is explicit
   lists (folding.rs:24), not `use super::*`.
7. Intra-doc links `[`Typer::x`]` survive move-only splits unchanged.

## Public surface that must not change

`Typer` + all pub methods, `TypeEnv`, `ClassNarrowing`, `collect_flow_writes`,
`pub use folding::RubyFolder`, `pub use source_index::{…}` re-exports.
External callers: rigor-rules (~15 sites), rigor-cli (type_of/mcp/lsp/
annotate/sig_gen/coverage), `source_index.rs` itself (`crate::Typer`,
`crate::MUTATOR_METHODS`, `crate::is_shape_mutator`).

Free shims `pub fn type_of`/`build_toplevel_env` (9432/9441) have **no
external callers** — keep them anyway (move-only); deleting is a separate
decision.

## Process

- One extraction per PR. After each move: `cargo test -p rigor-infer` +
  `harness/gate.sh` (must exit 0) — pure refactor needs no oracle probes;
  the gates are the verification. Clippy runs on 1.88 in CI.
- Extraction order above is leaf-first: `flow_writes` has no `self.` methods
  (pure fns — easiest, validates the re-export pattern); `typer.rs` LAST
  because everything references the type.
- Alternative: extract test mods to `tests/` first instead — it removes 4.6k
  lines from the file and touches zero production code.
- If a move exposes a hidden coupling that needs anything beyond visibility
  changes, STOP and note it — the design assumes move-only; behaviour changes
  belong in their own PRs.

## Context for the next session

- Merge bar changed 2026-09-27 (`docs/agents/review.md`): PR lands on
  issue-scope resolution; adjacent findings are filed, not blockers. For this
  refactor, "scope" = move-only; the review just verifies no semantic diff.
- The abandoned branches (`claude/issue-164/167/138-*`) contain unmerged
  inference work that WILL conflict with this split when resumed — expected;
  resurrection is a future-session problem, recorded in
  `20260927-abandoned-infer-pr-stream.md`.
