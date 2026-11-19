# #424 residual row: the sequential modifier-`if` quadratic

PR #697 → merged 2026-11-20 (`73e045bd`). Refs #424; residuals filed as
#701 (the `main_self_control_flow`/`walk_compact_root` kind lists, nested
class/module bodies, a committed timing script). Two review rounds:
round 1 Needs-fix → three blocking findings fixed in `e8f2e79f`;
round 2 Approved (Opus 5.5).

## The row and the outcome

A flat file of N `x_i = 1 if y` statements, release build, macOS arm64:

| N | master | PR head | reference |
|---|---|---|---|
| 4k | 7.29s | 1.23s | 2.37s |
| 8k | 35.78s | 2.12s | 5.49s |
| 16k | 87.80s | 7.66s | 47.56s |
| 20k | 153.6s | 15.94s | 189.62s |

Faster than the oracle at every size, still superlinear (~×3.5 per
doubling): the residual is `evaluate_if`'s whole-env `join_flow_envs`
per `if` — the reference's own per-edge env design, now at refcount
cost. Declined as out of scope (the oracle has the same curve).

## The five mechanisms (each layer profiled before cutting)

1. `latest_definite_assignment`'s per-query `body[..pos]` kill scan →
   `reach_memo` segment-fold rows (`kill_scans`/`kill_prefix`) with
   `seen`/`folding` dependency records and truncation on stale deps;
   `MAX_KILL_CELLS` cap; `RIGOR_REACH_MEMO_AUDIT` self-check.
2. `check_env_at` env clones per top-level statement → sqrt-stride
   checkpoints + `bind_check_statement` replay; statement write tables
   start-indexed behind `FlowEffects` (slices yielded in TABLE order —
   arena post-order — because `apply_mutation_effects`/`arm_write_ty`
   sequence effects by it).
3. `TypeEnv` keys `String` → `Rc<str>` (envs never cross threads;
   `clone_from` reuse was measured absent, so `Rc` was the real fix —
   per-edge clones bump refcounts).
4. `coll_join_envs_delta` — re-joins only env keys that changed since
   the edge fork, `debug_assert`-verified against the full join.
5. `locals_in_span`/`unrecorded_closure` on the `ReachIndex` span and
   closure tables; `drop_indexed_rebinds` gated on `slot_writes`
   non-empty.

## What review caught (the instructive part)

- **F1 — enumerate what you DO, not what the syntax looks like.** The
  changed set was first derived from syntax kinds (write nodes in
  span); a block-bound local's *mutation* (`args << u` under `if`
  inside `xs.map do |u|`) lands on the block env but is invisible to
  both `writes` and the write-node enumeration → 37 corpus files
  tripped the delta-join `debug_assert`, all new FPs. Fix:
  `CollCtx::dirty` records every `env.insert` key; the changed set is
  `dirty[mark..]` + the `writes` widenings + loop-exit locals. 0
  divergences over ~8.7k files on an assert-enabled build.
- **F2 — order is a contract.** `FlowEffects::within` sliced by span
  start, but the table is arena post-order: for `h.default = (h[:a]
  ||= 1)` the outer mutator applied before the nested op-write and
  dropped the narrowing record before it stored. Slices are sorted
  back into table order.
- **F3 — semantic merge conflicts compile.** #687 split landed
  answers into `truthy_landed`; a textually-clean rebase left
  `kill_prefix` reading `truthy_memo` (now empty-`seen` canonical
  only). Rebased code that reads a renamed/split invariant needs the
  invariant, not just the compile.

## Gate gap worth fixing someday

`run_snapshot.rb` exercises the `debug_assert` only on fixtures; the
37-file divergence needed an assert-enabled build over the sweep
corpora — a manual pass, not a gate. If a future PR debug-asserts an
equivalence, run that build over `sweep-corpora.yml` before review.
