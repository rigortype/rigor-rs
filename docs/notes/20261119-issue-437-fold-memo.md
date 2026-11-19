# #437 — order-independent predicate-fold memo + loop-pass `if` modelling (PR #687)

## What landed

Two coupled fixes; neither alone satisfied the issue's o6/o9/o10 rows.

### 1. `expr_truthiness` memo split by `seen`

The flat memo recorded a predicate's FIRST fold whatever the `seen` fuel —
a first visit inside another predicate's walk could starve on
`seen.len() >= 4`, memoize `None`, and poison the dead scan's own visit.

- `seen.is_empty()` → canonical `Typer::truthy_memo` (context-free).
  Folds computed inside a `dead_positions` scan are tagged
  `truthy_provisional` and evicted at scan end, so post-scan callers
  refold against the settled dead set.
- `seen` non-empty → `ReachMemo::truthys`, an
  `ExprTable<Option<bool>>` keyed `(pred, seen.len())` with the same
  dependency recording and `(ast, dead)`-epoch scoping as `expr_scalars`.
- `truthy_landed` holds first-completed answers for the reach memo's
  `folding` dep check — exactly the pre-#437 flat memo's first-completion
  semantics the dep validation was written against (a fold can only be
  in-flight once globally, so `landed[p]` always names the instance the
  entry saw running).
- Fold-order audit behind `RIGOR_REACH_MEMO_AUDIT`: each `if` predicate's
  scan-visit fold is recorded, refolded fresh at scan end, panics on
  mismatch. CI wiring for the audit remains #439.

### 2. `loop_pass` — the loop fixpoint models `if` decisions

The fold fix alone left o6/o10 silent: `replay_opaque`'s `Dynamic` exits
forced the untrusted structural verdict, which over-caps the pinned name.

- `Typer::loop_pass` set only inside `loop_continuation`'s CAP loop.
  Under it, `if` + `LocalVariableWrite`/`OpWrite`/`MultiWrite` statements
  whose decision reads are all typed **locals** bypass `replay_opaque`
  (ivar resets and class/global decision reads stay opaque).
- `replay_one`'s `if` arm takes a decided predicate's live arm only —
  matching the reference's `live_branch_for_if`, so a dead-arm write
  (`else t = nil` on a pinned `d`) never reaches the pass env.
- `widen_shell` drops shell writes inside folded-dead arms — a write that
  never runs is not stale (x5's `w = "x"` inside `if d`'s dead else).
- The seed-exactness gate relaxed: `Dynamic` seeds are tolerated; the
  pass-EXIT check is unchanged. A name unconditionally rebound each pass
  overwrites its seed anyway (x5's `s = u` then `s = (if d …)`); one it
  never overwrites exits `Dynamic` and still declines.

The reference's `BodyFixpoint` is order-sensitive: o6 converges at pass 2
(the decided `if d`'s unconditional `y = 2` absorbs the conditional
`y = 1`); o8's conditional-write-last keeps `y` moving to the cap pass →
widen floors `t`. The replayed-`if` pass reproduces both.

## Measured outcome

- `harness/probe.py` on the issue's rows: o6/o7/o9/o10/o11/v4/v5 now fire
  `always-truthy-condition` with identical tuples to the reference (the
  residual `≠` is rule-ordering, the #328 family m3 already shows).
  o8/x4/m1/m2/m435_tern/v1/v2 and the #638 decline controls
  (`x = f(x)`, `i += 1 if i < 3`, `x = (x || 0) + 1`) match exactly.
- `fp_audit.py --gaps --sweep` (release): **0 FP**;
  `gap_census.py --sweep` dump **byte-identical to master** (363 rows —
  the wins are on the issue's constructed rows, not the corpora).
- `RIGOR_REACH_MEMO_AUDIT=1 run_snapshot.rb`: clean, 0 panics.
- Timing: installed-gems tree `check --no-cache` 6.4 s, mastodon/app
  1.2 s — no walk blowup.

## The x5 residual — filed #688

The fixpoint converges `w = Constant(1)` in the pass env, but
`"abc"[w].zz` still can't fire: `ScopedEnv::at` hands an **empty env
inside every method body** (`in_method_body → empty`, deliberate — "the
bindings it withholds were all wrong"). `w = 1; w.zz` inside a bare
`def` is silent on master for the same reason. Method-body locals need
per-def diagnostic envs — a much larger arc, now filed as #688. #441's
x4/x6 rows are constrained the same way.

## Follow-ups

- #439: wire `RIGOR_REACH_MEMO_AUDIT` (now covering the fold-order audit)
  into CI.
- #688: method-body locals for diagnostics (x5's `zz`, biggest
  `call.undefined-method` gap bucket).
