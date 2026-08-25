# Harvest-cache GO/NO-GO re-measure (2026-08-26)

Re-measures the CLI phase costs
[the 2026-08-25 baseline note](20260825-frozen-index-baseline-measurements.md)
took, on a machine its own audit section flagged as contaminated by a sibling
agent's parallel `cargo build`. Since that note, three things landed on
master and change *what question the numbers answer*:

- **#92** ([impl](20260825-s92-harvest-merge-impl.md)) — `build_project` is now
  `merge(asts.map(|a| harvest(a, core)), core)`. Stage 1 is
  `parse+lower+harvest` (file-parallel, rayon); stage 2 is `merge`-only (still
  a serial barrier, still touches every AST for the two merge-resident passes
  — issue #92 §5 — but it is no longer where the per-file walking happens).
- **#94** ([impl](20260825-s94-ancestor-closure-impl.md)) — the merge's
  overridable-method degrade gate is O(candidates) instead of O(pairs);
  ~29% off stage 2 at both measured scales in the #92 note.
- **#103** ([impl](20260826-s102-file-identity-impl.md)) — `HarvestedConst`
  carries a path-derived `FileKey` instead of a process-global counter,
  unblocking (not building) a harvest that survives a process boundary.

So the question is no longer "how much does a warm cache save on the OLD
`index-load + stage1 + stage2` monolith" (77–97%, per the 08-25 note) — it is
**which of two different-shaped caches is worth building against the NEW
stage1(parse+lower+harvest) / stage2(merge) split**, per
[the recon note](20260825-rust-glancer-frozen-index-recon.md)'s slice 3 and
this note's own task framing:

- **Design A — per-file harvest cache.** blake3(file bytes) → cached
  `Harvest`. A warm run still pays `index-load + merge + analyze`; it skips
  only stage 1 for files whose content hash is unchanged.
- **Design B — harvest-hash-keyed merged-index cache** (the recon note's
  "firewall" property, generalized to the WHOLE project). If every file's
  content hash is unchanged, reuse the persisted, already-merged
  `SourceIndex` outright — skip stage 1 *and* stage 2, pay only
  `index-load + analyze`. The catch, stated as the task asked: stage 1 is
  what would normally compute those hashes as a side effect of reading each
  file, so a warm run that *skips* stage 1 must pay a **dedicated read+hash
  pass over every file** to even ask "did anything change" — B's true warm
  path is `index-load + hash-all-files + analyze`, not `index-load + analyze`
  alone.

## Method

**Machine**: Apple M4 Pro, 12 cores, 48GB RAM, macOS 26.5.2 (Darwin 25.5.0).
**This machine was not idle at the start of this session** despite the task's
premise — `uptime` showed load averages climbing from 18.8 to a peak of
**165.27** (1-min) partway through, traced to a `gh pr checks` polling loop
and an unrelated `rustdoc` test-compile for a different repository
(`src-srpg-rs`) both running concurrently under the same user account; a
first measurement pass taken during that spike was discarded outright (its
`conference-app` internal total came out 51–133ms against this note's clean
31ms, and every corpus's numbers were inflated by a similar factor — the
contamination was not subtle). By the time the numbers below were taken,
`uptime` read `load averages: 11.30 35.39 31.23`, then `10.49 34.02 30.82`
immediately after — the 1-minute figure had settled; the 5/15-minute figures
carry the earlier spike's tail and are not evidence of ongoing contamination
at measurement time. Every `os.getloadavg()` sample recorded *during* the run
itself (one per round, at the start of each of the 5 interleaved rounds) read
**10.10–10.49**, flat across the whole run — reported in the results file,
not just at the edges.

Given the demonstrated risk of a contiguous load burst landing on one
corpus's whole sample (which is exactly what invalidated the first attempt),
the 5 repetitions per corpus were **interleaved** — round *r* touches all
four corpora once, in the same order, before round *r*+1 starts — rather
than run as 5 back-to-back invocations of one corpus before moving to the
next. This is a deliberate deviation from the literal "5 consecutive runs"
phrasing: it is the same technique
[the #92 impl note](20260825-s92-harvest-merge-impl.md) used for its
before/after timing ("a straight before-then-after block put all the thermal
drift on one side"), applied here to guard against exactly the failure mode
that discarded this note's first attempt. Internal (`RIGOR_TIMING`) and
external (process spawn → exit) timing were both taken from the *same* 5
invocations per corpus rather than two separate sets of 5 — the timing
markers are unconditional `Instant::now()` calls costing nanoseconds
(`crates/rigor-cli/src/main.rs:657`), so formatting one `eprintln!` line does
not measurably perturb wall time, and reusing the runs halves the total
measurement window's exposure to background noise.

**Binary**: fresh release build, `cargo build --release --offline -p
rigor-cli`, 19.83s, finished 2026-08-26T01:31:45+0900, in a **freshly created
worktree** (`.claude/worktrees/harvest-cache-remeasure`, branch
`claude/harvest-cache-remeasure`, cut from `master` @ `68ba21e` — the same
commit this note's context describes). Path:
`/Users/megurine/repo/rust/rigor-rs/.claude/worktrees/harvest-cache-remeasure/target/release/rigor`
(`rigor 0.0.1`, 10,280,224 bytes — byte-identical size to an interrupted
earlier build in a worktree that was removed out from under this session
mid-task; the removal cost a rebuild, not any measured data, since nothing
had been measured yet at that point). Verified newer than every file under
`crates/` before any measurement ran.

**`RIGOR_TIMING=1 rigor check <corpus>`**, run with `cwd` set to the
worktree root (a Rust project with no `Gemfile.lock` of its own) — **not**
the corpus directory. ADR-72's plugin auto-detection reads `Gemfile.lock`
from `cwd` only (it does not walk up to a parent directory's, per the 08-25
note's own finding), so a corpus-directory `cwd` would let each corpus's own
`Gemfile.lock`-gated plugin overlay leak into `index-load`, breaking both
the "flat across corpus size" property this run reproduces (22.7–23.3ms
across a 19× file-count range) and comparability with the 08-25 numbers.

**Corpora**, resolved from `harness/sweep-corpora.yml` + the two additional
scales the LSP notes use, counted by this binary's own `files=` field:

| label | path | files (CLI) | files (hashprobe) |
|---|---|---:|---:|
| conference-app | `/Users/megurine/repo/ruby/conference-app` | 245 | 245 |
| mastodon/app | `/Users/megurine/repo/ruby/mastodon/app` | 1236 | 1236 |
| gitlab-foss/lib/gitlab | `/Users/megurine/repo/ruby/gitlab-foss/lib/gitlab` | 3117 | 3118 |
| gitlab-foss/lib | `/Users/megurine/repo/ruby/gitlab-foss/lib` | 4675 | 4676 |

The two gitlab corpora are off by one file between the two counting methods
— `hashprobe` is a plain recursive `*.rb` walk (see below), while the CLI's
count is post-exclusion (`.rigor.yml` `exclude:`, ERB-template sniffing,
I/O-error skips). Not investigated further, immaterial to every conclusion
below (same order of magnitude as the 08-25 note's own one-file discrepancy
on conference-app).

**Hash-only pass** (Design B's warm-path floor component): a throwaway Rust
probe, `hashprobe`, built **entirely under `/tmp`** — never inside this
repo's tree, never committed, no production code touched. It recursively
walks a directory for `*.rb` files, then reads and `blake3::hash`es each
one's full bytes in parallel via `rayon::par_iter` (mirroring stage 1's own
parallel-read shape in `main.rs`, so this is an apples-to-apples proxy for
"the hash-only slice of what stage 1 already does over the same files," not
a serial lower bound stage 1's own parallelism would never pay). Real
`blake3` (crate `blake3 1.8.6`, already vendor-cached locally — no network
fetch needed for a fresh `--offline` build), not a Python `hashlib` stand-in,
so the numbers are the actual algorithm the recon note's disk-cache sketch
names. Source, in full, for the record:
`/private/tmp/…/scratchpad/hashprobe/src/main.rs` (session-scoped scratch
path, not part of this repo).

## 1. CLI phase breakdown (median of 5 interleaved runs; spread = min–max)

| corpus | files | index-load | stage1(parse+lower+harvest) | stage2(merge) | stage3(analyze) | sort | **total (internal)** | **wall (external)** |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| conference-app | 245 | 23.280ms | 5.578ms | 0.567ms | 2.069ms | 0.005ms | **31.456ms** | **99.593ms** |
| mastodon/app | 1236 | 23.185ms | 20.538ms | 18.507ms | 12.025ms | 0.006ms | **75.850ms** | **165.531ms** |
| gitlab-foss/lib/gitlab | 3117 | 22.974ms | 68.019ms | 47.312ms | 33.170ms | 0.005ms | **175.633ms** | **285.606ms** |
| gitlab-foss/lib | 4675 | 22.776ms | 93.057ms | 70.537ms | 50.109ms | 0.005ms | **255.187ms** | **372.497ms** |

Spread (min–max across 5 interleaved runs, ms):

| corpus | index-load | stage1 | stage2 | stage3 | total | wall |
|---|---|---|---|---|---|---|
| conference-app | 22.690–26.414 | 3.740–5.860 | 0.522–0.703 | 1.978–4.356 | 29.079–35.031 | 93.779–171.837 |
| mastodon/app | 22.787–23.205 | 19.783–28.246 | 18.058–18.864 | 10.959–13.614 | 71.630–80.957 | 153.159–198.397 |
| gitlab-foss/lib/gitlab | 22.649–23.119 | 57.858–78.655 | 46.284–48.828 | 31.038–46.220 | 160.659–182.723 | 259.892–321.295 |
| gitlab-foss/lib | 22.695–23.213 | 89.720–119.370 | 68.209–76.107 | 46.096–80.271 | 229.668–274.509 | 351.296–419.512 |

`index-load` is flat (22.7–23.3ms) across a 19× file-count range, as
expected and as the neutral-`cwd` methodology above is designed to
reproduce. Every other stage grows with corpus size. Against the 08-25
note's (contaminated) numbers, `total` is **lower at every scale** here
(31.5 vs 33.5ms; 75.9 vs 85.0ms; 175.6 vs 216.4ms; 255.2 vs 341.2ms) — both
a cleaner machine and the real ~13–24% stage1+2 win #92/#94 measured
against their own pre-slice baseline (20260825-s92-harvest-merge-impl.md
§Timing) are plausible contributors; this note does not need to separate
them, since what matters below is today's shape, not a delta against a
differently-contaminated yesterday.

## 2. Hash-only pass (Design B's warm-path floor; median of 5 interleaved runs)

| corpus | files | bytes read | hash-only (median) | spread |
|---|---:|---:|---:|---|
| conference-app | 245 | 219,667 | 2.882ms | 2.473–4.008ms |
| mastodon/app | 1236 | 1,796,450 | 14.150ms | 13.598–14.519ms |
| gitlab-foss/lib/gitlab | 3118 | 7,013,260 | 35.217ms | 32.727–36.219ms |
| gitlab-foss/lib | 4676 | 10,989,742 | 53.382ms | 52.978–54.016ms |

The hash-only pass costs roughly **55–65% of stage 1's own time** at every
scale (5.58→2.88 = 52%; 20.5→14.2 = 69%; 68.0→35.2 = 52%; 93.1→53.4 = 57%) —
cheaper than a full parse+lower+harvest, as expected (no Prism parse, no
AST walk), but not free: it is dominated by the same file-read I/O stage 1
already pays, plus a fast but nonzero hash over multi-megabyte input at the
largest scale (11MB → 53ms ≈ 207MB/s aggregate across 12 threads).

## 3. Design A — per-file harvest cache: ceiling

Warm run = `index-load + stage2(merge) + stage3(analyze)`; **saved = stage1**
(the whole of it, on the all-files-unchanged ceiling case).

| corpus | files | saved (= stage1) | % of wall | % of internal total |
|---|---:|---:|---:|---:|
| conference-app | 245 | 5.578ms | **5.60%** | 17.73% |
| mastodon/app | 1236 | 20.538ms | **12.41%** | 27.08% |
| gitlab-foss/lib/gitlab | 3117 | 68.019ms | **23.82%** | 38.73% |
| gitlab-foss/lib | 4675 | 93.057ms | **24.98%** | 36.47% |

Both the absolute saving (5.6ms → 93.1ms) and its share of wall time (5.6% →
25.0%) grow monotonically with corpus size — the shape ADR-0037 asks for,
and the opposite of that ADR's flat ~0.06s sidecar-spawn finding that had
nothing to scale into.

**This ceiling degrades gracefully to the realistic single-file-edit case.**
Stage 1 is file-parallel with no cross-file dependency for the
skip/no-skip decision — a per-file cache keyed on that file's own content
hash answers independently of what else in the project changed. Editing one
file out of 4675 still skips stage 1 for the other 4674 (>99.9% of stage 1's
measured cost recovered), because nothing about *this* design's cache-hit
test depends on any other file's state. Stage 2 (merge) and stage 3
(analyze) are unconditionally repaid every run in this design's basic form —
they are not part of the ceiling being claimed here, and are not improved by
this cache regardless of how few files changed.

## 4. Design B — harvest-hash-keyed merged-index cache: ceiling

The chain, stated explicitly as the task asked:

1. To know whether the persisted merged `SourceIndex` is still valid, every
   file's current content must be hashed — there is no way to answer "did
   anything change" more cheaply than reading and hashing every file, short
   of a filesystem-level `mtime` heuristic this note was not asked to
   evaluate.
2. If **all** hashes match the stored set, the persisted merged
   `SourceIndex` is reused outright: stage 1 (parse+lower+harvest) **and**
   stage 2 (merge) are both skipped. Warm run = `index-load + hash-all-files
   + stage3(analyze)`.
3. If **any** hash differs, the whole cached index is invalid — this design,
   as specified, has no partial-reuse path — and the run falls back to a
   full `stage1 + stage2` recompute, exactly as cold, with the hash pass
   either wasted outright (a naive implementation that always hashes every
   file, e.g. to also refresh the stored set for next time) or reduced to a
   near-free prefix scan (an implementation that early-exits on the first
   mismatch). Either way, this design saves **nothing** on any run where
   even one file changed — the case Design A handles gracefully is the case
   Design B cannot partially answer at all.

Ceiling (case 2 only — the whole-corpus-unchanged case; **saved = stage1 +
stage2 − hash-only**):

| corpus | files | saved | % of wall | % of internal total |
|---|---:|---:|---:|---:|
| conference-app | 245 | 3.263ms | **3.28%** | 10.37% |
| mastodon/app | 1236 | 24.895ms | **15.04%** | 32.82% |
| gitlab-foss/lib/gitlab | 3117 | 80.114ms | **28.05%** | 45.61% |
| gitlab-foss/lib | 4675 | 110.212ms | **29.59%** | 43.19% |

Design B's ceiling is **larger than Design A's at every measured scale**
(3.3%→29.6% of wall vs A's 5.6%→25.0%) — it also scales monotonically with
corpus size, satisfying ADR-0037's first bar in isolation. The number alone
is not the decision, though: see below.

## GO/NO-GO

### Design A (per-file harvest cache) — **GO**

Both ADR-0037 bar questions clear:

- **Scales with corpus size**: 5.6ms/5.6% of wall at 245 files → 93.1ms/25.0%
  of wall at 4675 files, monotonic at every intermediate scale measured.
  This is the opposite shape from the ADR-0037 sidecar finding (flat ~0.06s
  with nothing to scale into) and matches the growth shape the 08-25 note
  already found in the pre-#92 monolith.
- **Beats the invalidation-surface cost**: the surface is exactly what the
  recon note already scoped for this design — blake3(file bytes) keys, a
  generation fingerprint over `(.rigor.yml, Gemfile.lock, sig/**, binary
  version)`, atomic writes, fail-open, "rejected and rebuilt rather than
  partially salvaged." Each cache entry is one small, self-contained
  `Harvest`, independently valid or invalid — the same shape #103 already
  made every `Harvest`'s only file-identity dependency (a `FileKey`) capable
  of surviving a process boundary. There is no cross-entry coupling for the
  cache layer itself to get wrong.
- **Realistically reachable**: the ceiling is not a best-case-only number —
  a single-file edit in a 4675-file project recovers essentially all of it,
  because the per-file skip decision has no cross-file dependency.

The two small corpora (conference-app, mastodon/app) remain the weakest
case, as the 08-25 note also found: 5.6% and 12.4% of wall respectively,
against a wall time still substantially made of process-spawn and
argv/config-parse overhead a harvest cache cannot touch. The case strengthens
monotonically with project size, which is the direction that matters — a
harvest cache's value proposition is precisely "large projects, small
diffs."

### Design B (harvest-hash-keyed merged-index cache) — **NO-GO**

The ceiling number alone clears ADR-0037's first bar (it scales, and by a
larger margin than Design A's). It fails the second: **the invalidation
surface is strictly worse than Design A's while the realistically reachable
saving is strictly worse too**, and both follow from the same structural
fact — this design's reuse test is a single all-or-nothing gate over the
*entire* project, not per file.

- **Realistic reach is far below the ceiling.** The ceiling in §4 is only
  earned on a byte-for-byte-unchanged re-run of the whole corpus — the
  narrow case of re-running `check` with zero edits (a duplicate CI
  invocation, an editor's idle re-check with no buffer changes). The
  workload this note's own framing repeatedly names as the target — "an
  agent-driven check-after-every-edit loop" — changes exactly one or a
  handful of files per invocation, which is precisely the case where this
  design contributes **zero**: one changed file invalidates the entire
  cached `SourceIndex`, forcing the full `stage1 + stage2` recompute anyway,
  on top of a hash pass that bought nothing. Design A's ceiling, by
  contrast, is reachable almost exactly in that same dominant workload
  (§3) — the two designs' ceilings are not competing for the same usage
  pattern, and the pattern Design B needs is the rarer one.
- **Higher invalidation surface for less realistic benefit.** Design B needs
  everything Design A needs (content-keyed hashing, a generation
  fingerprint, fail-open behaviour) **plus** a serialized whole-project
  `SourceIndex` — the recon note's own honesty caveat that `HashMap`
  iteration order must not leak into a persisted artifact (ADR-0020) applies
  to the WHOLE index here, not to small independent per-file records; a
  corrupt or stale persisted index is a single point of failure for the
  entire project's diagnostics rather than one file's; and the hash-all-files
  pass is paid on **every single invocation, hit or miss** (it is the only
  way to know which one applies), whereas Design A's per-file hash lookups
  are naturally bounded by however many files a caller actually asks about.

Design B is not "wrong" in the narrow case it targets — a true no-op re-run
is real and Design B's ceiling there is large — but that case is rare enough,
and the design coarse enough, that building it as a general-purpose warm
cache is not justified when Design A already exists, already covers the
dominant edit-loop workload near its own ceiling, and costs less to build
and to keep correct. **Revisit only if a real workload demonstrates the
whole-project-unchanged re-run is common** (e.g., a specific CI or editor
integration that re-invokes `check` without an intervening edit) — not
before, per the same "measured, not assumed" standard ADR-0037 sets.

## What this note does not claim

- **No hybrid design was measured.** A per-file harvest cache (Design A)
  *plus* a merge-result memo keyed on the multiset of per-file harvest
  hashes actually used (rather than "all files in the project") could in
  principle recover more of stage 2 than Design A alone without Design B's
  all-or-nothing fragility — that is a different, unmeasured design, out of
  this note's scope (issue #93 asked for exactly these two).
- **No actual cache was built.** Both ceilings are upper bounds computed
  from phase timings and a throwaway hash-only probe, exactly as the 08-25
  note's own ceilings were — a true warm-cache hit-rate measurement needs
  the cache to exist first.
- **The machine was not perfectly idle**, as detailed in Method. The
  numbers above come from the interleaved, load-monitored, second attempt
  after a first attempt was discarded for visible contamination; they are
  reported as the best achievable measurement in this session, not as a
  guarantee of zero residual noise. The GO/NO-GO calls rest on shape
  (monotonic growth, ceiling-vs-realistic-reach) and ratios between the two
  designs computed from the *same* runs, which is the load-robust framing
  the 08-25 note's own audit already recommended.

## Gates

Measurement-only session; no files under `crates/` were touched. The
throwaway `hashprobe` probe was built and run entirely under
`/private/tmp/…/scratchpad/hashprobe/`, outside this repository's working
tree, and is not part of this commit. `git status` at the end of this
session shows exactly the files this commit adds (this note); no corpus
checkout was modified. The original assigned worktree
(`.claude/worktrees/agent-aebee60f887f8ed1d`) was removed by something
outside this session partway through (its `git worktree list` entry was
gone, not merely its directory) while its working tree was clean at the
shared `master` commit this note's context describes — no uncommitted work
existed there to lose; a fresh worktree
(`.claude/worktrees/harvest-cache-remeasure`) was created from `master` to
continue, and every measurement in this note was taken there.
