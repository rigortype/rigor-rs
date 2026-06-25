# Value lattice carriers and the Dynamic[T] algebra

Status: accepted

The `Type` enum carries exactly the value-lattice members the reference defines, and the `Dynamic[T]` join/meet/difference **algebra** is ported as the reference's normative spec. This is a [parity surface](../../CONTEXT.md): the carrier set and the dynamic-origin algebra come verbatim (in meaning) from [value-lattice](../../../../ruby/rigor/docs/type-specification/value-lattice.md), [special-types](../../../../ruby/rigor/docs/type-specification/special-types.md), and [types.md](../../../../ruby/rigor/docs/types.md); only the Rust representation is ours.

## Carrier set

The single interned `enum Type` ([ADR-0005](0005-rust-architecture.md)) MUST represent every carrier:

- `top`, `bot`
- `Nominal[Class]` / `Nominal[Class[args]]` (applied generic arguments)
- `Constant[value]` — value-pinned; the literal payload is kept inline, the carrier interned ([ADR-0005](0005-rust-architecture.md): intern identifiers/symbols, store literal values inline)
- `Tuple` (per-position array shape)
- `HashShape` (record; distinguishes key-absent from key-present-with-nil)
- `IntegerRange` / `int<min, max>`
- `Refined` (predicate subset)
- `Difference` (point removal), `Intersection`, `Complement ~T`
- `Proc`, object-shape
- `App[uri, args]` — lightweight HKT, defunctionalised type constructor (ref reference [ADR-20](../../../../ruby/rigor/docs/adr/20-lightweight-hkt.md))
- `DataClass` / `DataInstance` — member-shape carriers (ref reference [ADR-48](../../../../ruby/rigor/docs/adr/48-data-struct-value-folding.md))
- result markers `void`, `self`, `instance`, `class`
- `Union`
- `Dynamic[T]`

Add carriers reluctantly and compose existing ones first ([ADR-0005](0005-rust-architecture.md)), but the set above is the floor the lattice must cover for parity.

## The Dynamic[T] algebra (normative)

`untyped` is deliberately outside the ordinary lattice; `untyped = Dynamic[top]`. Joins, meets, and differences preserve the wrapper rather than pretending the value is purely static:

```text
Dynamic[A] | Dynamic[B] = Dynamic[A | B]
T | Dynamic[U]          = Dynamic[T | U]      # dynamic infects unions
Dynamic[T] & U          = Dynamic[T & U]
Dynamic[T] - U          = Dynamic[T - U]
```

Generic slots preserve the wrapper: `Array[untyped]` is internally `Array[Dynamic[top]]`, and reading an element yields `Dynamic[top]` (not `top`). The wrapper is reversible at the RBS boundary — `Dynamic[top]` round-trips to `untyped` and preserved slots round-trip with the same shape — which is what makes RBS→Rigor lossless. Subtyping against a `Dynamic[T]` uses `T`; gradual consistency governs the boundary crossing ([ADR-0018](0018-certainty-and-relations.md)).

## Provenance is a side-channel, never a carrier field

Dynamic **provenance** — the 5-cause taxonomy `external-gem-without-rbs` / `framework-dsl-boundary` / `analyzer-budget-cutoff` / `explicit-untyped` / `unsupported-syntax` (ref reference [ADR-75](../../../../ruby/rigor/docs/adr/75-dynamic-provenance.md)) — is recorded in a **scope-side identity map** keyed on the introduction site, NEVER as a field on the `Dynamic` carrier. Two values that are the same type but became dynamic by different routes MUST remain `==` and dedup in unions and cache keys, so provenance is excluded from the carrier's equality, hash, and cache key. It is precision-additive (surfaced through coverage labels and JSON metadata) and fires no diagnostic and no severity on its own.

## Rust representation

Each carrier is a `Type` variant, interned in the arena and handled via exhaustive `match` in the lattice ops; unions/intersections are built through the normalizing builder ([ADR-0020](0020-normalization-and-determinism.md)) so the `Dynamic`-infection rules above are applied at construction. The provenance map is a `Scope`-side identity-keyed table excluded from `Type` (and `Scope`) equality/hash.

## Considered options

- **Add a `provenance` field to the `Dynamic` carrier** — rejected: breaks the `Dynamic[T]` value-equality that load-bearing union/cache dedup depends on, and forks the lattice by origin.
- **Normalize `untyped` to `top` and drop the wrapper** — rejected: loses gradual-boundary tracking and breaks lossless RBS round-tripping.
- **Reimplement the carrier set and dynamic algebra independently with a reasonable-but-different design** — rejected because this is a parity surface; diagnostics render these carriers and the algebra determines join/meet results, so any divergence is a parity break ([ADR-0002](0002-diagnostic-set-parity.md)).
