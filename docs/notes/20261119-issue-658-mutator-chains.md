# #658: mutator call chains (`v.push(1).push(1)…`)

PR #659 → merged 2026-11-19 (`97efc31`). Closes #658 (the mutator
row of #424). Two-round review: Opus Approved `510ec0f`, then
fix-confirmation Approved `6329933`.

## What was slow

`v = []; v.push(1).push(1)…` at N links: 2.5k **49.5 s**, 5k **52 s**.
Statement form (`v.push(1)` per line) was already 1.4 s at 2.5k. Three
mechanisms stacked:

1. `check_env_at` → `entry_descend` replays the receiver subtree per
   call site. `widen_mutated_locals` asked `path_unconditional` /
   `path_operand_evaluated` / `path_content_writeback` per contained
   mutation — O(depth) walks sharing prefixes ⇒ cubic.
2. Each `type_call` level re-walked `CoreData::ancestors` and the
   instance-method lookup (`method_return` / nilable / tuple / block-free).
3. `type_of` memoized only env-*free* `Call`s (#424). A `v.push(1)`
   receiver is env-sensitive, so every replay re-typed the remaining
   suffix ⇒ quadratic `type_call`.

Non-mutator self-returning chains (`freeze`/`itself`/`dup`) sat at
~4.5–7 s — the known #424 env-decline residual.

## Port shape

Pure memoization; diagnostics vs master unchanged after the span-window
fix.

- **`PathMarks`**: one DFS per replay root records `{uncond, operand,
  writeback}` per node span (outermost wins). A node is marked only
  when its span nests inside every ancestor — the recursive walks only
  descend a child whose span contains `wspan`. Heredoc interpolations
  escape the `InterpolatedString` node's `<<~T` span and fall back to
  the recursive walk. 8-entry LRU keyed `(ast.uid(), root)`.
- **`CoreData` memos**: `ancestors_memo` (unknown names bypass) and a
  two-level `MethodMemo` (`lookup` / `tuple_definer` / `block_free`)
  with `chain_method_lookup` shared by `method_return` /
  `method_return_nilable`. `Mutex` keeps `CoreData: Sync`.
- **`type_of_env_memo`**: env-sensitive counterpart of #424's
  `type_of_memo`. Same `(ast, dead)` window and `seen`/fold dependency
  record, plus fingerprint + verbatim env snapshot + `def_scope` +
  `Interner::uid`. Gated `env.len() <= 32` (larger envs stay uncached,
  identical output).
- **`widen_mutated_locals`** takes an iterator (no per-statement
  `String` clones). `stored_slot_writes` early-returns when empty.

`Interner::uid` (AtomicU64) replaced an address key —
`infer_one_return` drops a scratch interner per method in the same
stack slot.

## Measured (release `check`, fresh cwd)

| shape | before | after | oracle |
|---|---|---|---|
| `v.push(1)` × 800 | 4.25 s | 0.45 s | same key |
| `v.push(1)` × 2.5k | 49.5 s | 1.7–2.2 s | ~4.4 s |
| `v.push(1)` × 5k | 52 s | ~8 s | |
| gitlab/lib 4,892 files | 3.37 s | 3.26 s | byte-identical 1,301 diags |

Residual O(N²) is `entry_descend`'s own replay (`entry_children` per
level) — the oracle's shape. Survey 73,451 files / 23,311 diags
byte-identical vs master (adversarial review). `fp_audit --sweep`: 0 FP.
`RIGOR_REACH_MEMO_AUDIT=1` clean.

## Review

Round 1 (510ec0f) Approved with two notes: interner address identity,
PathMarks vs recursive descent on non-nested spans. Round 2 confirmed
both landed; 10-shape heredoc battery identical to master. The
`&& <<~T #{v.push(1)}` FP (`for []`) and `for 0` vs `for Integer`
drift predate the PR — rows for #311.

## Follow-ups (not filed; listed on the PR)

PathMarks audit-mode assertion; lazy `stored_slot_writes` marks;
`CoreData` Mutex → per-`ClassEntry`/`RwLock` if contention appears;
store `TypeId` now that `Interner::uid` exists.
