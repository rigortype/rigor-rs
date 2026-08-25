# Upstream pin

rigor-rs is a port of the Ruby **Rigor** reference implementation
([`rigortype/rigor`](https://github.com/rigortype/rigor)). The reference is the
parity oracle ([ADR-0002](docs/adr/0002-diagnostic-set-parity.md)): for a given
input, rigor-rs's `(rule id, location)` diagnostic set must match the reference's
(message wording may improve; the set must match).

To make that comparison **reproducible**, the reference is pinned as a git
submodule rather than tracked against a drifting local checkout.

## Pinned version

| | |
|---|---|
| Upstream repo | `git@github.com:rigortype/rigor.git` |
| Submodule path | `reference/rigor` |
| **Pinned ref** | **`v0.3.4`** (tag, released 2026-08-21) |
| Commit | `b10bd5df` |

> `v0.3.4` **does not move rbs** (still 4.1.1, `vendor_rbs.py --check` exact on
> all 174 `.rbs`) and **does not move the `data/` overlay** (`diff -r` clean in
> both directions; the only "Only in reference" lines are the two deliberate
> omissions, `README.md` and `prism/`). Both standing exception tables stay
> EMPTY. So for the first time since the ritual was written, steps 3's two
> re-sync halves are both no-ops — and the bump is still **not** free: the
> `v0.3.2 → v0.3.4` step (151 commits, two releases) opened **48 false
> positives**, all one root cause, which the fixture harness could not see and
> only the sweep caught. See [note](docs/notes/20260823-repin-v034.md).
>
> Most of the 151 commits are the two new OPT-IN subsystems — the effect system
> (`rigor effects`, ADR-103) and `rigor unused` (ADR-102) — which are new
> commands, not new `check` behaviour, and are out of the port's parity scope
> ([ADR-0002](docs/adr/0002-diagnostic-set-parity.md) is about `rigor check`'s
> diagnostic set). The `check`-visible delta is ten engine commits, and their
> net on the FIXTURE corpus was **one** new reference diagnostic.

> **The pin HOLDS at `v0.3.4`** as of 2026-08-25. Upstream master is 64 commits
> ahead with no tag past `v0.3.4`, and the surveyed delta is **0 diagnostics
> added / 0 dropped on 9204 files** — nearly all of it the two opt-in
> subsystems, which are new commands and not new `check` behaviour. Its rbs bump
> (4.1.1 → 4.1.3) is a library-code release: `diff -rq` over both gems' entire
> `core/`, `stdlib/` and `sig/shims/` trees reports zero differing files, and the
> reference self-diff under both versions is 0/0.
> [survey](docs/notes/20260825-upstream-survey-v034-master.md).

> Previous pin `v0.3.2` followed **rbs 4.1.1**, and the vendored RBS moved with
> it **in the same commit** (see the independent-pin note below — the two must
> match) — though only its version string: `vendor_rbs.py --check` against the
> 4.1.1 gem reports an exact match on all 174 `.rbs`, so `core/` + `stdlib/` are
> byte-identical to 4.1.0 and no signature resolution shifts. The `v0.3.1 →
> v0.3.2` bump (2026-08-09) measured **0 FP / 9204 files, gaps 1125 → 841**;
> the −284 is upstream retracting its own possible-nil FPs (#297), not port
> work. It also emptied BOTH standing exception tables:
> `UNBUILDABLE_DEFINITIONS` 12 → 0 (#299/#300/#301 fixed every collision it
> mirrored) and the divergence registry 1 → 0 (#237's fix is an ancestor).
> [note](docs/notes/20260809-repin-v032.md).

> **The `data/` overlay is the half that actually moves on a pin bump**, and
> `vendor_rbs.py` does not touch it. `v0.3.1 → v0.3.2` left the rbs tree
> identical while rewriting five overlay files and adding two directories, one
> of them load-bearing (`vendored_gem_sigs/racc/`, without which
> `Nokogiri::CSS::Parser`'s declared superclass does not resolve). Re-sync it by
> `diff -r`, not by copying the files a survey happened to name; `PROVENANCE.md`
> carries the executable recipe. The sharp edge: #300/#301 make the reference's
> `vendored_gem_sigs/{bundler,rubygems}/` DEPEND on the `rbs` gem's
> `sig/shims/`, which the reference loads on every run and rigor-rs does not
> vendor — syncing only `data/` left two live false positives
> (`Bundler.definition`, `Bundler.default_lockfile`) that the **9204-file sweep
> does not reach**. `overlay/rbs_shims/` closes them; a green sweep was not
> evidence there.

> The `v0.3.0 → v0.3.1` bump (41 commits, 2026-07-31) contributed **zero**
> upstream logic delta on 7131 corpus files; everything measurable came from the
> rbs version (4.0.3 → 4.1.0), and it closed 2 more gaps than it opened. Two
> port-side fixes were needed for 4.1.0's rewritten core signatures: bounded
> method type parameters (`[I < _ToInt] (I index)`) now resolve to their bound,
> and `-> instance` on an instance method resolves to the receiver's class.
> [note](docs/notes/20260731-upstream-pin-v031-rbs41.md). The pin then HELD at
> `v0.3.1` from 2026-08-07 to 2026-08-09 waiting for a tag, over a surveyed +150
> commits worth **2 diagnostics on 9204 files** — rigor-rs was silent at both,
> including the `c7f28da1`/#271 add, which was a new upstream FP. **That caveat
> is now moot: #298 fixed #271 before `v0.3.2`.**
> [survey](docs/notes/20260807-upstream-survey-v031-to-master.md).

> Previous pin: `v0.3.1` (`c39e6675`). Before it, `v0.3.0` (`5802c990`), and
> before that `7a69f142` (the v0.3.0 RC);
> that 42-commit bump was behaviour-neutral in every shipped profile, its only
> delta being 4 reference-only diagnostics traced by bisect to `861b08b9`
> (ADR-93 auto-wire of `rigor-rbs-inline` — inline `#:` annotations are now read
> by default, so the port's inline-RBS deferral
> [ADR-0035](docs/adr/0035-inline-rbs-deferred.md) becomes measurable as
> coverage gaps). Upstream's own inference additions stay off by default:
> `parameter_inference:` (ADR-67 WD6) is opt-in — upstream **declined** to flip
> it (#205) — and `static.value-use.void` is bleeding-edge.
> [note](docs/notes/20260731-upstream-bump-7a69f142-v030.md).

> **The local Ruby must resolve the rbs the pin bundles** (`rbs 4.1.1` today).
> The harness invokes the reference with a plain `ruby -I`, so RubyGems serves
> the highest installed version; running the oracle against a different rbs than
> upstream ships silently compares against different core signatures. `gem list
> rbs` should show 4.1.1 as the newest.

The differential harness (`harness/run.rb`, `harness/snapshot.rb`) defaults
`REFERENCE_RIGOR_DIR` to this submodule (`harness/lib.rb`). The reference-free
snapshot gate (`harness/run_snapshot.rb`, the CI `parity` job) never touches it —
it replays the pinned snapshots under `harness/snapshots/`, which were generated
from this exact reference version.

Note: the vendored RBS (`crates/rigor-index/vendor/rbs`, **rbs-4.1.1**) is a
**separate pin** with its own `PROVENANCE.md` — but it is not independent in
practice: it must carry the same rbs version the reference bundles, or the two
sides read different core signatures. Upstream bundled rbs-4.0.3 from `v0.2.7`
through `v0.3.0`, moved to 4.1.0 in `v0.3.1` and to 4.1.1 in `v0.3.2`, so the
vendored tree moved with it (the 4.1.1 step changed the version string only —
`core/` + `stdlib/` are byte-identical to 4.1.0).
`harness/vendor_rbs.py --check` verifies the tree still matches its source gem
exactly. It does NOT verify `overlay/`, which tracks the reference pin instead —
see step 3 below.

## First-time setup

```sh
git submodule update --init reference/rigor
# The reference is plain Ruby run in place — no build step:
ruby -I reference/rigor/lib reference/rigor/exe/rigor --version   # -> rigor 0.3.2
```

## Oracle invocation hazard: stale-gem plugin hijack (issue rigortype/rigor#194)

Since the ADR-93 auto-wire (upstream `861b08b9`), the reference `require`s
`rigor-rbs-inline` at startup. A bare `ruby -I reference/rigor/lib` invocation
lets RubyGems resolve that require against an INSTALLED `rigortype` gem's
bundled plugin copy — a stale version without the annotation gate synthesizes
untyped skeletons for every source file and silently poisons every diagnostic
comparison (measured: three phantom "regressions", one phantom feature). Every
oracle invocation MUST therefore pin the checkout's own plugin:

```sh
ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib \
  reference/rigor/exe/rigor check <path>
```

`harness/lib.rb` and `harness/fp_audit.py` do this unconditionally. Ad-hoc
probes must too — and since the pin is **post-**auto-wire (`v0.3.0` onward),
this is now load-bearing rather than defensive.

## Oracle invocation hazard 2: cross-checkout result-cache hits

The reference's persistent result cache (`.rigor/cache`, on by default,
stat-mode validation, keyed by cwd) is NOT scoped to the reference version
that produced it. Two invocations sharing a cwd — e.g. a pin-vs-tip self-diff —
silently cross-serve each other's cached diagnostics, making a "0/0" diff
meaningless (measured 2026-07-19: pin 0.3 s vs tip 26.8 s on gitlab-foss lib
was the tell; symmetric 24 s/24 s once eliminated). Any comparison of two
reference checkouts MUST pass `--no-cache` and run each invocation from its
own fresh temp cwd.

Harness status (audited 2026-07-19): `harness/lib.rb` (`run_reference`, used by
run.rb/snapshot.rb) and `harness/run_corpus.rb` use a fresh per-invocation
`Dir.mktmpdir` cwd — safe. `harness/fp_audit.py` previously ran from a shared
persistent `cwd="/tmp"` (a stale `/tmp/.rigor/cache` could survive a pin bump
and poison a re-baseline); it now uses a fresh temp cwd AND `--no-cache`.

## Oracle invocation hazard 3: `REFERENCE_RIGOR_DIR` pointed off the pin

`harness/lib.rb` lets `REFERENCE_RIGOR_DIR` override the oracle. Pointing it at
a *working* rigor checkout (e.g. `/Users/megurine/repo/ruby/rigor`) silently
compares against a DIFFERENT version: measured 2026-07-25 that tree was
`2fd08368`, **56 commits ahead of the pin**, and `harness/run.rb` reported
**213 unregistered FPs** where the pinned submodule reports **0**. The oracle
is the PIN, not any local checkout. If a linked git worktree has an unpopulated
submodule, run `git submodule update --init reference/rigor` there — never
redirect the variable to another tree. (Failure is loud, not silent: it shows
up as a red gate, so no past green result is suspect — but it can send you
chasing a non-bug.)

## Bumping the pin (following upstream)

> **Step 0 — read the release's `Fixed` section as an FP LIST, and diff it
> against our own open upstream issues.** Every "no longer reports X" bullet is
> a place the port may now be strictly LOUDER than the oracle, with no code
> change on our side: upstream retracted a diagnostic, we did not. A retraction
> is invisible to step 5's snapshot diff — rigor-rs never emitted it on a
> fixture either — so only step 7's sweep can see it, and only if a corpus file
> happens to exercise the shape. This is not hypothetical: the `v0.3.2 →
> v0.3.4` bump landed ALL FIVE defect reports of our own feedback batch 3 at
> once and opened **50 false positives**, three of the five being reference FPs
> this project had adjudicated and deliberately left matched. **A filed upstream
> issue is a scheduled port obligation**, and the bump that lands the fix is
> when it comes due. Expect step 7 to send you back to step 4.
> [note](docs/notes/20260823-repin-v034.md).

1. Fetch + check out the new tag inside the submodule:
   ```sh
   cd reference/rigor && git fetch --tags && git checkout vX.Y.Z && cd -
   ```
2. Record the new gitlink in the superproject: `git add reference/rigor`.
3. **Check whether the release moved rbs** (`git -C reference/rigor show
   vX.Y.Z:Gemfile.lock | grep '^    rbs ('`). If it did, install that rbs so the
   local `ruby` resolves it, and re-vendor in the SAME commit:
   ```sh
   gem install rbs -v <version>
   python3 harness/vendor_rbs.py "$(gem env gemdir)/gems/rbs-<version>"
   ```
   Also re-sync `vendor/rbs/overlay/` from the reference's own `data/`
   (`data/core_overlay/`, `data/vendored_gem_sigs/` — see `PROVENANCE.md` for the
   `rsync` recipe; those track the reference PIN, not the rbs version, and
   `vendor_rbs.py` never touches them). **Drive this off `diff -r`, both
   directions, not off a list of files a survey named** — the `v0.3.1 → v0.3.2`
   bump moved five files and added two directories where the survey had named
   one, and one of the additions (`vendored_gem_sigs/racc/`) was load-bearing.
   Read every "Only in reference" line as a new file to consider, and re-check
   whether the reference's `data/` has started DEPENDING on a signature source
   rigor-rs does not vendor: `v0.3.2` made `bundler`/`rubygems` defer to the
   `rbs` gem's `sig/shims/`, which cost two false positives until
   `overlay/rbs_shims/` was added — and the sweep could not see either, because
   no corpus file called the methods. A hand probe of the moved surface against
   the oracle is part of this step, not optional.

   **Re-sync `crates/rigor-index/vendor/plugins/` too** — it is a third
   pin-tracking surface, and until 2026-08-25 this ritual did not mention it. The
   `activesupport-core-ext` copy was taken in 2026-06-26 from a LOCAL rigor
   checkout (hazard 3, applied to a file instead of an env var) and never moved
   again; by the `v0.3.4` pin the drift was **10 live false positives**. Copy the
   PINNED submodule's plugin `sig/` byte-for-byte and `shasum` both sides — see
   `crates/rigor-index/vendor/plugins/PROVENANCE.md`, which also records why
   upstream's `data/gem_overlay/` twin is the wrong file to copy. **Neither sweep
   tool can see this surface**: `fp_audit.py` and `gap_census.py` run both sides
   from a clean cwd, so no `.rigor.yml` is read and no plugin is ever loaded.
   Harness fixture 98 is the only gate on it.

   **Re-sync `crates/rigor-effects/vendor/effects/` too** — the THIRD
   pin-tracking tree this step re-syncs (after `vendor/rbs/overlay/` and
   `vendor/plugins/`), added 2026-08-26 with the effect catalogue
   ([ADR-0043](docs/adr/0043-effect-system-port-parity-model.md) slice 1):
   ```sh
   python3 harness/vendor_effects.py --check   # exit 1 on ANY byte difference
   python3 harness/vendor_effects.py           # re-vendor from the pinned submodule
   ```
   On a mismatch, re-vendor and **read the diff as a semantic change, not a
   copy** — step 3's standing advice applies verbatim here, because a catalogue
   re-audit that moves `IO#write` from `io` to `io.fs.write` changes every
   summary with no source change on either side. `retired:` gaining an entry and
   a `vocabulary:` bump are the two that can invalidate a committed
   `.rigor-effects.yml`; a `schema:` bump changes the row grammar. **The
   behavioural instrument cannot cover this**: `harness/effects_diff.py` grades
   **6 of the catalogue's 420 rows (1.4 %)**, so a drift in the other 414 is
   invisible to it — exactly the shape that made harness fixture 98 insufficient
   for the plugin RBS. `crates/rigor-effects/tests/upstream_data_specs.rs` (the
   ported upstream data specs) and the embedded-bytes digest assertion are the
   other two layers; see that tree's `PROVENANCE.md`.

   Then re-derive the classes whose DEFINITION the reference cannot build —
   `DEFAULT_LIBRARIES`, the vendored gem sigs and the host's own gem `sig/`
   directories collide, and a collision blinds the oracle on that whole class:
   ```sh
   ruby harness/unbuildable_classes.rb --check   # else: paste the printed table
   ```
   A `MISSING` line is a false positive rigor-rs will now emit; a `STALE` line
   means the collision is gone and rigor-rs should resume witnessing. Both need
   the `UNBUILDABLE_DEFINITIONS` table in `crates/rigor-index/src/rbs.rs` updated
   in the same commit.

   **Run this in the project's normal dev environment — the same one the gates
   run in.** Unlike everything else in this ritual, the answer is NOT a pure
   function of the pin: `RBS::EnvironmentLoader` prefers an installed gem's own
   `sig/` over `rbs`'s `stdlib/` copy, so *which* signatures collide depends on
   the host's gem set. Measured on the `v0.3.1` pin: with the `bigdecimal` gem
   absent, the same pinned reference builds `BigMath` and the set shrinks from 12
   to 11. (At `v0.3.2` the table is EMPTY — record `gem list bigdecimal` when it
   next changes, so a future reader can tell a pin fix from a gem removal.) So **a
   diff here can mean a GEM changed rather than upstream changing** — the script
   tags each colliding source `[env]` (host-installed gem) or `[pin]` (the
   reference's own `data/` tree, or the version-locked rbs gem); check those tags
   before concluding anything about upstream
   ([note](docs/notes/20260731-bigmath-ingestion-asymmetry.md)).
4. **Re-baseline the harness** against the new reference:
   ```sh
   ruby harness/snapshot.rb        # regenerate harness/snapshots/*.json
   ruby harness/run.rb             # live differential — must PASS, 0 FP
   ruby harness/run_snapshot.rb    # reference-free gate — must PASS
   ```
5. Review the snapshot diff: any newly-appearing reference diagnostics are
   candidate coverage to port (new rules / behaviours in `vX.Y.Z`); any that
   rigor-rs now emits but the reference dropped is a regression to fix or a
   divergence to register ([ADR-0011](docs/adr/0011-reference-oracle-exceptions.md)).
   Also walk `harness/divergence-registry.yml`: an entry whose upstream fix is now
   an ancestor MUST be removed (`git -C reference/rigor merge-base --is-ancestor
   <fix> HEAD`). Remove the ENTRY, not the fixture — once both sides agree the
   fixture becomes positive parity coverage, and fixture 79 is one of the few that
   exercises project-`sig/` behaviour, which neither sweep tool can see.
6. **Re-measure the ported reference-implementation constants** — they can move
   silently across releases. Currently: the shape-tier thresholds
   ([ADR-0039](docs/adr/0039-shape-typing-tier.md)) — `ARRAY_NEW_TUPLE_LIMIT`
   (grep `method_dispatcher.rb`, empirically probe `Array.new(n)`-slice
   possible-nil around the boundary) and, once ported, the other
   `constant_folding.rb` / `shape_dispatch.rb` limits.
7. **Re-run the corpus gates on the RELEASE binary** — `cargo build --offline
   --release -p rigor-cli`, then `python3 harness/fp_audit.py --gaps --sweep`
   (must be 0 FP) and `python3 harness/gap_census.py --sweep` for the new gap
   baseline. Record both in `harness/CORPUS.md`.
8. Update the tag/commit in this file (and `PROVENANCE.md` if rbs moved), record
   the numbers in `harness/CORPUS.md`, write a dated note in `docs/notes/`, and
   fold ONE ledger line into `docs/CURRENT_WORK.md`.
