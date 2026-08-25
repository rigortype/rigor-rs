# LSP cross-file cache mini-spec — per-URI last-good index (2026-08-26)

The frozen-index arc's remaining LSP consumer, from
[the cache probe](20260826-lsp-crossfile-cache-probe.md). Policy decision
(made by the user, on record): sync handlers answer from the CACHED
last-good project index — bounded staleness, consistency with the
on-screen diagnostics — never a per-request rebuild.

## Design

**Cache write.** `overlay_source_index` wraps its merged index in an `Arc`;
`compute_diagnostics` returns it; `Computed` gains
`project_index: Option<Arc<SourceIndex>>`; `handle_result` stores it into
loop-owned state AFTER the existing 3-axis liveness gate passes — the cache
thereby inherits the staleness guarantees for free (probe §Q2). Entry:
`HashMap<Uri, (Arc<SourceIndex>, generation: u64)>`. Populated ONLY when the
overlay was on for that dispatch (`overlay_build.is_some()`).

**Invalidation.** Clear ALL entries on `swap_project` (generation bump —
covers structural invalidation and guard trips); remove the URI's entry on
`didClose`. A reader also checks `entry.generation == ctx.generation`
(belt-and-braces; entries can never legally outlive a generation).

**Readers — SAME-URI ONLY.** `hover` and method-`completion` consult the
cache for THEIR uri; hit → answer from the cached project index (an `Arc`
read on the loop thread, no rebuild — this is what the <100ms p95 budget
always demanded); miss or stale → today's single-file
`SourceIndex::build`, unchanged. An index cached for URI A is never used to
answer for URI B: it carries A's dirty overlay (REPLACE), so same-URI is
both the correctness guard and the "hover agrees with the published
diagnostics" UX property.

**`Foo::` namespace completion** joins the slice (the v4 note's withheld
item): `SourceIndex` gains a READ-ONLY accessor `namespace_children(ns)`
mirroring `CoreIndex::namespace_children`'s contract (immediate children
only, class-vs-module kind, deterministic order — match the existing RBS
path's ordering exactly). `namespace_completion` unions CoreIndex + the
same-URI cached index's children, deduplicated (RBS entry wins on kind
conflict), only on a valid cache hit; RBS-only otherwise. This is the ONE
change outside lsp.rs and it is read-only.

**Memory.** ~4KB/file ⇒ ~19MB per entry @ 4,675 files (probe §Q1). Evict on
`didClose` + an LRU cap of 8 entries as hygiene (probe verdict: memory does
not force the choice; the cap is not load-bearing).

## Tests (each pinned)

1. Cross-file hover: a class defined in file B, hovered in open file A
   after A's first dispatch, shows B's info; before any dispatch (or after
   `swap_project` and before the redispatch publishes) it falls back to
   single-file behavior.
2. Cross-file method completion: same shape.
3. `Foo::` completion offers a project-source nested constant defined in
   another file; RBS results unchanged; kind-conflict dedup pinned.
4. Same-URI guard: with A and B open and only A dispatched, hover in B does
   NOT see A's cached index.
5. `didClose` evicts; guard-off dispatches never populate; a generation
   bump invalidates (test with the forced-low guard threshold, the S4b
   pattern).
6. Diagnostics path unchanged: published diagnostics byte-equal before/after
   this change (the Arc hand-back must be observationally inert);
   `lsp_check_parity` green.

## Acceptance gates (BARE)

1. `cargo test --workspace`; `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy
   --workspace --all-targets -- -D warnings`.
2. `harness/run_snapshot.rb` PASS; release rebuild +
   `harness/fp_audit.py --gaps --sweep` 0 FP / 9,204, gap set unchanged;
   `rigor check` byte-identical vs a master binary on mastodon/app
   (tripwire — check must be untouched).
3. Report: hover/completion latency on a cache hit at 4,675 files (expect
   ~instant; the p95 <100ms budget must hold with headroom), and cache
   memory at 8 entries.

## Non-goals

`documentSymbol` (reads no SourceIndex), cross-URI freshness, persistence,
guard-policy changes, any change to what diagnostics are published or when.
