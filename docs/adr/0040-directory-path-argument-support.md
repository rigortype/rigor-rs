# `rigor check <dir>`: directory arguments + path-error diagnostics

Status: accepted

`rigor check` accepts DIRECTORY arguments (and bad paths), matching the
reference's `Runner#expand_paths` (`analysis/runner.rb`). Before this, rigor-rs
read every path argument as a file, so a directory failed with a stderr
`cannot read <dir>: Is a directory` and analyzed nothing — `rigor check .`,
`rigor check app/`, the first command a real user runs, was broken. The
differential harness/`fp_audit` always pass explicit `.rb` file lists, so the gate
never exercised this; it surfaced only by probing real-usage invocation.

## Context — the reference is reasonable, so match it

The reference's file discovery, verified against the oracle (with a FRESH cache
per case — the on-disk cache otherwise returns stale cross-path results and made
an earlier probe look buggy):

- **Directory** → `Dir.glob("<dir>/**/*.rb")` then `reject_excluded` — recursive,
  `.rb` only, **skips hidden dirs** (`.git/`…, glob's default dotfile rule),
  **does not follow symlinked dirs** but **DOES match symlinked `.rb` files**
  (probed; corrected by the 2026-07-06 audit — the first cut skipped symlinked
  files too), **does not read `.gitignore`** (only the config `exclude:` list
  prunes), results **sorted**.
- **File** → accepted only if it exists and ends with `.rb`.
- **Existing non-`.rb` file** → error `not a Ruby file (expected `.rb` or a
  directory)`.
- **Missing path** → error `no such file or directory`.
- **Severity** — if ANY files were found across all args, bad paths are
  `warning` (`… (skipped)`) and the run proceeds; if NOTHING was found, bad paths
  are `error` (exit 1). An empty-but-existing directory yields no files AND no bad
  path ⇒ `success`, nothing analyzed.

Every case is sensible (accurate messages; warn-and-skip keeps a useful run alive;
a lone typo still errors rather than silently no-oping). There is nothing
unreasonable to fix upstream, so rigor-rs **matches the reference faithfully**.
The `.rb`-only scope (a `Rakefile`/`.gemspec` is rejected) is a deliberate, matched
limitation, not a bug — extending it would be a both-sides change, not now.

## The decision

1. **Faithful port of the discovery.** A std-only recursive walk (no new
   dependency; the Rust `glob` crate matches leading dots by default, the
   opposite of Ruby, so a hand-walk is the faithful choice): recurse each
   directory arg, skip entries whose name starts with `.`, do not traverse
   symlinked directories (but include symlinked `.rb` files — glob matches them),
   collect `*.rb`, sort. Config `exclude:` still prunes via
   the existing per-file `Config::is_excluded` gate in `analyze_files`.
2. **Path errors matched in SEMANTICS, emitted in rigor-rs's format.** Accurate
   messages and the any-files-else-error severity rule. Bad-path diagnostics use a
   **synthetic `rule_id`** (`path.not-found` / `path.not-ruby`), NOT the
   reference's `rule: null`.

3. **Exit code becomes error-severity-driven** — entailed by (2) and a
   pre-existing divergence fixed here. The reference exits `1` iff there is ≥1
   ERROR-severity diagnostic (`error_count > 0`); a warning-only run exits `0`
   (verified: reference exits 0 on an unresolved-toplevel warning, 1 on an
   undefined-method error). rigor-rs exited `1` on ANY finding — so it failed on
   warnings, and the reference's whole point in the warn-and-skip path (`rigor
   check good.rb missing.rb` should SUCCEED analyzing `good.rb`) was
   unreachable. `check` now exits `1` iff any finding is `Severity::Error` (or a
   genuine read I/O error), matching the reference; this also removes the
   incidental `--format json` exit 1 / text exit 0 inconsistency on a directory
   arg. The differential harness ignores exit codes (captures `_status`), so the
   gate is unaffected. (`--strict`, which the reference uses to also fail on
   warnings, is a separate flag, not ported here.)

## Considered options (path-error representation)

- **`rule: null` via `Option<rule_id>`** (fully faithful) — rejected: rigor-rs's
  `Diagnostic.rule_id` is a required `&'static str` used by baseline, every
  formatter, and the LSP; making it optional ripples widely. And rigor-rs's JSON
  is already a bare array, not the reference's `{success, error_count,
  diagnostics, stats}` envelope, so `rule: null` alone would not achieve drop-in
  JSON parity regardless. Bad paths are never in the differential harness (it
  passes only valid `.rb`), so the `rule` value is gate-invisible.
- **Synthetic `rule_id`** (chosen) — zero blast radius; keeps the parts that
  actually matter for CI/tooling faithful (severity, message, path, exit code).
  If the JSON envelope is later aligned to the reference as its own slice, the
  path-error `rule` can become `null` there in one place.

## Follow-on — bare `rigor check` uses config `paths:` (2026-07-06)

A bare `rigor check` (no path args) now scans the config `paths:` (`.rigor.yml`,
default `["lib"]`) instead of erroring `expected at least one file` — matching the
reference's `runner.run(@argv.empty? ? configuration.paths : @argv)`. Explicit
path args still take full precedence (config `paths:` is used ONLY when no args
are given). This reuses the `expand_check_paths` walk above, so `rigor check` in a
project root — the most natural invocation — works like the reference (e.g.
`paths: [lib]` scopes to `lib/`, excluding `spec/`). Verified message-identical to
the reference (including a missing `lib/`: error + exit 1, both tools). Unblocks
the baseline `regenerate`/`drift`/`prune` subcommands, which depend on
`configuration.paths`. **`baseline generate` with no path args does the same**
(the reference generates from `configuration.paths`; audit #2) — explicit args
override there too.

**Known limitation (documented divergence):** rigor-rs resolves relative `paths:`
entries against the process CWD; the reference resolves them against the project
root (the config file's base). These coincide in the normal case (`.rigor.yml`
auto-discovered in the CWD) and diverge only under an explicit `--config
path/to/.rigor.yml` run from elsewhere — revisit if that becomes a real workflow.

**Exit-code amendment (2026-07-06 audit #1):** the error-severity-driven exit
gained one exception — the synthetic `internal-error` finding (a per-file PANIC,
ADR-0016 never-crash) fails the run despite its info severity (which exists only
to keep it out of the harness parity gate). Without this, the severity-driven
rule would have silently green-lighted a crashed analysis in CI (the original
"exit 1 on any finding" rule had covered it by accident).

## Out of scope (pre-existing, separate)

The JSON output shape: rigor-rs emits a bare `[{…}]` array; the reference emits a
`{success, error_count, diagnostics, stats}` envelope. The differential harness
compares `(rule, line, column, severity, message)` tuples and normalizes across
both, so this divergence is gate-invisible and predates — and is independent of —
directory support.
