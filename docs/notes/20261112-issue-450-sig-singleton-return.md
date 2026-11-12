# 2026-11-12 — #450 project-sig singleton returns

PR #574 merged as `6fb63c4`. Singleton calls on project classes now use
a buildable project `sig/` declaration before body inference, matching
the instance path. The receiver is resolved lexically (`::T`, `self::T`
and dynamic bases decline), unbuildable sig chains keep the body fold,
and singleton sig returns carry a nil bit through aliases and qualified
chains (`String?` declines instead of typing as `String`).

Brief row `T.one(1).upcase` is silent; no-sig and `.zork` controls fire
with reference-identical messages. Round-1 review found a nilable
singleton FP family; `f5fb331` closed it and the `::T`/`self::T`/`k::T`
anchoring rows. Release `fp_audit.py --gaps --sweep`: 0 FP / 658 gaps;
four sig-carrying gems are byte-identical. CI green on `b1860df`.
Review: Opus 5.5 `Needs fix` then round-2 `Approved`; Fable companion
`approve`.

Follow-ups filed: #584 (receiver lexical resolution), #585 (generic
return message drift), #586 (core singleton nilable FP), #587
(decline-side coverage).
