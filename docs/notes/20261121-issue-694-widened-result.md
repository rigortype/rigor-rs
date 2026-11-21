# #694 — WidenedResult nominal answers (investigation)

Issue: the port's fold returns `Option<Scalar>`; the reference's fold tier has a
third answer, `WidenedResult` (`constant_folding.rb`), which `build_constant_type`
turns into `Nominal[class_name]`. Where the port declines it reaches `Dynamic`/
`untyped` (coverage loss); the reference reaches `Nominal[Integer]`/`[Rational]`
and `call.undefined-method` fires.

## Probe matrix (master `2104267e`, release, `harness/probe.py`)

| row | reference | port |
|---|---|---|
| `x = 2 ** 20000; x.zz` | `for Integer` | silent |
| `x = 2 ** 4294967296; x.zz` | `for Integer` | silent |
| `x = 10 ** 4096; x.zz` | `for Integer` | silent |
| `x = 1; x <<= 20000; x.zz` | `for Integer` | silent |
| `x = 2; x **= 20000; x.zz` | `for Integer` | silent |
| `x = c ? 1 : 2; x <<= 13605; x.zz` | `for <literal> \| Integer` | silent |
| `x = <5001-digit>; y = x.succ; y.zz` | `for Integer` | `for Integer` (already — `big_scalar` decline lands on RBS `succ -> Integer`) |
| `x = 2 ** -20000; x.zz` | `for Rational` | silent |
| `x = 2.pow(20000); x.zz` | `for Integer` | silent |
| `x = 2.pow(5); x.zz` | `for 32` | silent |
| `x = <20-digit> ** <20-digit>; x.zz` | `for Integer` | silent |
| `x = -1; x **= 20001; x.zz` | `for -1` | silent |
| `x = 1; x += 3; x.zz` | `for 4` | silent |
| `x = 1; x -= 9; x.zz` | `for -8` | silent |
| `x = nil; x \|\|= 5; x.zz` | `for 5` | silent |
| `x = 1; x \|\|= 5; x.zz` | `for 1 \| 5` | silent |
| `x = 1; x &&= 5; x.zz` | `for 5` | silent |
| `x = nil; x &&= 5; x.zz` | silent | silent |
| `x = 1; if c; x = 4; end; x.zz` | `for 1 \| 4` | silent |
| `x = c ? 1 : 4; x.zz` | `for 1 \| 4` | silent (single-class literal union — `single_class_literal_union` unported) |
| `x = 1; x += 3; if x; y = 1; end` | always-truthy | silent |
| `x = nil; x \|\|= 5; if x; y = 1; end` | always-truthy | silent |
| `x = 1; x <<= 20000; if x; y = 1; end` | silent | silent |
| `x = 1; x += "s"; x.zz` | `for Integer` | silent (fold decline → RBS-tier fallback — residual, out of slice) |
| `x = 2.0 ** 20000; x.zz` | `for Infinity` | silent (Float `**` unmodeled — residual) |
| `x = 2 ** -3; x.zz` | silent | silent (Rational fold unmodeled on BOTH) |

## Required surface (all oracle-verified above)

1. `folding.rs`: `FoldOutcome::Value/Widened(&'static str)/Declined` +
   `fold_outcome()`; `fold()` stays the value-only wrapper. Widened sites:
   `fold_int` `**` (precheck + after-compute `decimal_oversized` + negative
   exponent → `Rational` + `b >= 2**32` non-unit base), `shift_result`,
   `bigint_shift`, `big_scalar` (after-fact `oversized_number?` mirror).
   `pow` on an Int receiver shares the `**` arm (reference folds it:
   `2.pow(5)` → `for 32`). BigInt-receiver `**`/`pow`: widened only under a
   conservative lower bound on `power_base_bits` (`floor((digits-1)·log2 10)`,
   never mints a nominal the reference's exact check wouldn't).
2. `call_dispatch.rs` tier-1 fold arm: `Widened(cls)` → `nominal_or_untyped`.
3. `flow_eval.rs` `op_write_result` Operator arm: memberwise `fold_outcome`;
   a `Widened` member joins `Nominal[cls]` (reference `build_constant_type`
   union of constants + nominals).
4. `flow_eval.rs` `bind_check_statement_effects`: `LocalVariableOpWrite` arm —
   bind `op_write_result` (today: unconditional widen via `other =>`). Or/And
   included (`x ||= 5` → `5`/`1 | 5`, `x &&= 5` → `5`; all oracle rows).
5. `flow_eval.rs` `flow_eval_stmt` `LocalVariableOpWrite` arm (line ~5033):
   same bind (always-truthy env; `x += 3` / `x ||= 5` fire `always-truthy`).
6. `call_receiver.rs` `check_call`: port `single_class_literal_union`
   (upstream #1699) — `Union[Constant|plain Nominal]*` with ONE distinct
   member class collapses to `Nominal[class]` for the check while the message
   renders the union (`for 1 | 4`, `for <literal> | Integer`).

## Residuals (deliberately out of slice)

- Op-write RBS-tier fallback when the fold declines (`x += "s"` → `Integer`).
- Float `**`/`pow` fold (`2.0 ** 20000` → `Infinity` constant).
- Rational/Complex fold results (`2 ** -3` — silent on both today).
- `pow` on non-Integer receivers; `x op= v` with non-Constant entry/operand
  (`n.to_i; x += 3` → `Integer` nominal on the reference — needs call-tier
  typing on the entry binding).
- `(±1) ** negative` Integer results the reference folds (`(-1) ** -3` → `-1`).
