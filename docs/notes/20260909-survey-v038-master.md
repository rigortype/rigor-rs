# Upstream survey `v0.3.8` → master `80b086a5` (219 commits) — 2026-09-09

Fifth run of the survey recipe ([pre-flight](20260731-v031-preflight-survey.md),
[+49](20260731-head-survey-and-set-op-folds.md),
[v0.3.1→master](20260807-upstream-survey-v031-to-master.md),
[v0.3.4→master](20260825-upstream-survey-v034-master.md)). Run the same day as the
`v0.3.8` re-pin, because upstream landed a ten-PR batch on top of it — including the
fixes for all three defects this port reported yesterday.

**Headline: HOLD the pin.** No tag past `v0.3.8`; 219 commits move **10 diagnostics on
9204 files** (2 added, 8 dropped) and the port is **silent on all ten**. rbs stays
4.2.0, and `data/` and `plugins/*/sig/` are byte-identical — so the next bump's step-3
re-sync is, so far, a no-op on every surface.

**But the corpus is blind to the one obligation that matters**, and that is this
survey's finding: the row that will become a false positive at the next bump does not
occur in 9204 files. It was found by taking upstream's `changelog.d/fixed/` fragments
as an FP list and hand-probing each one, exactly as `UPSTREAM.md` step 0 prescribes.

## Axis A — upstream logic: 10 rows on 9204 files

Reference vs reference (the pinned submodule at `ffb456b0` vs a worktree at
`origin/master`), both arms under the ambient rbs 4.2.0, `--no-cache` and a fresh temp
cwd per invocation, each checkout's own plugin path pinned (`UPSTREAM.md` hazards 1–2).

| corpus | files | pin | master | added | dropped |
|---|---|---|---|---|---|
| mastodon/app | 1236 | 440 | 440 | 0 | 0 |
| gitlab-foss/lib | 4676 | 1251 | 1252 | 2 | 1 |
| survey/mail | 874 | 7047 | 7040 | 0 | 7 |
| survey/Ruby | 192 | 36 | 36 | 0 | 0 |
| survey/dependabot-core | 1650 | 138807 | 138807 | 0 | 0 |
| survey/concurrent-ruby | 345 | 5802 | 5802 | 0 | 0 |
| survey/net-ssh | 180 | 150 | 150 | 0 | 0 |
| survey/haml/lib | 51 | 5 | 5 | 0 | 0 |

The 7 dropped `mail` rows are six `def.return-type-mismatch` in rdoc plus one
`call.undefined-method`, consistent with #856 (a method a class defines in its own
source is no longer typed from an ancestor-only signature). **rigor-rs emits none of
the 10** — checked row by row with the release binary — so nothing here is due.

## Axis B — rbs: unchanged

Master still bundles **rbs 4.2.0**. Nothing to re-vendor, and no signature-resolution
shift to survey.

## The performance regression is fixed, and it is worth a number

`rufo-0.18.2/lib/rufo/formatter.rb` — the file that did not finish in 25 minutes at
`v0.3.8` — checks in **3 s** at master (upstream #872 / PR #874, the mutual-recursion
memo). At corpus scale the `mail` arm of this very survey measured **2,162 s at the pin
against 39 s at master**, a 55× recovery. The 80-minute `--sweep` budget recorded in
`harness/CORPUS.md` and `UPSTREAM.md` step 7 lapses at the next bump.

## Step 0, done early: upstream's `Fixed` fragments as an FP list

`[Unreleased]` is empty at master — upstream keeps user-facing notes as
`changelog.d/*` fragments (36 of them). Every fixed fragment that could retract a
diagnostic was probed against BOTH references and the port:

| upstream fix | pin | master | rigor-rs | verdict |
|---|---|---|---|---|
| **#877 / PR #883** — a rooted `::RUBY_VERSION` guard now folds | reports | **silent** | **reports** | **FALSE POSITIVE DUE at the bump — [#122](https://github.com/rigortype/rigor-rs/issues/122)** |
| #878 / PR — a lambda literal's local writes now bind | silent | reports | silent | coverage gap, safe |
| #879 / PR — arity declines on an unenumerable receiver | reports | silent | silent | port already silent (its own gap) |
| #852 / #865 — block `next` / `break` value joins | reports | silent | silent | port already silent |
| #856 / PR #860 — inherited declaration precedence | reports ×2 | reports ×1 | silent | port already silent |

Three of those five are **our own reports from yesterday**, filed as
[#877](https://github.com/rigortype/rigor/issues/877),
[#878](https://github.com/rigortype/rigor/issues/878) and
[#879](https://github.com/rigortype/rigor/issues/879) and all fixed within the day —
so the standing lesson arrived faster than usual: **a filed upstream issue is a
scheduled port obligation**, and this time exactly one of the three comes back as work.
#122 carries the fix and the reason it must NOT be applied before the pin moves (the
port would then be louder than the pinned oracle, and fixture 103's f25 row pins that).

## What the next bump will be about

The unreleased work is dominated by **ADR-109's range carriers**: Integer ranges are
respelled in Ruby notation (`Integer[1..10]`, with `int<min, max>` deprecated behind a
new `dynamic.rbs-extended.deprecated-form` info diagnostic), Float gains a bounded
carrier with comparison narrowing and monotone `Math` folds, and `rand(1..6)` /
`clamp(range)` fold onto ranges. That is display text plus new coverage — the message
strings in our snapshots will move, which ADR-0002 permits and the fixture harness
grades by `(rule, line, column)`. Also `rigor check --fail-on=SEVERITY`, and a 40%
allocation cut (#819).
