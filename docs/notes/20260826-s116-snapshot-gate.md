# S116 — the snapshot gate: the instrument first, and it bites on the first run

Closes issue #116. Branch `claude/s116-snapshot-gate`, cut from `d7df8b7`
(master). **Instrument and docs only — no `crates/` change, and the shipped
binary is byte-identical to the master baseline (`sha256 21486bdd…`).**

Everything measured against the PINNED submodule at `b10bd5df` (v0.3.4),
populated into this worktree from the parent checkout's tree (never the network,
never `REFERENCE_RIGOR_DIR`; the populated tree diffs identical to the parent's
bar `.git`), with `.rigor/cache` cleared either side of every oracle run — which
`harness/effects_diff.py` does itself.

Three deliverables:

1. **`harness/effects_diff.py` compares the SNAPSHOT.** It runs `rigor effects
   update` on the reference arm and SYNTHESISES the port's `.rigor-effects.yml`
   from the JSON the port already emits — no port code, which is the whole
   point. `SNAPSHOT_HEADER_KEYS`, defined and unused since slice 0, is narrowed
   from four keys to one (`rigor`); `schema:`, `vocabulary:` and
   `config_digest:` are all compared and all agree (§ 2).
2. **It fails today, on `07_mutators`, on exactly two rows** — the § 5a
   inversion, reproduced to the digit (§ 3). It is reported SEPARATELY and does
   **not** set the exit code; the fix is a design question and is escalated, not
   taken (§ 5).
3. **Two ADR-0043 corrections** (§ 6): § 2 gains the stated caveat that the
   over-taint direction REVERSES for the snapshot, and § 5 row 5's unmeetable
   "byte-comparable modulo the excluded header" is restated to what is
   achievable.

**The standing report comparison did not move**: `TOTAL MATCH=5565 UNDER=2001
OVER=0 DECLARED-MISMATCH=0`, with the four pinned projects at 16/0/0/0 ·
10/5/0/0 · 9/2/0/0 · 0/4/0/0 — the s114 baseline exactly.

---

## 1. The mechanism, and why it needs no port code

`Snapshot.build_methods` (`snapshot.rb:213-221`) records `entry.direct` — the
collector's own-body summary. Every input `omit?` (`:293-300`) and `entry_for`
(`:262-269`) take is already in the port's `effects --full --format=json`:
`effects`, `declared`, `exhaustive`, `causes`, and the per-origin `direct`
bundles. So the port's snapshot is *derivable*, and the port shipping no
`update` verb is not a reason to leave the artifact ungraded — it is the reason
this comparison is cheap.

The port's JSON lanes ARE the direct readings: the transitive lane was declined
(slice 4), so `effects` is `direct.proven`, `exhaustive` is `direct.exhaustive?`
and `causes` is `direct.causes`. That correspondence is what makes the
synthesis one function rather than a model.

### 1a. Two self-tests, because a re-implementation of upstream's rules is where the bug would be

A grader that mis-implements `omit?` or the serialiser reports the port's
disagreements as noise and its own as the port's. Both halves are gated:

- **The round trip, on EVERY run.** The oracle's own `.rigor-effects.yml` is
  parsed and re-rendered, and must come back byte-identical before any verdict
  is computed. It is INVALID otherwise, never delta-free. This also asserts the
  file really is the declared JSON-compatible YAML subset (`snapshot.rb:36-38`);
  the parser is hand-written for exactly that reason, so a line it cannot read
  is a shape surprise rather than a value it guesses.
- **The re-derivation, under `--self-test`.** `omit?` is re-applied to the
  oracle's OWN `update --full` file (whose rows carry `direct.proven`, the
  rendered direct declared lane, `direct.exhaustive?` and the rendered direct
  causes) plus the JSON's `direct` bundles, and must reproduce the oracle's
  DEFAULT file **byte for byte, header included**. Measured on all eleven
  projects:

  | project | `--full` rows | re-derived default | oracle default |
  |---|---|---|---|
  | 01 / 02 / 03 / 04 | 16 / 15 / 11 / 4 | 12 / 9 / 2 / 1 | 12 / 9 / 2 / 1 |
  | 05 / 06 / 07 / 08 | 133 / 6 / 383 / 44 | 45 / 1 / 136 / 11 | 45 / 1 / 136 / 11 |
  | 09 / 10 | 2 / 4 | 1 / 4 | 1 / 4 |
  | **mastodon/app** | **6,948** | **1,340** | **1,340** |

  Every one byte-identical. `omit?` and the serialiser are transcribed
  correctly, on 6,948 real methods and not only on fixtures.

One shortcut inside the re-derivation, exact rather than approximate: `omit?`'s
second clause reads the RAW declared lane where the `--full` file carries the
RENDERED one, and the two coincide whenever `proven` is empty — which is the
only case that clause can reach.

### 1b. The controls, which are the load-bearing half

Neither self-test can prove a verdict FIRES: the oracle agrees with itself, so a
`compare_snapshots` returning MATCH unconditionally passes both. `--self-test`
therefore runs **28 pure-function controls** first — one per verdict in both
directions, one per `omit?` clause, the header's three compared keys, the label
algebra's segment-awareness, the serialiser's three field omissions, the digest
recipe, and the "no safe default" rule of § 2. Four sabotages were measured to
break them:

| sabotage | controls that FAIL |
|---|---|
| `compare_snapshots` returns an empty Counter | 8 |
| `SNAPSHOT_HEADER_KEYS` restored to all four keys | 3 |
| `omit?` with the `return false unless direct.exhaustive?` clause deleted | 1 |
| string-prefix subsumption instead of segment-aware | 1 |

They live in the tool rather than beside it, so they run whenever `--self-test`
does.

---

## 2. The normalisations, and the header narrowing

Every normalisation is listed in one block in the source, at the head of the
snapshot section, because a normalisation that quietly removes coverage is this
repo's most expensive recurring bug (it is PR #115's root cause). Seven, in
brief:

1. `rigor:` excluded; the other three header fields compared.
2. The port's `schema:` is the grader's constant, its `vocabulary:` is read from
   the port's vendored `registry.yml`, its `config_digest:` is recomputed here.
   **The port computes none of the three**, and the code says so where the next
   reader will hit it — agreement on those lines is agreement with a stand-in,
   and all three move into the port when `update` lands.
3. The compared artifacts are the DEFAULT tables, not `--full`. The default
   table IS the committed file and `omit?` is the thing under test; grading
   `--full` would normalise the omission rule away entirely. (The report
   comparison keeps `--full` for the opposite reason, and that is unchanged.)
4. `unresolved:` is compared and counted, never fatal (§ 4).
5. `reach:` is UNGRADED for a project configuring a non-empty
   `effects.snapshot.reach:`. No project in the standing set does — the
   reference itself writes `reach: {}` for every project that has not opted in
   — so this is a guard, not a behaviour.
6. **A port JSON row missing `effects` or `exhaustive` is INVALID, not
   defaulted.** `lanes()`'s weakest-value rule cannot transfer to this surface:
   defaulting `exhaustive` to False keeps the row and MANUFACTURES a
   SNAPSHOT-OVER, defaulting it to True drops the row and HIDES one. The
   inversion this whole comparison exists to catch also inverts the instrument's
   own fail-safe direction, so there is no default.
7. Row and label ordering is Python's `sorted()` over `str`, which equals Ruby's
   byte-wise `sort` because UTF-8 preserves code-point order.

### 2a. Three keys came back, and all three agree

| field | port-side source | measured |
|---|---|---|
| `schema:` | `PORT_SNAPSHOT_SCHEMA = 1`, the grader asserting `Snapshot::SCHEMA` on the port's behalf | agrees on 11/11 |
| `vocabulary:` | `crates/rigor-effects/vendor/effects/registry.yml` → `vocabulary: 1` | agrees on 11/11 |
| `config_digest:` | recomputed with `SHA256(JSON.generate(canonicalize(effects || {})))` | agrees on 11/11 |

`HEADER-MISMATCH=0` everywhere. The digest is the one worth reading carefully:
it is a **one-way reproduction**, not a two-way comparison, because the port has
no digest. What it catches is a pin that changes the recipe and a project whose
`effects:` block the reference resolved differently from the file on disk;
what it cannot catch is a port that computes the digest wrongly. Stated in the
function's own docstring. It is exercised non-degenerately by
`09_declared_envelopes`, the one standing project with a real `effects:` block
— the other ten digest the empty block to
`44136fa355b3678a…`, which the reference writes too.

`vocabulary:` is the interesting one long-term: it makes the vendored effect
catalogue a **third** pin-tracking surface with a standing gate, beside the
generators' `--check`.

---

## 3. The bite — measured, and narrower than it looks

`RESULT: FAIL` on the snapshot half, on the shipped master binary, first run:

```
SNAPSHOT-OVER: TypeFree#owned_writer          — row the oracle's record does not carry
SNAPSHOT-OVER: TypeFreeSingleton.owned_writer — row the oracle's record does not carry
```

Both engines agree on the label set and on the origin bundle; the only
difference is the taint bit, and the bit is the direction ADR-0043 § 2 calls
SAFE:

| | `effects` | `direct` bundle | `exhaustive` | default snapshot |
|---|---|---|---|---|
| oracle | `["mutate.local"]` | `construct:receiver-mutation` | **true** | **OMITTED** (trivial) |
| port | `["mutate.local"]` | `construct:receiver-mutation` | **false** (`dynamic-receiver`) | **WRITTEN** |

The reproducer is three lines of `harness/effects-corpus/07_mutators/mutators.rb`
(:2133) — `recv = []` then `recv.slot = 1`, a frame-owned local written through
a plain attribute-writer call.

**Why exactly two rows, and not the eight that look identical.** The corpus
carries the same frame-owned write in four syntaxes × two owners, which turns
the fixture into a controlled experiment:

| method | port taints? | port proves | outcome |
|---|---|---|---|
| `owned_writer` (`recv.slot = 1`) | **yes**, `dynamic-receiver` | `mutate.local` | **the bite** — not trivial, proven non-empty, non-exhaustive ⇒ KEPT |
| `owned_writer_op` / `_or` / `_and` (`recv.slot += 1`, `\|\|=`, `&&=`) | no | `mutate.local` | agrees — trivial ⇒ both omit |
| `owned_index_set` (`recv[0] = 1`) | yes | *nothing* | masked — `proven ∅ ∧ declared ∅` ⇒ both omit |
| `owned_index_op` / `_or` / `_and` | no | `mutate.local` | agrees — trivial ⇒ both omit |

So the class needs a **conjunction**: extra taint AND a surviving label that is
within `{mutate.local}`. `owned_index_set` is the near-miss where the port also
loses the label — an ordinary UNDER on the report, and invisible here because
`omit?`'s second clause catches it first. That is why the frequency is two and
not eight, and it is also why frequency is a bad guide to the class's
importance: every one of the 161 extra-taint rows on mastodon is one proven
label away from being another instance.

`harness/effects_diff.py`'s report half scores both rows `UNDER: extra-taint`
and the corpus `OVER=0`, exactly as the issue says. The two comparisons now
disagree about the same two rows, which is the finding.

---

## 4. The whole table, and what `unresolved:` costs

Default standing set, shipped master binary. Snapshot cells are
`MATCH / UNDER / OVER / unresolved-only`; `DECLARED-MISMATCH` and
`HEADER-MISMATCH` are 0 in every row.

| project | oracle rows | port rows | snapshot | body byte-identical |
|---|---|---|---|---|
| `01_core_origins` | 12 | 12 | 12 / 0 / **0** / 0 | **yes** |
| `02_propagation` | 9 | 9 | 5 / 3 / **0** / 1 | no |
| `03_taint` | 2 | 2 | 2 / 0 / **0** / 0 | **yes** |
| `04_declared` | 1 | 0 | 0 / 1 / **0** / 0 | no |
| `05_posture` | 45 | 19 | 19 / 26 / **0** / 0 | no |
| `06_edge` | 1 | 0 | 0 / 1 / **0** / 0 | no |
| **`07_mutators`** | 136 | 129 | 46 / 89 / **2** / 1 | no |
| `08_resolved` | 11 | 11 | 11 / 0 / **0** / 0 | **yes** |
| `09_declared_envelopes` | 1 | 0 | 0 / 1 / **0** / 0 | no |
| `10_declared_plugins` | 4 | 0 | 0 / 4 / **0** / 0 | no |
| **`mastodon/app`** | 1,340 | 1,301 | 125 / 210 / **0** / 1,005 | no |
| **TOTAL** | 1,562 | 1,483 | **220 / 335 / 2 / 1,007** | — |

Three fixture projects are already byte-identical on the body. The three the
#114 declared-lane self-defense silences (04 / 09 / 10) contribute nothing but
absent rows, which is the honest reading of a suppressed report — and a preview
of the probe's § 5c: an `update` implemented as "render the report" would write
those three files EMPTY, over a committed snapshot, on correct annotated code.
`06_edge`'s empty port table is unrelated: it reports six methods and `omit?`
drops all six.

**`unresolved:` is where mastodon's parity goes.** Of 1,301 shared rows, 1,005
differ in nothing else. Set the field aside and 1,130 of 1,301 rows (**86.9 %**)
are identical; keep it and 125 (**9.6 %**) are. The field renders the
reference's own account of why its typer declined — 4,503 of its 4,525
parameterised `dynamic-receiver(…)` causes carry `unsupported_syntax` or
`inferred_return_untyped`, reason codes a typer-free port cannot produce — so
grading it would be a gate that can only ever fail. It is counted and printed,
never fatal.

### 4a. This corrects the slice-5 probe's mastodon field table by three rows

The probe's § 3 field-by-field table records mastodon as
`128 identical / 10 effects / 158 exhaustive / 1005 unresolved-only`; this
measurement is `125 / 10 / 161 / 1005`. Same 1,301 shared rows, same 0 only-port
and 39 only-oracle, same `unresolved:` and `effects:` counts — three rows move
from *identical* to *exhaustive differs*. The probe's script was a throwaway and
is gone, so the difference cannot be attributed; what can be said is that this
implementation reproduces the oracle's own default file byte for byte on all
eleven projects (§ 1a) and reproduces the probe's `05` / `07` / `08` field rows
**to the digit** (07: 46 identical / 1 effects / 79 exhaustive / 1
unresolved-only). The numbers above are the ones to quote. The probe's headline
percentages survive: 87 % with `unresolved:` set aside, against a 19 %
structural byte-parity ceiling.

---

## 5. What was NOT done, and why the snapshot does not gate the exit code

**The port was not changed.** The two rows are a design question — does the port
suppress its extra taint at the snapshot boundary (`omit whenever EITHER reading
would omit`, strictly an under-claim), or does the artifact record the
divergence? — and it belongs to whoever owns ADR-0043. Touching
`crates/rigor-cli/src/effects/` to make a new gate green on its first run is
precisely the move the task forbids and the arc has learned to distrust.

**So the comparison is reported separately and does not set the exit code.**
The reasoning, which is written into `_snapshot_result` so it is re-read rather
than re-invented:

- A standing gate that is red for a known, escalated reason teaches the next
  reader to ignore a red gate. This arc already paid for one gate nobody could
  fail (#112) and one that was too narrow to fire (#114); a gate everybody
  ignores is the third shape of the same problem.
- The failure is nonetheless LOUD: its own `SNAPSHOT TOTAL` line, its own
  `SNAPSHOT RESULT: FAIL` line, and the two rows named in full on every run.
  Neither line can be confused with `RESULT:`, which is what gets grepped and
  quoted.
- `--snapshot-gate` promotes it to the exit code today, and **becomes the
  default the moment the design question is answered**. That is a dated
  decision, not a posture.

**What was deliberately not built: a known-failure allow-list.** Both of this
repo's exception tables are empty and a new entry is a finding
(`harness/divergence-registry.yml`, `UNBUILDABLE_DEFINITIONS`); an
expected-failure set for the one class this comparison exists to see would
re-create the blindness somewhere else. A new SNAPSHOT-OVER row is therefore
visible as a changed count against the recorded 2, not as a silent pass.

---

## 6. The two ADR-0043 corrections (plus one entailed sentence)

- **§ 2 gains a stated caveat.** The `exhaustive` row's "more taint is sound"
  holds only where a non-exhaustive summary produces no finding; through
  `omit?` it CONTRADICTS the same section's "a method rigor-rs reports that the
  reference does not is an OVER-CLAIM". `TypeFree#owned_writer` is named as the
  reproducer, the sound rule for a port writer is stated (omit whenever either
  reading would omit), and the resolution is marked open. Cites the probe.
- **§ 5 row 5's gate is restated.** "Snapshot byte-comparable modulo the
  excluded header" is unmeetable while `unresolved:` carries typer reason codes,
  with the 19 %-vs-89–100 % fixture blind spot named; the replacement is the
  subset/byte-equal/one-way-bit/ungraded-`unresolved:` gate the tool now
  measures, with today's 87 % recorded beside it. Cites the probe.
- **Entailed, and beyond the two places asked for: § 3's one sentence about the
  header.** It said the header "(`rigor:` version, `vocabulary:`,
  `config_digest:`) is **excluded**", which the narrowing makes false and which
  would have contradicted the restated § 5 gate three paragraphs later. Replaced
  in place with the per-field position and a dated note. No other § 3 text moved.

---

## 7. Gates

Run bare, from the worktree.

| gate | verdict |
|---|---|
| `cargo test --workspace` | **PASS** |
| `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy --workspace --all-targets -- -D warnings` | **PASS** — exit 0, fresh target dir |
| `harness/effects_diff.py --self-test` | **PASS** — 28 controls, byte-exact re-derivation on all 11 projects, all-MATCH |
| `harness/effects_diff.py` (default set, incl. `mastodon/app`) | **PASS** — `MATCH=5565 UNDER=2001 OVER=0 DM=0`; four pinned projects hold at 16/0/0/0 · 10/5/0/0 · 9/2/0/0 · 0/4/0/0 |
| …its SNAPSHOT half | **FAIL, separately and non-gating** — `220 / 335 / 2 / 0 DM / 0 HEADER`, the two § 3 rows |
| `harness/effects_diff.py --scale` | see § 7a |
| `rigor check` vs the master baseline, `mastodon/app` | **PASS** — stdout byte-identical (`sha256 5c7a7e07…`, 420 lines), stderr empty, exit 1 both |
| `harness/run_snapshot.rb` | **PASS** — 407 matched, 35 gaps, 2 registered divergences, **0 unregistered** |
| `harness/docs_check.py` | **PASS** — 4 budgets, links resolve |
| `harness/gen_effects_posture_corpus.py --check` | **PASS** |
| `harness/gen_effects_mutator_corpus.py --check` | **PASS** |
| residue in the user's corpus checkouts | **none** — see § 7b |

The measured binary is `sha256 21486bdd…`, identical to the master baseline
taken before the branch: no `crates/` file is touched, so `rigor check`'s
byte-identity is structural here rather than merely observed.

### 7a. `--scale`, and the frequency of the new class

`gitlab-foss/lib` is the standing set's strongest member — every slice-4 arm
that was 0 OVER on the fixtures AND on mastodon still leaked here. On the
snapshot it is **clean**:

| project | oracle rows | port rows | snapshot |
|---|---|---|---|
| `gitlab-foss/lib` | 4,543 | 4,123 | 1,467 / 1,300 / **0** / 1,776 |
| `--scale` TOTAL | 6,105 | 5,606 | **1,687 / 1,635 / 2 / 2,783** |

Report half unchanged at `20,990 / 7,617 / 0 / 0`; `--scale TOTAL MATCH=26555
UNDER=9618 OVER=0 DM=0` (the s112 figure of 9,612 plus the 6 UNDER that #114's
two fixtures added).

So across **36,167 oracle methods and 6,105 snapshot rows, the class fires
exactly twice**, both in one fixture. Frequency is not the argument for it: the
class is *unbounded* in principle — 807 extra-taint rows on gitlab and 161 on
mastodon are each one surviving `mutate.local` label away from being another
instance — and it is the only class the standing gate cannot express at all.
`0 OVER` on gitlab is also the answer to "is this comparison merely noisy on
real code": it is not.

### 7b. Residue

`rigor effects update` overwrites its target with no guard of any kind (probe
§ 5b), and for a FIXTURE project that target is a file in this repo's working
tree. `project_dir` makes a real corpus residue-proof by copying it; a fixture
is measured in place, so the defence is a `_preserving()` context manager that
restores the snapshot path to exactly its prior state — bytes, or absence — on
the exception path too. Verified after every run in this session:

- `git status --short --untracked-files=all` in this worktree lists only the
  three files this branch changes; no `.rigor-effects.yml` anywhere under
  `harness/effects-corpus/`.
- `/Users/megurine/repo/ruby/mastodon/` and `…/gitlab-foss/` carry only their
  own pre-existing artifacts (`.rigor/`, `.rigor-baseline.yml`,
  `.rigor.dist.yml`, `.rigor.yml`); nothing matching `.rigor*` exists under
  `mastodon/app` or `gitlab-foss/lib`.
- no `rigor-effects-*` temp project survives in `$TMPDIR`.

---

## 8. Deviations, with reasons

1. **The snapshot comparison does not gate the exit code.** § 5. It is a dated
   decision with a switch (`--snapshot-gate`) and a stated flip condition, not a
   permanent posture.
2. **ADR-0043 § 3's header sentence was edited, which is a third place.**
   Entailed by the narrowing the issue asks for, and leaving it would have made
   the ADR contradict itself within three paragraphs (§ 6).
3. **The comparison costs one extra oracle run per project** (`effects update`,
   ~10 s on mastodon; the default set goes from ~22 s to ~33 s). The `--full`
   snapshot is fetched only under `--self-test`, where the re-derivation needs
   it, so the standing run pays for one run and not two. `--no-snapshot` skips
   it entirely.
4. **`--scale` was measured but is not a gate here.** § 7a; the task's gate list
   names the default set.
5. **`docs/CURRENT_WORK.md` is not touched.** The ledger fold is the
   orchestrator's own commit by convention, and `harness/docs_check.py` passes
   as-is.
6. **The `reach:` half is guarded, not implemented.** No project in the standing
   set opts in, and the port has no transitive lane; scoring reach rows as
   under-claims would describe a surface that does not exist (normalisation 5).
7. **PyYAML is now needed for a fixture-only run.** `config_digest` is a
   function of the project's parsed `effects:` block and `vocabulary:` of the
   vendored registry, so the snapshot half imports `yaml` — previously only the
   real-corpus path did. `--no-snapshot` restores the dependency-free run.
