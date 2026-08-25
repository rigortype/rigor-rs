# LSP held-harvest mini-spec (2026-08-25)

The frozen-index arc's LSP consumer, right-sized by what #92/#94 measured.
The recon note's "layered index + masking" is NOT needed: after #94, merge
alone is 82.5ms @ 4,675 files — under the OverlayGuard's 100ms budget at the
median. What still breaks the budget is the `build_project` WRAPPER the LSP
calls per dispatch: it re-harvests every held AST serially (~57ms @ 4,675,
derived from the #92 stage-1/stage-2 deltas) before merging. This slice
removes that term by HOLDING per-file harvests in tier-1 state.

## Phase 0 — verify the premise (STOP if it fails)

Before writing any production code, measure on a release build:

1. Current per-dispatch overlay cost (the tier-2 `build_project` call in
   `overlay_source_index`) at 1,236 / 3,117 / 4,675 files, and the guard's
   state at 4,675. Expectation: ~100–140ms @ 4,675 (serial harvest-all +
   merge). **If the dispatch cost is already comfortably under 100ms at
   4,675, STOP and report — the slice may be unnecessary.**
2. Per-file `Harvest` memory (rough: serialize-free estimate via struct
   sizes or RSS delta of holding them). Expectation: small vs the ~50KB/file
   ASTs. If it rivals the ASTs, STOP and report.

## Design

- `ProjectContext`'s held table becomes per-file `(PathBuf,
  Arc<LoweredAst>, Arc<Harvest>)` (Arc per entry so context swaps stay
  ~100µs, the existing pattern).
- `build_overlay` (startup / structural `invalidate`): compute each file's
  harvest alongside its parse+lower. It runs on the loop thread today;
  keep that, TIME it, and report the delta (expected ~+57ms serial at
  4,675 — acceptable for a rare invalidation per the S4 decision; note if
  not).
- `reharvest_sources` (a `.rb` save): recompute BOTH the AST and the
  harvest for exactly that file, swap the pair.
- `overlay_source_index` (per dispatch): parse+lower the buffer → harvest
  the buffer (one file, ~µs) → assemble the files slice with the dirty
  file's `(harvest, ast)` pair REPLACED (same REPLACE semantics as today)
  → call `SourceIndex::merge` DIRECTLY, not the wrapper. The OverlayGuard
  now times this merge (+ the single harvest) — same 100ms budget, same
  disclosure and hysteresis, nothing else about the guard changes.
- Fallback paths (unresolvable buffer path, guard-off single-file mode)
  unchanged.

**Equivalence argument** (why no diagnostic can move): `merge` IS
`build_project`'s body (#92), and swapping the dirty file's pair is exactly
today's AST-REPLACE followed by the wrapper. The change is WHERE the
harvests come from (held vs recomputed), and a held harvest is a pure
function of (AST, frozen CoreIndex) — both of which are what the held table
already pins.

## Non-goals

- Cross-file hover/completion: NOT this slice. The sync handlers stay
  single-file; serving them needs a cached-last-good-index design with its
  own staleness policy — own slice, own spec.
- No AST eviction (merge's M3 still consumes ASTs — probe-established).
- No persistence, no guard-budget change, no `check` behavior change of any
  kind.

## Tests + acceptance gates (BARE)

1. New LSP test: a dirty-buffer edit whose diagnostics depend on cross-file
   context (a class defined in another file) publishes identical
   diagnostics before/after this change; plus a save (`didChangeWatchedFiles`)
   → next dispatch sees the re-harvested content (pins the
   `reharvest_sources` harvest-swap).
2. `cargo test --workspace` (includes `lsp_check_parity`); clippy
   `--workspace --all-targets -- -D warnings` in a fresh `CARGO_TARGET_DIR`.
3. `harness/run_snapshot.rb` PASS; release rebuild +
   `harness/fp_audit.py --gaps --sweep` 0 FP / 9,204, gap set unchanged
   (this slice must not touch `check` at all — also verify `rigor check`
   byte-identical vs a master binary on one corpus as a tripwire).
4. Timing table: per-dispatch overlay cost before/after at 1,236 / 3,117 /
   4,675; guard state at 4,675 before/after. **Success criterion: the guard
   stays ON at 4,675 at the median.** Report the tail honestly.
