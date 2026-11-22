# Issue #710 — `case`/`when`/`in` falsey-edge threading

PR #819, merged head `39ddcd1e` (PR head `fd660b6e` + a merge of master;
PR's own diff verified identical). `Refs #710` — the brief's "Coverage
gaps" section (ivar guard operands, a rooted operand naming the
enclosing class) is **not** delivered; the issue stays open for it.

Review: one-round Opus 5.5 adversarial review, `Verdict: Approved` at
`fd660b6e` after an independent 133-row re-probe against master
`b8b52ae4` and the pinned reference. Grok 4.6 advisory quota-blocked
(402); the Fable design companion ran substituted on
`opencode/beam-free` — claudeAgent Fable quota-dead, the opencode
Anthropic route rejected the key, Codex disabled (recorded in the PR
body).

## What was wrong

`class_flow_case` cloned every `when`/`in` clause body and the `else`
from the PRE-`case` facts. The reference's `eval_case_when_branches`
(`statement_evaluator.rb:2489`) threads a FALSEY scope through the
clauses in source order: a subject whose carrier a `when`/`in` class
test covers is `Bot` in every later clause and in the `else`. The port
therefore fired inside arms the reference had already eliminated —
live FPs on `case x in NoSuchZz` (body fired, else correctly fired),
on `when String` after `when Integer` over `x = 1`, and on every `in`
arm shape (never descended at all).

## What landed

- **`class_flow_case` rewritten around a threaded `falsey` env** —
  clause bodies (and the `else`) clone the accumulated no-match scope,
  including the conditions' own effects (`run_when_test`).
- **`CaseCond`** classifies each `when`/`in` operand the way
  `apply_case_when_condition` (`narrowing.rb:3254`) reads it: `Class`
  (constants resolved lexically, `nil`/`true`/`false` → singleton
  classes, non-integer ranges → `Numeric`/`String`), `IntLit`,
  `IntRange`, `Regexp`, `Unread`. The same classification drives the
  truthy meet (clause body) and the falsey subtraction
  (`fully_narrowable` gate — an integer range's two-piece complement or
  any unread member hands the entry scope back).
- **`case`/`in` arms descend.** A bare class pattern narrows like a
  `when` (`bare_class_pattern_node`); every other pattern narrows
  neither edge but still descends for call recording. Pattern-bound
  names (`in C => y`) die on the arm's `UnmodeledWrite` span.
- **`case`/`in` without `else`** pushes no else edge
  (`pattern_case_matches_every_path?` — `NoMatchingPatternError` has no
  continuation); the subject's post-`case` fact is the arm-edge union
  (`Widened` absorbs, `Bot` is the join identity, otherwise the fact
  drops to the tenv carrier).
- **`when_test_rebinds_subject?`** (reference #1963): once a `when`
  test writes the subject, no later clause narrows it.
- **`Node::Range::exclude_end`** populated from `range.is_exclude_end()`
  so `when a...b` decrements the literal upper bound
  (`case_equality_integer_range`).

## Measured

- Probe matrix (~40 rows authored + the review's 133-row re-probe):
  **every error/warning diagnostic matches the reference** — the brief
  rows, Bignum/i64-boundary literals and ranges, `...`, endless/
  beginless/negative bounds, splats, `&&` in conditions, `then`
  clauses, guards, deep/rooted `in` constants, `in … => y`, `case`/`in`
  with no `else`, Dynamic subjects, multi-byte source, nested `case`.
- Residual diffs: `flow.unreachable-clause` info rows only — the port
  catalogues the rule but does not emit it (coverage, safe side,
  acknowledged in #710).
- `fp_audit.py --gaps --sweep` on the release binary: **0 FP
  candidates** (re-verified on the master-merged tree after #763).
- `harness/gate.sh` green; clippy 1.88 `-D warnings` clean; CI green on
  `9d0c266f` (Linux, macOS, snapshot parity, live reference, docs).

## Follow-ups (filed from the review's non-blocking findings)

- **#820** — `when (C)`: a parenthesised class condition is unread in
  the reference; the port reads it as a class. FP on master **and** the
  branch, plus a newly lost coverage row.
- **#821** — `case` over-fires in `while`/`until` bodies, receiver/
  argument position, and after an exiting arm. All pre-existing on
  master.
- **#822** — residual `case`/`in` coverage cluster: prior-fact subjects
  (`case_cond_covers` reads tenv, not facts), union/int-range carriers,
  `in` pattern bindings, Dynamic arm-union post-case, `x += 1` in a
  test. Design companion's remediation: port `case_when_types` +
  `unmatched_pattern_result` + `join_case_branch_scopes` wholesale and
  extend `ClassFact` to carry a narrowed carrier set.
- #710 stays open for the brief's "Coverage gaps" section.
