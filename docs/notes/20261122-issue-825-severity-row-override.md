# 2026-11-22 #825 — severity `row_override` + `FAMILY_FALLBACK_EXEMPT` (PR #832)

`severity::resolve` never ported the reference's `qualified_rule` /
`plugin_family` kwargs or `FAMILY_FALLBACK_EXEMPT`. Two observable
divergences, only the second visible through `check` output today:

1. `severity_overrides: {plugin: …}` (a dot-free key) reached the built-in
   id `plugin.api-surface` — reference: exempt, the key is a plugin-family
   key. Port emits no `plugin.api-surface` rows, so this arm is unit-test
   coverage only until a plugin-side emitter lands.
2. **Both run-level plugin streams bypassed the stamp entirely** — a stale
   comment claimed `SeverityStamp` skipped `plugin_loader.*` / `plugin.<id>.*`
   rows on the strength of one probe (`plugin_loader: "off"` leaves the row
   at error). Re-probing showed that miss is just key-shape, not exemption:
   the row answers to `plugin_loader.load-error` (qualified), `load-error`
   (bare), or a family wildcard — and a plugin's own row answers only to its
   qualified id or `plugin.<id>`, never its bare rule or a wildcard.

## Fix

`resolve` gained `qualified_rule`/`plugin_family` (reference kwarg order);
`lookup_override` → faithful `row_override` + `family_override` with the
dot-free-key short-circuit and `FAMILY_FALLBACK_EXEMPT`. The two run-level
streams stamp via `stamp_plugin_row` — bare rule derived by stripping the
literal `<source_family>.` prefix from the port's qualified `rule_id`,
`plugin_family` only under the `plugin.` prefix (`plugin_loader` is a
source family, not a `plugin.` one). Built-in call sites pass
`qualified == rule`, `None` — behaviour-preserving. `disable:` handling
unchanged (`plugin_row_disabled?` stays the only disable gate on plugin
rows; the loader row remains unreachable by it).

## Measured outcome

9 `.rigor.yml` shapes probed on both engines (`plugins:` activerecord /
missing-gem rows × family/qualified/bare/wildcard keys): all tuples and
exit codes match after the fix; `plugin.activerecord: "off"` and
`plugin_loader.load-error: "off"`/`load-error: "off"` previously left rows
standing. 0 FP over the standing sweep; gate + CI green. Opus 5.5 review
**Approved** `8a51395d` after re-probing all rows + 20 counterexamples.
Design companion substituted to `devin/swe-2` (claudeAgent Fable quota,
Grok 402, opencode key — substitution recorded per review.md).

## Surfaced follow-ups

- **#833** — the `(rule_id, source_family) → (bare, qualified,
  plugin_family)` triple lives in three places; per-file/LSP stamp sites
  hardcode `plugin_family = None` under a builtin-only invariant. Centralise
  before `plugin.<id>.*` per-file rows / `plugin_loader.runtime-error` /
  `plugin_trust.*` / `generated.*` land (runtime-error case is an FP
  direction).
- **#834** — severity is stamped post-cache upstream (`RunCacheProbe`
  re-stamps served rows); any ported diagnostics cache must store
  pre-stamp severities or key on every `resolve` input.
- **#835** — `check --format json` prints a bare array vs the reference's
  schema envelope (pre-existing, disclosed on the PR).
