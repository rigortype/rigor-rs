# #670: decline class-guard narrowing on a dynamic-base operand

PR #671 → merged 2026-11-19 (`981cf15`). Closes #670. Approved (Opus 5.5
on `6c7e9ec7`). Follow-ups #672 (rooted `::Stat`), #673 (`rescue k::`).

## What diverged

`resolved_static_constant` / `guard_constant` used the trailing name of a
dynamic-base `ConstantRead` (`k::String`, `self::String`,
`self.class::String`). The port narrowed `x` to `String` and fired
`call.undefined-method`; the reference's `qualified_name_or_nil` /
`static_class_name` is nil, so the guard is opaque. Leftover of #655.

## Port shape

The same `dynamic_base` flag #655 used at the typing sites, at the
operand resolvers. `self_anchored` is a subset. `dead.rs` `Isa` and the
`reach.rs` `is_a?` fold get the same gate so a predicate fold cannot
treat `k::String` as `String`. The reference keeps both arms live under
a dynamic-base predicate.

## Measured

Brief rows + control (`k::String === x`) + must-still-fire (`String` /
`::String`) byte-identical. Reviewer re-probed 84 rows: 13 master FPs
removed, some `&&` coverage gained, 0 new FPs. Sweep **0 FP**, gaps 360
(unchanged vs #655). Snapshot 0 unregistered, 959/1063.

## Residuals (unchanged, pre-existing)

- Rooted `::Stat` inside `class File` walks the lexical ladder — #672.
- `rescue k::ArgumentError` binds as `ArgumentError` — #673.
- Literal-bound locals after `is_a?`/`case`, static `String === x`,
  `flow.unreachable-clause` on `case 1; when String` (coverage, master
  identical). Fable: later, `ConstRef` / `ast.static_const` so a lenient
  `&str` is not the default.
