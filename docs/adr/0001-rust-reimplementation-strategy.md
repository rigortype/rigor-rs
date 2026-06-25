# rigor-rs is a phased, full-parity Rust reimplementation of Rigor

Status: accepted

rigor-rs reimplements **Rigor** (the Ruby type-aware bug finder at `/Users/megurine/repo/ruby/rigor`, the [reference implementation](../../CONTEXT.md)) in Rust. The primary driver is **performance and distribution**: low startup latency, higher analysis throughput, and a single static binary whose **core analysis** needs no Ruby runtime — the properties that matter most for editor (LSP) and CI usage. (Full-fidelity constant folding and plugin target-library invocation use an optional cached Ruby sidecar; the core binary runs standalone and degrades gracefully without it — see [ADR-0008](0008-real-ruby-sidecar.md).)

The end goal is **full parity in phases, eventually replacing the Ruby version**. The first phases target the core analyzer (`check` / `annotate` / `type-of`); plugins, LSP, MCP and sig-gen follow in later phases. Until parity is reached, the Ruby version remains the authoritative reference.

## Scope of "full parity"

"Full parity" means parity with **Rigor's diagnostic behaviour**, not with full Ruby execution semantics. Rigor is itself a static, zero-false-positive analyzer that already draws pragmatic boundaries: it treats `Dynamic` as a first-class escape hatch rather than an error, and it handles metaprogramming through plugins, not by executing code. rigor-rs **inherits those boundaries**. Concretely, Ruby's runtime semantics — `eval`, runtime `method_missing`, runtime refinements, `Binding` capture, exact encoding behaviour — are out of scope exactly as they are for Rigor, and `Dynamic` stays a first-class escape hatch.

This boundary is what keeps "full parity" from becoming the boil-the-ocean trap that stalled comparable projects: artichoke (a Ruby implementation in Rust) spent 6 years and 7,500 commits without running most Ruby, because *implementing Ruby* has unbounded surface area; pylyzer (a Python checker on a foreign inference engine) hit a correctness ceiling and was abandoned for a purpose-built tool. rigor-rs mirrors a static analyzer (Rigor), not an interpreter, and delegates stdlib semantics to RBS — so its target is bounded by Rigor's own scope, and success is measured by matching Rigor's diagnostics, not by Ruby-semantics coverage.

## Considered options

- **Permanent fast core, coexisting with Ruby Rigor** — rejected: we want a single eventual implementation, not an indefinite split.
- **A learning/experimental subset** — rejected: this is a serious migration with a real parity bar.
- **A native extension that speeds up only hot paths via FFI** — rejected: does not deliver the single-binary, Ruby-free distribution that motivates the project.

## Known risk: metaprogramming and the plugin phase

pzoom's author (porting Psalm to Rust) found that for dynamic languages the hard adoption blocker is **runtime metaprogramming** that static analysis cannot observe — handled in practice by a plugin system. Ruby is metaprogramming-heavy (Rails especially), and the reference implementation ships ~31 plugins precisely for this. rigor-rs's phasing puts the core analyzer first and plugins later, so it will be weak on real Rails/DSL code until the plugin phase lands. This is an accepted, eyes-open consequence of the phasing, not a reason to change it — but the plugin system is not optional for eventual adoption, and the phase order should be communicated as such.
