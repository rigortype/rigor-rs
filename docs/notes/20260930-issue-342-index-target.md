# Issue #342 — index-target stores drop the IndexedFlow slot record

PR #350, merged `7f1f5b5` (head `d21c18e`). One adversarial round, Approved.

## What landed

#325's `IndexedFlow` recorded `h[k] ||= v` stored-slot types, but index-TARGET
stores (`h[:a], z = 5, 6` / `x, h[:a] = …` / `for h[:a] in xs` / `rescue =>
h[:a]`) emitted `("[]=", drop_key: None)` — the record survived an overwrite the
reference drops via `IndexedNarrowing.invalidate_indexed_write`
(`indexed_narrowing.rb:142-150`, from `widen_index_target`
`statement_evaluator.rb:1026/:2135/:2154/:5106`).

- `rigor-parse/src/ast/multi_target.rs`: `IndexWrites` is now
  `Vec<(String, Span, Option<IndexTargetKey>)>`; `IndexTargetKey`
  (`Sym`/`Str`/`Int`, mirroring `stable_key`'s `STABLE_KEY_NODES`) is computed at
  lowering by `index_target_key()` — `Some` only for a **bare**
  `LocalVariableReadNode` receiver with a literal first index arg.
- `flow_writes.rs` forwards the key as `drop_key` for the MultiWrite / Loop /
  BeginRescue arms; `drop_indexed_mutation` drops the specific slot.
- Parity details verified: `h[:a, :b]` drops `(h, :a)` only; parenthesised /
  assigned / conditional receivers decline like `stable_receiver`'s node-kind
  gate; join intersection makes the unconditional drop correct.

## Measured

~70-probe review matrix: all issue-row shapes + `def`/`class`/`begin`/loops
silent on both engines; over-drop declines (`h[:b]`, `h[k]`, `(h)[:a]`,
`it[0][:a]`…) identical. Controls fire (`h[:b], z` keeps the record).
`fp_audit --gaps --sweep`: 0 FP / 818 gaps.

## Residuals

- **#352** — String receiver: `s[0] ||= "x"; s[0], z = 5, 6; s[0].frobnicate`
  fires `for String` — the drop unmasked a pre-existing `s[k] = v` read-typing
  divergence that already shipped on master.
- **#353** — ivar receivers never get slot records (ref records `@h[:a]`).
- **#354** — Bignum index keys never recorded (`IntegerLit{value: None}`).
