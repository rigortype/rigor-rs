# Coverage precision mode — implementation + measured parity (2026-07-19)

Implements the two slices scoped in
[20260719-coverage-command-scoping.md](20260719-coverage-command-scoping.md):
the `rigor coverage` default (precision) mode and the read-only MCP `coverage`
tool. Branch `coverage-precision-mode`; oracle = the pinned reference
`7a69f142`, every invocation hardened per `UPSTREAM.md` (#194 plugin path),
from a clean temp cwd.

## Construction (what is byte-exact by design)

- **Denominator**: the scan walks the real Prism tree via the `ruby_prism`
  `Visit` trait — the identical `compact_child_nodes` child set the reference's
  `NodeWalker`/`rigor_each_child` yields — minus the same
  `NON_EXPRESSION_NODE_TYPES` list. Measured: `expressions_typed` is equal on
  **70/70 fixtures, 4235/4235 (conference-app/app), 31381/31381
  (mastodon/app/models)** — zero denominator drift anywhere.
- **Renderers**: text + JSON are hand-ported (Ruby `JSON.pretty_generate`
  shape, `Float#round` half-away-from-zero + `Float#to_s` spellings, `ljust`/
  `rjust`, the `files - parse_errors` header arithmetic). Proven byte-exact by
  the 31 whole-output fixture matches and the byte-identical multi-file run
  below.
- **File resolution**: `collect_paths` reproduces the reference exactly,
  including Ruby `Dir.glob("**/*.rb")`'s *per-directory* traversal order (the
  `admin/` dir sorts before the `admin.rb` file, so the subdir's files emit
  first — a flat path sort is WRONG and was caught + fixed against
  conference-app). File lists are byte-identical on both real corpora.
- **Tiers**: per-node classification reproduces `ExpressionTyper#type_of`'s
  `PRISM_DISPATCH`: value leaves route to the existing `rigor_infer::Typer`
  (the `type-of`/`check` substrate — no parallel mapping) through an
  exact-span Prism→arena map; structural handlers (if/unless branch elision,
  and/or short-circuit, case, begin/rescue, statements-tail, jumps→Bot,
  loops→Constant[nil]) compose at the tier level with Bot-absorbing unions.

## Measured parity

Whole-output byte-diff (text AND json), per file:

| target | result |
|---|---|
| harness fixtures (70 files) | **31/70 byte-exact** (json and text agree file-for-file); all 39 divergences enumerated below, 0 unexplained |
| fixtures, all-70-in-one-run | denominator + file list + header byte-identical; per-file entries 30/69 exact (the 31st single-run match is the parse-error fixture) |
| conference-app/app (98 files) | denominators + file list identical; 49/98 files tier-divergent; ratio ref 0.3922 vs rs 0.3636; **0 over-claims** |
| mastodon/app/models (248 files) | denominators + file list identical; 223/248 files tier-divergent; ratio ref 0.4911 vs rs 0.4282; **0 over-claims** |

Over-claim audit (the sound-subset direction check — rs must never report MORE
precision than the reference): on both real corpora, **no file** has
`rs precise_count > ref` or `rs dynamic_top < ref`. One fixture is the single
exception, listed as delta family F below.

Switch checks:

- `--workers`: `--workers=1`, `=7`, `=0`, and absent all byte-identical
  (json + text) on mastodon/app/models (248 files).
- `--threshold`: fixture 01 (ratio 0.75): 0.5→exit 0, 0.9→exit 1, equal to the
  oracle's exits. mastodon (rs ratio 0.4282): 0.4→0, 0.5→1. `--threshold 0.5`
  space form accepted.
- Exit 64 + the reference's exact stderr on a bad path
  (`coverage: not a file or directory: X`); exit 64 on no paths; exit 2 +
  "not yet implemented in this port" for `--protection` / `--mutation` /
  `--with-tests` / `--test-command` / `--include-dynamic` / `--limit` /
  `--seed` (the deferred ADR-63/70 track).
- MCP `coverage` tool: output byte-identical to CLI `--format=json`
  (unit-asserted, including the trailing `puts` newline the reference's
  `out_io.string` carries).

## Divergence attribution (all 39 fixture divergences + both corpora)

Every divergence is a TIER difference on an identically-counted node set, and
every one traces to an already-documented rigor-rs inference delta — the
zero-FP sound-subset dispatch contract (ADR-0023) plus the deferred ADR-0022
flow substrate plus the v0.3.0-RC precision cluster (UPSTREAM.md pin note).
Families, with the fixtures they explain (per-fixture numeric table:
scratchpad capture, reproduced by the sweep commands below):

- **A. Implicit-self / Kernel call returns** — the reference types `puts`→nil,
  `private`/`attr_reader`→Constant, `require`→bool; rigor-rs's implicit-self
  dispatch deliberately declines everything but the ported `p`/`pp` fold →
  `Dynamic[top]`. Fixtures 23–26, 30, 33–36, 38, 40, 66; the dominant family
  on both real corpora.
- **B. Flow-substrate facts (ADR-0022 deferral)** — branch-join rebinds, loop
  convergence, ivar/cvar/global accumulators, `$~`/`$1` narrowing,
  interprocedural literal tails beyond the ADR-0038 slice, mutation-widening
  precision. Fixtures 33/34 (predicate locals), 42, 50, 59, 60, 65, 67.
- **C. Constant-fold chain depth** — the Rust core folder declines mid-chain
  (`"s".downcase.strip.reverse` stops at Nominal[String] after one hop);
  ref pins the value. Fixtures 10, 12, 19, 22 — note 10/12/19 keep
  `precise_count` EQUAL (constant↔nominal shuffle inside the precise family).
- **D. `raise` → Bot** — the reference types a `raise`/`fail` call Bot;
  rigor-rs treats it as an undeclared call → `Dynamic[top]`. Fixtures 54/55
  (−17/−16 bot), 28 (−1).
- **E. Shape/refined carriers** — RBS-generic returns the reference elaborates
  to Tuple/HashShape/App, and `Type::Refined` narrowing results, on paths
  where rigor-rs yields Nominal or declines. Fixtures 29, 43 (−21 shaped),
  53 (refined↔nominal, precise_count equal), 57, 63, 64, 68–70.
- **F. Nilable-return side-channel (the ONE contrary-direction case)** —
  fixture 27: rigor-rs binds `x = s.byteslice(0, 2)` to `String`, carrying
  nil-ness in the ADR-0038 provenance side-channel rather than the type
  carrier, so `x.upcase` types Nominal where the reference (which binds
  `String | nil`) declines the dispatch → Dynamic. **+1 precise node, this
  fixture only; 0 occurrences across all 346 real-corpus files.** Substrate
  behaviour (verified via `type-of` directly), not a scanner artifact;
  changing it means changing how the possible-nil substrate represents
  nilability — out of scope for a measurement-command slice.

Unexplained diffs: **0**.

## Gates (all green)

- `cargo build --offline` + `cargo test --offline --workspace` (825 tests, 0 fail;
  9 new coverage/MCP tests, each pinned to an oracle-measured shape)
- `ruby harness/run.rb`: PASS, 0 unregistered FP, 216/218
- `ruby harness/run_snapshot.rb`: PASS
- `python3 harness/docs_check.py`: PASS
- `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy --workspace -- -D warnings`: clean

## Repro

```sh
# oracle (hardened), from a clean temp cwd, absolute paths:
ruby -I <repo>/reference/rigor/lib -I <repo>/reference/rigor/plugins/rigor-rbs-inline/lib \
  <repo>/reference/rigor/exe/rigor coverage <abs-path> --format json
# rigor-rs:
<repo>/target/release/rigor coverage <abs-path> --format json
```
