# S104 — the harvest's own cost, measured. **NO-GO on the per-file harvest cache** (2026-08-26)

Resolves the bound the [re-measure note's CORRECTION section](20260826-harvest-cache-remeasure.md)
left open. That correction retracted Design A's "25 % of wall at 4,675 files"
ceiling — it was computed on the false premise that a warm run skips all of
stage 1, which it cannot (the `LoweredAst` is required by merge M3 and by every
file's stage-3 rule walk; see the [#104 probe](20260826-s104-harvest-cache-probe.md)
§0). A per-file harvest cache skips **exactly one call**,
`SourceIndex::harvest(&ast, &index)` at `crates/rigor-cli/src/main.rs:776`.
The correction predicted the real prize at "~57 ms of CPU but only ~5 ms of
WALL", derived from PR #95's before/after, and suspended the GO pending a
measurement.

**This note takes that measurement and the answer is NO-GO.** The prize is
**4.1–4.7 ms of wall at 4,675 files — 1.1–1.3 % of total wall**, and the
cache's own mandatory warm-path cost exceeds it. The GO is withdrawn, not
merely suspended.

Read alongside: the [probe](20260826-s104-harvest-cache-probe.md) (how to build
it soundly — its structural findings all stand and are not disturbed here),
[ADR-0037](../adr/0037-sidecar-perf-slices-retired-by-measurement.md) (the bar),
[the #92 impl note](20260825-s92-harvest-merge-impl.md) (where the ~57 ms /
~4.7 ms bound came from), [ADR-0020](../adr/0020-normalization-and-determinism.md).

---

## 0. What shipped regardless of the verdict: the stage-1 split

`RIGOR_TIMING`'s `stage1(parse+lower+harvest)` was one number, so the prize was
unobservable. It is now five more fields, emitted between the stage-1 and
stage-2 labels (`crates/rigor-cli/src/main.rs`, `Stage1Times` carries the
method comment):

| field | what it is |
|---|---|
| `stage1(parse+lower+harvest)` | unchanged — stage 1's measured **wall** |
| `stage1.parse+lower-cpu` | **SUM** over files of read + ERB sniff + parse + lower, per-file worker time |
| `stage1.harvest-cpu` | **SUM** over files of `SourceIndex::harvest` alone |
| `stage1.harvest-cpu-max` | the single worst file's harvest — a **floor** on any wall contribution (one file's harvest cannot be split across threads) |
| `stage1.harvest-wall-amortized` | `harvest-cpu / threads` — a **MODEL**, not a measurement |
| `stage1-ex-harvest` | stage-1 wall minus that model |

The honesty rule the code comment states: stage 1 runs on rayon, so a per-file
duration is CPU on one worker, **not** wall. Summing 4,675 of them yields a
number several times larger than the stage's own wall clock, and quoting that
sum as "what the cache saves" would be wrong by exactly the parallel speedup.
Hence three separately-labelled quantities plus the wall, and — because the
model is still a model — the A/B differential in §2 below, which measures the
marginal wall directly.

**Cost when `RIGOR_TIMING` is unset: zero clock reads.** The per-file timers are
`timing.then(Instant::now)`, so the default path takes one already-hot predicted
branch per file and never reads the clock. Verified by the ADR-0020 gate: `check`
output is byte-identical (stdout **and** stderr, exit code) against a
master-built binary on mastodon/app with the variable unset.

`docs/PORT_BACKLOG.md`'s `RIGOR_TIMING` entry (which enumerates the label
format) is updated in the same commit.

---

## Method

**Machine**: Apple M4 Pro, 12 cores, 48 GB, macOS 26.5.2 (Darwin 25.5.0).

**Load honesty.** The task's premise was an idle machine; it was not idle. One
synchronous `uptime` before the run read `load averages: 9.48 8.84 8.29`, and
`os.getloadavg()` sampled at the start of each of the 9 rounds read 15.6, 14.9,
14.1, 14.5, 14.4, 13.6, 12.7, 11.7, 11.8 (1-minute). A `ps -Ao %cpu -r` snapshot
found **no compute jobs** — no `cargo`, `rustc`, `ruby` or sibling agent — only
desktop applications (WindowServer, Chrome, Discord, coreaudiod). So the load is
a steady GUI-app floor of roughly 8–9 plus this measurement's own 12-thread
pool, not a contending build. It is still enough to distort a single 5 ms
signal, and the method below is designed around that rather than pretending
otherwise:

1. **Interleaved rounds** — round *r* touches every (corpus × cell) once before
   round *r*+1 starts, so a load burst cannot land on one cell's whole sample.
   Same technique as the #92 impl note and the 08-26 re-measure.
2. **A serial control** (`RAYON_NUM_THREADS=1`) alongside every parallel cell.
   With one worker on a 12-core box the per-file timers are nearly uncontended,
   which makes the serial `harvest-cpu` the trustworthy **CPU** figure and the
   parallel one visibly inflated (§3).
3. **Amplified A/B for the marginal wall.** A throwaway probe binary repeats the
   harvest *K = 8* extra times per file; `stage1_wall(K=8) − stage1_wall(K=0)`,
   divided by 8, is one harvest's marginal wall **under the real rayon
   schedule**, with the signal lifted 8× above the noise floor. Both sides come
   from the **same binary** (mode selected by env), so codegen is identical.
4. **min as well as median.** Under contention every sample is inflated by a
   non-negative amount, so min-over-9 is the least-contaminated estimate of the
   uncontended value, and the difference of two mins is the least-contaminated
   estimate of the true difference. Both are reported; where they disagree,
   the disagreement is the error bar.

**Binaries** (all `cargo build --release --offline -p rigor-cli`, `rigor 0.0.1`):

| binary | what | build | path |
|---|---|---|---|
| baseline | `master` @ `33850a0`, unmodified | fresh `CARGO_TARGET_DIR`, **23.47 s** | `…/scratchpad/rigor-master-baseline` (10,756,528 B) |
| A | this branch @ `12721b6` (the instrumentation) | fresh worktree `target/`, **19.35 s** (+5.45 s incremental relink after the probe patch was stripped) | `.claude/worktrees/agent-a1c553fb72980665e/target/release/rigor` (10,733,968 B) |
| P | A + a throwaway `RIGOR_PROBE` / `RIGOR_PROBE_N` hook | separate `CARGO_TARGET_DIR`, **21.67 s** + 10.13 s | `…/scratchpad/rigor-probe` (10,786,896 B) |

The probe hook was applied, built, the binary copied out, and the patch then
**removed from the tree** — the committed diff is the instrumentation only.
The `readprobe` used in §4 is a standalone throwaway cargo project built
entirely under the session scratchpad, never inside this repository.

**Cells** (5 per corpus per round, 9 rounds, plus a warm-up sweep per corpus
before round 1): `A` (full pool), `A1` (`RAYON_NUM_THREADS=1`), `P.n0`
(control), `P.hK` (8 extra harvests), `P.sK` (8 extra sha256s).

**Corpora** — the four standard scales, `cwd` set to the worktree root (a Rust
tree with **no** `Gemfile.lock` and no `.rigor.yml`, verified), never the corpus
directory, so ADR-72's `Gemfile.lock` plugin auto-detection cannot let a
corpus's own overlay leak into `index-load`. File counts are the binary's own
`files=` field.

| label | path | files |
|---|---|---:|
| conference-app | `/Users/megurine/repo/ruby/conference-app` | 245 |
| mastodon/app | `/Users/megurine/repo/ruby/mastodon/app` | 1,236 |
| gitlab-foss/lib/gitlab | `/Users/megurine/repo/ruby/gitlab-foss/lib/gitlab` | 3,117 |
| gitlab-foss/lib | `/Users/megurine/repo/ruby/gitlab-foss/lib` | 4,675 |

`mastodon/app` and `gitlab-foss/lib` are `harness/sweep-corpora.yml` members;
the other two are the additional scales `harness/CORPUS.md` and the LSP notes use.

---

## 1. The harvest's cost, per corpus

Cell A, 12 threads, median of 9 interleaved rounds (min–max in brackets), ms:

| corpus | files | stage-1 wall | stage2 | stage3 | index-load | internal total | **wall (spawn→exit)** |
|---|---:|---:|---:|---:|---:|---:|---:|
| conference-app | 245 | 3.95 [3.59–7.00] | 0.58 | 1.92 | 22.65 | 28.95 [28.06–43.82] | 121.34 [98.41–151.39] |
| mastodon/app | 1,236 | 23.36 [18.94–30.61] | 18.47 | 11.85 | 22.57 | 77.39 [69.30–88.09] | 186.66 [151.73–191.26] |
| gitlab-foss/lib/gitlab | 3,117 | 58.92 [53.69–83.06] | 47.29 | 31.83 | 22.39 | 163.46 [153.88–237.37] | 277.59 [256.48–386.74] |
| gitlab-foss/lib | 4,675 | 82.19 [81.27–97.81] | 70.56 | 46.68 | 22.53 | 232.74 [216.12–248.98] | 364.72 [343.76–400.67] |

**The harvest inside it** — three views of the same call, plus its share of the
whole run:

| corpus | files | harvest CPU **sum** (serial cell A1) | harvest CPU sum (parallel cell A) | worst single file | **marginal WALL** (A/B ÷8, min / median) | share of **stage-1 wall** | share of **internal total** | **share of TOTAL wall** |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| conference-app | 245 | **0.37** | 0.94 | 0.18 | **0.015 / 0.056** | 0.4–1.4 % | 0.05–0.19 % | **0.01–0.05 %** |
| mastodon/app | 1,236 | **6.03** | 12.70 | 0.78 | **0.647 / 0.595** | 2.5–2.8 % | 0.77–0.84 % | **0.32–0.35 %** |
| gitlab-foss/lib/gitlab | 3,117 | **27.27** | 62.90 | 1.57 | **3.737 / 4.074** | 6.3–6.9 % | 2.29–2.49 % | **1.35–1.47 %** |
| gitlab-foss/lib | 4,675 | **38.84** | 84.31 | 1.27 | **4.111 / 4.745** | 5.0–5.8 % | 1.77–2.04 % | **1.13–1.30 %** |

Serial cell (`RAYON_NUM_THREADS=1`), where CPU and wall coincide, medians in ms:

| corpus | stage-1 wall | `parse+lower-cpu` | `harvest-cpu` | **harvest share of stage-1 CPU** | internal total |
|---|---:|---:|---:|---:|---:|
| conference-app | 11.23 | 10.76 | 0.37 | **3.3 %** | 41.55 |
| mastodon/app | 80.98 | 74.48 | 6.03 | **7.5 %** | 188.00 |
| gitlab-foss/lib/gitlab | 241.73 | 213.35 | 27.27 | **11.3 %** | 513.55 |
| gitlab-foss/lib | 372.99 | 333.40 | 38.84 | **10.4 %** | 780.41 |

**The A/B measurement, unreduced** (probe binary, stage-1 wall, ms):

| corpus | K=0 (control) | harvest ×8 | Δ | Δ/8 | sha256 ×8 | Δ | Δ/8 |
|---|---:|---:|---:|---:|---:|---:|---:|
| conference-app | 3.81 [3.60] | 4.26 [3.72] | 0.45 [0.12] | 0.056 [0.015] | 4.49 [3.86] | 0.68 [0.26] | 0.084 [0.032] |
| mastodon/app | 20.39 [19.62] | 25.15 [24.80] | 4.76 [5.18] | 0.595 [0.647] | 23.61 [22.75] | 3.22 [3.13] | 0.403 [0.391] |
| gitlab-foss/lib/gitlab | 58.66 [52.53] | 91.26 [82.43] | 32.60 [29.90] | 4.074 [3.737] | 71.55 [69.08] | 12.89 [16.55] | 1.611 [2.069] |
| gitlab-foss/lib | 85.49 [81.64] | 123.44 [114.53] | 37.95 [32.89] | 4.745 [4.111] | 133.96 [102.74] | 48.47 [21.10] | 6.060 [2.637] |

(median [min]; Δ columns are median−median and min−min respectively.)

**Three independent estimates agree**, which is why the ~4–5 ms figure is
trustworthy despite the machine:

1. The A/B differential: **4.111–4.745 ms** at 4,675 files.
2. The amortized model: serial `harvest-cpu` 38.84 ms ÷ 12 threads = **3.24 ms**
   (a floor — it assumes perfect parallel efficiency).
3. PR #95's own before/after, where the harvest MOVED from the serial merge into
   stage 1's rayon closure: stage 2 fell 56.7 ms while stage 1 rose **4.7 ms**.
   The correction section's prediction, reproduced independently here.

---

## 2. Does the prize scale? Partly — and it plateaus

The absolute saving grows monotonically (0.02 → 0.6 → 3.9 → 4.4 ms). Its
**share of wall does not**: 0.03 % → 0.34 % → 1.4 % → **1.2 %**. It flattens
between 3,117 and 4,675 files because stage 1 is I/O + libprism-FFI bound and
the harvest is neither — the harvest is a cheap in-memory walk of an AST that
has already been paid for, and it parallelises as well as everything else
around it.

The serial view says the same thing from the other side: the harvest is
**3.3 % → 11.3 % → 10.4 %** of stage-1 CPU, i.e. roughly a tenth of a stage that
is itself a third of the run. There is no regime in the measured range where it
becomes the bottleneck.

---

## 3. Methodology note: why the parallel CPU sum is ~2.2× the serial one

`harvest-cpu` reads 84.31 ms in the parallel cell and 38.84 ms in the serial
cell for the same 4,675 files. That is not a real cost difference — it is the
per-file timers measuring **wall on a contended worker**. With a 12-thread pool
on a machine already carrying a load of ~9, a worker is descheduled mid-harvest
often enough to roughly double its clock delta. The same ratio shows on
`parse+lower-cpu` (888.57 parallel vs 333.40 serial = 2.7×).

**Consequence for anyone reading the shipped line:** on a loaded machine,
`stage1.harvest-cpu` and `stage1.parse+lower-cpu` are upper bounds, and
`harvest-wall-amortized` inherits that inflation. Take `RAYON_NUM_THREADS=1`
for a clean CPU decomposition; take an A/B for a clean marginal wall. Both
techniques are in the Method section above and cost nothing but a rerun. This
is exactly why the shipped instrumentation reports the wall, the sum, the max
and the model as four separately-labelled numbers rather than collapsing them
into one "harvest time".

---

## 4. Pricing the cache against the prize

The warm path of the design in the probe note is: for every file, hash the
already-read source, look the key up on disk, read the entry, decode it. The
prize is **4.1–4.7 ms of wall** at 4,675 files. Each cost below is measured on
the same machine, at the same scale, in the same session.

### 4.1 Hashing every file — 2.6–6.1 ms, paid on hit and miss alike

sha256 over the already-read `String` (the repo's own dependency-free
`sha256_hex`, `crates/rigor-cli/src/diagnostic_formats.rs:302`), inserted into
the same rayon closure so it is apples-to-apples, measured by the same ×8 A/B:

| corpus | sha256 marginal wall (min / median) | as % of the harvest prize |
|---|---:|---:|
| conference-app | 0.032 / 0.084 ms | 150–210 % |
| mastodon/app | 0.391 / 0.403 ms | 60–68 % |
| gitlab-foss/lib/gitlab | 2.069 / 1.611 ms | 40–55 % |
| gitlab-foss/lib | 2.637 / 6.060 ms | **64–128 %** |

(The 4,675-file median/min disagreement is the widest in the whole measurement
and is noise, not structure; the honest reading is "between about half and all
of the prize".) Note this is *cheaper* than the 08-26 note's blake3 hash-only
pass (53.4 ms at 4,675) for the reason Design A was supposed to be better than
Design B: there is no extra read — the bytes are already in hand. That
advantage is real, and it is still not enough.

### 4.2 Reading the entries — 44–52 ms, i.e. **10× the entire prize**

A throwaway `readprobe` (standalone cargo project under the scratchpad, `rayon`
only) builds the exact on-disk shape the probe note specifies —
`<root>/<kk>/<64-hex>.hv`, one small file per source file, two-hex shard dirs —
then reads them all back with `par_iter`, warm, exactly as stage 1 reads the
sources. Best of 9 sweeps after a warm-up:

| files | 4 KB entries, total | read sweep min / median / max | packed into ONE file instead |
|---:|---:|---|---:|
| 245 | 1.0 MB | 2.46 / 2.79 / 2.99 ms | 0.034 ms |
| 1,236 | 5.1 MB | 12.89 / 13.73 / 18.23 ms | 0.124 ms |
| 3,117 | 12.8 MB | 32.27 / 33.58 / 33.95 ms | 0.472 ms |
| 4,675 | 19.1 MB | **44.50 / 49.08 / 50.37 ms** | **1.121 ms** |

Controls, all at 4,675 files:

* **It is syscalls, not bytes.** 1 KB entries cost the same as 4 KB ones
  (49.46 ms vs 48.68 ms) — the cost is 4,675 open/read/close round trips.
* **It does not parallelise away.** Serial 83.09 ms; 4 threads 46.22 ms;
  12 threads 48.68 ms. The VFS layer plateaus at ~46 ms and more threads do
  nothing.
* **It is not a `/tmp` artifact.** Repeated under `$HOME/Library/Caches` (where
  §4.1 of the probe note actually puts the cache): 48.14 ms.
* **Cross-validation**: the 08-26 note's independent blake3 hashprobe read+hashed
  the same 4,676 files in 53.4 ms — the same ~50 ms floor for a 4,675-file
  parallel read sweep, measured by different code in a different session.

The 4 KB entry size is the probe note's own figure (`lsp.rs:644`, "~4 KB/file"
in the LSP's held table).

### 4.3 Decoding — unmeasured, and structurally not small

No number, because the codec does not exist. But the shape is knowable: decode
must rebuild *exactly* the ten collections, four nested `Vec`s of structs, and
the thousands of `String`s that `harvest` produces. `harvest`'s own 38.84 ms of
CPU is a single AST walk **plus** that construction; decode skips the walk and
keeps the construction. So decode is plausibly a substantial fraction of the
prize all by itself — and every honest sensitivity analysis has to allow for
that on the cost side.

### 4.4 The arithmetic

At 4,675 files, per warm run, wall:

```
prize      : 4.1 – 4.7 ms   (skip one SourceIndex::harvest per file)
cost, hash : 2.6 – 6.1 ms   (mandatory, hit or miss)
cost, read : 44.5 – 49.1 ms (one entry per file)   ← alone, 10× the prize
cost, decode: unmeasured, > 0, plausibly ~ half the prize
--------------------------------------------------------------
net        : roughly −45 to −55 ms.  The cache makes `check` SLOWER.
```

**The steelman, priced honestly.** Pack every entry into one per-project sidecar
and the read collapses to 1.12 ms (§4.2, last column). Then:

```
prize   : 4.1 – 4.7 ms
hash    : 2.6 – 6.1 ms
read    : 1.1 ms
decode  : unmeasured, > 0
--------------------------------
net     : between roughly break-even and clearly negative
```

…and that variant buys its read back by rewriting a ~19 MB artifact whenever
anything changes (or maintaining an append log plus compaction), and by putting
a whole-project serialized blob under ADR-0020 — the single-point-of-failure
determinism hazard the probe note flags for **Design B**, now attached to
Design A. Paying Design B's invalidation surface for a break-even prize is a
strictly worse trade than not building it.

---

## 5. Verdict — **NO-GO on #104**

**One line: the prize is 4.1–4.7 ms of wall (1.1–1.3 % of total) at 4,675 files
and the cache's mandatory warm-path cost — ~48 ms of read plus 2.6–6.1 ms of
hashing — exceeds it by an order of magnitude.**

Against [ADR-0037](../adr/0037-sidecar-perf-slices-retired-by-measurement.md)'s
two bars:

* **"Does the saving scale with corpus size?"** — *Partly, and it plateaus.* The
  absolute saving grows (0.02 → 4.4 ms) but its share of wall peaks at 1.5 % and
  turns over between 3,117 and 4,675 files. ADR-0037's reopening clause asks for
  "a project where the delta **grows** with size"; a share that flattens at
  ~1.3 % is not that.
* **"Does it beat the invalidation-surface cost?"** — *No, and not narrowly.*
  It does not even beat the cache's own **read**, before any invalidation
  question is asked.

The comparison that settles it: **ADR-0037 retired an on-disk cache for a
~60 ms per-run cost** ("caches an already-cheap (~0.06s) per-run cost; the
cross-run saving does not justify a content-addressed disk cache and its
invalidation surface — a real soundness-risk area"). This one would cache a
**4–5 ms** cost — twelve times smaller — behind a *larger* invalidation surface
(per-file content keys + a generation fingerprint over `.rigor.yml`,
`Gemfile.lock`, the core's class surface, and binary identity; the probe note's
§3 table has eight terms). Building it would contradict the ADR that is already
on the books, on the ADR's own criterion.

**The GO is withdrawn, not suspended.** The re-measure note's Design A GO rested
on a ceiling of 93.1 ms / 25 % of wall that the probe's §0 already showed
belongs to a different design (one that also evicts the AST from merge M3 and
from stage 3 — both blocked). With the real number in hand, the structural
arguments the GO also rested on (graceful degradation to the single-file-edit
case, low per-entry coupling) are all still true and all irrelevant: they
describe how *well* a cache would deliver 4 ms.

Design B's NO-GO is untouched — it rested on invalidation shape, not on this
ceiling — and is now doubly supported, since its mandatory hash-all-files pass
(53.4 ms) is on the same side of the ledger as §4.2's read.

### What would reopen it

Any **one** of these, measured, not argued:

1. **The AST stops being required.** If a future slice removes `&LoweredAst`
   from merge M3 (#92 §5) *and* from stage 3's per-file rule walk, the skippable
   unit becomes `parse+lower+harvest` and the prize jumps to stage 1's whole
   82 ms wall — 20× larger, and then the ~48 ms read is worth paying. This is
   the only change that moves the prize by an order of magnitude. Note stage 3
   is the harder half: it needs a per-file **diagnostics** cache, which is
   Design-B-shaped (the probe note's §0, and #93 already said NO-GO to the
   diagnostics cache).
2. **The harvest gets much more expensive.** If a future harvest pass pushes
   `stage1.harvest-cpu` past roughly a third of stage-1 CPU at ≥3,000 files
   (it is 10–11 % today), re-run §1's A/B. The shipped instrumentation makes
   this a one-command check, and that is its standing value.
3. **A workload where the read cost does not apply.** A long-lived process that
   holds entries in memory across runs — which is the LSP, and the LSP
   *already* holds `Arc<Harvest>` per file (`lsp.rs:661`), so its steady state
   needs no disk cache at all. The only reachable case is LSP **cold start**,
   where the same ~48 ms read would be amortized over a session rather than
   paid per run; that is a different measurement against a different baseline
   and would need its own note.

Absent one of those, do not re-open this. The instrumentation stays; the cache
does not get built.

---

## 6. What this note does not claim

* **No cache was built**, so no hit-rate, no `--verify-incremental` differential,
  no real decode number. §4.3 is an argument, not a measurement, and it is on
  the *cost* side — the verdict does not depend on it (§4.4's per-file-entry
  arithmetic is already decisive with decode set to zero).
* **The machine was not idle** (Method). The verdict rests on a 10× margin and
  on three independent estimates of the prize agreeing to within 1.5 ms, not on
  any single number's precision. No plausible re-measurement on a quieter
  machine turns a 10× loss into a win: a quieter machine makes the *prize*
  smaller too, since the prize is CPU-bound work that contention inflates.
* **The probe note's design findings are not re-litigated.** Serializability,
  the determinism rules, the key and generation fingerprint, placement, the
  `--verify-incremental` forms — all still correct, all now unbuilt. If item 1
  above ever fires, that note is still the build instruction.
* **`RIGOR_TIMING` is not a benchmark harness.** It is one stderr line under an
  env gate. Every number here came from driving it in a loop from a scratchpad
  script, and the script is not committed.

## Gates

| gate | verdict |
|---|---|
| `cargo test --workspace` | PASS |
| `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy --workspace --all-targets -- -D warnings` | PASS (clean) |
| `check` byte-identical vs a `master`-built baseline on mastodon/app, `RIGOR_TIMING` **unset** | PASS — stdout (420 lines), stderr (0 lines) and exit code (1) all identical |
| `harness/run_snapshot.rb` | PASS |
| `python3 harness/docs_check.py` | PASS |

No file under `crates/` other than `crates/rigor-cli/src/main.rs` was touched.
The throwaway probe hook was stripped from the tree before committing; the
`readprobe` project lives only under the session scratchpad. `reference/rigor`
and `REFERENCE_RIGOR_DIR` were not touched.
