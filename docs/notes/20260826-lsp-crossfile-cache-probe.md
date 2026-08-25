# Cross-file hover/completion cache — probe (2026-08-26)

Investigation + measurement only, no production code changed. Answers the
sizing/lifecycle/staleness questions the cross-file hover/completion slice
needs before a "sync handlers answer from a cached last-good project
`SourceIndex`" design (bounded staleness, per the held-harvest slice's own
"Not done" list — [20260825-lsp-held-harvest-impl.md](20260825-lsp-held-harvest-impl.md))
can be sized. Builds on [the S4b mini-spec](20260719-lsp-s4b-overlay-mini-spec.md)
(hover/completion/documentSymbol deliberately carved out of the diagnostics
overlay) and [the frozen-index baseline note](20260825-frozen-index-baseline-measurements.md)
(§4, whose "SourceIndex in isolation from the held ASTs" gap this probe closes
now that #99's harvest/merge split makes the isolation possible).

## Method (§1 — the RSS measurement)

Machine: same worktree, `cargo build --release -p rigor-cli --offline`
(clean, matches HEAD). Corpora resolved from `harness/sweep-corpora.yml` +
the two additional scales the S4b/held-harvest notes used — same three the
task named:

| label | path | files (`find … -name '*.rb'`) |
|---|---|---:|
| mastodon/app | `/Users/megurine/repo/ruby/mastodon/app` | 1236 |
| gitlab-foss/lib/gitlab | `/Users/megurine/repo/ruby/gitlab-foss/lib/gitlab` | 3117 (3118 on disk; one drops to `exclude:`/discovery, matching prior notes' 3117) |
| gitlab-foss/lib | `/Users/megurine/repo/ruby/gitlab-foss/lib` | 4675 (4676 on disk, same one-file gap) |

**Instrument**: a temporary `#[test] #[ignore] fn probe_project_source_index_rss`
added to `crates/rigor-cli/src/lsp.rs`'s test module (reverted before the
final commit — `git diff` against `150f120` is empty for `lsp.rs`; `cargo
test -p rigor-cli` passes at 378/4/3/9 as before). It called the file's own
private `build_core_index` / `build_overlay` / `SourceIndex::merge` directly
(same module, `use super::*`), so it measures the exact production code path,
not a re-implementation.

**Why not `/usr/bin/time -l` (the frozen-index note's §4 method) or
`getrusage`'s `ru_maxrss`**: both report a whole-process *high-water mark* —
useful for a single before/after delta across two process runs, useless for
"before/after/after-drop" checkpoints *within* one run, since a high-water
mark cannot decrease when memory is freed. Instead: `ps -o rss= -p <self
pid>` (current RSS, KB) shelled out at checkpoints inside one process —
release build, one process per corpus (a process that has already
built-and-freed a `SourceIndex` reuses its own free pool for the next one,
which reads as an artificially small delta — exactly the confound the
2026-08-25 held-harvest note's Phase 0 flags for the same reason; see below).

At each corpus: sample RSS at start → after `CoreIndex` build → after
`build_overlay` (tier-1 held `(AST, Harvest)` pairs, matching `ProjectContext`)
→ after building and holding a first merged `SourceIndex` → a second → a
third → then drop them one at a time, sampling after each drop.

**The confound, reproduced and worked around.** `build_overlay` itself calls
`SourceIndex::merge` once internally (to time it for the scale guard) and
drops the result immediately (`crates/rigor-cli/src/lsp.rs:830-836`). So by
the time the probe measures its own first `SourceIndex::merge`, the process
has *already* built-and-freed one — the allocator's free list satisfies much
of the new allocation from pages already resident, and the "first" sample
reads low (16 KB–6.3 MB across the three corpora, no consistent scaling).
Samples 2 and 3 converge tightly on each other at every scale (within 1–11%)
once that one-time reuse is spent, so **the reported per-instance cost below
is the average of samples 2 and 3, not sample 1**.

## 1. SourceIndex memory at scale — the answer

Raw checkpoints (KB, `ps -o rss=`), one corpus per process:

| corpus | files | 00 start | 01 +core index | 02 +held pairs (tier 1) | 03 +1st index | 04 +2nd index | 04b +3rd index |
|---|---:|---:|---:|---:|---:|---:|---:|
| mastodon/app | 1236 | 2768 | 19728 | 82096 | 82112 | 88448 | 93600 |
| gitlab-foss/lib/gitlab | 3117 | 2752 | 21088 | 193184 | 195488 | 209472 | 222064 |
| gitlab-foss/lib | 4675 | 2752 | 21008 | 283216 | 289504 | 308896 | 328160 |

Derived deltas:

| corpus | files | `CoreIndex` | held `(AST, Harvest)` pairs (tier 1) | held pairs/file | **ONE `SourceIndex`** (avg of 2nd/3rd) | SourceIndex/file |
|---|---:|---:|---:|---:|---:|---:|
| mastodon/app | 1236 | 16.6 MB | 60.9 MB | 50.5 KB | **5.6 MB** | 4.65 KB |
| gitlab-foss/lib/gitlab | 3117 | 17.9 MB | 168.1 MB | 55.2 KB | **13.0 MB** | 4.26 KB |
| gitlab-foss/lib | 4675 | 17.8 MB | 256.1 MB | 56.1 KB | **18.9 MB** | 4.13 KB |

**Drop behavior** (four checkpoints per corpus: drop the 3rd instance, the
2nd, the 1st, then the held pairs): across all three corpora, at most ONE of
the four drop steps showed a measurable RSS decrease, and it was not the same
step in every corpus — 3117 files reclaimed ~5.3 MB on the "drop the 2nd
instance" step, 4675 files reclaimed ~5.3 MB on the "drop the 1st instance"
step, 1236 files reclaimed nothing measurable at any step. Every OTHER step,
across all three corpora, showed 0 KB change (or ±32 KB noise). This is
consistent with macOS `malloc`'s arena-level reclaim being chunky and
timing-dependent (a `madvise`-back-to-the-OS decision made per arena, not per
`free`), not a per-instance guarantee — freeing a `SourceIndex` does not
reliably shrink RSS on any predictable schedule, so a design that expects
"drop the cache entry, get the memory back immediately" is measured to be
false on this platform; count-bounding what gets held (§5) is the honest
mitigation, not an assumption that eviction is instantly visible in RSS.

**Cross-check against the frozen-index note's independent method** (§4,
`/usr/bin/time -l`, cross-process delta, five weeks earlier on a different
machine): that note measured 52.9 KB/file held-AST-fused-with-a-transient-
SourceIndex-peak at 4675 files. This probe's held-pairs-only figure (56.1
KB/file, post-#99 so no fused SourceIndex peak) is within 6% of it — two
independent methods agree, which is the main confidence check this probe
can offer given the machine/session isn't otherwise controlled.

**Headline finding: one project-wide merged `SourceIndex` costs ~4.1–4.7
KB/file, converging toward ~4.1 KB/file at scale — the same order of
magnitude as ONE file's `Harvest` (~4 KB/file, 2026-08-25 note), NOT the
~44–56 KB/file `LoweredAst` cost.** This makes architectural sense:
`SourceIndex::merge` consolidates N harvests' facts into shared registries
(class/method/constant tables); it does not retain a copy of anything
AST-shaped. Holding *several* merged `SourceIndex`es — e.g. one per open
editor buffer — costs a small multiple of one file's harvest, not a small
multiple of the held-AST baseline that dominates memory today.

**Drops did not shrink RSS** (0 KB reclaimed at the multi-instance steps, a
small reclaim only at the final single-instance-to-zero step at two of three
scales) — expected macOS `malloc` behavior (freed small/medium allocations
are kept in the process's own free lists, not returned to the OS), not a
leak; consistent with why the "after drop" checkpoints exist at all (to make
this an observed fact, not an assumption).

## 2. Dispatch-index lifecycle — where it dies today, and what Arc'ing needs

**Today the merged `SourceIndex` never leaves the rayon worker.** Exact path:

- `spawn_worker` (`crates/rigor-cli/src/lsp.rs:2106-2131`) runs
  `compute_diagnostics(&project, &paths, &text)` **inside `rayon::spawn`**
  (worker thread, line 2124).
- `compute_diagnostics` (`:2389-2480`) calls `overlay_source_index` at
  `:2425-2426`, binding `let (source, overlay_build) = …`.
- `source: SourceIndex` is read by `analyze_with_source_and_folder` (`:2433`),
  `shadowed_rescue_diagnostics` (`:2434-2436`), and conditionally
  `void_value_use_diagnostics` (`:2441-2446`) — all inside the
  `panic::catch_unwind(AssertUnwindSafe(|| { … }))` closure spanning
  `:2411-2449`.
- The closure returns `Some((diags, comments, overlay_build))` at `:2448` —
  **`source` is not in the tuple**, so it drops at the closure's closing
  brace, `crates/rigor-cli/src/lsp.rs:2449` — still on the worker thread,
  before the result ever reaches the channel.
- Only `diags: Vec<Diagnostic>` and `overlay_build: Option<Duration>` (a
  timing sample, not the index) cross back, packed into `Computed`
  (`:2129`) and sent over `tx: crossbeam_channel::Sender<Computed>` to the
  loop thread's `handle_result` (`:2141-2198`).

**What Arc'ing it would take.** Three signature changes, all mechanical:

1. `overlay_source_index` (`:2227-2262`) wraps its `SourceIndex::merge`
   result in `Arc::new(..)` before returning it (or returns `Arc<SourceIndex>`
   outright), so the value survives past the local scope.
2. `compute_diagnostics`'s return type gains the `Arc<SourceIndex>` (or
   `Option<Arc<SourceIndex>>`, `None` exactly when `overlay_build` is `None`
   — see §4) alongside `(Vec<Diagnostic>, Option<Duration>)`.
3. `Computed` (`:1234-1251`) gains one field, e.g.
   `source_index: Option<Arc<SourceIndex>>`, populated at the `tx.send(..)`
   call site (`:2129`).

**The metadata this needs is already there — nothing new to plumb.**
`Computed` already carries `uri: Uri`, `version: i32`, `generation: u64`, and
`epoch: u64` (`:1235-1250`), stamped at dispatch time
(`spawn_worker:2109-2110`) and re-checked against current session state in
`handle_result`'s three-axis `live` computation (`:2153-2155`). A per-URI
cache write is a one-line addition **inside the existing `live` branch**
(`:2156-2158`, where `send_diagnostics` already runs): stash
`(computed.generation, computed.version, computed.epoch,
computed.source_index)` keyed by `computed.uri` into a new `Session` field.
Because the cache would be written at exactly the point the diagnostics it
was computed alongside are published, **a cached entry is never staler than
what the editor is currently showing as diagnostics for that file** — the
cache rides the existing S3/S4 staleness machinery for free, rather than
needing its own.

**`SourceIndex: Send + Sync` is not in question**: `crates/rigor-infer/src/source_index.rs`
has no `Mutex`/`RefCell`/`Cell` field (checked directly), and `ProjectContext`
— which already holds one `Arc`'d across every rayon worker — documents the
same `Send + Sync` requirement as inherited from `CoreIndex`/`SidecarFolder`
(`:288-291`). Sending `Arc<SourceIndex>` over the existing channel adds no
new concurrency reasoning.

## 3. Sync-handler call sites

| handler | site | builds a `SourceIndex`? | reads | cross-file gap today |
|---|---|---|---|---|
| `hover` | `crates/rigor-cli/src/lsp.rs:2510-2590`, `SourceIndex::build` at `:2522` | single-file, fresh per request | `Typer::with_source` → `type_of`, `class_name_of`, `method_arity` (class/method facts by name) | a call/constant whose CLASS is defined in another file resolves to `Dynamic`/unresolved — hover degrades exactly where cross-file diagnostics improve |
| `completion` (method position, `x.`) | `:2631-2695`, `SourceIndex::build` at `:2674` | single-file, fresh per request | `Typer::type_of` on the receiver → `method_names_for` (`class_name_of`, `instance_method_names`/`singleton_method_names`) | same gap: a receiver whose class lives in another file yields an empty (null) completion list |
| `namespace_completion` (constant position, `Foo::`) | `:2710-2755` | **does not build a `SourceIndex` at all** | `project.index.namespace_children(&parent)` — the RBS-only qualified registry on `CoreIndex` | **a different, wider gap**: sees core/stdlib/plugin/project-`sig/` constants only, never a class defined in ANY project `.rb` file (buffer or otherwise) — this is the mini-spec's and the LSP-v4 note's explicit carve-out ("buffer-local constants after `::` … a cost decision, not a correctness one"). A cached project `SourceIndex` would NOT close this gap by itself: `SourceIndex` has no `namespace_children`-equivalent walk over its own registered classes (confirmed — `rigor-infer/src/source_index.rs`'s public API has `knows_class`/`class_id`/`class_name_for_id`/`discovered_superclass` but no children-of-namespace enumerator). Closing it is a second, separate piece of work. |
| `document_symbols` | `:2830-2844` | **does not build a `SourceIndex` at all** | pure AST outline (`crate::outline::build(&ast)`) | none — no type/cross-file facts are read, so this handler is out of scope for a `SourceIndex` cache regardless of design |

So the cache only helps two of the four sync handlers directly
(`hover`, method-position `completion`); `namespace_completion` needs its own
follow-up (SourceIndex does carry project-registered classes — `knows_class`
et al. — confirming the DATA a namespace walk would need already exists in
the merged index; only the enumeration API over it is missing) and
`document_symbols` needs nothing at all.

## 4. Staleness window

**Bound, from the code.** A `didChange` → cache-visible-update path:

1. `didChange` updates the buffer **synchronously** and schedules a debounce
   deadline `now + ctx.debounce` (`DEBOUNCE_DEFAULT` = 200 ms,
   `crates/rigor-cli/src/lsp.rs:276`, scheduled at `:1939`). A burst of edits
   coalesces to the LAST deadline (`Debouncer::schedule`, `:1196-1201`) — so
   worst case is one 200 ms wait from the *last* keystroke in a burst, not
   per keystroke.
2. `fire_due` (`:1802-1806`) dispatches once the deadline passes;
   `spawn_worker` runs `compute_diagnostics` off-thread, which pays
   `overlay_source_index`'s harvest-one-file-plus-merge-all cost — measured
   post-#99 (2026-08-25 note) at 19.4 ms / 48.7 ms / 69.8 ms medians at
   1236/3117/4675 files (guard ON at the first two; OFF at 4675 today) —
   plus stage-3 analyze (~12–55 ms per the frozen-index note's §1).
3. `handle_result` on the loop thread publishes (and, per §2's design, would
   write the cache) as soon as the worker's result arrives and passes the
   3-axis liveness check — no additional loop-thread delay beyond normal
   `select!` scheduling (sub-ms).

**Worst-case bound: ~200 ms (debounce) + ~20–70 ms (overlay merge, scale-
dependent) + ~10–55 ms (analyze) ≈ 230–330 ms** at the two scales where the
overlay is active — inside ADR-0029's own `< 250 ms p50 / < 500 ms p95`
`didChange`→`publish` budget (`docs/adr/0029-lsp-architecture.md:42`),
because it rides the *same* pipeline that budget already governs. A hover on
the file you are *actively* typing in, mid-debounce-window, reads the
PRE-this-keystroke cache entry — the same "accepting bounded staleness" the
task's decision already commits to, now bounded concretely at ~1 debounce +
1 dispatch cycle, never longer while the session is quiescent.

**Invalidating events** (from the code):

- **Generation bump** — every `swap_project` call (`:1561-1584`) increments
  `st.project.generation`, on every `invalidate` (config/watched-file
  structural change, `:1430-1446`) and every `reharvest_sources` swap
  (`:1633-1713`, including the guard-off/empty-project early return at
  `:1639`). A cache entry keyed with the generation it was built under
  (already available on `Computed`, §2) is stale the moment the current
  `st.project.generation` moves past it — the same test `handle_result`
  already applies to a pending worker result.
- **`didClose` epoch bump** — `bump_epoch` (`:1785-1789`) runs on both
  `didOpen` and `didClose`; a cache entry's `epoch` field (also already on
  `Computed`) goes stale on close, matching the existing close+reopen
  version-reuse guard. `didClose` is also the natural point to just DROP the
  URI's cache entry outright (the document cannot receive a hover/completion
  request while closed), which additionally bounds cache size to "currently
  open buffers" — see §5.
- **Guard trip** — `OverlayGuard::record` returning `Disabled`
  (`handle_result:2186-2193`) calls `swap_project(ctx, st, index, None)`,
  which is itself a generation bump (covered by the first bullet) AND sets
  `project.overlay = None`.

**Guard-off fallback stays single-file, confirmed for both diagnostics and
the proposed cache.** `overlay_source_index` (`:2232-2234`) short-circuits to
`SourceIndex::build(ast, &project.index)` (single-file) whenever
`project.overlay` is `None`, returning `overlay_build: None`. A cache design
that only stores an entry when `overlay_build.is_some()` (the same signal
`handle_result` already uses to decide whether to feed the guard, `:2182`)
never populates a cache entry from a guard-off dispatch — so hover/completion
naturally fall through to computing `SourceIndex::build` fresh per request,
identically to today, with no special-case code needed.

## 5. Sizing verdict

**Budget** (ADR-0029, `docs/adr/0029-lsp-architecture.md:44`): < 600 MB
steady state. **Already paid, unavoidable, independent of this decision**
(tier 1: `CoreIndex` + held `(AST, Harvest)` pairs — needed for the
diagnostics overlay regardless of what hover/completion do):

| corpus | files | baseline RSS (core index + held pairs) | headroom to 600 MB |
|---|---:|---:|---:|
| mastodon/app | 1236 | ~80 MB | ~520 MB |
| gitlab-foss/lib/gitlab | 3117 | ~189 MB | ~411 MB |
| gitlab-foss/lib | 4675 | ~277 MB | ~323 MB (but overlay is OFF here today — see below) |

**Per-open-URI cache entry cost** (§1's steady-state figure): ~5.6 MB /
~13.0 MB / ~18.9 MB at the three scales. Max cacheable URIs before exhausting
headroom:

- 1236 files: 520 / 5.6 ≈ **92 entries**
- 3117 files: 411 / 13.0 ≈ **31 entries**
- 4675 files: 323 / 18.9 ≈ 17 entries — **but moot**: the overlay guard is
  already OFF at this scale (measured both in the 2026-08-25 held-harvest
  note and independently reproduced in the frozen-index baseline note), so
  by §4's rule no cache entry is ever written there — hover/completion pay
  only the existing cheap single-file `SourceIndex::build` (a few KB, the
  same order as one `Harvest`), exactly as today, at exactly the scale where
  a naive per-URI cache would otherwise be most expensive.

**Recommendation: per-open-URI cache, not a single most-recent-dispatch
slot**, for three independent reasons:

1. **Memory does not force the choice.** At every scale where the cache
   would actually activate (guard ON, ≤3117 files in this measurement), the
   budget supports 30–90+ concurrently cached entries — an order of
   magnitude above realistic open-tab counts (typically 5–20, rarely 30–50).
   There is no memory argument for restricting to one slot.
2. **A single shared slot has a real correctness cost §3 flags, that per-URI
   avoids for free.** The task's own framing asks "whether answering from a
   project index built with ANOTHER buffer's overlay swapped in is
   semantically acceptable" — it is not, in general: `overlay_source_index`
   REPLACES (never adds) the dirty buffer's entry (`:2244-2259`) specifically
   because a stale-vs-fresh double-registration is a false-positive risk this
   project never trades for speed. A single slot holding buffer A's spliced
   index and then asked to answer for buffer B would either serve A's stale
   splice for B's own file (wrong facts about the file being hovered) or need
   a same-URI guard that silently falls back to single-file for every URI
   except whichever was dispatched most recently — a real functional
   downgrade with no memory saving behind it, since per-URI entries are cheap
   (§1).
3. **The structure self-bounds without extra machinery.** Cache entries only
   exist for currently open buffers (§4: drop on `didClose`, using the epoch
   hook that already fires there) and only when the overlay was actually used
   (§4: `overlay_build.is_some()`) — so total cache size is naturally
   `(open buffers using the overlay) × (~4–5 KB/file in the project)`, never
   unbounded.

**Hybrid note, as a hygiene backstop, not a load-bearing memory control**: a
simple count-based LRU cap (a few dozen entries) is cheap insurance against a
pathological session (hundreds of buffers opened via a bulk "open all files"
action in a huge monorepo) without changing the arithmetic above for any
realistic session — recommended as an implementation detail of the per-URI
design, not as a reason to prefer a single slot.

## What this probe did not settle

- **`namespace_completion`'s separate gap** (§3) — needs a
  namespace-children-style enumerator over `SourceIndex`'s own registered
  classes, which does not exist today; out of scope here (LSP-v4 note already
  flagged it as its own cost decision).
- **The exact `Session` field shape and eviction policy** for the cache —
  this probe sizes and confirms feasibility; the implementation slice picks
  the concrete `HashMap`/LRU structure.
- **A live end-to-end prototype.** Every number here comes from driving the
  production `build_core_index` / `build_overlay` / `SourceIndex::merge`
  functions directly against real corpora, not from a mocked or synthetic
  harness — but no hover/completion code path was touched.

## Gates

`git diff 150f120 -- crates/` is empty (the temporary probe test was added
and fully reverted). `cargo test -p rigor-cli --offline`: 378 + 4 + 3 + 9,
0 failed — unchanged from `150f120`. No `reference/rigor` /
`REFERENCE_RIGOR_DIR` touched (port-side only, as instructed). No PR opened;
pushed to `claude/lsp-crossfile-probe` via explicit refspec.
