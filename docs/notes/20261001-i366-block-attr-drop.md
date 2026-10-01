# Issue #366 — in-block attr writes drop indexed records

PR #377, merged `6382719` (head `7839f57`). Approved after adversarial review
(~50 probes over nested blocks, eval-order, sibling arms, dead positions).

## What landed

`[1].each { h.default ||= 0; h[:a].frobnicate }; h[:a].frobnicate` — the
reference evaluates the compound attr write in the block's *own* scope and
drops `h`'s indexed narrowing for in-body reads; outer records survive. The
port kept the narrowing in-body → `call.undefined-method` FP.

- `Node::AttrWrite` gains `closure_evaluated`: `evaluated` minus the
  `closure_depth` clause (still gated on dead_operand, recovery_suppressed,
  operand-effects) — matching the reference's evaluate-vs-type positions.
- `flow_writes.rs::closure_mutations()`: `(owning body, span, receiver)` for
  shape-mutator attr writes, keyed by innermost deferred body; `def`/`class`/
  `module` bodies key to `None`.
- `flow_eval.rs`: `closure_descend` replays mutations at Barrier boundaries in
  evaluation order; `same_cond_path` declines sibling if/case/rescue/recovery
  arms (`ensure` sees the joined scope); `apply_closure_mutations` drops the
  record.

## Measured

Issue row: silent in-block @3:35, fires post-block (master fired both).
21 suppression rows + 10 must-still-fire controls pass; sweep 0 FP.

## Residuals (all filed)

- **#379** — writes in earlier sibling statements of a *nested* deferred body
  don't drop (owner re-keys one level late); pre-existing, partially closed.
- **#380** — closure mutations under non-descending containers
  (`while`/`case`/`rescue` arms) never replay; also `class_narrowing.rs` still
  gates on `evaluated` only (in-block writes don't kill `Narrowed` facts).
- **#381** — `closure_mutations` omits `in_inert_carrier` retain → dead/inert
  attr writes inside blocks (`END{}`, `if false`, `when` conditions) drop
  records the reference keeps. Safe-side coverage loss.
- Pre-existing, unchanged: multi-write attr targets, `h.default = 0` writeback
  leaking to outer env, `@h` ivar records (#353), `||=` in-block record
  creation, block-param `|h|` coverage, in-block `+= 1` writeback.
