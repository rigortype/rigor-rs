# #427 residual walkers: cmp loop + iterative predicates

PR #680 merged 2026-11-19 (`08c12580`). Opus 5.5 Approved (`44a1d038`) +
fix-confirmation Approved (`a6927f90`); Fable 5.2 design companion shaped the
final `cmp` form. Grok advisory unavailable (provider 402); Fable 5.2 stood in
for 5.1 (not an offered model).

## What

#648's review named the leftover Tuple-axis walker family; this finished it:

- `Interner::cmp` — the total order behind union/intersection member
  canonicalization (`Algebra::{join,meet}` → `sort_by`). Recursive version
  overflowed a 512 KiB thread stack on two 200k-level `Tuple` chains
  (reproduced, SIGABRT; #431's reserved command stack masked it at 60k on the
  CLI). Final form is a **stackless first-difference loop**: hash-consing
  dedups carriers by structure, so distinct `TypeId`s can never compare
  `Equal` and the first differing unit of the left-to-right expansion decides
  the comparison — scan leading atoms, descend into the first differing child
  pair. The recursion's order and early exit, with no call stack and no
  allocation. (A `CmpWork` Vec draft landed first; the design companion
  measured it 4–10× slower on cheap calls via eager full-width expansion and
  per-call allocation, so it was replaced before review closed.)
- `literal_seed` (call_receiver) — the `call.undefined-method` receiver gate;
  `literal_seed`↔`element` mutual recursion → single DFS stack.
- `nested_pinned_literal` + `composite_holds_unsound` (sig_gen) — `any`
  predicates → DFS stacks.

**Deep `Drop` checked**: `Type` children are `TypeId`s — carriers drop
shallow; declared-shape trees are bounded by syntax + the #426 25k bound.

## Verification

- `cmp` equivalence is pinned in-tree by a verbatim copy of the removed
  recursion differentially fuzzed ~50k random pairs over every arm (also
  asserting the distinct-ids premise); two external reviewers fuzzed 2.288M +
  2.4M pairs with `App`/varied-atom coverage, antisymmetry and mutation
  controls — 0 mismatches all.
- `check`/`sig-gen` JSON byte-identical on wrap70 (630k levels), literal-seed
  probes, and mastodon/dependabot-core/net-ssh/gitlab-foss corpora; `rigor
  lsp` `didOpen` of a 180k-level file publishes diagnostics and stays alive.
- `fp_audit --gaps --sweep`: 0 FP, gaps identical to master (198/85/55/16/6).

## Bounded-wrapper walkers (left recursive, deliberately)

`Algebra::{join,meet,difference,normalize}`, `collect_*_members`, `subtype`,
`coll_erased_store_member`, `coll_could_be_range`, `classify_type`,
`widen_value_pinned` recurse only through Union/Intersection/Refined/
Difference/Dynamic **wrapper** chains — flat by construction *almost*
everywhere. Exception found by review: raw `intern(Type::Union(..))` at
`coll_nominal_with` + `narrow_type_members` skips flattening → #685
(fidelity, not a crash: ~1M statements to overflow).

## Open residual

- The `.map` repro still does not run to completion — type-building time,
  #424's scope.
- Display remains O(depth²) bytes copied (#648 note).
- `x = [x, x]` DAG: the DFS predicates have no visited set — exponential on
  shared carriers, same as the recursive originals (folded into #685's
  related note).
