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
new head.

```bash
pi -p --model xai/grok-4.6:high --append-system-prompt docs/agents/review.md "Review PR #N at head <sha>."
```

```bash
pi -p --model claude-bridge/claude-opus-5-5:high --append-system-prompt docs/agents/review.md "Review PR #N at head <sha>."
```

`pi` loads `AGENTS.md` itself. If a model id stops resolving
(`pi --list-models grok-4.6`), use the same model through another provider
(`opencode/claude-opus-5-5`, `opencode/grok-4.6`). If no provider serves it,
report `Blocked — need human`. A cheaper model does not stand in for it.

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
- **Scope.** The diff delivers the brief and nothing outside it.

## Output

End with exactly one verdict:

- `Approved`, followed by:
  1. **a PR body revision draft**: title, summary, probe tables and gate
     numbers rewritten so every claim matches the diff, with `Closes #N` only
     when the brief is fully delivered (`Refs #N` otherwise);
  2. **suggested PR comments** for what the body cannot carry (caveats,
     non-goals, follow-ups), or `[]`.
- `Needs fix`, followed by a checklist the implementer can act on. Each item
  carries its counterexample: input, expected (reference) output, actual (port)
  output.

Withhold `Approved` while the PR text claims something the diff does not
deliver: either return `Needs fix`, or correct the claim in the body draft.
