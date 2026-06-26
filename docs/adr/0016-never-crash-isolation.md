# Never crash a run: per-file panic isolation with partial results

Status: accepted

A run over a large codebase must never abort because one file is malformed or trips an internal bug. Each file's analysis is wrapped in `catch_unwind`; a panic (or other internal error) skips that file, is converted into an **internal-error diagnostic** for it, and the run continues over the rest. Parse errors use Prism's error recovery ([ADR-0003](0003-prism-rust-bindings.md)) — rigor-rs analyzes what parsed rather than aborting the file. rayon workers ([ADR-0006](0006-incremental-computation.md)) isolate per file, so one worker's panic never takes down the pool. Sidecar crashes are already contained ([ADR-0008](0008-real-ruby-sidecar.md)).

This matches the reference's never-crash + zero-false-positive discipline: a skipped file yields reduced coverage, never a wrong diagnostic and never a crashed run. Internal-error diagnostics are surfaced as upstream-report candidates (the [ADR-0011](0011-reference-oracle-exceptions.md) feedback path), since an internal error is usually a rigor-rs bug.

## Considered options

- **Fail-fast (stop on the first error)** — rejected: one malformed file halts analysis of an entire large codebase.
- **Let panics propagate (Rust default)** — rejected: one file's bug crashes the whole run, contradicting never-crash.

## The `internal-error` diagnostic (audit R5)

The synthetic diagnostic emitted for a skipped (panicked) file is a **deliberate rigor-rs-specific signal with no reference counterpart**, so it must not be treated as a parity false positive. It is emitted at **`:info` severity**, which the [differential harness](0002-diagnostic-set-parity.md) excludes from its error/warning comparison — a crashed file therefore never counts as a divergence. Rule id `internal-error`, `source_family` `builtin`. This keeps never-crash observable to the user without breaching the one-sided parity gate.
