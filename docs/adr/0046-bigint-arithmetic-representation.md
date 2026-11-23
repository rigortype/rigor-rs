# BigInt arithmetic: reuse the base-1e9 chunk machinery

Status: accepted 2026-11-23 (issue
[#695](https://github.com/rigortype/rigor-rs/issues/695), PR #873).

## Context

`Scalar::BigInt` receivers folded only members whose answer reads straight
off the decimal spelling; `+`, `-`, `*`, `/`, `%`, `&`, `|`, `^` declined
where the reference's `public_send` on real Ruby bignums folds the literal
(`x = 99999999999999999999 + 1; x.zz` was silent — a coverage loss). The
issue's design companion proposed unifying the integer representation on a
single sign + little-endian-u32-limb type — "makes `bit_length` exact,
deletes `decimal_cmp`/`decimal_oversized`/`POW2_BIT_LIMIT`, and gives
`+`/`-`/`*`/shifts for free" — with `num-bigint` as the alternative and the
choice explicitly delegated to an ADR.

## Decision

Add `+`, `-`, `*` over the representation already in `folding.rs`: sign +
little-endian base-1e9 chunks (`decimal_chunks`/`chunks_mul`/
`chunks_sub`/`chunks_to_decimal`), plus `chunks_add` and a signed combine.
`/`, `%` and the bit ops stay declined (the issue itself permits deferring
them).

- `*` is schoolbook O(w_a·w_b), so a bit-length lower bound
  (`v >= 1e9**(len-1)`) answers `FoldOutcome::Widened("Integer")` up front
  when it already clears `INTEGER_FOLD_BIT_LIMIT` — the reference's
  `WidenedResult` without spelling the product. `+`/`-` are O(width) and
  exit through `big_scalar`'s `oversized_number?` gate like every spelled
  result.
- A Float operand coerces through `rb_big2dbl`/`(double)v` — both Rust's
  `str::parse::<f64>` and `i64 as f64` are the same IEEE rounding Ruby's C
  casts perform. A NaN (`inf * -0.0`) declines, matching
  `foldable_constant_value?`.

## Alternatives

- **u32-limb unification** (the companion proposal) deletes the most code
  eventually, but rewrites the proven `pow`/`powmod`/`<<` chunk paths in
  the same slice for no additional coverage — the limbs buy `&`/`|`/`^`/
  `[]`/`bit_length`, which this slice defers anyway.
- **`num-bigint`** brings tested division/pow but adds the
  arbitrary-precision dependency the crate has avoided for `Scalar::BigInt`
  (#194), to implement ~120 lines of schoolbook arithmetic.

## Consequences

`big + 1`, `big - 3`, `big * 2`, `1 + big`, `5 - big`, `3 * big`,
`big + big`, `big - big`, `big + 1.5`, `1 + 1.5` all fold tuple-identically
to the reference (probe-verified); `big / 3`, `big % 3`, `big & 3` keep
declining (coverage loss — the RBS `Integer` or silence, never a wrong
diagnostic). If `/`, `%` or the bit ops become load-bearing, the u32-limb
unification is still the right next step: `chunks_mod`'s Knuth-D loop
already computes the quotient digits it discards.
