# Real-Ruby execution via a cached sidecar for constant folding and plugin invocation

Status: accepted

## Background

The reference implementation reaches its precision by **executing real Ruby** in two places:

- **Constant folding** — it calls the real `Integer#+`, `String#upcase`, `(1..10).first(3)`, etc. on literal values it constructed, gated by a purity allowlist + a method catalogue + a result check. Non-deterministic methods (`Array#sample`, `Object#hash`) are never folded.
- **Plugin target-library invocation** ([ADR-39](../../../../ruby/rigor/docs/adr/39-plugin-target-library-invocation.md)) — plugins call their target library's pure, allow-listed methods (e.g. `ActiveSupport::Inflector.pluralize`), **declining (→ silence) rather than approximating** when the library is absent, because an approximation that diverges from the library's real rules is a false positive.

Because an approximation is by design a false positive, reaching [diagnostic-set parity](0002-diagnostic-set-parity.md) — especially in the plugin phase — **requires executing real Ruby**. Reimplementing inflection (or other target-library facts) in Rust would be exactly the approximation ADR-39 forbids. The analyzed application's *own* code is never executed (the reference's ADR-2 line; unchanged here).

## Decision

rigor-rs executes real Ruby through an optional, cached **[Ruby sidecar](../../CONTEXT.md)**, on a hybrid boundary:

- **Foldability is decided in Rust** from the purity allowlist + catalogue, embedded/vendored like the RBS stdlib ([ADR-0007](0007-rbs-stdlib-shipping.md)). Ruby is needed only to *execute* a confirmed-foldable call, never to *decide* foldability.
- **Rust-native folding** covers the conservative core where byte-exact agreement with Ruby is trivially guaranteed: integer arithmetic/bitops/comparisons, boolean/nil logic, symbol equality, simple ASCII string ops.
- **The sidecar** executes everything else (Float formatting, encoding-sensitive String, Rational/Complex, Date/Time, Regexp, `String#%`) and **all** plugin target-library invocations — guaranteeing parity via real Ruby and avoiding a reimplementation of subtle Ruby semantics (the artichoke trap, [ADR-0001](0001-rust-reimplementation-strategy.md)).

### Sidecar architecture (mirrors the reference's ADR-39 `process` strategy)

- A **persistent Ruby worker**, spawned **lazily** (only on the first non-Rust-foldable or plugin call) under the **project's Ruby + bundle**, so plugin target gems resolve at the project's versions. Pure-Rust analyses spawn no Ruby.
- **Length-prefixed MessagePack IPC, batched** — one round-trip per file's worth of foldable calls, to amortize IPC.
- **Crash containment** — a worker crash (e.g. a target-gem segfault) is caught as EOF; the call declines (widen/silence) and the worker respawns.

### Caching (two-level, persistent)

Results are memoized in-memory per run **and** in a **content-addressed on-disk cache** keyed by `(operation, receiver literal, method, argument literals, Ruby version, relevant gem versions)`. Purity + determinism (guaranteed by the foldability gate) make the cache sound; it collapses real-Ruby calls across LSP sessions and repeated CI runs.

### Degradation

When no project Ruby/bundle is available, rigor-rs **degrades gracefully**: non-Rust folding widens to the nominal type, plugin invocations decline — preserving the zero-false-positive bar as a **sound subset** (some diagnostics missing, none wrong) — and emits a one-time "sidecar unavailable" notice. The core binary therefore runs standalone; **full parity requires the sidecar**, and the differential harness ([ADR-0002](0002-diagnostic-set-parity.md)) runs with it available, so parity is measured in the full configuration.

## Considered options

- **Reimplement all ~150–200 foldable methods (and inflection) in Rust, no sidecar** — rejected: Float/encoding/format byte-exactness is a heavy, error-prone reimplementation of Ruby semantics (artichoke trap), and reimplementing a plugin's target-library behaviour is precisely the approximation ADR-39 forbids (it produces false positives).
- **Route all folding through the sidecar (no Rust-native core)** — rejected: simplest and fully faithful, but forces a Ruby process for any literal-folding analysis, undercutting the standalone single-binary value; the conservative Rust core keeps the hot path Ruby-free.
- **In-memory-only cache** — rejected: no cross-run reuse; repeated CI/LSP runs re-pay every real-Ruby call.
- **Require the sidecar always (error when absent)** — rejected: discards the standalone-binary value and contradicts the reference's decline-to-silence discipline.

## Relationship to other ADRs

- **Qualifies [ADR-0001](0001-rust-reimplementation-strategy.md) / [ADR-0003](0003-prism-rust-bindings.md)**: the *core analyzer binary* stays Ruby-free; full-fidelity folding + plugin invocation use this optional sidecar.
- **Extends the reuse boundary of [ADR-0004](0004-own-the-index-layer.md)**: executing the real runtime / target library through a bounded harness is *data computation*, not "engine parasitism" — rigor-rs still owns its inference engine.
- **Mirrors the reference's ADR-39** (target-library invocation) and its constant-folding tier.

## Product positioning: standalone is a sound subset, not full parity (audit R1)

Standalone (no-sidecar) mode and full parity cannot both hold: without the sidecar, full-fidelity folding and plugins **decline**, so standalone analysis is a *sound subset* — it never emits a wrong diagnostic, but **silently misses some** the full configuration would catch. This is the unavoidable structural trade-off of a compiled port (the Pzoom lesson) — a positioning risk, not a soundness one. A naive competitor can market "pure Rust, no sidecar," look faster on a benchmark, and quietly miss bugs.

The defense is communication, surfaced *in the tool*, not buried in an ADR:
- the core binary emits a one-time **"sidecar unavailable → reduced coverage"** notice (already specified under § Degradation);
- `rigor doctor` (ADR-0031) reports sidecar availability and the resulting coverage posture as a first-class check;
- docs state plainly: **standalone = fast, sound, but incomplete; full parity needs the project's Ruby sidecar.**
