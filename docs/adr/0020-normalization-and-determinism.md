# Normalization is a deterministic, port-faithful canonical form

Status: accepted

rigor-rs's type normalizer is a **faithful port** of the reference's ruleset, not an independently-reasonable Rust normalizer, and its output is **deterministic**: equivalent inputs MUST produce identical normalized outputs across runs and across analyzer instances. This is a [parity surface](../../CONTEXT.md): the rules come from [normalization](../../../../ruby/rigor/docs/type-specification/normalization.md); only the Rust representation is ours.

Determinism is itself normative because diagnostics render normalized types, caches key on them, and parity is defined over the resulting `(rule id, location)` sets ([ADR-0002](0002-diagnostic-set-parity.md)). A normalizer that produced an equivalent-but-differently-spelled canonical form would diverge on diagnostic text and on cache identity even while being "correct" — so the ruleset, not just the result, is ported.

## Rules that bite parity

The full list is in the spec; the non-obvious rules rigor-rs must reproduce exactly:

- Flatten nested union/intersection and drop duplicate operands.
- Drop `bot` from unions (`T | bot = T`); drop `top` from intersections (`T & top = T`).
- Expand `T?` to `T | nil` internally.
- **`1 | Integer` does NOT subsumption-collapse.** A value-pinned member records a reachable exact value with distinct provenance (a zero-iteration seed, a recursion base case); collapsing it into the co-member nominal base would erase that evidence from display and from value-aware consumers while buying nothing. Widening value-pinned members is the job of the explicit cap/budget rules, not of union construction.
- Collapse `true | false` to `bool` for **display only** — this never changes type identity.
- Preserve literal precision until a widening budget is exceeded, then widen to the nominal base.
- Preserve the `Dynamic` wrapper rather than normalizing `untyped` to `top`; normalize dynamic-origin unions/intersections/differences by transforming the static facet and keeping the wrapper (the algebra is in [ADR-0019](0019-value-lattice-and-dynamic-algebra.md)).
- `void | bot` collapses to `void` in result summaries (the `bot` path contributes no normal value).

## Rust representation

Unions and intersections are constructed exclusively through a **normalizing builder** ([ADR-0005](0005-rust-architecture.md)) over the interned lattice — never by assembling `Type` variants directly — so flattening, de-duplication, identity drops, and the `Dynamic`-wrapper rules are applied at construction. Operand ordering is a fixed, total, content-derived order so the canonical form is identical across runs and instances; display-only transforms (e.g. `bool`) live in the reporter, not in the canonical identity.

## Why this ADR matters

Normalization is the **weakest-ADR-footprint area in the reference** — it is largely spec-only, with no dedicated reference ADR. Recording the decision here (port the ruleset faithfully, treat determinism as normative) makes the parity obligation explicit on the rigor-rs side rather than leaving it implicit in the spec.

## Considered options

- **Spell a different but equivalent canonical form (e.g. subsumption-collapse `1 | Integer`, or sort operands by a convenient internal key)** — rejected: equivalent types would render and cache differently, breaking diagnostic-set and cache parity.
- **Reimplement normalization independently with a reasonable-but-different algorithm** — rejected because this is a parity surface; diagnostics are defined over normalized types ([ADR-0002](0002-diagnostic-set-parity.md)).
