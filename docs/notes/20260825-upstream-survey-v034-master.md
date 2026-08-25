# Upstream survey `v0.3.4` → `4dda960f` (64 commits, + rbs 4.1.3) — 2026-08-25

Fourth run of the survey recipe
([pre-flight](20260731-v031-preflight-survey.md), [+49](20260731-head-survey-and-set-op-folds.md),
[v0.3.1→master](20260807-upstream-survey-v031-to-master.md)). Still **no tag past
`v0.3.4`**; upstream master is 64 commits ahead and its tip bumps rbs
4.1.1 → 4.1.3.

Headline: **64 commits and an rbs bump move ZERO diagnostics across 9204 files.
Hold the pin.** Nearly all of the delta is the two new opt-in subsystems, which
are new commands rather than new `check` behaviour.

**But the survey found two live rigor-rs defects that had nothing to do with the
bump**, and one of them is 10 false positives that no standing gate could see.
That is the finding worth the session; the survey itself is a one-line answer.

## Axis A — upstream logic: 0 diagnostics on 9204 files

`v0.3.4` vs `origin/master` (a worktree of the pinned submodule — the pin itself
never moved), both arms under the ambient rbs, `--no-cache` and a fresh temp cwd
per invocation, each checkout's own plugin path pinned (`UPSTREAM.md` hazards
1–2). Mechanised this time as `harness/effects_diff.py`'s sibling script (kept in
the session scratchpad; the recipe is the note above).

| corpus | files | `v0.3.4` | master |
|---|---|---|---|
| mastodon `app` | 1236 | 436 | 436 |
| gitlab-foss `lib` | 4676 | 1265 | 1265 |
| survey `mail` | 874 | 7091 | 7091 |
| survey `Ruby` | 192 | 35 | 35 |
| survey `dependabot-core` | 1650 | 138818 | 138818 |
| survey `concurrent-ruby` | 345 | 5771 | 5771 |
| survey `net-ssh` | 180 | 150 | 150 |
| survey `haml/lib` | 51 | 5 | 5 |

**+0 / −0.** Not one diagnostic moves.

That is not luck either: of the 64 commits, ~50 are `rigor effects` / `rigor
unused` work — new opt-in COMMANDS behind an `effects:` block, which upstream
states leave `rigor check` byte-identical. The check-visible remainder is small
and every piece of it is gated on something the sweep does not turn on
(`#437` needs a plugin; the `documentation_url` fix touches a JSON field, not the
diagnostic set).

## Axis B — rbs 4.1.1 → 4.1.3: nothing, and structurally so

Reference self-diff, same checkout under both rbs versions, selected by a
**load-path override** (`ruby -I <gem>/lib -I <ext dir>`) rather than
`GEM_HOME`/`GEM_PATH` — the mechanism the previous survey established, because
the env-var form also changes which *other* gems resolve and silently cost that
survey the whole dependabot-core corpus.

| | added | dropped |
|---|---|---|
| `v0.3.4`, 8 corpora / 9204 files | **0** | **0** |

And the structural argument that says it could not have been otherwise:
`diff -rq` over the two gems' **entire** `core/`, `stdlib/` and `sig/shims/`
trees reports **zero** differing files. 4.1.3 is a library-code release, not a
signature release — the same shape as 4.1.0 → 4.1.1.

> rbs 4.1.3 was installed for this measurement and **uninstalled afterwards**.
> `UPSTREAM.md` requires the local Ruby to resolve the rbs the PIN bundles
> (4.1.1); leaving a newer one installed makes a plain `ruby -I` oracle
> invocation silently read different core signatures. `gem list rbs` is back to
> `4.1.1` as the newest.

## The two defects the survey turned up

Neither is a bump regression. Both were live on master-of-rigor-rs before this
session, and both are in surfaces the standing gates structurally cannot reach.

### 1. The vendored plugin RBS had drifted — 10 false positives

`crates/rigor-index/vendor/plugins/activesupport-core-ext/` is a **third
pin-tracking surface**, alongside the vendored rbs tree and the `data/` overlay.
Unlike those two it was in no ritual step and had no gate. Its `PROVENANCE.md`
recorded the source as
`/Users/megurine/repo/ruby/rigor/plugins/…` — a **local working checkout**, which
is `UPSTREAM.md` hazard 3 applied to a file instead of an environment variable —
vendored 2026-06-26 and never moved again while upstream's copy grew from 478 to
867 lines.

Measured at the `v0.3.4` pin with the plugin enabled on both sides, ten selectors
the reference resolves and rigor-rs witnessed absent:

```
titlecase  dasherize  upcase_first  remove  remove!
in?  Time#advance  Time#all_day  Date#advance  Date#all_day
```

All ten close on a verbatim re-sync from the PIN. **Neither sweep tool can see
this surface**: `fp_audit.py` and `gap_census.py` run both sides from a clean
temp cwd, so no `.rigor.yml` is read and no plugin is ever loaded — the same
blind spot that hid the `sig/shims` FPs at the `v0.3.2` bump, in a different
file. The whole of the plugin surface's coverage was fixture 17, which exercises
three lines.

Fixture 98 now grades it, `PROVENANCE.md` records the pin as the source with a
`shasum`, and `UPSTREAM.md` step 3 re-syncs it.

> **Upstream keeps a second copy of this surface and it is the WRONG one to
> vendor.** `data/gem_overlay/activesupport/core_ext.rbs` (ADR-72) is the
> auto-applied twin, gated on the gem being in `Gemfile.lock`; the plugin's own
> `sig/` is the authoring home and is 390 lines longer. rigor-rs implements the
> plugin mechanism, so the plugin's `sig/` is the source. Copying the overlay
> would have silently vendored a weaker surface and looked like a successful
> re-sync.

### 2. Every `documentation_url` rigor-rs has ever emitted 404s

The base named a `main` branch; the rigor repository's default branch is
`master` and origin has never had a `main`. On every `--format json` diagnostic,
every `rigor explain`, and `rigor docs`. Upstream hit the identical defect and
fixed it (#438, ADR-65 amendment) by moving the base off `github.com/…/blob/<ref>/…`
entirely: a git ref inside a frozen public contract is a mutable component, a
branch name rots on a rename, and a tag resolves only once pushed — so every
build between a version bump and its tag would 404 again. The published docs host
carries no ref and renders the same page with the same `<a id="rule-…">` anchors,
so the fragment half of the contract is untouched.

rigor-rs takes the same base. This is a **non-parity field** — the harness keys
on `(rule, line, column)` and never compared it — so it is ported now rather than
waiting for a pin bump; matching a broken URL has no value.

## Verdict

**Hold the pin at `v0.3.4`.** Re-survey when a tag lands, or sooner if the
effects work starts landing `check`-visible changes (it has not yet). The next
bump's step 0 now has three surfaces to re-sync, not two.
