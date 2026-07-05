# nil/flow substrate (ADR-0022) — coverage-gap recon (2026-07-06)

Goal: close the two biggest coverage-gap clusters vs the reference on `rigor-survey` —
`call.possible-nil-receiver` (~118) and `flow.always-truthy-condition` (~117). This
note records the reconnaissance so the next attempt doesn't repeat it.

## Current substrate state

Partial. `Typer::always_truthy_snapshots` + `flow_eval_*` do straight-line
*constant* folding for `flow.always-truthy-condition` (fire only when a predicate
folds to a value-pinned constant). `check_nil_receiver` handles
`call.possible-nil-receiver` inside a **named `def`** for a **bare local** whose
single source is `x = <call>` with a CERTAIN nilable core RBS return, plus an
aggressive guard-decline scan (`nil_local_is_guarded`) as the zero-FP keystone.
The full ADR-0022 edge-aware narrowing (5 edges / 6 buckets) is NOT built.

## Gap patterns (by tier)

- **Tier A — possible-nil scope + source.** e.g. algorithms `treemaps.rb`:
  `select_subset = random_array[0..n]; select_subset.size`. Two blockers:
  (a) **source**: `Array#[]`'s overloads disagree on class (`Elem?` vs `Array?`),
  so `method_return_nilable` collapses to `None`; the reference selects the
  `Range` overload (`→Array?`). Also `Array.new(n) { … }` (block-bearing `.new`)
  types Dynamic in rigor-rs (block calls skip the tier-4 `.new`→instance path).
  (b) **scope**: the real occurrences are in **top-level / nested blocks**
  (`RBench.run do … report do … end`), not `def`s — `enclosing_def` defers.
- **Tier B — always-truthy nil-guard narrowing.** `return unless x; if x` ⇒
  always-truthy. Needs the edge-aware truthy/falsey scope threading. FP-safe by
  nature (only fire on proven).
- **Tier C — ivar value-flow always-truthy.** deque `pop_front`: `if @size == 1`
  proven false — needs ivar-field value tracking (ADR-58-class). Deepest.

## Key finding (why bounded slices don't pay off)

A bounded, FP-safe **source-resolution** slice was built and measured: block-`.new`
instance typing (`type_dot_new` shared by the plain + block paths) + collection-
slice nilability (`arr[Range]`/`str[Range]` → nilable self-collection, arg-aware).
It correctly fires the treemaps pattern **when placed in a `def`** and is 0-FP
across 15 corpora — **but closes ZERO real corpus gaps** (before/after gap delta
identical on algorithms/oj/net-ssh/mail/redmine), because every real occurrence is
in a **block / top-level** scope, not a def. So the source fix is a no-op without
the scope-handling.

**⇒ The paying piece is the FP-delicate part: possible-nil across block / top-level
scopes** (a nilable local assigned in one block and used in a nested block, with
guard-decline extended across block boundaries — where capture/re-entrancy make
FP-safety hard). This is not a quick bounded win; it needs a designed edge/scope
model with the FP audit (0-FP across the survey) as the gate on every increment.

## Recommendation

Treat the substrate as a designed effort (like the sidecar reversal): specify the
scope/edge model + the FP-safety strategy for block-crossing narrowing first, then
land it in gated slices. The reverted source-resolution work (`type_dot_new` +
collection-slice nilability) is correct and can be re-introduced ONCE the scope
piece makes it pay off — it is a prerequisite, not a standalone win.
