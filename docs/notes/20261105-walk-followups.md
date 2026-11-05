# Re-pin walk follow-ups #472/#473/#476/#479/#482 (2026-11-05)

The v0.4.x walk ([v0.4.7 note](20261105-repin-v047.md)) left five issues of
port-only rows that predate the walk. Six PRs fixed them at pin `v0.4.7`, and
all six were merged on 2026-11-05.

| PR | issue | port change |
|---|---|---|
| #483 | #482 1–2 | Rows inside a dead arm only the typer sees are dropped. `\| ...` overloads of a plugin member are merged into one arity envelope; the return is kept only when both sides agree. An unbuildable class side declines arity. |
| #484 | #472 2–3, #482 | sig-gen prints the reference's stderr skip summary. `--write` counts a core-class reopening as user-authored. A def-local literal collection names its shape in messages (`for [1]`). |
| #485 | #476 1–2 | Singleton slot writers decline superseded folds. `severity_overrides` keys that are not Strings are printed with Ruby `inspect`. |
| #486 | #479 1–3 | An out-of-range Tuple index declines, as does a literal `concat`. `instance_eval { extend M }` extends the eval owner. |
| #487 | #473 | A parent's visibility is taken from its last write; reopenings in different files that disagree decline. A file-scope closure reads the binding at the closure, not a later write. |
| #488 | #472 1, #479 4 | `pre_eval:` files are scanned, and a call that a patch answers declines. This is decline-only, so it covers check, LSP, mcp, coverage, type-of and annotate. |

**Result:** 0 FP over the sweep. MATCHED is unchanged on every corpus (623
gaps). `check` behaviour under project `.rigor.yml` / `sig/` was probed by hand,
since the sweep reads neither. Before any merge, all six branches were merged
together and gated (tests, `gate.sh`, clippy 1.88, release sweep), so an
interaction between them could not slip in. Two conflicts were keep-both:
`tests.rs`, and the `pre_eval` decline placed ahead of #484's literal-shape
message in `call_receiver.rs`.

**Known new FP (filed):** an `attr_reader` in a reopening of a private parent
fires `def.override-visibility-reduced`. This is upstream #2013, not ported
(#492).

**Left open:** #489 (concat of adjacent literals), #490 (#476 item 3,
coverage), #491 (pre_eval), #492, #493 (sig-gen/check), #494 (singleton side of
closure failures).

**Standing-conclusion change:** the 2026-08 `pre_eval:` "CLOSED" verdict was
about *closing gaps*. That would invert the ADR-0033 provenance gate, and it
stands. Reading the patches only to decline is a different, FP-removing lever:
75 port-only rows on 52 hand projects went to 0.
