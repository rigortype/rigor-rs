# 2026-11-12 — #438 `class_ivar_scope` memo key

PR #588 merged as `ae6982a`. `ReachIndex::class_ivar_scope` now keys its
memo by the containing class `region` instead of the ivar read span, so all
reads in one class body share one `Rc<IvarScope>` instead of cloning the
region's `defs` once per read.

Stress files reproduce the #432 regression and collapse it: the required
shapes return to within ~0.2–0.3% of pre-#432 RSS (D=1000,K=5: master
1638.8 MB → 1543.2 MB; D=2000,K=2: master 1308.4 MB → 1179.9 MB), with
byte-identical `check` output. The memo is exact because `barriers` and
`defs` are pure functions of `region` over immutable index data.

9,441/9,441 corpus files are byte-identical; the reviewer re-measured the
RSS table and a 1,500-file sample. `RIGOR_REACH_MEMO_AUDIT` does not cover
this memo; the evidence is the exactness argument plus byte identity.
Review: Opus 5.5 `Approved`; Fable companion `approve`. Follow-up #594
records a pre-existing same-key `Float("1.5")` message drift.
