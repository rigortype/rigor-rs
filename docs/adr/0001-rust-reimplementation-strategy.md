# rigor-rs is a performance-oriented Rust prototype of Rigor, coexisting with the Ruby mainstream

Status: accepted

rigor-rs reimplements **Rigor** (the Ruby type-aware bug finder at `/Users/megurine/repo/ruby/rigor`, the [reference implementation](../../CONTEXT.md)) in Rust. The primary driver is **performance and distribution**: low startup latency, higher analysis throughput, and a single static binary whose **core analysis** needs no Ruby runtime — the properties that matter most for editor (LSP) and CI usage. (Full-fidelity constant folding and plugin target-library invocation use an optional cached Ruby sidecar; the core binary runs standalone and degrades gracefully without it — see [ADR-0008](0008-real-ruby-sidecar.md).)

**rigor-rs is a prototype, not a replacement.** The Ruby implementation is the **mainstream** and **always leads development**; rigor-rs tracks it for performance and is **not** intended to replace it. Full compatibility with the Ruby version is **not yet verified** and is **not a committed goal** — reaching diagnostic-set parity, and one day syncing as a behaviour-preserving Rust implementation, is a *possibility*, not a plan. Until and unless that happens, the two coexist and the Ruby version remains authoritative.

## Scope of "parity" (what rigor-rs aims to match, where it implements a rule)

"Parity" here means parity with **Rigor's diagnostic behaviour**, not with full Ruby execution semantics. Rigor is itself a static, zero-false-positive analyzer that already draws pragmatic boundaries: it treats `Dynamic` as a first-class escape hatch rather than an error, and it handles metaprogramming through plugins, not by executing code. rigor-rs **inherits those boundaries**. Concretely, Ruby's runtime semantics — `eval`, runtime `method_missing`, runtime refinements, `Binding` capture, exact encoding behaviour — are out of scope exactly as they are for Rigor, and `Dynamic` stays a first-class escape hatch.

This boundary is what keeps a parity effort from becoming the boil-the-ocean trap that stalled comparable projects: artichoke (a Ruby implementation in Rust) spent 6 years and 7,500 commits without running most Ruby, because *implementing Ruby* has unbounded surface area; pylyzer (a Python checker on a foreign inference engine) hit a correctness ceiling and was abandoned for a purpose-built tool. rigor-rs mirrors a static analyzer (Rigor), not an interpreter, and delegates stdlib semantics to RBS — so its target is bounded by Rigor's own scope, and where it implements a rule, success is measured by matching Rigor's diagnostics, not by Ruby-semantics coverage.

## Coexistence, not re-unification

rigor-rs is a performance prototype that **coexists** with the Ruby mainstream. There is **no planned retirement of Ruby Rigor** and **no commitment to make rigor-rs the single implementation** — the Ruby version leads, and replacing it is not planned. The double-maintenance of tracking a fast-moving reference is accepted only to the extent the prototype is actively pursued; it is **not** justified by an eventual single-implementation payoff. If full diagnostic-set parity is ever reached (the [divergence registry](0011-reference-oracle-exceptions.md) empty across the OSS parity corpus) and the plugin / LSP / MCP phases ship, **synchronising the two — or letting the Rust build stand in for some uses — becomes an option to evaluate then, not a commitment now.** The canonical upstream for the reference itself, and for divergence reports, remains `rigortype/rigor`.

## Considered options

- **Performance prototype coexisting with the Ruby mainstream (chosen)** — a fast, standalone Rust port that tracks Ruby Rigor for performance, with the Ruby version remaining authoritative and leading development; full parity and eventual sync are possibilities, not commitments.
- **A phased march to replace Ruby Rigor with a single Rust implementation** — not pursued: the maintainers keep Ruby as the mainstream; replacing it is explicitly not planned.
- **A native extension that speeds up only hot paths via FFI** — rejected: does not deliver the single-binary, Ruby-free distribution that motivates the project.

## Known risk: metaprogramming and the plugin phase

pzoom's author (porting Psalm to Rust) found that for dynamic languages the hard adoption blocker is **runtime metaprogramming** that static analysis cannot observe — handled in practice by a plugin system. Ruby is metaprogramming-heavy (Rails especially), and the reference implementation ships ~31 plugins precisely for this. rigor-rs's phasing puts the core analyzer first and plugins later, so it is weak on real Rails/DSL code until the plugin phase lands. This is an accepted, eyes-open consequence of the phasing — and, because rigor-rs is a prototype that coexists with the Ruby mainstream rather than a replacement, the gap is a limitation of the prototype, not a blocker for users (the Ruby version remains available and authoritative).
