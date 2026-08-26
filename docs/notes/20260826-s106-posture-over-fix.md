# S106 — the posture tier is a live OVER: the gate first, then the fix (2026-08-26)

Closes issue #106. Branch `claude/s106-posture-over`, cut from `1925537`
(master). Two deliverables, landed in this order:

1. **`harness/effects-corpus/05_posture`** — a GENERATED corpus project that
   catches this whole class of over-claim. It **FAILS on the master binary with
   `OVER=7`**, which is the point: no gate in the repo saw the defect before it.
2. **The fix** — the collector drops the POSTURE tier for every receiver
   (`crates/rigor-cli/src/effects/collect.rs`). Rows and universal selectors are
   consulted first and are untouched. `rigor-infer` / `rigor-index` ingestion is
   not touched (ADR-0043 § 1).

Everything measured against the PINNED submodule at `b10bd5df` (v0.3.4),
populated into this worktree with `git submodule update --init reference/rigor`
(never the network, never `REFERENCE_RIGOR_DIR`), with `.rigor/cache` cleared
either side of every oracle run — which `harness/effects_diff.py` does itself.

---

## 1. The defect, restated from what the oracle actually says

`posture_allowed?` is `!implicit && !record&.dynamic && !DEFERRED_SELECTORS…`
(`unit_scan.rb:429`). The gate is on how the receiver was **TYPED**, not on how
it was **SPELLED** — and the reference cannot resolve every constant its own
catalogue names. Measured on `05_posture`, the oracle marks exactly eight of the
eighty catalogued classes non-exhaustive with a `dynamic-receiver` cause and
proves **∅** for them:

```
Fiddle::Function  Fiddle::Handle  Net::FTP  Net::HTTP  Net::SMTP
OpenSSL::SSL::SSLSocket  PTY  SOCKSSocket
```

Slice 2's licence for keeping the tier ("with the typer arm declined the posture
only ever fires on a constant both engines spell from the same syntax",
`collect.rs` module docs) does not follow from that rule, and the shipped binary
proved seven labels the oracle does not. `Fiddle::Function` is the eighth: its
posture is `value` (∅), so it costs nothing until slice 3 turns the
exhaustiveness bit on, when it becomes an OVER too.

---

## 2. The gate — `harness/effects-corpus/05_posture`, generated

`harness/gen_effects_posture_corpus.py` writes the whole project (`posture.rb`
and `.rigor.yml`) from `crates/rigor-effects/vendor/effects/core.yml`, and
`--check` re-derives it in memory and compares bytes — the same drift-gate shape
as `harness/vendor_effects.py --check`, one layer up.

**Why generated.** The eight classes are a function of the reference's RBS
environment at the pin, and the catalogue moves with the pin too. A hand-list
would freeze today's eight and go quietly stale — the exact failure mode
`harness/sweep-corpora.yml` exists to prevent. Nothing in the generator encodes
*which* constants the reference resolves; it probes **every** catalogued class
and lets the differential report the answer, so a re-vendor that adds a class
adds its probe for free.

**Why the VENDORED catalogue and not `reference/rigor/data/effects/core.yml`.**
The vendored bytes are what the measured binary `include_str!`s. Probing those
grades the catalogue actually under test; `vendor_effects.py --check` is what
keeps them equal to the pin.

133 methods in three sections, all derived:

| section | n | what it is |
|---|---|---|
| `Posture#c_*` | 80 | one method per class carrying a `posture:`, calling `zz_uncatalogued_zz` — a selector no row and no `universal:` name claims, so only the posture tier can answer |
| `Row#r_*` | 19 | **must-still-fire control**: the class's first identifier-spelled row with plain non-empty `effects:` (no `narrow:`, no `mutates:`). Includes `Net::HTTP.get` — a row on a class the oracle CANNOT resolve, which the oracle still proves `io.net.http` |
| `Universal#u_*` | 34 | **must-still-fire control**: on every class with a non-empty posture, the first `universal:` name it does not row. The oracle proves ∅ for all 34 — the universal tier beats the posture — so a posture leaking past it would show as an OVER here |

Oracle facts on this project, dumped directly (`--full --format=json`): 133
methods, 46 proven labels = **26 posture + 20 row + 0 universal**, and the only
non-exhaustive methods are the eight `Posture#c_*` above.

### 2a. The honest limit of the two control sections

`effects_diff.py`'s verdict is `0 OVER`; a *lost* label is UNDER, which never
fails. So the corpus controls cannot, on their own, make "delete the whole
catalogue tier" fail the differential — they make it **visible** (the port's
proven-label count on this project drops 20 → 0 and `UNDER by kind` flips from
`missing-label` to nothing). The mechanical must-still-fire assertion is
therefore also spelled where it can be fatal: the new crate unit test
`the_row_and_universal_tiers_still_answer_with_the_posture_off`
(`collect.rs`) fails the build if either tier stops answering. When slice 3
turns the exhaustiveness bit on, all 53 control methods become MATCH and the
corpus half becomes an assertion too.

---

## 3. Verdict tables

### 3a. BEFORE — master binary (`1925537`), with the new project in place

```
01_core_origins  oracle=16 / 12 labels   rigor-rs=16 / 12   MATCH= 3 UNDER=13 OVER=0 DM=0
02_propagation   oracle=15 / 12 labels   rigor-rs=15 /  9   MATCH= 4 UNDER=11 OVER=0 DM=0
03_taint         oracle=11 /  3 labels   rigor-rs=11 /  2   MATCH= 5 UNDER= 6 OVER=0 DM=0
04_declared      oracle= 4 /  1 labels   rigor-rs= 0 /  0   MATCH= 0 UNDER= 4 OVER=0 DM=0
05_posture       oracle=133/ 46 labels   rigor-rs=133/ 53   MATCH= 1 UNDER=125 OVER=7 DM=0
TOTAL            MATCH=13  UNDER=159  OVER=7  DM=0   => FAIL
```

**The recorded pre-fix OVER count on the new gate project is 7**, and it names
the seven classes with a non-empty posture out of the eight:

```
OVER: Posture#c_fiddle__handle          ['ffi']
OVER: Posture#c_net__ftp                ['io.net']
OVER: Posture#c_net__http               ['io.net.http']
OVER: Posture#c_net__smtp               ['io.net']
OVER: Posture#c_openssl__ssl__sslsocket ['io.net']
OVER: Posture#c_pty                     ['io.process']
OVER: Posture#c_sockssocket             ['io.net']
```

The four pre-existing projects are unchanged at `MATCH=12 UNDER=34 OVER=0 DM=0`,
i.e. the figure this arc has been carrying.

### 3b. AFTER — the fix

```
01_core_origins  oracle=16 / 12 labels   rigor-rs=16 / 12   MATCH= 3 UNDER=13 OVER=0 DM=0
02_propagation   oracle=15 / 12 labels   rigor-rs=15 /  9   MATCH= 4 UNDER=11 OVER=0 DM=0
03_taint         oracle=11 /  3 labels   rigor-rs=11 /  2   MATCH= 5 UNDER= 6 OVER=0 DM=0
04_declared      oracle= 4 /  1 labels   rigor-rs= 0 /  0   MATCH= 0 UNDER= 4 OVER=0 DM=0
05_posture       oracle=133/ 46 labels   rigor-rs=133/ 20   MATCH= 8 UNDER=125 OVER=0 DM=0
TOTAL            MATCH=20  UNDER=159  OVER=0  DM=0   => PASS
```

Per-project UNDER by kind, after: 01 `{extra-taint 13}` · 02 `{extra-taint 9,
missing-label 2}` · 03 `{extra-taint 5, missing-label 1}` · 04
`{absent-method 4}` · 05 `{extra-taint 99, missing-label 26}`.

**The three graded corpora did not move at all**: `12 MATCH / 34 UNDER / 0 OVER
/ 0 DM` before and after, exactly as `20260826-effects-s3-probe.md` § 5c
predicted ("cost, measured: zero on the graded corpus"). The movement is
confined to the new project, where OVER 7 → 0, MATCH 1 → 8 (the eight classes
now agree at ∅ with the oracle's own non-exhaustive ∅), and the port's proven
labels drop 53 → 20 — the 20 being **exactly** the oracle's row-control total,
so no control lost a label.

`--self-test` (the instrument's own gate) is all-MATCH on all five projects,
including the new one: 16 / 15 / 11 / 4 / 133.

---

## 4. The measured cost of the fix

**Graded corpus: zero, at the byte level.** The port's `effects --full
--format=json` output for `01_core_origins`, `02_propagation`, `03_taint` and
`04_declared` is **byte-identical** between the master baseline binary and the
fixed one. Only `05_posture` differs.

**mastodon/app (6,948 methods, 1,236 files), both binaries, same project
(`paths: [app]`):**

| | master | fixed |
|---|---|---|
| methods | 6,948 | 6,948 (key sets equal) |
| proven labels | 1,425 | **1,420** |
| MATCH | 4,517 | **4,514** |
| UNDER | 2,431 (missing-label 1,486 · extra-taint 945) | 2,434 (missing-label 1,489 · extra-taint 945) |
| OVER | 0 | **0** |

Five methods lose a label; three of them were MATCH:

```
AttachmentBatch#remove_files                     [io, io.fs]                   -> [io]
DomainResource#mx                                [io.net]                      -> []
EmailMxValidator#resolve_mx                      [io.net]                      -> []
Request::Socket.open                             [io, io.net]                  -> [io]
UpdateMediaAttachmentsPermissionsService#call    [global.read, io.fs, io.fs.write] -> [global.read, io.fs.write]
```

Every one is a strict UNDER — the safe direction — and two of the five were
already UNDER for another reason, which is why MATCH moves by 3 and not 5.

---

## 5. The change, in three lines of behaviour

`claimed_by_catalogue` now passes `posture: false` to `Catalog::resolve`
unconditionally. Consequences, each checked:

- **`DEFERRED_SELECTORS` is deleted.** Its only reader was the posture gate
  (`send` / `public_send` / `__send__` / `call` must not be answered by a class
  default); with no posture tier there is nothing to defer.
- **The posture half of `mutating_catalogued?` is deleted.** Upstream's rule is
  `mutates_receiver? || (posture? && mutating?(node, owner))`, and no entry this
  lookup can now return has `posture?` set. It was **already inert**: that arm
  only ever fired on a CONSTANT-path receiver, and
  `MutationClassifier::label_for` never classifies a constant (it recognises
  only nil/self, ivar and cvar reads, parameters and frame-owned locals), so it
  could never produce a label. The mutator-set data
  (`crates/rigor-effects/vendor/effects/mutators.yml`) and its crate tests are
  untouched — they are a faithful port of a pinned surface and slice 3+ owns
  whether anything reads them again.
- **`Resolution::Posture` is now unreachable from the collector.** The match arm
  is kept beside `Resolution::Universal` (they read identically) rather than
  turned into an `unreachable!()`: re-introducing a tier would be a decision, not
  a panic.

The module docs are rewritten to say why the tier is off, and to record the two
rejected alternatives — an allow-list of the constants the reference resolves (a
function of ITS RBS environment, which moves with the pin *and* with the
machine's installed gems: the `UNBUILDABLE_DEFINITIONS` hazard), and consuming
rigor-rs's own typer (the port is deliberately more robust than the reference
where it degrades to `untyped`, and `untyped == Dynamic`, so its answer moves in
exactly the unsafe direction).

---

## 6. Gates

| gate | verdict |
|---|---|
| `cargo test --workspace` | **PASS** — 1,252 tests, 0 failed (incl. the two rewritten / new collector tests) |
| `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy --workspace --all-targets -- -D warnings` | **PASS** — exit 0, fresh target dir |
| `harness/effects_diff.py --self-test` | **PASS** — all-MATCH on all five projects |
| `harness/effects_diff.py` (full corpus, incl. `05_posture`) | **PASS** — `MATCH=20 UNDER=159 OVER=0 DM=0` (§ 3b) |
| `rigor check` vs the master baseline, mastodon/app (1,236 files) | **PASS** — stdout + stderr + exit code byte-identical, default threads AND `RAYON_NUM_THREADS=1`; the new binary's two thread modes agree (420 stdout lines, exit 1) |
| `harness/run_snapshot.rb` | **PASS** — 98 fixtures, 407 matched, 35 gaps, 2 registered divergences, **0 unregistered** |
| `harness/docs_check.py` | **PASS** — 4 budgets, links resolve |
| `harness/gen_effects_posture_corpus.py --check` | **PASS** — committed corpus matches the vendored catalogue exactly |

---

## 7. Deviations, with reasons

1. **The must-still-fire control is split across two places.** The issue asks
   for row and universal methods in the corpus so the fix cannot be "delete the
   whole tier"; they are there (53 of the 133 methods), but the differential's
   verdict is `0 OVER` and a deleted tier reads as UNDER, so the corpus half is
   *visible* rather than *fatal* today. The fatal half is the new crate unit
   test. § 2a.
2. **The measured mastodon cost is 5 label losses / 3 MATCH, not the probe's
   "4 of 6,948".** Different arm: the probe's 5,238 → 5,234 was measured on the
   simulated T2 collector (posture drop *plus* slice 3's taint rules), where a
   method's MATCH also depends on the exhaustiveness bit. On the shipped slice-2
   lane the same posture drop moves 5 proven lanes, 3 of which were MATCH. Both
   numbers describe the same change; ours is the one that describes THIS binary.
3. **The generator writes `.rigor.yml` as well as `posture.rb`.** It costs three
   lines and makes `--check` cover the whole project, so a hand-edit to the
   config is caught the same way a hand-edit to the corpus is.
4. **Operator-spelled rows are excluded from the row controls** (`[]`, `[]=`,
   `<<`). They need their own call syntax and `01_core_origins` already carries
   `ENV["HOME"]`; a control should fail for one reason only. Same reasoning
   excludes `narrow:` rows (labels depend on literal arguments and on whether
   the port implements the handler) and `mutates: receiver` rows (the reading is
   the ownership judgment, which `01_core_origins` owns).
5. **The standing 9,204-file sweep was not re-run.** The gate list named
   byte-identity on mastodon/app for the ADR-0043 § 1 obligation, and the
   collector is not reachable from `check` at all — no `crates/rigor-infer` or
   `crates/rigor-index` file is touched by this branch. The fixture snapshot
   harness (98 fixtures, 0 unregistered FPs) is the second half of that evidence.
