# #435: widen loop-tracked locals when the rebind fixpoint caps

PR #634 → merged 2026-11-19 (`0c8c283`). Closes #435. Follow-ups #638
(over-cap / coverage loss), #639 (under-cap residuals), #640 (top-level
receiver-display drift `for 1` vs `for Integer`).

## What was wrong

The reference's `BodyFixpoint.converge` (ADR-56 slice B, CAP = 3) widens
**every** local a `while`/`until` body writes when the rebind fixpoint has
not converged by the last pass: `Constant → Nominal` via
`widen_value_pinned`, still-moving names to `Dynamic[top]`. The port kept a
dead-arm pin (`if false; t = nil; end` is filtered out of the write marks),
so a non-convergent body left `t = 1` pinned past the loop and fired
`flow.always-truthy-condition` / `call.undefined-method` where the
reference stays silent.

The port's env algebra cannot replay the reference's passes
(`join_flow_envs` collapses any arm disagreement to `Dynamic` on pass 1),
so a simulated-pass approach cannot observe the same divergence.

## Port shape

`loop_cap_widening` (`flow_eval.rs`) reads the criterion **structurally**
on the loop's **entry** env. A tracked name can still move at pass ≥ 2
when a live body write:

- **compounds** on its own binding (`y = [y]`, `i += 1`, `x ||= [x]`), or
  is an op-write the reference refolds per pass;
- **reads the assumption** of a name that moved (the read precedes that
  name's first body write);
- lands only under a **decision whose reads can still move** (`if s then
  y = 1 end` — o8).

Any such name makes the loop cap. `apply_loop_cap_widen` then lands on the
caller's **post-join** env: suspect names floor to `Dynamic[top]`, every
other tracked name sheds `Constant → Nominal` via `widen_value_pinned` of
its joined binding, and a name the join does not bind is left alone
(never minted `nil`). Applied in both env models (`flow_eval_stmt` and
`replay_one` `Node::Loop` arms). A convergent loop returns `None`.

The first review round caught the overlay being built from the entry env
— a body write to a non-suspect name was discarded and body-first locals
minted `Constant[nil]`, an always-falsey FP on `w = nil; while c; w = 1;
i += 1; end; if w`. That is now applied post-join.

`for`, `begin…end while`, and bodies with `break`/`next`/`redo`/`retry`
keep the plain widen (the replay arm declines the same shapes).

## Measured outcome

Brief rows m1/m2/o8/m435_tern/m435_top match the reference (full tuples);
m3/c1/c2/c4 still fire (order differs — #328). Review-regression rows
r3 / r3 top-level / `count`/`last` / n3 stay silent like the reference.

`harness/gate.sh` clean; CI green on the rebased head. `fp_audit.py
--gaps --sweep`: **0 FP**, **0 gaps closed, 0 opened** vs a detached
master build on mastodon (0/44), gitlab-foss (0/203), dependabot-core
(0/87), net-ssh (0/26). mail/Ruby/concurrent-ruby were sig-only on this
host; haml/lib not populated.

Timing (fp_audit wall, master vs branch): gitlab-foss 118.6s → 117.5s,
dependabot-core 114.5s → 115.0s, net-ssh 63.6s → 65.8s, mastodon
30.7s → 30.0s. Synthetic 1600 top-level loops 2.15s → 2.62s; no
#422-class blowup.

Reviews: Opus 5.5 adversarial Needs-fix (entry-env overlay) then
fix-confirmation **Approved** on `004ec32`; Grok 4.6 advisory Approved
(non-blocking over/under-cap shapes filed as #638/#639). Human-merged
under owner discretion (0 gaps closed is self-merge-blocked).

The structural criterion is an approximation of the pass comparison, not
the reference's pass itself; #638 sketches the hybrid
`loop_rebind_fixpoint` (`join_arm_writes_bound` is already
`single_pass`).
