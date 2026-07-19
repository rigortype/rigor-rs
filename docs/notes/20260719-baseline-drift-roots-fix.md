# baseline drift/prune positional roots + scope-less guard (2026-07-19)

## The defect

`rigor baseline drift` and `baseline prune` IGNORED positional path arguments and
analyzed ONLY config `paths:` (default `["lib"]`), while `baseline generate` /
`regenerate` (and `check`) HONOR positional roots. Their optparse `Positional`
arm dropped the token (`Ok(OptEvent::Positional { .. }) => {}`) and they called
`baseline_analysis(explicit_config, &[], "baseline")` with an EMPTY roots slice.

On a project with no `.rigor.yml` (config `paths:` falls back to the implicit
`["lib"]` default), this is dangerously misleading. Reproduced on
`/Users/megurine/repo/ruby/conference-app` (no `.rigor.yml`, but a `lib/` with
3 `.rb` files):

- `rigor baseline generate .` wrote a full baseline (1956 diagnostics / 98
  buckets, whole tree).
- `rigor baseline drift .` then dropped the `.` and analyzed only `lib/` (3
  files) — so all 98 buckets outside `lib/` reported as **Cleared (98)**
  (`app/... 1 → 0`, `config/... 5 → 0`, …), looking as if every finding had been
  resolved. A follow-up `prune` would have emptied the baseline.

## The fix

Both parts in `crates/rigor-cli/src/main.rs` (+ one accessor in `config.rs`).

**(a) Honor positional roots** — mirror generate's existing rigor-rs extension.
`baseline_drift` and `baseline_prune` now collect the `Positional` token into a
`files: Vec<&str>` and pass `&files` to `baseline_analysis` (was `&[]`). With no
positional, the empty slice still falls back to config `paths:`, so the
no-positional invocation stays reference-faithful.

**(b) Guard the scope-less audit** — `baseline_analysis` now returns a third
value `scope_undeclared: bool`, computed as
`roots.is_empty() && !cfg.paths_explicitly_declared()` (a new `Config`
accessor over the existing `present_keys` set — the reference's `["lib"]`
default is a fallback, NOT a user-declared scope). When `scope_undeclared` AND
the loaded baseline is non-empty, drift/prune emit a stderr usage error and
return exit **64**:

```
rigor: baseline drift: nothing to analyze — pass a path (e.g. `rigor baseline drift .`) or declare `paths:` in .rigor.yml
```

generate/regenerate ignore the flag (a `lib`-scoped baseline is legitimate).
Behavior is unchanged whenever there IS a declared scope (a positional, or an
explicit `paths:`).

## Reference parity

Confirmed additive. In `reference/rigor/lib/rigor/cli/baseline_command.rb`,
`run_drift`/`run_prune` (and generate) all analyze `configuration.paths` via
`runner.run(configuration_for_generation.paths)`; `parse_drift_options` /
`parse_prune_options` call `OptionParser#parse!(@argv)`, which strips known
options and LEAVES positionals unused — the reference never reads them.
Positionals are therefore a rigor-rs additive extension (exactly like generate's,
already documented), and the reference-faithful no-positional path is unchanged.
The scope-less guard is an additive rigor-rs safety improvement; it does not
touch any parity-tested default-config surface (it only refuses when the loaded
baseline is non-empty AND no scope is declared, where the reference would
silently mislead).

## conference-app before/after (release binary, this branch)

Step 1 — `rigor baseline generate . --output /tmp/ca-bl.yml --force`:
`rigor: wrote baseline to /tmp/ca-bl.yml (98 bucket(s) covering 1956 diagnostic(s); match-mode: rule)` (exit 0).

| Step | Before (defect) | After (fix) |
| --- | --- | --- |
| 2. `drift . --baseline /tmp/ca-bl.yml` | `## Cleared (98) …` (all buckets `→ 0`) | `No drift detected.` (exit 0) |
| 3. `drift --baseline /tmp/ca-bl.yml` (no positional, no `paths:`) | `## Cleared (98) …` (exit 0) | `rigor: baseline drift: nothing to analyze — …` (exit 64) |

## Tests

`crates/rigor-cli/tests/baseline_drift_roots.rs` (hermetic, `RIGOR_NO_RUBY=1`,
std-only temp dir):
- generate-then-drift with a positional root on a project WITHOUT config
  `paths:` reports ZERO drift (not all-cleared);
- drift / prune with NO roots and NO declared `paths:` against a non-empty
  baseline error (exit 64) — prune leaves the baseline byte-identical;
- drift with a declared `paths:` and no positional does NOT trip the guard.

Gates: `cargo test` (workspace), `ruby harness/run.rb` + `run_snapshot.rb`
(PASS, 0 unregistered FP, 216/218), `python3 harness/docs_check.py` (PASS),
fresh-dir `cargo clippy --workspace -- -D warnings` (clean).
