# Robustness principle: precise returns, permissive parameters

Status: accepted

Whenever the engine or a plugin must **author** a carrier, RETURN positions take the most precise carrier provable and PARAMETER positions take the most permissive carrier that stays sound — Postel's law for types. This is a [parity surface](../../CONTEXT.md): the rule is ported faithfully from [robustness-principle](../../../../ruby/rigor/docs/type-specification/robustness-principle.md) (design rationale in the reference's [ADR-5](../../../../ruby/rigor/docs/adr/5-robustness-principle.md)); only the Rust representation is ours.

## The rule

- **Strict returns (clause 1).** A return type is as strict as can be *proved* without compromising soundness, reaching for the most precise carrier ([ADR-0019](0019-value-lattice-and-dynamic-algebra.md)): `Constant`, `Tuple`, `HashShape`, `IntegerRange`, small `Union`, then nominal, with `Dynamic[T]` the last resort. Precision propagates: a tighter return tightens every downstream narrowing chain.
- **Lenient parameters (clause 2).** A parameter type is as permissive as the body's correct behaviour permits (capability role, structural interface, supertype, `T | nil` when the body guards), so callers do not paste defensive coercions at every call site.

Both clauses are SHOULD-strength defaults that direct the choice among correctness-preserving carriers; **correctness always takes precedence**. The principle binds wherever rigor-rs authors a type (built-in catalog return tier, inferred user-method signatures, `RBS::Extended` payloads) but MUST NOT override hand-written or vendored RBS authorship.

This asymmetric robustness is the **zero-false-positive foundation** and a cross-cutting rule every inference function and plugin MUST follow. It pairs with trinary certainty — a strict return can still produce a correct `maybe` ([ADR-0018](0018-certainty-and-relations.md)) — and with the narrowing tier, which recovers the body's precise type from a wide parameter.

## Rust representation

No new type; this is a constraint on every function and plugin that *constructs* a `Type` for a return or parameter position. The precision ladder and permissiveness ladder are expressed over the interned carriers ([ADR-0019](0019-value-lattice-and-dynamic-algebra.md)), and the asymmetry is enforced by convention and review across `rigor-infer` and the plugin API ([ADR-0013](0013-plugin-architecture.md)), with the differential harness ([ADR-0002](0002-diagnostic-set-parity.md)) catching divergences as parity breaks.

## Considered options

- **Symmetric authoring (same precision policy for both positions)** — rejected: either over-strict parameters force call-site workarounds, or widened returns discard facts downstream; the asymmetry is the point.
- **Reimplement the authoring policy independently with a reasonable-but-different heuristic** — rejected because this is a parity surface: the carriers rigor-rs authors feed the diagnostic-set comparison ([ADR-0002](0002-diagnostic-set-parity.md)), so a different policy diverges from the reference.
