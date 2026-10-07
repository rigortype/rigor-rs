# Issue #368 — dead-position writes no longer leak past their carrier

PR #385, merged `0b05b06` (final head `06712ac`). Two adversarial review
rounds; r1 found clippy regressions + a `RescueArm.span` sibling-clause
regression + a wrong ensure-env join; r2 found a `!`-fold over-generalization
with a genuine strict-subset FP. All fixed and verified.

## What landed

A new `crates/rigor-infer/src/dead.rs` (~500 lines) plus hooks in
`reach.rs`/`flow_eval.rs`/`expr_type.rs`/`typer.rs`/`literal_fold.rs`:

- **Dead-position exclusion**: writes inside constant-folded dead arms
  (`if true … else (q=1)`), dead clause-rescue arms (`rescue; q=1; raise`),
  and block/lambda/loop bodies under rescue modifiers no longer bind —
  matching the reference's pruning. Dead bodies stop emitting
  `undefined-method for nil` on provably-dead receivers.
- **`RescueArm.span` clipped to the clause's own extent** — Prism's
  `RescueNode#location` spans following clauses, so writes in later clauses
  were judged by earlier dead clauses. Bonus fix: the sibling-clause-read
  family also converged (dead clause writes no longer leak into a live
  sibling's reads).
- **`clause_retries` boundary**: only `BeginRescue` nodes with real clauses or
  an ensure body are retry boundaries — the builder's empty-shell reuse for
  if-else/case-in/parens now passes through correctly.
- **Clause retry widens over the whole `begin` span** (retry re-runs the try
  body — the `attempts += 1` idiom), and **ensure env joins only live
  rescues** (`live_rescues`, not all clause exits) — r1's initial join was
  verified wrong vs the oracle and corrected.
- **`!`/`not` fold restricted to `Nil | Bool` operands** (delegating to the
  existing `folding::fold` boundary, matching `BOOL_UNARY`/`NIL_UNARY`).
  Truthy non-Bool scalars now decline certainty → both arms stay live.
  Fixed a strict-subset FP (`flow.always-truthy-condition` on `def m = !"x"`)
  plus systematic coverage regressions on `!!x`/`not x`/`r = !x`.
- Union rescue bindings: `rescue A, B => e; e.w` now types
  `ArgumentError | TypeError` like the oracle (master declined);
  toplevel `rescue => e` types `e` as `StandardError`.

## Measured

~110 probe rows across r1+r2 + parent verification; sweep 0 FP on all heads;
CI 5/5 on `06712ac`.

## Process notes

- GitHub Actions stalled on dispatch for ~6 days (platform-side), recovered
  on the amended push. Local gates + reviewer-probe coverage carried the load.
- Two implementer sessions died mid-work; both times the uncommitted tree
  compiled clean and was preserved as WIP commits (`f75afd9`, `1e7e008`) —
  zero work loss.

## Residuals

- **#386** — `branch_terminates?` Bot-half (`raise`-via-method), barrier-scope
  flat-env leaks (`begin/rescue` inside block bodies), `begin…rescue` main-body
  clause-read leak, `if (q = nil)` missing warning, join message drift.
