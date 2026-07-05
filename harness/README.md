# Differential Parity Harness

This directory implements the differential parity harness for rigor-rs, per
ADR-0002 (diagnostic-set parity via snapshots) and ADR-0011 (divergence
registry).

## Two ways to run the gate

| Script                   | Reference needed? | Role                                              |
|--------------------------|-------------------|---------------------------------------------------|
| `harness/run.rb`         | **Yes** (live)    | **Local source-of-truth gate**; regenerates snapshots |
| `harness/run_snapshot.rb`| **No**            | **CI parity gate** — compares against committed snapshots |

Both apply the IDENTICAL gate semantics (shared in `harness/lib.rb`): a
`(rule, line, column)` match over error/warning severities, false positives
fail, missing diagnostics are coverage gaps, and the divergence registry
excuses specific extras. `run_snapshot.rb` differs only in WHERE the expected
reference diagnostics come from (a committed JSON snapshot vs. a live reference
run) — it never touches `REFERENCE_RIGOR_DIR`.

### Live gate (local, needs the reference)

From the repo root:

```
ruby harness/run.rb
```

The script will:
1. Build `target/debug/rigor` if the binary is absent (`cargo build --offline -p rigor-cli`).
2. Run both the reference Ruby Rigor and rigor-rs over every `harness/corpus/*.rb` fixture.
3. Print a per-fixture report and a summary.
4. Exit `0` if no unregistered false positives are found; exit `1` otherwise.

### Snapshot gate (CI, no reference)

```
ruby harness/run_snapshot.rb
```

Identical to `run.rb` but loads each fixture's pinned reference diagnostics
from `harness/snapshots/NN_name.json` instead of running the live reference. It
needs only the built `rigor` binary + Ruby + the committed snapshots — no
reference checkout. This is the gate the `parity` job in `.github/workflows/ci.yml`
runs on every PR.

## Reference snapshots

`harness/snapshots/NN_name.json` pins the reference's expected diagnostic set
for each fixture: a stable, sorted, pretty-printed list of
`{rule, line, column, severity, message}` (the gate keys on
`(rule, line, column)`; `severity`/`message` are for human review). The
absolute `path` is deliberately omitted — it is machine-specific and the
generator already filters to the fixture.

### Regenerating snapshots (needs the reference)

Regenerate whenever a fixture changes or the pinned reference updates, then
commit the diff:

```
ruby harness/snapshot.rb           # rewrite every snapshot from the live reference
ruby harness/snapshot.rb --check   # verify snapshots are up to date (exit 1 on drift)
```

Output is deterministic (sorted, pretty, trailing newline), so a no-op
regeneration produces no diff. The live `harness/run.rb` remains the
source-of-truth: the snapshots are derived from it, and snapshot-mode must
always agree with a live run (same matched / 0 FP / same missing set). Plugin
sidecar fixtures (`NN_name.rigor.yml`) are handled exactly as `run.rb` does
(the generator runs the live reference with the plugin `-I` + `--config`).

### Environment variables

| Variable             | Default                              | Description                        |
|----------------------|--------------------------------------|------------------------------------|
| `REFERENCE_RIGOR_DIR`| `/Users/megurine/repo/ruby/rigor`    | Path to the Ruby rigor checkout    |
| `RIGOR_RS_BIN`       | `target/debug/rigor`                 | Path to the rigor-rs binary        |
| `CORPUS_DIR`         | `harness/corpus`                     | Directory of fixture `.rb` files   |
| `DIVERGENCE_REGISTRY`| `harness/divergence-registry.yml`    | Path to the divergence registry    |

## Parity discipline

The gate is: **rigor-rs must never emit a diagnostic the reference does not emit.**

| Category                    | Name in harness   | Effect on CI              |
|-----------------------------|-------------------|---------------------------|
| Both emit `(rule, line, col)` | **matched**       | Green — parity confirmed  |
| Reference emits, rigor-rs doesn't | **missing** (coverage gap) | Reported, not a failure — expected during the port |
| rigor-rs emits, reference doesn't, **registered** | **extra (registered)** | Green — excused divergence |
| rigor-rs emits, reference doesn't, **unregistered** | **extra (unregistered)** | **RED — hard failure** |

Coverage grows over time as rigor-rs implements more rules. The coverage
percentage is `matched / |reference diagnostics|`.

### Why this framing?

Rigor's defining property is zero false positives. An `extra` diagnostic
means rigor-rs is noisier than the reference — that is a regression the port
must never introduce. Missing diagnostics (coverage gaps) are expected and
harmless during the incremental port.

## Corpus

`harness/corpus/` contains small self-contained fixture files. Each file has
a header comment documenting the expected reference diagnostics and the
current rigor-rs support status. Fixtures are numbered:

- `01`-`06`: cases rigor-rs already handles (tracer-bullet `call.undefined-method`)
- `07`-`09`: coverage-gap cases (rules not yet implemented in rigor-rs) that
  exercise the `missing` path without causing failures

### Fixture conventions

- **Sidecar config** — `corpus/NN_name.rb` may ship `corpus/NN_name.rigor.yml`;
  both tools then run with `--config <sidecar>` (used for plugin fixtures, ADR-25).
- **Project sig/** (ADR-0033) — `corpus/NN_name.rb` may ship a sibling directory
  `corpus/NN_name.sig/` of `.rbs` files. The harness stages a copy as `sig/` in
  each tool's cwd, so the default `signature_paths: ["sig"]` ingests it — the two
  implementations run symmetrically over the real project-signature path (e.g.
  `37_project_sig_new`, `38_project_sig_negatives`).
- **rbs collection** (ADR-0034) — `corpus/NN_name.rb` may ship a sibling
  `corpus/NN_name.collection/` whose CONTENTS (an `rbs_collection.lock.yaml` +
  a `.gem_rbs_collection/` tree) are copied into each tool's cwd root, so the
  default `rbs_collection.auto_detect` discovers and ingests the gem RBS (e.g.
  `39_rbs_collection_new`).

A staged fixture (sig/ or collection) runs BOTH tools with `chdir` into the
staged tmpdir, so they see an identical project layout — no per-tool config
divergence.

## Divergence registry

`harness/divergence-registry.yml` lists excused `extra` entries per ADR-0011.
Each entry must:
- Identify `(fixture, rule, line, column)` precisely
- Explain `reason` why the reference is wrong
- Link an `upstream` issue/PR on `rigortype/rigor`

Entries are removed once the upstream fix lands and the pinned reference is
bumped. The registry is expected to trend toward empty as both implementations
converge.
