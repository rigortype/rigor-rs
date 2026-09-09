# Parse-error reporting: the port stops answering `[]` on a file it cannot read (2026-09-09)

Closes bucket 01 of the [v0.3.8 gap adjudication](20260909-gap-adjudication-799.md)
— 9 rows, 4 files, `rigor-survey/Ruby/searches/`.

**The defect.** `rigor check` on a file Prism could not parse answered `[]` and
**exited 0**. A CI gate built on the port read a file it could not read as
clean. The reference answers one `error`-severity diagnostic per Prism error and
`success: false`.

**What did NOT change.** The guard at `result.errors().next().is_some()` in
`crates/rigor-cli/src/main.rs` still skips an unparseable file **index
included** — no inference, no rule diagnostics — because Prism's error recovery
invents bindings the rules over-fire on (ADR-0016's never-crash posture
surrounds it, and `harness/corpus/72_parse_error_no_rules.rb` pins it). Only the
REPORTING half is ported. The two halves are separable because the reference
separates them itself: `Runner#analyze_file_body` RETURNS `parse_diagnostics`
before any rule runs.

## The measured contract

Re-verified at the pin (`ffb456b0`, ruby 4.0.5 / rbs 4.2.0, fresh temp cwd,
`--no-cache`, both reference libs on `-I`). The reference's producer is four
lines long (`lib/rigor/analysis/runner.rb`, `parse_diagnostics`):

| field | value |
|---|---|
| count | **one row per raw Prism error, 1:1** — no filtering, no dedupe, no per-line collapsing |
| severity | `error` |
| rule | **`nil`** (defaulted; `Diagnostic#qualified_rule` returns `nil`) |
| source_family | `builtin` |
| line / column | `error.location.start_line` / `error.location.start_column + 1` |
| message | Prism's message, verbatim |
| exit / `success` | 1 / `false` |

Everything above reproduced. Two terms were probed rather than assumed:

- **Prism *warnings* are NOT reported.** A file with one Prism warning
  (`found '= literal' in conditional`) and zero errors gives the reference zero
  parse rows. `parse_diagnostics` never reads `warnings`.
- **A parse error can never be suppressed or re-stamped.** `SeverityStamp.stamp`
  short-circuits on `diagnostic.rule.nil?`, and `filter_suppressed` says
  "Diagnostics with `rule == nil` … are NEVER suppressed — they represent
  failures the user cannot silence away". Probed end to end: a broken file
  carrying `# rigor:disable-file all` still reports both rows, in both engines.
  The port therefore pushes these diagnostics **straight into `findings` from
  the stage-1 drain**, bypassing stage 3 (suppression / `disable:` / severity
  profile) exactly as the reference's early return does.

Per-file oracle comparison on all four corpus files, field by field and in
order: `binary_search` 2 rows, `fibonacci_search` 3, `linear_search` 2,
`ternary_search` 2 — **9/9 exact, exit codes 1/1**.

**Ordering and attribution** across a multi-file run: a directory of
`a_broken.rb` / `b_clean.rb` / `c_broken.rb` / `d_clean.rb` gives both engines
the same 7 rows in the same order — parse errors sit in their own file's slot,
interleaved with rule diagnostics from the clean files, and `c_broken.rb`'s
three errors on ONE line stay three rows. The port gets this for free: the
parse-error findings carry the file's input-order key and
`findings.sort_by_key` is stable.

**The exit code needed no change.** `finding_fails_run` is
`severity == Error || rule_id == "internal-error"`, so an `error`-severity parse
row fails the run on its own. Verified, not assumed: port exit 1 on each of the
four files and on the mixed directory. `cmd_check` is untouched.

## `null` vs `""` — the decision

`Diagnostic.rule_id` is `&'static str` and over fifty call sites read it as a
plain string (the `disable:` matcher, the severity stamp, the baseline binner,
the LSP `code`, triage, MCP). Two options:

1. `rule_id: Option<&'static str>` — mechanically correct, but a rewrite of
   every one of those sites for the sake of ONE producer, with a real chance of
   changing an unrelated behaviour in passing.
2. **A sentinel plus an accessor** — `rigor_rules::NO_RULE` (`""`) for the
   field, and `Diagnostic::qualified_rule() -> Option<&'static str>` that maps
   it back to `None`. Chosen.

This is the reference's own shape: it stores a nullable `rule` and reads it back
through `Diagnostic#qualified_rule`, which returns `nil`. Every emitter goes
through the accessor, so "what does a ruleless diagnostic look like here" is one
decision per emitter and cannot silently render as `""`.

**Why `""` in the output would be a bug and not a cosmetic one.**
`harness/lib.rb`'s `DiagKey` is `(rule, line, column)` and the reference gives
Ruby `nil`. `""` is a different key, so the row would score as a coverage gap
AND an unregistered extra at once — the slice would manufacture a false positive
in the very gate that grades it. The risk the sentinel carries is an emitter
that forgets to map; that is mitigated by routing every emitter through the
accessor and by a test per emitter.

### Emitters touched

Measured against the reference on `binary_search.rb` at the pin, format by
format:

| emitter | ruleless rendering |
|---|---|
| `--format json` (`print_json`) | `"rule": null`; no catalogue enrichment |
| `--format text` (`print_text`) | unchanged — the port never appended `[rule]`, so it already matched `path:line:col: error: message` |
| `--format github` | unchanged — the port emits no `title=` property |
| `--format sarif` | result carries NO `ruleId` key; the rule is not declared in `driver.rules` |
| `--format gitlab` | `check_name` falls back to `"rigor"`; description has no `[rule]` bracket |
| `--format checkstyle` | the `source=` attribute is ABSENT, not empty |
| `--format junit` | `classname` falls back to `"rigor"` |
| `--format teamcity` | message has no `[rule]` bracket |
| `rigor diff` | `"rule": null`, so a diff against a reference `--format json` baseline compares like against like |
| `rigor triage` | buckets under the reference's `UNCATEGORISED` = `"(uncategorised)"` |
| `rigor check --baseline` | ruleless rows bypass the baseline entirely — never binned, never silenced, and `baseline generate` writes no row for them (reference `group_for_baseline`'s `next if diag.qualified_rule.nil?`) |

`crates/rigor-cli/src/mcp.rs` and `lsp.rs` need no change: both skip or never
reach an unparseable file, so neither can produce a ruleless diagnostic.

While wiring the SARIF test it turned out `sarif_value` in `main.rs`'s test
module was a **re-implementation** of `print_sarif`, so it could not have caught
a change to the emitter it was meant to pin. `print_sarif` and `print_json` are
now split into a `sarif_document` / `json_document` builder plus a printer, and
the test calls the real builder.

## The fixture, and what the trap turned up

`harness/corpus/107_parse_errors.rb` — a dangling `else` (the survey corpus's
shape) plus a broken parameter list (three Prism errors on ONE line). Five rows,
each oracle-measured at the pin, and the snapshot records `"rule": null`.

**The brief's premise was wrong, and that is the finding.** "The corpus has
never contained a file that does not parse" — it has, since
`72_parse_error_no_rules.rb`, the fixture that pins the SKIP half of the
standing decision. Consequences:

- Every corpus tool was already exercised on an unparseable fixture, so nothing
  broke: `harness/snapshot.rb` wrote 1 new snapshot and left all 106 others
  untouched, `run.rb` / `run_snapshot.rb` ran 107 fixtures, `docs_check.py`
  passed. The remaining corpus consumers (`fp_audit.py`, `gap_census.py`,
  `run_corpus.rb`, the effects generators) walk OSS checkouts or snapshots, not
  `harness/corpus/*.rb`, and none of them parse Ruby themselves.
- Fixture 72's own header claimed "The reference's parse diagnostics
  (`rule: null`) are a coverage gap here — rigor-rs emits none". That sentence
  is now false; it is corrected in place (same line count, so its snapshot is
  unchanged).
- It explains the harness arithmetic below: fixture 72's **3** rows were part of
  the standing 50 gaps and close along with the new fixture's 5.

The must-still-fire control is the rest of the corpus: 106 fixtures, 507
matched, 0 unregistered, unchanged.

## Gates

| gate | before | after |
|---|---|---|
| `harness/run_snapshot.rb` | PASS — 106 fixtures, ref 558, rs 508, **507 matched / 50 gaps** / 0 unregistered | PASS — 107 fixtures, ref 563, rs 516, **515 matched / 47 gaps** / 0 unregistered |
| `cargo test --workspace` | — | ok, 0 failed |
| `cargo clippy --workspace --all-targets -- -D warnings` | — | clean |
| `python3 harness/docs_check.py` | PASS | PASS |

`+8 matched / −3 gaps` decomposes exactly: `+5` new fixture rows, and `+3`
fixture-72 rows moving from the gap column to the matched column. `+5 −3` on the
gap count, `+5 +3` on the matched count. No other row moved.

## Corpus evidence (instead of the 80-minute sweep)

Release binary before and after, over `rigor-survey/Ruby` plus three more sweep
corpora (`net-ssh`, `concurrent-ruby`, `haml/lib`). 5841 rows → 5850 rows; the
complete diff is **nine additions and nothing else** — nothing lost, nothing
moved, no row's rule/position/message changed:

```
Ruby/searches/binary_search.rb    28:1  error  rule=nil  unexpected 'else', ignoring it
Ruby/searches/binary_search.rb    30:1  error  rule=nil  unexpected 'end', ignoring it
Ruby/searches/fibonacci_search.rb  1:25 error  rule=nil  expected a delimiter to close the parameters
Ruby/searches/fibonacci_search.rb  1:26 error  rule=nil  unexpected write target
Ruby/searches/fibonacci_search.rb  1:35 error  rule=nil  unexpected local variable or method, expecting end-of-input
Ruby/searches/linear_search.rb    16:1  error  rule=nil  unexpected 'else', ignoring it
Ruby/searches/linear_search.rb    18:1  error  rule=nil  unexpected 'end', ignoring it
Ruby/searches/ternary_search.rb   41:1  error  rule=nil  unexpected 'else', ignoring it
Ruby/searches/ternary_search.rb   43:1  error  rule=nil  unexpected 'end', ignoring it
```

That is bucket 01's 9 rows, exactly, and each was compared field-by-field
against the reference on its own file. The other three corpora are byte-identical
before and after.

## Residues

- **Path errors are still not ruleless.** The reference gives `rule: nil` to
  "parse errors, path errors, internal analyzer errors"; rigor-rs invents
  `path.not-found` / `path.not-ruby` / `internal-error` for the other two, which
  `prepend_path_errors` and `internal_error_diag` document as deliberate. They
  could now go through `NO_RULE` — but `internal-error` is load-bearing in
  `finding_fails_run` and in the severity-stamp bypass, so that is its own
  slice with its own oracle probes, not a rider on this one.
- **The gitlab fingerprint joins on `\0`; at v0.3.8 the reference joins on a
  space** (`[...].join(" ")`), while the port's comment claims the reference
  uses `\0`. Pre-existing and unrelated to rulelessness — it makes EVERY row's
  fingerprint differ, not just a ruleless one. Not touched here; worth a
  separate look, since a `--format gitlab` consumer dedups on it.
- **SARIF key order** for rule-carrying rows was preserved as rigor-rs has
  always emitted it (`ruleId` first); the reference appends `ruleId` last. Also
  pre-existing, also untouched.
- The full `fp_audit.py --gaps --sweep` was NOT run (~80 min at this pin); the
  four-corpus before/after diff above stands in for it.
