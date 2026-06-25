# Plugins behind a Rust trait: Ruby plugins hosted in the sidecar by default, ported to native Rust over time

Status: accepted

The reference ships 31 bundled plugins (plus a third-party ecosystem) that produce facts, synthesize RBS / synthetic method nodes for DSL-generated methods, run custom node rules, share a cross-plugin fact store (reference ADR-9), and occasionally invoke real target-library methods (ADR-39). A plugin is a **fact producer / RBS synthesizer, not an inference engine** — so hosting the real Ruby plugin does not cross the reuse boundary of [ADR-0004](0004-own-the-index-layer.md); it is bounded data computation, like [ADR-0008](0008-real-ruby-sidecar.md).

rigor-rs models plugins behind a **Rust trait**: a plugin takes file paths + config and returns facts / synthetic RBS / diagnostics, which the rigor-rs-owned inference engine consumes. The trait has two implementation kinds:

- **Sidecar-hosted Ruby plugin (the default).** The real Ruby plugin runs in the Ruby sidecar (ADR-0008, extended from pure-method execution to plugin hosting). This gives diagnostic-set parity for free — it *is* the reference's own plugin — and **preserves the third-party Ruby plugin ecosystem permanently**.
- **Native Rust plugin.** Bundled plugins are ported to native Rust over time, hottest-first (the Rails-family plugins), recovering performance. Their target-library invocations (e.g. inflection) still route through the sidecar per ADR-0008 / ADR-39.

This is a strangler-fig migration: start with all plugins hosted as Ruby (instant compatibility + parity, no upfront 31-plugin port — avoiding the artichoke trap), then port bundled plugins to Rust where speed matters. Plugin outputs (facts, synthetic RBS, diagnostics) are **cached** keyed by input file content + plugin version (the ADR-0008 cache model), so plugin-heavy (Rails) analyses pay the Ruby cost once.

The cross-plugin fact store is a rigor-rs-owned typed channel that both Rust-native and sidecar-hosted plugins publish to and read from, so a plugin's host (Ruby vs Rust) is invisible to its consumers.

Degradation follows ADR-0008: with no project Ruby available, sidecar-hosted plugins decline (their checks go silent, zero false positives preserved); Rust-native plugins still run (their non-target-library logic needs no Ruby).

## Considered options

- **Reimplement all 31 bundled plugins in Rust upfront, drop Ruby plugins** — rejected: a huge upfront port (artichoke trap), discards the third-party ecosystem, and leaves early phases with no Rails support.
- **Host every plugin in the Ruby sidecar permanently, never port to Rust** — rejected: maximal compatibility and least code, but plugin-using analysis always needs Ruby and never recovers the performance win for Rails-heavy projects.

## Relationship to other ADRs

- Extends [ADR-0008](0008-real-ruby-sidecar.md): the sidecar hosts whole Ruby plugins, not just pure target-library calls.
- Consistent with the reuse boundary of [ADR-0004](0004-own-the-index-layer.md): plugins are fact producers; the inference engine stays rigor-rs's own.
- Phased per [ADR-0001](0001-rust-reimplementation-strategy.md): plugins are a later phase; the strangler order ports hottest bundled plugins first.
