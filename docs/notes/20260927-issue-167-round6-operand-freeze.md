# Issue-167 round 6 — operand-threaded interiors + rescue nilability

Round 6 of PR #183 (rigor-rs#167): the round-5 join work left a family of
false positives because the port recorded a scope-index entry at every
statement position, while the reference evaluates operand interiors through
`thread_operand` (`statement_evaluator.rb`) on an evaluator whose `on_enter`
is nil. The fix adds a freeze, not a new join.

## The model (measured from the reference, pin `e59b7b89`)

- `bind_operand` pushes a node id onto `operand_threaded` for the duration of
  a bind; every statement-position `arm_entries` push now checks
  `operand_frozen()`. Operand CHILDREN (a parenthesized sequence's
  statements, a call's receiver/arguments/`&expr` block-pass, container
  elements, interpolation parts) still record — they are the reference's
  `walk.later` records, kept iff the entry scope moved off the operand
  anchor (`OperandWalk`'s taken test → the `*env != anchor` check).
- Consequence rows: a real `begin` operand types its interior reads from the
  ENTRY env (`x = (begin; w = "s"; w.frob; end rescue 2)` reads `w` at
  entry), a parenthesized sequence keeps ordered threading, and a
  `case`/`begin` value tail inside a rescue modifier resolves at entry
  (`(begin; w = "s"; w; end rescue 2)` binds `1 | 2`, not `"s" | 2`).
- `type_of_case` (expression_typer.rb:1028) inside a rescue modifier applies
  `===`-certainty (`rescue_case_value` + `case_pattern_certainty`): `:yes`
  ends the scan, `:no` drops the branch, else/no-match adds `Constant[nil]`
  — `(case 2; when 1; w = "s"; w; end rescue 3)` binds `3?`.
- `type_of_loop` types `Constant[nil]` (expression_typer.rb:1180); `break
  VALUE` is unmodeled there too — `(while false; end rescue 2)` binds `2?`.
- A `&.` call's arguments run only on the non-nil receiver edge:
  `eval_call` joins them into the post-RECEIVER scope nil-injected, so
  `x&.foo(w = 1)` binds `entry | 1`. The nil-flow pass (`nil_flow_expr`)
  does the same for facts: `&.` suppresses its own diagnostic but does not
  erase the local's `C | nil` fact for later sites.
- `case … in` guard isolation: the `in` clause's carrier first body entry is
  the pattern (a guard folds inside it); `eval_when_or_in` `sub_eval`s
  `node.statements` only, so guard writes bind nowhere — the clause body and
  post-case scope skip the pattern node entirely.
- `nil_flow_rescue_join`: a name unbound at carrier entry that exactly ONE
  side of `expr rescue arm` writes joins as `C | nil` (all non-nil members
  of the writing side must name the same class — `w = gets` keeps
  `"String"`). Facts survive non-writing guards (`if sub; noop; end` keeps
  `sub` nilable — the earlier decline was a coverage gap), drop on
  rebinding constructs, and drop when a guard's surviving edge narrows the
  read (`return if x.nil?`; `w && return` / `w || return` drop the LHS-read
  fact too — exiting RHS ⇒ opposite-edge narrowing).

## Measured outcome

- Gate: `harness/gate.sh` PASS (docs_check, 5 crate test runs, snapshot
  0 unregistered FP); `cargo +1.88.0 clippy --workspace --all-targets -- -D
  warnings` clean; `fp_audit --gaps --sweep` 0 FP over 8 corpora / 9,337
  files, matched == port total everywhere (no retractions).
- Fixture 119 rows (77)–(86) added; snapshot 136→150 reference diagnostics;
  row comments record the two remaining gaps.
- Known declines (safe side, documented): `w && return` — the reference
  narrows `w` to `nil` on the surviving edge and fires `undefined-method
  for nil`; the port drops the stale `C | nil` fact and stays silent.
- Deferred: `case [1]; in [w]; w.frob; end` — pattern-binding deconstruction
  unported (reference fires `for 1`); filed rigor-rs#200. Row (85)'s
  `flow.always-truthy-condition` on the guard is also a standing gap.
- Pre-existing drift (unchanged this round): diagnostic ORDERING on a line
  with both `unresolved-toplevel` and `undefined-method` (port emits the
  call-site error first; harness keys on (rule,line,col) so both match).
