# Severity-resolution machinery (2026-07-18)

The design decision ratified after the bleeding-edge arc: of the remaining
productization items, `severity_profile:` / `severity_overrides:` was the one
REAL incompatibility (rigor-rs ignored both keys entirely — a project setting
`strict` or a family override got balanced output today) with near-zero design
freedom (the reference's resolve is small and portable verbatim).

## What landed

- `severity.rs` (S1, subagent-built): the reference `SeverityProfile` port —
  three 28-row PROFILES tables dumped verbatim from the pinned reference,
  `resolve()` with the exact precedence: user override (rule id, then FAMILY =
  first `.`-segment) > bleeding-edge override (exact id only) > profile table >
  authored fallback.
- Config keys (S2, subagent-built): `severity_profile:`
  (lenient|balanced|strict; anything else degrades to balanced — the reference
  raises; rigor-cli config never aborts, documented divergence) and
  `severity_overrides:` (rule-or-family → error|warning|info|off; invalid
  entries dropped). serde_yaml 0.9 is YAML 1.2, so bare `off` parses as a
  STRING here — the reference's Psych (YAML 1.1) bare-`off`-is-false trap does
  not exist; verified empirically and documented.
- SeverityStamp (S3): the `severity_stamp.rb` port at the `analyze_files`
  exit — re-stamp each diagnostic, DROP `:off`; the internal-error sentinel
  bypasses (the reference's `rule.nil?` short-circuit). Bleeding-edge
  overrides merged via `severity_overrides_for` (active features, later wins).
- The `static.value-use.void` collector gate became the reference runner's
  memoized activation gate: run when the RESOLVED severity ≠ `:off` — so a
  user `severity_overrides:` entry ALONE resurrects the rule without the
  bleeding-edge feature, exactly as it does there (probed live).

## Parity evidence (all live byte-diffs vs the reference)

strict / lenient / rule-`"off"` / family-`"info"` / strict+rule-downgrade /
lenient+bleeding-edge — IDENTICAL (6/6). Void resurrection via override alone
and feature-on + user-downgrade-to-info — IDENTICAL (2/2). Default (configless)
output byte-unchanged: live + snapshot gates 205 matched / 0 gaps / 0 FP;
fp_audit spot 0 FP. Workspace tests +25 (17 resolve, 8 config).
