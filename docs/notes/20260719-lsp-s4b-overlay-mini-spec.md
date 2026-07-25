# LSP §12 S4b — cross-file overlay mini-spec (2026-07-19)

The last tier-1 item of [the §12 plan](20260719-lsp-s12-two-tier-impl-plan.md)
(item 8). Today the LSP builds a SINGLE-FILE `SourceIndex::build(&ast, index)`
at three sites (`lsp.rs` diagnostics / hover / completion), so the editor has
no cross-file project knowledge and its diagnostics can differ from `check`'s
for the same file. S4b closes that for the DIAGNOSTICS path.

## Measured substrate cost (this session, `RIGOR_TIMING=1`, release build)

| corpus | files | stage1 parse+lower | **stage2 `build_project`** | stage3 analyze |
|---|---|---|---|---|
| conference-app | 244 | 17.0ms | **2.3ms** | 4.5ms |
| mastodon/app | 1236 | 49.2ms | **28.8ms** | 15.8ms |
| gitlab-foss/lib | 4675 | 148.8ms | **236.9ms** | 44.7ms |

`build_project` grows superlinearly (~5× files → ~12× time). Against
ADR-0029's budget (didChange→publish < 250ms p50, < 500ms p95) a per-dispatch
rebuild is comfortable to ~1.5k files and blows the p50 budget alone at 5k.

`SourceIndex` has no per-file provenance and no merge/extend API
(`build_project(asts, core)` is an additive multi-pass harvest over ALL ASTs;
`#[derive(Default)]` only — not `Clone`). So "replace one file's contribution"
is not expressible today without a rigor-infer change.

## Decision — Architecture A (swap-and-rebuild) + a measured scale guard

**Per-dispatch**: rebuild the project `SourceIndex` from the tier-1 held ASTs
with the dirty buffer's file's AST **REPLACED** by the buffer's AST, then run
the existing `analyze_with_source_and_folder` against it. Replacement, not
addition — registering both the on-disk and buffer versions of the same file
would give the index two competing method/return facts and risks a wrong type
(an FP), which this project does not trade away for speed.

Correct by construction: the resulting index is exactly what `check` builds
for the same file set, so LSP diagnostics equal `check`'s (the acceptance
test below). No new inference semantics.

**Tier-1 (`ProjectContext`, per S4, rebuilt synchronously on invalidation)**
gains: the project's file list + their `LoweredAst`s (held), and the cost of
its own `build_project` call, TIMED at build.

**Scale guard (no silent budget blowout)**: if the tier-1 `build_project`
timing exceeds a threshold (propose **100ms**, leaving headroom under the
250ms p50), the session DISABLES the overlay and falls back to today's
single-file index, disclosing the posture via `window/showMessage` — the
ADR-0036 posture-disclosure precedent already used at LSP startup for the
sidecar. Never silently degrade, never silently blow the budget.

**hover / completion / documentSymbol stay on the single-file index in v1.**
They are answered synchronously on the loop thread under a <100ms p95 budget,
which cannot absorb a project rebuild; and serving them from a *saved-state*
project index would answer from stale facts after an edit. Cross-file hover /
completion is a follow-up gated on the extend API below.

## Deferred follow-up (NOT S4b)

`SourceIndex::extend_with(&mut self, ast, core)` — an incremental
single-AST extension that would make the overlay O(1 file) and unblock both
5k-file projects and cross-file hover/completion. **Needs investigation
first**: `build_project`'s later passes may depend on complete earlier-pass
state (e.g. the lexical override index), in which case extending by one AST
is not semantically equal to a full rebuild. That investigation + API is its
own slice with its own parity evidence; do not attempt it inside S4b.

## Acceptance

1. **Parity with `check`**: for a SAVED (non-dirty) buffer, the LSP's
   diagnostics MUST equal `rigor check <file>`'s diagnostics for that file.
   Test on real corpus files where cross-file context changes the answer
   (a class defined in another file — today's single-file index misses it).
   This is the test that proves the overlay works at all.
2. **Dirty-buffer overlay**: an edit that adds/removes a method in an open
   buffer changes that file's own diagnostics without a save.
3. **Replacement, not addition**: renaming a method in the buffer must not
   leave the on-disk name resolvable (pins the REPLACE semantics).
4. **Guard**: with the threshold forced low in a test, the session falls back
   to the single-file index and emits the `window/showMessage` disclosure.
5. All S1–S4 behavior preserved (debounce, 3-axis stale-drop, single-writer,
   invalidation, no-lost-update); harness `run.rb`/`run_snapshot.rb` 0 FP.

## Risks to measure and report

- **Memory**: holding every project `LoweredAst` at 5k-file scale, against
  ADR-0029's <600MB target. Report RSS at the three corpus scales. If the
  guard already disables the overlay at that scale, ALSO consider dropping
  the held ASTs when the guard trips (no overlay ⇒ no need to hold them).
- Tier-1 rebuild latency now includes parse+lower of the whole project
  (149ms at gitlab-lib scale) on the loop thread — acceptable for a rare
  invalidation per S4's synchronous-rebuild decision, but report it.
