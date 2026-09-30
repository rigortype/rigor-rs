# 2026-09-30 — issue #306: unlinked effect spans in flow_children

Fixes the FP regression introduced by #136's per-site operand env replay:
`path_unconditional`'s "inside `id` but no linked child → true" fallback minted
unconditional mutator nominals for spans `flow_children` never linked.

PR #320, merged `d8af094`. Adversarial review Approved (~57 probes + ~40
counterexamples); sweep 0 FP / 818 gaps; CI 5/5.

## Fix

- `Node::Range` gained `left`/`right: Option<NodeId>`; `flow_children` links
  bounds as `Uncond` in order (reference `OPERAND_CONTAINERS` evaluates bounds
  unconditionally, left→right).
- `path_unconditional` returns false for `Loop::index_writes` and
  `BeginRescue` clause spans — the decline direction; the reference joins the
  zero-iteration scope (`eval_for`/`join_with_nil_injection`) and binds
  `rescue =>` inside the clause edge (`bind_rescue_reference`), so those
  positions are conditional, not unlinked.
- `descendants_of`/`node_child_ids` deliberately keep Range bounds unlinked:
  under-marking keeps writes as rebinds (safe side).

## Probe outcomes

- `for h[:k] in xs; h.frobnicate`, `rescue => h[:k]; h.frobnicate`,
  `(b.unshift("s"))..b.first.upcase` — silent on both engines now (were
  `for Hash`/`for Hash`/`for 1` FPs).
- Destructured/splat for-indices, multi-clause rescues, `...`/beginless/
  endless ranges, nested ranges — all silent/silent.
- Controls still firing: `b.first.upcase..(b.unshift("s"))` (fires ×2),
  sites inside rescue clause bodies, swapped-arg #136 row.

## Residuals filed

- #321 loop-predicate/`for`-collection mutations widen `Dynamic` not the
  carrier (`while b.shift`, `for x in b.unshift(9)` — ref fires).
- #322 `for`-expression operands and loop-body sites replay to all-Dynamic;
  also swallows the paren-seq-in-bound row (was a drifted-message emit).
- Review-verified pre-existing: #309 (rescue body effects), #301
  (`for obj.m[:k]` receiver), #307 (range rebind joins), #310 (message drift).
