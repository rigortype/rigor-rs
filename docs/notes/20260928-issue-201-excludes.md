# #201 — `BUILTIN_EXCLUDES`/`exclude:` applied at expansion, exact `File.fnmatch?`

PR #285 (merged, head `02b9000`, squash `8902991`).

## The bug — two layers

1. **Filtering ran too late.** The port expanded directories, then applied a
   per-file `Config::is_excluded` (`glob::Pattern`) gate inside `analyze_files`
   stage 1. The reference applies `BUILTIN_EXCLUDES + exclude:` inside
   `PathExpansion.directory_files` (`reject_excluded`), and keeps explicit
   `.rb` roots verbatim (`accept_as_ruby_file?` never consults the list). So
   the port analysed `lib/node_modules/x.rb` (FP the reference never emits)
   and silently dropped an explicit `check lib/ext.rb` under
   `exclude: [lib/ext.rb]` (coverage loss).
2. **The matcher drifted.** `glob::Pattern` ≠ `File.fnmatch?` at flags 0:
   `a/**/b` matched `a/b` upstream it does not, leading-period and bracket
   semantics differed.

## What landed

- `expand_check_paths_excluding` is the single analyzed-set expansion
  (dir glob → `reject_excluded`; `.rb` file roots verbatim); `check`,
  `baseline`, `diff`, `triage`, `effects`, `project_files` (LSP overlay) all
  share it. The per-file stage-1 gate is gone.
- `conformance_gate::fnmatch_chars` — an exact `dir.c` `fnmatch_helper` +
  `bracket` port at flags 0: `*`/`?`/`[` span `/`, consecutive `*`s collapse,
  leading-period at position 0, `\` escapes, first-`]` closes, endpoint
  equality before the `c1 <= c <= c2` test, unterminated `[` fails. Every row
  oracle-measured. `&str` wrapper kept `#[cfg(test)]`; hot paths decode each
  pattern once per expansion (`exclude_fnmatch`) and each path once per call.
- `effective_config_paths` absolutizes declared `paths:` (`resolve_paths_in`)
  so the exclude match sees the same spelling upstream produces.
- LSP `ExcludeMatcher` (final-review fix): the overlay-absent tiers no longer
  pattern-match spellings `check` keeps verbatim — a `paths:` `.rb` FILE entry
  contributes no excludable spelling (`push_spellings`), the out-of-`paths:`
  fallback contributes none (`check <file>` is verbatim), and
  `survives_discovery` counts a file root resolving to the buffer as surviving
  (`file_root_hit`). Fixes the overlay-state-dependent divergence where
  `paths: [lib/ext.rb]` + `exclude: ["**/ext.rb"]` published nothing.
- Baseline `generate`/`regenerate` summary now counts `rule: nil`
  path-expansion diagnostics (`covering 1 diagnostic(s)` for a missing
  `lib/`); written YAML byte-identical.
- `effects` analyzes `(configuration.paths + scope).uniq` like upstream
  (`effects_command.rb:439`); the report renderer stays a pre-existing gap.

## Measured outcome

Fresh-dir `harness/probe.py` vs pin `e59b7b89` — every listed scenario's
stdout/stderr/exit identical: `check lib` with `lib/node_modules/x.rb`
firing (was FP), `check lib/ext.rb` under `exclude: [lib/ext.rb]` firing (was
silent), `a/**/b.rb` shape (`a/b.rb` fires, `a/x/b.rb` excluded — was silent),
`check .` `./`-led spellings surviving `**/*.rb`, `triage`, and `baseline
generate` stderr (`covering 1 diagnostic(s)`; YAML byte-identical).

Gates: `gate.sh` pass; CI green on `02b9000`; `fp_audit --gaps --sweep` over
8 corpora: **0 FP**; `cargo test -p rigor-cli` 488 bin + 27 integration.

## Review

OpenCode (GLM/Kimi/MiMo/DeepSeek) + Grok 4.6 all quota/payment-exhausted —
Codex `gpt-5.6-sol` substituted for both rounds. Primary: 3× REVISION_REQUIRED
(per-element `to_string`, repeated `Vec<char>` decode, `mut` negation flag) →
`[PASS_PRIMARY]`. Final: REJECT_AND_REWORK on the LSP verbatim-root hole above
→ fixed in `2760808` → 【MERGE_APPROVED】.
