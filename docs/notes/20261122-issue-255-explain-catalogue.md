# Issue #255 — `rigor explain` synced to the reference's full catalogue

PR #816, merged head `0a1f579b`. One-round Opus 5.5 adversarial review
(`claude -p`, tool-enabled): `Approved`, no findings; 151 re-probes
byte-identical. Design companion also Opus 5.5 — `claude-fable-5-1`, opencode
Grok and Codex-sol all quota-blocked (recorded in the PR body).

## What was wrong

`explain` kept its own copies of the rule-token vocabulary, and they had
drifted on three axes (the #255 pattern):

- `RULE_FAMILIES` carried 6 of the reference's 9 — `explain effect|static|
  global` exited 64 where the oracle prints the family.
- `ENTRIES` missed 9 of 36 ids (`effect.*` ×4, `global.*` ×2,
  `call.deprecated-ruby2-keywords`, `rbs.contradicting-signature`,
  `plugin.api-surface`) and 10 surviving entries' authored text had drifted.
- `SigGen::SkipReasonCatalog` (ADR-108 WD4) — the second catalogue `explain`
  resolves FIRST — was unported: `explain sig.skipped.*` read `Unknown rule`
  and the index lacked the sig-gen skip-reason section.

## What landed

- `explain.rs` reads `rigor_rules::{rule_families, all_canonical_rules,
  legacy_aliases}` (newly exported from `suppression.rs`) instead of copies;
  `ENTRIES` regenerated verbatim from the oracle's `explain --format=json`
  dump; `SkipReasonCatalog` ported (16 ids, own text + bare-object JSON
  shapes, `explain_skip_reasons` notice, index section).
- `suppression.rs`: `plugin.api-surface` joined `ALL_CANONICAL_RULES` (it is
  in `ALL_RULES`) — a real fix beyond the brief: `disable:` /
  `severity_overrides:` naming it no longer warns `names no plugin` under
  `plugins: []` (review-verified; `plugin.api-surfacex`/`plugin.nope` still
  warn). Three doc nits fixed.
- `sig_gen.rs`: the 16 `sig.skipped.*` ids single-sourced as `SKIP_*` consts +
  `SKIP_IDS`; the stderr summary now **derives** `SUMMARISED_SKIP_REASONS`
  (16 − the 4 own-report ids) — the hand list had drifted to 8 (companion
  finding; output-neutral, the 4 missing ids have no producing path yet).
- Drift guards: `catalogue_agrees_with_the_rules_crate_tables` (families
  expand ≥1 entry; `ENTRIES` ids == `ALL_CANONICAL_RULES`) and
  `skip_catalogue_covers_the_sig_gen_vocabulary` (`SKIP_ENTRIES` ids ==
  `SKIP_IDS`).

## Measured

Every `explain` probe byte-identical (stdout/stderr/exit): 36 rule ids × 2
formats, 16 skip ids × 2 formats, 9 families, index text + json, alias and
must-still-fail controls (`sig`, `sig.skipped`, `rbs`, `plugin`, `pre-eval`,
`all`, `bogus.rule`) — plus review counterexamples (case variants, `""`, `é`,
`🦀`, extra positionals, `--format=j`, `-h`). `sig-gen --print|--check` on a
skip-producing project identical. Sweep: 0 FP.

## Follow-ups

- **#818** — `severity.rs` PROFILES carry the same drift (28 vs 36 rows —
  the same nine ids). Latent: no emitted diagnostic is affected; the port
  falls back to authored severity where the reference resolves a profile
  row (e.g. `global.write-type-mismatch` lenient `:warning`, not authored
  `:error`). Fix: verbatim resync + an entries↔profiles agreement test.
- MCP `explain_json` emits `{"skip_reasons":…}` for skip tokens — the port's
  own `{"rules","count"}` wrapper convention; the reference MCP tool's
  bare-object shape is a pre-existing divergence, not opened here.
