# Abandoned infer-PR stream (#177 / #183 / #184) — lessons

2026-09-27. Three draft PRs — #177 (issue #164, literal/nilable folding),
#183 (issue #167, rescue/for flow joins), #184 (issue #138, HashLookupMutation)
— were abandoned unmerged after 6–8 adversarial review rounds each and well
over ten wall-clock hours. Branches keep the work; this note keeps the record.

## What happened

Each PR sat on the deepest part of `crates/rigor-infer/src/lib.rs` (and
`rigor-parse/src/ast.rs`), running the full-parity review bar from
`docs/agents/review.md`: zero new FPs, full-tuple probes, must-still-fire
controls. Every round was productive in the narrow sense — the reviewer found
real new FPs — but the findings were successive branches of the same 2–3
reference functions (`eval_rescue_modifier`, `branch_terminates`,
`join_member_bindings`, `thread_operand`). The review was discovering the
reference's decision table one row per round, and each fix added machinery
(env threading, `CheckEnvs`, flow events, retry seeds) whose own attack
surface seeded the next round's findings. Final state: all three heads
(`6e86120` / `9502a3a` / `eb1edd3`) had all-green CI and each still carried a
fresh list of new-FP blockers.

Compounding factors:

- **Shared files.** All three PRs (plus merged #180) edited the same two
  files at +1000 lines each; every merge forced a rebase + re-review cascade.
- **Fixture-number races.** #177 and #184 both claimed 117/118 while #179/#180
  landed them — orchestrator bookkeeping, not agent error.
- **Review-vs-head races.** Implementers pushed while a review probed, so
  verdicts landed on mixed heads.
- **No round cap.** The gate had no exit condition; "one more Needs fix" was
  always individually justifiable.

## What remains valuable (kept on the branches)

- `#177` @ `6e86120`: equality-predicate narrowing on trusted finite domains,
  loop-body write back-edges, retry widening, safe-nav arg handling. Remaining
  known blockers are listed in `/tmp/review-177-final-opus.txt` and the PR's
  closing comment.
- `#183` @ `9502a3a`: operand-freeze (`thread_operand`), `&.` argument joins,
  `case…in` guard isolation, jump-sink env model. Remaining: the
  `nil_flow_rescue_join` and nil-fact-survival FP families (32 rows).
- `#184` @ `eb1edd3`: ordered `ApplyEvent` replay, positional `CheckEnvs`,
  typed-only regions, block-scope write census. Remaining: statement-entry
  value typing, missing back-edges in region seeds, masgn region start.
- Follow-up issues filed during the stream stay valid: #190–#203.

## Policy change (from this)

- Review gate updated: the PR lands on issue-scope resolution; everything
  else is a filed concern (`docs/agents/review.md` → Merge bar). Two-round
  cap; concerns are disclosed, not vetoed.
- Per-PR parity with the reference is not expected; parity is a release-level
  property tracked by the sweep and the filed issues.

## Process lessons for the next attempt

1. **Spec-first for reference-mechanism ports.** Before implementing, make a
   decision-table inventory of the target reference functions and probe every
   row of it. The reviews this stream paid for were, in effect, that table —
   rediscovered expensively, one branch per round.
2. **One inference-PR at a time**, or partition by file. Three streams on one
   file produced rebase cascades, not throughput.
3. **Freeze the head during review**; a push mid-review voids the verdict.
4. **Prefer declines for landing.** Where parity is unverified, silence is
   free; an FP is not. The safe-side gate would have landed all three PRs
   rounds earlier.
5. The `lib.rs` module split designed during this stream (9 modules) is now
   unblocked — no open PRs touch the file.
