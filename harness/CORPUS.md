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
| `REFERENCE_RIGOR_DIR` | `/Users/megurine/repo/ruby/rigor`           | Path to the Ruby rigor checkout        |
| `RIGOR_RS_BIN`        | `target/debug/rigor` (under repo root)      | Path to the rigor-rs binary            |

The binary is auto-built if absent (`cargo build --offline -p rigor-cli`).

## Corpora (built-in)

| # | Label                    | Directory                                           | Cap |
|---|--------------------------|-----------------------------------------------------|-----|
| 1 | `rigor/examples`         | `/…/ruby/rigor/examples`                            | 80  |
| 2 | `rigor/lib/rigor/type`   | `/…/ruby/rigor/lib/rigor/type`                      | 80  |
| 3 | `mastodon/app/models`    | `/…/ruby/mastodon/app/models`                       | 60  |

### Corpus 1 — rigor/examples (32 files)
The reference implementation's own demo projects (`rigor-web`, `rigor-units`,
`rigor-deprecations`, `rigor-pattern`, `rigor-routes`, `rigor-lisp-eval`).
These are real idiomatic Ruby that exercises the public Rigor plugin API.

### Corpus 2 — rigor/lib/rigor/type (23 files)
The reference's own type-carrier source code.  Clean, zero-diagnostic Ruby — a
strong false-positive stress test.

### Corpus 3 — mastodon/app/models (60 files)
The first 60 `*.rb` files (alphabetical) from Mastodon, a production Rails
application.  Real-world mixed-paradigm Ruby (ActiveRecord, ActiveSupport,
modules, concerns, etc.).

## Run results (2026-06-26)

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
