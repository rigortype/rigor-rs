# Tier B/C nilable-return / ScopeIndexer track — CLOSED (not deferred)

2026-07-17 go/no-go investigation (Sonnet; docs read + reference source +
1-in-8 gitlab-foss lib sample + 16 sites adjudicated by reading the code).
**Verdict: NO-GO. Close permanently; do not re-litigate.**

## What the reference does

`ScopeIndexer`/`StatementEvaluator` type a project method's nilable return
interprocedurally and thread it into the local: `scope = scope_for(id)` where
`scope_for` has `return nil if ...` and a param-rooted tail types the local
**`Dynamic[top]?`** (probe-confirmed via `type-of`). rigor-rs types it non-nil
and stays silent. That union is the ~86-gap bucket.

**The FP cliff, located:** `check_rules.rb:1219` `union_method_present_on_non_nil?`
→ `method_present_anywhere?` (:1226):
`class_name = concrete_class_name(member); return true if class_name.nil?  # Dynamic/Top/Bot — be permissive.`
A `Dynamic` arm satisfies the non-nil-arm check for EVERY method name. Probe:
the reference fires possible-nil on `scope.frobnicate_xyz` — a method that
exists nowhere. Once a local is `Dynamic|nil`, EVERY call on it fires.
rigor-rs's `check_nil_receiver` requires a nameable concrete arm with
`knows_class(C)` — **that requirement IS its FP-safety mechanism.**

**ADR-58 WD1 does NOT rescue this bucket.** WD1 (`declaration_sourced?`,
check_rules.rb:1171) suppresses ctor-seeded ivar-copied nil (probe-confirmed
silent) and is MANDATORY if ivar field typing (WD2) is ever ported (else the
documented 109 FPs). But ADR-58's own WD1b re-adjudicates method-return-transit
nil as "genuine-conservative… earned conservatism" and leaves it demand-gated:
there is no suppression to port. That bucket fires raw.

## The measurement (the decisive part)

1-in-8 sample of gitlab lib (585 files): reference 20 possible-nil, rigor-rs 4,
0 FP, 16 gaps. **All 16 adjudicated by reading the code → 16/16 are REFERENCE
FALSE POSITIVES, 0 real bugs.** Families: 6 nil-SAFE `present?`/`blank?` calls
(fire only because config-less mode lacks the AS RBS, so `NilClass#present?`
looks absent); 7 correctly-guarded sites (guard is `present?`/`&&`/a raising
helper the reference doesn't narrow through — incl. Grape `not_found!`/
`render_api_error!` which RAISE); 3 unprovable-invariant conservative firings.
The reference's own ADRs agree: ADR-57 WD3 calls the present?-guard class "the
one adjudicated ARTIFACT class"; ADR-58 exists because this exact `Dynamic|nil`
firing was "94% of possible-nil errors" on idiomatic Ruby.

## Slice decomposition (why there is no FP-safe path)

| Slice | Content | Reuse | Predicted close |
|---|---|---|---|
| S1 | nilable-return inference map + source arm | branch `tier-bc-nilable-return` (4fb56c5) cherry-picks whole | **0** (measured 2026-07-06) |
| S2 | `Node::If` descent + nenv/penv join + truthy narrowing | branch `flow-cond-assign-nilability` (7b7fe3d) cherry-picks whole | **0** (measured 2026-07-11) |
| S3 | **drop the concrete-arm requirement** (admit Dynamic arms) | net-new ~30 LoC | ~129 — **all reference FPs** |
| S4 | param-dependent returns + ivar typing + WD1 provenance + per-scope | ~9.6k LoC of reference substrate | multi-session |

**S1 and S2 are ALREADY BUILT and both measured 0 gaps.** Every gap in this
track is closed by S3 alone — the one slice whose entire content is deleting
the FP-safety mechanism.

## THE CRUX — the parity gate points the wrong way here

`fp_audit` measures FP *against the reference*, so S3 would score **0 FP and
+129 matched** and pass the whole standing battery cleanly. The gate cannot see
this failure. AGENTS.md's goal is a SOUND SUBSET; S3 imports ~150 unsound
diagnostics into gitlab lib alone. **Shipping it would be gaming the metric.**
First track where this divergence appears — record it.

## Decision

CLOSE the track. Keep both branches as the record. Narrow exception: if ADR-58
**WD1** is ever needed for a DIFFERENT measured reason (ivar precision), it must
be ported WITH the precision, never after. The 3 ATM corpus gaps and
`sig-gen --params=observed` are blocked on S1/S4 (nilable-return TYPING), not
S3 — S1 is already built and can be revived cheaply IF a measured gap needs it
(the ADR-0041 precedent working as designed). This is the **5th consecutive**
FP-safe flow slice to close 0 gaps, and now we know why it always will be: the
residual is not gated on missing machinery — it is gated on rigor-rs declining
to be imprecise.

## Do instead (ROI order)

1. **Productization** (the track with the demonstrated record: `check <dir>`,
   MCP sig_gen, the v0.3.0 arc): LSP §12 two-tier (watched-files invalidation,
   debounce, worker pool), baseline subcommands, config schema,
   `--bleeding-edge`, `coverage --workers`.
2. **Re-pin at the v0.3.0 tag** when upstream tags it (currently commit pin
   47ec8625) per UPSTREAM.md.
3. **The gitlab UM-200 residual is AS-overlay-dominated** — the only remaining
   measured coverage lever, but scope it as an INVESTIGATION first: an AS RBS
   overlay on rigor-rs alone would diverge against the config-less reference
   (and would incidentally suppress 6 of the 16 sampled reference FPs by making
   `NilClass#present?` known).
