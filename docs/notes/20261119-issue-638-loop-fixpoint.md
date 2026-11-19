# #638: loop BodyFixpoint replay, gated on value-exactness

PR #674 → merged 2026-11-19 (`391898ce`). Refs #638 — the issue stays open
for the residual family, filed as #684. Other follow-ups: #681 (untracked-
local predicate narrowing), #682 (`folding.rs` `<<` i64 wrap,
pre-existing), #683 (structural-criterion indexed-read FP, pre-existing).

## What was wrong

`loop_cap_widening` (#435) decided convergence **structurally**: every
self-reading or op- write counted as compounding, so loops that settle
inside the reference's 3-pass `BodyFixpoint` still hit the cap pass,
widened every tracked name, and shed a sibling dead-arm pin
(`if false; t = nil`) — `flow.always-truthy-condition` went silent on
`x |= 1`, `i += 0`, `x = [x].first`, `s.strip`, `"#{y}x"`,
`y = 2 if s.nil?`, nested `while`, `begin…rescue`, `until`.

## Port shape

`loop_continuation` (`flow_eval.rs`) returns `LoopContinuation`:

- **Fixpoint.** Bounded passes (CAP = 3) replay the body via
  `replay_statements` — already the port's `single_pass` +
  `join_with_nil_injection` — on the entry env overlaid with the running
  assumption (pre-existing names only; a body-first local's `nil` seed is
  a join constituent, matching `except(*body_first)`). Per-name stability
  is `TypeId` equality after `Algebra::join`; on the last pass both sides
  are `widen_value_pinned`, and a still-moving name floors to
  `Dynamic[top]`.
- **Structural.** The #435 `loop_cap_widening`/`apply_loop_cap_widen`
  criterion, restored verbatim, applied over the post-join/post-widen
  env.

The gate — the replay is trusted **only when value-exact end to end**:

- every tracked seed and every tracked pass exit free of
  `Dynamic`/`Top`/`Bottom`/`Void` (`type_has_decline` walks every
  `TypeId`-carrying `Type` variant);
- the `Node::Loop` predicate reads or writes no tracked name (the pass
  does not model `loop_pass_entry` edge narrowing — this gate also
  closed the predicate-interaction FP family the first review round
  found);
- never under `def_scope` (sig-gen's def-local replay keeps the pre-#638
  declines; applying the structural verdict post-marks is equivalent to
  the old in-`replay_one` order — a marks-widened name is `Dynamic`
  under either ordering).

Why the gate exists: `join(Dynamic, Dynamic) == Dynamic` makes a declined
exit read as *stable*. On the ungated head, `x = f(x)`, `i += 1 if i < 3`,
`x = (x || 0) + 1`, `x = x * 2` (Float) and friends "converged" where the
reference still caps — an always-truthy FP family on the sibling pin.
The issue's own remediation prescribed exactly this fallback.

`op_write_result` (`x <op>= v`) folds memberwise over union-of-constant
receivers — the port's answer to `ConstantFolding.try_fold_binary_set`,
with `UNION_FOLD_INPUT_LIMIT` (32) / `UNION_FOLD_OUTPUT_LIMIT` (8)
declines — so `x |= 1` stabilizes at `0 | 1` instead of declining pass 2
to `Dynamic` (which would have made even the flagship row untrusted).
sig-gen gains new union signatures, byte-identical to the reference
except `<<` past 62 bits — `folding.rs` `<<` wraps at i64 (pre-existing;
#682).

Three call sites apply the continuation: `replay_statements` (post
`apply_def_local_marks`), `bind_check_statement`'s new `Loop` arm, and
`flow_eval_stmt`'s `Loop` arm. The check-env arm is why top-level
`i += 1; i.zz` now reports `for Integer` like the reference.

## Measured outcome

~390 check rows + 45 sig-gen methods probed in review (both engines +
a master build): 0 retractions vs master, no new-key FP. All listed
self-op rows converge; every declined/guarded/growing shape caps. Known
divergences filed (#681 message-drift at same key, #682, #683); residual
coverage losses equal to master collected under #684.

`harness/gate.sh` pass; clippy 1.88 clean;
`fp_audit.py --gaps --sweep`: **0 FP, 360 gaps, 0 closed, 0 opened**.
Timing flat on real code (rdoc `markdown.rb` 4.16s vs 4.00s master);
pathological moving-nests ~3^depth, the same shape as the reference's
CAP^depth.

## Review history

Three rounds on this PR: Fable design companion (found the
Dynamic-stable FP family → exactness gate), Opus round 1 (approved the
ungated head, found the predicate-interaction family → predicate gate),
Opus re-review on the final head (**Approved** — F1–F3 filed as
#681/#682/#683). First-push CI caught sig-gen `def_local_env`
regressions → the `def_scope` route to the structural verdict.
