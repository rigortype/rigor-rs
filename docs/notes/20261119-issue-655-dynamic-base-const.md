# #655: skip static typing on a dynamic-base ConstantRead

PR #668 merged 2026-11-19 (`658f154`). Closes #655 and #661. Approved
(Opus 5.5 on `279c2956`). Follow-up #670 (class-guard operand).

## What diverged

The reference's `type_of_constant_path` falls back to Dynamic when
`Source::ConstantPath.qualified_name_or_nil` is nil — a dynamic base
anywhere in the chain (`self::X`, `k::X`, `self.class::X`,
`(Object)::X`). The port lowered those paths to `ConstantRead { name:
trailing, dynamic_base: true }` and ran C5 (literal fold), C1
(singleton), `.new` minting, the singleton-literal fold and the ENV
object-constant arm against the trailing name. `self::X.bogus` with
`X = 1` fired `call.undefined-method … for 1`; the oracle was silent.

## Port shape

Gate those four sites on `dynamic_base` (the lowering of
`qualified_name_or_nil` answering nil). `self_anchored` is a subset, so
the one flag covers the whole family including a parenthesised base
(#661).

A decline that stops a fold can revive a dead arm: `if self::X` with
`X = nil` no longer reports a spurious `always-truthy-condition`, and
`if self::X.nil?` with `X = 1` now fires `1.bogus` matching the
reference.

## Measured

Brief rows + #661 + must-still-fire (`X.bogus`, `C::X.bogus`,
`Integer.bogus`, `C.one.bogus`, `Array.new.bogus`, `Time.current`)
byte-identical to the reference. Sweep **0 FP**, gaps 360. Reviewer
re-probed project `sig/`, `&.`, multi-byte, >i64, containers, blocks.

## Residuals (unchanged, pre-existing)

- C5 ignores `rooted`: `class C; X = 1; ::X.bogus` still FPs;
  `module M; class Object; end; ::Object.new.definitely_missing` still
  a coverage loss.
- Class-guard operand still uses the trailing name (`x.is_a?(k::String)`
  fires) — #670. Fable: later, `ConstRef::static_name()` so a lenient
  `&str` is not the default.
