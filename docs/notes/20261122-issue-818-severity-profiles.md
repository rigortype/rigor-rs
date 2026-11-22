# 2026-11-22 #818 — severity PROFILES resynced 28→37 rows (PR #824)

Second reference-table drift in the family #255 found: `explain`'s catalogue
was resynced in PR #816, but `crates/rigor-cli/src/severity.rs`'s three
PROFILES tables still carried 28 rows where `severity_profile.rb` (pin
`bbea1bac`) has **37**. Missing: the same nine catalogue ids —
`call.deprecated-ruby2-keywords`, `global.readonly-write`,
`global.write-type-mismatch`, `effect.envelope-exceeded`,
`effect.liskov-widened`, `effect.unknown-label`,
`effect.annotations-unchecked`, `plugin.api-surface`,
`rbs.contradicting-signature`.

## Fix

All three tables regenerated verbatim from the pin and verified
row-for-row by script (id + severity, alphabetical, no pre-existing row
changed). Comment + count test updated to 37;
`resynced_rows_spot_checks` covers the rows whose profile value diverges
from authored severity (lenient `global.write-type-mismatch` → warning vs
authored error; `effect.unknown-label` caps at warning even under strict;
`effect.annotations-unchecked` stays info everywhere;
`plugin.api-surface` off everywhere).

## Drift guard

`explain.rs` gained `severity_by_profile_agrees_with_the_profile_tables`:
per `ENTRIES` id and profile, `severity::resolve(id, severity_authored,
profile)` must equal the entry's `severity_by_profile`. Verified first
that the reference agrees with itself — all 36 `RuleCatalog::ENTRIES`
match its PROFILES on every cell, and every catalogue id has a row.
`rbs_extended.unsatisfied-conformance` is in PROFILES but not the
catalogue in both engines (emitted outside `IMPLEMENTED_RULES` via
`conformance_rows`).

Known blind spot (from review, disclosed on the PR): an absent row whose
value equals authored×3 is invisible to the guard — harmless while rules
emit at `severity_authored`.

## Measured outcome

0 FP over the standing sweep; no diagnostic can change — none of the
nine ids is implemented. CI green (build+test ×2, snapshot parity,
live-reference check, docs). Opus 5.5 gate **Approved** `a5ff86e8`;
design-companion pass unavailable (claudeAgent Fable quota, Grok 402,
opencode key, opencode-go weekly — substitution recorded per
`docs/agents/review.md`).

## Surfaced follow-up

`severity::resolve` never ported the reference's `qualified_rule` /
`plugin_family` params or `FAMILY_FALLBACK_EXEMPT = ["plugin.api-surface"]`,
so a `plugin` family `severity_overrides:` key re-stamps
`plugin.api-surface` in the port but not the reference. Filed as #825.
