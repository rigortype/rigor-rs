# Issue #352 — String `[]` reads after `[]=` stores stay non-firing

PR #370, merged `41e993a` (final head `dac33e2`). Three rounds: r1 found the
`Loop`/`BeginRescue` rebind clobber; r2 found pattern-binding rebinds bypassing
the gate; both fixed.

## What landed

- `widen_mutated_binding` gains `Type::Constant(Scalar::Str) → Nominal[String]`
  under `STRING_MUTATORS` (`StringMutation.widen_constant` parity); a
  `String#[]` read after an `[]=` mutation now reads the reference's
  non-firing type instead of raw `String`.
- `call_dispatch`: `T | nil` union receivers retry dispatch on the non-nil
  fragment when NilClass lacks the method (`try_non_nil_receiver_retry`);
  tier-3 mints `T | nil` for nilable RBS returns on Nominal receivers.
- `nilable.rs`: `s[k] ||= v` slot narrowings recorded on operand writes;
  masgn/for/rescue index targets apply `[]=` widening with `unwrap_or(pre)`;
  `name\x1f*` slot records drop on rebinds; `fragment_class` resolves union
  fragments.
- **Rebind gating** (`flow_writes.rs`): `local_rebinds` — toplevel rebinds
  plus def-internal writes — and `rebound_within` keep a rebind of the
  index-target local from being overwritten back by the pre-state widening.
- **`UnmodeledWrite` now carries pattern-bound local names**
  (`pattern_bound_locals` collects `LocalVariableTargetNode`s in `in`-clause
  patterns, `MatchWrite`/`MatchRequired`/`MatchPredicate` targets) so both the
  rebind gate and the widen census see `in [s]`/`=> s` bindings;
  `in_outer_inert_carrier` keeps named markers on their own `in` carrier only.

## Measured

~109-row probe matrix; all r1+r2 blocking rows verified silent-and-matching
(7 rebind forms + 11 pattern-bind forms); controls fire (`for s[0] in [5]`,
`s[1] = 9`, `in [w]`, `s[0] += "x"`, `K = 1`, `@i = 1`). Sweep 0 FP.

## Residuals

- **#374** — the decline cluster: conditional/cross-clause/iterable-position
  rebinds, ternary-source nilability, `ENV["K"]`, sig-lane `String?`,
  ordering + `for "a"` message drift, `nil?` fold gap.
