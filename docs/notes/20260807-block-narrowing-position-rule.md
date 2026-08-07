# Block-body narrowing survives by POSITION, not by safe-nav — two live FPs on master (2026-08-07)

PR #63 (class narrowing) declined block-body descent under **safe-nav** after
the sweep caught rigor-rs firing where the reference is silent
(gitlab-foss `import_export/group/relation_tree_restorer.rb:213-215`). That
decline picked the **wrong axis**. The full probe matrix below shows safe-nav
is irrelevant; what decides it is the block-bearing call's **syntactic
position**. Consequence: the merged code loses coverage on two shapes the
reference does fire on, and **emits two false positives** the reference does
not — a live violation of the ADR-0002 zero-FP contract on master.

## The matrix

Probe body (each file is one `def f(h)` with the block-bearing call in a
different position); the narrowed call is `v.frobnicate_zzz` guarded by
`v.is_a?(String)`, so a surviving narrowing must fire `undefined method
'frobnicate_zzz' for String`. Reference = pinned `v0.3.1`, fresh cwd,
`--no-cache`, plugin path pinned. rigor-rs = `target/release/rigor` at
`d6aa6e5` (post-merge).

| # | shape | position | ref | rigor-rs | verdict |
|---|---|---|:--:|:--:|---|
| s1 | `h.transform_values { … }` | tail statement | 1 | 1 | agree |
| s2 | `h.transform_values do … end` | tail statement | 1 | 1 | agree |
| s3 | `h&.transform_values { … }` | tail statement | 1 | **0** | coverage lost needlessly |
| s4 | `h&.transform_values do … end` | tail statement | 1 | **0** | coverage lost needlessly |
| s5 | `h&.transform_values do … end&.compact` | receiver | 0 | 0 | agree (accidentally) |
| s6 | `h&.transform_values { … }&.compact` | receiver | 0 | 0 | agree (accidentally) |
| s7 | `h.transform_values { … }.compact` | receiver | 0 | **1** | **FALSE POSITIVE** |
| s8 | `x = h.transform_values { … }` | assignment RHS | 1 | 1 | agree |
| s9 | `g(h.transform_values { … })` | call argument | 0 | **1** | **FALSE POSITIVE** |
| s10 | `h.transform_values { … }` then `nil` | statement, discarded | 1 | 1 | agree |
| s11 | `h.transform_values { … }.compact.to_a` | receiver (2 deep) | 0 | **1** | **FALSE POSITIVE** (same as s7) |
| s12 | `return h.transform_values { … }` | `return` operand | 0 | **1** | **FALSE POSITIVE** |
| s13 | `x = g(h.transform_values { … })` | argument (assigned) | 0 | **1** | **FALSE POSITIVE** |

The same rule governs `case`/`when` narrowing, and `if`/ternary is the
exception that proves it:

| # | shape | position | ref | rigor-rs | verdict |
|---|---|---|:--:|:--:|---|
| p6 | `case v when Hash then v.zzz end` | tail statement | 1 | 1 | agree |
| p1 | `x = case v when Hash … end` | assignment RHS | 1 | 1 | agree |
| p2 | `g(case v when Hash … end)` | call argument | 0 | **1** | **FALSE POSITIVE** |
| p3 | `(case v when Hash … end).to_s` | receiver | 0 | **1** | **FALSE POSITIVE** |
| p7 | `return case v when Hash … end` | `return` operand | 0 | **1** | **FALSE POSITIVE** |
| p4 | `g(v.is_a?(Hash) ? v.zzz : v)` | ternary as argument | 1 | 1 | agree — `if` narrows here |
| p8 | `(v.is_a?(Hash) ? v.zzz : v).to_s` | ternary as receiver | 1 | 1 | agree — `if` narrows here |
| p5 | guard in a block nested in a block | statement | 1 | 1 | agree |

Five further rows measured while building the fix (the `x` block pins the
carriers neither matrix above covers; all against the same pinned reference):

| # | shape | position | ref | rigor-rs | verdict |
|---|---|---|:--:|:--:|---|
| x1 | `g(case k when Integer then h.transform_values { … } end)` | block in an expr-position `case` clause | 0 | **1** | **FALSE POSITIVE** |
| x2 | `g(k ? h.transform_values { … } : nil)` | block in an expr-position ternary branch | 0 | **1** | **FALSE POSITIVE** |
| x3 | `a, b = h.transform_values { … }` | multi-write RHS | 1 | 1 | agree |
| x4 | `x \|\|= h.transform_values { … }` | op-write RHS | 1 | 1 | agree |
| x5 | `if k then h.transform_values { … } end` | statement inside a statement `if` | 1 | 1 | agree |

x1/x2 make **ten** FP shapes, not eight, and they are the rows that decide the
SHAPE of the fix: expression position must propagate *through* the branch and
clause bodies of a conditional, not merely gate the narrowing at the
construct's own node. x3/x4 show "assignment RHS" covers **every** local
write carrier (`=`, `a, b =`, `||=`), not just `LocalVariableWrite`.

Probes live in the session scratchpad (`snprobe/`); they are trivial to
recreate from the tables.

## The rule the reference actually implements

For **block bodies and `case`/`when` clauses**, class narrowing survives only
when the construct sits in **statement position or as the direct RHS of a
local-variable write** (s1–s4, s8, s10, p1, p6). It is LOST when the value is
consumed as a **receiver** (s5–s7, s11, p3), as an **argument** (s9, s13 —
even when the outer call is itself assigned), or as a **`return` operand**
(s12, p7).

For **`if`/ternary**, branch-internal narrowing happens in EVERY position
(p4 argument, p8 receiver both fire) — `scope_indexer.rb`'s
`propagate_if_branches` gives expression-position conditionals their own
treatment, which `case` and block bodies never got. Only the *early-return
propagation past* an `if` is statement-only, which the PR #63 review already
closed as finding R2.

Ten false-positive shapes in total: five for blocks (s7, s9, s11, s12, s13),
three for `case` (p2, p3, p7), and two where an expression-position conditional
carries a block in one of its bodies (x1, x2).

That is the same statement-vs-expression asymmetry this project already found
in the reference's `if` handling: `StatementEvaluator#eval_if` threads narrowed
scopes, while a receiver or argument is typed by `ExpressionTyper`, which does
not. The 2026-08-07 adversarial review of PR #63 closed the `if` half of it
(review finding R2, "early-return propagation is statement-position-only");
the block half was mis-attributed to safe-nav and left open.

Note the reference's behaviour is arguably a defect on its own terms — whether
a guard narrows should not depend on whether the enclosing expression is later
chained — but it is a COVERAGE inconsistency, not an unsoundness, so it costs
us nothing to mirror. Worth carrying into the next upstream feedback batch.

## Fix (as built — PR "fix(infer): position rule for block/`case` narrowing")

`stmt_position` is threaded through the WHOLE class-narrowing pass rather than
gated at individual nodes, because x1/x2 show the position has to survive a
descent through a conditional's bodies. `class_flow_scope`, `class_flow_stmt`,
`class_flow_expr` and `class_flow_case` each take the flag (`class_flow_if`
already had it, for review finding R2); `crates/rigor-infer/src/lib.rs`.

1. **The safe-nav decline is deleted.** `class_flow_expr`'s block descent is
   now gated on `stmt_position` instead of `!safe_nav` — the wrong axis: it
   suppressed s3/s4, which the reference fires on, and suppressed neither
   s7 nor s11. The separate safe-nav decline on *recording a use*
   (`value&.frobnicate` never narrows) is untouched.
2. **Statement position is established** at a method/program body and
   **propagated** through: statement lists, the branch bodies of an `if`, the
   clause and `else` bodies of a `case`, and the RHS of `LocalVariableWrite` /
   `MultiWrite` / `LocalVariableOpWrite` (x3, x4). It is **dropped to `false`**
   at a call receiver, a call argument, a `return` operand, an `if` predicate,
   a `case` subject and `when` conditions, and both sides of a `Logical`.
3. **`class_flow_case` takes the same gate**: `subject` is `.filter(|_| stmt_
   position)`, so an expression-position `case` narrows no clause (p2, p3, p7),
   and its clause bodies inherit the flag so a block nested inside one narrows
   nothing either (x1).
4. **`if`/ternary is left alone**: `class_flow_if` still narrows its branches
   unconditionally (p4, p8 fire), and only the early-return propagation past it
   stays statement-only (R2). Its branch bodies do inherit the flag, which is
   what closes x2.

Everything else is unchanged: the R1 arg-position write threading, the R3
conflicting-guard removal, mutation/write invalidation, the Dynamic/Top gate,
and the lexical shadow gate.

Declining by position is a strict subset of the reference in every row of all
three matrices, which is the FP-safety argument. All 26 rows are pinned as a
table-driven unit test (`class_narrowing_position_matrix`), and the corpus
fixture `harness/corpus/81_class_narrowing.rb` carries a safe-nav positive
(case 9, s3) plus chained-receiver and `case`-as-argument negative controls
(cases 10 and 11, s7/p2).

## Why the sweep did not catch it

`fp_audit --gaps --sweep` was green (0 FP / 9204 files) both before and after
the PR #63 review round. The corpora simply do not contain a *witnessable*
narrowed call inside a block whose call is chained or passed as an argument —
the [fixture-corpus blind spot](20260731-survey-fp-triage-24.md) lesson again,
one level up: a green sweep is evidence about the corpora, not about the rule.
The matrix was built by asking what the reference's rule IS, not by grepping
for where it differs.
