# #769 — fold pow(exp, mod), BigInt ** small results, Float ** (PR #836)

Merged `fc0ee52a`. `crates/rigor-infer/src/folding.rs` only (+528/−20).
Coverage-loss slice: every row was port-silent where the reference folded.

## What landed

- **`Integer#pow(exp, mod)`** — the reference's `invoke_ternary` path (a
  catalog `:leaf` with no `numeric_blow_up_class` precheck; the result is
  bounded by |mod| anyway). `int_pow_mod_result` computes the modular
  exponentiation: sign of the divisor (`2.pow(5, -3)` → `-1`), `RangeError`
  on a negative exponent, `ZeroDivisionError` on a 0 modulus, `TypeError`
  on non-Integer operands — all decline like `invoke_ternary`'s rescue.
  `u128` path for `i64` moduli; new base-1e9 chunk arithmetic
  (`chunks_mul`/`chunks_mod`, normalised Knuth-style division) for a BigInt
  modulus, work-bounded at `1<<28` word-ops (→#837). Exponents iterate
  `u64` bits or decimal-chunk halving (`ExpBits`), so `2.pow(10**40, 97)`
  folds.
- **BigInt `**`/`pow` small results** — `bigint_pow_result` now computes
  `(10**20) ** 2` → the 10^40 literal via `chunks_pow` (early abort at 457
  words ≈ 13.6k bits → boundary spelling the `big_scalar` gate widens).
  A negative exponent keeps `rational_oversized`'s denominator gate
  (`(10**20) ** -1` declines like the i64 rows — both silent).
- **`Float#**` and Integer `**`/`pow` × Float exponent** — `powf` in
  doubles (`2 ** 0.5` → `1.4142135623730951`); NaN — a Ruby Complex —
  declines. A BigInt exponent parses to `±inf`, matching `rb_big2dbl`
  (parity is lost at f64 magnitude: `(-2.0) ** (10**30+1)` is `Infinity`
  on the oracle, not `-Infinity`).

`Float#pow` is deliberately NOT folded: the oracle answers
`undefined method 'pow'` — the issue's `2.0.pow(5)` row was already at
parity. `FoldOutcome::Widened` minting stays confined to
`numeric_blow_up_class` + `fold_result_gate`.

## Measured

- All 5 brief rows plus ~40 extended edges probed tuple-for-tuple:
  `for 35`, `for 2`, `for Infinity`, `for -62`, `for 47`, `for 61`,
  `for -8870234378727297892477999`, `for 10000000000.0`, …
- Must-still-fire controls unchanged (13606/13607 boundary, `for Rational`,
  `(10**4000) ** 2` → `for Integer`, Complex/silent rows).
- Sweep `fp_audit --gaps --sweep`: **0 FP** on the standing set.
- Opus review (Approved, `0eac9927`): 300/300 random powmod rows
  oracle-identical including `10**9`/`10**18`/`10**27` chunk boundaries
  and 521/1279-bit moduli.

## Residuals (filed / known)

- Declined `pow` rows land on the flat RBS slot's overload collapse →
  silent where the ref says `for Integer` / `for Integer | Rational`
  (`2.pow(5,0)`, `2.pow(-1,97)`, `2.pow(0.5,3)`). Overload-selection work,
  not folding.
- Computed BigInt operands (`10**30 + 1`) need BigInt `+` — rigor-rs#695.
- `WORK_LIMIT` constant-factor cost (~4.5 s/fold at the bound) +
  infallible-cast fallback → **#837**.
- Design companion: recorded unavailable (Fable quota; Grok weekly limit;
  swe-2 substitute free-tier limited) — `docs/agents/review.md` fallback.
