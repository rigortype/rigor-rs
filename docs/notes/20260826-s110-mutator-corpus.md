# S110 — the mutator sets had no corpus: the gate, and what it found (2026-08-26)

Closes issue #110. Branch `claude/s110-mutator-corpus`, cut from `c8633f1`
(master). One deliverable: **`harness/effects-corpus/07_mutators`**, a GENERATED
corpus project, plus its generator and drift gate.

**No production code changed.** The measurement it was built to force came back
`0 OVER`, so there was nothing to fix — the finding is a coverage one, recorded
in § 5 and deliberately left for a slice that predicts it first.

Everything measured against the PINNED submodule at `b10bd5df` (v0.3.4),
populated into this worktree from the parent checkout's tree (never the network,
never `REFERENCE_RIGOR_DIR`), with `.rigor/cache` cleared either side of every
oracle run — which `harness/effects_diff.py` does itself.

---

## 1. The blind spot, measured

`crates/rigor-effects/vendor/effects/mutators.yml` vendors **72 (set, selector)
pairs — 53 distinct selectors** across `array` (31), `hash` (15) and `string`
(26), extracted from upstream's `%i[…]` literals by `harness/vendor_effects.py`.
The whole pre-existing `harness/effects-corpus/` reaches **4 of the 53**:

```
<<        01_core_origins      []=       01_core_origins
upcase!   01_core_origins      clear     05_posture
```

That is the #106 shape exactly — a vendored data table the collector consumes,
with no corpus generated FROM the table — and #106 is what happens when nobody
looks: the generated project found seven live over-claims in shipped code on its
first run.

### 1a. What the sets actually reach in the shipped binary

Traced before the corpus was written, and it is sharper than "thinly covered".
There are three readers of `rigor_effects::mutators()`:

| reader | reachable in the shipped port? |
|---|---|
| `MutationClassifier::mutating` (`ownership.rs:106`) | **No.** Its only production call site is `collect.rs:676`, which passes `receiver_class: None` — the collector declines upstream's typer arm — and the `None` case returns before the set lookup. |
| `Catalog::resolve`'s `in_mutator_set` → `mutating_posture_entry` (`catalog.rs:496`) | **No.** It sits behind `if !posture { return None }`, and production passes `posture: false` unconditionally (#106). |
| `build_row`'s `mutates_receiver = mutates == "receiver" \|\| in_mutator_set` (`catalog.rs:657`) | **Yes, at build time — and inert.** Only `Array` / `Hash` / `String` carry a `mutators:` key; of their three rows only `Array#shuffle!` is in its own set, and it *also* declares `mutates: receiver`. It is an INSTANCE row, so the port's syntax-only `catalog_target` cannot reach it either. |

So the 72 selectors change **nothing** the shipped binary decides. That is a
correct consequence of declining the typer (ADR-0043 § 1 / the #106 fix), not a
bug — but it was undocumented and unmeasured, and "the data is inert" is exactly
the claim a gate should be pinning rather than a reader should be re-deriving.

---

## 2. The gate — `harness/effects-corpus/07_mutators`, generated

`harness/gen_effects_mutator_corpus.py` writes the whole project (`mutators.rb`
and `.rigor.yml`) from the VENDORED `mutators.yml`, with `core.yml` supplying the
two catalogue-derived control sections. `--check` re-derives it in memory and
compares bytes — the same drift-gate shape as `gen_effects_posture_corpus.py` and
`vendor_effects.py --check`.

**Why generated.** The sets move with the pin: `vendor_effects.py` re-extracts
them from upstream's Ruby on every re-vendor. A hand-list would freeze today's 72
and go quietly stale. Nothing in the generator encodes what either engine
*answers*; every selector in the vendored file gets the same probes and the
differential reports the verdict, so a re-vendor that adds a selector adds its
probes for free. It is also **total by assertion** rather than by filtering: an
unknown set name, an unspellable selector, a slug collision, or `[]=` leaving any
set all `sys.exit` rather than silently dropping probes.

**383 methods in eight sections, all derived:**

| section | n | what it is |
|---|---|---|
| `Owned#*` | 72 | one method per (set, selector): a local seeded by the set's own literal and never let out, so the reference's typer names the class AND `LocalOwnership#owned` proves the frame owns it. The one shape in which upstream proves `mutate.local` for a set member. A `nil` tail is load-bearing — a trailing bare read is an escape (`local_ownership.rb:122`) |
| `Unowned#*` | 72 | the same seed and call handed to the caller by that trailing read: the type is known and the ownership is not, so upstream answers ∅ + `unknown-ownership` and **never a bare `mutate`** |
| `Ivar#*` | 72 | an `@ivar` receiver, seeded in the body so the reference can type it |
| `Konstant#*` | 72 | a constant receiver, seeded at the top of the file. `label_for` recognises nil/self, ivar and cvar reads, parameters and frame-owned locals — a constant is none of them, so ownership is never provable however precisely the receiver was typed |
| `TypeFree#*` | 56 | the type-free half: `[]=` (a member of all three sets, asserted) and its `ATTRIBUTE_WRITER` twin `slot=`, across seven receiver shapes × eight write spellings — the two plain calls plus the six compound-write Prism node types the collector branches on individually (`collect.rs:574-588`) |
| `TypeFreeSingleton#*` | 24 | the same, inside `class << self`, where self and its ivars flip to `mutate.static` and a local does not |
| `Rowed#*` | 10 | **must-still-fire control**: every catalogued row whose selector is in a vendored set and whose owner a CONSTANT receiver can spell — `ENV#store` / `#delete` / `#update` / `#merge!` / `#replace` / `#clear` / `#[]=`, `File.delete`, `Dir.delete`, `Warning.[]=`. A row is authoritative and is not a receiver mutation unless it says so |
| `Suppressed#*` | 5 | **must-still-SUPPRESS control**: the rows behind `NON_MUTATING_ROWED_SELECTORS` (`collect.rs:185`) — `ENV#[]=`, `Thread#[]=`, `Warning.[]=`, `Encoding.default_external=`, `Encoding.default_internal=`. The receiver is made provably-owned wherever the class has an allocator, which is what makes the suppression falsifiable (§ 4) |

`effects_diff.py --self-test` (the instrument's own gate) is all-MATCH on the new
project: 383 of 383.

---

## 3. The verdict table this exposed

Nothing in it is a *consequence* of this branch — the port binary is byte-identical
to master's. It is the pre-existing state, first made visible.

### 3a. The whole corpus, on the master binary

```
01_core_origins  oracle=16 / 12 labels   rigor-rs=16 / 12   MATCH= 16 UNDER=  0 OVER=0 DM=0
02_propagation   oracle=15 / 12          rigor-rs=15 /  9   MATCH= 10 UNDER=  5 OVER=0 DM=0
03_taint         oracle=11 /  3          rigor-rs=11 /  2   MATCH=  9 UNDER=  2 OVER=0 DM=0
04_declared      oracle= 4 /  1          rigor-rs= 0 /  0   MATCH=  0 UNDER=  4 OVER=0 DM=0
05_posture       oracle=133/ 46          rigor-rs=133/ 20   MATCH= 61 UNDER= 72 OVER=0 DM=0
06_edge          oracle=  6/  1          rigor-rs=  6/  0   MATCH=  5 UNDER=  1 OVER=0 DM=0
07_mutators      oracle=383/233          rigor-rs=383/149   MATCH=219 UNDER=164 OVER=0 DM=0
TOTAL            MATCH=320  UNDER=248  OVER=0  DM=0   => PASS
```

The four PINNED projects are cell for cell on their numbers (01: 16/0/0/0 · 02:
10/5/0/0 · 03: 9/2/0/0 · 04: 0/4/0/0), and `05_posture` / `06_edge` are unmoved.

### 3b. `07_mutators`, by section

| section | n | MATCH | UNDER missing-label | UNDER extra-taint | OVER |
|---|---|---|---|---|---|
| `Owned` | 72 | **0** | **72** | 0 | 0 |
| `Unowned` | 72 | 71 | 1 | 0 | 0 |
| `Ivar` | 72 | 0 | 1 | 71 | 0 |
| `Konstant` | 72 | **72** | 0 | 0 | 0 |
| `TypeFree` | 56 | 47 | 3 | 6 | 0 |
| `TypeFreeSingleton` | 24 | 18 | 2 | 4 | 0 |
| `Rowed` | 10 | **10** | 0 | 0 | 0 |
| `Suppressed` | 5 | 1 | 4 | 0 | 0 |
| **total** | **383** | **219** | **83** | **81** | **0** |

Every lost label, by shape:

```
Owned            71x [mutate.local]                 the typed mutator lane, entire
Owned             1x [mutate.local, nondet.random]  a_shuffle_bang: the Array#shuffle! ROW too
Unowned / Ivar    1x [nondet.random] each           the same row, on the other two shapes
TypeFree          1x [mutate.local] 1x [mutate.instance] 1x [mutate.self]
TypeFreeSingleton 1x [mutate.local] 1x [mutate.static]
Suppressed        4x [global.write]
```

**`Konstant` is 72/72 MATCH and `Rowed` is 10/10** — the two sections whose
answer does not depend on a receiver class agree completely. Every one of the
five `TypeFree*` missing-label rows is the *plain* `[]=` call
(`*_index_set`); all six compound-write spellings and all four attribute-writer
spellings MATCH on every receiver shape.

---

## 4. Does it bite? Two variant builds

`0 OVER` on a fresh gate is only worth something if the gate can produce one.
Both variants were built into a fresh `CARGO_TARGET_DIR` and graded with the same
instrument, one line changed each.

**(a) drop the `[]=` suppression** — `visit_uncatalogued` stops consulting
`NON_MUTATING_ROWED_SELECTORS`:

```
07_mutators   MATCH=220  UNDER=163  OVER=1
  OVER: Suppressed#s_thread_index_set — proven labels not proven by the oracle: ['mutate.local']
TOTAL  MATCH=321  UNDER=247  OVER=1  => FAIL
```

`recv = Thread.new; recv[0] = 1` is the whole case: upstream reaches the
`Thread#[]=` row through the typer and proves `global.write` — *not* a receiver
mutation — while a port without the suppression proves `mutate.local` off the
owned local. **Projects 01–06 all stay at 0 OVER**; `07_mutators` is the only
thing in the repo that sees it, and until now the suppression had no gate at all
(it was argued in a comment, and the ledger records the reasoning as "the subset
argument failed a FOURTH time").

**(b) guess an owner** — `label_for` answers `Ownership::Local` instead of `None`
where ownership is unprovable, i.e. the port emits a `mutate` on a
fresh-but-unproven receiver:

```
07_mutators   MATCH=205  UNDER=164  OVER=26
  14 methods, all TypeFree#escaping_* and TypeFree#konstant_*
  (12 x "proven labels not proven by the oracle: ['mutate.local']"
   + 14 x "claims exhaustiveness the oracle does not")
TOTAL  MATCH=306  UNDER=248  OVER=26  => FAIL
```

Again `07_mutators` alone: **01–06 stay at 0 OVER.**

### 4a. The honest limit

The `Unowned` and `Konstant` sections (144 methods) are MATCH controls with **no
teeth today**, and variant (b) shows why: their selectors are typed mutators, so
`mutating(selector, None)` is false and `classify_mutation` is never reached —
there is nothing there to over-claim *yet*. They become load-bearing the moment
any slice gives the collector a receiver class, which is precisely the slice that
would want them. The same caveat the #106 note records for its control sections
(§ 2a there); it is recorded rather than papered over.

---

## 5. The UNDER classification — what would close it, and why not now

Two independent causes, both already decided elsewhere. Neither is chased here:
the task's rule is that a coverage slice needs its own prediction first, and both
of these would change what the port *proves*, which is the fatal direction.

**(i) 72/72 `Owned` — the typed mutator lane needs a receiver class (all 83
missing-label rows but 4).** `mutating(selector, receiver_class)` claims `<<`,
`push`, `upcase!` and the rest only when the receiver's class is known, because
`n << 2` is a bit shift and `io << "x"` is output. The collector declines
upstream's `record.receiver_class` arm wholesale (module docs; ADR-0043 § 1), so
it always passes `None`. Closing this means one of:

- *consume `rigor-infer`'s answer* — the road #106 rejected in the posture case
  for a reason that applies unchanged here: rigor-rs is deliberately MORE robust
  than the reference where the reference degrades to `untyped`, and
  `untyped == Dynamic`, so its answer moves in the unsafe direction. Worse here
  than there, because a receiver-class answer feeds a PROVEN `mutate.*` label
  rather than a class default.
- *a syntax-only literal-seed rule* — "a local whose every assignment is an
  `ArrayNode` / `HashNode` / `StringNode` is that class". `LocalOwnership` already
  computes almost exactly this set (`allocation?`), so the increment is small and
  it needs no typer. It is a **sound-superset** claim about Ruby, not about the
  reference, and ADR-0043 § 2 inverts the sound-superset licence — so it needs a
  probe and a prediction, not an improvisation. It would close the 72 `Owned`
  rows and nothing else (the `Unowned` / `Konstant` / `Ivar` shapes are ∅ on both
  sides for other reasons).

**(ii) 5 `TypeFree*` + 4 `Suppressed` rows — the blanket `[]=` suppression.**
`[]=` is suppressed for EVERY uncatalogued receiver because upstream can route it
to `ENV#[]=` / `Thread#[]=` / `Warning.[]=` through the typer, where the answer is
`global.write` and not a mutation. Measured cost, exactly: `mutate.local` on an
owned local, `mutate.instance` on a parameter, `mutate.self` on `self`,
`mutate.static` in a singleton unit — one row each — plus the four `global.write`
labels the port cannot prove at all. Two narrowings look safe and neither is
taken here:

- restrict the suppression to receivers that *could* hold one of those three
  objects. They are all constants, and a constant-path receiver is already named
  by syntax and answered by the row — so the residue is locals, ivars and `self`.
  A `self` receiver in a project class body is not `ENV`; a local can be
  (`env = ENV`, the documented case) and an ivar can be.
- suppress the LABEL but keep the ownership question, so the compound-write forms
  stay symmetric with the plain call.

Both change what the port proves and are exactly the "would this OVER?" question
the orchestrator owns. Recorded, not built.

**Not a cause: the 81 extra-taint rows.** They are slice 3's blanket
`dynamic-receiver` at every uncatalogued call with an explicit receiver, already
priced and deliberate (`20260826-effects-s3-impl.md` § 5.6). `Ivar` is 71 of
them, which is why that section reads 0 MATCH while its labels agree exactly.

---

## 6. What was hunted and did not exist

The OVER hunt ran before the corpus was frozen, on two scratch projects (removed
after measurement), because a gate that reports `0 OVER` should say what it
looked for:

- **`owned_locals` divergence.** The port is a line-by-line port of
  `local_ownership.rb`, but "line-by-line" is a claim. 33 shapes probed against
  the oracle — literal / `+""` / interpolated / lambda / `.dup` seeds, escape via
  keyword, splat, interpolation, `yield`, block-pass, `return`, ivar, array
  element, aliasing and the trailing read, reassignment to a non-allocation,
  op-writes, multi-assign, block-shadowed names, safe navigation, seeds inside
  blocks, and the three singleton spellings. **Every proven label agreed**; every
  difference was the exhaustiveness bit.
- **`ATTRIBUTE_WRITER` divergence** — the port's hand-expanded predicate vs
  upstream's regex. Pinned by an existing crate test and agreed on every generated
  probe.
- **A row that a set membership could override** — `Rowed`, 10/10 MATCH.
- **A compound write routed through the catalogue on one side only** — upstream
  classifies all six node types unconditionally (`unit_scan.rb:215`) and so does
  the port; 40 generated probes agree.

---

## 7. Gates

| gate | verdict |
|---|---|
| `cargo test --workspace` | **PASS** — 1,263 tests, 0 failed (unchanged: no crate source touched) |
| `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy --workspace --all-targets -- -D warnings` | **PASS** — exit 0, fresh target dir |
| `harness/effects_diff.py --self-test` | **PASS** — all-MATCH on all seven projects (16 / 15 / 11 / 4 / 133 / 6 / **383**) |
| `harness/effects_diff.py` (full corpus incl. `07_mutators`) | **PASS** — `MATCH=320 UNDER=248 OVER=0 DM=0`; the four pinned projects unmoved (§ 3a) |
| `rigor check` vs the master baseline, mastodon/app | **PASS** — stdout + stderr + exit code byte-identical, default threads AND `RAYON_NUM_THREADS=1`; the branch binary's two thread modes agree (420 stdout lines, 57,681 bytes, exit 1) |
| `harness/run_snapshot.rb` | **PASS** — 98 fixtures, 407 matched, 35 gaps, 2 registered divergences, **0 unregistered** |
| `harness/docs_check.py` | **PASS** — 4 budgets, links resolve |
| `harness/gen_effects_mutator_corpus.py --check` | **PASS** — committed corpus matches the vendored sets exactly |
| `harness/gen_effects_posture_corpus.py --check` | **PASS** — `05_posture` still matches the vendored catalogue |

---

## 8. Deviations, with reasons

1. **No crate unit test was added.** #106 needed one because its corpus control
   was only *visible* (a lost label reads as UNDER, and the verdict is 0 OVER).
   Here both controls are **fatal in the differential itself**, measured: § 4's
   two variants score OVER 1 and OVER 26, and on `07_mutators` alone. A crate test
   would restate what the corpus already proves.
2. **The generator reads `core.yml` as well as `mutators.yml`.** The two control
   sections are about the INTERACTION between a set membership and a catalogue
   row — `Rowed` needs the rows in a set, `Suppressed` needs the rows behind
   `NON_MUTATING_ROWED_SELECTORS` — and deriving them from the catalogue is what
   keeps them from becoming the hand-list this project exists to avoid. Both
   files are vendored and both are `--check`-gated by `vendor_effects.py`.
3. **The four receiver shapes are per-(set, selector); the type-free forms are
   not.** `[]=` is the only set member that is type-free, and its
   attribute-writer twin is a REGEX in upstream, not a list — so a per-selector
   cross-product there would have been 72 copies of one probe. The generator
   instead asserts `[]=` is in every set and crosses the two type-free spellings
   with the receiver shapes and the six compound-write node types.
4. **A parameter receiver appears only in the type-free sections.** For a typed
   mutator it is ∅ on both sides (an untyped parameter has no receiver class), so
   it would have added 72 uninformative MATCHes; in the type-free sections it is
   load-bearing — it is the only route to `mutate.instance` in the whole corpus
   (`20260826-effects-s2-probe.md` § 7b said so and had no fixture for it).
5. **The standing 9,204-file sweep was not re-run.** No `crates/` file is touched
   on this branch and the port binary is byte-identical to the master baseline on
   mastodon/app in both thread modes; the fixture snapshot harness is the second
   half of that evidence. Same reasoning as the #106 note's deviation 5, with a
   stronger premise (there, one line of the collector changed; here, none).
6. **`harness/README.md` gained the second generator.** Its "One of them is
   **generated**" paragraph became false the moment this project landed, which is
   the kind of staleness the docs gate exists to prevent.
7. **No `docs/CURRENT_WORK.md` ledger line — it is OWED at merge, not on this
   branch.** The file is at **24,571 of its 24,576-byte budget**: five bytes. A
   line carrying verdict + numbers + link costs ~250, and the only place to
   reclaim it is the two 2026-08-26 effects lines this one extends. Lossless
   shorthand there yields ~150 (`35 MATCH / 11 UNDER / 0 OVER / 0 DM` → `35/11/0/0`,
   a duplicated ADR-0043 link); the remaining ~100 would have to come out of
   recorded detail, which is a fold decision the budget exists to force onto a
   person, not something to improvise inside a draft PR. Small doc-only changes
   go direct to master by convention anyway, so the ledger line belongs to the
   merge. **The one-line entry, ready to paste, folded into the existing
   2026-08-26 effects line after its `#106` clause:**

   > **Then #110**: the 72 vendored MUTATOR selectors had no corpus either (4 of
   > 53 reached) — generated `07_mutators` (383 methods), **219/164/0/0, no
   > defect**; the sets are INERT under `posture: false` + `receiver_class: None`,
   > and its two controls are fatal on 07 alone (no `[]=` suppression → OVER 1; a
   > guessed owner → OVER 26).
   > [#110](notes/20260826-s110-mutator-corpus.md)
