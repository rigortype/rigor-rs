# Own the index layer; reuse only the parser. Rubydex is an optional accelerator

Status: accepted (supersedes this ADR's own initial draft — see Revision history)

rigor-rs **owns its [index layer](../../CONTEXT.md)** in Rust, behind a rigor-rs-owned trait: declaration discovery, ancestor linearization (with visibility), method/constant resolution, and refinement/generic resolution. It reuses external Rust crates **only for parsing** — `ruby-prism` for Ruby ([ADR-0003](0003-prism-rust-bindings.md)) and `ruby-rbs` for RBS syntax (supplementing it where needed). The **[inference engine](../../CONTEXT.md)** (type lattice, RBS method-type translation, flow-sensitive inference, narrowing, typed dispatch, extended annotations) is owned by rigor-rs as before.

**[Rubydex](../../CONTEXT.md) is demoted to an optional accelerator**: it may be adopted behind the same index trait *only if* a spike proves it exposes populator-grade detail. It is not the foundation.

## Why own the index (the pzoom precedent)

The closest precedent to rigor-rs is **pzoom** — Psalm's author porting his own PHP type checker to Rust, reusing Mago's parser. pzoom deliberately **built its own code-info / populator** (`pzoom-code-info`) rather than delegating it, because that layer encodes analysis-specific semantics (ancestor linearization with visibility, trait flattening, template/generic substitution, property sealing) that an external indexer does not expose. Mago supplied only *parsing*.

This converges with the one objection from Ruby Rigor's [ADR-21](../../../ruby/rigor/docs/adr/21-rubydex-evaluation.md) that survives under a Rust host: Rubydex exposes parameter *shape*, not typed method definitions — and that gap falls exactly on the index/populator layer the inference engine depends on. Owning the index also maximizes [diagnostic-set parity](0002-diagnostic-set-parity.md) control: the populator's semantics must match the reference implementation's environment exactly, and we don't want parity coupled to Rubydex's early-stage, churny, session-rebuild model.

We still avoid writing an RBS *syntax* parser from scratch by reusing `ruby-rbs` — the narrow, well-defined reuse that is safe.

## The reuse boundary

The decision of what to reuse versus own follows one principle, made vivid by **pylyzer** (a Python checker that ran Python through Erg's inference engine via a `py2erg` transpile, then was abandoned for the purpose-built ty):

> **Reusing a parser or type *data* is safe; reusing a foreign inference *engine* is fatal.** A parser (`ruby-prism`) and an RBS parser (`ruby-rbs`) are pure syntax/data with no inference semantics baked in — reusing them is data plumbing. An inference engine designed for another language carries that language's semantics and assumptions; bending it to a different language ("engine parasitism") accrues exponential impedance and a correctness ceiling.

rigor-rs therefore reuses parsers and RBS data freely, owns the index and inference engine, and treats Rubydex strictly as an index/data accelerator behind a trait — never as the engine.

## Verification spike (gate)

1. **`ruby-rbs` exposes typed method definitions** (return types, parameter types, variance) from its parse AST — needed regardless, since rigor-rs owns the type translation. If not, add a thin type-extraction layer over its AST (still far smaller than a from-scratch RBS parser).
2. **Only if considering Rubydex as accelerator**: does it expose populator-grade detail (typed method defs, ancestor linearization with visibility, refinement/generic hooks) and build/link cleanly against the Ruby 4.0 corpus? If not, skip Rubydex entirely.

## Considered options

- **Delegate the index layer to Rubydex** (this ADR's initial draft) — demoted to optional accelerator: pzoom shows the index/populator needs analysis-specific detail external indexers don't expose; the surviving ADR-21 objection hits this layer; parity control favors owning it.
- **Write an RBS syntax parser from scratch** — rejected: reusing `ruby-rbs` is safe and narrow.
- **Embed Ruby / FFI to the `rbs` gem** — rejected: contradicts single-binary, Ruby-free distribution ([ADR-0001](0001-rust-reimplementation-strategy.md)).

## Note on the ADR-21 inversion (still valid, just not decisive)

The reasons Ruby Rigor's ADR-21 objections invert under Rust (native-to-native crate use, no Ractor constraint, greenfield with no cache to preserve) remain true — which is why Rubydex stays a *candidate accelerator* rather than being rejected outright. They were simply not strong enough to outweigh the pzoom precedent and the parity-control argument for owning the index.

## Revision history

- (initial draft, this session) Decided to **delegate** the index layer to Rubydex, re-evaluating Ruby Rigor's ADR-21 under a Rust host (its objections invert).
- (revised, this session) **Flipped to owning the index**, reusing only the parser (`ruby-rbs`), after analyzing pzoom — the closest precedent — which deliberately owned its code-info/populator. Rubydex demoted to an optional accelerator gated on a sharper spike.
