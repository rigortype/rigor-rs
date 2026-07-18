# Compat next stage — 4-phase plan (2026-07-18)

Raise Ruby-Rigor parity to the next stage from the post-bump baseline
(pin `7a69f142`, live 188/193 matched, 0 FP everywhere). Every phase is
measurement-gated; standing conclusions in CURRENT_WORK.md stay binding.

## Baseline (measured 2026-07-18)

- Fixture parity 188/193. The 4 residual gaps are IDENTIFIED:
  - `53_kernel_constant_folding.rb` ×3 — `format("%d","abc")` / `Integer("abc")`
    / splat `format`: the reference falls to its literal-string lift / RBS
    nominal after the value-fold declines; rigor-rs declines to Dynamic.
  - `65_regexp_last_match_nil.rb` ×1 — `Regexp.last_match(<non-literal>)`: the
    reference selects the `(Integer) -> String?` overload BY ARITY and fires
    possible-nil. Both union arms are nameable-concrete — NOT the Tier B/C
    `Dynamic|nil` shape, so it composes with the FP-safety mechanism.
- gitlab-foss lib gaps 364 = UM 179 / possible-nil 162 (Tier B/C, CLOSED) /
  always-truthy 16 / tails 7. mastodon models 5 (UM only). 0 FP.
- RC surfaces with NO rigor-rs code (grep 0 hits): `static.value-use.void`
  (ADR-100 void_origins), `rbs.environment-build-failed`, config unknown-key
  (ADR-99), rbs-inline reader (ADR-93/94), override generic instantiation.

## Phase 0 — measurement gates (first; ~half a day)

- **M1 reference self-diff**: run the reference at `47ec8625` (worktree) vs
  `7a69f142` over gitlab lib / mastodon / conference-app; diff the
  `(rule, path, line, col)` sets; attribute each new-only diagnostic to an RC
  mechanism (`(?)` return, regex narrowing, join fold, Data/Struct, void→top,
  …). A mechanism with 0 real-corpus firings is NOT ported (the
  five-slices-0-gaps lesson, applied before building instead of after).
- **M2 UM-residual characterization**: the standing INVESTIGATION item
  (~179 on gitlab lib). Sample ~30, classify by mechanism (AS-overlay /
  typer-substrate / other) → per-cluster go/no-go. No slice before this.

## Phase 1 — fixture parity 100% (small, certain; parallel to Phase 0)

- **S1** format/sprintf/Integer nominal fallback on fold-decline (post-guards)
  → closes 53×3.
- **S2** `Regexp.last_match(dynamic)` arity-selected `String | nil` → closes
  65×1.
- Honest framing: corpus payoff ≈ 0 (P2 already took what occurs); the goal is
  a 193/193 fixture face. Exit: 66 fixtures, 193/193, 0 FP.

## Phase 2 — RC inference cluster (ONLY mechanisms M1 proves fire)

Candidates: `(?)`-return retention (`0812d60c`), regex-match narrowing
(`91d6d528` + `5628c4ff`), `Array#join` all-literal fold (`d25932c8`),
`Data.define`/`Struct` re-typing (`f27b84b6`), void→top (`42597145`, also the
Phase-3 static.* prerequisite). SKIP the FP-reducing-side items (non-empty
invalidation `6e0441fb`, mutation-widening `e733e509`) while rigor-rs holds
0 FP — they fix over-firing rigor-rs does not do.

## Phase 3 — new rule surfaces (medium)

- Config unknown-key warning (ADR-99; same bundle as the §7 config-schema
  remainder already on Now/Next).
- `rbs.environment-build-failed` diagnostic.
- `static.value-use.void` + void_origins side-table (ADR-100); needs
  project-sig fixtures (37-series pattern); void→top lands first.
- **Out of scope**: rbs-inline reader (ADR-93/94) — a plugin-engine-class
  separate ADR track, not a slice.

## Not doing (standing)

Possible-nil 162 (Tier B/C — reference FPs); coverage slices without a
valid-mode `fp_audit --gaps` prediction; always-truthy 16 pends the ADR-0022
flow substrate — re-judge after M1/M2. Re-pin at the v0.3.0 tag when it lands.
