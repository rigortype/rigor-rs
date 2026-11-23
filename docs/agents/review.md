# Review

The review gate a draft PR passes before `gh pr ready` (`AGENTS.md` → Work
loop, step 7). A reviewer is adversarial and read-only: it builds the input that
would make the change wrong, probes it on both engines, and never edits or
merges.

## Merge bar

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
and the PR lands or is abandoned; it does not loop. The cap binds the primary
(style) pass too: each round must list ALL findings, and a fix-confirmation
re-review checks only whether listed items landed — new non-blocking findings
are filed, not bounced. A finding that removes a clone/allocation must name a
compilable alternative — borrow constraints through `&mut` calls have made
three such findings non-actionable. The full-parity review that produced the
2026-09-27 abandoned stream is recorded as an anti-pattern in
`docs/notes/20260927-abandoned-infer-pr-stream.md`.

## Who reviews

- **Claude Code subagents**: Opus 5.5, inheriting the session model. Opus is
  the review gate; one `Approved` is enough.
- **Reviewers from other agents**: Grok 4.6 (`high`) may run as a second
  independent review when available — it catches different things, and has
  approved PRs Opus found regressions in, so its verdict is advisory, not
  blocking.
- **Substitute reviewers**: when OpenCode providers or Grok are quota-blocked,
  Codex (`gpt-5.6-sol`) may run either pass with the same prompt and contract —
  record the substitution in the PR body. Always attach the diff INLINE in the
  prompt and forbid tool calls: a tool-driven reviewer can spend its whole
  budget reading and produce no verdict. Probe failures
  cheaply — try each provider once, then move on; do not retry a hang.

## Review tiers

A PR carries at most one of these labels. **No review-tier label means
`review:full`.**

- **`review:full`** (the default). The adversarial review in "What to check":
  re-probe parity claims, must-still-fire controls and counterexamples.
  Required for any change to engine behaviour, gates, or fixtures that affect
  diagnostics. Before merge, the approved SHA must have green required CI
  **and** a green standing-set `fp_audit` (`python3 harness/fp_audit.py
  --gaps --sweep`).
- **`review:light`**. Documentation or comments only — prose, comments, typo
  fixes in rustdoc. The diff must **not** touch `harness/`,
  `harness/docs_check.py`, `AGENTS.md`, or `docs/agents/review.md`. Do not
  spend an Opus adversarial re-probe; a short read that the diff stays in
  this scope, plus green `docs.yml`, is enough. If the diff is out of scope
  for light, drop the label and review as `full` (or `human`).
- **`review:human`**. The human-only paths in "Self-merge", and any PR so
  labelled. Agents do not self-merge these.

The implementer may apply `review:light` or `review:human` when the diff
qualifies; a mis-labelled PR is re-tiered by the reviewer, not merged under
the wrong bar.

## When to start the review

Start the review on a **feature-complete** head, **in parallel with CI**, not
after CI is green. Review once per PR on that head, not on every push. Do not
wait for `gh pr checks` to start the review; if checks have already finished,
read them rather than re-running those gates, and spend the time on probes
and counterexamples.

`gh pr ready` and self-merge still wait until the **approved SHA** has green
required CI (and, for `review:full`, a green standing-set fp_audit). A
`Needs fix` sends the PR back to the implementer only for landing-blocking
items; every other finding in it is filed as an issue and disclosed in the PR
body, and the review does not re-run on them.

Alongside each adversarial review, run a **Fable 5.1 design companion**
(`claude -p … --model claude-fable-5-1`, or `opencode/claude-fable-5-1`), fed
the same diff and prior findings. Its job is not probing but design: for every
repeated `Needs fix` family it proposes a root-cause remediation — the
reference function to port wholesale rather than patch branch-by-branch — so
the implementer gets a fix that closes the family, not the instance. Bounce
the companion's proposal against the adversarial findings before it reaches
the implementer.

## Self-merge

An `Approved` PR is merged by the agent that owns it; a reviewer never merges.
Every condition below holds at the moment of `gh pr merge N --merge`, or the PR
waits for a human:

- **Verdict.** `Verdict: Approved` from a read-only reviewer other than the
  implementer, on the current head SHA, with the PR body revised as the verdict
  asked. `Needs fix` and `Blocked — need human` never self-merge. After the
  two-round cap, a human decides.
- **No drift since approval.** The head is the approved SHA, or the only
  commits after it are merges of `origin/master`. Compare the PR's own diff
  before and after (`git diff <base>...<head>`, test files aside); any other
  change goes back for a fix-confirmation review.
- **Green on the approved SHA.** Required checks pass on that head
  (`docs-check`, and — once the merge-queue ruleset is active — the
  `CI required` fan-in; until then, the code CI jobs when the diff is
  not docs-only). For `review:full`, the standing-set `fp_audit --sweep`
  is also green on that tree. A `review:light` PR needs green `docs.yml`
  only. Do not block on checks documented as advisory. macOS workspace tests (`ci-macos.yml`, check name `build + test (macos, advisory)`) are not part of `CI required`: they do not run on merge_group or on ordinary PRs, only on a path-filtered pull_request, nightly, dispatch, or pre-release. If that check appears, ignore it when deciding whether required checks passed.
- **Enqueue, do not wait for the previous merge's master CI.** Two PRs
  that are each green can break master together: #517 and #522 did on
  2026-11-08, and master went red until #565. Once the master merge
  queue is enabled (repo ruleset, applied separately), enqueue with
  `gh pr merge N --merge` and stop waiting on `gh run list --branch
  master` for an earlier merge — the queue's `merge_group` run is the
  combination check. The approved SHA remains pinned: if the queue
  reports the head moved other than a queue rebase / merge of
  `origin/master`, go back for a fix-confirmation review. Until that
  ruleset is active, keep the previous one-at-a-time rule: before the
  next self-merge, `gh run list --branch master` shows CI passing on
  the previous merge commit; PRs whose changed files do not overlap may
  go out of order. `gh pr view N --json mergeStateStatus` should be
  `CLEAN` (or `STABLE` / in-queue once the ruleset is on).
- **Stacks.** Merge (or enqueue) the parent first, then
  `gh pr edit N --base master`, wait for `CLEAN`, and treat a conflict
  as a needed rebase, not a merge.
- **Human-only, whatever the verdict:** a `review:human` label; a pin bump
  or change under `reference/`, `.github/`, `harness/gate.sh` or
  `harness/docs_check.py`, an ADR, a change to `AGENTS.md` or
  `docs/agents/review.md`, and a slice whose measured outcome is "0 gaps
  closed" (AGENTS.md → Probing, "Measure before you build").

After the merge: the fold step (AGENTS.md step 8), then release the PR's
worktree (`git worktree remove`; `--force` for a populated submodule once
`git status` is clean).

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
- **What a decline feeds.** A value that stops folding can turn an arm the
  reference treats as dead into a live one, and narrowing is lost with it.
  Probe where a declined value feeds dead code and narrowing, not only its
  direct users.
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
