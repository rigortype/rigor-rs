# 2026-11-12 — #449 `Regexp.last_match` after an early-exit match guard

PR #589 merged as `51ae854`. The nil walker now ports the reference's
match-global flow: after an early-exit guard on a `=~`/`!~` literal match
it records `$~` and the unconditional capture groups, `Regexp.last_match`
for a mandatory group (and the zero-arg form) stops being a nil source,
and any later statement that might run another regex match forgets the
record.

All five brief rows and all controls match the oracle. The mandatory-group
walker is a line-for-line port including named groups, lookbehind and
char-class escapes; the parser now keeps regexp pattern text and `//x`.
Release `fp_audit.py --gaps --sweep`: 0 FP / 658 gaps. The change only
removes `possible-nil-receiver`; about 170 reviewer rows and 186 gem files
showed removals only.

Review: Opus 5.5 `Approved`; Fable companion `approve`. Follow-ups #597
(new empty-then coverage loss) and #598 (pre-existing over-eager forget).
