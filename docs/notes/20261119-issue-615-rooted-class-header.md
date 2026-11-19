# #615: rooted `class ::Path` header resets the effects unit prefix

PR #620 → merged 2026-11-19 (`1958fd7`). Closes #615; follow-ups filed as #621
(unrelated `check` FP, #584 family) and #622 (consolidate the piecemeal
`ConstantPath` ports).

## What diverged

`Scanner::walk_namespace` (`effects/collect.rs`) appended every class/module
header's rendered name to the outer prefix. Upstream's
`Source::ConstantPath.declaration_prefix` + `rooted?`
(`lib/rigor/source/constant_path.rb:72-99`, pin v0.4.8) **replace** the prefix
when the header is rooted: `class ::Rooted` inside `module Outer` names
`Rooted`, not `Outer::Rooted`. Every rooted row was one OVER (a key the oracle
never reports) plus one UNDER (the oracle's key missing) in `effects --full`.

## Port shape

- `rooted` walks the `ConstantPathNode` chain to its LEFTMOST parent — Prism
  spells `::Foo` as a path node with `parent: nil`; a `ConstantReadNode` or a
  dynamic base (`expr::Bar`, `self::Dyn`) is false.
- `declaration_prefix` keeps upstream's element shape: ONE string per header
  (`class A::B` → `["A::B"]`, not `["A","B"]`) — the same constraint #622's
  consolidation must keep, because upstream's `lexical_candidates` walks
  `prefix.first(depth)`.
- `walk_namespace` dropped its `ctx` parameter, matching upstream's
  `(node, prefix)` signature; the unnameable-header arm now passes a fresh
  `CLASS_BODY` per `scanner.rb:156` instead of the outer context the port
  carried (dead arm — only a syntax-error header reaches it — but the
  structure now matches). This closes #616 item 1 incidentally; #616's other
  items stay with it.
- `pushed_nesting` deliberately NOT ported: no caller under
  `lib/rigor/effects/` at the pin. `DefinitionLines#header_nesting`
  (`definition_lines.rb:89`) is a second `rooted?` reader — watch it when the
  effects-diff line anchors land.

## Measured outcome

`probe.py --dir … -- effects {--full | (none) | --full --format json | --full
--why}` — every row byte-identical (stdout/stderr/exit): the issue's three
rows, `A::B` reset under `module M; class N` (both `#` and `.` unit keys),
`module ::M`, `class ::Top < Base`, rooted inside `class << self`, dynamic
bases, multi-byte names, syntax-error header, and a `.rigor.yml`+`sig/`
project reopening a rooted class unrooted in a second file.

Unit-key regression tests land beside `a_nested_namespace_joins_lexically`
(the effects-corpus fixture blind spot — no rooted header anywhere, so
reverting the port left every gate green; raised by both reviewers).

Gates: `gate.sh` pass; `effects_diff.py` MATCH 384 / UNDER 320 / OVER 0
(fixture corpus unchanged — no rooted header in it); `fp_audit` 0 FP on
`reference/rigor/lib` (683 files); clippy 1.88 clean; CI green. Sweep corpora
were absent on the worker, so `--sweep` was vacuous.

## Review trail

Opus 5.5 adversarial review: Approved on `e3de847` (re-probed every row,
added `sig/` project and multi-byte counterexamples). Fable 5.2 design
companion: `Needs fix` on the missing regression guard only → `b0cbfee`
(test-only). Fix-confirmation re-review: Approved on `b0cbfee` — verified the
tests fail under both the reverted append-always and an over-reset.
