# S92 harvest/merge mini-spec (2026-08-25)

Implements issue #92 from the probe evidence in
[the pass inventory](20260825-s92-buildproject-pass-inventory.md) (direction:
[the Glancer recon](20260825-rust-glancer-frozen-index-recon.md)). This is a
PURE REFACTOR: no emitted diagnostic may change, anywhere, under any file
order or thread count. The probe's `probes_s92` fingerprint harness is the
instrument that proves it.

## Goal

```
build_project(asts, core)  ≡  merge(files.map(|f| (harvest(f.ast, core), f.ast)), core)
```

bit-identical on the 17-field fingerprint, with `harvest` per-file and
embarrassingly parallel, and `merge` serial and deterministic. `build_project`
survives as a thin wrapper so every existing call site is untouched.

## Shape

**`pub struct Harvest`** — one file's contribution, computed from that file's
AST + the frozen `CoreIndex` alone (never from other files). Fields, per the
probe's classification:

- *Pure unions* (order-free): `toplevel_defs`, `discovered_methods`,
  `mutated_params`, `constant_write_bare_names`.
- *Ordered replay* (first-write-wins / append semantics reach diagnostics):
  `source_classes`, `override_classes` entries (incl. `method_visibilities` +
  `includes`), `rbs_constant_names` (pre-filtered against the frozen
  CoreIndex — legal because Pass 2's cross-file-looking `classes` read is an
  idempotent no-op, probe §Pass 2), `constant_writes` **with a per-file write
  count per qualified name** (Σ ≥ 2 across files-and-within-a-file ⇒ multi;
  reproduces `lit_first`/`lit_multi` exactly), `fold_defs` with
  **file-relative** sites (today's `FoldSite.ast_idx` is a slice position —
  merge re-bases; do NOT bake a global index into `Harvest`).

**`SourceIndex::harvest(ast, core) -> Harvest`** — extraction of today's
passes 1, 1b, 1c, 1d, 1e, C5a, 2, 4a(harvest half).

**`SourceIndex::merge(files: &[(Harvest, &LoweredAst)], core) -> SourceIndex`**
— three internal phases, in order:

- **M1 ordered replay**: apply each file's ordered fields in the GIVEN slice
  order. The order is the CALLER's (`expand_check_paths`: per-arg sorted
  expansion, concatenated in argument order — `crates/rigor-cli/src/main.rs`).
  Merge must never sort; sorting changes diagnostics (probe examples 1/2).
- **M2 barrier aggregates** (need the complete replayed state, cheap): C1
  constant-shadow tables, C5b literal-constant gates, 2b tuple-element
  registry.
- **M3 AST-consuming passes** (run as today over the complete index + the
  ASTs): Pass 3 tier-4b returns (Typer), Pass 4a inversion (`definers`) +
  Pass 4b `literal_returns`. These are WHY merge takes the ASTs; see
  Non-goals.

## CLI wiring (same slice)

Move `harvest` INTO stage 1's existing per-file rayon closure (alongside
parse+lower, collected in input order — the ADR-0028 ordering contract is
untouched: harvests are frozen before merge starts). Stage 2 becomes
`merge` only. Expected stage-2 reduction ≈ the probe's ~43% decomposable
share; report `RIGOR_TIMING` before/after at 1,236 / 3,117 / 4,675 files.
Implement in two commits: (1) serial harvest inside `build_project`, prove
bit-identity; (2) hoist to stage 1 + wrapper, prove again. The LSP keeps
calling `build_project` — its redesign is the NEXT slice, not this one.

## Invariants to pin (each gets a test)

1. **Order is normative**: probe examples 1 (visibility first-write-wins) and
   2 (includes append order) as unit tests — `a,b` fires, `b,a` silent,
   before AND after.
2. **Cross-file couplings behave identically**: probe examples 3 (C5 →
   Pass 3), 4 (Pass 4b degrade), 5 (Pass 2b declaration-only) as tests.
3. `lit_first`/`lit_multi` incl. INTRA-file duplicate writes (per-file count,
   not a bool).
4. `register` stays idempotent (the Pass-2 no-op argument depends on it).
5. `HarvestedConst.file_id` semantics unchanged (in-memory process-global
   counter; add a comment naming the persistence hazard: a persisted harvest
   must re-stamp it, probe §must-not-miss 2).
6. Fix the FALSE doc claim at `source_index.rs:1613-1615` ("a method never
   appears in BOTH maps" — probe 8 disproves it across a cross-file reopen);
   restate the truth (call sites consult `method_returns` first). Do not
   lean on the old claim anywhere in the new code.
7. Promote `probes_s92` from throwaway to permanent equivalence tests, plus
   a NEW probe: fingerprint(`merge`) == fingerprint(old inline path) on the
   toy corpora — keep the old path alive behind `#[cfg(test)]` for exactly
   this comparison, or pin against committed fingerprint snapshots.

## Acceptance gates (run BARE, in this order)

1. `cargo test -p rigor-infer` (incl. the probes + new pins), then workspace
   tests.
2. `cargo clippy --workspace --all-targets -- -D warnings` in a FRESH
   `CARGO_TARGET_DIR`.
3. `harness/run_snapshot.rb` — PASS, 0 unregistered extras.
4. Rebuild release, then `harness/fp_audit.py --gaps --sweep` — **0 FP /
   9,204 files, gap set unchanged** (a moved gap is a failure: pure
   refactor).
5. `rigor check` byte-identical vs master on mastodon/app under BOTH
   `RAYON_NUM_THREADS=1` and default threads.
6. `RIGOR_TIMING` stage-1/stage-2 before/after table in the impl note.

## Non-goals (probe-revised — these corrections supersede the recon note)

- **AST eviction is NOT unblocked** — M3 consumes ASTs at merge time
  (`typer.type_of`, `fold_expr` cross-AST deref). Capturing pass-3/4b inputs
  into harvests is its own future investigation.
- **The OverlayGuard does not fall with this slice** — merge-resident passes
  are ~57% of `build_project`; the guard's removal additionally needs #94
  (`compute_literal_returns` is 43–47% alone).
- No LSP changes, no persistence, no `literal_returns` work (#94), no
  diagnostic deltas of any kind.

## Follow-up order after this lands

#94 (Pass 4b cost) → LSP layered-index slice (needs #92, wants #94) →
re-evaluate the on-disk harvest cache against the #93 baselines.
