# `if`-edge narrowing — join per-edge narrowed types for predicate subjects

Issue #451 → PR #571 (merged). `h = [1, 2]; if h.empty?; end; h.frob` fired
`for Array` on the port; the oracle deads the subject inside the
predicate-covering arm and re-joins the carrier on entry type.

## Edge algebra (all oracle-probed)

`loop_edges` gained a real `Empty` (decided) state alongside `Keep`/`Other`,
with `T ∪ Bot = T` join semantics: `Empty` is the join identity, `Other`
dominates, `Keep ∪ Keep = Keep`, `Keep ∪ Empty = Keep`, `Empty ∪ Empty =
Empty`.

- `subject_edge` atoms: `empty?` (truthy Keep / falsey Other on known
  carriers); `nil?`/`== nil`/`!= nil` via `nilness`; `==`/`!=` restricted to
  a `nil` LITERAL operand on either side — `v == 1` does NOT narrow on the
  oracle (declines to `unknown`).
- `class_edge`: `(Subclass|Equal, false)` splits on the carrier — `Empty`
  for literals (Tuple/HashShape/Constant — the arm is decided), `Other`
  for nominals (the oracle mints `non-empty-array[top]`, which the port
  cannot express — decline).
- `unknown` consults `expr_truthiness`: a folded predicate deads the
  contrary arm (`h = 5; if h.negative?; h.bad` is silent on the oracle),
  otherwise `Keep` both edges (an unmodeled predicate keeps the carrier —
  `if h.frozen?` still fires `for Array` inside).
- `nil&.m` evaluates to `nil` — `empty?`/`unknown` under `safe_nav` on a
  provably-nil carrier read `Empty` on the truthy edge.

## Terminating arms

`class_flow_if` computes per-arm `branch_terminates` and masks the
terminated edge to `Empty` before the subject join (`return if
h.is_a?(Array)` on `[1, 2]` → `Empty ∪ Empty` → `Bot`, oracle-silent). The
subject loop runs AFTER `join_cenv`/`propagate_widened` — earlier it ran
before and `join_cenv` wiped the joined facts.

`stmt_terminates` now accepts the same exit set `dead.rs`'s `EXIT_CALLS`
and `recovery.rs` hold: `raise`/`throw`/`exit`/`abort`/`fail`. Each
propagates the surviving guard map (oracle fires `for String` past
`throw :done unless v.is_a?(String)`); `redo`/`retry`/`exit!`/`catch`
stay unmodelled.

## Accepted declines

`non-empty-array[top]` rows (the oracle narrows `Array` falsey to a real
firing type the port cannot mint); `||`/`&&` guard-map declines;
`x&.nil?` truthy-arm on a nil carrier (oracle keeps the call live, the
port's `&.`-on-nil `Empty` deads it).

Measured: all probe pairs identical to the oracle on both engines;
fp_audit 0 FP; the four review rounds (each verified against the oracle)
landed as fixup commits `1a65a5a`..`3a51a1d`.
