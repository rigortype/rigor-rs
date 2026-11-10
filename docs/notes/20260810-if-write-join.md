# Post-`if` env: join the conditional write's type, not a blanket `Dynamic`

Issue #443 → PR #572 (merged). `v = nil if p` on `v = nil` widened `v` to
`Dynamic` in BOTH check envs (`bind_check_statement`'s non-folded `if` arm
and `class_flow_if`'s `widen_flow_writes`), so `v.zz` stayed silent where
the oracle fires `for nil`, and `w = v ? 1 : "s"` joined on an untyped
predicate — `for "s" | 1` instead of `for "s"`.

## Shape

- `bind_check_statement`'s `None` (non-folded) `if` arm replaces
  `widen_flow_writes` with a per-name join: each local written inside the
  `if` span joins `entry ∪ last-write` per edge (`arm_write_ty` types the
  arm's LAST write — plain `v = e` via `type_of`, `op=` via
  `op_write_result`, `v, w = rhs` via `multi_target_binder`). A
  terminating arm contributes the entry type. A disagreeing write unions
  (`1 | "s"`) — decline-side, matching the oracle's own union.
- `class_flow_if` mirrors the join for `tenv`, hoisting the arm `tenv`s
  out of `truthy_edge`/`falsey_edge` (they now return `(c, t)`).

## Accepted declines

Same-class literal unions the oracle fires on and the port cannot witness
(`for 1 | 3`, `for 2 | 3`, `for "A" | "a"`, `for "s" | 1` inside `each`).

## Fold-in note

The issue's two headline rows (dead-arm `"abc"[v].zz`, the `"s" | 1`
ternary) were ALREADY fixed by #571's `DeadOp` narrowing — this slice
closes the `pre`-env join the review surfaced.
