# Review

The review gate a draft PR passes before `gh pr ready` (`AGENTS.md` → Work
loop, step 7). A reviewer is adversarial and read-only: it builds the input that
would make the change wrong, probes it on both engines, and never edits or
merges.

## Who reviews

| Harness | Model | Thinking | Role |
| --- | --- | --- | --- |
| Claude Code subagent | Opus 5.5 (inherit the session model) | session default | the review pass when the orchestrator is a Claude session |
| `pi` | `xai/grok-4.6` | `high` | external pass A |
| `pi` | `claude-bridge/claude-opus-5-5` | `high` | external pass B |

The two external passes run on every PR as independent reviews. Neither sees
the other's output, and **both must return `Approved`**. A `Needs fix` from
either sends the PR back to the implementer, and both passes run again on the
new head. The two catch different things: on PR #154, Grok approved while Opus
found six message regressions the PR text denied.

```bash
harness/review.sh <PR number>
```

It builds the PR head once in a worktree, starts both passes there at once
(`pi -p`, read-only tools, this checkout's copy of this file as the appended
prompt), and prints each verdict. A pass takes about 15 minutes. Both ids are
subscription-backed. If either stops resolving (`pi --list-models grok-4.6`),
report `Blocked — need human`. Neither a pay-per-use provider route nor a
cheaper model stands in for it.

`claude-bridge` runs the
Claude Code bundled in its own `@anthropic-ai/claude-agent-sdk`, not the one on
`PATH`. A `400 … does not support this model` means that bundle is too old.
`pi update --extensions` leaves it pinned, so update it directly with
`npm --prefix ~/.pi/agent/npm update @anthropic-ai/claude-agent-sdk`.

## Input

- The PR number and head SHA.
- The issue's agent brief (its acceptance criteria are the contract).

## What to check

- **Parity claims.** Re-probe every row of the PR's probe tables on both
  engines (`AGENTS.md` → Probing). Diff the full tuple, message included.
- **Must-still-fire controls.** A suppression needs a nearby row that still
  fires. Add a control the PR is missing.
- **Counterexamples.** Construct shapes the PR does not test: other receivers,
  argument shapes, value edges, project `sig/`. A green gate is not evidence
  for a shape the gate cannot see.
- **Negative claims.** Every "no regression", "identical" or "no new key" in
  the PR body or its note needs a probe that could have falsified it. A
  same-key message change is the usual miss: the (rule, line, col) gates and
  the sweep diff both pass it.
- **Scope.** The diff delivers the brief and nothing outside it.

## Output

Report the verdict, and make the report's **last line** exactly one of
`Verdict: Approved`, `Verdict: Needs fix` or `Verdict: Blocked — need human`
(`harness/review.sh` reads that line and nothing else).

- `Approved` comes with:
  1. **a PR body revision draft**: title, summary, probe tables and gate
     numbers rewritten so every claim matches the diff, with `Closes #N` only
     when the brief is fully delivered (`Refs #N` otherwise);
  2. **suggested PR comments** for what the body cannot carry (caveats,
     non-goals, follow-ups), or `[]`.
- `Needs fix` comes with a checklist the implementer can act on. Each item
  carries its counterexample: input, expected (reference) output, actual (port)
  output.

Withhold `Approved` while the PR text claims something the diff does not
deliver: either return `Needs fix`, or correct the claim in the body draft.
