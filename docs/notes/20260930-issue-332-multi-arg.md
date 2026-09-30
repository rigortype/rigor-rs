# 2026-09-30 — issue #332: nominal multi-arg decline + when-pattern FP

PR #334, merged `3b2d613`. Adversarial review Approved (~60 adversarial
probes, no new FP); sweep 0 FP / 818; CI 5/5. Residuals #340/#341.

## What landed

- `local_reach` returns `(Reach, Option<Scalar>)` — `Some(s)` iff every
  reaching value folds to the same scalar (`pin_join` tri-state, `seen`-key
  loop discipline, depth cap).
- `expr_scalar` — foldable view of the pin: literals, locals via
  `local_reach`, call chains via `folding::fold`.
- `expr_reach`'s catch-all consults `expr_scalar` → `pinned_scalar_reach`;
  the tier-3 `arg_ty` conjunct declines nominal-typed multi args.
- `edge_evaluates`: `Node::When` patterns/conditions no longer entered
  (reference does shape analysis only, never scope-evals them).

## Fixed FPs

`"abc"[1+v]`/`"abc"[v..]`/`"abc"[v.to_i]` `for String`; `when`/`in`-pattern
closure `for Float`. Bonus coverage: same-scalar multi (`v=1; v=1 if c`)
now collapses to `Constant[1]` and fires like the reference.

## Residuals

- #340 pin-spoil coverage family (If/Logical/Case/BeginRescue-valued writes,
  `op=`/multi-write, fold-whitelist misses, `is_a?`-narrowed unions,
  `each` param element-fold) — all retractions vs master, safe direction.
- #341 sibling FP: write inside a `when` condition leaks through
  `local_reach`'s span scan (`when (q = 1; Integer)`).
