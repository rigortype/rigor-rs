# 2026-09-30 — issue #162: baseline parity (regroup, folded scalars, `../` keys)

PR #323, merged `09c5024`. Two review rounds; both blocking items fixed;
sweep 0 FP; CI 5/5 on final head `6cc2bab`. Residuals #326–#329.

## What landed

- `filter` regroups surfaced diagnostics into insertion-ordered bins keyed
  `(file, rule, message-source)` / `NoBucket`, matching the reference's
  `group_diagnostics_for_filtering`; ruleless/pathless diagnostics append
  last; empty baseline stays a pass-through. `apply_baseline` replays the
  ordered indices instead of re-sorting.
- Psych-compatible scalar reader (`RowAcc`/`FieldVal`): folded plain scalars,
  `'…'`/`"…"` escapes incl. `\x`/`\u`/`\N`, `|`/`>` blocks with chomping,
  `!tag`s, ` #` comments. Message-mode baselines parse instead of dropping.
- Psych-compatible writer: `|-` literal for embedded `\n`, quote rules for
  `<<`/leading non-word/`y`/`n`, fold at col>80 — `generate` output
  byte-identical incl. hostile filenames.
- `../` keys via `relative_path_from` port (verified manually —
  `probe.py --dir` copies the project so `../` targets vanish; that probe is
  vacuous for outside-cwd paths).
- Uncompilable `message:` regex → whole baseline drops (ref's own
  `Regexp.new`→`LoadError` contract); Onigmo-only syntax is a disclosed
  residual (port warns + continues without baseline).

## Review fixes

- R1: `version:`/`ignored:` top-level lines now strip ` #` comments and read
  `null`/`~` case-insensitively — a hand-edited `version: 1 # schema` no
  longer drops the whole baseline.
- R2: `ignored:` + a PRESENT inline scalar (`~`/`null`/`[]`) now closes the
  array so following `- file:` rows are rejected (ref: Psych::SyntaxError;
  port: graceful LoadError degrade, never silences).

## Residuals

- #326 typed scalars (`count: 1_000`/`0x10`, falsy `message:`, typed
  `file:`/`rule:`, 32-bit `usize`).
- #327 Psych-level divergence (tab-indent silent-empty worst; anchors, bad
  escapes, `|+` chomping, `#`/`---` inside open quotes, duplicate `ignored:`).
- #328 per-file diagnostic ordering is rule-grouped vs positional — diverges
  even without a baseline.
- #329 `baseline dump` format (grouped-by-rule vs flat rows).
