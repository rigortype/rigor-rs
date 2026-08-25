# S94 ancestor closure — implementation (2026-08-25)

Implements [the mini-spec](20260825-s94-ancestor-closure-mini-spec.md) from
[the Pass-4b cost probe](20260825-s94-pass4b-cost-probe.md) (issue #94). One
production commit plus this note. **Pure performance: no emitted diagnostic
changes anywhere**, under either thread count, on either corpus scale.

## What changed

Pass 4b's overridable-method degrade gate ran one ancestor BFS per
`(candidate, owner)` PAIR. It now runs one per CANDIDATE and answers every pair
from the resulting closure:

```rust
related_to_owner(c, o)   ==   ancestor_closure(c).contains(o)
```

* **`AncestorClosures = HashMap<String, HashSet<String>>`** — a plain local,
  created in `compute_literal_returns` and threaded by `&mut` through
  `resolve_fold_key` → `fold_key_sites` → `fold_expr` → `overridden_in_project`.
  No `SourceIndex` field, no interior mutability: the memo is valid exactly as
  long as `override_classes` is frozen, which is exactly this one call, and it
  cannot outlive a merge (an LSP dispatch rebuilds the index, so a longer-lived
  cache keyed by class name would serve stale answers between keystrokes —
  probe §3).
* **`ancestor_closure(candidate, closures)`** fills lazily on first use.
  `contains_key` + `insert` rather than the `entry` API on purpose: a HIT
  (12–22× more frequent than a miss) must not pay for `candidate.to_string()`.
* **`build_ancestor_closure(candidate)`** runs today's loop owner-free, using the
  `seen` set as the closure being built — a node enters it exactly when the old
  walk would have owner-checked it for the first time.

### Cap-boundary fidelity

The one subtle equivalence, written into the builder's doc comment. In the old
loop the `current == owner` test ran on POP — before the seen-skip AND before
the `visited > OVERRIDE_ANCESTOR_WALK_LIMIT` return. So:

* the node that OVERFLOWS the cap is still owner-checkable ⇒ the builder records
  it, then stops WITHOUT expanding it;
* everything left in the queue behind it never was ⇒ it stays out of the closure;
* a DUPLICATE pop needs no recording (its first pop already recorded membership)
  with one exception — `candidate` itself is pre-seeded into `seen` as the cycle
  guard and so is never recorded by a first pop, yet a cycle that walks back to
  it DID make it owner-checkable. Tracked by a `candidate_popped` flag and
  removed at the end when it was never reached.

That is also why BFS order stays byte-identical: below the cap the closure is
order-independent, but at the cap WHICH nodes make it in depends on pop order.

### Mechanical companions (from the probe's Q4)

* `Vec::remove(0)` → `VecDeque::pop_front`. FIFO order unchanged.
* The per-pop `String` re-allocation is gone: the expansion is moved into the
  queue and `current` is moved into `seen`, where the old walk cloned `current`
  on every pop (~164k node expansions at gitlab-foss/lib).
* The stale doc comment on `related_to_owner` ("reuses the same ancestor walk
  as method resolution") now describes the closure, and the walk itself is
  labelled as the pre-#94 body kept as an oracle.

## The equivalence instrument

`probes_s94`, following #92's `build_project_legacy` pattern: the pre-#94
`related_to_owner` is kept **verbatim** under `#[cfg(test)]` and is the oracle.
Each test grades EVERY ordered pair over the built index's whole class universe
(plus names the index does not know), through both the memoized entry point —
one shared `AncestorClosures` map, so cache hits are exercised — and a freshly
built closure, so a cache hit can never be what makes the two agree.

| test | shape | graded |
|---|---|---|
| `closure_matches_legacy_on_probe_corpora` | the #92 probe-1 four files (`probe1_sources`, now `pub(super)`) + the probe-2 reopen trio + cycles / self-superclass / self-include + a diamond + lexical nesting where the same short name resolves two ways | 351 pairs, 36 related |
| `closure_matches_legacy_on_random_hierarchies` | 120 generated projects (3 files each, 4–17 classes/modules, random reopens, includes, superclasses, nested namespaces, dangling names, cycles), deterministic xorshift64 seed | 19,308 pairs, 6,156 related |
| `closure_matches_legacy_at_the_walk_cap` | a 110-deep chain with the owner just inside the cap (`C9`, visit 100), AT the overflow (`C8`, visit 101) and one past it (`C7`) | all three agree; closure size exactly 101 |
| `closure_matches_legacy_when_the_cap_abandons_a_queue` | a 200-wide fan-out: `M100` overflows the cap and stays owner-checkable, `M101..M199` are abandoned in the queue | all pairs agree |

The corpus and random tests assert a floor on the number of RELATED pairs, so
they cannot pass vacuously. The cap is the only coverage
`OVERRIDE_ANCESTOR_WALK_LIMIT` has — the probe measured 0 cap hits in 12 corpus
runs.

**The oracle bites** (verified by three deliberate mutations, each reverted):

| mutation | caught by |
|---|---|
| do not record the cap-overflowing node | both cap tests |
| always drop `candidate` from its own closure | corpora (`cycles`) + random |
| `pop_back` instead of `pop_front` (LIFO) | the fan-out cap test only — below the cap, order is invisible |

That last row is the spec's warning made concrete: without a cap test that
leaves nodes in the queue, a BFS-order change would ship silently.

## Timing (`RIGOR_TIMING`, stage 2 = merge)

Interleaved BEFORE/AFTER, master-built baseline binary vs the branch binary, one
discarded warm-up per corpus per binary, **10 reps per side** pooled from two
interleaved passes (the machine was noisier than the #92 session; interleaving
puts drift on both sides equally, and the paired per-rep saving is reported
alongside the medians because of it).

| corpus | files | stage2 BEFORE (median) | stage2 AFTER (median) | delta | paired median saving |
|---|---|---|---|---|---|
| mastodon/app | 1,236 | 26.91 ms | **22.59 ms** | −16.0 % | −4.98 ms |
| gitlab-foss/lib/gitlab | 3,117 | 69.23 ms | **59.95 ms** | −13.4 % | −5.28 ms |
| gitlab-foss/lib | 4,675 | 164.91 ms | **82.49 ms** | −50.0 % | −72.83 ms |

Spread (min–max over the 10 reps): mastodon 23.83–37.49 → 19.07–29.41;
lib/gitlab 56.51–77.08 → 52.59–77.48; lib 133.49–205.55 → 76.94–130.87.
A pre-change baseline on a quiet machine (master binary, 5 reps) read
27.22 / 56.63 / 136.12 ms — i.e. the BEFORE column above is inflated by machine
load at the two smaller scales, which is exactly why the interleaved paired
saving is the number to trust there.

The shape matches the probe's prediction: the win is scale-dependent and lands
where `overridden_in_project` dominates 4b (70 % at gitlab-foss/lib, 38 % at
mastodon/app). At 4,675 files the AFTER runs are also far more STABLE
(76.9–130.9 vs 133.5–205.5), which is what removing an O(pairs) walk from the
serial barrier should look like.

### Does merge fit the OverlayGuard's 100 ms budget at 4,675 files?

**At the median, yes — 82.5 ms, down from 164.9 ms.** 9 of 10 measured reps came
in under 100 ms (76.9–96.7 ms); the tenth was 130.9 ms on a loaded machine. So
the budget is now met with roughly 20 % headroom at the median but no headroom
in the tail: stage 2 alone no longer breaks the guard, though a single dispatch
under load still can. Retiring the guard needs either a tail-aware measurement
on a quiet machine or the next lever (the fold walk itself, which the probe put
at 27.3 ms of gitlab-foss/lib's 4b — the majority share at mastodon scale).

## Gate verdicts

| # | gate | verdict |
|---|---|---|
| 1 | `cargo test -p rigor-infer` | **PASS** — 277 passed (273 before, +4) |
| 1 | `cargo test --workspace` | **PASS** — 376 / 4 / 3 / 9 / 94 / 277 / 47 / 251 / 48, 0 failed |
| 2 | `cargo clippy --workspace --all-targets -- -D warnings`, fresh `CARGO_TARGET_DIR` | **PASS** — no findings, no new `#[allow]` |
| 3 | `harness/run_snapshot.rb` | **PASS** — 98 fixtures, 407 matched, 2 registered divergences, **0 unregistered extras** |
| 4 | `PYTHONHASHSEED=0 harness/fp_audit.py --gaps --sweep` | **PASS** — 0 FP / 9,204 files, gap set **byte-unchanged** vs the branch-point baseline (8 corpora present, 0 absent) |
| 5 | `rigor check` vs the master-built binary | **PASS** — stdout + stderr + exit byte-identical on mastodon/app AND (extra) gitlab-foss/lib, under default threads and `RAYON_NUM_THREADS=1`; the new binary's two thread modes agree |
| 6 | `RIGOR_TIMING` stage-2 before/after at 1,236 / 3,117 / 4,675 | table above; guard-budget question answered above |

## Deviations from the spec

* **None on the design.** The closure is a local `&mut`-threaded map, the cap
  boundary is reproduced and argued in a code comment, `VecDeque` was taken with
  BFS order preserved, the per-pop `String` clone is gone, the legacy walk is the
  `#[cfg(test)]` oracle, and the stale doc comment is updated.
* Test-support widening: `probes_s92::probe1_sources` became `pub(super)` so the
  #94 harness can grade the same corpus. Test-only, no production surface.
* Gates 3, 4 and 5 were each run twice — once mid-implementation, once against
  the FINAL binary — because whitespace-only edits to production code DO change
  the release artifact (line tables move), so a mid-implementation sweep cannot
  be assumed to have graded the committed tree. The verdicts above are the
  final-binary runs; the committed tree rebuilds to exactly the release binary
  they measured (`sha256 a9978aed…`), and `run_snapshot.rb`'s own staleness
  check caught the one stale debug binary before it could grade anything.
* Gate 6 used 10 interleaved reps per side (spec said 3–5) after the first pass
  showed the two smaller scales were noise-dominated.
* No `harness/` changes (#96 is a separate slice); `PYTHONHASHSEED=0` is pinned
  on both sides of the gap-set comparison until it lands.
