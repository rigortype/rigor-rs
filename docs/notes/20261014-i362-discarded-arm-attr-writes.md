# #362 — AttrWrite in discarded positions: `blocked` vs `discarded`

PR #403, merged `f6052ed` (squash of `5c9a3bd`..`c9f38a8`). Sister issue to
#341 (`when`-condition write leaks) in the indexed-flow family: a compound
attribute write (`h.default ||= 0`) in a scope-discarded position dropped the
receiver's `"s" | 1` narrowing where the oracle keeps it, so the port stayed
silent or reported `for 1` where the reference fires `for "s" | 1` on the
post-scope read.

## The two kinds of dead position

The review-critical distinction — get it wrong in either direction and you
ship an FP:

- **`blocked` — never scope-evaluated.** `when`/`in` conditions and patterns
  (`Narrowing.case_when_scopes` shape-narrows them without `sub_eval`),
  literal-folded arms, modifier-rescue flattened positions. The write never
  runs; `AttrWrite.evaluated`/`closure_evaluated` gate on
  `recovery_blocked == 0` and the span goes to `blocked_spans`, so the write
  binds nothing anywhere.
- **`discarded` — evaluated, join drops the scope.** Terminated
  `if`/`unless`/`case`/`in` arms (`eval_if`'s early-return edges,
  `join_case_branch_scopes`, `live_rescue_results`), dead `rescue` clauses,
  exiting `and`/`or` right sides. The write DOES run in-arm —
  `eval_attribute_compound_write` widens the receiver for in-arm reads
  (`if c; h.default ||= 0; h[:a].f; return; end` is oracle-silent) — only the
  post-arm join throws the scope away.

First iteration gated everything on `recovery_blocked`; adversarial review
(Opus) caught the resulting FP family — every terminated-arm + attr-write +
later-in-arm-read shape fired `for "s" | 1` where the oracle is silent. The
fix keeps the write `evaluated` and scopes it positionally instead.

## Mechanism

`LoweredAst.discarded_spans` (new `CarrierSpans` member) records the region at
lowering — one span per arm, whole body hull for statement lists so the arm's
own earlier write still reaches its later reads. `DeadPositions.dropped`
consumes it in `write_reaches`: a write inside reaches a read inside the same
region, never outside. "Inside" requires `use_.0 < d.1` — the zero-width
post-statement probe at `(end, end)` must not count as in-region (same trap
as the rescue-arm boundary in #386; `c && (w; return)` ends its dropped RHS
at the statement's own end).

Wiring: `bind_check_statement` now filters rebinds + mutations through
`dead.write_reaches` for EVERY statement kind (previously only the `if` and
`begin` arms did) — `case`/`&&`/`||` statements were the leak path.
`apply_closure_mutations` honours `write_reaches` for the same reason.
`entry_descend`'s `if` arm and `entry_children`'s earlier-sibling replay
switched from `apply_subtree_effects` (flat, untyped) to
`bind_check_statement` (real sequential bindings, same as the rescue-clause
path) — `if c; x = 2; x.w; return; end` now fires `for 2` in-arm.

`arm_lands` exempts arms that merge downstream: `next`/`break` in iterated
bodies (`loop_iteration`, `join_break_scopes` — which joins locals but NOT
the narrowing table, so local writes land while attr-write widening does
not) and `retry` (lexical: real `begin` frames and def/class/module/sclass
boundaries retarget it; blocks/lambdas/loops pass it through). A `for` +
`break` does NOT land — `eval_for` collects no break scopes.

## Measured

- All 7 issue rows fire `for "s" | 1` byte-identically; master was silent on
  6/7 (i7 already fired).
- FP family (18 shapes): `if`/`unless`/`elsif`/`case`/`in`/`rescue`/`&&`/`||`/
  `throw`/`exit`/paren/nested/iterated/`each`-closure/`+=`/`&&=` in-arm reads
  — silent on all three engines. Read-before-write in the same arm still
  fires `for "s" | 1`.
- Parity gains: `for`+`break` post-loop read; in-arm `x = 2; x.w` in
  `if`/`else` arms (`for 2`); `x = 1; begin 2 rescue x = "s" raise end; x.w`
  (`for 1`).
- Gates: `harness/gate.sh` pass (incl. `issue_368_live_position_writes`),
  `cargo test --workspace` clean, clippy 1.88 clean, release sweep 0 FP
  across all 8 corpora. CI green on `c9f38a8`.
- Reviews: Opus Needs-fix → the discarded/blocked conflation above →
  fix-confirmation Approved. Grok Changes-requested (test-row subject
  conflated case-subject narrowing with the write's receiver — replaced
  `case h` with a non-`h` subject; comment drift fixed) → clean.

## Accepted losses (master-equal, safe side)

- In-arm reads inside `case`/`when`/`&&`-RHS get flat subtree replay — a
  read BEFORE an in-arm write stays silent where the oracle fires
  (`when 1; h[:a].f; h.default ||= 0` — ordering isn't modelled inside
  non-`if` containers).
- `x = y rescue (q = 2; q.w)` — the modifier-RHS read isn't diagnosed at all
  (pre-existing `Recovered`-carrier walk gap, not introduced here).
- `while`/`until` `break` arms land locals but the narrowing table is
  untouched — `join_break_scopes` asymmetry above.
