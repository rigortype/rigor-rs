# 2026-11-22 #820 — parenthesised operands AND subjects are unread (PR #829)

Second half of the paren family: `38a7cf34`/`981f18f1` had already gated
the OPERAND side (`when (C)`, `x.is_a?((C))`, `x == (nil)`, `x < (lit)`).
The review's counterexample sweep showed the same upstream rule applies
to the SUBJECT positions the port still unwrapped: `(x).is_a?(C)`,
`C === (x)`, `(x).nil?`, `(x) == nil`, `(x) < 2`, `(x) =~ /re/`,
`case (x)`, `while (x) < n`, `x.foo((y))`.

## What the reference does

Every predicate analyser gates on operand NODE CLASS —
`receiver_slot`, `analyse_nil_predicate`, `analyse_class_predicate`,
`equality_local_literal`/`static_literal_type`,
`comparison_local_literal`/`numeric_literal_node?`, `static_hash_key`,
`analyse_between_predicate` all match
`LocalVariableReadNode`/`ConstantReadNode`/literal nodes only; a
`ParenthesesNode` operand answers unread for the WHOLE call. Consequences
that had to be honoured together:

- Predicate-position parens DO read — `analyse` unwraps and recurses —
  so `!(x)` narrows `x` and `if (x)`/`while (x)` keep their edges.
- `(x = v).m` — a paren'd WRITE — stays readable
  (`write_receiver_as_read`, upstream #1223); `LocalVariableWrite` arms
  are intentionally ungated.
- `x&.m` on a provably-nil carrier folds to `nil` (truthy edge dead)
  when the RECEIVER is bare — `x&.foo((y))` — but a paren'd receiver
  declines the call wholesale: `(x)&.nil?` on `x = nil` fires `for nil`
  on BOTH arms.
- `x&.nil?` narrows exactly like `x.nil?` —
  `analyse_nil_predicate` has no `safe_navigation?` decline — truthy
  binds `x` to `nil`, falsey to its non-nil fragment (`Bot` on a nil
  carrier).
- Upstream `apply_safe_nav_non_nil` is a FALLBACK, not a post-pass over
  `nil?`'s result: `x&.nil?` on `T | nil` gives `(T, T)` (truthy keeps
  the non-nil fragment), on `nil` gives `(nil, Bot)`.

## Fix

All gate sites honour `LoweredAst::paren_unwrapped` (the lowering already
marks unwrapped ids):

- `loop_edges.rs subject_edges`: early arm before the method table —
  paren'd subject receiver or argument → `Keep/Keep`; paren'd ARGUMENT
  on a live receiver still honours `nil_safe_nav` (truthy `Empty`);
  `!`'s operand excluded. `class_edge` gained the `nil_safe_nav`
  parameter (`guard_suppression.rs` passes `false` — `when`/`in`
  conditions are never `&.` receivers).
- `dead.rs predicate_edge_locals` / `predicate_cmp_local`,
  `class_narrowing.rs guard_predicate`, `record_guards.rs`: paren'd
  reads skipped, paren'd writes kept.
- `dead.rs predicate_edge_locals_impl`: `NilQ` now collected through
  `safe_nav` for `nil?` only (`!*safe_nav || method == "nil?"`), fixing
  two live FPs — `x=nil; if x&.nil?` emitted `foo_b for nil` where the
  reference's falsey-edge scope is `Bot`; `x="s"` emitted `foo_a`.
- `loop_edges.rs` `empty?` falsy edge: `Nominal[Array]` detection via
  `index.class_name_for_id` (CORE index) — `source.class_name_for_id`
  answers `None` for a core-range `ClassId`, so `ENV.keys`/`Dir.glob`/
  `[1,2].shuffle` carriers wrongly widened. The round-1 review blocker.

`expr_scalar` deliberately does NOT gate parens: `(x)`'s pin must still
resolve for the VALUE fold — `always-truthy-condition` fires on both
engines on every row.

## Measured outcome

Every issue-brief row and the ~90-probe matrix is `=` except classified
adjacent gaps (all pre-existing, paren-independent): `gate.sh` pass;
`cargo +1.88.0 clippy` clean; **`fp_audit --gaps --sweep` 0 FP** over the
8-corpus standing set; CI green on `97cca16a`. Reviewer compared head vs
merge base `7baa4c9b` over ~135 rows: every change moves toward the
reference, no new port-only diagnostic. Opus 5.5 round 1 **Needs fix**
(`empty?` core-index) → fixed; round 2 fix-confirmation **Approved**.

New regression matrix: `issue_820_parenthesised_subject_matrix`
(`rigor-rules` tests, 22 oracle-measured rows incl. `&.`/`empty?`
controls).

## Surfaced follow-ups (filed, disclosed on the PR)

- **#838** — `typed_only_dead_arms` (#442) blanket-declines arms the
  reference's `propagate` fills with the edge scope (`x=[nil].first; if
  (x)&.nil?` misses `foo_b for nil`; `p || true` family same root).
- **#839** — `safe_navigation_call_type` value type: literal `nil&.m`
  never folds; inferred-nil receiver folds where the reference
  dispatches to `bool`.
- **#840** — `x&.is_a?(C)` on `T | nil` unions narrows nothing.
- **#841** — `while x < N` loop exit widens a literal-pinned local
  (`until x.nil?` twin, same `narrow_loop_exit_edge`).
- **#842** — `P && y` with a literal-truthy right operand drops the
  falsey edge's subject scope.
- **#843** — `if x = "q"` assignment-predicate write never reaches the
  live arm's env.
- **#844** — `y = x.is_a?(C)` value position types untyped (reference:
  `bool`).
- **#845** — `x.nil?`/`x&.nil?` on `T | nil` unions: `nilness` answers
  `None` → `Other/Other`; reference narrows memberwise.
- **#846** — `(x = v).m` write receivers never narrow (`Subject::of`
  skips `LocalVariableWrite`).
- **#847** — `x&.m` on a provably-nil carrier keeps edges the reference
  empties, in `while`/`until`/`return unless`/`and`/chain shapes (the
  `NilQ` fix covers if/ternary/unless only). Port FPs, base-identical.

Design companion: Fable 5.2 + Grok 4.6 quota-blocked → pi `devin/swe-2`
substitute (advisory; recorded per `docs/agents/review.md`).
