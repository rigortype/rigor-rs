# LSP cross-file cache — per-URI last-good project index (impl, 2026-08-26)

Implements [the mini-spec](20260826-lsp-crossfile-cache-mini-spec.md) from
[the probe](20260826-lsp-crossfile-cache-probe.md). Branch
`claude/lsp-crossfile-cache`, cut from `43644fc`. Closes the frozen-index arc's
last LSP consumer: the synchronous handlers (`hover`, method-`completion`,
`Foo::` `namespace_completion`) now answer from the **cached last-good project
`SourceIndex`** the diagnostics dispatch already built, instead of a per-request
single-file rebuild.

## What changed

**The `Arc` hand-back** (`crates/rigor-cli/src/lsp.rs`). The merged index used to
die on the rayon worker at the closing brace of `compute_diagnostics`'s
`catch_unwind` closure. Three mechanical signature changes carry it home:
`overlay_source_index` returns `(Arc<SourceIndex>, Option<Duration>)`;
`compute_diagnostics` returns a third slot `Option<Arc<SourceIndex>>`, `Some`
on **exactly** the condition the guard sample is (`overlay_build.is_some()`);
`Computed` gains `project_index`. The guard's timer still stops on the *merge*,
before the `Arc::new`, so the scale guard measures the same quantity it always
did.

**The cache** (`CrossFileCache`, loop-owned like every other `Session` field).
`HashMap<uri_key, CachedIndex { index, generation, used }>` plus a monotonic LRU
`clock` — a counter, not an `Instant`, so eviction order is deterministic in
tests. Written in `handle_result` **inside the existing 3-axis liveness branch**,
so a result that is fit to *publish* is what gets cached and nothing else: the
cache inherits version / generation / epoch staleness for free rather than
inventing its own, and an entry is never staler than the markers on screen.

**Invalidation.** `swap_project` clears the whole map — one site, so config
reloads, watched-file re-harvests, empty-project swaps and guard trips are all
covered without any of them having to remember to. `didClose` evicts the URI's
entry. A reader also re-checks `entry.generation == ctx.generation`
(belt-and-braces; `swap_project`'s clear means it can never legally fire). LRU
cap 8.

**Readers, SAME-URI ONLY.** `crossfile_for(st, uri)` is a single free function
both handlers go through, so they cannot drift apart on the rule that carries the
correctness: an index cached for A holds A's dirty buffer *replacing* A's on-disk
file, so serving it for B would put A's unsaved edits into B's answers — the
double-registration hazard the REPLACE rule exists to prevent, moved into the
query handlers. Miss ⇒ today's single-file `SourceIndex::build`, untouched. Both
handlers use deferred init (`let single_file; match cached { … }`), so the
single-file index is not even *built* on a hit.

**`Foo::` namespace completion** (the LSP-v4 note's withheld item) unions the RBS
children with the project's own. The one change outside `lsp.rs`:

**`SourceIndex::namespace_children`** (`crates/rigor-infer/src/source_index.rs`),
read-only, mirroring `CoreIndex::namespace_children` field for field — immediate
children only, `(leaf, is_module)`, `BTreeMap`-collected so the order is the
RBS path's existing name-sorted order. It enumerates the ADR-35 **lexically
qualified** override registry, which is the only project-wide table keyed by
fully-qualified name (the collapsed `classes` map cannot answer a namespace
question: `Foo::Bar` and `Baz::Bar` share the key `Bar`). The union inserts the
project's children first and the RBS children **last**, so RBS — the declaration
of record — wins a kind conflict, and the name still appears exactly once.

## Deviation from the letter of the spec (one, with its reason)

**`is_module` had to be plumbed, because `SourceIndex` did not carry it.** The
spec asks the new accessor for "class-vs-module kind", and the probe assumed the
data was already there ("SourceIndex does carry project-registered classes …
only the enumeration API over it is missing"). It is not: `override_classes`
records `superclass: None` for a module *and* for a bare `class Foo`, so the two
are indistinguishable after harvest. Rather than mislabel every project child
`CLASS` (which would match the reference, but not the spec, and would make the
"RBS wins on kind conflict" rule vacuous), one `bool` was added to
`HarvestedOverrideClass` / `OverrideClass`, set at the single `collect_override_classes`
walk site and replayed through `ingest_override_class` with **first-write-wins**
semantics (`or_insert_with`, matching `superclass`; Ruby raises on a
class/module mismatch, so a reopen can never legally disagree).

This is strictly more than "one read-only accessor", so it is called out rather
than buried. It is inert for diagnostics by construction: nothing but the new
accessor reads the field, the merge fingerprint (`probes_s92`) does not include
it, and gates 3-5 below confirm no observable movement.

## Measured costs and gains

| | miss (today's single-file index) | hit (cached project index) |
|---|---|---|
| hover on a cross-file call | `Dynamic[top]#label → Dynamic[top]` | `Beta#label → String` |
| completion on a cross-file receiver | null (0 items) | 259 items (the `String` surface) |
| `Wrapper::` (project-only namespace) | null | `Inner` (CLASS), `Mixin` (MODULE) |
| `Process::` (RBS namespace) | 6 RBS children | **byte-identical** 6 children |
| `FOO` where the same file has `FOO = 5` | `FOO : 5` | `FOO : Dynamic[top]` |

The last row is a **real, measured regression**, pinned by
`crossfile_cache_hit_declines_the_same_file_literal_constant_fold` so it is a
known behaviour and not a surprise. `SourceIndex::literal_constant` is gated on
the assigning file's `LoweredAst::file_id` — a process-global counter stamped at
`lower()` (the persistence hazard documented on `HarvestedConst`). The cached
index was merged against the *worker's* lowering; a hover lowers the buffer
afresh, so the ids differ and the per-file gate declines. It costs precision, never
soundness — the fold is a refinement, so declining it widens to `Dynamic` rather
than answering wrongly — and it is confined to a constant read in the *same file*
as its literal assignment (a cross-file one never folded: the gate is per-file by
design, precisely because the reference's constant-value table is). Closing it
would mean making `file_id` settable in `rigor-parse`, which is outside this
slice and a change to a soundness-load-bearing identity.

## Tests — all six families, each proven non-vacuous

11 new tests in `crates/rigor-cli/src/lsp.rs`. Every one was proven non-vacuous
by re-breaking the implementation once and confirming that exactly the tests
claiming that behaviour fail:

| # | family | test(s) | re-break applied | failed |
|---|---|---|---|---|
| 1 | cross-file hover | `crossfile_hover_answers_from_the_cached_project_index` | hover never handed the cached index | 2 (this + same-URI) |
| 2 | cross-file method completion | `crossfile_completion_answers_from_the_cached_project_index` | completion never handed the cached index | 2 (this + namespace) |
| 3 | `Foo::` completion | `…_offers_project_source_children`, `…_keeps_rbs_results_and_rbs_wins_a_kind_conflict` | union order inverted (project last) | 1 (the kind test) |
| 4 | same-URI guard | `crossfile_cache_is_never_read_across_uris` | reader serves any entry, ignoring the URI | 1 |
| 5 | evict / gate / invalidate | `…_didclose_evicts_the_uris_entry`, `…_is_never_populated_by_a_guard_off_dispatch`, `…_is_cleared_by_the_generation_bump_a_guard_trip_makes`, `…_reader_rejects_a_stale_generation_and_caps_at_eight` | `didClose` evict removed; population gate widened to always cache; `swap_project`'s clear removed | 1 each |
| 6 | diagnostics byte-inertness | `crossfile_cache_leaves_the_published_diagnostics_byte_identical` | — (see gates 3-5) | — |

Notes on the fixtures, since two of them were nearly wrong:

* **"Before any dispatch" is a held worker, not a race.** Families 1-3 hold the
  v1 worker in the gate, ask the question (getting the genuine no-entry answer),
  release, receive the publish, and ask again. Family 4 opens B at version 7 so
  the gate holds only B's worker while A's entry exists — and asserts the same
  question in A *does* resolve, so the guard assertion is a guard and not a
  broken fixture.
* **The cache is only written by a dispatch that PARSES.** A buffer Prism cannot
  parse returns early with no index, so the namespace fixtures spell the trigger
  as an uppercase *partial* (`Wrapper::Inner` with the cursor at the end) rather
  than a bare `Wrapper::`, which does not parse. This is also why an unparseable
  mid-keystroke buffer does not *evict*: the `if let Some(index)` gate skips the
  store, and the last-good entry survives — which is the design, stated.
* Family 6 pins three things: parity with `check`'s project-wide answer for the
  file, the **exact serialized** `publishDiagnostics` payload (so a change to the
  bytes, not just the rule set, fails), and that interleaving the new cache
  *readers* between two dispatches does not move the second payload.

## Gate verdicts (BARE, in the spec's order)

| # | gate | verdict |
|---|---|---|
| 1a | `cargo test --workspace` | **PASS** — 389 / 4 / 3 / 9 / 94 / 277 / 47 / 251 / 48, 0 failed (rigor-cli 378 → 389: +11 new; every other suite unchanged) |
| 1b | `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy --workspace --all-targets -- -D warnings` | **PASS** — clean, exit 0 |
| 2a | `ruby harness/run_snapshot.rb` | **PASS** — 98 fixtures, 407 matched / 443 reference, 35 gaps, 2 registered divergences, **0 unregistered FPs** (the pre-change numbers exactly) |
| 2b | release rebuild + `python3 harness/fp_audit.py --gaps --sweep` | **PASS** — **0 FP across 9 204 files** (8 corpora), and the **gap set is byte-identical** to a baseline swept at the branch point with the master binary: 62 body lines, every corpus's reference/rigor-rs/matched/FP/gap counts and every per-rule total unchanged (the only diff is the header naming the binary and its build time) |
| 2c | `rigor check` byte-identical vs the master baseline on `mastodon/app` | **PASS** — 421 lines, stdout + stderr + exit code identical under default threads AND `RAYON_NUM_THREADS=1`; the new binary's two thread modes also agree (ADR-0020) |
| 2d | **`publishDiagnostics` byte-identical across binaries** (the empirical form of test family 6) | **PASS** — a scripted stdio session (`didOpen` → 20× hover + 20× completion → `didChange` → `didClose`) driven against the master-baseline binary and the branch binary produced **byte-identical JSON** for every publish, at `mastodon/app` (1 236 files, 4 diagnostics) and `gitlab-foss/lib` (4 675 files, 97 diagnostics) |
| — | `python3 harness/docs_check.py` | **PASS** |

**On the ordering of 2b.** The sweep measured the release binary built from the
tree as committed. The latency/memory harness below is a `#[test] #[ignore]`
that was added *after* the sweep and **fully reverted** before the commit (the
probe note's own pattern); `git diff --stat` returns to the reviewed 966/72 and
the suite, a release rebuild and the 2c byte-identity check were all re-run on
the reverted tree.

## Latency and memory

**End-to-end, production posture** (the stdio session of gate 2d — full
round-trip: client request → loop thread → reply), branch binary:

| corpus | files | overlay | hover median / p95 | completion median / p95 |
|---|---:|---|---|---|
| `mastodon/app` | 1 236 | **ON** ⇒ cache hits | 0.31 / 0.48 ms | 0.30 / 0.40 ms |
| `gitlab-foss/lib` | 4 675 | OFF ⇒ no cache, single-file as today | 0.33 / 0.47 ms | 0.45 / 0.59 ms |

Master, same session, same file, same position: 0.39 / 0.59 ms and 0.36 / 0.54 ms
hover. So the readers are **at worst neutral and slightly faster**, and the
p95 budget (ADR-0029, < 100 ms) holds with ~200× headroom.

**Cache-hit vs cache-miss in isolation at 4 675 files.** The production posture
cannot show a hit at this scale — the scale guard is already off there, so the
cache is never populated (see "Not done" below). With the overlay forced on, the
same `hover` / `completion` functions, 25 iterations, release build, two runs:

| | hit median / p95 | miss median / p95 |
|---|---|---|
| hover | 0.16-0.23 / 0.27-0.38 ms | 0.19-0.35 / 0.25-0.56 ms |
| completion | 0.19-0.38 / 0.30-0.54 ms | 0.23-0.38 / 0.31-0.55 ms |

The hit is consistently at or below the miss (it skips the single-file
`SourceIndex::build` entirely, via the deferred init). **The number that
justifies the design is the third one:** the same dispatch's `SourceIndex::merge`
— what a *per-request rebuild* would have cost — measured **162-230 ms**, i.e.
1.6-2.3× the entire p95 budget for one keystroke's worth of hover. That is why
the policy decision was "answer from the cache, never rebuild".

**Memory at the LRU cap of 8 entries** (`ps -o rss=` at checkpoints inside one
release process, the probe's method):

| corpus | files | RSS before | RSS at 8 entries | delta | per entry | per file per entry |
|---|---:|---:|---:|---:|---:|---:|
| `mastodon/app` | 1 236 | 83.4 MB | 119.4 MB | **35.1 MB** | 4.4 MB | 3.64 KB |
| `gitlab-foss/lib` | 4 675 | 290.7 MB | 428.3 MB | **134.4 MB** | 16.8 MB | 3.68 KB |

Reproduced across two runs at 4 675 files within 0.2% (134.42 / 134.67 MB), and
the per-file-per-entry figure is stable across a 3.8× scale change — slightly
*below* the probe's 4.1-4.7 KB/file estimate, so the sizing verdict holds with
margin. At the scale where the cache actually populates today (≤ 1 236 files
measured here), a full cache costs 35 MB against ADR-0029's 600 MB budget.

## Not done / left open

* **Cross-URI freshness, persistence, guard-policy changes** — non-goals, binding.
* **`documentSymbol`** reads no `SourceIndex`; untouched, as specced.
* **The same-file literal-constant fold on a hit** (above) — a `rigor-parse`
  change, deliberately out of scope.
* **At 4 675 files the cache never populates**, because the overlay guard is
  already off at that scale (measured again here: the dispatch merge costs
  162-230 ms against the guard's budget). Hover/completion there pay exactly
  today's single-file cost — which is also why the cache-hit numbers at that
  scale had to be measured with the overlay forced on. The slice therefore
  *helps* exactly where the overlay is already live (≤ ~3 117 files) and is
  *inert* where it is not. Nothing here changes the guard, which is a non-goal;
  raising the scale at which the overlay stays on is the separate lever, and it
  is the one that would extend this slice's reach.
