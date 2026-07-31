# Scaled Corpus Harness — Audit R4

## Purpose

`harness/run_corpus.rb` is the scaled differential corpus harness for Audit
action R4 ("land OSS corpora — 12 fixtures can't grade the design").  It runs
the reference Ruby Rigor and rigor-rs over real Ruby source files, compares the
diagnostic sets, and surfaces **false positives**: diagnostics rigor-rs emits
that the reference does NOT.  False positives are the gate — any nonzero count
fails the run and exits 1.

Coverage gaps (reference emits, rigor-rs misses) are reported but are expected
noise given that rigor-rs currently implements only three rules.

## How to run

From the repo root:

```sh
ruby harness/run_corpus.rb
```

Default corpora are built in (see below).  Custom directories can be passed as
positional arguments:

```sh
ruby harness/run_corpus.rb /path/to/dir1 /path/to/dir2
```

### Environment variables

| Variable              | Default                                     | Purpose                                |
|-----------------------|---------------------------------------------|----------------------------------------|
| `CORPUS_LIMIT`        | `80`                                        | Max .rb files sampled per corpus dir   |
| `REFERENCE_RIGOR_DIR` | `reference/rigor` (the PINNED submodule)    | The oracle. Pointing this at a working checkout compares against a different version — UPSTREAM.md hazard 3 |
| `SWEEP_CORPORA`       | `harness/sweep-corpora.yml`                 | The standing sweep set's manifest      |
| `RIGOR_RS_BIN`        | `target/debug/rigor` (under repo root)      | Path to the rigor-rs binary            |

The binary is auto-built if absent (`cargo build --offline -p rigor-cli`).

## Corpora

Two sources, one list:

1. The reference's own trees (`examples/`, `lib/rigor/type/`) — taken from the
   **pinned submodule**, so they move with the pin.
2. The **standing sweep set**, read from `harness/sweep-corpora.yml`. That file
   is the single membership list for both this script and
   `harness/fp_audit.py --sweep`, so the two cannot drift apart on *which*
   corpora are measured — the drift that let 24 false positives sit unmeasured
   on corpora nobody had typed into a command line
   ([note](../docs/notes/20260731-survey-fp-triage-24.md)).

Each entry carries a `why:` recording what that corpus caught that no other
member catches. Add one only with such a reason; do not remove one because it is
currently quiet. A member absent on this machine is reported as `SKIPPED` by both
tools — a partial sweep must never read as a full one.

Custom directories passed as positional arguments replace the whole list.

## Standing sweep-set baseline (2026-07-31)

`python3 harness/fp_audit.py --gaps --sweep`, reference pinned at `v0.3.1`,
vendored rbs 4.1.0. **9204 files, 0 FP candidates.** Coverage gaps are the
sound-subset side of ADR-0002 and are expected:

| corpus | files | coverage gaps |
|---|---|---|
| mastodon/app | 1236 | 48 |
| gitlab-foss/lib | 4676 | 329 |
| survey/mail | 874 | 540 |
| survey/Ruby | 192 | 30 |
| survey/dependabot-core | 1650 | 81 |
| survey/concurrent-ruby | 345 | 86 |
| survey/net-ssh | 180 | 75 |
| survey/haml/lib | 51 | 5 |

Note what this measurement CANNOT see: both sides run from a clean cwd
(core+stdlib only), so no project-`sig/` behaviour is exercised. Use a
hand-built project for that — see
[note](../docs/notes/20260731-head-survey-and-set-op-folds.md).

## Historical run results (2026-06-26, Audit R4)

The original three-corpus run that commissioned this harness. Kept as the record
of what it was built to prove; the live numbers are the baseline above.


### Corpus 1 — rigor/examples
| Metric                    | Value |
|---------------------------|-------|
| Files scanned             | 32    |
| Reference diagnostics     | 19    |
| rigor-rs diagnostics      | 11    |
| Matched                   | 11    |
| Coverage gaps (missing)   | 8     |
| Coverage %                | 57.9% |
| **False positives**       | **0** |

### Corpus 2 — rigor/lib/rigor/type
| Metric                    | Value  |
|---------------------------|--------|
| Files scanned             | 23     |
| Reference diagnostics     | 0      |
| rigor-rs diagnostics      | 0      |
| Matched                   | 0      |
| Coverage gaps (missing)   | 0      |
| Coverage %                | 100.0% |
| **False positives**       | **0**  |

### Corpus 3 — mastodon/app/models
| Metric                    | Value |
|---------------------------|-------|
| Files scanned             | 60    |
| Reference diagnostics     | 36    |
| rigor-rs diagnostics      | 27    |
| Matched                   | 26    |
| Coverage gaps (missing)   | 10    |
| Coverage %                | 72.2% |
| **False positives**       | **1** |

#### False positive detail

| Field        | Value                                             |
|--------------|---------------------------------------------------|
| File         | `mastodon/app/models/async_refresh.rb`            |
| Location     | line 73, col 7                                    |
| Rule         | `call.undefined-method`                           |
| Message      | `undefined method 'to_json' for Hash`             |
| Method       | `to_json`                                         |
| Receiver     | `Hash`                                            |

**Root cause:** rigor-rs's RBS index (`crates/rigor-index/src/rbs.rs`) only
loads files from `core/*.rbs`.  `Hash#to_json` is defined in the `json` stdlib
extension RBS (`stdlib/json/0/json.rbs`), which adds `to_json` to Hash, Array,
Integer, Float, String, NilClass, TrueClass, FalseClass, Symbol, etc.  Because
that file is never loaded, the index considers `Hash#to_json` absent and emits a
false positive.  The reference loads the full `stdlib/` tree and correctly
suppresses the diagnostic.

**Fix direction:** add `stdlib/json/0/json.rbs` (or the full `stdlib/` dir) to
the `CURATED_FILES` list in `crates/rigor-index/src/rbs.rs`, OR expand the
curation strategy to include known stdlib extensions that patch core types.

### Grand totals

| Metric                    | Value |
|---------------------------|-------|
| Total files scanned       | 115   |
| Total ref diagnostics     | 55    |
| Total rigor-rs diags      | 38    |
| Matched                   | 37    |
| Coverage gaps (missing)   | 18    |
| Grand coverage %          | 67.3% |
| **Total false positives** | **1** |
