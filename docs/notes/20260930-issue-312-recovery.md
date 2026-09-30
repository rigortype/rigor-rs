# 2026-09-30 — issue #312: compound index writes under recovery wrappers

PR #324, merged `d6b409d`. Three review rounds (r1: `joined` over-approximated;
r2: granularity overshot in three FP directions); all blocking items fixed.
Sweep 0 FP; CI 5/5 on `d324e03`. Parent re-probed the r2 FP classes on the
final head — all match the oracle.

## What landed

`collect_recoverable_children` gained `visit_index_{or,and,operator}_write_node`
plus a `ScopeMarks` model over positions where the reference re-merges or
discards a write's post-scope:

- `joined` — write pushes whole → `Node::IndexWrite` → receiver widening
  (rescue modifier, `&&`/`||` right, if/case/begin live arms, crossed
  block/lambda bodies).
- `blocked` — never evaluated or scope discarded (`super`/`yield`/`BEGIN`/
  `END`/`defined?` operands, `when` conditions, `in` patterns/guards,
  multi-target embedded exprs, dead/terminating arms).
- `iterative`/`next_sink`/`suppressed` — loop & crossed-block writeback
  (`loop_content_writeback`, `join_break_scopes`, `retry` edges) so
  `next`/`break`/`redo`/`retry`/`return`/`raise` inside iteration still
  widen, matching the reference's jump sinks.

## Rounds (measured)

- R1: `joined` wrongly covered `ensure`, `when`/`in` conditions, `super`/
  `yield` args, multi-target exprs, terminating arms, folded predicates →
  ~13 lost reference rows.
- R2: `blocked` wrongly dropped jump arms inside loops/blocks (scope
  writeback), `ElseNode` arm made the rescue-modifier check smarter than
  `branch_unconditionally_exits?` (strictly syntactic — `if…else` never
  counts), `case/in` without `else` counted a phantom no-match arm.
- Final: every r1/r2 row re-probed identical (modulo pre-existing
  `flow.always-truthy-condition` under-wrapper gap); `puts(*[h[:a] ||= 1])`
  control still fires `for 1`.

## Residuals

#325 (stored-value narrowing under transparent wrappers) and the disclosed
Bot-ending / non-literal-predicate folds stay open — all gap-side.
