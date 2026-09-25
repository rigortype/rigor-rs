# AGENTS.md

rigor-rs is a faithful Rust port of the Ruby reference (`reference/rigor`, a
pinned submodule). **The reference is the oracle.** For any behaviour, read the
reference source AND probe the oracle; never reconstruct semantics from memory.

## Where things live

- **Issues** (GitHub, `gh` CLI; external PRs are also a triage surface):
  `docs/agents/issue-tracker.md`. Triage labels: `docs/agents/triage-labels.md`.
- **Domain language + decisions**: `CONTEXT.md`, `docs/adr/`
  (`docs/agents/domain.md`).
- **Oracle invocation + pin bumps**: `UPSTREAM.md`. Read its three "Oracle
  invocation hazard" sections before your first probe.
- **What to pull next**: `docs/CURRENT_WORK.md` (the baton). Subsystem map:
  `docs/PORT_BACKLOG.md`. Measured outcomes: `docs/notes/`.

## Work loop (one issue → one PR)

Async agents share one GitHub account, so the claim protocol below is the only
thing that stops two agents taking the same issue.

1. **Pick.** An open `ready-for-agent` issue without `in-progress` and without
   a linked open PR:
   `gh issue list --label ready-for-agent --search '-label:in-progress'`, then
   `gh issue view N --json closedByPullRequestsReferences`. The agent brief
   comment on the issue is the contract; the body is context.
2. **Claim.** `gh issue edit N --add-label in-progress`, then comment
   `Claimed: branch claude/issue-N-<slug>`. Re-read the comments: if an earlier
   claim exists with no abandon comment after it, the earlier one wins. Remove
   your comment and return to step 1.
3. **Branch.** A fresh worktree on `claude/issue-N-<slug>` cut from
   `origin/master`.
4. **Investigate.** Read the reference code path, probe both engines on the
   brief's rows (see *Probing*), and confirm the claim reproduces on master
   before writing code.
5. **Open the draft early.** After the first commit, push with an explicit
   refspec: `git push origin HEAD:refs/heads/claude/issue-N-<slug>`.
   `push.default = tracking` sends a bare `git push -u` to **master**. Then run
   `gh pr create --draft` with `Closes #N` in the body, and remove
   `in-progress`. From here the draft PR is the in-flight state.
6. **Implement until every gate is green** (see *Gates*). Record the measured
   outcome in the PR body: the probe tables and the gate numbers.
7. **Review, then ready.** The orchestrator (or maintainer) re-runs the
   gates and gets `Approved` from the review gate (`harness/review.sh N`;
   contract in `docs/agents/review.md`), whose passes re-probe the parity
   claims themselves. Only then
   `gh pr ready`. A non-draft PR means "reviewed, mergeable".
8. **Fold after merge.** Write the detail into a dated `docs/notes/` file or
   an ADR, then add one ledger line to `docs/CURRENT_WORK.md`.

**Abandoning a claim:** remove `in-progress`, and comment on the issue with what
you learned and the branch holding any work worth keeping. The ADR or issue text
is the durable record; a branch alone is not.

## Gates

Each must exit 0 and be read by its exit code, never through `grep`.
Clippy's ANSI-coloured lines defeat a line count.

- `cargo test --workspace --locked`
- `cargo +1.88.0 clippy --workspace --all-targets --locked -- -D warnings`,
  in a fresh `CARGO_TARGET_DIR`. CI pins 1.88, and a newer local clippy
  disagrees (e.g. `only_used_in_recursion`). An incremental target hides
  warnings.
- `ruby harness/run.rb` and `ruby harness/run_snapshot.rb`: 0 unregistered FP.
- `cargo build --release` then `python3 harness/fp_audit.py --gaps --sweep`:
  0 FP over the standing set (`harness/sweep-corpora.yml`). It measures
  `target/release` and scores a crashing port as `[]`, so a stale binary
  passes silently.
- `python3 harness/docs_check.py` whenever docs change (CI `docs` job).
- **Fresh-dir parity probes** (`harness/probe.py`) on every row the change
  touches, plus **must-still-fire controls**. A suppression is only proven
  when a nearby row still fires.

What the gates cannot see, so probe it by hand:

- **Project `sig/`**: the harness and the sweep run core+stdlib only. Probe
  a small project with `harness/probe.py --dir`.
- **Message drift**: the harness keys on (rule, line, col). Diff the full
  tuple, message included.
- **Retractions**: a site the reference stops flagging is invisible to the
  snapshot diff. Only the sweep sees it.

## Probing

- **Probe with `harness/probe.py`**:
  - `-e 'SRC'` / `-f FILE` compare `check` diagnostics as full tuples plus the
    exit code.
  - `--dir PROJ -e 'SRC'` runs the same inside a copy of a project, for
    project `sig/` and `.rigor.yml`.
  - `--dir PROJ -- ARGS` compares any subcommand's stdout, stderr and exit
    code: CLI, config, baseline and output formats. Channels are a contract:
    baseline `generate` writes to stderr, `drift` to stdout.

  It gives each engine a fresh cwd, pins the checkout plugin, passes
  `--no-cache` to the reference only, and refuses a stale port binary. Each of
  those has produced a false result in a hand-rolled loop.
- By hand, the oracle command is
  `ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib reference/rigor/exe/rigor check --no-cache …`,
  run from a fresh cwd with explicit file lists.
- Distrust a surprising number until the harness reproduces it. The audit
  harness itself has been wrong.
- **Measure before you build.** A coverage slice needs a `fp_audit --gaps`
  count predicting that it closes gaps. FP-safe flow slices have repeatedly
  closed 0 gaps (`docs/notes/20260706-flow-frontier-exhausted.md`).

## Parity bars

- **`check` is diagnostic**: a strict zero-FP subset of the reference. Never
  emit a diagnostic the reference doesn't. Declining (a coverage loss) is always
  the safe side.
- **`sig-gen` and other generative tools**: byte-identity on the methods BOTH
  tools emit. Where the port's sound inference is more precise than the
  reference's current gaps, the extra signature is coverage, not an FP. Add a
  guard only to fix port unsoundness, to match a permanent reference design
  decision, or to skip an emit the port cannot yet elaborate. Prefer porting the
  reference's inference at the source over per-case output guards; the
  reference converges toward more precision.

## Orchestrating subagents

- Claude Code subagents run on Opus 5.5 for every role: investigation,
  implementation and review. Leave `model` unset so they inherit the session
  model. Where the stakes are high, run two independent investigations.
- An implementer works in an isolated worktree, from a spec that names the
  likely mis-implementations and requires the full *Gates* list. It may
  resolve a spec-vs-oracle conflict toward the oracle; the review confirms it.
- Reviewers from other harnesses are Grok 4.6 and Opus 5.5, both at `high`,
  run together (`docs/agents/review.md`).

## Docs hygiene

- Record hard-to-reverse, surprising, real-tradeoff decisions as ADRs, with
  the slice's **measured** outcome (even "0 gaps, deferred").
- `docs/CURRENT_WORK.md` holds Now/Next plus one ledger line per closed arc.
  Byte budgets are enforced by `harness/docs_check.py`.
- Small doc-only changes go straight to master; code goes through a PR.
