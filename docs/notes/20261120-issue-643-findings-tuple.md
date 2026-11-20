# #643 — findings-tuple leftovers from #641 (PR #725)

Two cleanups #641's review left in `crates/rigor-cli/src/main.rs`: a
deliberately false `start_offset` on conformance rows, and ~20 sites
spelling out the findings tuple while `Finding`/`Findings` existed.

## The honest-offset change

`conformance_rows` stored `start_offset = f.start_offset - delta` (and a
saturating `end_offset`) solely so render-time `line_col` printed RBS's
CHARACTER column on multi-byte `.rbs`. Now the diagnostic keeps honest
offsets and the row stores `line_char_col(source, f.start_offset)` =
`line_col` on the honest offset, then `column - char_column_delta(...)`.

The identity holds for every input: `delta` is bytes-minus-chars of the
offset's own line prefix, so `0 ≤ delta ≤ off - line_start` and
`line_col(off - delta) == (line(off), col(off) - delta)`; past-EOF and
mid-codepoint offsets yield `delta = 0` on both sides, and `\r` counts as
one byte and one char (CRLF-safe). No findings consumer reads
`diag.start_offset`/`end_offset` — the reviewer grepped every consumer in
main/diff/triage/baseline/suppression. `line_char_col` (added in the
review round) is also what the unit test now asserts, replacing the stale
assertion that still exercised the shifted-offset formula.

## The named struct

`struct Finding { order, path, line, column, diag }` — the issue's
second option; named fields remove the order/position transpose hazard
the tuple allowed. `Finding::run_level(order, path, diag)` builds the
eight positionless `(1, 1)` sites. Departure from the issue's two-arg
sketch: `order` is a parameter because the stage-1 panic/depth and
stage-3 panic sentinels must keep their file's index to sort beside that
file's findings. `order` is consulted ONCE by `sort_by_key`; post-sort
rows (run-level prepended, conformance appended) carry `0`/`usize::MAX`
as documentation only — the reviewer caught the earlier doc implying
`MAX` fed the sort. `diff.rs`/`triage.rs` destructure fields now.

## Measured

- Probes (implementer + reviewer re-probe): 8-shape multi-byte `.rbs`
  project — annotation at offset 0, multi-byte previous/same line,
  `🎉`+combining-mark payloads, CRLF, `%a(…)`/`%a[…]`, two directives on
  one line — reference == port on every row; `s = "ü🎉"; s.upcasee` and
  parse-error snippets identical; **18/18** head-vs-master byte-identical
  outputs over all `--format`s, `--fail-on`, `triage`, and the four
  baseline commands.
- `harness/gate.sh` pass; CI green on both heads; `fp_audit --gaps
  --sweep` **0 FP / 437 gaps — identical to a detached origin/master
  sweep** (0-gaps refactor, as expected).
- Reviews: Opus adversarial **Approved** on `96eddbbf`; fix-confirmation
  **Approved** on `4acf95a3`. Design companion on a **Sonnet 5.5
  substitute** — Fable quota-blocked on OpenCode (balance) and
  claudeAgent (rate limit), Grok balance-blocked.

## Follow-ups filed

- **#727** (pre-existing, found by review): `triage` drops `conforms-to`
  rows the reference counts (the `verb == "check"` gate's comment claims
  upstream never carries them; the pin disagrees), and a `pre_eval:` key
  stands down the port's scan.
- **#728**: residual tuple-shaped leaks — `diagnostic_value`'s
  `(line, col)` param, the three `(String, &Diagnostic)` entry-pair
  copies, `AnalysisOutcome`'s positional counters, `Stage3.panic`'s pair,
  and `conformance_rows`'s per-finding `LineIndex` rebuild.
