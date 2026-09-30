# 2026-09-30 — issue #309: rescue-carrier collection snapshot FP

PR #333, merged `6a668a9`. Two review rounds; sweep 0 FP; CI 5/5 on
`5e44f8a`. Residuals #337–#339.

## What landed

Mutations inside a `begin/rescue` carrier now join through the recovery
post-scope instead of reaching the unconditional snapshot path —
`begin; b.unshift("s"); rescue; nil; end; b.frobnicate` is silent like the
reference (`eval_begin`/`live_rescue_results` semantics; `ensure`
straight-lines onto the exit scope).

## R1 find + fix

The same PR's `member_evidence` union-collapse introduced a port-only FP
family (`a = c ? [1] : [2]; a.push(3); a.frobnicate` — literal-arm
contents were invisible to the grown member set, so distinct arms minted
the same bare nominal and collapsed). Fixed by giving
`coll_value_members` Tuple/HashShape evidence (erased key-class + pinned
values, `key_union_for` semantics) and minting literal arms in
`widen_mutated_binding`'s union arm — identical grown arms still collapse
(converging rows fire), distinct ones stay unioned. The sweep's last
corpus FP (gitlab `compact!`/`stringify_keys!`) stays fixed.

## Residuals

- #337 heterogeneous conditional/rescue mutation convergence (ref's
  admissibility gate drops the heterogeneous store).
- #338 `rescue => e` types `for StandardError` in ref; port silent.
- #339 non-carrier union member decline (`c ? [1] : "x"` + push).
- `[]=`/store slot-rewriting erasure family — coverage trade vs master,
  documented at collection_shape.rs:1108-1110.
