# LSP held harvest — implementation (2026-08-25)

Implements [the mini-spec](20260825-lsp-held-harvest-mini-spec.md), the
frozen-index arc's LSP consumer, on top of [#92](20260825-s92-harvest-merge-impl.md)
and [#94](20260825-s94-ancestor-closure-impl.md). **No emitted diagnostic changes
anywhere**: `check` is byte-identical on 5 912 corpus files, and the LSP's
dispatch index is graded against the pre-slice one as an oracle.

Headline: at 4 675 files the cross-file overlay guard was **OFF** and is now
**ON** — the editor gets `check`-equal cross-file diagnostics at a scale where it
had silently fallen back to single-file scope.

## Phase 0 — the premise, measured before any production code

Release build, master tree, `#[ignore]`d probes driving `build_overlay` +
`overlay_source_index` directly on the real corpora (11 reps, rep 0 discarded).

**1. Per-dispatch overlay cost** (the tier-2 `build_project` in
`overlay_source_index`) and the guard's state at each scale:

| corpus | files | median | min–max | `OverlayGuard` after 11 samples |
|---|---|---|---|---|
| mastodon/app | 1 236 | 26.6 ms | 26.0–28.4 | ON |
| gitlab-foss/lib/gitlab | 3 117 | 78.9 ms | 77.3–86.6 | ON |
| gitlab-foss/lib | 4 675 | **114.5 ms** | 110.3–151.2 | **OFF** (Disabled at sample 2) |

Not "comfortably under 100 ms at 4 675" — over it on **every one of 11 samples**,
so the guard trips at the first opportunity and the session runs single-file.
Premise 1 **holds**.

**2. Per-file `Harvest` memory.** Measured in a process that has NOT yet built
and freed a `SourceIndex` — the dispatch probe's RSS delta reads ~0 KB because
tier 1's `build_project` leaves a free pool the harvests then reuse, which is
exactly how a memory probe lies:

| corpus | files | `size_of::<Harvest>()` | held ASTs | held harvests | ratio |
|---|---|---|---|---|---|
| mastodon/app | 1 236 | 312 B | 36.6 KB/file | **3.68 KB/file** | 10.1 % |
| gitlab-foss/lib | 4 675 | 312 B | 44.2 KB/file | **4.04 KB/file** | 9.1 % |

~4 KB/file against the ~40 KB/file ASTs the table already holds — a 9–10 %
increase on a structure the S4b risk register already accepted. It does not
rival the ASTs. Premise 2 **holds**.

Both STOP conditions cleared, so the slice was implemented.

## What changed

Everything is in `crates/rigor-cli/src/lsp.rs` except one signature (below).

* **`HeldFile = (PathBuf, Arc<LoweredAst>, Arc<Harvest>)`** — the tier-1 table's
  entry. `Arc` per member for the reason the AST always had one: one entry is
  replaceable and the whole table clones by pointer copy.
* **`build_overlay`** harvests inside the SAME rayon closure as parse+lower
  (`held_pair`, the single site where the two are produced together), and the
  `SourceIndex::merge` it then times is the quantity the guard is defined on —
  the same quantity a dispatch pays, rather than a different one.
* **`reharvest_sources`** swaps BOTH members of the changed entry.
* **`overlay_source_index`** harvests only the dirty buffer, assembles
  `Vec<(&Harvest, &LoweredAst)>` with that file's PAIR replaced (same REPLACE /
  append rule, same order), and calls `SourceIndex::merge` directly. The timer
  now starts before the buffer harvest, so the guard's budget still means "what
  one dispatch costs".
* `harvest_one` → **`lower_one`**: after #92 "harvest" is the per-file index
  harvest, and the old name meant read+parse+lower. Two things called harvest in
  one function was not survivable.
* `OverlayBuild.parse_lower` → `parse_lower_harvest`, `.build_project` →
  `.merge`, and the `RIGOR_TIMING` line's labels follow (invisible by default;
  no test, harness or editor reads it).

**The one change outside `lsp.rs`** (the note the task allowed):
`SourceIndex::merge` is now generic over `H: Borrow<Harvest>`. The merge only
ever READ each harvest, so this is a widening, not a semantic change: `check`
still instantiates `H = Harvest` and every existing call site compiles
unchanged. Without it the LSP could not hand `merge` a harvest it holds behind
an `Arc` without deep-cloning it — i.e. it could not avoid the recomputation
this slice exists to remove. The body gained six `let h = h.borrow();` lines and
nothing else; one `probes_s92` test needed a turbofish for the empty-slice case.

### Why no diagnostic can move

`build_project(asts, core)` **is** `merge(asts.map(|a| (harvest(a, core), a)),
core)` (#92), and `harvest` reads only its own AST and the frozen `CoreIndex`.
So the dispatch's index differs from the pre-slice one only in WHERE each
harvest came from — held vs recomputed — over the same files, in the same order,
against the same core. The `CoreIndex` half of that is an invariant rather than
an assumption, written into `HeldFile`'s docs: every path that can change the
core rebuilds the whole table (`invalidate` → `apply_full_overlay_build` →
`build_overlay`), and the one path that does not (`reharvest_sources`) reuses
`st.project.index` verbatim because a project `.rb` file cannot change the
plugin set or the signature dirs.

All S1–S4b invariants are untouched: REPLACE not ADD (now of the PAIR — a
swapped AST beside a kept harvest would serve facts the buffer no longer states,
the double-registration hazard by another route), the 3-axis stale-drop, the
generation guard, the debounce, the single-writer publish, and the guard's
budget / hysteresis / disclosure / fallbacks.

## Tests

**`held_harvest_dispatch_index_equals_the_pre_slice_rebuild`** — the equivalence,
in the `probes_s92`/`probes_s94` shape: `overlay_source_index_legacy` keeps the
pre-slice body VERBATIM (only the held entry's arity is destructured away) as the
oracle, and the two indices are graded by the full diagnostic set (rule, span,
message) over ten editor states: a held file unedited, a held file edited both
ways, a buffer redefining the base another file subclasses, a cross-file toplevel
call, a constant write, an untitled buffer and an out-of-project `file:` buffer
(both append-branch), and the overlay-OFF fallback.

**`held_harvest_save_makes_the_new_content_visible_cross_file`** — a
`didChangeWatchedFiles` save on a file that is NOT the open buffer, driven
through `call.unresolved-toplevel` (harvest pass 1c) rather than the override
index the other re-harvest tests use, so the two cover different harvest fields.
Rename the definer away on disk → the call goes unresolved; restore it → it
resolves again.

**The oracle bites — verified by mutation, and the first version did NOT.**

| mutation | caught by |
|---|---|
| `overlay_source_index` keeps the HELD harvest beside the buffer's AST | equivalence test — **only after the case set was fixed** |
| `reharvest_sources` swaps the AST and keeps the harvest | the new save test **plus** both pre-existing S4b re-harvest integration tests |

The first row is the finding worth recording. The equivalence test as first
written passed the stale-harvest mutation: every case it graded either analysed a
buffer whose own diagnostics came from walking its AST (the override rule reads
the analysed AST's visibility, not the index's) or produced no diagnostics at
all. Only a buffer whose OWN diagnostics depend on a fact carried solely by its
harvest can tell a replaced harvest from a kept one — so the case set gained two
discriminators, and each was verified to fail the mutation on its own:

* the buffer ADDS a toplevel `def` the file on disk lacks and calls it (a kept
  harvest ⇒ `unresolved-toplevel` fires), and
* the buffer REMOVES a toplevel `def` the file on disk has while still calling it
  (a kept harvest ⇒ the removed name still resolves — the "renamed away but still
  resolvable" false negative that the REPLACE rule exists to prevent).

## Gate verdicts

| # | gate | verdict |
|---|---|---|
| 1 | the two new LSP tests | **PASS** — both, and both mutation-verified |
| 2 | `cargo test --workspace` (incl. `lsp_check_parity`) | **PASS** — 378 / 4 / 3 / 9 / 94 / 277 / 47 / 251 / 48, 0 failed (376 → 378 in rigor-cli: +2 new) |
| 2 | `cargo clippy --workspace --all-targets -- -D warnings`, fresh `CARGO_TARGET_DIR` | **PASS** — clean, no new `#[allow]` |
| 3 | `harness/run_snapshot.rb` | **PASS** — 98 fixtures, 407 matched, 2 registered divergences, **0 unregistered extras** |
| 3 | `fp_audit.py --gaps --sweep` (release rebuild) | **PASS** — **0 FP / 9 204 files** (8 corpora present, 0 absent), 820 gaps, gap set **byte-identical** to the branch-point baseline (see the harness note below for how it was run) |
| 3 | `rigor check` vs the master-baseline binary | **PASS** — see below |
| 4 | timing + guard | table below; success criterion **met** |

`cargo doc --no-deps -p rigor-cli` has the same six unresolved intra-doc links
before and after — no new ones. `harness/docs_check.py`: PASS (4 budgets, links
resolve).

Every corpus gate above was re-run against the FINAL frozen tree
(`target/release/rigor` `sha256 f289eb07…`, `target/debug/rigor`
`sha256 f84c2657…`), not a mid-implementation build — #94's deviation 3: a
doc-comment insertion moves line tables, so the release artifact changes even
when no code does, and a sweep from before the last edit has not graded the
committed tree. The first sweep of this session was discarded for exactly that
reason.

### The `check` tripwire

Byte-identical stdout + stderr + exit against the master-built baseline binary
(`sha256 c20b4226…`, cut at the branch point) on **5 912 files**:

* mastodon/app (1 236 files), text format, project cwd — under default threads
  AND `RAYON_NUM_THREADS=1`, and the branch binary's two thread modes agree with
  each other;
* `--format json` from a clean temp cwd with absolute paths (fp_audit's own
  invocation shape) over mastodon/app **and** gitlab-foss/lib (4 676 files) —
  647 KB of JSON, identical byte for byte.

The first attempt at this compared two EMPTY outputs and reported "identical":
BSD `xargs` has no `-a`. That is AGENTS.md measurement artifact #3 verbatim, so
the script now refuses to report a verdict when the baseline side produced
almost no output.

## Timing (release, 20 interleaved paired reps per scale, rep 0 discarded)

BEFORE and AFTER run **in the same process on the same inputs**, alternating
within each rep: `overlay_source_index_legacy` IS the pre-slice dispatch, so both
sides see the same held table, the same buffer and the same thermal state. (The
machine was shared with another agent's benchmark for part of the session, which
is what the tails show; the paired per-rep saving is the number to trust.)

### Per-dispatch overlay cost — the keystroke path

| corpus | files | BEFORE | AFTER | paired median saving | guard BEFORE | guard AFTER |
|---|---|---|---|---|---|---|
| mastodon/app | 1 236 | 26.5 ms (25.0–27.7) | **19.4 ms** (18.8–21.1) | −7.1 ms (−27 %) | ON | ON |
| gitlab-foss/lib/gitlab | 3 117 | 80.7 ms (77.7–117.9) | **48.7 ms** (46.4–68.6) | −32.1 ms (−40 %) | ON | ON |
| gitlab-foss/lib | 4 675 | 112.8 ms (108.9–118.5) | **69.8 ms** (66.5–93.1) | −43.4 ms (−38 %) | **OFF** | **ON** |

**The success criterion — does the guard stay ON at 4 675 at the median? Yes.**
And not only at the median: all 20 timed AFTER samples came in under the 100 ms
budget, 19 of them under 72.1 ms, the single 93.1 ms outlier landing while the
machine was loaded — and the guard never flipped across all 21 reps. The BEFORE
column never produced an under-budget sample at that scale and disabled the
overlay at its second one.

The honest tail: 69.8 ms median leaves ~30 % headroom, not the comfortable margin
that would justify retiring the guard. A loaded machine still gets within 7 ms of
the budget, and the same quantity spanned 3.3× across machines when the guard was
specified. The guard stays.

### Tier-1 build (6 reps, ORDER ALTERNATED per rep; medians)

| corpus | BEFORE parse+lower / `build_project` / total | AFTER parse+lower+harvest / `merge` / total |
|---|---|---|
| 1 236 | 19.8 / 27.8 / 47.2 ms | 22.6 / 19.3 / **42.2 ms** |
| 3 117 | 52.3 / 83.5 / 136.3 ms | 56.6 / 48.1 / **105.0 ms** |
| 4 675 | 81.5 / 121.4 / 202.4 ms | 85.6 / 69.9 / **156.1 ms** |

**The spec expected tier 1 to get ~57 ms SLOWER at 4 675 and it got ~46 ms
faster.** The harvest did not become new work — it moved from the SERIAL
`build_project` into the PARALLEL rayon closure, exactly as #92 did for `check`'s
stage 1. Wall-clock cost of adding it to stage 1: +3…+4 ms at every scale
(the serial harvest is 42.7 ms at 4 675). Cost removed from stage 2: −51 ms. So
the "rare invalidation pays for it" trade the spec was prepared to accept was
never needed; the loop thread's synchronous rebuild is strictly cheaper than
before.

Two measurement confounds were found and fixed while producing this table, both
of which had made the AFTER column look better than it is:

1. **Order.** Running BEFORE first in every rep gave AFTER a warmer page cache
   and allocator. The order now alternates per rep.
2. **The directory walk.** `project_files` sits outside `build_overlay`'s timer,
   and the first legacy harness had it inside its own — ~15 ms at 4 675 files,
   charged to BEFORE only. It made parse+lower look SLOWER than
   parse+lower+harvest, which is impossible and was the tell.

## Deviations from the spec

1. **`SourceIndex::merge`'s signature is widened** (`H: Borrow<Harvest>`) rather
   than a `Harvest` accessor being made public. Same budget — one signature, with
   this note — and it is the change the design actually needs: the LSP must pass a
   harvest it OWNS behind an `Arc` into the merge, which no accessor enables.
   Argued in the method's own docs; `check` is byte-identical across 5 912 files
   and `probes_s92`'s 17-field equivalence suite is unchanged and green.
2. **The harvest runs inside `build_overlay`'s rayon closure**, not serially. The
   spec said "compute each file's harvest alongside its parse+lower … it runs on
   the loop thread today; keep that". `build_overlay` is still called
   synchronously ON the loop thread — that property is kept; what is parallel is
   the same per-file closure that was already parallel, matching `check`'s stage 1
   after #92. This is why the expected +57 ms never appeared.
3. **The Phase-0 / gate-4 timing probe is NOT committed.** Precedent: #92 and #94
   promoted their EQUIVALENCE harnesses to permanent tests (this slice does the
   same with `overlay_source_index_legacy`) but measured timing with
   `RIGOR_TIMING` on the real binary rather than a committed bench. The probe is
   only meaningful against machine-specific external corpora; its method is
   described above and the durable operator-facing instrument is the
   `RIGOR_TIMING` line, whose labels this slice updated.
4. **20 reps per side rather than 3–5**, for the reason #94 found: the machine was
   not quiet, and the paired interleaved saving is what survives that.

## Found in passing, NOT fixed here (no `harness/` changes)

`harness/fp_audit.py --gaps` **crashes on master**, before this slice exists:

```
TypeError: '<' not supported between instances of 'str' and 'NoneType'
  harness/fp_audit.py:254 in _by_count
```

`_by_count` (added by PR #97 for issue #96) sorts the gap Counter's keys, and the
reference emits **9 diagnostics with a null `rule`** across the sweep set — the
first is on `rigor-survey/Ruby`, which aborts the run before the FP total is ever
printed. It was invisible until #97 because `Counter.most_common()` never
compares keys. Both sweeps in this note were therefore run through a wrapper that
executes `harness/fp_audit.py` **unmodified** with `builtins.sorted` ordering
`None` as `""` — it changes no count, no set and no element, only the render
order of a single row. Worth a one-line harness fix (`kv[0] or ""`) plus a look
at what those 9 rule-less reference diagnostics are.

## Not done (spec non-goals, unchanged)

Cross-file hover / completion (still single-file, still needs its own
cached-last-good-index design); no AST eviction (merge's M3 still consumes ASTs);
no persistence; no guard-budget change; no `check` behaviour change of any kind.
