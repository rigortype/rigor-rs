# Differential Parity Harness

This directory implements the differential parity harness for rigor-rs, per
ADR-0002 (diagnostic-set parity via snapshots) and ADR-0011 (divergence
registry).

## How to run

From the repo root:

```
ruby harness/run.rb
```

The script will:
1. Build `target/debug/rigor` if the binary is absent (`cargo build --offline -p rigor-cli`).
2. Run both the reference Ruby Rigor and rigor-rs over every `harness/corpus/*.rb` fixture.
3. Print a per-fixture report and a summary.
4. Exit `0` if no unregistered false positives are found; exit `1` otherwise.

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

## Divergence registry

`harness/divergence-registry.yml` lists excused `extra` entries per ADR-0011.
Each entry must:
- Identify `(fixture, rule, line, column)` precisely
- Explain `reason` why the reference is wrong
- Link an `upstream` issue/PR on `rigortype/rigor`

Entries are removed once the upstream fix lands and the pinned reference is
bumped. The registry is expected to trend toward empty as both implementations
converge.
