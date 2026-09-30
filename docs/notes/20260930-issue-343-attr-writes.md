# Issue #343 — compound attribute writes own-lowered, invalidate IndexedFlow

PR #358, merged `1f98e5c` (rebased head `bb1aaec`). One adversarial round, Approved.

## What landed

`h.attr ||= / &&= / +=` were `is_unmodeled_write` — `collect_mutations` never
saw the receiver store, so IndexedFlow slot records survived a mutation the
reference widens/drops via `widen_attribute_write`/`IndexedNarrowing.mutator?`.

- `Node::AttrWrite{receiver, read_name, write_name, compound, safe_nav,
  evaluated, value, span}`; `IndexCompound` renamed `Compound` (shared with
  `IndexWrite`).
- `CallOr/And/OperatorWriteNode` own-lowered in builder.rs with
  `typed_depth`/`closure_depth`/`recovery_suppressed` counters marking
  non-evaluated operand positions (call args, splat, containers,
  interpolation, `return`, `in`-patterns, rescue-modifier operands, block/lambda
  bodies); recovery.rs gained `typed`/`closure` marks.
- `toplevel_mutations` emits `(h, write_name)` only for `evaluated` writes on
  `is_shape_mutator` writers; `expr_type` types `||=`/`&&=` as
  `narrow_{truthy,falsey}(recv.attr) | rhs` (`op=` declines — see #365).
- Threaded through flow_eval/class_narrowing/nilable/collection_shape/
  block_call/def_attribution/rules traversal/CLI `type-of`.

## Measured

Review: ~50 fresh-dir probes + post-rebase `fp_audit --gaps --sweep` 0 FP.
All issue rows silent incl. `&&=`/`+=`/`default_proc`/`h&.x`/`@h.x`/`C.x`
variants; non-mutator writers (`h.count ||=`) still fire identically;
pure typed-operand positions agree; controls keep firing.

## Residuals

- **#361** — `OperandEffects.any?` gate absent from `evaluated`: attr writes
  inside effect-bearing operands (`puts(h.default ||= (y = 1))`, kwargs,
  interpolation, rescue arms, `ensure`) keep records — port-only FP,
  pre-existing on master.
- **#362** — `evaluated` omits `recovery_blocked`: `when`/`in` conditions and
  terminated arms over-drop (new, safe-side).
- **#363** — `break`-arg mutations propagate past `join_break_scopes`'
  locals-only join (new, safe-side).
- **#364** — `(h).x ||= v` parenthesized receiver over-resolves vs
  `stable_receiver` (new, safe-side).
- **#365** — compound attr-write value types collapse to `Dynamic`.
- **#366** — in-block attr write doesn't drop records visible inside the
  block (pre-existing).
