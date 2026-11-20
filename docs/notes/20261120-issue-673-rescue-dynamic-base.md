# #673: rescue exception designator with a dynamic base is opaque (PR #711)

A `rescue` exception class whose constant path has a **dynamic base**
(`k::ArgumentError`) lowered to a `ConstantRead` carrying the *lenient*
trailing name (`ArgumentError`), so five rescue consumers resolved it as
if spelled statically — two FP families on the snapshot-diff blind side
(emissions the reference never makes).

## Problem

- `exception_union` / `rescue_reference_type` / `coll_rescue_exception_ty`
  / `collect_rescue_bindings` bound `rescue k::ArgumentError => e`'s `e`
  to `Nominal[ArgumentError]`, minting `call.undefined-method` on the
  reference and `def.ivar-write-mismatch` on `@x = e` writes.
- `resolve_constant` (`shadowed_rescue.rs`) certified a `k::`-designated
  clause, minting `flow.shadowed-rescue-clause` — the reference's
  `qualified_name_or_nil` is STRICT (nil on a dynamic base) → the clause
  is opaque.

## Change

Gate each of the five consumers on the pre-existing
`Node::ConstantRead.dynamic_base` flag (the same bit `expr_type.rs:393`
has gated since #655): a dynamic-base member types `Dynamic[top]`, as
`ErrorInfo.rescue_exception_type` does through the expression typer — a
non-`Singleton` member contributes `Dynamic[top]`, never a name.

## Measured outcome

- Both brief rows + ~70 reviewer counterexamples match the oracle as full
  tuples; static controls (`rescue ArgumentError => e`, static shadow)
  still fire.
- `gate.sh` pass; `snapshot.rb --check` no drift; `fp_audit --gaps
  --sweep` **0 FP** over the 8-corpus standing set; CI green on the head.
- Fixture 57 gained the oracle-silent shadow-direction rows (snapshot
  stays `[]`); a `shadowed_rescue.rs` unit test pins the opaque verdict.

## Follow-ups filed

- **#712** — pre-existing coverage gap the fix *surfaces*: top-level
  locals (rescue-bound `e` included) do not narrow through
  `is_a?`/`===` guards; `e = foo; e.bogus_zz if e.is_a?(ArgumentError)`
  fires `for ArgumentError` on the reference, silent in the port (def
  params DO narrow; cf. #568's local-case-subject family, #670).
- Older gaps unchanged on master (reviewer-measured): `Errno::ENOENT
  => e`, static `rescue StandardError; rescue K::E` shadow (project
  `K::E`), `=> h[:e]` message drift (`{}` vs `Hash[...]`).
