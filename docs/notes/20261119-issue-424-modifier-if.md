# #424: modifier-`if` chain (`1 if x if x…`)

PR #669 → merged 2026-11-19 (`55fbd787`). Refs #424 (the modifier-`if`
row). Local-read chains already linear after #658. Nested class /
sequential `x = 1 if y` × N remain.

## What was slow

A modifier chain is two Prism levels per link. Two walks re-scanned the
remaining suffix at every level:

1. `unconditional_exit` evaluated the then-arm before noticing there is
   no `else`. An `if` without `else` never exits, so that walk was
   wasted and quadratic.
2. `compact_children` (`MainSelfCalls`) used a full Prism `Visit` and
   kept only depth==1 nodes.

Master (release `check`): `1 if true if true…` 10k **2.16 s**;
`1 if x if x…` 10k **5.70 s**. 20k is refused by the 25,000 depth bound.

## Port shape

- `unconditional_exit` / `rescue_arm_exits` check `subsequent` /
  `else_clause` first. Identical predicate.
- Control-flow kinds: type-specific child walk, no descend. Other kinds
  (`Call`/`Def`/`Parameters`) keep master's depth==1 visitor — emptying
  those dropped mixin-taint writes inside blocks (review round 1).

## Measured (release `check`)

| shape | master | after |
|---|---|---|
| `1 if true if true…` 5k / 10k | 0.74 / 2.16 s | 0.20 / 0.52 s |
| `1 if x if x…` 5k / 10k | 1.53 / 5.70 s | 0.63 / 2.58 s |

Sweep **0 FP**; gaps 44 / 203 / 87 / 26 unmoved. Two-round Opus review
(Needs fix → Approved on the fallback). `time_nesting.py --all` includes
local-read and modifier-if rows.

## Follow-ups

- `main_self_control_flow` and `walk_compact_root` list the same 25
  kinds by hand (design companion).
- Nested `class`/`module` (brief out of scope). Sequential `x = 1 if y`
  × N is a different family (flat, not nested).
