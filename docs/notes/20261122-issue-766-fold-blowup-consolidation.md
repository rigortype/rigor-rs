# #766 — FoldOutcome minting consolidated (PR #823)

Cosmetic consolidation from the PR #763 design review (Q3), merged
`3c984bb5`. `crates/rigor-infer/src/folding.rs` only (+248/−180).

## What landed

The reference structures the bit-limit gate in exactly two places
(`constant_folding.rb`); the port now mirrors it:

- **`numeric_blow_up_class`** (:1910) — one operand precheck consulted at
  the head of the `**`/`pow`/`<<`/`>>` folds of both `fold_int` and
  `fold_bigint`, like `invoke_binary` (:1662). `power_blow_up_class` (:1921)
  decides Integer-vs-Rational by exponent sign in one place;
  `shift_blow_up` mirrors `shift_blow_up?` (:1952) — exact on `i64`
  receivers, `bigint_base_bits_lb` lower bound on `BigInt` ones (trips only
  where the exact bound does). Replaced `int_power_blow_up`,
  `bigint_pow_outcome`, `shift_result`'s inline check and the `b >= 2**32`
  arm.
- **`fold_result_gate`** — the `fold_result`/`oversized_number?` (:1695)
  after-fact mirror; `big_scalar` delegates to it and the positive-`**`
  post-compute routes through it (was hand-inlined).
- **`rational_oversized`** — the negative-exponent denominator check,
  named after `oversized_number?`'s Rational branch (:1859).
- `decimal_pow` returns the `2**13606` boundary spelling at its
  (unreachable) paranoia bound, so every path exits through the gate.

`FoldOutcome::Widened` is minted inside exactly two functions:
`numeric_blow_up_class` and `fold_result_gate`.

## The one behavioural diff — a parity gain, not a regression

`x = (-4) ** -6803; x.w` is `for Rational` on the oracle and silent on
master. `Rational`'s denominator is always positive (Ruby normalises the
sign into the numerator), so the denominator must gate on the *magnitude*;
master checked the signed spelling `-2^13606`, where `decimal_oversized`'s
negative-magnitude adjustment (`bit_length` counts `|v| - 1`) fell one
step short. `rational_oversized` trims the sign.

## Measured

- 20-row `harness/probe.py` table: all match except `0 ** -5` (reference
  `for Numeric`, port silent on master too — declined-call typing gap,
  outside the fold).
- Implementer branch-vs-master differential: 932 generated cases over the
  whole touched surface → exactly one diff, the gain row.
- Reviewer (Opus 5.5) re-probe: 174 rows + an independent 188-row master
  differential → same conclusion, Approved.
- Gates: `gate.sh` pass (423 rigor-infer tests), `fp_audit --gaps --sweep`
  0 FP / gap totals identical to master, CI green on `e1786163` (head =
  approved `7f4ed89e` + merge of `a63e4715`, diff byte-identical).
- Timing: 2,100-line fold file 125.1 s vs master's 125.6 s — no
  regression; the precheck allocates nothing.

## Filed from review (pre-existing on master)

- **#826 (real FP)** — a widened fold result as an `if` condition
  (`2 ** 13607`, `1 << 13607`, `2 ** -20000`) makes the port fire
  `call.undefined-method` inside the dead `else` arm where the reference
  is silent.
- **#827 (perf)** — bit-limit fold paths ~60 ms/line; a 2,100-line
  fold-heavy file takes ~125 s on both binaries.
- Coverage residuals disclosed in the PR: `0 ** -n` (`for Numeric`),
  `(±1) ** -n` (unit Rational folds), `9e4095 ** -1` (exact bit count),
  two-arg `pow`, non-Integer `**` args, `1 << 2.0` message text.
