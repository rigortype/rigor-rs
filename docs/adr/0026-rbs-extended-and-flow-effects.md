# RBS::Extended annotation grammar and the flow-effect bundle

Status: accepted

Rigor reads Rigor-specific metadata from RBS annotations in `*.rbs` files under the name `RBS::Extended`, using `%a{rigor:v1:...}` payloads. These annotations let users and plugin authors describe types that exceed standard RBS without changing Ruby application code; standard RBS tools preserve or ignore them. The flow-effect bundle produced by these annotations and by plugins is the canonical plugin↔engine data contract. Sources: [rbs-extended.md](../../../../ruby/rigor/docs/type-specification/rbs-extended.md) and [control-flow-analysis.md](../../../../ruby/rigor/docs/type-specification/control-flow-analysis.md).

## Directive set

All directives use the versioned `rigor:v1:` namespace. Unversioned `rigor:` directives MUST NOT be emitted and SHOULD be treated as invalid. An unsupported `rigor:vN:` directive is preserved by RBS tooling but reported by rigor-rs as unsupported metadata.

### Per-method directives

| Directive | Effect |
|---|---|
| `rigor:v1:return: T` | Overrides the RBS-declared return type with `T` at every call site. |
| `rigor:v1:param: name [is] T` | Tightens the declared type of parameter `name` to `T` at call sites and inside the method body. The `is` glue word is optional. |
| `rigor:v1:predicate-if-true target is T` | Refines `target` to `T` on the **true** branch of a conditional call. |
| `rigor:v1:predicate-if-false target is T` | Refines `target` to `T` on the **false** branch. |
| `rigor:v1:assert target is T` | Refines `target` after the method returns normally. |
| `rigor:v1:assert-if-true target is T` | Refines `target` when the method returns a truthy value. |
| `rigor:v1:assert-if-false target is T` | Refines `target` when the method returns `false` or `nil`. |
| `rigor:v1:conforms-to _Iface` | Declares that the class satisfies `_Iface` and instructs rigor-rs to verify the conformance proactively, even without a call site that exercises it. Multiple directives combine as an interface intersection. |
| `rigor:v1:pure` | (per-method) Declares the method has no observable side effects relevant to fact stability. |

### Declaration-level HKT directives

These attach to a `class` / `module` declaration and take space-separated `key=value` pairs.

| Directive | Effect |
|---|---|
| `rigor:v1:hkt_register: uri=<uri> arity=<int> variance=<v1>,<v2>,... bound=<class_or_untyped>` | Registers a type-constructor URI with arity, per-position variance, and erasure `bound`. |
| `rigor:v1:hkt_define: uri=<uri> params=<P1>,<P2>,... body=<body_text>` | Binds the URI to a type-function body; `body=` gobbles the remainder and is parsed into a union tree. |

## Source restriction

Directives fire ONLY from `.rbs` files. rigor-rs MUST NOT parse or honor `%a{rigor:v1:...}` annotations in `.rb` source files.

## Target grammar

The initial target grammar is:

```text
target ::= parameter-name | self
```

`parameter-name` is an RBS method parameter identifier (`/[a-z]\w*/`). If a predicate needs to refer to an argument, the RBS method type MUST name that argument. Future versions MAY extend targets to instance variables, record keys, and shape paths, but those MUST use explicit path syntax rather than overloading directive names.

## RHS payload grammar

The right-hand side of `return:`, `param:`, `assert*`, and `predicate-if-*` accepts:

- An RBS-style class name (`String`, `::Foo::Bar`).
- A kebab-case refinement from the imported-built-in catalogue (`non-empty-array[Integer]`, `non-empty-hash[Symbol, Integer]`, `int<min,max>`), parsed by `Builtins::ImportedRefinements::Parser`.
- Symbol or String literal tokens (`:name` / `"name"`) and unions of them with `|`; each literal is lifted to `Constant<value>` and folded via `Type::Combinator.union`.
- `~T` negation is allowed only on class-name payloads; refinement-form payloads MUST NOT use `~T` (the difference-against-refinement algebra is reserved for a future slice).

## Annotation composition rules

- Multiple annotations on the same RBS node are interpreted deterministically and independently of source order.
- Exact duplicate annotations are idempotent.
- Compatible annotations compose by directive kind, target, and flow edge (e.g., true-edge and false-edge predicate facts on the same parameter are different effect slots).
- **Conflicting annotations are diagnostics** — never first-wins or last-wins. Conflicts include: incompatible payload syntax, two non-identical singleton directives for the same effect slot, contradictory refinements whose intersection is `bot`, and any annotation whose refinement exceeds the ordinary RBS contract.

## Flow-effect bundle

The flow-effect bundle is the canonical plugin↔engine data contract. A plugin or `RBS::Extended` annotation MAY contribute a bundle with:

- Normal return type.
- Truthy-edge facts.
- Falsey-edge facts.
- Post-return assertion facts.
- Exceptional or non-returning effects.
- Block call-timing effects.
- Escape effects (receivers, arguments, blocks, captured locals).
- Receiver and argument mutation effects.
- Fact invalidation effects.
- Dynamic reflection members introduced by the call.
- Provenance and certainty for all contributed facts and effects.

The analyzer applies bundle contributions through the same control-flow machinery it uses for built-in guards. A plugin-defined predicate on the left side of `&&` MUST refine the scope used to analyze the right side; its negative fact MUST flow into the right side of `||`.

## Contribution merging

Merging is **analyzer-owned** and deterministic:

- Core Ruby semantics and accepted signature contracts are **authoritative**. `RBS::Extended`, generated metadata, and plugins MAY refine compatible facts but MUST NOT weaken or contradict the ordinary Ruby/RBS contract.
- Compatible facts on the same target, flow edge, and effect kind compose: positive type facts intersect; negative facts and relational facts accumulate under their normal budgets; mutation, escape, and invalidation effects are unioned conservatively.
- **Contradictory contributions are diagnostics**, not first-wins or last-wins. rigor-rs SHOULD keep the nearest non-conflicting authoritative fact and ignore or weaken the conflicting contribution for that target and edge.
- Truthy-edge and falsey-edge facts remain **edge-local**. A plugin MAY contribute one-sided facts; rigor-rs MUST NOT infer the opposite edge unless the contribution explicitly provides it or the core analyzer can derive it.
- Repeated `maybe` evidence does not become `yes` by count alone. Certainty changes only when a contribution supplies a stronger proof or the core analyzer can derive one from compatible facts.
- Dynamic return contributions are checked against the selected signature. An incompatible return contribution is a conflict diagnostic, not an override.

## Considered options

- **Reimplement the annotation grammar and merging rules independently** — rejected; parity surface. The directive set, target grammar, payload grammar, composition rules, and merging algorithm are fully specified by [rbs-extended.md](../../../../ruby/rigor/docs/type-specification/rbs-extended.md) and must be reproduced faithfully.

## Relationship to other ADRs

- [ADR-0019](0019-value-lattice-and-dynamic-algebra.md) — the value lattice definitions (`bot`, `Dynamic[top]`, union, intersection) underlie the merging algebra.
- [ADR-0013](0013-plugin-architecture.md) — plugins return flow-effect bundles through the Rust trait described there.
- [CONTEXT.md](../../CONTEXT.md) — canonical glossary.
