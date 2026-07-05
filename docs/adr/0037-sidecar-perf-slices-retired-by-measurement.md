# Sidecar batching / on-disk cache retired by measurement; plugin invocation is the value

Status: accepted

[ADR-0008](0008-real-ruby-sidecar.md) sketched a Ruby sidecar with three cost-
oriented features beyond basic folding: **batched IPC** ("one round-trip per
file's worth of foldable calls"), **length-prefixed MessagePack** framing, and a
**two-level (in-memory + on-disk) cache**. After landing the sidecar (Slices 1–2,
[ADR-0036](0036-ruby-sidecar-default-reversal.md) phase b), those were queued as
Slices 3–4. Measurement against the real `rigor-survey` corpora retires them.

## Evidence

`rigor check` full fidelity vs `--ruby=off` (sound subset), release build, warm:

| corpus | files | full | no-ruby | Δ | diag full vs subset |
|---|---|---|---|---|---|
| kramdown/lib | 55 | 0.11s | 0.04s | 0.07s | 0 = 0 |
| mastodon/app/models | 248 | 0.10s | 0.04s | 0.06s | 109 = 109 |
| algorithms | 548 | 0.15s | 0.08s | 0.06s | 1561 = 1561 |
| liquid/lib | 63 | — | — | — | 0 = 0 |

Two facts:

1. **The full-vs-subset time delta is FLAT (~0.06s) across 55→548 files.** It does
   not scale with file count, so it is the fixed sidecar *spawn+handshake* cost —
   **not** per-call folding IPC. Constant folds fire only on *value-pinned literal*
   receivers (`255.to_s(16)`), which are rare in real code (method calls are
   overwhelmingly on variables, which don't pin), so the per-file fold volume is
   near zero. Batching optimizes a bottleneck that does not exist at these scales.

2. **The diagnostic set is IDENTICAL full vs subset on every corpus.** The
   constant-folding sidecar, though correct, changes *no* diagnostics on real code:
   even when a tail fold does fire, a folded `String` and the nominal `String` have
   the same method surface for `call.undefined-method`, so witnessing is unchanged.

## The decision

- **Slice 3 (batched IPC + MessagePack): retired.** Per-call folding overhead is
  negligible and does not scale; the in-memory memo already collapses repeats.
  Building a two-pass collect-then-resolve inference refactor (or per-thread
  workers) + a MessagePack codec to optimize a non-bottleneck is premature. The
  v1 newline-JSON per-call transport stays.
- **Slice 4 (on-disk cache): retired.** It would cache an already-cheap (~0.06s)
  per-run cost; the cross-run saving does not justify a content-addressed disk
  cache and its invalidation surface (a real soundness-risk area).
- **Reorientation:** the sidecar's parity value is **plugin target-library
  invocation** (Slice 5), exactly ADR-0008's founding premise ("reaching parity —
  especially in the plugin phase — requires executing real Ruby"). Constant
  folding via the sidecar is precision-additive on rare literal constructs and the
  *substrate* plugin invocation reuses — not, by itself, a diagnostic-moving
  feature on real apps. Remaining sidecar effort should target Slice 5 and
  allowlist growth driven by real signal, not the perf slices.

The ~0.06s eager spawn under `require`/`auto` is acceptable; a cheap future
refinement is to lazy-spawn under `auto` (ADR-0008's "spawned lazily" — skip the
process entirely when a run has no sidecar-foldable call), which `require` cannot
do because its upfront exit-69 probe (ADR-0036) needs the handshake.

## Considered options

- **Build batching + MessagePack as planned** — rejected: measurement shows no
  bottleneck to relieve; it is complexity + FP risk (a two-pass inference walk) for
  no measured gain.
- **Build the on-disk cache** — rejected: caches a ~0.06s cost; invalidation risk
  outweighs the saving.
- **Micro-optimize spawn now** — deferred: ~0.06s is not worth it; revisit with
  lazy-spawn-under-auto if spawn cost ever matters.

## Revisiting

Re-open Slice 3/4 only if a real workload demonstrates fold-heavy scaling cost
(a project where the full-vs-subset delta grows with size, i.e. many pinned-
literal folds) — not before. Slice 5 (plugin invocation) and reference-verified
allowlist growth are the live sidecar work.
