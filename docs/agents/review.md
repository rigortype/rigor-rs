# Review

The review gate a draft PR passes before `gh pr ready` (`AGENTS.md` → Work
loop, step 7). A reviewer is adversarial and read-only: it builds the input that
would make the change wrong, probes it on both engines, and never edits or
merges.

## Who reviews

- **Claude Code subagents**: Opus 5.5, inheriting the session model.
- **Reviewers from other agents**: Grok 4.6 and Opus 5.5, both at `high`
  thinking, run as two independent reviews. Neither sees the other's output,
  and **both must return `Approved`**. They catch different things: on
  PR #154, Grok approved while Opus found six message regressions the PR text
  denied.

Review once per PR, on the final head after CI is green, not on every push. A
`Needs fix` sends the PR back to the implementer, and the review runs again on
the new head. Read CI (`gh pr checks`) rather than re-running its gates; spend
the time on probes and counterexamples.

## Input

- The PR number and head SHA.
- The issue's agent brief (its acceptance criteria are the contract).

## What to check

- **Parity claims.** Re-probe every row of the PR's probe tables on both
  engines (`AGENTS.md` → Probing). Diff the full tuple, message included.
- **Must-still-fire controls.** A suppression needs a nearby row that still
  fires. Add a control the PR is missing.
- **Counterexamples.** Construct shapes the PR does not test: other receivers,
  argument shapes, project `sig/`, and value edges (literals past `i32` and
  `i64`, `&.`, control and multi-byte characters, interpolated or mutated
  receivers). A green gate is not evidence for a shape the gate cannot see.
- **Subset arguments.** "The port only declines where the reference would",
  "we handle a subset of its receivers": such arguments quantify over a term
  (`Dynamic`, "block", "narrower") whose meaning differs between the engines.
  They have been wrong five times. Probe each term on both engines, in both
  directions, including where the port is precise and the reference
  collapses.
- **Negative claims.** Every "no regression", "identical" or "no new key" in
  the PR body or its note needs a probe that could have falsified it. A
  same-key message change is the usual miss: the (rule, line, col) gates and
  the sweep diff both pass it.
- **Scope.** The diff delivers the brief and nothing outside it.

## Output

Report the verdict, and make the report's **last line** exactly one of
`Verdict: Approved`, `Verdict: Needs fix` or `Verdict: Blocked — need human`.

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
