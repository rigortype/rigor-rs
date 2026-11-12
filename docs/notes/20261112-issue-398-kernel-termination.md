# 2026-11-12 — #398 Kernel non-returning termination

PR #575 merged as `e097386`. Dead-code termination no longer applies
block-call timing's `project_defines_anywhere` veto to `raise`/`fail`/
`throw`/`exit`/`exit!`/`abort`. `Kernel.` / `::Kernel.` calls terminate;
implicit-self calls terminate unless a project toplevel def binds and the
site's `self` does not answer, with `def_exits` deciding that bound def.
Block-call timing keeps its veto.

All 13 brief FP rows plus three extra families are silent on both
engines; every brief control still fires identically. Release
`fp_audit.py --gaps --sweep`: 0 FP / 658 gaps, gap delta 0. CI green on
merge-commit head `1655729`. Review: Opus 5.5 `Approved`; Fable
companion flagged follow-ups.

Follow-ups filed: #581 (cross-file toplevel-def bind/body rules + doc
placement), #582 (singleton-self coverage loss), #583 (pre-existing
no-else `if` FP).
