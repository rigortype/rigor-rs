# Upstream rbs-inline auto-wire: measured regressions (2026-07-18)

> **CORRECTED 2026-07-19 — all three "regressions" (and the cross-file
> "feature") were ENVIRONMENT ARTIFACTS, not upstream changes.** The auto-wire's
> `require "rigor-rbs-inline"` resolved a stale installed `rigortype 0.2.4`
> gem's plugin copy (pre-annotation-gate: 0 hits for `annotated?`) instead of
> the checkout's, synthesizing skeletons for every file. With the checkout's
> plugin pinned (`-I plugins/rigor-rbs-inline/lib`) the wave's fixture-corpus
> delta is **0 added / 0 dropped**. Root-caused via upstream's triage
> discriminators (universe count 1348→1349 = the skeleton entering; a
> gem-less Ruby env load-erroring = the breadcrumb) — full trail on
> [rigortype/rigor#194](https://github.com/rigortype/rigor/issues/194). The
> REAL reportable finding is the engine↔plugin version-skew hazard, now
> guarded in `UPSTREAM.md` + `harness/lib.rb` + `fp_audit.py`. The re-pin
> item is UNBLOCKED (still waiting on the v0.3.0 tag per policy). Original
> (superseded) analysis kept below for the record.

Tracking check on the post-pin upstream wave (`73141341..b70adcb5`, the ADR-93
"auto-wire the bundled rigor-rbs-inline plugin by default" arc). The M1
self-diff tool (old pin `7a69f142` vs `b70adcb5`, `--no-cache`) plus minimal
repros isolate THREE single-file regressions and one behavior inversion.
**Pin held at `7a69f142`** — re-pinning today would turn 6 rigor-rs
diagnostics into oracle-FPs and drop upstream's own v0.3.0 flagship folds.

## Single-file regressions (no batch, no sigs — pure engine deltas)

1. **In-source return-chain inference lost** (fixture 15): the true
   `lenght` diagnostic on a chained in-source `String` return no longer
   fires. Same loss on the param-bound chain (fixture 19).
2. **Interprocedural literal folds lost** (fixture 59): all four
   always-truthy/falsey firings from `Gitlab::Database.read_only? -> false`
   -style constants are gone — the RC's own flagship feature.
3. **New cross-owner singleton FP** (fixture 59:72): `Unrelated.read_only?`
   — an EXPLICIT "must stay silent" negative (own-class resolution declines;
   only `Gitlab::Database` defines it) — now fires
   `undefined method `read_only?' for singleton(Unrelated)`.

Mechanism (inferred): the auto-wire synthesizes in-source classes into RBS
shells even for annotation-free files in standalone mode ("route standalone
installs to rbs-inline when annotations go unread"), so the RBS dispatch tier
answers BEFORE the in-source / interprocedural inference tiers — losing chain
inference and literal folds, and giving in-source classes a witnessable-empty
singleton surface.

## Cross-file inversion (batch runs)

A 2-file probe (`class Widget def spin` in a.rb; `w.spin` + an absent method
in b.rb) shows the new reference resolves in-source classes ACROSS analyzed
files and witnesses absences — a deliberate feature by the look of it (spin
resolves, the absent method fires), but it inverts the in-source-only
leniency in multi-file runs. NOTE: the earlier full-batch fixture-37/38
"breakage" was an ARTIFACT of the measurement (fixtures analyzed without
their per-fixture sig staging, cross-contaminated by fixture 35's in-source
`class Widget`); the harness's staged runs are unaffected.

## Disposition

- Pin stays at `7a69f142` until upstream resolves the wave (the regressions
  degrade upstream's own fixtures' intent, so a fix seems likely).
- Feedback package (minimal repros above) ready to file upstream — the same
  differential-harness feedback loop that produced upstream `c9d2e473` /
  `4e0ca475`.
- rigor-rs is NOT affected in default mode (we never re-pinned); the
  rbs-inline reader remains out of scope (ADR-93/94 track, Phase-3 note).
