# #636: findings carry resolved (line, column), not the file's source

PR #641 → merged 2026-11-18 (`800af1d`). Closes #636; residuals filed as
#642 (MCP/LSP per-diagnostic `line_col` walks + the ~6 duplicate line-start
tables → one `LineIndex`) and #643 (`conformance_rows`' shifted
`start_offset` is vestigial; finish the `Finding` alias or name the struct).

## What was slow

Every findings tuple was `(order, path, source: String, diag)` — a **full
clone of the file's text per diagnostic** — so the renderers could re-resolve
`diag.start_offset` at print time via `line_col`'s O(prefix) byte walk. Both
halves were O(diagnostics × file bytes): ~49 GB of clones and ~100% of the
busy thread inside `diagnostic_formats::to_rendered` → `line_col` on the
issue's 60k-diagnostic reproducer (`harness/gen_object_mixin_perf.py 300 30
30000 --no-include`, 814 KB file). The stage-3 `with_lines` map did the same
walk in parallel.

## Port shape

The position resolves **once where the row is produced**, over a per-file
newline-start table — `line_starts` in `main.rs`, the same table
`rigor_parse`'s lowering builder computes for node lines and drops with the
`LoweredAst` — and `line_col_at` binary-searches it (`partition_point` on
`starts <= offset`; an offset on a `\n` stays on the newline's line, EOF
clamps, empty source yields `(1, 1)`, all identical to the walk).

- Stage 3 builds one table per `Prepared` file; it feeds `with_lines` (the
  `filter_suppressed` line keys) and each survivor's stored position.
- The stage-1 `ParseErrors` drain builds one per unparseable file.
- Run-level rows (plugin load/emitted, pre-eval, path, config-error) carry
  literal `(1, 1)` — what `line_col("", _)` returned.
- `conformance_rows` resolves against the `.rbs` text at push, after the
  character-column delta shift.
- `to_rendered` / `json_document` / `rigor diff` read the stored position.
  Baseline regroup moves whole tuples, so no order path can mis-position.
- `line_col` stays for the single-shot callers (`annotate`, `mcp`, tests).

Resolving at production is also the reference's own shape:
`Analysis::Diagnostic` is constructed with `path:, line:, column:`.

The nested `(usize, usize)` pushed `apply_baseline`'s
`Vec<Option<…>>` over `clippy::type_complexity`; the `Finding`/`Findings`
aliases now exist (most call sites still spell the tuple — #643).

## Measured outcome

`check` on the reproducer (release build, this host):

| build | wall | peak RSS | peak footprint |
|---|---|---|---|
| master | 169–173 s | 2.0–2.6 GB | ~49 GB |
| PR #641 | **1.75–2.2 s** | **141–142 MB** | 133 MB |

Output **byte-identical** — all 60,002 rows — and byte-identical to master in
all 8 `--format`s, under `baseline:` / `--baseline-strict` / `--no-baseline`,
and `rigor diff` (reviewer re-measure). Reviewer probes vs the reference:
positions identical on multi-byte/CRLF/bare-CR/EOF parse errors, conformance
rows (incl. multi-byte char column 4:18), baseline-regrouped output,
run-level `(1, 1)` rows. `harness/gate.sh` clean (snapshot 0 unregistered /
0 divergent); `fp_audit --gaps --sweep` 0 FP on a partial corpus set
(mastodon, gitlab-foss, dependabot-core, net-ssh ran; mail/Ruby/
concurrent-ruby had no .rb files, haml absent on this host).

Reviews: Opus 5.5 adversarial `Approved` (re-probed both engines), Fable 5.2
design companion `sound`, Grok 4.6 (high) `Approved`.

Residuals: `mcp.rs:360/367/416` and `lsp.rs` `offset_to_position` still walk
the prefix per diagnostic — the same wall through MCP/LSP publish (#642);
`annotate`'s per-node `line_of` too. The newline table being rebuilt in ~6
places is the same issue's wholesale-remediation half.
