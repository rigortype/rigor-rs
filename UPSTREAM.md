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
| **Pinned ref** | **`v0.3.1`** (tag, released 2026-07-29) |
| Commit | `c39e6675` |

> `v0.3.1` follows **rbs 4.1.0**, so the vendored RBS was bumped **in the same
> commit** (see the independent-pin note below — the two must match). The
> `v0.3.0 → v0.3.1` bump (41 commits, 2026-07-31) contributed **zero** upstream
> logic delta on 7131 corpus files; everything measurable came from the rbs
> version, and it closed 2 more gaps than it opened. Two port-side fixes were
> needed for 4.1.0's rewritten core signatures: bounded method type parameters
> (`[I < _ToInt] (I index)`) now resolve to their bound, and `-> instance` on an
> instance method resolves to the receiver's class.
> [note](docs/notes/20260731-upstream-pin-v031-rbs41.md).

> Previous pin: `v0.3.0` (`5802c990`). Before it, `7a69f142` (the v0.3.0 RC);
> that 42-commit bump was behaviour-neutral in every shipped profile, its only
> delta being 4 reference-only diagnostics traced by bisect to `861b08b9`
> (ADR-93 auto-wire of `rigor-rbs-inline` — inline `#:` annotations are now read
> by default, so the port's inline-RBS deferral
> [ADR-0035](docs/adr/0035-inline-rbs-deferred.md) becomes measurable as
> coverage gaps). Upstream's own inference additions stay off by default:
> `parameter_inference:` (ADR-67 WD6) is opt-in — upstream **declined** to flip
> it (#205) — and `static.value-use.void` is bleeding-edge.
> [note](docs/notes/20260731-upstream-bump-7a69f142-v030.md).

> **The local Ruby must resolve the rbs the pin bundles** (`rbs 4.1.0` today).
> The harness invokes the reference with a plain `ruby -I`, so RubyGems serves
> the highest installed version; running the oracle against a different rbs than
> upstream ships silently compares against different core signatures. `gem list
> rbs` should show 4.1.0 as the newest.

The differential harness (`harness/run.rb`, `harness/snapshot.rb`) defaults
`REFERENCE_RIGOR_DIR` to this submodule (`harness/lib.rb`). The reference-free
snapshot gate (`harness/run_snapshot.rb`, the CI `parity` job) never touches it —
it replays the pinned snapshots under `harness/snapshots/`, which were generated
from this exact reference version.

Note: the vendored RBS (`crates/rigor-index/vendor/rbs`, **rbs-4.1.0**) is a
**separate pin** with its own `PROVENANCE.md` — but it is not independent in
practice: it must carry the same rbs version the reference bundles, or the two
sides read different core signatures. Upstream bundled rbs-4.0.3 from `v0.2.7`
through `v0.3.0` and moved to 4.1.0 in `v0.3.1`, so the vendored tree moved with
it. `harness/vendor_rbs.py --check` verifies the tree still matches its source
gem exactly.

## First-time setup

```sh
git submodule update --init reference/rigor
# The reference is plain Ruby run in place — no build step:
ruby -I reference/rigor/lib reference/rigor/exe/rigor --version   # -> rigor 0.3.1
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
   (`data/core_overlay/`, `data/vendored_gem_sigs/` — see `PROVENANCE.md`; those
   track the reference pin, not the rbs version).
   Then re-derive the classes whose DEFINITION the reference cannot build —
   `DEFAULT_LIBRARIES`, the vendored gem sigs and the host's own gem `sig/`
   directories collide, and a collision blinds the oracle on that whole class:
   ```sh
   ruby harness/unbuildable_classes.rb --check   # else: paste the printed table
   ```
   A `MISSING` line is a false positive rigor-rs will now emit; a `STALE` line
   means upstream fixed a collision and rigor-rs should resume witnessing. Both
   need the `UNBUILDABLE_DEFINITIONS` table in `crates/rigor-index/src/rbs.rs`
   updated in the same commit
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
6. **Re-measure the ported reference-implementation constants** — they can move
   silently across releases. Currently: the shape-tier thresholds
   ([ADR-0039](docs/adr/0039-shape-typing-tier.md)) — `ARRAY_NEW_TUPLE_LIMIT`
   (grep `method_dispatcher.rb`, empirically probe `Array.new(n)`-slice
   possible-nil around the boundary) and, once ported, the other
   `constant_folding.rb` / `shape_dispatch.rb` limits.
7. Update the tag/commit in this file (and `PROVENANCE.md` if rbs moved), and
   note the bump in `docs/CURRENT_WORK.md`.
