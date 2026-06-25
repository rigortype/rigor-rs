# Structural typing applies only at four boundaries; classes stay nominal

Status: accepted

Ruby is nominal by default — `is_a?` and `kind_of?` test inheritance, not shape — so rigor-rs applies structural typing only at the four boundaries the spec names. Classes and module names remain nominal; structural matching is triggered by assignment or call context, not by comparing two nominal types. See [structural-interfaces-and-object-shapes](../../../../ruby/rigor/docs/type-specification/structural-interfaces-and-object-shapes.md) for the authoritative rules.

## The four structural boundaries

Per the spec, structural typing applies exactly at:

1. Assigning or passing a value where an RBS interface type is expected.
2. Checking whether an inferred object shape satisfies an interface.
3. Checking a direct method send against a known shape.
4. Using plugin-provided dynamic reflection to add members to a shape or nominal type.

Outside these four boundaries, class-to-class compatibility is nominal. rigor-rs MUST NOT make ordinary class-to-class compatibility TypeScript-style structural by default.

## `MethodEntry` record

One `MethodEntry` record exists per `(class-or-module, method-name)`. It corresponds to the single runtime-resolved method body for that name on that class; Ruby has no per-signature runtime overloading.

- Visibility is stored at the entry level; `private :foo` toggles the whole entry.
- Signature variants (RBS overloads, `RBS::Extended` payloads, plugin contributions) are stored as a list of branches inside the entry. Branches share entry-level visibility and MAY carry different argument shapes, return types, predicate effects, and mutation effects.
- Conditional `def` and dynamically constructed method definitions are out of scope for the first implementation and surface as diagnostics or dynamic-origin facts.

## Reader / writer / accessor variance

`attr_reader`, `attr_writer`, and `attr_accessor` are sources of method facts, not field declarations. rigor-rs MUST model their output as separate `MethodEntry` records:

- `attr_reader :x` → one public reader entry `x`; **covariant** in its return type.
- `attr_writer :x` → one writer entry `x=`; **contravariant** in its accepted value type.
- `attr_accessor :x` → two entries `x` and `x=`; the pair is effectively **invariant** in the value type.

A manually defined `x` or `x=` replaces or refines the method fact via ordinary Ruby method lookup and source order.

## Open-class merge order

Open classes, reopens, and monkey patches contribute to the same `MethodEntry`, not parallel ones. Merge follows Ruby dispatch order:

1. `prepend`ed modules (in reverse prepend order, nearest first).
2. The class itself (last definition wins within a single class body).
3. `include`d modules (in reverse include order).
4. Superclass chain.

Strict mode raises a diagnostic when a redefinition changes an RBS-visible signature or visibility without an explicit override marker (`rigor:v1:override=replace`; see [rbs-extended.md](../../../../ruby/rigor/docs/type-specification/rbs-extended.md)).

## Core capability-role catalog

The catalog is FIXED. Plugins MAY add framework roles and `maybe` conformance facts but MUST NOT replace or redefine catalog entries.

Reused RBS interfaces (matched by existing RBS shape, not redefined):

| Interface | Use |
|---|---|
| `_Each[T]` | Enumerable iteration over `T` |
| `_Reader` | Stream-like read access |
| `_Writer` | Stream-like write access |
| `_ToS` | Implicit string conversion |
| `_ToStr` | Explicit string coercion |
| `_ToInt` | Integer coercion |
| `_ToProc` | Block conversion |
| `_ToHash[K, V]` | Hash coercion |
| `_ToA[T]` | Array conversion |
| `_ToAry[T]` | Strict array coercion |
| `Enumerable[T]` | Broad collection protocol |
| `Comparable` | Ordering protocol |

Rigor-specific roles, each shipped with a bundled RBS interface:

| Role | Required members |
|---|---|
| `_RewindableStream` | `read`, `rewind` |
| `_ClosableStream` | `close`, `closed?` |
| `_FileDescriptorBacked` | `fileno` |
| `_Callable[**A, R]` | `call(*A) -> R` |

## Named-interface matching

Matching is indexed, not a global scan. A candidate interface is compared only when it shares at least one required member and passes cheap arity/visibility filters. When multiple interfaces match, selection is deterministic:

1. Exact member-signature match.
2. Configured standard-library role over unrelated coincidental interface.
3. Fewer extra required members.
4. Stable lexical name order.
5. If candidates remain meaningfully ambiguous, keep the anonymous shape and emit no suggestion.

The candidate limit is `budgets.interface_candidates` per [inference-budgets.md](../../../../ruby/rigor/docs/type-specification/inference-budgets.md). Intersections are useful but rigor-rs MUST NOT solve an unbounded set-cover problem.

## `respond_to?` visibility grading

- `obj.respond_to?(:foo)` — public existence fact on the true branch.
- `obj.respond_to?(:foo, false)` — same as the default.
- `obj.respond_to?(:foo, true)` — existence fact whose visibility may be public, protected, or private; does not prove the method is legal as an external explicit-receiver call.
- Second argument not statically known — weaker maybe-private visibility fact.

`method_missing`-backed facts carry dynamic provenance and an unknown or plugin-provided signature.

## Escalation rule

Structural inference results are available to callers and tooling but are not silently promoted to public contracts:

- **Diagnostic** — a call that does not satisfy the declared parameter type is always reported.
- **Hint** (`hint.role-generalization.*`) — when the body's inferred role is strictly smaller than the declared nominal type and a structural interface would still type-check. Gated by `style.suggest_role_generalization`, default off.
- **Silent** — otherwise; retained internally for the plugin `Scope` API.

rigor-rs MUST never both reject a call and offer a hint for the same parameter, and MUST never silently rewrite a public nominal contract into a structural one.

## Considered options

- **Reimplement structural matching logic independently** — rejected; parity surface. The four-boundary rule, member compatibility, variance semantics, catalog contents, tie-break order, and escalation rule are all specified by [structural-interfaces-and-object-shapes.md](../../../../ruby/rigor/docs/type-specification/structural-interfaces-and-object-shapes.md) and must be reproduced faithfully.

## Relationship to other ADRs

- [ADR-0019](0019-value-lattice-and-dynamic-algebra.md) — object shapes interact with the value lattice; `Dynamic[top]` and `bot` are the erasure endpoints.
- [ADR-0013](0013-plugin-architecture.md) — plugins contribute dynamic reflection members to shapes at boundary 4.
- [CONTEXT.md](../../CONTEXT.md) — canonical glossary.
