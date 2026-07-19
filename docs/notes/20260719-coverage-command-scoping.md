# Coverage command scoping call + precision-mode spec (2026-07-19)

Resolves the open CURRENT_WORK item "`coverage --workers` — needs a scoping
call vs `type-scan`". Fact-finding (this session, subagent sweep of
`reference/rigor` + rigor-rs) established:

- Both `coverage` and `type-scan` are **stubs** in rigor-rs (`main.rs`
  `COMMANDS` list only; fall through to "not yet implemented", exit 2). The
  "existing type-scan" exists only as reference/spec surface.
- Reference `coverage` (`lib/rigor/cli/coverage_command.rb`, 264 lines):
  default **precision mode** walks every Prism node, classifies each
  expression's inferred type into tiers (constant / nominal / shaped /
  refined / bot / dynamic_specific / dynamic_top / top), reports aggregate +
  per-file precise-vs-Dynamic ratio; `--format=text|json`, `--threshold`
  (exit 1 below), exit 64 usage. Scan shared with `check --coverage` via
  `CoverageScan.precision_report` — **sequential**.
- Reference `--workers` is **`--protection`-mode only**: fork N processes
  (`ProtectionForkScan` → `Inference::ForkMap`, `Process.fork` + Marshal temp
  files), parent merges in path order, byte-identical to sequential.
  Precedence CLI > `RIGOR_RACTOR_WORKERS` > config `parallel.workers:` > 0.
- rigor-rs already has the shared-memory **rayon** substrate (check pipeline
  stages 1/3 `par_iter`, byte-identical, ~2.4×). The fork+Marshal model is a
  Ruby-address-space artifact; porting it would be structurally wrong here.
- MCP `rigor_coverage` (reference `mcp/server.rb`: shells
  `coverage --format=json`) is deferred solely on the command's absence.
- Reference `type-scan` uses a *different* scanner (per-AST-node-class
  recognized/unrecognized tally, no `--workers`, dev-facing probe).

## Decision

1. **Build `coverage` precision mode** (the default mode) as the next
   productization slice: `rigor coverage PATH...`, `--format=text|json`,
   `--threshold=R`, exit codes 0/1/64, text and JSON renderers byte-exact to
   the reference. The scan runs on the existing rayon file-parallel pattern
   (per-file results merged in input order — byte-identical to sequential by
   construction, same contract as the check pipeline).
2. **`--workers=N` is accepted in all modes** for CLI-surface parity (the
   reference accepts it as a command option regardless of mode) and maps to
   the rayon pool size for the scan; absent/0 → default pool. The full
   reference precedence chain (env / config `parallel.workers:`) stays
   deferred together with the check pipeline's existing deferral of the same
   knob (PORT_BACKLOG §parallelism) — one consistent stance, revisit as one
   item.
3. **`--protection` / `--mutation` / `--with-tests` (ADR-63/70) stay
   deferred** as the separate large mutation-machinery track. The flags are
   parsed and rejected with a clear "not yet implemented in this port"
   message, exit 2 (existing stub convention).
4. **`type-scan` stays deferred** (no build): precision coverage subsumes its
   product value; it has no `--workers`, no MCP consumer, and a disjoint
   scanner that would be a second port surface for a dev-facing probe.
5. **Slice 2: MCP `rigor_coverage`** — port the reference tool (read-only,
   output byte-identical to CLI `--format=json`), unblocked by (1).

## Parity model

Renderer/JSON bytes must be exact given identical tier counts. Tier counts:
byte-exact target on the harness fixture corpus; on real corpora every
divergence must be enumerated and attributed to a known, already-deferred
inference delta (the v0.3.0-RC precision deltas deferred as coverage-only) —
documented-divergence parity, mirroring the sig-gen sound-superset precedent.
Zero tolerance for unexplained diffs. Oracle invocations follow the hardened
form in `UPSTREAM.md` (#194 plugin-path hazard; note `coverage_scan.rb` is
deliberately on the bare environment upstream, but harden anyway).

## Gates (for the implementing agent)

`cargo build --offline && cargo test --offline`; `ruby harness/run.rb`;
`ruby harness/run_snapshot.rb`; `python3 harness/docs_check.py`; clippy in a
**fresh** `CARGO_TARGET_DIR` with `-D warnings`; oracle byte-diff of
`coverage` text+JSON on the fixture corpus and at least two real corpus dirs
(e.g. conference-app, mastodon app/models), `--workers` output equality
(N=1 vs default), `--threshold` exit-code behavior.
