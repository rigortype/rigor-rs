# Slice 1 finding — the treemaps gap is blocked on ARRAY constant-folding (2026-07-06)

ADR-0038 Slice 1 (possible-nil in block scopes) was implemented on the branch
`flow-substrate-slice1`. The substrate works and is FP-safe, but it closes **zero
measured survey gaps** — because the one Slice-1-reachable gap (treemaps) needs a
feature the recon did not identify: **array constant-folding**. This note records
why, so the next attempt targets the real prerequisite.

## What was built (correct, FP-safe, on the branch)

- **Shared `type_dot_new`** — block-bearing `X.new(…){…}` now types as an `X`
  instance (plain + block paths agree). Correct, 0 FP, but closes 0 undefined-
  method gaps on the survey (measured: algorithms undefined-method 36 → 36).
- **`nilable_receiver_snapshots`** — a threaded flow-eval (type env inherited into
  blocks, nilability facts fresh per block, straight-line with block descent,
  decline-all on any unmodeled construct) that REPLACES the `enclosing_def`
  span-scan. `check_nil_receiver` fires from the snapshot map. Harness 53/53,
  0 FP; the existing def-scope fixtures (27 fire / 28 negatives) still pass.

## Why treemaps needs array-folding (the blocker)

treemaps fires possible-nil on `select_subset.size` where
`select_subset = random_array[0..n]` and `random_array = Array.new(300000){…}`.
The nil source is `Array#[](Range) -> Array?`. Building that source revealed the
reference gates it entirely on its **constant-folding**, measured against the
oracle:

| receiver of `x[0..k]`            | reference | why |
|----------------------------------|-----------|-----|
| `Array.new(n)` with **n ≤ 16**   | silent    | folds to a concrete array; slice folds to non-nil |
| `Array.new(n)` with **n ≥ 17**   | **fires** | too big to fold ⇒ stays `Array` nominal ⇒ `Array?` |
| array literal `[…]` (any size)   | silent    | concrete array ⇒ slice folds to non-nil |
| `[…].map{…}` (method-return)     | silent    | folds |
| **string** literal `"…"`         | silent    | folds (`"hello"[0..2] ⇒ "hel"`) |
| `String.new(…)` / interpolated   | **fires** | not folded ⇒ `String` nominal ⇒ `String?` |
| param string / array             | silent    | (in these samples) |

rigor-rs types `Array.new(…)` and array literals as `Nominal[Array]` (it does NOT
fold them), so an Array-slice source over-fires on exactly the cases the reference
folds (`Array.new(≤16)`, literals, `.map`) — **confirmed FPs** on synthetic
`arr = Array.new(10){…}; sub = arr[0..5]; sub.size` (rigor-rs fires, reference
silent). The fold threshold (16) is a reference-implementation constant; matching
it is fragile and reference-version-specific.

## What shipped FP-safely (but pays nothing on the survey)

Restricting the slice source to **String** IS FP-safe, because rigor-rs's
`Constant`-vs-`Nominal` split for String aligns with the reference's fold-vs-
nominal split (string literals are `Constant` ⇒ declined = folded-silent;
`String.new`/interpolated/method-return are `Nominal` ⇒ fire). Verified 6/6 vs the
oracle. But **no String-slice-in-scope possible-nil gap exists in the survey**, so
it closes nothing. Measured possible-nil gap delta (baseline vs Slice 1),
FP-safe build: algorithms 50→50, mail 4→4, redmine 6→6, net-ssh 12→12,
mastodon models 0→0 / services 4→4, concurrent-ruby 1→1 — **all zero.**

## Why block-scope alone doesn't pay (the recon was half-right)

The [recon](20260706-nil-flow-substrate-recon.md) concluded "the paying piece is
possible-nil across block/top-level scopes." That is necessary but NOT sufficient:
the treemaps gap needs block-scope **AND** array-folding. The other survey
possible-nil gaps are a different tier entirely — sampled algorithms gaps
(`t.left` in `splay_tree_map.rb`, `t.right`, heap `.key`) are **project-method /
ivar nilable returns with loop-flow narrowing** (`break unless t.left`), i.e.
Tier B/C (project nilable-return inference + ivar typing + loop narrowing), not
core-RBS-return or slice sources. So Slice 1's source model reaches none of them.

## Options (design decision)

1. **Array constant-folding** — fold `Array.new(≤threshold)` + array literals +
   pure array methods to concrete `Constant` arrays, so an Array-slice source
   becomes FP-safe (Constant-decline handles the folded cases; large/nominal
   arrays fire). Unblocks treemaps. Cost: a real folding feature; the threshold
   must match the reference (fragile). This is the true treemaps prerequisite.
2. **Ship the substrate anyway** — it retires the span-scan and is the FP-safe
   foundation later slices build on, but closes 0 survey gaps today (violates the
   ADR-0038 §5 "a slice that closes no gaps is not shipped" gate).
3. **Revert Slice 1**, keep only the ADR + this finding; re-target the possible-
   nil effort at Tier B/C (project nilable returns + ivar + loop narrowing), which
   is where the bulk of survey possible-nil gaps actually live.
4. **Re-target Slice 1** to always-truthy on the substrate, or another cluster.

## Recommendation

The substrate + `type_dot_new` are correct and worth keeping, but shipping them as
a "Slice 1" that closes zero gaps contradicts the ADR's own gate. The honest
read: **block-scope was not the whole story; the treemaps gap is fold-gated, and
the rest of the possible-nil cluster is Tier B/C.** Prefer option 3 (revert to a
clean substrate-less master, keep the ADR + this finding) OR option 2 explicitly
re-scoped ("substrate landing, 0 gaps, foundation") if the span-scan retirement is
judged valuable on its own — a call for the maintainer.
