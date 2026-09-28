# #163 — non-JSON output formats byte-identical

PR #282 (merged, head `8783eb9`). `text`, `github` and `sarif` rendered rows
differently from the reference and were ungated (the parity harness compares
JSON only).

`diagnostic_formats.rs` gains `render_text` (` [rule]` suffix, dropped for
ruleless rows; summary = `No diagnostics`, nothing on warnings-only, else blank
line + `N error(s) in M file(s)` counting error rows and their distinct paths),
`render_github` (`title=<rule>` with the reference's escape order), and
`render_sarif` (serde-derived structs so the key order is byte-exact; `ruleId`
last and `skip_serializing_if` when absent). `main.rs` routes all three through
one `fn(&[Rendered]) -> String` dispatch; JSON untouched.

Measured: gate.sh 0, CI green, sweep 0 FP / 9,337. 49 probe cells (7 formats ×
7 cases) identical except one deliberate field: SARIF `driver.version` reports
the port's own `CARGO_PKG_VERSION` where the reference writes `0.3.9` — kept by
design rather than shipping a false tool version; the new regression test pins
everything else via a `{VERSION}` placeholder. `crates/rigor-cli/tests/
output_formats.rs` pins text/github/sarif vs captured reference output
(including zero rows, warnings-only, escaping).

Residual: the reference's stderr stats block stays deferred (ADRs 36/40);
`--no-stats` is not yet a port flag.
