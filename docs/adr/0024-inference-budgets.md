# Inference budgets: two tiers

Status: accepted

Inference budgets split into **hard termination guards** (counting, non-configurable, widen to `Dynamic[top]` on hit) and **precision budgets** (widening, user-configurable within validated ranges, emit a `static.*` incomplete-inference diagnostic on every hit). This is a parity surface: the normative budget semantics are ported faithfully from [inference-budgets](../../../../ruby/rigor/docs/type-specification/inference-budgets.md) and [ADR-41](../../../../ruby/rigor/docs/adr/41-inference-budget-design.md); only the Rust representation is ours ([ADR-0002](0002-diagnostic-set-parity.md)).

## Two tiers

**Termination guards** (hard, non-configurable, always widen):

On a hit, the inference result widens to `Dynamic[top]` and no `static.*` diagnostic is emitted — the hit is observable only through the type widening and (with `RIGOR_BUDGET_TRACE`) through aggregate counters. A project cannot opt into non-termination.

**Precision budgets** (widening, configurable, always diagnose):

On a hit, the result widens to a conservative type and rigor-rs emits a `static.*` incomplete-inference diagnostic at `:info` severity naming the budget that fired and where inference stopped. The relational `maybe` and a budget cutoff are **distinct provenance channels** ([ADR-0018](0018-certainty-and-relations.md)): a cutoff diagnostic names itself and is never hidden inside a relational `maybe`.

## Currently-wired termination guards (normative — must be reproduced exactly)

These four guards are observable through the types and diagnostic sets they produce. rigor-rs MUST wire them as named constants and reproduce their exact behaviour:

| Guard | Constant name | Value | Configurable? |
|---|---|---|---|
| Recursion re-entry | `RECURSION_GUARD` + `RECURSION_UNROLL_FUEL` + `RECURSION_FIXPOINT_CAP` | Effective depth 1 → Kleene fixpoint from `bot`, cap 3; value-pinned args unroll to fuel 32 | No |
| Ancestor-walk cap | `ANCESTOR_WALK_LIMIT` | 100 nodes | No |
| HKT reducer fuel | `HKT_REDUCER_DEFAULT_FUEL` | 64 steps | No |
| Dependency-source budget | `BUDGET_PER_GEM_DEFAULT` | 5 000 method definitions (range 1 250–20 000) | Yes — `.rigor.yml` `dependencies.budget_per_gem:` |

### Recursion guard detail

When the engine re-enters `(receiver, method)` at effective depth 1, it applies the Kleene fixpoint return-summary mechanism ([ADR-55](../../../../ruby/rigor/docs/adr/55-recursive-return-precision.md)):

- The outermost entry seeds an assumed summary `bot`.
- In-cycle re-entries return the current assumed summary instead of `Dynamic[top]`.
- After the body evaluates, if the result is consistent with the assumption the fixpoint has converged; otherwise the assumption is updated to `join(assumption, computed)` and the body is re-evaluated, up to **cap 3**.
- On the final permitted iteration, value-pinned constituents are widened to their nominal base (`Constant[1]` → `Integer`) to force convergence; non-convergence collapses to `Dynamic[top]`.
- When every argument is `Constant` / `Tuple[Constant]`, the guard key extends to `(receiver, method, argument values)` and distinct constant frames may recurse under hard fuel 32 with a 64-node value-size cap.
- On fuel exhaustion the fallback is the fixpoint summary rather than bare `Dynamic[top]`.

`BudgetTrace` counters (`RECURSION_GUARD`, `RECURSION_UNROLL_FUEL`, `RECURSION_FIXPOINT_CAP`) are exposed for observability; the constants are named so that configuration wiring later is non-breaking.

## Precision budget table (normative intent — not yet wired in the reference)

The reference specifies this table as normative-for-v1 intent but the configurable `budgets:` surface is **not yet wired** in the reference implementation as of the parity snapshot. rigor-rs treats these rows as **forward design**: the semantics are normative, the defaults are placeholders pending measurement-gated validation ([ADR-41](../../../../ruby/rigor/docs/adr/41-inference-budget-design.md) WD3), and rigor-rs MUST wire them as named constants so that `.rigor.yml` wiring later is non-breaking.

| Key | Category | Spec default | Range |
|---|---|---|---|
| `recursion_depth` | Recursion precision-unroll depth | 1 (= off) | 1–32 |
| `call_graph_width` | Call-graph expansion width | 16 | 1–256 |
| `overload_candidates` | Overload candidate count | 8 | 1–64 |
| `union_size` | Union size for joined returns | 24 | 4–256 |
| `structural_growth` | Structural requirement growth | 16 | 1–256 |
| `hash_erasure_keys` | Hash-shape literal-key union | 16 | 1–256 |
| `hash_erasure_values` | Hash-shape literal-value union | 8 | 1–256 |
| `negative_fact_display` | Retained negative-fact display | 3 | 0–32 |

Because `union_size` / `structural_growth` are not yet enforced in the reference, rigor-rs MUST NOT enforce them either until the parity snapshot reflects their activation. Values outside the accepted range produce a configuration diagnostic and fall back to the spec default for that key.

## On-hit policy

Every budget hit (both termination guards and precision budgets) widens to a conservative type — never errors on working code. Precision-budget hits additionally emit a `static.*` incomplete-inference diagnostic at `:info` by default so the user can always distinguish "genuine open surface (`Dynamic[top]` from unresolved dispatch)" from "budget cutoff (`Dynamic[top]` with a named reason)". This is the distinguishing product behaviour: widen like TypeProf, but name the cutoff site ([ADR-41](../../../../ruby/rigor/docs/adr/41-inference-budget-design.md) WD2).

## Boundary contracts as inference cutoffs

An accepted signature contract (inline `#:`, full `# @rbs`, generated stub, external `.rbs`) is an inference cutoff for the callee's return type. Callers use the declared return and recursion does not fan out into the body. The body is still checked against the contract. When no boundary is supplied, callers MUST NOT receive a fabricated precise type; rigor-rs uses `Dynamic[top]` or the declared bound and preserves the incomplete-inference provenance ([CONTEXT.md](../../CONTEXT.md)).

## Considered options

- **Reimplement budgets independently with different thresholds or counting strategies** — rejected: the wired guards produce observable types and diagnostics; any divergence is a parity break ([ADR-0002](0002-diagnostic-set-parity.md)).
- **Error on budget hit** — rejected: errors on working code violate the zero-false-positive bar; the `static.*` diagnostic is `:info`, not an error.
- **Make every budget configurable including termination guards** — rejected: termination guards must not be disable-able below their floor; non-termination is not a valid project configuration.
