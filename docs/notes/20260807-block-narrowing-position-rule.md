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

Probes live in the session scratchpad (`snprobe/`); they are trivial to
recreate from the table.

## The rule the reference actually implements

A block body's class narrowing survives when the block-bearing call sits in
**statement position or an assignment RHS** (s1, s2, s3, s4, s8, s10), and is
LOST when the call's value is consumed as a **receiver of a further call**
(s5, s6, s7, s11) or as an **argument to another call** (s9).

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

## Fix (see PR)

1. **Delete the safe-nav-based decline** (`class_flow_expr`'s `if !safe_nav`
   guard around block descent, `crates/rigor-infer/src/lib.rs`). It is the
   wrong axis: it suppresses s3/s4, which the reference fires on, and it does
   not suppress s7/s11.
2. **Descend into a block body only from statement position** — a statement in
   a scope's statement list, or a local-variable-write RHS — mirroring the
   `stmt_position` flag review finding R2 already introduced for
   `class_flow_if`. A block-bearing call reached as a receiver or as an
   argument records nothing inside its block.

Declining by position is a strict subset of the reference in every row of the
matrix, which is the FP-safety argument.

## Why the sweep did not catch it

`fp_audit --gaps --sweep` was green (0 FP / 9204 files) both before and after
the PR #63 review round. The corpora simply do not contain a *witnessable*
narrowed call inside a block whose call is chained or passed as an argument —
the [fixture-corpus blind spot](20260731-survey-fp-triage-24.md) lesson again,
one level up: a green sweep is evidence about the corpora, not about the rule.
The matrix was built by asking what the reference's rule IS, not by grepping
for where it differs.
