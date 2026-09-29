# 2026-09-29 — issue #134: index-target widening in multi-assign, for, rescue

**PR #295, merge `e5613cc`.** Closes #134. Upstream `e59b7b89`: rigor#1209 (`c078b5e2`) + rigor#1211 (`198572df`), both through `IndexWriteWidening` → `MutationWidening.widen_receiver_aliases`. Residuals filed: #298–#304.

## What landed

- `MultiTarget::Index { receivers, span }` (rigor-parse) carries the receiver's mutated local reads — the local-only half of `ReceiverAlias.mutated_reads`, walked through parens/statements/`if`/`unless`/`&&`/`||`/local-writes, depth-capped.
- `MultiTargets::index_writes()` / `for_index_writes` / `rescue_reference_index_writes` report `(receiver local, target span)` pairs for whole-index, multi-slot, nested and bare `*h[k]` splat targets. `Node::Loop` and `RescueClause` gain `index_writes`.
- `collect_flow_writes` + `toplevel_mutations` record index-target `[]=` writes keyed by target span so containment widening applies; `MultiWrite` arms in `flow_eval`/`collection_shape`/`nilable`/`class_narrowing` widen receivers *after* bindings (`h, h[:a] = h, 1` stores into the rebound `h`); mutation kills `Narrowed`/chain facts, not `kill_local`.
- The stored slot's value is deliberately NOT joined as content evidence — widening is the strict-decline half of the reference.

## Measured

- All three brief shapes silent on both engines (`h[:a], z = …`; `rescue => h[:e]`; `for h[:a] in xs`); control `h={}; if h[:a]==1` still fires identical tuple+message.
- Upstream fixtures: port emits a strict subset — multi_write fixture ref `@64:16/@86:20`, port `@64:16`; for/rescue fixture ref `@44/@49/@57/@83/@121`, port `@44/@49/@57`.
- CI green on `76bc5bc`; sweep 0 FP over 8 corpora; gate.sh pass.
- Review (Opus adversarial, ~70 counterexample snippets + fixtures re-probed): **Approved**, all findings non-blocking.

## Residuals (filed)

#298 `h[k] +=/&&=/||=` never widen (+ `||=` wrong-polarity fold — upstream `NODE_CLASSES` unported); #299 conditional index stores over-widen non-carrier bindings (new-vs-master decline); #300 receiver-expression predicate folds; #301 unbound for/rescue index receivers unlowered; #302 `collect_flow_writes` ignores block-param shadowing; #303 scalar-RHS multi-assign slot typing; #304 `Range[::int]` message drift.
