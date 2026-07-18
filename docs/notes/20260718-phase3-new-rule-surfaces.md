# Compat Phase 3 — new rule surfaces (2026-07-18)

Third phase of [the compat plan](20260718-compat-next-stage-plan.md), scoped by
the minimal-final-diff directive: port what is DEFAULT-OBSERVABLE, document
what is structurally absent or gated off.

## Landed — unknown top-level config keys (reference #166 / ADR-99)

`excludee:` for `exclude:` loaded in silence; now warned with a did-you-mean
hint. Three deliberate choices:

- `Config::KNOWN_KEYS` is the REFERENCE's full 21-key list (dumped from the
  pinned `Configuration::KNOWN_KEYS`), not rigor-rs's parsed subset — a key
  the reference owns but rigor-rs does not parse (`severity_overrides:`,
  `libraries:`, …) is real and never warned. `rigor_rs:` is a member, so the
  ADR-99 reserved-namespace exemption is inherent.
- Top level only (nested unknowns are the reference schema tier's job).
- The suggestion engine is a VERBATIM port of Ruby stdlib
  `DidYouMean::SpellChecker` (+ JaroWinkler + Levenshtein), pinned against the
  REAL stdlib over 13 typo cases and byte-identical end-to-end
  (`` rigor: `excludee` is not a recognized configuration key; it has no
  effect. Did you mean `exclude`? ``).

## Structurally absent — `rbs.coverage.environment-build-failed`

The reference's env collapses to nil (empty run) when a project sig
redeclares a bundled class (`RBS::DuplicatedDeclarationError`); the
diagnostic flags that self-failure mode. rigor-rs's native index merges
reopens by UNION, so the environment CANNOT collapse — the failure condition
does not exist, and reproducing the collapse would make rigor-rs strictly
worse. Nothing to port; recorded here.

## Deferred — `static.value-use.void` (+ void→top)

Verified in the pinned reference: authored `:warning` but **resolved `:off`
in every shipped profile** — it reaches a user only through the
`use-of-void-value` bleeding-edge feature (ADR-50 WD1 / ADR-100). A port
today would add the void_origins side-table + value-context collector + the
`--bleeding-edge` surface for ZERO default-observable effect (M1 also
measured void→top at 0 corpus diagnostics). It rides with the
`--bleeding-edge` CLI item already on the productization track, where it
becomes observable the moment it lands.
