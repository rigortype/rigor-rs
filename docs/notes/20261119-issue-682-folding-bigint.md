# #682: folding `<<` wraps at `i64` — promote integer overflow to `Scalar::BigInt`

PR #693 → merged 2026-11-19 (`c34e449e`). Closes #682. Approved (Opus 5.5
adversarial on `73cf391d` + fix-confirmation on `898bc7d6`; Fable 5.2
design companion). Follow-ups #694, #695, #696.

## What diverged

`fold_int`'s `<<` used `i64::checked_shl`, which guards only the shift
**count**: `2 << 63` wrapped to `0` and `2 << 62` to `i64::MIN`. Around
it the audit found the whole family: every `+`/`-`/`*`/`abs`/`succ`/
`pred`/`-@` overflow declined where the reference folds a Bignum
`Constant`; `**` had a port-only `b <= 4096` cap; every fold on a
`Scalar::BigInt` receiver declined; and `i64::MIN % -1` **panicked**
inside `ruby_mod` (`sig-gen` exit 101, `check` →
`internal error: unknown panic` — the first port-side panic the survey
has surfaced).

## Reference semantics worth keeping in one place

- **`INTEGER_FOLD_BIT_LIMIT = 13606`** on the *folded* result's
  `bit_length` (`oversized_number?`); past it the reference emits
  `WidenedResult.new("Integer")` → `Nominal[Integer]`, never declines.
  `x = 1 << 13605` → `for <4096-digit literal>`; `x = 1 << 13606` →
  `for Integer`. Ruby `bit_length(-v) == bit_length(|v| - 1)`.
- `shift_blow_up?` estimates `bit_length(v) + n.abs` (no compute);
  `power_blow_up?` uses a lower-bound `floor(log2|a|) * b`.
- Shifts by negative counts flip direction; `>>` **floors** negative
  receivers (`-8 >> 2 == -2`); huge shifts saturate at `0` / `-1`;
  `a << -huge` saturates, `a << +huge` widens.
- `Integer#/` floors (`7 / -2 == -4` — `div_euclid` is wrong, `-3`).
- `ruby_mod`/`ruby_rem`: `%` takes the divisor's sign; `rem` the
  dividend's. `i64::MIN % -1 == 0` (wraps → use `wrapping_rem`).

## Port shape

One file (`folding.rs`); `fold` dispatches `Scalar::BigInt` receivers
to `fold_bigint` (predicates, sign ops, `to_s`, `succ`/`pred` via
`decimal_step`, integer comparisons, `<<`/`>>` via `bigint_shift`).
`Int` overflow promotes through `int_scalar` (i128) or decimal-spelling
paths (`decimal_pow`, `decimal_shl`, base-1e9 chunk ops). **Every
integer-producing BigInt arm exits through `big_scalar`**: demote to
`Int` when in range (canonical form — a `BigInt("-9223372036854775808")`
spelling breaks later folds) and decline past the bit limit. `u128` in
`decimal_pow`'s inner multiply; `u32::try_from` on `**` exponents.

## Measured

All issue rows byte-identical on `check` tuples AND sig-gen output
(incl. the union `(4611686018427387904 | 9223372036854775808)`); ~60
edge controls byte-identical; sweep **0 FP**, gaps 198/85/55/16/6
unchanged. Review found five fixes (u32-truncated `**` exponents,
missing result gate on BigInt receivers, `decimal_pow` u64 wrap, `/`
floor, non-canonical `-2^63` spelling) — all landed and re-probed.

## Residuals (filed)

- **#694** — `fold` has no `Widened` answer: oversized `**`, op-writes
  (`x <<= 20000`), and union members past the limit decline to
  `Dynamic` where the reference widens to `Integer` (and joins it into
  unions: `x = c ? 1 : 2; x <<= 13605` → `<literal> | Integer`).
- **#695** — BigInt-receiver `+`/`-`/`*`/`/`/`%`/`**`/bit ops decline;
  proposes a limb-type unification (needs an ADR: in-house vs
  `num-bigint`).
- **#696** — `Integer#to_s` on a widened receiver: ref
  `decimal-int-string` vs port `String` (pre-existing on master).
- `2 ** -3` → `Rational`; cross-kind `==` (`1 == "a"`); top-level
  `x += 3` re-pin — all pre-existing / unmodelled.
