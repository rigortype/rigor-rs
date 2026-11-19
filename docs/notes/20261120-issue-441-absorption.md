# #441 — `local_reach` write-loop absorption (PR #698)

Slice B of the #441 design: the sub-cap W^4 band (`local_reach` walking W
conditional self-rewrites of one loop-carried local) closed by breaking the
reaching-write loop once the answer is saturated. Landed after slice A (#437's
order-independent fold memo, PR #687) and #435 (PR #634), exactly as the issue
body ordered them.

## Fix

One guard in `local_reach_uncached`'s write loop
(`crates/rigor-infer/src/reach.rs`):

```rust
if reach == Reach::UNKNOWN && pin == Some(None) { break; }
```

placed after the definite-assignment `continue` and the after-the-read
`continue`, before the join. `Reach::UNKNOWN` is `join`'s top element and
`Some(None)` is `pin_join`'s, so every later write joins into the same
`(reach, pin)` — the loop is provably pure cost past that point.

- The pin conjunct is implied today (every untyped contributor meets `None`
  into the pin, and `expr_scalar` gates the pin on `reach.precise &&
  !reach.untyped`, so `(UNKNOWN, Some(Some(_)))` is unreachable). It stays as
  lattice-level exactness; `debug_assert!(!reach.untyped || pin ==
  Some(None))` pins the invariant in debug builds.
- The brief's "different memoised tuple → audit panic" mechanism is not quite
  right: `RIGOR_REACH_MEMO_AUDIT` recomputes through the same code path, so a
  pin-only drift would not panic — it would return a different tuple to
  `expr_scalar` callers. The conjunct is still the right shape; the audit
  story just isn't what catches a missing half.
- Skipped writes' only effects are un-populated memo entries — `seen` stays
  balanced, the dead scan already ran, and a smaller `tested`/`folding`
  dependency record is still exact (the memo's hit conditions only get
  broader, correctly).

## Measured outcome (release)

| row | master `5a6eb0da` | head |
|---|---|---|
| regexp_parser 2.12.0 `scanner.rb` | 19.0 s | 0.56 s |
| `onevar_24` (issue-body shape, `.zz` in the loop) | 50.9 s | 0.25 s |
| `onevar_48` | 46m07s | 0.26 s |
| `lexer-F0.rb` / `lexer-F1.rb` | 3.95 / 4.19 s | 3.54 / 4.15 s |

- Sweep corpora + full gem tree (1,518 files): **byte-identical** stdout and
  exit, port vs port. Reviewer's own run: 3,200 generated programs (710 hit
  the break, instrumented count) + 3 gem trees, 0 diffs, 0 audit panics.
- `fp_audit --gaps --sweep`: **0 FP**; `gap_census --dump` byte-identical
  (437 rows).
- Audit mode clean over `run_snapshot.rb`, `cargo test`, every corpus, and
  the reviewer's 275-file gem subset.
- Gates: `gate.sh`, clippy 1.88, CI — all green.

## What the rows taught

- The triage's "band is gone" was measuring #440's `onevar` variant (a bare
  trailing `p`), which never reaches the write loop. The issue-body shape
  with `"abc"[p].zz` INSIDE the `while` is still in the band — the read must
  be a diagnostic-driving one inside the carrier. `harness/gen_onevar_band.py`
  checks the generator in.
- A residual band is possible in principle: a join that stays PRECISE never
  saturates and never breaks. None measured; #700 asks whether the #436 cap
  itself can now move.
- x6 (300 extra writes) does not exercise the guard — the #436 cap returns
  first. The cap and the guard answer `(UNKNOWN, None)` the same way, so
  output is unchanged either way.

## Filed

- #699 — `truthy_landed` first-completion-order dependence (pre-existing;
  absorption changes which fold lands first).
- #700 — re-measure the #436 cap's necessity post-absorption.
- #704 — the x5/`onevar` `.zz` coverage gap (loop-carried literal never pins;
  predates `3c45548`, top-level too, so not only #688).
