# Issue #304 — `Range[::int]` renders as bare `Range`

PR #378, merged `f157a67` (head `cf3b535`). Approved after adversarial review;
the brief's rows are byte-identical on both render channels.

## What landed

`call.argument-type-mismatch` dropped generic type arguments when retaining a
parameter's written form — `Range[::int]` rendered as `Range`. The fix was
larger than the brief implied (+726/−166): `RetainedParamType` in
`crates/rigor-index/src/rbs.rs` now recurses into class/alias/interface type
arguments (new `ClassInstance`/`Alias`/`Interface`/`Variable`/`Tuple` leaves,
`type_alias_params` map, `substitute_vars`), feeding two render channels in
`call_arguments.rs`:

- single-overload non-nil: the param's translated `describe(:short)` (alias
  expansion + declared-param substitution, `Dynamic[top]` for interfaces /
  unbound vars, `T?`/`bool` union collapses);
- multi-overload & pure-nil: the RBS `to_s` written form with one leading
  `::` stripped at the label edge (`Range[::Foo]`, `Range[::Foo] |
  ::Set[::Foo]`).

Also ported `stub_typed_param?`: a param head naming an undefined or
synthesized-stub class declines — which additionally removed a **master FP**
(`BogusClass` param firing `expected BogusClass, got nil`).

## Measured

34-row project-`sig/` probe matrix: 33 byte-identical; sweep 0 FP; a
master-binary sweep subset produced identical gap counts.

## Residuals (all filed)

- **#382** — `Optional` arm (`translated_param_accepts`) fires `String?` +
  sig-declared arg class where the reference answers "maybe" (host-process
  class resolution vs port's RBS-index `Disjoint`). Extends a shipped family.
- **#383** — `stub_typed_param` leaf-keyed lookup declines qualified declared
  param names (`deep(nil)`, `take_loc(nil)`) — regression vs master, safe
  side. Bundles the pre-existing describe/acceptance gaps.
- **#384** — `render_describe` drift: `Optional`-in-`Union` and nested
  `Optional` not flattened; global `::` deletion fabricates names inside
  literal/singleton `Other` forms.
