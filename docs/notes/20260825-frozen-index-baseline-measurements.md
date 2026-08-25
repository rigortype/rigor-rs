# Frozen-index baseline measurements (issue #93) — 2026-08-25

Measurement only, no production code touched. Answers the numbers issue #93
asks for so the persistence slices in
[the recon note](20260825-rust-glancer-frozen-index-recon.md) can be
justified or retired BEFORE any are built (AGENTS.md "measure before you
build", ADR-0037 precedent).

## Method

- **Machine**: Apple M4 Pro, 48GB RAM, macOS (Darwin 25.5.0). No isolation
  from other load beyond an otherwise-idle session; spread is reported so a
  reader can judge noise.
- **Binary**: fresh release build in this worktree —
  `cargo build --release -p rigor-cli --offline`, 24.6s, finished
  2026-08-25T18:45:29+09:00. Path:
  `/Users/megurine/repo/rust/rigor-rs/.claude/worktrees/agent-a0ffedfa93ab06e1c/target/release/rigor`
  (`rigor 0.0.1`). Verified newer than every file under `crates/` before any
  measurement ran; every command below names this exact path or a `cwd`
  inside which it is `$PATH`-free (invoked by absolute path throughout).
- **Corpora**, resolved from `harness/sweep-corpora.yml` (mastodon/app,
  gitlab-foss/lib) plus the two additional scales the 2026-07-19 LSP notes
  used (conference-app, gitlab-foss/lib/gitlab), file counts as **this
  binary's own discovery** counts them (`RIGOR_TIMING`'s `files=` field),
  not `find`:

  | label | path | files (this run) | files (2026-07-19 notes) |
  |---|---|---|---|
  | conference-app | `/Users/megurine/repo/ruby/conference-app` | **245** | 244 |
  | mastodon/app | `/Users/megurine/repo/ruby/mastodon/app` | **1236** | 1236 |
  | gitlab-foss/lib/gitlab | `/Users/megurine/repo/ruby/gitlab-foss/lib/gitlab` | **3117** | 3117 |
  | gitlab-foss/lib | `/Users/megurine/repo/ruby/gitlab-foss/lib` | **4675** | 4675/4676 |

  conference-app is off by one file from the S4b note (live OSS checkout,
  five weeks later — not investigated further, immaterial to every
  conclusion below). The other three match exactly.
- All `rigor check` runs exit 1 (ERROR-severity diagnostics present, per
  `harness/CORPUS.md`'s documented exit-code contract) — expected, not a
  failure.
- A **performance caveat up front**: every absolute number in this note runs
  25–55% higher than the equivalent number in
  [20260719-lsp-s12-s4b.md](20260719-lsp-s12-s4b.md) (e.g. tier-1
  `build_project` 68.4ms → 91.7ms @ 3117 files; held-AST RSS 189MB → 241MB @
  4675 files). Machine difference and five weeks of feature growth
  (ADR-0043 effects tracking, the rbs 4.1.1 vendor bump, live-corpus content
  drift) are all plausible causes and this note does not attribute it to
  one; what matters for the GO/NO-GO calls below is that **today's numbers
  are internally consistent** (same machine, same binary, across all four
  measurements), and the drift is uniform enough that it does not change
  any of the shapes the original notes found — the OverlayGuard cliff still
  lands exactly at 4675 files, not some other scale.

## 1. CLI phase breakdown + skippable share

`RIGOR_TIMING=1 rigor check <corpus>`, 5 runs per corpus, **median** reported
(spread in the second row). "Skippable" = `index-load + stage1 + stage2`, as
issue #93 defines it — what a warm per-file harvest cache could avoid on
unchanged input.

| corpus | files | index-load | stage1 (parse+lower) | stage2 (build_project) | stage3 (analyze) | **total (internal)** | skippable share |
|---|---:|---:|---:|---:|---:|---:|---:|
| conference-app | 245 | 27.30ms | 4.26ms | 0.88ms | 2.12ms | **33.50ms** | **96.8%** |
| mastodon/app | 1236 | 25.22ms | 16.99ms | 30.35ms | 11.89ms | **84.98ms** | **85.4%** |
| gitlab-foss/lib/gitlab | 3117 | 23.34ms | 48.15ms | 95.52ms | 36.32ms | **216.41ms** | **77.2%** |
| gitlab-foss/lib | 4675 | 25.52ms | 72.99ms | 187.83ms | 54.53ms | **341.18ms** | **83.9%** |

Spread (min–max across 5 runs), all in ms:

| corpus | index-load | stage1 | stage2 | stage3 | total |
|---|---|---|---|---|---|
| conference-app | 25.31–47.28 | 3.37–6.78 | 0.83–2.17 | 1.95–2.15 | 32.01–56.98 |
| mastodon/app | 24.84–25.69 | 16.54–28.93 | 29.51–32.66 | 11.76–14.42 | 82.65–101.71 |
| gitlab-foss/lib/gitlab | 23.19–24.80 | 46.38–85.83 | 92.47–99.76 | 34.03–52.70 | 204.62–235.67 |
| gitlab-foss/lib | 24.87–25.87 | 66.40–79.76 | 186.39–191.04 | 52.76–55.12 | 335.90–346.32 |

`index-load` (`CoreIndex::for_project` — RBS parse/load) is flat across
corpus size, as expected: it depends on the plugin/signature set, not the
file count. The dip in skippable share at 3117/4675 vs conference-app is
because `index-load`'s ~25ms fixed floor is a shrinking fraction of a
growing total — the *absolute* skippable time still grows monotonically
(32ms → 73ms → 167ms → 286ms), unlike a fixed cost.

## 2. Repeated-invocation profile

5 consecutive `rigor check` runs per corpus, unchanged input, **external
wall-clock** (process spawn to exit — a strict superset of the internal
total above, so it also prices in argv/config-parse overhead outside the
timed region and process startup).

| corpus | wall time (median) | wall spread | internal total (median) | fraction unaccounted (spawn/config/etc.) |
|---|---:|---:|---:|---:|
| conference-app | 110.4ms | 102.1–269.7ms | 33.5ms | ~70% |
| mastodon/app | 188.8ms | 183.7–236.1ms | 85.0ms | ~55% |
| gitlab-foss/lib/gitlab | 330.6ms | 319.6–363.2ms | 216.4ms | ~35% |
| gitlab-foss/lib | 476.1ms | 473.1–510.2ms | 341.2ms | ~28% |

Bare process startup (`rigor --version`, no CoreIndex/analysis) is
**~9–10ms** warm — nowhere near enough to explain the unaccounted fraction
at the small-corpus end, so most of that gap is argv/config parsing and
disk I/O outside `analyze_files`'s timed region, not process-launch
overhead. Not decomposed further here (would need a second timing seam in
`cmd_check`, which is a production-code change, out of scope for a
measurement-only pass) — reported honestly as an unattributed remainder
rather than folded silently into "index-load."

**Every one of these 5 runs recomputes 100% of `index-load + stage1 +
stage2` from scratch** — there is no caching today, so "fraction spent
recomputing unchanged inputs" is exactly the skippable share from §1
(77–97%), repeated identically on every one of the 5 invocations (spread
above is run-to-run noise, not a trend — no run benefits from a
predecessor).

## 3. LSP cold start + structural-invalidation re-verification

Protocol: JSON-RPC over stdio (same framing as
`crates/rigor-cli/tests/lsp_check_parity.rs`'s `LspChild`), `RIGOR_NO_RUBY=1`
(hermetic — no sidecar-spawn variance), `RIGOR_TIMING=1`. A temporary
`.rigor.yml` (`paths: ["."]`) is written into each corpus root for the
run's duration only (removed in a `finally`, verified absent before write)
so the LSP's project root — the corpus directory itself, since `cwd` is set
there — discovers the whole corpus tree, matching what the CLI's positional
argument discovers. **Cold start** = wall-clock from process spawn to the
first `textDocument/publishDiagnostics` for one `didOpen`'d file (3 runs,
median reported). 3 runs per corpus (not 5, since it's a much heavier signal
per run — full process lifecycle each time — and the picture was already
consistent by run 2).

| corpus | files | cold start (median) | spread | handshake (`initialize` round trip) |
|---|---:|---:|---:|---:|
| conference-app | 245 | 102.8ms | 100.1–153.9ms | ~9ms |
| mastodon/app | 1236 | 140.9ms | 134.6–164.2ms | ~9–14ms |
| gitlab-foss/lib/gitlab | 3117 | 316.6ms | 312.1–358.5ms | ~8–11ms |
| gitlab-foss/lib | 4675 | 555.4ms | 540.1–568.2ms | ~8–9ms |

The handshake itself is negligible; cold start is dominated by the
server-side `CoreIndex` build + the S4b overlay build (parse+lower +
`build_project` over every project file, held at startup) plus the first
buffer's dispatch.

### Structural-invalidation cost, re-verified

A `workspace/didChangeWatchedFiles` naming `<root>/.rigor.yml` is the
STRUCTURAL trigger (`watched_file_is_structural`) — full synchronous
`CoreIndex` + overlay rebuild on the loop thread, then a redispatch of the
open buffer. Sent after cold start; timed client-side (send → next
`publishDiagnostics`) AND cross-checked against the server's own
`RIGOR_TIMING` line (`report_overlay_timing`, emitted both at startup and
inside `invalidate`).

| corpus | files | server `build_project` (startup / at invalidate) | guard state after invalidate | client round trip (send → republish) |
|---|---:|---|---|---:|
| conference-app | 245 | 0.9ms / 0.9ms | on | 96.8ms |
| mastodon/app | 1236 | 30.0ms / 29.9ms | on | 129.0ms |
| gitlab-foss/lib/gitlab | 3117 | 92.9ms / 91.7ms | on | 314.4ms |
| gitlab-foss/lib | 4675 | 187.1ms / 189.6ms | **off** | 398.8ms |

**The OverlayGuard cliff reproduces exactly where the S4b note put it: it
survives 3117 files (both samples under the 100ms `build_project` budget)
and disables at 4675 (both the startup sample AND the invalidate sample
exceed it — 2 consecutive strikes, matching the hysteresis design).** This
is a live, reproducible functionality gap TODAY, not a historical one: any
project at gitlab-foss/lib scale gets **zero cross-file diagnostics** in the
editor once the guard trips, for the rest of the session (until a
save-triggered rebuild comes in under budget).

The gap between the server's own `build_project` timing and the client
round trip (e.g. 189.6ms server-measured vs 398.8ms client-observed at 4675
files) is the full end-to-end cost an editor actually feels: it additionally
includes the `CoreIndex` rebuild (~25ms, from §1's `index-load` figure —
not separately timed inside `invalidate`, so this is a bound, not a
measured isolate), the post-invalidate `reanalyze_open_buffers` dispatch,
and IPC/JSON marshalling on both ends. Both numbers are reported because
they answer different questions: the server figure is what a future
harvest/merge keystone would need to beat; the client figure is what a user
actually waits on.

## 4. Size proxy: held-AST RSS vs the ~40KB/file estimate

No `MemorySize`-like facility exists in this codebase today (checked:
`grep -rn "MemorySize\|memory_size" crates/` finds nothing production-side).
Building one is instrumentation, i.e. production code — out of scope for a
measurement-only pass. Method used instead, matching what
[20260719-lsp-s12-s4b.md](20260719-lsp-s12-s4b.md)'s "held-AST RSS Δ / peak
RSS" columns imply: **`/usr/bin/time -l`'s "maximum resident set size"**
(a whole-process high-water mark, macOS `getrusage`), delta'd between two
LSP sessions at the **same project root** (so the same auto-detected
`Gemfile.lock` plugin overlay / `CoreIndex` size applies to both sides) —
one with `paths:` pointed at a nonexistent subdirectory (0 files discovered,
isolates `CoreIndex` + runtime baseline) and one with `paths: ["."]` (the
full corpus, ASTs held). 3 runs each side, median reported.

*(A first attempt used a single fixed baseline root — conference-app — for
all four corpora. Rejected: conference-app's own `Gemfile.lock` auto-detects
several plugin RBS overlays that mastodon/app's and gitlab-foss/lib's
own roots do not have — `effective_plugins` never walks up to a parent
directory's `Gemfile.lock` — so the fixed baseline compared a heavier
`CoreIndex` against lighter ones and produced deltas that did not scale
with file count. Same-root baselines fixed it.)*

| corpus | files | baseline (0 files) | full (all files held) | **held-AST delta** | delta / file |
|---|---:|---:|---:|---:|---:|
| conference-app | 245 | 67.8MB | 73.4MB | **5.6MB** | 23.6KB |
| mastodon/app | 1236 | 20.6MB | 76.8MB | **56.2MB** | 46.6KB |
| gitlab-foss/lib/gitlab | 3117 | 20.4MB | 173.0MB | **152.6MB** | 50.1KB |
| gitlab-foss/lib | 4675 | 21.3MB | 262.6MB | **241.3MB** | 52.9KB |

(conference-app's baseline is genuinely ~47MB heavier than the other three's
— a real, reproducible effect of its `Gemfile.lock` auto-detecting
`activesupport`/`activerecord`/`actionpack`-family RBS overlays, confirmed
stable across 3 repeats each of baseline and full; not measurement noise.)

Per-file rate converges to **~50–53KB/file at gitlab-foss scale** — the same
order of magnitude as, and moderately above, the ~40KB/file `LoweredAst`
estimate issue #93 cites. conference-app's lower per-file rate (23.6KB) is
consistent with smaller average file size in that corpus and a larger
fixed-cost share diluting the per-file signal at low file counts; the
three larger corpora (1236/3117/4675 files) agree with each other within
13%.

**Honesty caveat**: this delta is NOT a clean isolation of `SourceIndex`'s
memory alone. Per the S4b note, the per-dispatch `SourceIndex::build_project`
result is discarded (only the ASTs and the timing are kept) — so the
measured RSS high-water mark is a **fused peak** of (held `LoweredAst`s +
whatever `SourceIndex` momentarily existed during the last `build_project`
call before shutdown), not a term-by-term breakdown. Splitting those two
apart would need a `#[cfg(test)]` probe over the actual struct sizes —
explicitly out of scope here (production code). What this number answers
is exactly the question issue #93 asks — "per-file harvest size vs the
~40KB/file `LoweredAst` it would let the LSP drop" — at the resolution an
RSS delta can honestly provide: order-of-magnitude, not byte-exact.

## GO / NO-GO per slice

### 1. On-disk harvest cache — **CONDITIONAL GO, gated on the #92 keystone**

Unlike ADR-0037's sidecar-cache finding (a **flat** ~0.06s cost regardless
of corpus size 55→548 files — nothing for a cache to scale into), the
skippable cost here **grows monotonically with project size**: 32ms → 73ms
→ 167ms → 286ms of `index-load+stage1+stage2` recomputed, unconditionally,
on every single `rigor check` invocation regardless of how little changed.
At gitlab-foss/lib scale that is up to 60% of the 476ms median wall time on
an agent-driven "check after every edit" loop with a near-100%-unchanged
corpus. This is a real, scale-dependent cost the ADR-0037 finding explicitly
was not.

The catch: it is **entirely gated on #92**. `SourceIndex` has no per-file
provenance or merge/extend API today (confirmed still true in this
worktree — `build_project(asts, core)` is one additive multi-pass harvest
over ALL ASTs, `#[derive(Default)]` only, not `Clone`), so a *correct*
per-file cache cannot be expressed without the keystone's harvest/merge
decomposition landing first. Building a disk cache against today's
substrate would mean caching whole-project `build_project` output keyed on
a whole-project content hash — a much coarser, much less useful cache (one
changed file invalidates everything, same as today) that would not earn
the win this section measured. **Recommendation: do not start before #92;
re-evaluate immediately after it lands using these same four corpora as the
before/after comparison.**

For the two SMALL corpora (conference-app, mastodon/app) the case is weak
even post-keystone: wall time there is dominated by the ~9–10ms process
spawn plus a much larger, unattributed argv/config-parse remainder (§2) that
a harvest cache cannot touch at all — the absolute savings (32ms, 73ms) are
real but small next to that floor.

### 2. Per-file diagnostics cache — **NO-GO**

This slice would cache stage-3 (`analyze`) results ON TOP of a harvest
cache, i.e. it is the marginal win *beyond* slice 1. Stage-3 is already
small in absolute terms at every measured scale — 2.1ms / 11.9ms / 36.3ms /
54.5ms (§1) — a maximum of 54.5ms at 4675 files, itself already a small
fraction (16%) of the internal total and a smaller one still of wall time.
This is exactly ADR-0037's shape: "caches an already-cheap cost; the
cross-run saving does not justify a content-addressed disk cache and its
invalidation surface." A second persistent-cache layer, with its own
`--verify-incremental` soundness gate (diagnostics depend on cross-file
facts a naive per-file cache key cannot see — a change to file B can move
file A's diagnostics without touching file A), buys at most ~55ms at the
largest measured scale. **Not worth the invalidation-surface risk
independent of what slice 1 decides.**

### 3. LSP-only path without persistence (in-memory keystone: layered index + AST eviction) — **GO**

This is the strongest case of the three, and does not need an on-disk cache
at all — only #92's in-memory harvest/merge decomposition. Two independent,
reproduced-today findings justify it:

- **The OverlayGuard cliff is a live functionality gap, not a historical
  one.** §3 reproduced it exactly where S4b found it: cross-file
  diagnostics are completely unavailable, for the rest of the session, on
  any project at gitlab-foss/lib scale (4675 files) — a realistic size for
  a mid-size Rails app's `lib/`. A harvest/merge decomposition removes the
  guard's reason to exist (per-dispatch cost becomes O(1 file), not
  O(project)) and un-blocks cross-file hover/completion, which are
  currently withheld at EVERY scale for cost reasons alone.
- **Held ASTs are the dominant term in LSP memory and keep growing.** §4
  measured 241MB held at 4675 files (up from S4b's 189MB) — still under
  ADR-0029's <600MB@5k budget today, but it is the single largest
  contributor to a >260MB process and would shrink to harvest-sized (a
  fraction of the ~50KB/file `LoweredAst` cost, since a harvest is
  extracted facts, not the syntax tree) once ASTs are evicted after
  harvesting.

No disk-persistence risk applies to this slice at all (nothing crosses a
process boundary), so it does not carry the invalidation-surface concern
that makes slice 2 a NO-GO — it is pure in-memory architecture, gated only
on #92's pass-coupling investigation (already scoped as that issue's first
deliverable).

## What was NOT measurable, and why

- **`SourceIndex`'s memory footprint in isolation from the held ASTs.**
  §4's RSS delta is a fused peak of both; splitting them needs a
  `#[cfg(test)]` probe over live struct sizes, which is instrumentation
  (production code), out of scope for this measurement-only task.
- **The `CoreIndex` rebuild cost inside `invalidate()`, isolated from the
  overlay rebuild.** `invalidate()` calls `build_core_index` then
  `build_overlay`, but only the latter is `RIGOR_TIMING`-instrumented in
  `lsp.rs`. §3 bounds it via the CLI's `index-load` figure (~23–27ms, the
  same operation) rather than inventing a number for the LSP's own call
  site.
- **The 40-consecutive-sample guard-hysteresis distribution** S4b built to
  validate the classifier itself (1-sample vs 2-consecutive trip rates)
  was not re-run — out of scope; this note re-verifies WHERE the cliff
  lands (confirmed: exactly 4675 files, both samples over budget), not
  the hysteresis policy's false-trip rate, which is unrelated to the
  persistence-slice decision.
- **A true warm-cache hit-rate measurement** (i.e. build the harvest cache
  and measure it) is definitionally impossible before #92 exists — §1's
  "skippable share" is the pre-keystone ceiling this note can honestly
  offer, not a measured cache.

## Gates

Measurement-only agent; no `crates/` files touched. `git status` at the end
of this session shows exactly one new file (this note). No corpus checkout
was left with a stray `.rigor.yml` — every temporary config written during
§3/§4 was removed in a `finally` block and its absence was a precondition
checked before every write; verified clean after the run.
