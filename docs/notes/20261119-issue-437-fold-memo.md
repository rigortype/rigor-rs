# #437 — order-independent predicate-fold memo + loop-pass `if` modelling (PR #687)

## What landed

Two coupled fixes; neither alone satisfied the issue's o6/o9/o10 rows.

### 1. `expr_truthiness` memo split by `seen`

The flat memo recorded a predicate's FIRST fold whatever the `seen` fuel —
a first visit inside another predicate's walk could starve on
`seen.len() >= 4`, memoize `None`, and poison the dead scan's own visit.

- **Canonical** (`Typer::truthy_memo`): `seen.is_empty()` AND
  `truthy_in_flight` empty — context-free. Entries computed inside a
  `dead_positions` scan are tagged `truthy_provisional` and evicted at
  scan end, so post-scan callers refold against the settled dead set.
  (Design review: an empty-`seen` call arriving while other predicates
  fold — a `&mut Vec::new()` caller inside a reach — is NOT context-free;
  it now routes to the contextual table, or an ancestor's re-entry `None`
  would land in the flat memo permanently.)
- **Contextual** (`ReachMemo::truthys`): an `ExprTable<Option<bool>>`
  keyed `(pred, seen.len())` with dependency recording and `(ast, dead)`-
  epoch scoping, same discipline as `expr_scalars`.
- **`truthy_landed`**: first-completed answers for the reach memo's
  `folding` dep check — the pre-#437 flat memo's first-completion
  semantics the dep validation was written against (a fold can only be
  in-flight once globally, so `landed[p]` always names the instance the
  entry saw running).
- **Fold-order audit** behind `RIGOR_REACH_MEMO_AUDIT`: each `if`
  predicate's scan-visit fold is recorded WITH the partial dead set it
  ran under, then refolded fresh at scan end against that same partial
  set with the pred marked in-flight; a mismatch panics. A first cut that
  refolded against the *settled* set panicked on a loop back-edge
  (`if s` read before `if d`'s dead `s = nil` arm records — #691): that
  is the scan's inherent source-order limit, not a memo bug. CI wiring
  for the audit remains #439.

### 2. `loop_pass` — the loop fixpoint models `if` decisions

The fold fix alone left o6/o10 silent: `replay_opaque`'s `Dynamic` exits
forced the untrusted structural verdict, which over-caps the pinned name.

- `Typer::loop_pass` is save/restored (`prev`) around the CAP loop — a
  nested `loop_continuation` must hand the flag back to the outer pass,
  not reset it (design-review N5).
- `if` statements, and writes whose **value itself is an `if`**
  (`s = (if d …)`), bypass `replay_opaque` when their decision reads are
  all typed **locals**; ivar resets, class/global decisions, and other
  decision shapes (`x = t && v`, `x = t&.m`, `case` values) stay opaque.
- `replay_one`'s `if` arm takes a decided **`Constant`-typed**
  predicate's live arm only under `loop_pass` — restricted to `Constant`
  because a `Nominal`/`Tuple` answer could be an optimistic carrier the
  reference declines to elide; the port has no `OptimisticOrigin` channel
  yet (#690).
- `widen_shell` drops shell writes inside folded-dead arms — a write that
  never runs is not stale (its judge is the scan fold, not the pass env —
  the residual two-judges inconsistency is #690).
- The value-exactness gates are UNCHANGED on seeds and exits: a tolerated
  `Dynamic` seed could mask a moving precise reference seed and converge
  a pass early, keeping pins the cap widen sheds (design-review (a)).

The reference's `BodyFixpoint` is order-sensitive: o6 converges at pass 2
(the decided `if d`'s unconditional `y = 2` absorbs the conditional
`y = 1`); o8's conditional-write-last keeps `y` moving to the cap pass →
widen floors `t`. The replayed-`if` pass reproduces both.

## Measured outcome

- `harness/probe.py` on the issue's rows (post-tightening): o6/o7/o9/o10/
  o11/v4/v5/backedge fire `always-truthy-condition` with identical tuples
  to the reference (residual `≠` is rule-ordering, the #328 family m3
  already shows). o8/x4/m1/m2/m435_tern/v1/v2 and the #638 decline
  controls (`x = f(x)`, `i += 1 if i < 3`, `x = (x || 0) + 1`) match
  exactly. Exact-seed w1 (`s = (if d …)` inside a loop) fires as the
  oracle does.
- `run_snapshot.rb`: matched **952→959 (+7 gaps closed)**, 0 unregistered
  FP — the slice closes snapshot gaps the sweep corpora don't contain.
- `fp_audit.py --gaps --sweep` (release): **0 FP**; `gap_census.py
  --sweep` dump **byte-identical to master** (363 rows — the wins are on
  the issue's rows and the snapshot corpus, not the corpora).
- `RIGOR_REACH_MEMO_AUDIT=1`: clean over `run_snapshot.rb` AND all four
  sweep corpora (0 panics).
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

## Review findings → disposition

- Fable 5.2 design companion: N5 flag-clobber (fixed), N3 Constant-only
  elision (fixed), (d)2 in-flight canonical routing (fixed), seed-gate
  removal reverted entirely (stricter than their N1), `modelled` narrowed
  to value-is-`if` (fixed), N4 memo-key flag miss (verified not real —
  `type_of` never calls the replay paths), N2 two-judges inconsistency
  (filed #690), (d)3 back-edge dead-set epoch (audit rescoped; filed
  #691), (d)4 `landed` first-completion under two memos (unchanged
  pre-#437 semantics; noted).
- Grok 4.6 second opinion: provider quota exhausted (402) — no review.
- Opus 5.5 gate review (on the pre-tightening head aa0076d): `Needs fix`
  — (1) new FP family from the removed seed gate: `s ||= nil`, `s, =
  nil`, `s = (k && 1)` etc. minted early convergence keeping `t`'s pin —
  fixed by the full restore, reviewer-verified on a scratch build, now
  pinned by `loop_untyped_seed_still_forces_structural_cap`; (2) the
  first-cut settled-set fold-order audit panicked on fixture 130, v5,
  backedge and two sweep files, and `run_snapshot.rb` swallows panic
  stderr — fixed by the same-partial-set rescope (verified via stderr
  grep over all 173 corpus files + the flagged sweep files); (3) the
  nested-loop flag clobber — fixed (prev restore); (4) PR body: `Refs`
  not `Closes` (x5's `zz` undelivered), x5's "before" state, audit gate
  restated.

## Follow-ups

- #688: method-body locals for diagnostics (x5's `zz`, biggest
  `call.undefined-method` gap bucket).
- #690: one arm-deadness judge for the loop pass (`widen_shell` filter vs
  `known_polarity(env)`; wholesale `branch_certainty` port incl.
  `OptimisticOrigin`).
- #691: dead arms from the converged forward pass (Fable S4) — retires
  the backward fold, both memos, and the source-order back-edge limit.
