# Effects slice 3 — the taint bit + `causes`: what shipped (2026-08-26)

Implements [the mini-spec](20260826-effects-s3-mini-spec.md) from
[the probe](20260826-effects-s3-probe.md), on top of the
[#106 posture fix](20260826-s106-posture-over-fix.md). Branch
`claude/effects-s3-taint`, cut from `b44791b` (master).

Everything measured against the PINNED submodule at `b10bd5df` (v0.3.4),
populated into this worktree from the parent checkout's tree (never the network,
never `REFERENCE_RIGOR_DIR`), with `.rigor/cache` cleared either side of every
oracle run — which `harness/effects_diff.py` does itself.

---

## 1. What shipped

`crates/rigor-cli/src/effects/collect.rs` (the collector) and `mod.rs` (the
report). No other crate is touched; nothing reads inference state; `rigor check`
is not reachable from any of it (ADR-0043 § 1).

**The rule.** A method is exhaustive iff the collector can see that no producer
fires, every undecidable site counting as firing. Per producer, at the pin:

| producer | treatment | why |
|---|---|---|
| `dynamic-send` | **EXACT** | a reflective send's selector is a `SymbolNode` / `StringNode` or it is not; pure syntax |
| `opaque-callable` — eval family ≥1 positional, bare `binding` | **EXACT** | pure syntax; the block and keyword forms deliberately fall through |
| `opaque-callable` — `&expr` | **EXACT** | needs the unit's `&blk` NAME, which slice 2 dropped and this slice reinstates |
| `opaque-callable` — the `.call` arm | **SOUND, over-taints** | upstream's own last condition is `record.nil? \|\| …`, always true typer-free |
| `unknown-ownership` | **EXACT** at the six compound-write node types and on the claimed `mutates: receiver` path | `MutationClassifier::label_for` is pure syntax on both sides |
| `dynamic-receiver` | **taint** at every uncatalogued call with an explicit receiver | the typer's `dynamic` bit; the port cannot prove the negative |
| `unresolved-self-call` | **taint** at every uncatalogued receiver-less call | the typer's `resolved` bit; same |
| `method-missing`, `budget`, `collector-error`, `plugin-attribution`, `template-not-analysed` | not emitted from the walk | no producer at the pin / per-FILE fail-soft / plugin stratum |

**The transitive AND**, priced as a selector-set test. `push_edge` is the single
funnel every project edge goes through upstream — the claimed path when
`keeps_project_edge?` says so (`unit_scan.rb:409`), a reflective `send` with a
literal selector (`:476`), the uncatalogued path (`:514`) — so the port taints
there whenever the selector names any unit the run collected. With the posture
tier off (#106) the surviving disjunct of `keeps_project_edge?` is IMPLICIT
SELF, which is exactly the `Kernel#format`-shadowing case. Sound because
`Propagator::Index#targets_for` can only resolve to keys the collection holds
(`propagator.rb:195`), so the port's candidate set is a superset. The set is
knowable only after every file is scanned, so a unit records candidate selectors
and `report_rows` resolves them — a two-pass report, no propagator.

**`causes` is the real shape.** `[[cause, detail], …]`, de-duplicated and sorted
by `[cause, detail]` (`BTreeSet` order, `None` reading as upstream's `nil.to_s`),
`cause` drawn from the closed `TaintCause::ALL` enum, `detail` the selector for
`unresolved-self-call` and `null` otherwise. **The out-of-enum `port-incomplete`
marker is RETIRED**: it broke the port's own `causes.empty? == exhaustive`
invariant, which now holds by construction (the edge taints are materialised
into `causes` by the same call that decides the bit).

**Plugin self-defense.** A project whose `.rigor.yml` configures `plugins:`
never gets `exhaustive: true`. Deliberately narrower than the annotation
self-defense's `methods: {}` — the rows are still reported, only the one lane a
plugin could move is withheld. Keyed on `plugins:` alone: the reference
discovers effect-bearing plugins only from that list, and its one auto-wire
(ADR-93, `configuration.rb:308`) is `rigor-rbs-inline`, which ships no
`effect_attributions:` — checked against the pinned checkout.

**`Row::trivial` is now reachable**, so the DEFAULT report starts omitting rows.
The differential always passes `--full`; the snapshot landmine the probe records
(§ 3e — upstream's `trivial?` also reads `rendered_declared`, and the port's
declared lane is always ∅) is slice 5's.

**New corpus project `harness/effects-corpus/06_edge`** — the probe's `p_edge`,
hand-written and promoted. Six methods, three shapes, no proven label to lose:
it grades one lane, the bit. § 3.

---

## 2. Verdict tables

### 2a. The four PREDICTED projects — the pinned acceptance, exactly

| project | oracle | rigor-rs | MATCH | UNDER | OVER | DM | UNDER by kind |
|---|---|---|---|---|---|---|---|
| `01_core_origins` | 16 / 12 labels | 16 / 12 | **16** | 0 | 0 | 0 | — |
| `02_propagation` | 15 / 12 | 15 / 9 | **10** | 5 | 0 | 0 | missing-label 2, extra-taint 3 |
| `03_taint` | 11 / 3 | 11 / 2 | **9** | 2 | 0 | 0 | missing-label 1, extra-taint 1 |
| `04_declared` | 4 / 1 | 0 / 0 | 0 | 4 | 0 | 0 | absent-method 4 |
| **TOTAL** | **46** | **42** | **35** | **11** | **0** | **0** → **PASS** |

Cell for cell the mini-spec's table, including the UNDER *kinds*. The 4 non-04
UNDERs are the structural ones it names: `Pipeline#transform` and
`Taint#through_a_ghost` need receiver typing; `Recursive#mutual_b` and
`Recursive#walk` need the `resolved` bit plus slice 4's real closure. The 3
missing-label UNDERs (`Pipeline#run`, `Recursive#mutual_a`, `Taint#literal_send`)
are slice 4's transitive LABEL lane and are unchanged from slice 2.

`01_core_origins` at 16/16 is the collector fixture closing completely.

### 2b. `05_posture` — UNPREDICTED, reported

| section | n | MATCH | what it means |
|---|---|---|---|
| `Posture#c_*` | 80 | **8** | the eight classes the oracle's typer cannot resolve, where both sides now agree at ∅ + non-exhaustive |
| `Row#r_*` | 19 | **19** | must-still-fire control: every row still answers, and now agrees on the bit too |
| `Universal#u_*` | 34 | **34** | must-still-fire control: the universal tier still beats the posture |
| **total** | **133** | **61** | UNDER 72 (missing-label 26, extra-taint 46), **OVER 0** |

**All 53 control methods are MATCH**, which is what the #106 note predicted would
happen "when slice 3 turns the exhaustiveness bit on". The corpus half of that
control is still not *fatal* on its own (a lost label reads as UNDER, and the
differential's verdict is 0 OVER) — but the 53 is now a number a regression moves,
beside the crate test that is fatal.

### 2c. `06_edge` — the new gate

| | oracle | rigor-rs | MATCH | UNDER | OVER |
|---|---|---|---|---|---|
| `06_edge` | 6 / 1 label | 6 / 0 | **5** | 1 (missing-label) | **0** |

The one UNDER is `Reader#read_it`, where the oracle proves `File`'s `io.fs`
posture and the port's posture tier is off — a known, deliberate #106 UNDER.

**It bites.** Rebuilt with one line changed — `Summary::exhaustive` ignoring
`edge_selectors`, i.e. emitting the DIRECT bit — into a fresh `CARGO_TARGET_DIR`
and graded with the same instrument:

```
06_edge   MATCH=3  UNDER=1  OVER=2
  OVER: Shadow#calls_shadowed          — claims exhaustiveness the oracle does not
  OVER: Shadow#calls_shadowed_caller   — claims exhaustiveness the oracle does not
TOTAL  MATCH=99  UNDER=84  OVER=2  => FAIL
```

Every other project — 01, 02, 03, 04, 05 — stays at **0 OVER** under that
variant. `06_edge` is the only thing in the repo that sees the trap.

### 2d. mastodon/app — real scale, 6,948 methods / 1,236 files

Same project directory, same oracle run, the two binaries:

| | master baseline (`b44791b`) | this branch |
|---|---|---|
| methods | 6,948 | 6,948 (key sets equal) |
| oracle proven labels | 4,050 | 4,050 |
| rigor-rs proven labels | 1,420 | **1,420 — unchanged** |
| MATCH | 4,514 | **5,217** |
| UNDER | 2,434 (missing-label 1,489 · extra-taint **945**) | 1,731 (missing-label 1,489 · extra-taint **242**) |
| OVER | 0 | **0** |
| DECLARED-MISMATCH | 0 | 0 |

**703 of the 945 extra-taints close (74.4%), at 0 OVER, and the proven lane does
not move by one label** — which is the honest reading of a slice that touches
only the bit.

---

## 3. `06_edge`, and why it is hand-written

Three shapes, all DIRECTLY exhaustive on the caller's side and TRANSITIVELY
tainted, measured on the oracle before the port was pointed at them:

| method | the call | direct | transitive |
|---|---|---|---|
| `Shadow#calls_shadowed` | `format("a")` → the `Kernel#format` row (`effects: []`), implicit ⇒ edge to `Shadow#format` | true | **false** |
| `Shadow#calls_shadowed_caller` | `caller` → the `Kernel#caller` row ⇒ edge to `Shadow#caller` | true | **false** |
| `Reader#read_it` | `File.slurp("x")` → `File`'s POSTURE ⇒ edge to the project's `File.slurp` | true | **false** |

Hand-written rather than generated: unlike `05_posture` there is nothing here
that is a function of the vendored catalogue *as data* — the shape is
`keeps_project_edge?`'s two disjuncts, which are structural. The two Kernel rows
it names (`format`, `caller`) are pinned by the `--check`-gated catalogue anyway,
so a re-vendor that removed either would show up as this project losing its
teeth rather than as a silent pass.

---

## 4. Gates

| gate | verdict |
|---|---|
| `cargo test --workspace` | **PASS** — 1,263 tests, 0 failed (12 new: 9 collector, 3 report) |
| `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy --workspace --all-targets -- -D warnings` | **PASS** — exit 0, fresh target dir |
| `harness/effects_diff.py --self-test` | **PASS** — all-MATCH on all six projects (16 / 15 / 11 / 4 / 133 / 6) |
| `harness/effects_diff.py` (full corpus incl. `06_edge`) | **PASS** — `MATCH=101 UNDER=84 OVER=0 DM=0`; the four predicted at 35/11/0/0 |
| `rigor check` vs the master baseline, mastodon/app (1,236 files) | **PASS** — stdout + stderr + exit code byte-identical, default threads AND `RAYON_NUM_THREADS=1`; the new binary's two thread modes agree (420 stdout lines, exit 1) |
| `harness/run_snapshot.rb` | **PASS** — 98 fixtures, 407 matched, 35 gaps, 2 registered divergences, **0 unregistered** |
| release rebuild + `harness/fp_audit.py --gaps --sweep` | **PASS** — 0 FP / 9,204 files; gap set byte-identical to the baseline taken at the branch point |
| `harness/docs_check.py` | **PASS** — 4 budgets, links resolve |
| `harness/gen_effects_posture_corpus.py --check` | **PASS** — `05_posture` still matches the vendored catalogue exactly |

---

## 5. Deviations, with reasons

1. **The producer table's `unresolved-self-call` exception is not spelled
   separately.** The mini-spec writes it as "taint unless the call resolves to a
   unit the collector itself collected in the receiver's own class". Implementing
   that exception is a **no-op for the bit**: a receiver-less call whose selector
   names a project unit in the receiver's own class is, by definition, a call
   whose selector is in the run's selector set, so the edge taint of rule 3 fires
   at exactly those sites and re-taints them. Spelling both would have produced
   the same `exhaustive` on every method and a differently-worded `causes` on
   some. The port therefore implements the probe's simulation exactly — blanket
   taint plus the edge taint — which is what the acceptance table was derived
   from. Anything else would have been improvising against a pinned number.

2. **The probe's `p_edge` is committed as `06_edge`.** The corpus directories are
   numbered and the differential enumerates them in sorted order; keeping the
   probe's scratch name would have made it the only unnumbered member.

3. **The `06_edge` control scores 2 OVER without the edge taint, not the probe's
   3.** Different arm, and the difference is #106: the probe measured T0 (posture
   tier ON), where `Reader#read_it` is claimed by `File`'s posture and so
   over-claims; this port's tier is already off, so that method taints for
   another reason and only the two Kernel-row shadowings are left. The case is
   kept in the corpus because it is upstream's second edge source and a future
   slice that re-introduces the tier must not lose the edge with it.

4. **The mastodon figures are MATCH 5,217 / extra-taint 242, not the probe's
   5,234 / 243.** The probe said in as many words to read its MATCH column as an
   upper bound at that scale: it was measured on the SIMULATED T2 collector,
   whose proven lane is better than the shipped port's on ~15 methods
   (missing-label 1,471 vs the port's 1,486), and #106 then moved the port's to
   1,489. `5,234 − 5,217 = 17` against an 18-method proven-lane deficit is the
   same story told twice. The extra-taint column — the one the probe called the
   honest slice-3 figure — lands within one method (243 predicted, 242 measured),
   and OVER is exact at 0. Not a contradiction; the probe's own caveat.

5. **The plugin self-defense emits a `plugin-attribution` cause.** Forcing
   `exhaustive: false` with an empty `causes` would re-introduce, in a narrower
   place, exactly the invariant break this slice retires `port-incomplete` for.
   `plugin-attribution` is upstream's own spelling for "a stratum claimed this
   and the analyzer did not read the body" (`unit_scan.rb:262`), which is what is
   being withheld; the detail is the plugin row key upstream and unknown here, so
   it is `null`.

6. **Two sound levers left unspent, both deliberate.** An explicit `self.` receiver
   is never `Dynamic` in either engine, and neither is a LITERAL receiver
   (`[1,2,3].map`, `1 + 2`) — so upstream taints at neither, and the port taints
   at both. Spending either would have raised MATCH **above** the pinned table,
   which the mini-spec calls a stop-and-re-derive event rather than a win. They
   are recorded here as the cheapest remaining typer-free precision, to be
   derived and re-pinned deliberately (the probe prices the literal island at
   § 6b) rather than picked up as a side effect of this slice.
