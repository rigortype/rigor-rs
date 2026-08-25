# S94 ancestor-closure mini-spec (2026-08-25)

Implements issue #94 from [the Pass-4b cost probe](20260825-s94-pass4b-cost-probe.md).
Like #92 this is a PURE PERF slice: no emitted diagnostic may change, anywhere.
The `probes_s92` equivalence harness and the #92 gate battery carry the proof.

## Probe anchors (what this buys and why this shape)

- `overridden_in_project` is 70.4% of Pass 4b at gitlab-foss/lib (64.9ms of
  92.1ms), 37.8% at mastodon/app — the win is scale-dependent, biggest where
  the guard hurts.
- Distinct `(candidate, owner)` pairs ÷ distinct candidates = **12.5×
  (gitlab) / 21.5× (mastodon)** — so the lever is a **per-CANDIDATE
  transitive ancestor closure**, computed once per merge; a per-pair memo
  cuts only 6–11% and is REJECTED as the primary lever.
- Soundness is structural: the walk reads only `override_classes` (M1-frozen)
  and `definers` (written at `:946`, immediately before the single
  `compute_literal_returns` call at `:947`); all readers take `&self`; the
  LSP rebuilds `SourceIndex` fresh per dispatch, so a merge-scoped cache
  cannot leak. `OVERRIDE_ANCESTOR_WALK_LIMIT` (100) was hit 0 times in 12
  corpus runs — the cap path is covered ONLY by a synthetic test (below).

## Design

Inside the Pass-4b call chain, thread a **local, lazily-filled**
`HashMap<String, ClosureEntry>` (a plain `&mut` parameter or a local in
`compute_literal_returns` — NO struct field, NO interior mutability). On the
first query for a candidate, build its closure by running EXACTLY today's
loop, owner-free; afterwards `related_to_owner(c, o)` ⇔ `closure(c).contains(o)`.

**Cap-boundary fidelity (the one subtle equivalence):** in today's loop the
owner check happens on POP — before the seen-skip AND before the
`visited > LIMIT` return, so the node that overflows the cap is still
owner-checkable, while nodes left in the queue are not. The closure must
reproduce that boundary exactly: record each first-popped node, stop when
the recorded count exceeds the cap (recording, not expanding, the
overflowing node). Duplicate pops are harmless (first pop already recorded
membership). Write this argument as a code comment on the closure builder.

**Mechanical companions (same slice):** reuse/borrow the
`override_ancestor_names` expansion inside the closure builder instead of
re-allocating `String`s per pop (~164k node expansions at gitlab).
`VecDeque` is optional (queue depths ≤ 24 measured) — if taken, BFS ORDER
MUST stay byte-identical: under the cap, closure CONTENT depends on visit
order. Update the now-stale doc comment on `related_to_owner` ("reuses the
same ancestor walk…") to describe the closure.

## Tests

1. **Equivalence oracle**: keep today's `related_to_owner` under
   `#[cfg(test)]` (the #92 pattern) and compare old-vs-new on the existing
   probe corpora AND on randomized synthetic hierarchies (reopened classes,
   includes, cycles).
2. **Cap test**: a synthetic chain deeper than `OVERRIDE_ANCESTOR_WALK_LIMIT`
   with the owner placed (a) just inside, (b) exactly at, and (c) just past
   the boundary — old and new must agree at all three. This is the only
   coverage the cap has; do not skip it.
3. All existing tests (incl. `probes_s92`, the order-leak and coupling pins)
   untouched and passing.

## Acceptance gates — the #92 battery, run BARE

1. `cargo test -p rigor-infer`, then `cargo test --workspace`.
2. clippy `--workspace --all-targets -- -D warnings`, fresh `CARGO_TARGET_DIR`.
3. `harness/run_snapshot.rb` — PASS, 0 unregistered extras.
4. Fresh release build, then `harness/fp_audit.py --gaps --sweep` — 0 FP /
   9,204, gap set unchanged (pin `PYTHONHASHSEED=0` on both sides until #96
   lands).
5. `rigor check` byte-identical vs a master-built binary on mastodon/app,
   default threads AND `RAYON_NUM_THREADS=1`.
6. `RIGOR_TIMING` stage-2 medians before/after at 1,236 / 3,117 / 4,675
   files. Expectation from the probe: stage 2 @ 4,675 drops from ~140ms
   toward ~80–90ms. Not a hard gate (machine noise), but the table is a
   deliverable — it feeds the NEXT slice's question: **does merge now fit
   the OverlayGuard's 100ms budget at 4,675 files?** Report that answer
   explicitly.

## Non-goals

No change to what "related" MEANS (same reachability, same cap); no LSP
changes; nothing outside the Pass-4b call chain; #96 is a separate slice.
