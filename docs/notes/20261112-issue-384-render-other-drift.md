# 2026-11-12 — #384 ATM describe labels

PR #573 merged as `ba126c5`. `render_describe` now flattens nested
unions, optionals and union-valued aliases before `union_describe`, and
`Other` forms strip only root-position `::` rather than every separator.
The brief's 12-row `call.argument-type-mismatch` set is byte-identical
against reference pin `f7595e289`; accept and nil-channel controls are
unchanged.

Gates: `harness/gate.sh` pass; pinned 1.88 clippy clean in a fresh
`CARGO_TARGET_DIR`; release `fp_audit.py --gaps --sweep` reported 0 FP;
CI green on `e49073a`. Review: Opus 5.5 `Approved`; Fable companion
`approve`.

Follow-ups filed from review: #577 (`bool` member ordering in `Other`)
and #578 (structural normalisation for literal/singleton/record/
intersection `Other` forms).
