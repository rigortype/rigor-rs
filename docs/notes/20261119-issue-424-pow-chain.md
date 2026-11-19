# #424: memoize env-free `**` / call / `+` chains

PR #649 → merged 2026-11-19 (`2d47177`). Refs #424 (the `**` row). Follow-ups
stay on the issue: `A::A::…` still times out at 10k; a chain that reads a
local anywhere skips the `type_of` memo and stays quadratic.

## What was slow

A right-nested `2 ** 2 ** …` re-walked each remaining suffix from every
call site. `arg_reach` (`expr_scalar` / `expr_reach`) plus `pin_arg_scalars`
(`type_of`) made it cubic-to-super-polynomial. Master on this host:

| size | time |
|---|---|
| 100 | 1.04 s |
| 200 | 3.15 s |
| 400 | 38.5 s |

The oracle is ~quadratic with a small constant (400 → 13.8 s). Real code
never reaches these depths (deepest `Visit` nesting in the 19k-file corpus
is 40); 400 links is small enough for generated input.

## Port shape

- `expr_scalar` / `expr_reach` memoize per node in the existing `ReachMemo`
  window (same `seen` / dead-set / in-flight-fold discipline as
  `local_reach`, rigor-rs#422).
- `type_of` of a `Call` whose span holds no env-sensitive node (local /
  ivar / cvar / gvar read, local `op=`, lambda, block call) memoizes in
  the same window. Only interner-free `Type`s are stored (flat Constant /
  Nominal / Singleton / `Dynamic` over a pre-interned atomic).
- AST keys switch from `(pointer, len)` to `LoweredAst::uid`. A freed
  arena's address reused by a later same-size tree served `.first` as
  `.last` (`tuple_projection_folds`). The uid rides `CarrierSpans`' box
  so `enum Lowered` stays under `clippy::large_enum_variant` (#642's
  LineIndex pattern).

`harness/time_nesting.py` regenerates the table.

## Measured (release `check`)

| shape | size → time | size → time | growth |
|---|---|---|---|
| `2 ** 2 ** …` | 800 → 0.41–1.02 s | 1600 → 0.46–0.56 s | ×0.55–1.13 |
| `x.a.a…` (bare `x`) | 10k → 0.45–0.51 s | 20k → 0.59–0.65 s | ×1.16–1.45 |
| `1+1+…` | 10k → 0.61–0.68 s | 20k → 0.87–0.88 s | ×1.28–1.46 |
| `A::A::…` | 10k TIMEOUT | 20k TIMEOUT | still super-linear |

Literal-only call / `+` rows fall out of the same memos. Rooted at a local
or ivar they stay quadratic (`v ** v ** …` 800 → 1.55 s, 1600 → 5.48 s;
`v+v+…` 5k → 24 s). `check --format json` byte-identical to master on
mastodon `app/` and gitlab `lib/` (1,200 files each). Sweep **0 FP** /
8,251 files, gaps 360 unmoved. Opus Approved.
