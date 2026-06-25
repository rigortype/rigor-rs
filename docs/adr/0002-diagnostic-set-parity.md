# Correctness bar: diagnostic-set parity, verified by a differential harness

Status: accepted

The migration's correctness bar is **[diagnostic-set parity](../../CONTEXT.md)**: for a given input, the set of `(rule id, location)` pairs rigor-rs emits must match the [reference implementation](../../CONTEXT.md). Message wording is allowed to diverge (and improve); the *set* of diagnostics must not. This preserves Rigor's signature zero-false-positive bar while leaving room to improve message quality.

Parity is measured by a **[differential harness](../../CONTEXT.md)** that runs rigor-rs and the reference implementation over the same corpus (including the reference's existing spec/fixture suite) and compares their diagnostic sets. The reference implementation is the oracle — qualified by [ADR-0011](0011-reference-oracle-exceptions.md): discovered reference defects are reported upstream and excused via a divergence registry, not reproduced bug-for-bug.

## Mechanism: pinned-reference snapshots

The reference and rigor-rs live as **sibling repositories** (`/repo/ruby/rigor`, `/repo/rust/rigor-rs`). Rather than run the Ruby reference on every CI run, the harness generates **expected-diagnostics snapshots**: a pinned reference (fixed `rigor` SHA, rbs 4.0.2, prism 1.9.0) is run once over the corpus with `--format json`, and its diagnostic sets are committed as JSON snapshots. CI compares rigor-rs's output against the committed snapshots — fast, reproducible, and free of a Ruby runtime in CI. A refresh task re-runs the pinned reference to regenerate snapshots when the reference version is intentionally bumped.

This keeps the oracle fixed, CI fast, and snapshot updates explicit. The alternative — running the live reference every time — was rejected as slow and as imposing a Ruby toolchain on rigor-rs's CI.

## Considered options

- **Byte-for-byte output identity** — rejected: over-constrains; locks in message wording we may want to improve and couples us to exact formatting/SARIF byte layout.
- **"Spiritually equivalent" (catch the same class of bug, no per-diagnostic match)** — rejected: too weak to drive a parity migration or to use the existing fixture suite as an oracle.
