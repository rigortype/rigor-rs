# 2026-09-30 — issue #325: stored-slot narrowing under transparent recovery

PR #336, merged `351eb34`. Adversarial review Approved (~60 probes);
sweep 0 FP; CI 5/5. Residuals #342–#344.

## What landed

`Scope#with_indexed_narrowing` port — `IndexedFlow` records `h[k]` stored
types so `h[k]` reads answer the recorded narrowing ahead of `[]` dispatch:

- `Node::IndexWrite` gains `compound`/`operand` flags; recovery pushes the
  write whole in operand-transparent positions (descend stays for
  `blocked`/`suppressed` from #312).
- `flow_writes`: `IndexedFlow` (`SlotWrite` stable `(local, literal key)`
  `||=`-only + `SlotMutation` + `operand_spans`); `toplevel_mutations`
  gains `drop_key` for keyed `[]=` invalidation.
- `flow_eval`: indexed replay in reference order (invalidate → stored type
  on pre-widening env → widen + record); rebinds drop the name's records.
- `expr_type`/`call_dispatch`: `slot_stored_type` (declines unless
  `fully_tracked_receiver_type?`), truthy/falsey narrowing, `h[k]` `[]`
  reads consult the record.

## Fixed FP

`puts(*[h[:a] ||= "s"]); h[:a].upcase` — port fired `for 1`, ref silent
(kept narrowing reads `"s"`). Stale test row corrected: `x = (h[:a] ||= 1)`
fires `for 1` on the oracle.

## Residuals

- #342 index-target stores keep the slot record (needs
  `invalidate_indexed_write` on `drop_key` path) — edge FP.
- #343 compound attribute writes invisible to `collect_mutations` — edge FP.
- #344 coverage family (non-literal RHS collapse, branch-join unioning,
  ivar receivers, memberwise `apply_slot_mutation`, latent span-start
  ordering in `land_indexed_stored`).
