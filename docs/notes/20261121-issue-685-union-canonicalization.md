# #685 — canonicalize union mints outside `Algebra::join`

Issue: `Algebra::join`'s `make_union` flattens nested `Type::Union`
members at build time, but seven sites interned raw
`Type::Union(...)` outside it — `Union[A, Union[B,C]]` vs
`Union[A,B,C]` interning under two `TypeId`s for the same logical
member set, breaking the `Interner::cmp` canonicalization invariant.
`coll_nominal_with` was the named site (`coll_value_members` pushes
Tuple elements / HashShape values unflattened);
`narrow_type_members` the second. PR #761, merged `06ef5c2b` (head
`1b33c662`).

## What changed

- **`Algebra::union(i, members)`** — the new primitive: recursively
  flatten nested unions, `Interner::cmp` sort + dedup,
  `[]` → `bot` / `[t]` → `t`. Deliberately NOT `join`: no Dynamic
  facet merge (`Dynamic[A] | Dynamic[B]` stays two members), and
  `top`/`bot` kept VERBATIM, not absorbed/dropped. These callers hold
  a member SET compared by `TypeId` equality — the coll pass's set
  carries `Type::Top` as its gradual-floor marker, and absorbing it
  collapsed `Array[1 | top | …]` and `Array[2 | top | …]` onto one
  `TypeId`, unioning distinct literal arms the reference keeps apart
  and firing `frobnicate_zzz` where the oracle is silent
  (`coll_union_literal_arms_stay_distinct` caught this mid-flight —
  a `make_union`-reusing `union` was the first draft).
- **`Algebra::flatten_union_members`** — the same expansion, order-
  PRESERVING, for sig-gen's `literal_tail_nominal` Range arg whose
  describe-sorted member order is the byte-identity contract
  (`Algebra::union`'s canonical sort would mint the wrong signature).
- Routed: `coll_nominal_with` arg, `narrow_type_members` kept list,
  `widen_mutated_binding` union-arm grown list, `coll_union_carrier`
  (replacing its identical hand-rolled flatten+sort+dedup, plus the
  transitive flatten it lacked).
- Audited + left raw with site notes: `rescue_exception_type`'s
  union (members all `Nominal`/`untyped`) and
  `coll_grown_carrier`'s `minted` (all `coll_nominal_with`
  `Nominal`s) — both provably flat-only.

## Measured outcome

Correctness fix measured at 0 gaps by design: `fp_audit --gaps
--sweep` 0 FP, gap census identical to master's (212/138/59/18/8/2 —
no coverage shift, no retraction). Fresh-dir probe rows identical or
byte-identical to master's drift (bare-`Array`/`Hash` member-evidence
erasure, #328 ordering). Reviewer's independent port-vs-port 30-probe
sweep: 0 differences; sig-gen on 8 Range shapes byte-identical.
Tests +3 (rigor-types ×2, rigor-infer ×1).

Residual (non-blocking, review-filed on the PR): the two left-raw
sites still `sort_unstable()` by `TypeId`, not `Interner::cmp` —
flat but not canonical-ordered; reachable impact is coverage-loss
only. Review substitution: all named review providers quota-blocked;
adversarial pass ran on a devin/swe-2 delegate per review.md's
substitution path.
