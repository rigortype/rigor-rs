# 2026-11-12 — #393 retry-edge indexed narrowings

PR #590 merged as `9fc4e52`. A top-level retrying `begin` now drops
indexed narrowings rooted at locals rebound or slot-written by the
protected body or a retrying clause, matching the reference's retry-edge
rebind. The two brief rows and ~20 adjacent FP rows now match; controls
without a retrying write still fire.

The safe over-decline drops a record on any write because the port does
not model `retry_binding_accepted?`. Release `fp_audit.py --gaps --sweep`:
0 FP / 658 gaps; all 214 corpus files containing `retry` are
byte-identical. Review: Opus 5.5 `Approved`; Fable companion `approve`.
Stress timing was unchanged within noise.

Follow-ups filed: #592 (accepted slot-write narrowings / coverage loss),
#593 (pre-existing post-rescue `h.frobnicate` receiver FP).
