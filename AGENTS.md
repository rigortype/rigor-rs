# AGENTS.md

## Agent skills

### Issue tracker

Issues live in GitHub Issues (via the `gh` CLI); external PRs are also a triage surface. See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical triage roles use their default label strings (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.

## Working discipline (hard-won; 2026-07-06 session)

These are load-bearing lessons paid for in this codebase — violating them wastes a
session. Read before doing coverage/parity or productization work.

### Measure before you build; never ship a speculative slice

- **The zero-FP bar is measured against the oracle, not argued.** A slice is
  gated by `harness/run.rb` + `harness/run_snapshot.rb` (fixtures, 0 unregistered
  FP) AND `harness/fp_audit.py --gaps` across the survey corpora.
- **Do NOT build a coverage slice without a valid-mode `fp_audit --gaps` count
  predicting it closes gaps.** Three consecutive FP-safe flow slices closed 0
  survey gaps this session (shape `Type::Tuple`, project-method nilable-return) —
  each correct, each paying nothing, because the real gaps are all deep clusters.
  See [[possible-nil-fold-gated]] memory + `docs/notes/20260706-flow-frontier-exhausted.md`.
- The flow frontier (possible-nil / always-truthy) has **no cheap FP-safe wins
  left**; the residual is param-dependent return typing, ActiveSupport RBS,
  project-class arms, ivar whole-class flow, loop narrowing — each a deep,
  opt-in, ADR-backed, one-at-a-time effort. **Default new work to
  productization**, which has demonstrably higher ROI (directory support,
  config `paths:`, the ADR-22 baseline subcommands all landed clean).

### Measurement is treacherous — three artifacts burned this session

Ad-hoc measurement lied three times; always distrust a surprising number until
the harness reproduces it:

1. **Invocation mode.** `rigor check <dir>` did nothing before ADR-0040 (errored
   on a directory), so every "dir-mode rigor-rs" number was 0-because-nothing-ran,
   NOT leniency. The valid gate (`fp_audit.py`) passes explicit **file lists**.
2. **Reference on-disk cache.** The reference has a `.rigor/cache` that returns
   **stale cross-path results** in the same cwd. Use a **FRESH scratch dir per
   probe scenario** (or `rm -rf .rigor` between edits). A "reference is buggy"
   finding was 100% cache pollution once.
3. **Shell quoting / evaluation order.** `find … | check $FILES` collapsed a
   newline list into one arg; `$(pwd)` evaluated *inside* a `cd` subshell resolved
   the reference path wrong (LoadError → empty output → false "identical"). Fix
   the harness, re-run, before believing a diff.

### Faithful port: read the reference, don't guess

- rigor-rs is a faithful Rust port of the Ruby reference (`reference/rigor`,
  pinned submodule). For any behavior, **read the reference source AND probe the
  oracle empirically** — do not reconstruct semantics from memory. The reference
  is the oracle; match it (fix upstream only if a behavior is genuinely
  unreasonable — verified reasonable every time this session).
- Probe both tools: `ruby -I reference/rigor/lib reference/rigor/exe/rigor check …`
  vs `target/release/rigor check …`, comparing **stdout + stderr + exit code**
  (channels are a contract; e.g. baseline `generate` writes to stderr, `drift` to
  stdout).

### Delegation protocol (main = design/coordinate/audit; subagents = investigate/implement)

When splitting work to subagents:
- **Investigate with Sonnet** (read reference + oracle probes → a precise data
  report). Run **two independent investigations** where stakes are high; agreement
  is cross-validation. Warn every investigator about the cache-pollution trap.
- **Implement with Opus** on a NEW branch from a spec that names the
  誤実装しやすい pitfalls explicitly. Require gates in the prompt: full tests +
  clippy + both harnesses + **fresh-dir E2E parity probes** vs the reference.
- **Audit before merge, always.** Re-run gates yourself, review the diff scope,
  and **byte-verify the subagent's parity claims with your own probes** — the
  implementer may resolve a spec-vs-oracle conflict toward the oracle (correct)
  but you confirm it; and your *audit harness itself* can be buggy (see artifact
  #3) — a broken audit nearly rejected a correct implementation this session.
- Preserve deferred-but-built work on its branch and point the ADR at it (e.g.
  `tier-bc-nilable-return` holds the FP-safe-but-0-gap piece A). Branches are
  local until a remote exists — the ADR text is the durable record.

### ADR / doc hygiene

- Record hard-to-reverse + surprising + real-tradeoff decisions as ADRs; record
  a slice's MEASURED outcome (even "0 gaps, deferred") in the ADR, not just the
  plan. Audit findings get absorbed into the ADR + a dated note in `docs/notes/`.
- `docs/CURRENT_WORK.md` is the session-to-session baton: newest status on top,
  prune stale "next session" plans as they land or die.
