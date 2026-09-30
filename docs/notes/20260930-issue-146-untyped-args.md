# 2026-09-30 — issue #146: two untyped-argument false positives

PR #318, merged `1c74bea`. Adversarial review Approved (~90 probes, no new
FP family); sweep 0 FP / 818 gaps; CI 5/5. Residuals #330–#332.

## What landed

- `reach.rs`: `Reach::multi` flag; operand-aware `local_reach`; helpers
  `unrecorded_closure`/`edge_evaluates`/`closure_bound_elsewhere` mirroring
  the reference's `propagate`/`closure_scope`.
- `call_dispatch.rs`: tier-3 constant-receiver decline for multi-valued
  (incl. union-shaped) arguments.

## Fixed FPs (reference silent, port fired)

- `NL3 = { a: lambda { |q| q = 1; Float(q).w_nl3 } }` — lambda in an
  unentered operand position no longer evaluates the rebound param.
- `v = 1; v = 2 if c; "abc"[v].w` — union-literal args now decline the
  constant-receiver fold instead of picking one member's overload.

## Also removed FPs (verified in review)

`x.f = lambda{…}` (extra `for Float`), `"abc"[v]` inside top-level `while`
(`for String`).

## Residuals

- #330 new retractions: `case/in` bodies floored via phantom
  `UnmodeledWrite`; `=>` subjects via `Recovered` carrier edge.
- #331 `multi` over-decline family: same-value rewrites, `op=`, union
  member-folds, chained declines — all coverage loss.
- #332 pre-existing: nominal-typed multi args (`"abc"[1+v]` keeps
  `for String`); `when`-pattern `edge_evaluates` FP.
