# Upstream pin `v0.3.0 → v0.3.1` + vendored rbs `4.0.3 → 4.1.0` (2026-07-31)

The arc the [pre-flight survey](20260731-v031-preflight-survey.md) sized. Both
pins move in ONE commit, because `v0.3.1` follows rbs 4.1.0 and an oracle running
different core signatures than the port reads is not an oracle.

Outcome: **0 FP across 9153 corpus files, 2 net gaps CLOSED**, harness gates
green, one snapshot byte changed (a message string, not a diagnostic).

## What moved

| | from | to |
|---|---|---|
| `reference/rigor` | `v0.3.0` (`5802c990`) | **`v0.3.1`** (`c39e6675`) |
| `crates/rigor-index/vendor/rbs` | rbs-4.0.3 | **rbs-4.1.0** |
| `vendor/rbs/overlay/core_overlay/` | — | re-synced (`string_scanner.rbs`) |
| local gem env | rbs 4.0.3 newest | **rbs 4.1.0 installed** (see UPSTREAM.md) |

The overlay is NOT rbs-derived — it mirrors the reference's own
`data/core_overlay/` + `data/vendored_gem_sigs/`, so it tracks the *reference*
pin. `v0.3.1` edited `string_scanner.rbs`; that had to be copied across
separately, and the bump checklist now says so.

## `harness/vendor_rbs.py` — the recipe, executed instead of described

The vendored tree's recipe (whole `core/` ⊕ the `DEFAULT_LIBRARIES` closure over
each lib's `manifest.yaml`) lived only in `PROVENANCE.md` prose and had been
carried out by hand once, in 2026-06-26. Re-doing that by hand for 4.1.0 is how
a tree silently drifts, so the recipe is now a script that reads
`DEFAULT_LIBRARIES` out of `src/rbs.rs` (one source of truth) and carries
`overlay/` + `PROVENANCE.md` across untouched.

Its `--check` mode is the validation that matters: pointed at the **4.0.3** gem
it reproduces the committed 4.0.3 tree **byte-for-byte** (49 libs resolved,
`prism`/`rbs` skipped, 171 `.rbs`). The recipe was therefore proven before it
was used to write anything.

4.1.0 then yields 174 `.rbs`: `core/` gains `file_constants.rbs`,
`file_stat.rbs`, `rbs/ops.rbs`; 48 files change; **`tempfile` ships its first
`manifest.yaml`** — which is exactly why the closure must be recomputed rather
than copied from the previous file list.

## Two port-side regressions, both from rewritten core signatures

The survey predicted "2 lost `Hash` witnesses, accept as gaps". Both turned out
to be real port defects with clean fixes, so the bump **gained** coverage instead.

### 1. Bounded method type parameters ⇒ resolve to the bound

`Array#fetch`'s block overload is now `[I < _ToInt, T] (I index) { (I index) ->
T } -> (E | T)`. A bare type variable is retained as an opaque `Other` leaf,
which the acceptance walk admits — so that one overload accepted everything and
silenced the mismatch the other two report. `[1, 2, 3].fetch("x")` went silent
(harness fixture 66 line 20 + a unit test).

`method_overloads` now collects the method type's bounded parameters and
resolves such a variable to its declared upper bound; an UNbounded variable stays
opaque (genuinely unconstrained). Message is byte-exact with the oracle again —
and note the bound is visible in it: `expected int | _ToInt, got "x"`.

### 2. `-> instance` on an instance method ⇒ the RECEIVER's class

`Hash#compact: () -> ::Hash[K, V]` became `() -> instance`. The port tracked
`instance` only on the SINGLETON path (`Time.now -> instance`), so an instance
method's return collapsed to `None` ⇒ Dynamic, and everything chained off it
went silent — `{...}.compact.presence` stopped being witnessed (the 2 gitlab
`Hash` losses).

It now rides the existing call-site sentinel (`SELF_RETURN`, previously only for
`self` block returns), resolved at lookup to the class the accessor was QUERIED
with. Resolving to the receiver rather than the declaring class is what keeps it
right under inheritance (`my_hash.compact` is a `MyHash`). The two singleton
paths that read an instance method's return through an `extend` or a base class
DROP the sentinel to `None` — the receiver there is a class object the flat slot
cannot spell, and declining is always sound.

## Method correction to the pre-flight survey

The survey's Axis C mirror (`RIGOR_RBS_CORE_DIR` pointed at 4.1.0 `core/` + the
49 stdlib libs) omitted `overlay/`, which the loader also reads from an override
root — 539 classes vs the real embedded 660. The conclusion held (the same 2
`Hash` witnesses), but the measurement was confounded. **A `RIGOR_RBS_CORE_DIR`
mirror must include `overlay/`**; class-count parity against `rigor doctor` is
the cheap check that it does.

## Gates

| gate | result |
|---|---|
| `cargo test` | all green (index 67 → **69**: one test per fix) |
| `cargo clippy --all-targets -D warnings` (fresh target dir) | clean |
| `harness/run.rb` (live, v0.3.1 + rbs 4.1.0) | 76 fixtures, **0 FP**, 3 gaps |
| `harness/snapshot.rb` | 1 file changed — the `fetch` message string only |
| `harness/run_snapshot.rb` | PASS |

The 3 remaining gaps are the pre-existing `rule: null` parse diagnostics of
fixture 72 (documented in the fixture header), not this bump's.

## Sweep — 0 FP, gaps net −2

| corpus | files | gaps before | gaps after |
|---|---|---|---|
| mastodon `app` | 1236 | 49 | **48** |
| gitlab-foss `lib` | 4676 | 330 | **329** |
| survey `mail` | 874 | 540 | 540 |
| survey `Ruby` | 192 | 30 | 30 |
| survey `dependabot-core` | 1650 | 81 | 81 |
| survey `concurrent-ruby` | 345 | 87 | **86** |
| survey `net-ssh` | 180 | 75 | 75 |

**0 FP candidates everywhere.** The three closures are reference diagnostics that
rbs 4.1.0 itself retires (an `argument-type-mismatch` on mastodon, a
`possible-nil-receiver` on gitlab, a `wrong-arity` on concurrent-ruby) — the port
was silent at all of them, so they leave as gaps. gitlab lands 1 BELOW its
pre-bump count because the 2 `Hash` witnesses came back.

`ARRAY_NEW_TUPLE_LIMIT` re-measured at `v0.3.1`: still 16.

## Left open

- **`-> self` on an instance method** is still unmapped (only block returns
  resolve it). The sentinel machinery now in place makes it a small change, but
  it widens typing beyond what this bump broke, so it wants its own measured
  slice.
- Upstream regenerated `data/builtins/ruby_core/*.yml` for 4.1.0 (the offline
  purity catalog `constant_folding.rb` consults). Measured inert on our corpora
  — the whole `v0.3.0 → v0.3.1` upstream-logic delta was zero — but the port has
  no equivalent catalog, so this is a place a future divergence can hide.
