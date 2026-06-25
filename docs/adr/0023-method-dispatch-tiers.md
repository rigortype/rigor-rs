# Method dispatch is a fixed tier order

Status: accepted

Method dispatch runs a fixed cascade — constant-folding → shape → RBS → in-source → fallback — and the first tier that produces a result wins. This is a parity surface: the normative dispatch order is ported faithfully from [05-methods-and-blocks](../../../../ruby/rigor/docs/handbook/05-methods-and-blocks.md) and the reference engine design ([ADR-4](../../../../ruby/rigor/docs/adr/4-type-inference-engine.md)); only the Rust representation is ours ([ADR-0002](0002-diagnostic-set-parity.md)).

## The five tiers

| Tier | Condition | Result |
|---|---|---|
| 1. Constant folding | All arguments are `Constant` / `Tuple[Constant]`, receiver is a known nominal class, method is on the purity allowlist | Execute at analysis time; return the folded `Constant` |
| 2. Shape dispatch | Receiver carries `Tuple` / `HashShape` / `IntegerRange` / refinement and the method has a per-shape rule | Return the shape-specific type |
| 3. RBS dispatch | The class has an RBS sig for the method (including an `RBS::Extended` directive) | Check arguments against the parameter contract; return type from the sig |
| 4. In-source dispatch | A `def` (or `define_method`, `attr_*`) is discovered in the project | Infer return type from the method body; parameter contracts are not checked |
| 5. Fallback | None of the above | Return `Dynamic[top]`; stay silent |

Tier 3 precedes tier 4: an RBS sig overrides the in-source body's inferred return. Tightening at the sig level is the supported way to teach the engine about a method whose return is narrower than what body inference produces.

## Inference core: pure dispatch function, not OOP inheritance

The inference core is a pure `type_of(node, ctx) -> Type` function dispatched by Prism node variant using pattern matching, not OOP inheritance. `MethodDispatcher` is a separate component responsible only for the tier cascade; type-object classes stay thin and do not embed method-dispatch logic. This mirrors the reference's `ExpressionTyper` / `MethodDispatcher` split ([ADR-4](../../../../ruby/rigor/docs/adr/4-type-inference-engine.md)).

## Explicit `return` nodes must contribute to inferred return

When inferring the return type for an in-source method body, every reachable explicit `return` node contributes its value type to the inferred return union. A tail-only body evaluator that discards `return value` nodes would produce an unsoundly narrow return type; that is a reference defect class ([ADR-57](../../../../ruby/rigor/docs/adr/57-self-call-return-adoption.md)) and must not be reproduced.

## Cross-file implicit-self call resolution

An implicit-self call inside a method body resolves against the enclosing class's full ancestor chain — own definitions, superclass chain, and included modules — across project files and RBS-known ancestors, exactly as [ADR-24](../../../../ruby/rigor/docs/adr/24-self-method-call-resolution.md) specifies:

- On a hit, the call site adopts the callee's inferred return type and parameter contract.
- On a miss, the call stays `Dynamic[top]` (the leniency principle: Ruby methods are routinely defined dynamically).

The ancestor walk is bounded by the reference's `ANCESTOR_WALK_LIMIT` (100 nodes); exceeding it falls back to `Dynamic[top]`. See [ADR-0024](0024-inference-budgets.md) for the budget context.

## Reflexive `send` / `public_send` must not constant-fold on a non-literal method name

`send`, `public_send`, and `__send__` MUST NOT be constant-folded unless the method-name argument is itself a value-pinned literal `Constant` (e.g. `:foo`). With a runtime-variable method name the call degrades to `Dynamic[top]` (RBS result). Folding a reflective send on a non-literal method name produces a spurious `Type::Constant` that downstream rules can misread as a proven runtime constant, generating false-positive `flow.always-truthy-condition` diagnostics. Source: [ADR-78](../../../../ruby/rigor/docs/adr/78-reflexive-overfold-always-truthy.md).

## Callee return adoption and the overridable-method gate

A resolved self-call adopts its inferred return unconditionally — the gate introduced in [ADR-57](../../../../ruby/rigor/docs/adr/57-self-call-return-adoption.md) is now open. One refinement applies: before adopting a **flow-constant-foldable** return (`Constant` or `Tuple[Constant]`) the engine checks whether the callee's owner has a discovered project type that redefines the same method on a related class or module. If so, the return degrades to `Dynamic[top]` to avoid unsound always-truthy folds on template-method patterns. This refinement is part of the normative reference behaviour and must be reproduced.

## Considered options

- **Reimplement dispatch independently with a different tier order or OOP inheritance on type classes** — rejected: the tier order is observable through diagnostics (which tier wins changes the return type and which diagnostic fires); any divergence is a parity break ([ADR-0002](0002-diagnostic-set-parity.md)).
- **Treat unresolved in-source calls as errors** — rejected: the reference's leniency on uncertain dispatch is a first-class design principle ([ADR-24](../../../../ruby/rigor/docs/adr/24-self-method-call-resolution.md) WD3).
