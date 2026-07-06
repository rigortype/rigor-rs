# The possible-nil / always-truthy frontier has no cheap FP-safe wins left (2026-07-06)

Three consecutive carefully-designed, FP-safe flow slices closed **0 measured
survey gaps**. Recorded so the next session does not re-discover this the hard way.

## The three 0-gap slices

1. **Shape-tier Slice 1b — `Type::Tuple`** (ADR-0039). Deferred BEFORE building by
   measurement: undefined-method gaps are Rails/AS-plugin + project-class + Tier
   B/C; a Tuple shares Array's method set, so it adds no undefined-method
   coverage; element precision (`[1,"a"][0].typo`) does not occur in real gaps.
2. **Piece A — project-method nilable-return, CLEAN core arm** (ADR-0041). Built,
   FP-safe (0 FP survey-wide), fires the synthetic clean pattern — but 0 real
   gaps: the corpus project-return nilables all have param-dependent or
   AS/project-class arms.
3. (Slice 1a DID pay — treemaps — so the substrate itself is sound; the point is
   the *cheap* frontier beyond it is gone.)

## Why the residual gaps are all deep (valid-mode classification, redmine+parser+mastodon)

| cluster | example | needs |
|---|---|---|
| param-dependent return arm | redmine `scm_iconv` (`return str`) | port the reference `return_type_heuristic` (type the arg → the param) |
| AS-method on the arm | redmine `render_...` `.blank?` | ActiveSupport RBS (the plugin phase) |
| **project-class arm** | parser `x.adjust` (25), textbringer (11) | possible-nil where the non-nil arm is a PROJECT class + the method a project method (project-method-on-arm resolution + the leniency invariant) |
| ivar value-flow | redcloth3 `@pre_list`, `if @added` | whole-class ivar analysis (any method can rewrite an ivar) — ADR-58-class |
| loop / conditional-assignment | splay_tree `break unless t.left`; `x = v if cond` | loop-carried narrowing; one-sided-assignment nilability |

Each is a deep, per-cluster effort for a handful of gaps. There is no remaining
pattern that is both common in real code AND cheaply + FP-safely inferable.

## Contrast: productization paid off

`rigor check <dir>` (ADR-0040) was a real user-facing gap fixed cleanly, and it
also corrected two prior *measurement artifacts* (the "dir-mode leniency" /
"Rails reopened-core-class over-leniency" findings were both rigor-rs analyzing
NOTHING on a directory arg). Coverage-independent productization (the CURRENT_WORK
lever A: §11 CLI completion, §12 LSP two-tier / MCP tools, config schema, baseline
subcommands) has demonstrably higher ROI right now than grinding the deep flow
clusters.

## Recommendation

Treat the flow deep-clusters as **opt-in, ADR-backed, one-at-a-time** efforts
(project-class-arm possible-nil is the largest single one — parser 25 +
textbringer 11), NOT片手間 slices. Default the next work to productization unless
a specific deep cluster is explicitly chosen. Do NOT build another speculative
flow slice without a valid-mode gap count predicting it pays.
