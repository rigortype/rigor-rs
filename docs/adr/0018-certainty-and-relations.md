# Trinary certainty and two relations

Status: accepted

Relation and member queries return a **trinary certainty** (`yes`/`no`/`maybe`) paired with evidence, never a bare `bool`, and rigor-rs keeps **two distinct relations** — subtyping `<:` and gradual consistency `consistent(A, B)` — exactly as the reference specifies. This is a [parity surface](../../CONTEXT.md): the normative semantics are ported faithfully from [relations-and-certainty](../../../../ruby/rigor/docs/type-specification/relations-and-certainty.md) (and the special-types rules in [special-types](../../../../ruby/rigor/docs/type-specification/special-types.md)); only the Rust representation is ours.

## Trinary certainty

Type, reflection, role-conformance, and member-availability queries answer `yes` (proven), `no` (disproven), or `maybe` (every other case), under the current source, signatures, plugin facts, and configured assumptions. The reference's invariants are normative and bind every inference function:

- `maybe` MUST NOT refine a value as if the answer were `yes`, and MUST NOT manufacture the complementary false-edge fact as if the answer were `no`. It is retained only as a weak relational / member-existence / dynamic-origin fact for diagnostics and later explanation.
- `maybe` does not promote by repetition: repeated `maybe` evidence stays `maybe`; rigor-rs MUST NOT upgrade uncertainty to `yes` by count.
- The relational `maybe` and an inference **budget/incomplete-inference cutoff** are DISTINCT provenance channels. A relational `maybe` means "cannot prove either side under the available evidence" even when inference is complete; a cutoff is an analyzer outcome that produces a `static.*` diagnostic with the cutoff reason and a conservative placeholder (typically `Dynamic[top]`). They compose, but a diagnostic MUST name the cutoff as such and MUST NOT hide "stopped early" inside a relational `maybe`.

## Two relations, kept separate

- **Subtyping** `A <: B` is value-set inclusion: reflexive and transitive, with `bot <: T` and `T <: top`. It is checked against the **static facet** — for `Dynamic[T]` it uses `T` as the value-set witness. Subtyping drives method availability, member access, and refinement.
- **Gradual consistency** `consistent(A, B)` is symmetric and **non-transitive**, and is the ONLY relation that lets a dynamic value cross a typed boundary. It is not a substitute for subtyping; method availability is never decided by consistency.

`untyped` is therefore NOT `top`: `top` is the greatest static value type, while `untyped` (`Dynamic[top]`) suppresses precise static checking at a boundary while preserving the fact that precision was lost. The `Dynamic[T]` lattice algebra these relations witness is recorded in [ADR-0019](0019-value-lattice-and-dynamic-algebra.md).

## Rust representation

A `Certainty` enum (`Yes` / `No` / `Maybe`) is the return type of relation and member queries, and the relation functions return `(Certainty, Evidence)` rather than `bool` — the evidence carries the weak relational fact that keeps `maybe` explainable without letting it narrow. Subtyping and consistency are two separate functions over the interned lattice ([ADR-0005](0005-rust-architecture.md)); the cutoff channel is the placeholder-type + `static.*` diagnostic path, kept disjoint from the `Maybe` value so neither masquerades as the other.

## Considered options

- **Return `bool` and collapse `maybe` into one of the two ends** — rejected: erases the zero-false-positive `maybe` discipline and conflates "unproven" with "disproven".
- **Unify subtyping and consistency into one relation (treat `untyped` as `top`)** — rejected: loses the gradual-boundary semantics the reference depends on.
- **Reimplement the relations and certainty independently with a reasonable-but-different algorithm** — rejected because this is a parity surface; diagnostics are defined over these results, so any divergence in the semantics is a parity break ([ADR-0002](0002-diagnostic-set-parity.md)).
