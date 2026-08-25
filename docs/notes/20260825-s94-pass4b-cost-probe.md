# Pass 4b (`compute_literal_returns`) cost probe — issue #94 (2026-08-25)

Probe for [#94](https://github.com/rigortype/rigor-rs/issues/94), which reads
[the #92 pass inventory](20260825-s92-buildproject-pass-inventory.md) (§7's
43.1 % / 47.1 % `build_project` numbers, pre-decomposition) and
[the #92 harvest/merge impl note](20260825-s92-harvest-merge-impl.md) (post-
decomposition: `compute_literal_returns` is still 43–47 % of the now-smaller
`merge`). **Investigation only — nothing was refactored; all production
behaviour is unchanged from `master`.**

Subject: `crates/rigor-infer/src/source_index.rs` —
`compute_literal_returns` (:1469), `overridden_in_project` (:1672),
`related_to_owner` (:1684), `override_ancestor_names` (:1391),
`OVERRIDE_ANCESTOR_WALK_LIMIT` (:105, = 100).

**Method.** Temporary `RIGOR_S94_PROBE`-gated instrumentation was added
directly to `merge`/`compute_literal_returns`/`overridden_in_project`/
`related_to_owner` (`Instant` timers + thread-local counters, env-var gated so
normal runs pay nothing), a release binary was built, and `rigor check` was
run over the two `harness/sweep-corpora.yml` scales named in the issue —
`/Users/megurine/repo/ruby/mastodon/app` (1 236 files) and
`/Users/megurine/repo/ruby/gitlab-foss/lib` (4 675 files) — six times each,
mastodon/gitlab runs alternating (the #92 note's interleaving discipline,
against one-sided thermal drift). Medians below are of those six. **The
instrumentation was verified equivalence-preserving** (`cargo test -p
rigor-infer` — 273/273 passed — run WITH the instrumentation present) and was
then fully reverted; the tree this note ships against is byte-identical to
`master` plus this file.

---

## Verdict up front

| | |
|---|---|
| Q1: is the BFS gate (`overridden_in_project`) "the" dominant cost of 4b? | **Scale-dependent — confirmed at gitlab-foss/lib (70 % of 4b), refuted at mastodon/app (38 % of 4b, minority)** |
| Q2: memoize per-`(candidate, owner)` pair or precompute per-candidate closure? | **Precompute-per-candidate — pair-memo only removes 6–11 % of calls; per-candidate reuse is 12–22×** |
| Q3: is a per-merge memo sound? | **Yes** — the only state read (`override_classes`) is frozen before M3 by construction (borrow-checker-enforced, not just observed) |
| Q4: what do the mechanical fixes alone buy? | `VecDeque` — **negligible** at measured queue depths (cap never approached); string-allocation avoidance — **plausible, proportional to ~13K–165K node expansions**, but doesn't touch the call-count redundancy |
| Cap (`OVERRIDE_ANCESTOR_WALK_LIMIT` = 100) hit rate | **0 / 0** — never triggered, either corpus, any of the 12 runs |

---

## 1. Within-4b profile (Q1)

| | mastodon/app (1 236 f) | gitlab-foss/lib (4 675 f) |
|---|---|---|
| `compute_literal_returns` total (median) | 13.66 ms | 92.14 ms |
| … inside `overridden_in_project` (median) | 5.17 ms | 64.86 ms |
| … rest of 4b (fold walking: `resolve_fold_key`/`fold_key_sites`/`fold_expr`) | 8.50 ms | 27.28 ms |
| **`overridden_in_project` share of 4b** | **37.8 %** | **70.4 %** |
| `stage2(merge)` total (median, `RIGOR_TIMING`) | 23.77 ms | 140.19 ms |
| 4b share of `stage2(merge)` | 57.5 % | 65.7 % |

(Ranges across the 6 runs: mastodon 4b_total 13.04–15.32 ms, gitlab 4b_total
87.89–128.01 ms — the high outliers in both are each corpus's FIRST run,
cold-cache/cold-branch-predictor, exactly the effect the #92 note flagged;
medians already discard it.)

**Verdict: neither a flat confirm nor a flat refute — it depends on scale.**
At gitlab-foss/lib, the larger and more override-graph-rich corpus,
`overridden_in_project` genuinely dominates 4b (70 %, matching the #92 probe's
"likely explains"). At mastodon/app it is a MINORITY of 4b (38 %) — the
fold-walking half (`fold_expr` resolving implicit-self/`Const.method` calls
into other files' ASTs, `resolve_fold_key`'s memo bookkeeping) is actually
larger there. This is consistent with an `O(definers × ancestor-branching)`
unmemoized-BFS cost model for the gate: it grows worse than the roughly
per-method-linear fold walk as the project's override graph gets bigger
(§2 below quantifies why — gitlab-foss has ~6.4× mastodon's file count but
~12.6× its `related_to_owner` call volume and a visit-count median 12× higher).
So the issue's "likely explains the dominance" is the right prediction for the
corpus that actually stresses the LSP budget (gitlab-lib is the scale the
#94 issue's "~100 ms @ 4,675 files" budget claim is about), but a fix aimed
only at the BFS gate would under-deliver at mastodon-sized projects, where the
fold walk itself is the bigger piece.

Also worth noting mechanically: these 4b/`stage2` shares (57.5 % / 65.7 %) are
HIGHER than the #92 note's old 43.1 %/47.1 % of the pre-#92 `build_project`,
because #92 already moved passes 1/1b/1c/1d/1e/C5a/2/4a-walk out of the merge
and into stage 1's harvest — 4b's ABSOLUTE cost didn't change, but the
denominator it's a share of got smaller. This is the expected consequence of
#92 landing, not a new finding.

---

## 2. Call statistics (Q2)

| | mastodon/app | gitlab-foss/lib |
|---|---|---|
| `overridden_in_project` calls | 177 (exact, all 6 runs) | 903 (exact, all 6 runs) |
| `related_to_owner` calls (median, range) | 4 986 (4 984–4 989) | 19 739 (19 725–19 753) |
| distinct `(candidate, owner)` pairs (median, range) | 4 681 (4 678–4 683) | 17 512 (17 494–17 523) |
| distinct candidates (median, range) | 218 (217–218) | 1 400 (1 394–1 407) |
| BFS visit count: median / p95 / max | 1 / 3 / 24 | 12 / 13 / 13 |
| BFS visit count: mean, Σ (one representative run) | 1.61, Σ=8 022 (n=4 984) | 7.34, Σ=144 771 (n=19 725) |
| `OVERRIDE_ANCESTOR_WALK_LIMIT` (100) hits | **0** | **0** |

`overridden_in_project`'s call count is EXACT and stable across runs (177 /
903) because it is gated by `resolve_fold_key`'s per-key memo (:1494–1509) —
called at most once per `(owner, method, kind)` key whose fold produced
`Some`. `related_to_owner`'s counts jitter by a few (±5 of ~4 986, ±25 of
~19 739) between runs of the SAME binary on the SAME input: `definers`'
per-key `Vec<String>` order comes from `HashMap` iteration (documented as
already-nondeterministic-per-process in the #92 note, §3.4), and
`overridden_in_project`'s `candidates.iter().any(...)` (:1676–1678)
short-circuits on the first related candidate — so which candidates get
skipped by the early exit varies slightly run to run. This is the same
already-known-inert nondeterminism the #92 note catalogued; it does not reach
diagnostics (every read of `definers` is `.any(...)`) and doesn't materially
move these counts.

**What decides the memoization direction.** The key ratio is
`distinct_pairs / distinct_candidates`: **21.5× at mastodon, 12.5× at
gitlab-foss** — i.e. each definer-candidate is tested against roughly a dozen
to two dozen DIFFERENT owners over the course of one merge (not necessarily
for the same method name — `related_to_owner` doesn't take a method
argument, so the SAME `(candidate, owner)` ancestry fact gets re-derived every
time ANY two methods happen to share overlapping definer sets across those two
classes). A **per-`(candidate, owner)`-pair memo** only collapses
`related_calls` down to `distinct_pairs` — a **6.1 % (mastodon) / 11.3 %
(gitlab)** reduction in walk count, because most pairs are queried close to
exactly once. A **per-candidate transitive-closure precompute** collapses the
walk count down to `distinct_candidates` — a **~21.5× / ~12.5×** reduction in
the number of BFS walks executed, because it answers every subsequent
`(candidate, *)` query from one cached `HashSet<String>` in O(1). The caveat:
a closure computation must enumerate the candidate's FULL reachable ancestor
set rather than stopping at the first match, so its per-computation cost is
somewhat above the mean EARLY-EXIT walk cost recorded above (1.61 / 7.34
visited nodes is an early-exit-biased mean, not a full-closure size) — but
even a generous 2–3× per-computation markup is dwarfed by a 12–22× reuse
factor, so the closure direction should still net several-fold fewer total
ancestor-name expansions than either "no memo" or a pair-keyed memo. This is
the quantitative case for the issue's "or precompute each definer's
transitive ancestor closure once" option over the "memoize per pair" option.

---

## 3. Soundness of memoization (Q3)

**What state `related_to_owner`/`override_ancestor_names` read — exactly, by
line.**

* `related_to_owner(candidate, owner)` (`:1684–1706`) reads no `self` field
  directly. It reads only through `self.override_ancestor_names(candidate)`
  (`:1685`, seeding the queue) and `self.override_ancestor_names(&current)`
  (`:1701`, expanding each visited node); the `current == owner` test
  (`:1691`) is a local `String` comparison.
* `override_ancestor_names(class)` (`:1391–1407`) reads
  `self.override_classes.get(class)` (`:1392`) and, per include/superclass,
  calls `self.resolve_override_ancestor`.
* `resolve_override_ancestor(subclass, raw)` (`:1415–1431`) reads only
  `self.override_classes.contains_key(&candidate)` (`:1426`).

So the **entire transitive read surface is exactly one field**:
`SourceIndex::override_classes: HashMap<String, OverrideClass>` (`:350`).
`overridden_in_project` (`:1672–1679`) additionally reads
`SourceIndex::definers: HashMap<(String, DefKind), Vec<String>>` (`:377`)
directly, at `:1673`.

**Both fields are frozen before M3, and the freeze is structural, not
observational.**

* `override_classes` has exactly one production write site:
  `ingest_override_class` (`:1434–1458`), which mutates it at `:1442`
  (`self.override_classes.entry(qualified.to_string()).or_default()`). Its
  ONLY call site is `merge`'s M1 replay loop (`:774–784`), which completes
  before the M2 barrier (`:814` onward) and M3 (`:914` onward, where
  `compute_literal_returns` lives). The one other textual match,
  `idx.override_classes.insert(...)` at `:2749`, is inside
  `#[test] fn nearest_ancestor_returns_unknown_visibility_for_methods_only_entry`
  — a unit test that hand-seeds a throwaway index for one assertion and is
  never reachable from `merge`.
* `definers` has exactly one production write: `idx.definers =
  invert_definers(&defs);` at `:946`, the line immediately before the SOLE
  call to `compute_literal_returns` at `:947`.
* Neither field is wrapped in `RefCell`/`Cell`/`Mutex` — both are plain
  `HashMap`s (`:350`, `:377`) — and every function in the read chain
  (`compute_literal_returns`, `resolve_fold_key`, `fold_key_sites`,
  `fold_expr`, `overridden_in_project`, `related_to_owner`,
  `resolve_instance_owner`, `owner_defines`, `override_ancestor_names`,
  `resolve_override_ancestor`) takes `&self`. Given that, the Rust borrow
  checker makes "neither field can be mutated during
  `compute_literal_returns`" a COMPILE-TIME guarantee for the current code
  shape, not merely a property this probe happened not to violate: no `&mut
  self.override_classes` / `&mut self.definers` can be taken while the `&self`
  borrow used to reach any of these functions is alive, and stage 2 is a
  single synchronous call (no concurrent mutator thread).

**Consequence.** For the full duration of one `compute_literal_returns` call
(one `merge`/`build_project` invocation), `related_to_owner(candidate,
owner)` is a pure function of `(candidate, owner, self.override_classes)` over
FROZEN state. A memo `HashMap<(String, String), bool>` scoped to that one call
(or a field reset at the top of `merge`) can never observe two different
answers for the same pair within a merge.

**The cap is deterministic per pair, so caching a cap-truncated `false` is
sound.** `override_ancestor_names` never iterates a `HashMap` — it walks
`entry.includes: Vec<String>` (populated in SOURCE order by
`ingest_override_class`'s append-with-dedup) and `entry.superclass:
Option<String>` — so, given frozen `override_classes`, the sequence of
`queue`/`seen`/`visited` values for a given `(candidate, owner)` call is fully
determined; whether `visited > OVERRIDE_ANCESTOR_WALK_LIMIT` (`:1698` /
equivalent) fires is therefore also deterministic for that pair. Re-running
the identical walk reproduces the identical truncation and the identical
`false`, so memoizing a cap-hit is sound by the same "same inputs ⇒ same
output" argument — independent of the fact that §2 shows the cap is never
actually exercised on either measured corpus (0/0 across all 12 runs, well
below the p95=13/max=24 observed depths against a cap of 100).

**LSP per-dispatch interaction: none, by construction.** `SourceIndex::merge`
always starts from `SourceIndex::default()` (`:760`) — a brand-new empty
index — and `SourceIndex::build_project` (`:520–524`) is literally
"harvest every file, then `merge`". The LSP's cross-file overlay calls
`SourceIndex::build_project(&refs, index)` fresh on every dispatch
(`crates/rigor-cli/src/lsp.rs:787`, `:2180`) — it never reuses or mutates a
persisted `SourceIndex` across dispatches; the doc comment at `lsp.rs:1473`
states the discipline explicitly ("Re-harvest the WHOLE overlay against
`index`"). A memo scoped to one `compute_literal_returns` call — or a
`SourceIndex` field that is reset at the top of `merge`, never a
process-global/static — is therefore automatically re-created empty on every
dispatch and can never straddle two different overlay states. The one thing
to get right in the implementation: the memo must NOT be hoisted into
anything that outlives a single `merge()` call (a `lazy_static`/`OnceLock`
cache keyed only by `(candidate, owner)` names, for instance, WOULD leak a
stale answer across dispatches once the project's `override_classes` changes
between keystrokes — that shape must be avoided).

---

## 4. Cheap-wins inventory, no memo (Q4)

**`VecDeque` instead of `Vec::remove(0)`.** `Vec::remove(0)` is O(queue
length). §2's visit-count distribution — median 1 / p95 3 / max 24 at
mastodon, median 12 / p95 13 / max 13 at gitlab-foss, against a cap of 100
NEVER approached — means the actual `queue` this workload builds is small: a
handful to a few dozen entries at the observed worst case, since each
expansion appends at most `includes.len() + 1` new entries. At those depths
the O(n) shift is a few dozen pointer-sized moves, not a measurable cost
relative to the ~65–70 % of 4b that timing already attributes to
`overridden_in_project`. **Verdict: correctness-preserving and worth doing
(protects against a future pathological override graph nearer the cap), but
on THESE numbers it would not move total 4b time measurably** — it is cheap
to implement, not high-payoff on this workload.

**Avoiding the per-call `String` allocations.** This is the better-targeted
half of the "cheap wins" pair. Every visited node triggers one
`override_ancestor_names` call, which allocates a fresh `Vec<String>` and, per
include/superclass, calls `resolve_override_ancestor` — itself allocating a
`Vec<&str>` via `subclass.split("::").collect()` (`:1417`) plus one
`format!("{}::{}", …)` `String` per enclosing-scope prefix tried (`:1424`).
Counting "node expansions" as `visit_sum + related_calls` (one
`override_ancestor_names(candidate)` call up front per `related_to_owner`
invocation, plus one per subsequently-visited node): **≈13,008 expansions at
mastodon (8 022 + 4 986), ≈164,510 at gitlab-foss/lib (144 771 + 19 739)** —
each doing at least one `Vec<String>` allocation plus several `split`/`format!`
allocations. That is a plausible, roughly-proportional estimate of what a
per-class cached ancestor-name list (a `HashMap<String, Vec<String>>`
populated once per qualified class instead of re-derived on every visit) would
buy inside `overridden_time` — but it does **not** touch the O(related_calls)
call-count redundancy §2 quantifies, so it is complementary to, not a
substitute for, one of the memoization directions. A per-class cached
ancestor-name list is also a natural building block FOR the closure
precompute (§2's recommendation): computing a candidate's transitive closure
still needs each visited class's own `override_ancestor_names`, and caching
that per-class list means the closure computation for a LATER candidate that
passes through an already-visited class gets it for free too.

---

## 5. Recommendation

**Precompute each definer-candidate's transitive project-ancestor closure
once per merge** — a `HashMap<String, HashSet<String>>` (or equivalent lazily
memoized structure) keyed by candidate name ONLY, populated by the same BFS
`related_to_owner` already runs but not stopping at the first match, then
answering `related_to_owner(candidate, owner)` as `closure(candidate)
.contains(owner)`. This is the direction Q2's numbers support: a 12–22×
reuse factor per candidate against a measured 6–11 % redundancy for a
pair-keyed memo. Ship it alongside, not instead of, the two mechanical fixes
from Q4 — `VecDeque::pop_front` (free, protects the pathological case) and a
per-class cached `override_ancestor_names` result (the natural memoization
substrate the closure computation itself needs, and the one that actually
targets the allocation cost Q4 quantifies). A pure per-`(candidate, owner)`
pair memo is not recommended as the primary lever on these numbers, though it
falls out for free once a candidate's closure is cached (every subsequent
`(candidate, *)` query is answered from the same cache).

Any implementation should keep #92's acceptance discipline: bit-identical
diagnostics on the 97-fixture harness and the 0 FP / 9 204-file sweep, and
`RIGOR_TIMING` before/after at 1 236 / 3 117 / 4 675 files as the issue's
acceptance criteria already say — this is a pure performance change with no
intended behaviour delta, and `override_classes`/`definers` being frozen
before M3 (§3) is exactly the property that makes "no intended delta" provable
rather than merely hoped for.

---

## Gates run

* `cargo test -p rigor-infer` — 273 passed, 0 failed, with the temporary
  instrumentation present (verifying it changed no production behaviour) AND
  again after full revert (the tree this note ships against).
* `cargo build --release -p rigor-cli` — clean, both with and after removing
  the instrumentation.
* The temporary instrumentation touched only
  `crates/rigor-infer/src/source_index.rs` (`git diff --stat` before revert:
  1 file, +102/−6) and was reverted with `git checkout --` before this
  commit — the tree is `master` plus this note file only.
* No `reference/rigor` work, no `REFERENCE_RIGOR_DIR` use — this is a
  port-side timing/counting investigation only, per the issue's constraints.
  (`harness/fp_audit.py --gaps --sweep` was attempted as an incidental sanity
  check and came back `INVALID` on this machine — the reference produced no
  parseable output on every corpus batch, an environment issue unrelated to
  this probe's changes, since the probe touches no reference/oracle path and
  the change was fully reverted before this note was written. Not chased
  further — it is not one of this probe's required gates.)
