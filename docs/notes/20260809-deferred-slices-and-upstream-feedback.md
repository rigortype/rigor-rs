# Category-2 slice adjudication + upstream feedback batch 3 (2026-08-09)

Four parallel investigations (fresh probes, pin `v0.3.1` at measurement time;
verdicts unaffected by the same-day `v0.3.2` re-pin — none of the 13 new
upstream commits touch narrowing or `Logical` typing) settled the remaining
category-2 implementation candidates and shipped the upstream feedback batch.

## Verdicts (standing; re-litigate only with new census evidence)

- **3b-2 (`while`/`until` body descent): DEFER at 0 verified rows.** A
  nesting-aware scan of all 813 undefined-method + possible-nil rows found
  exactly ONE guard-in-loop-body shape, and it is disqualified by an
  orthogonal blocker (`Numeric#nan?` is not declared on `Numeric` in either
  engine's RBS — the same row the 08-08 remeasure reduced). The additive
  `Loop.is_for` arena discriminator stays cheap (~15-25 lines, all consumers
  match with `..`) if a future census surfaces a candidate.
- **3a-4 (`when`/`||` unions): DEFER, bound now measured 0** (was ≤1). No row
  in the current gap set renders a union message or needs a static-class
  multi-condition `when`.
- **Logical carrier fidelity (`a || b` typed `Dynamic[top]` vs the
  reference's union): the REAL fix is a dedicated arc, not a slice.** New
  facts from the probe session: the reference's `Combinator.union` does NOT
  absorb `Dynamic` (rigor-rs's `Algebra::join` does — the naive fix is a
  no-op for the archetype); the two most exposed rules (undefined-method,
  the narrowing gate) are ALREADY Union-safe by construction on both
  engines; directly-attributable prize is ~1 row (the fp1/fp2 d4-d7
  decline) — the auth_hash/normalizer rows are ALLOW-list mechanism costs,
  not Logical costs. Option (b) (allow-list `Logical` whose operands are
  narrowable) is REFUTED: `narrow_class_other` declines on the union's own
  type regardless of operands. Cheap follow-up if ever wanted: re-enable
  d4-d7 gated on non-`Logical` values (~1 row).
- **Chain-guard meet: BUILT** (PR #86 — 2 live FPs, +4 matched rows).
- **Sequential-guard join-wipe (re-seed granularity): GO, in flight.** The
  blanket `join_cenv` wipe loses an earlier statement's fact at ANY
  intervening `if`/`unless`/`case` (terminating or not, same or different
  local, blocks and `case` included — 10/14 synthetic probes diverge from
  the reference, all in the coverage direction). Census prize: exactly 1
  probe-confirmed row (gitlab-foss `bulk_imports/object_counter.rb:52`),
  but the fix is reference-`Scope#join` fidelity with a bounded design
  (restore untouched pre-join facts, reusing the `rewritten` filter).

## Upstream feedback batch 3 (filed 2026-08-09)

Verified same-day on both `v0.3.1` and upstream master `2c38c76b`, then filed:

| upstream issue | item |
|---|---|
| rigor#316 | toplevel `def output` captures RSpec's matcher cross-file (16 live rows) |
| rigor#317 | `Array.new(n) { block }` elements typed `nil` from the no-block overload |
| rigor#318 | `defined?` operand analyzed as evaluated code |
| rigor#319 | `Class.new do…end` block body scoped at top level (16 live rows) |
| rigor#320 | `class << Const = Object.new` singleton body dropped (haml pattern) |
| rigor#321-#323 | spec-pinning asks: suppression self-ack polarity, `raise` singleton/instance asymmetry, duplicate-hash-key Float label |

Loop already closing: batch-2's three headline defects were found ALREADY
fixed upstream (#293→#298, #294→#297, #295→#296) — #297 is what retracted
295 possible-nil gaps at the `v0.3.2` re-pin. Batch-1 items 1-2 resolved
themselves upstream between the RC and `v0.3.1` (no report needed). Not
filed: rdoc generated-parser cluster (no cheap minimal repro — needs a slab
of the 16k-line kpeg file), `pre_eval` cluster (by-design per upstream
ADR-17).
