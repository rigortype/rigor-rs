# 2026-11-12 — #405 class guards drop rooted index records

PR #591 merged as `2a09baa`. A class guard on a local (`is_a?`,
`kind_of?`, `instance_of?`, `Hash === h`, `case h when …`,
`case h in Hash`, and `&&`/`||` compounds) now drops `h[k] ||= v` records
rooted at that local, mirroring the reference's
`Scope#with_local` → `drop_indexed_narrowings_for` rebind on both edges.
`record_guards.rs` adds a per-site overlay applied after
`if_edge_overlay`; a covered pre-guard record reads `untyped`, so it can
only decline.

Top-level probe table: 58/59 rows match, with 18 master FPs closed and
all unguarded/non-class/foreign-local controls still firing. Release
`fp_audit.py --gaps --sweep`: 0 FP / 658 gaps. Timing on 400 gem files
was within noise.

Review: Opus 5.5 `Approved`; Fable companion `approve`. Follow-ups #595
(new coverage loss on `case` conditions / do-while bodies) and #596
(pre-existing multi-statement parenthesized guard FP).
