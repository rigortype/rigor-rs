# #253: `# rigor:disable` capture bound + Psych 1.1 severity booleans — measured outcome

PR #799 (approval head `48ed7284`, merged `b90d38fe` after an
origin/master merge; oracle `bbea1bac` / v0.4.8). The two open items of
the re-triaged brief; both landed, zero new FPs.

## What landed

- **Item 1 — directive token capture.** The reference's
  `LINE/FILE_SUPPRESSION_PATTERN`
  (`/\A#\s*rigor:disable(?!-file)\s+(?<rules>[\w.,\s-]+)/`) captures a
  class-bounded run, consumed by both the filter absorb and the marker
  surveillance (`suppression.rs`/`suppression_markers.rs`). The port
  returned the verbatim tail, so `call.*` read as unknown-rule `call.*`
  instead of `call.`. Now ports the class (ASCII `\w`, the six-char Ruby
  `\s` — including `\v`/`\f`) and the greedy-`\s+` backtrack that hands
  trailing whitespace back before the capture ends.
- **Item 4 — Psych 1.1 severity booleans.** Psych resolves a PLAIN
  untagged `off`/`on`/`no`/`yes` (case-insensitive) to `false`/`true`;
  serde_yaml's 1.2 parse keeps the String, so `{call: off}` loaded clean
  where the reference exits 64. New `config/psych_booleans.rs` walks the
  first document's libyaml event stream and replays the port's DupOk
  mapping rules; the load flips matching severity values per file before
  the include merge, so `validate_severity_overrides` /
  `coerce_severity_profile` hit their existing boolean errors.
  `severity_profile` is the uncaught family on both engines (disclosed:
  same exit 1 + stdout, the port's one-line `rigor:` stderr convention).

## Measured

- 19/19 PR probe rows identical on both engines, full tuples + exit
  codes — re-probed by the reviewer on the approval head.
- Reviewer counterexamples, all identical: CRLF, `\v`/`\f`, tab runs,
  multi-byte inside/before/after tokens and left of the comment,
  `disable-file`+whitespace, prose/`##`/`#x#`/`=begin` non-directives,
  anchors/aliases (die earlier on both), includes, duplicate keys,
  `y`/`n` spellings, non-String keys, `<<` merges, second documents.
- Standing sweep: **0 FP** on the merged landing tree; gap census
  unchanged (call.undefined-method 212 / possible-nil 138 / arg-type
  59 …, 0 FP/437=master).
- Gates: `gate.sh`, `cargo test --workspace`, clippy 1.88 fresh
  `CARGO_TARGET_DIR`, CI 5/5 on the merge head.

## Review (round 1: `Approved`; Fable companion skipped — Claude Fable
limit + OpenCode Go weekly limit both exhausted, no Codex instance)

None blocking; three filed:

- **#801** — `DupOkVisitor::visit_map` merge order is fill-only; Psych's
  `revive_hash` `merge!`s later `<<` over earlier keys. Port emits
  warnings the reference suppresses and loads configs it rejects
  (predates the PR — master's DupOk rule; `psych_booleans` copies it).
- **#803** — scan declines + message drift: top-level `<<` holding
  `severity_overrides`, `~`/`0x10` keys (port exit 1 vs ref 64),
  `on:`/`!!str <<:`/`call: [off]` same-exit different-message rows.
- **#802** — harness: `probe.py -f` crashes `UnicodeDecodeError` on
  invalid UTF-8; the reference's `comment_text` scrub path is unprobable.
