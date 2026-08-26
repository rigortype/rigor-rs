# S114 — the declared-lane self-defense was too NARROW: the gate first, then the fix (2026-08-26)

Closes issue #114. Branch `claude/s114-declared-selfdefense`, cut from `3621b93`
(master). Three deliverables, landed in this order:

1. **Two corpus projects** — `harness/effects-corpus/09_declared_envelopes` and
   `10_declared_plugins` — that reproduce the slice-6 probe's § 6a / § 6b
   measurements. They **FAIL on the master binary with 1 and 3
   DECLARED-MISMATCH**, the fatal verdict, on correct code. No gate in the repo
   saw either before them.
2. **The fix** — `carries_effect_annotations` is widened from two of the four
   declared-lane producers to all four
   (`crates/rigor-cli/src/effects/mod.rs`). The port emits LESS, which is the
   sound direction. The declared lane is NOT implemented and no suppression is
   narrowed anywhere.
3. **The stale refuted text** — `harness/effects-corpus/04_declared/sig/declared.rbs`
   and ADR-0043's "Open at accepted" both still described the `declared:`
   question as open. It was solved on 2026-08-26 (it is the CALLER's lane); both
   now state the solved semantics and cite
   [the slice-1 catalogue probe § 7](20260826-effects-s1-catalogue-probe.md).

Everything measured against the PINNED submodule at `b10bd5df` (v0.3.4),
populated into this worktree from the parent checkout (never the network, never
`REFERENCE_RIGOR_DIR`; the populated tree was diffed against the parent
checkout's and is identical bar `.git` and an untracked `vendor/`), with
`.rigor/cache` cleared either side of every oracle run — which
`harness/effects_diff.py` does itself.

---

## 1. The defect, restated

Slice 2 shipped a self-defense: a project that carries a declared lane gets
`"methods": {}` rather than a lane the port cannot compute. ADR-0043 § 4 grades
the declared lane for EXACT equality and reads lane-absent as lane-empty, so a
reported method whose oracle lane is non-empty is a **fatal DECLARED-MISMATCH**,
and silence is the only sound answer.

The predicate covered the `ANNOTATION_HINT` line scan and `effects.attribution:`
— **two of upstream's four producers**:

| # | producer | reference | covered before #114 |
|---|---|---|---|
| 1 | `import_envelope` — an envelope at a call site | `unit_scan.rb:337` | annotations yes; **`effects.envelopes:` NO** |
| 2 | `attribute` — the project's `effects.attribution:` | `unit_scan.rb:370` | yes |
| 3 | `attribute_plugin` — a loaded plugin's effect rows | `unit_scan.rb:258` | **NO** |
| 4 | `FrameworkUnits` — plugin-synthesised units born with a lane | `framework_units.rb:155` | **NO** |

Producers 3 and 4 are keyed on the same thing — a non-empty `plugins:` — so one
arm covers both. `configures_plugins` already existed and already computed it;
it was wired only to the NARROWER exhaustiveness self-defense, which is correct
for the bit it withholds and says nothing about this lane.

**Why no gate saw it.** No fixture under `harness/effects-corpus/` carried
`plugins:`, `effects:` or `envelopes:` (0 hits across all eight), and the two
real corpora run against a *synthesised* `.rigor.yml` carrying `paths:` and
nothing else — the normalisation PR #112 added so the two arms would differ in
nothing but the engine under test. That normalisation erased the only config
shape that exercises producers 3 and 4. A normalisation that removes a confound
can also remove coverage, and this is the first recorded instance here.

---

## 2. The gate — two hand-written projects, one per new arm

Hand-written, not generated: there is no vendored table to derive them from
(unlike `05_posture` / `07_mutators`, which are functions of the vendored
catalogue and mutator sets). The shapes come from the reference's own config
grammar and one bundled plugin's effect rows, and every method carries a comment
naming the producer it exercises so the fixture cannot drift into testing
something else.

### 2a. `09_declared_envelopes` — producer 1's CONFIG stratum

`.rigor.yml` carries `effects.envelopes: [{namespace: "Svc::*", effect: [io.db]}]`
and the project carries **no annotation at all** — no `sig/`, and no `.rb` line
containing `%a{`, so the port's line scan cannot fire for the wrong reason and
mask the arm under test.

| method | producer | why it is there |
|---|---|---|
| `Svc::Repo#find` | 1 (`import_envelope`, config stratum) | calls `row(id)` implicit-self, so `envelope_target` answers the unit's own owner class and the index is consulted at `Svc::Repo#row`; a `namespace:` entry answers for ANY selector of a matching class, so `io.db` joins THIS method's lane. **The DECLARED-MISMATCH.** |
| `Svc::Repo#row` | 1, the callee side | the control for "a method's own bound never colours its own row" — bounded by the same entry, calls nothing bounded, must be `declared: []` on both sides |

### 2b. `10_declared_plugins` — producers 3 and 4

`.rigor.yml` carries `plugins: [rigor-activesupport-core-ext]` and **nothing
else**: no `effects:` block, no `sig/`, no annotation. The plugin is bundled by
the pinned reference and `Plugin::Loader` requires a bundled plugin by its
engine-relative path (`loader.rb:74`, upstream #194 slice 2), so the entry
resolves from the submodule with **no extra `-I` and no installed gem** — the
probe's § 6b arm added the plugin's `lib` to the load path defensively and it
turns out not to be needed, which matters because `effects_diff.py`'s `run_ref`
pins only `rigor-rbs-inline`.

| method | producer | why it is there |
|---|---|---|
| `Clock#now` | 3 (`attribute_plugin`) | `Time.current` → `row(TIME, :current, CLOCK, singleton: true)`, labels `["nondet.time","global.read"]` |
| `Clock#today` | 3 | `Date.current`, the same row family — a second instance so the fixture does not rest on one key resolving |
| `Clock#zone` | 3 | `Time.zone` → `ZONE_READ` = `["global.read"]` alone; a DIFFERENT label set from the same plugin, so a port that hard-codes one bundle per plugin still fails |
| `Clock#plain` | none | **the must-still-fire control in the corpus half**: `puts` is a core catalogue row no plugin claims, so no declared lane on either side and `io.output.stdout` proven on both |

A plugin row's labels go to the DECLARED lane always — `attribute_plugin` calls
`add_declared` whether or not the row discharges (`unit_scan.rb:255-258`) — and
never to the proven lane, which is why the oracle shows a non-empty `declared:`
beside an empty `effects:` and `rendered_declared` drops nothing.

Producer 4 (`FrameworkUnits`) is NOT separately exercised: it fires on the same
`plugins:` trigger, and reproducing it needs an ActiveRecord model with a
uniqueness validator, i.e. a second plugin and a framework base class. The
predicate arm it needs is the one `10_declared_plugins` already gates.

---

## 3. Verdict tables

### 3a. BEFORE — the master binary (`3621b93`), with both new projects in place

```
01_core_origins        oracle=16   / 12 labels  rigor-rs=16  / 12   MATCH=  16 UNDER=   0 OVER=0 DM=0
02_propagation         oracle=15   / 12         rigor-rs=15  /  9   MATCH=  10 UNDER=   5 OVER=0 DM=0
03_taint               oracle=11   /  3         rigor-rs=11  /  2   MATCH=   9 UNDER=   2 OVER=0 DM=0
04_declared            oracle= 4   /  1         rigor-rs= 0  /  0   MATCH=   0 UNDER=   4 OVER=0 DM=0
05_posture             oracle=133  / 46         rigor-rs=133 / 20   MATCH=  61 UNDER=  72 OVER=0 DM=0
06_edge                oracle= 6   /  1         rigor-rs= 6  /  0   MATCH=   5 UNDER=   1 OVER=0 DM=0
07_mutators            oracle=383  /233         rigor-rs=383 /149   MATCH= 219 UNDER= 164 OVER=0 DM=0
08_resolved            oracle=44   / 27         rigor-rs=44  / 12   MATCH=  28 UNDER=  16 OVER=0 DM=0
09_declared_envelopes  oracle= 2   /  0         rigor-rs= 2  /  0   MATCH=   1 UNDER=   0 OVER=0 DM=1   <-- FATAL
10_declared_plugins    oracle= 4   /  1         rigor-rs= 4  /  1   MATCH=   0 UNDER=   4 OVER=0 DM=3   <-- FATAL
mastodon/app           oracle=6948 /4050        rigor-rs=6948/1420  MATCH=5217 UNDER=1731 OVER=0 DM=0
TOTAL                  MATCH=5566  UNDER=1999  OVER=0  DECLARED-MISMATCH=4     => FAIL
```

**The recorded pre-fix DECLARED-MISMATCH counts on the two new gate projects are
1 and 3**, exactly the issue's, and they name the same rows with the same label
sets:

```
09: DECLARED-MISMATCH: Svc::Repo#find — declared lane [] != oracle ['io.db']
10: DECLARED-MISMATCH: Clock#now      — declared lane [] != oracle ['global.read', 'nondet.time']
10: DECLARED-MISMATCH: Clock#today    — declared lane [] != oracle ['global.read', 'nondet.time']
10: DECLARED-MISMATCH: Clock#zone     — declared lane [] != oracle ['global.read']
```

The eight pre-existing projects and mastodon/app are unchanged at the recorded
baseline (`MATCH=5565 UNDER=1995 OVER=0 DM=0`), re-measured on this tree before
the fixtures were added.

### 3b. AFTER — the fix

```
01_core_origins        MATCH=  16 UNDER=   0 OVER=0 DM=0
02_propagation         MATCH=  10 UNDER=   5 OVER=0 DM=0
03_taint               MATCH=   9 UNDER=   2 OVER=0 DM=0
04_declared            MATCH=   0 UNDER=   4 OVER=0 DM=0
05_posture             MATCH=  61 UNDER=  72 OVER=0 DM=0
06_edge                MATCH=   5 UNDER=   1 OVER=0 DM=0
07_mutators            MATCH= 219 UNDER= 164 OVER=0 DM=0
08_resolved            MATCH=  28 UNDER=  16 OVER=0 DM=0
09_declared_envelopes  MATCH=   0 UNDER=   2 OVER=0 DM=0   (rigor-rs=0 methods; absent-method 2)
10_declared_plugins    MATCH=   0 UNDER=   4 OVER=0 DM=0   (rigor-rs=0 methods; absent-method 4)
mastodon/app           MATCH=5217 UNDER=1731 OVER=0 DM=0
TOTAL                  MATCH=5565  UNDER=2001  OVER=0  DECLARED-MISMATCH=0     => PASS
```

The four pinned projects hold exactly: **01 16/0/0/0 · 02 10/5/0/0 · 03 9/2/0/0
· 04 0/4/0/0**, cells `MATCH / UNDER / OVER / DM`. `0 OVER` and `0
DECLARED-MISMATCH` everywhere.

**The measured cost of the widening is 1 MATCH, and it is confined to the new
fixtures.** `Svc::Repo#row` was a MATCH before the fix and is withheld after it,
so TOTAL MATCH moves 5566 → 5565 and UNDER 1999 → 2001. Every pre-existing
project — the eight fixtures and mastodon/app — is bit-for-bit unchanged: none
of them trips any of the new arms, which is exactly why the fixtures had to ship
WITH the fix.

Supplementary, `--scale`: `gitlab-foss/lib` is unchanged at
**20,990 / 7,617 / 0 / 0** (its synthesised config carries `paths:` and nothing
else, so no new arm can fire there).

`--self-test` (the instrument's own gate) is all-MATCH on all eleven projects,
the two new ones included: 16 / 15 / 11 / 4 / 133 / 6 / 383 / 44 / **2** / **4**
/ 6,948.

---

## 4. The change, in four lines of behaviour

`crates/rigor-cli/src/effects/mod.rs` only. No `crates/rigor-infer` or
`crates/rigor-index` file is touched, so ADR-0043 § 1 is structurally satisfied.

- **`config_declares_attribution` → `config_declares_effect_lane`**, now testing
  `effects.attribution:` **or** `effects.envelopes:`. One parse, one read of the
  config file, same fail-closed rule for an unparseable config ("I could not
  read it" is not "it has no table").
- **`carries_effect_annotations` gains a `configures_plugins` arm** — producers
  3 and 4 in one test, on the `plugins:` list alone, for the reasons
  `configures_plugins`'s own doc already states (the reference discovers
  effect-bearing plugins only from that list; its one auto-wire,
  `rigor-rbs-inline`, ships no `effect_attributions:`).
- **The plugin exhaustiveness self-defense is now SUBSUMED at the call site** —
  the same `plugins:` list suppresses the whole report, so `report_rows` is only
  ever reached with `plugins: false` from `cmd_effects`. It is **kept**, and the
  call site says why in a comment: it is the narrower and independently correct
  rule for the one lane a plugin moves, it is the arm that survives whenever the
  declared lane lands, and deleting a working FP-safety mechanism to tidy up a
  subsumption is how this repo has been burned before.
- **The module docs tabulate all four producers against the four arms**, and
  state the direction this forces — for the declared lane, parity with the
  reference means MORE suppression, not less, reconciled only by ADR-0043 § 2's
  exact-match rule.

One RESIDUAL is recorded in the module docs rather than closed: envelope
**stratum 5** — annotated members of the BUILT RBS environment — has no arm of
its own. At the pin the only `%a{…}` in installed core/stdlib RBS is one
`%a{pure}` in `core/regexp.rbs` (an EMPTY bound, no declared label), and the
non-empty ones live in plugin RBS, which the `plugins:` arm now covers. What is
left uncovered is a third-party gem shipping an annotated `.rbs` into a project
that names no plugin; closing it needs an RBS annotation reader the port does
not have (`crates/rigor-index/src/rbs.rs` carries no `%a{…}` handling at all).
No corpus on this machine contains the shape, so it is stated, not gated.

### 4a. The must-still-fire controls, which are the load-bearing half

Widening a suppression is the one change that cannot fail its own gate: every
project gets quieter and quieter is the direction the differential grades as
safe. Over-suppression is invisible to `effects_diff.py`. So each arm owes a
project that trips NEITHER trigger and still emits a real method:

| test | trigger asserted | must-still-fire control |
|---|---|---|
| `an_annotation_free_project_reports_real_methods` (pre-existing, kept) | no annotation | reports `X#m` with `["io.output.stdout"]` |
| `an_envelopes_block_in_the_config_suppresses_and_a_config_without_one_still_reports` (new) | `effects.envelopes:` suppresses | an `effects:` block carrying `tolerated:` only still reports `X#m` with `["io.output.stdout"]` |
| `a_plugins_list_suppresses_the_whole_report_and_a_plugin_free_project_still_reports` (new) | `plugins:` suppresses | the SAME project with the entry removed still reports `X#m` with `["io.output.stdout"]` **and still reaches exhaustiveness** |
| `a_plugin_free_project_still_reaches_exhaustiveness` (pre-existing, kept) | — | the exhaustiveness half, unchanged |
| `an_attribution_table_…_and_a_plain_config_does_not` (pre-existing, kept) | `attribution:` suppresses | `effects: {tolerated:}` does not; an unparseable config does |

Corpus-side, `Clock#plain` and `Svc::Repo#row` are the same argument at the
project level: pre-fix they prove the port really was reporting these projects
(a project that reported nothing would score 0 DECLARED-MISMATCH for the wrong
reason), post-fix they make the silence read as `absent-method` UNDERs rather
than as accidental agreement.

**No over-suppression was found.** The one deliberate imprecision is stated
below.

---

## 5. Gates

| gate | verdict |
|---|---|
| `cargo test --workspace` | **PASS** — 1,277 passed, 0 failed |
| `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy --workspace --all-targets -- -D warnings` | **PASS** — exit 0, fresh target dir |
| `harness/effects_diff.py --self-test` | **PASS** — all-MATCH on all eleven projects |
| `harness/effects_diff.py` (default set, incl. `mastodon/app`) | **PASS** — `MATCH=5565 UNDER=2001 OVER=0 DM=0`; the four pinned projects hold at 16/0/0/0 · 10/5/0/0 · 9/2/0/0 · 0/4/0/0 |
| `harness/effects_diff.py --scale` | **PASS** — `gitlab-foss/lib` unchanged at 20,990 / 7,617 / 0 / 0 |
| `rigor check` vs the master baseline, `mastodon/app` | **PASS** — stdout byte-identical (one SHA-256 across all four runs, 420 lines), stderr empty, exit 1; default threads AND `RAYON_NUM_THREADS=1`, both binaries |
| `harness/run_snapshot.rb` | **PASS** — 98 fixtures, 407 matched, 35 gaps, 2 registered divergences, **0 unregistered** |
| `harness/docs_check.py` | **PASS** — 4 budgets, links resolve |
| `harness/gen_effects_posture_corpus.py --check` | **PASS** |
| `harness/gen_effects_mutator_corpus.py --check` | **PASS** |
| residue in the user's corpus checkouts | **none** — nothing under `mastodon/`, `gitlab-foss/lib` or `rigor-survey/` modified since the branch was cut; no `rigor-effects-*` temp project survives |

---

## 6. Deviations, with reasons

1. **`envelopes: []` suppresses although it produces nothing.** The test is
   PRESENT-AND-NOT-NULL, which is exactly the reading the `attribution:` arm has
   always had, kept uniform deliberately. It is an under-claim (the safe
   direction), and making it exact would mean parsing entries the port does not
   otherwise read — the probe's § 4c step 1, which is a separate slice. The same
   applies to a `namespace:` that matches no class in the project. This is the
   only known over-suppression the widening introduces and it is degenerate; no
   real project shape was found that should emit and now does not.
2. **`10_declared_plugins` does not separately exercise producer 4.**
   `FrameworkUnits` fires on the same `plugins:` trigger the fixture already
   gates, and reproducing it needs an ActiveRecord model with a uniqueness
   validator — a second plugin and a framework base class, i.e. a fixture that
   could fail for more than one reason. § 2b.
3. **The two new projects' non-DM counts differ slightly from the probe's
   scratch projects.** `09_declared_envelopes` scores `MATCH=1 UNDER=0 DM=1`
   where probe `p_envcfg` scored `MATCH=1 UNDER=1 DM=1`; the DM count, the row
   named and the label set are identical, and the extra UNDER was an artefact of
   the scratch project's second method. `10_declared_plugins` reproduces § 6b to
   the digit (`MATCH=0 UNDER=4 OVER=0 DM=3`).
4. **The plugin's `lib` is NOT added to the reference arm's `-I`.** The probe's
   § 6b arm did so; measured here, it is unnecessary — `Plugin::Loader` requires
   a bundled plugin by its engine-relative path. `effects_diff.py` is untouched,
   which is the outcome to prefer: the instrument keeps pinning
   `rigor-rbs-inline` and nothing else.
5. **`docs/CURRENT_WORK.md` is not touched.** Its byte budget has 307 bytes of
   headroom and a ledger line of the usual density is ~700; the ledger fold is
   the orchestrator's own commit by convention, and doing it here would fail
   `harness/docs_check.py`, which is a listed gate.
6. **The standing 9,204-file sweep was not re-run.** The gate list named
   byte-identity on `mastodon/app` for the ADR-0043 § 1 obligation, and the
   effects module is not reachable from `check` at all — no `crates/rigor-infer`
   or `crates/rigor-index` file is touched by this branch. The fixture snapshot
   harness (98 fixtures, 0 unregistered FPs) is the second half of that evidence.

---

## 7. Explicitly NOT done

- **The declared lane is not implemented**, in any direction. The slice-6 probe
  measures its entire direct half at **+2 MATCH across 36,167 oracle methods**,
  needing an RBS annotation reader the port does not have, and wrong in both
  fatal directions on any project that calls an annotated method the ordinary
  way. Silence remains the sound answer.
- **No suppression is narrowed anywhere.** The lexical `ANNOTATION_HINT` arm
  stays as-is, including its known false-positive shape (a `%a{` in prose
  suppressing a whole project). Replacing it with a parsed envelope index is the
  probe's § 4c step 1 and a separate slice.
