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
| `RIGOR_RS_BIN`        | `target/release/rigor` (under repo root)    | Path to the rigor-rs binary. Same default in `run_corpus.rb`, `fp_audit.py` and `gap_census.py` |

### Which binary IS rigor-rs

**`target/release/rigor`, for every corpus-scale tool** — `run_corpus.rb`,
`fp_audit.py`, `gap_census.py`. Release is what every recorded sweep number was
measured with, and the sweep runs 9204 files through both implementations, where
a debug build costs several times more. The fixture harness (`harness/run.rb`,
`harness/lib.rb` — 76 tiny files in an edit-run loop) deliberately stays on
`target/debug/rigor`; it is a different loop with different economics.

`run_corpus.rb` used to default to debug while `fp_audit.py` defaulted to
release, so the two corpus entry points disagreed about which file *is*
rigor-rs — and nothing rebuilt or dated the release one. A six-day-old release
binary was measured silently, and two merged slices read as "closed nothing"
([note](../docs/notes/20260807-fp-audit-port-side-blind-spots.md)). All three
tools now share one contract:

* the binary is **auto-built when absent** (`cargo build --offline --release -p
  rigor-cli`) — unless `RIGOR_RS_BIN` was set explicitly, which is then required
  to exist;
* its **path and build time are printed in the run header**, so the measured
  binary is in the transcript rather than reconstructed days later;
* a binary under `target/` that is **older than the newest file under
  `crates/`** is REFUSED (exit non-zero, nothing measured). An explicit
  `RIGOR_RS_BIN` outside the repo is honoured as a deliberate choice; its
  staleness is not checked, and the header says so.

### A failed run is never a pass

False positives are `rigor-rs − reference`, so an empty *port* result makes the
FP count 0 by construction; an empty *reference* result turns all of rigor-rs's
output into phantom FPs. Both sides therefore report a batch failure as
**INVALID** — never as 0 — and any invalid comparison makes the whole run exit
non-zero, with `TOTAL FP candidates` marked `INCOMPLETE`.

What counts as a port-side failure: `rigor check` exits **1 iff at least one
ERROR-severity diagnostic was emitted**, else 0 — so a warning-only batch exits
0 *with* diagnostics on stdout. The failure signals are therefore an exit code
outside `{0, 1}` (64 usage, 101 panic, 127 not-found …) and stdout that is not a
JSON array. Exit code versus diagnostic-list emptiness is deliberately **not**
compared: warnings are a parity severity, so such a rule marks healthy
warning-only corpora INVALID, and it earns nothing — empty stdout already fails
the JSON parse.

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

## Standing sweep-set baseline (2026-08-09)

`python3 harness/fp_audit.py --gaps --sweep`, reference pinned at `v0.3.2`,
vendored rbs 4.1.1. **9204 files, 0 FP candidates, 841 coverage gaps.** Coverage
gaps are the sound-subset side of ADR-0002 and are expected:

| corpus | files | coverage gaps | (was, `v0.3.1`) |
|---|---|---|---|
| mastodon/app | 1236 | 13 | 48 |
| gitlab-foss/lib | 4676 | 170 | 328 |
| survey/mail | 874 | 439 | 540 |
| survey/Ruby | 192 | 30 | 30 |
| survey/dependabot-core | 1650 | 81 | 81 |
| survey/concurrent-ruby | 345 | 80 | 86 |
| survey/net-ssh | 180 | 26 | 75 |
| survey/haml/lib | 51 | 2 | 5 |

The `v0.3.1` column is the 2026-07-31 baseline (1193 gaps; 1125 by the time of
the 08-08 slices). Almost all of the drop is upstream RETRACTING diagnostics, not
the port gaining coverage: `v0.3.2`'s #297 requires a nameable non-nil arm before
a possible-nil witness, which is the position
[Tier B/C](../docs/notes/20260717-tier-bc-track-closed.md) had already argued.
Read the total as "what the oracle now claims", not as progress.
[pin note](../docs/notes/20260809-repin-v032.md).

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
