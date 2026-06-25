# Content-addressed persistent analysis cache for fast cold starts

Status: accepted

rigor-rs persists core analysis results in a **content-addressed cache** under `.rigor/cache` (honouring the reference's `cache.path`, [ADR-0009](0009-config-baseline-compatibility.md)): the parsed RBS environment (constant table, ancestor tables, known class names) and per-file inference snapshots, keyed by a hash of each file's content plus the RBS and configuration it depends on. A repeated run on an unchanged SHA — the common CI case — reuses cached results instead of re-parsing RBS and re-inferring, directly serving the performance driver ([ADR-0001](0001-rust-reimplementation-strategy.md)). The cache is bounded by LRU eviction (mirroring the reference's ADR-54 and its default cap).

This concretizes [ADR-0006](0006-incremental-computation.md)'s "file-level cache": the pure query functions' outputs (kept Salsa-ready, not Salsa-bound) are stored content-addressed. It sits in a different layer from the sidecar cache ([ADR-0008](0008-real-ruby-sidecar.md), which memoizes real-Ruby folding/plugin calls) and from in-session incremental recompute (ADR-0006) — this one persists *core* analysis across process invocations.

Soundness rests on content-addressing: an entry is keyed by everything that determines it (source content + dependent RBS + config), so a stale entry cannot be silently reused.

## Considered options

- **In-memory-only cache** — rejected: no cross-run reuse; CI and repeated local runs re-pay full analysis every time, against the performance driver.
- **Defer until profiling** — rejected: the reference already shows this cache is load-bearing (its ADR-6 / ADR-54), and config compatibility ([ADR-0009](0009-config-baseline-compatibility.md)) already requires reading `cache.path`.
