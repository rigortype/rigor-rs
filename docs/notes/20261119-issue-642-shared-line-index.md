# #642: one LineIndex shared across check / MCP / LSP / annotate

PR #644 → merged 2026-11-19 (`9f263b4`). Closes #642 (residual of #636 /
PR #641).

## What was slow

PR #641 removed the O(diagnostics × file-prefix) `line_col` walk from
`check`'s findings path, but the same walk survived where the gates cannot
see it, and the newline-start table was rebuilt independently in ~6
places:

- MCP `check`/`triage`/`outline` — one or two prefix walks per diagnostic
- LSP `offset_to_position` — up to three walks per diagnostic on publish
- `annotate` — one walk per AST node
- `comment_lines`, the lowering builder (then dropped), `pre_eval`,
  `shadowed_rescue`, and stage 3 each built their own table

The #636 generator at smaller scale (`20_001` lines, a multi-byte
character per line, `20_000` diagnostics) took LSP `didOpen`→publish
2.89 s and an MCP check+triage+outline+annotate session 4.26 s on
master; `check` itself was already 0.66 s after #641.

## Port shape

One `rigor_parse::LineIndex` (`line` / `line_col` / `line_start`):
`starts[0] == 0`, then `i + 1` per `\n`, binary-searched with
`partition_point(s <= offset)` — a `\n` at the offset stays on that
line, past-EOF clamps, empty source is `(1, 1)`, identical to the walk.
Columns are **bytes** (Prism `start_column + 1`).

Lowering already built the table and dropped it. It now rides on
`LoweredAst` inside the existing boxed `CarrierSpans` (`Arc<LineIndex>`)
so `enum Lowered` stays under clippy's `large_enum_variant` cap (an
extra pointer on the AST itself pushed the variant to 232 bytes).
Consumers with an AST share it:

- stage 3 `with_lines` + stored findings (no second file scan)
- MCP check/triage/outline
- LSP publish/symbols (UTF-16 `character` counted only on the hit line)
- annotate, `shadowed_rescue`, `comment_lines`

Paths with no AST (parse-error drain, LSP depth refusal and hover, the
`pre_eval` scanner) build their own `LineIndex` once.
`crate::line_col` is a one-shot wrapper for tests, `conformance_rows`,
and `print_rendered`'s parse-error path. The walk-equivalence test moved
to `line_index.rs`.

## Measured outcome

Master vs head release binaries (reviewer, same host):

| Probe | Result |
|---|---|
| MCP check/triage/outline/annotate, 173 fixtures + 7 edge files (multi-byte, CRLF, no trailing NL, empty, shadowed-rescue + markers, parse error, deep nesting) | 720 responses byte-identical |
| LSP publish/documentSymbol/hover, same 180 files, 60 hovers each | identical message multiset; 0.5 s pre-`didClose` delay → byte-identical (remaining difference is async arrival order) |
| LSP multi-byte must-still-fire | 3 diags at UTF-16 cols 10/12/6, identical |
| `rigor check --format json` on edge files + pathological file | byte-identical, exit 1 |
| `rigor annotate` on multi-byte/rescue files | byte-identical |

Timing (20,001-line / 329 KB file, 20,000 diagnostics):

| Path | master | head |
|---|---|---|
| LSP `didOpen` → publishDiagnostics | 2.89 s | **0.36 s** |
| MCP check+triage+outline+annotate session | 4.26 s | **0.54 s** |
| `rigor check` | 0.66 s | 0.65 s (fixed by #636) |

Isolated mapping: 8,000 lines / 48 KB, 20,000 queries — prefix walk
14.07 s vs indexed 0.018 s.

`harness/gate.sh` clean (snapshot 0 unregistered / 959/1063); CI green
on `6953d59` (ubuntu/macOS test+clippy 1.88, snapshot parity,
live-reference snapshots, docs-check). `fp_audit.py --gaps --sweep`
**0 FP** on mastodon/app, gitlab-foss/lib, dependabot-core, net-ssh
(haml/lib absent on the review host).

Reviews: Opus 5.5 adversarial **Approved** on `6953d59` (re-probed both
engines). Human-merged under owner discretion (oldest-first vs #601/#634).

Nits left standing: `line_col` still builds a whole-file table per call
for the few `conformance_rows` / parse-error rows; LSP hover scans once
per request; `tests.rs` has a double blank line where the walk test was
removed.
