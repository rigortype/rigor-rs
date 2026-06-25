# Read the reference's `.rigor.yml` and baseline formats unchanged

Status: accepted

rigor-rs reads the **identical** configuration and baseline formats as the reference implementation: `.rigor.yml` / `.rigor.dist.yml` (the same key schema — `target_ruby`, `paths`, `exclude`, `plugins`, `disable`, `libraries`, `signature_paths`, `pre_eval`, `baseline`, `cache`, `severity_profile`, `bundler.auto_detect`, `rbs_collection.auto_detect`, `plugins_isolation`, …) and the baseline file (reference ADR-22). Keys for features rigor-rs has not yet implemented are **warned-and-ignored, never errored**, so a project's existing config keeps working as phases land.

This is forced by the eventual-replacement goal ([ADR-0001](0001-rust-reimplementation-strategy.md)): a drop-in replacement must consume a project's existing configuration with no migration step. It also makes the baseline portable for free — because rigor-rs targets [diagnostic-set parity](0002-diagnostic-set-parity.md), baseline entries (keyed by rule id + location) match the reference's, so an existing baseline suppresses the same diagnostics under rigor-rs, and the differential harness can drive both tools from the same config over a corpus.

Severity profiles (`balanced` / `strict`) and `disable` wildcards are honoured with the reference's semantics so severities and suppressions match.

## Considered options

- **A bespoke rigor-rs config + a converter from `.rigor.yml`** — rejected: adds a migration step and ongoing drift between two schemas, friction against drop-in replacement.
- **Support only a core subset of keys** — rejected as the *contract*: unimplemented-*feature* keys are inertly accepted (warn-and-ignore), but erroring on unknown keys would break real configs mid-migration.
