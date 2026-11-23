# #877 — BigInt receiver residual folds (PR #890)

The rest of the reference's `Integer` constant-fold catalogue on a Bignum
receiver, shared with the `i64` receiver over ADR-0046's sign + base-1e9
chunk layer. ~70 brief rows plus ~200 adversarial review probes, all
byte-identical to the oracle (`harness/probe.py`, fresh dirs, full tuple +
exit code). `fp_audit --gaps --sweep`: **0 FP** over 8/8 standing corpora
on the final head.

## What landed

- **`/` `div` `ceildiv` `remainder` `divmod`** — `chunks_mod` split into
  `chunks_divmod` (Knuth-D quotient; it already computed one and threw it
  away). Sign rules: floor for `/`/`div`, divisor-sign for `%`/`modulo`,
  `+inf` for `ceildiv`, dividend-sign for `remainder`. A `0` divisor
  declines to the RBS tier (fires `flow.always-raises` for the five
  `INTEGER_RAISING_OPERATORS`; `ceildiv`/`remainder` are not in it).
- **Bit ops `&` `|` `^` `~` `[]` `bit_length`** — u32-limb view of the
  base-1e9 chunks (`chunks_to_limbs`/`limbs_to_chunks`; `-v = ~(v-1)` for
  negative magnitudes). Float operand on `&`/`|`/`^` raises → decline.
- **`<=>`** — `chunks_cmp` for int/int; `int_cmp_f64` spells the f64 as
  `m × 2^e` for an EXACT int/float compare (`9007199254740993 <=>
  9007199254740992.0` → `1`); non-numeric → `Constant[nil]`, joining
  `is_nilable_fold`/`declines_unfolded`.
- **Decimal rounders** — `ndigits` is a C `int`: `5.round(2147483648)`
  raises `RangeError` → declines to `for Integer`; a Float `ndigits`
  truncates via `to_i` (`big.round(-5.0)` folds); the rounding side's
  `10**k` widening caps at `k = 4096` (`INTEGER_FOLD_BIT_LIMIT` proxy).
- **Bit-test predicates** — a Float mask coerces via `to_int`:
  `f64_to_int_limbs` spells the f64's exact integer (`nobits?(1e300)` →
  `true` — its low bits are zeros), `-0` normalizes to `+0`
  (`5.allbits?(-0)` → `true`). `digits`' Float base does NOT coerce —
  see "comment caveat" below.
- **`FoldOutcome::Tuple`** — `divmod`/`gcdlcm`/`digits` return
  `Tuple([Constant, …])`; `call_dispatch` maps to `Type::Tuple` so
  `a, b = big.divmod(3)` pins `b` → `for 0`. `digits` caps at 32 elements
  (`STRING_ARRAY_LIFT_LIMIT`); `0.digits(base)` pins `[0]`.
- **`FoldOutcome::Unrepresentable`** — `to_r`/`to_c`/`rationalize`/`quo`
  pin a Rational/Complex constant `Scalar` cannot hold → untyped. The
  oracle's pinned carrier is silent for undefined methods — the port's
  old RBS `for Rational`/`for Complex` nominal was a real FP.
- **Unary family on BigInt** — `to_i`/`to_int`/0-arg rounders/`nonzero?`
  re-gate through `big_scalar`'s oversized check; `next`/`magnitude`/
  `finite?`/`infinite?`/`inspect`/`abs2`/`to_f`/`~`/`bit_length`.
- **`AlwaysRaisesShield` port** (upstream #3036, never ported) —
  `begin; x = 5/0; rescue; end` was a port FP for `i64` too.
  `always_raises_rescued`/`rescue_chain_catches`/`class_catches` in
  `crates/rigor-rules/src/flow.rs`; parent map via
  `Typer::ancestor_map` (`flow_eval.rs`) over `flow_children` edges +
  `IndexWrite` receiver-`None` edges. Boundaries: `Definition`,
  `Lambda`, literal-block `Call`. `class`/`module`/`Inert` carriers are
  crossed. `rescue` classes resolve via `type_of` → `Type::Singleton` →
  `index.class_name_for_id` → `typer.source().class_name_for_id`
  (two-step, mirroring `resolve_class_name`), then `class_ordering` ∈
  {Equal, Subclass} catches. 45 probed rescue shapes byte-exact.

## Residuals (disclosed in the PR; all pre-existing or safe-side)

- Multi-overload RBS-return gap: `quo(0)`/`remainder(0)`/`fdiv(0)`/
  `divmod(0)`/oversized-`divmod`/`5 / 0`-in-rescue → ref answers the
  declared type, port silent → **#894**.
- `RbsReturnShape::Class` drops type args: `big.digits(-3..3)` declines →
  ref `Array[Integer]` vs port `Array` (same rule/col; port-wide).
- Rational/Complex unary folds on the pinned carrier
  (`5.to_r.numerator` → ref `for 5`) → **#895**.
- `begin; END { x = 5/0 }; rescue; end` → ref fires always-raises, port
  silent (Inert carrier; `BEGIN` inside a `begin` is a parse error).
- `def m; begin; x = 5/0; rescue; end; end` → ref `dead-assignment`,
  port silent — pre-existing, unmasked by the shield → **#896**.
- `1.fdiv(v)` under the #436 reaching-write cap → ref `for 1.0`, port
  `for Float` (the cap decline is the port's own approximation); the
  `issue_436` test re-probed onto `1.coerce(v)`.
- i64 unary carve-out → **#893**.

## Comment caveat (round-2 review, non-blocking)

`int_binary_float`'s comment says "`0.digits(3.0)` raises" — wrong
about live Ruby 4.0.5, which returns `[0]` (Ruby coerces the Float base;
only a negative radix raises). The port is still oracle-correct: the
reference's fold gates the base as `Integer` and declines either way, so
both engines decline to the RBS `Array[Integer]`/`Array` tier. Reword
when the hunk is next touched.

## Review + gates

Two review rounds on `opencode-go/glm-5.3` (recorded substitute):
round 1 Approved with 8 findings; the wrong-value fix (`0.digits` →
`[0]`), the Float-mask coercion and the `END` doc fix landed in
`3adc8f8a`; round 2 fix-confirmation Approved. CI 6/6 green on the head;
merged `d4ab9ffd`. Follow-ups filed: **#893 #894 #895 #896**.
