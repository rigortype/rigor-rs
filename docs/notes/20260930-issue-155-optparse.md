# Issue #155 — OptionParser-compatible CLI argument parsing

PR #359, merged `9eaae52` (rebased head `a0c158b`). One adversarial round, Approved.

## What landed

A shared OptionParser-compatible parser (`rigor-cli/src/optparse.rs`) replaces
the "unrecognised argument becomes a path" behavior across all subcommands:

- Unknown flags / `-file.rb` → exit 64, `invalid option:`/`ambiguous option:` —
  byte-identical, no output as path.
- Unambiguous-prefix abbreviations (`--basel=x`, `--no-basel`); ambiguous
  prefixes rejected with the reference's candidate list.
- `=` forms (`--config=x`, `--baseline=x`, `--format=json`); `--` terminator;
  missing-argument `missing argument:` exit 64.
- `POSIXLY_CORRECT` (even empty) stops option permutation at the first
  non-option — `check a.rb --config x.yml` treats both as paths on both engines.
- Per-command flag tables and ambiguity sets (`check --b` ambiguous vs
  `baseline dump --b` → baseline); required args consume flag-looking tokens;
  `--no-` negations; did-you-mean suggestions; post-parse validation order.
- Reference-only flags classified: implemented (`--fail-on`, `--workers`…),
  accepted no-ops (`--no-cache`, `--clear-cache`, `--stats`…), rejected
  exit-64 (`--explain`, `--coverage`, `--incremental`, `--tmp-file` pairing…).
- Port-only `--ruby`/`--no-ruby` kept (ADR-0036) with exit-64 mutual exclusion.
- Manual-dispatch parity for `plugin`/`skill`/`describe`/`docs` error shapes
  (Unknown subcommand 64, Unknown skill list exit 1, verbatim `--help`).

## Measured

Review ran ~240 fresh-cwd invocations across 24 commands incl.
POSIXLY_CORRECT/`-file.rb`/`=`-edge/mixing rows — byte-identical exit +
stdout + stderr everywhere claimed. 528 unit + 27 integration tests; gates
green; 0 FP sweep.

## Residuals (filed / disclosed)

- **#360** — manual-dispatch missing-name grammar on deferred commands
  (`plugin path` bare → upstream 64+usage vs exit-2 stub).
- Disclosed pre-existing: `baseline *` positionals-as-roots extension;
  `docs --list/--path` standalone deviations; `diff` prints only the first
  failing load (ref prints both, serde wording); `triage` doesn't count
  missing-path warnings; `coverage --protection` exit 2; `--clear-cache`
  is a no-op on disk; `--*-completion-zsh` bare prints `#compdef rigor`;
  `--format=json` flat array vs envelope; stderr `--stats` block out of scope.
