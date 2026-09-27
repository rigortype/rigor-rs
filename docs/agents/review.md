# Review

The review gate a draft PR passes before `gh pr ready` (`AGENTS.md` → Work
loop, step 7). A reviewer is adversarial and read-only: it builds the input that
would make the change wrong, probes it on both engines, and never edits or
merges.

## Merge bar (2026-09-27 policy)

A PR's purpose is resolving the scope in its issue's agent brief. Per-PR full
parity with the reference is NOT required — convergence is measured across the
release, not inside one PR. The reviewer's job is to *surface* concerns, not to
veto on them:

- **Blocks landing**: the brief's rows still diverge, a gate is red, or the
  diff causes harm OUTSIDE any reasonable reading of the brief (a broad new-FP
  family on ordinary code — e.g. an env-model change firing on plain locals).
- **Does not block**: new FPs in edge shapes adjacent to the fix, message
  drift at the same key, coverage lost vs master, missing precision. These get
  filed as issues, disclosed in the PR body, and the PR lands. When a finding
  could be either, prefer filing + landing over another fix round — a `Needs
  fix` round costs more than a filed issue and a small follow-up PR.

Cap review rounds at two per PR. After that, everything outstanding is filed
and the PR lands or is abandoned; it does not loop. The full-parity review
that produced the 2026-09-27 abandoned stream is recorded as an anti-pattern
in `docs/notes/20260927-abandoned-infer-pr-stream.md`.

## Who reviews

- **Claude Code subagents**: Opus 5.5, inheriting the session model. Opus is
  the review gate; one `Approved` is enough.
- **Reviewers from other agents**: Grok 4.6 (`high`) may run as a second
  independent review when available — it catches different things (on PR #154
  it approved while Opus found six message regressions), but its verdict is
  advisory, not blocking.

Review once per PR, on the final head after CI is green, not on every push. A
`Needs fix` sends the PR back to the implementer only for landing-blocking
items; every other finding in it is filed as an issue and disclosed in the PR
body, and the review does not re-run on them. Read CI (`gh pr checks`) rather
than re-running its gates; spend the time on probes and counterexamples.

Alongside each adversarial review, run a **Fable 5.1 design companion**
(`claude -p … --model claude-fable-5-1`, or `opencode/claude-fable-5-1`), fed
the same diff and prior findings. Its job is not probing but design: for every
repeated `Needs fix` family it proposes a root-cause remediation — the
reference function to port wholesale rather than patch branch-by-branch — so
the implementer gets a fix that closes the family, not the instance. Bounce
the companion's proposal against the adversarial findings before it reaches
the implementer.

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
