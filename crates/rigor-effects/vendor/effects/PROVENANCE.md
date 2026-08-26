# Vendored effect catalogue — provenance

This tree holds the **exact** two data files that make up Rigor's effect
catalogue, vendored into the repo so the analyzer is standalone
([ADR-0043](../../../../docs/adr/0043-effect-system-port-parity-model.md) slice
1), plus **one derived file** slice 2 added because upstream keeps its contents
as code rather than data. All three are embedded byte-for-byte with
`include_str!` from `crates/rigor-effects/src/lib.rs` (`REGISTRY_YML` /
`CORE_YML` / `MUTATORS_YML`) — no `build.rs`, no codegen — and parsed lazily on
first use.

Never hand-edit any of them: the two copies are **verbatim**, the third is
**generated**. A hand-edit fails `cargo test -p rigor-effects` on the digest
assertions below, and a drift against the pin fails
`python3 harness/vendor_effects.py --check`.

## Contents

### `registry.yml` — the label vocabulary

- **Source path:** `reference/rigor/data/effects/registry.yml` — **the PINNED
  submodule**, not a local checkout.
- **Vendored:** 2026-08-26 at the `v0.3.4` pin (`b10bd5df`).
- **`sha256`** `bb0eb3f08568bc52c47ce3caa75d22d359b0455b3182825906884797289d7104`
  (67 lines, 2,217 bytes).
- **What it is:** `vocabulary: 1`, **36 declared labels** in four commented
  groups (Steins v1 verbatim 25, Ruby's `mutate` leaves 3, proposed shared
  `io.db` leaves 3, application-meaning roots 5), and an EMPTY `retired:` table
  — the rename/removal compatibility mechanism, present and unused at
  vocabulary 1. Loaded upstream by `Rigor::Effects::Registry.load_file`
  (`lib/rigor/effects/registry.rb:71`).

  **The load-bearing subtlety:** `Registry#known?` is the declared rows ∪ every
  ANCESTOR of a declared row (`registry.rb:161`). Four of the ten roots —
  `global`, `email`, `job`, `cache` — exist ONLY as implied ancestors; no row
  spells them. `core.yml`'s `global` posture emits the bare `global`, so a
  reader that validates the catalogue against the 36 declared rows alone
  **rejects the shipped catalogue.**

### `core.yml` — the per-method catalogue

- **Source path:** `reference/rigor/data/effects/core.yml` — the PINNED
  submodule.
- **Vendored:** 2026-08-26 at the `v0.3.4` pin (`b10bd5df`).
- **`sha256`** `85778dd3433fcb5561a933c9b2b22fb07048af980e35f93091f545655bda9c31`
  (843 lines, 52,785 bytes).
- **Upstream's own identity anchor:** `1:85778dd3433fcb5561a933c9b2b22fb07048af980e35f93091f545655bda9c31`.
  Upstream's effects cache keys on `Catalog#identity` = `schema:sha256(core.yml)`
  (`lib/rigor/effects/catalog.rb:158`) — i.e. **upstream already treats this
  file's digest as the catalogue's identity**, so the provenance anchor and
  upstream's invalidation key are one number. `Catalog::identity()` reproduces
  it, and a test asserts the string.
- **What it is:** `schema: 1`, `vocabulary: 1`, **14 `defaults:` postures**, a
  **34-name `universal:` list**, and **80 classes / 420 rows** (216 instance,
  204 singleton). Loaded upstream by `Rigor::Effects::Catalog.load_file`
  (`catalog.rb:122`).

  Two spellings a reader must not normalise away: an explicit `effects: []` (77
  rows) is NOT the same as having no row, and the `<<` selector is spelt
  `!!str "<<"` because a bare — even quoted — `<<` key is YAML's MERGE key,
  which a resolving loader would splice into the enclosing `methods:` map. Four
  rows depend on it (`IO`, `File`, `SizedQueue`, `Logger`).

### `mutators.yml` — the three by-reference mutator sets (DERIVED)

- **Source paths:** `reference/rigor/lib/rigor/inference/mutation_widening.rb`
  (`ARRAY_MUTATORS`, `HASH_MUTATORS`) and
  `reference/rigor/lib/rigor/effects/mutation_classifier.rb`
  (`STRING_MUTATORS`) — the PINNED submodule.
- **Vendored:** 2026-08-26 at the `v0.3.4` pin (`b10bd5df`), ADR-0043 slice 2.
- **`sha256`** `5bd8091db9ce2cf593ffe6409154482a38c452967b5d0ad075403e5525915ed7`.
  This digests the GENERATOR'S OUTPUT, not an upstream file: `--check`
  regenerates the document in memory from the pinned Ruby and compares bytes, so
  a `%i[…]` literal that moves upstream fails the gate exactly as an edited copy
  would.
- **What it is:** `schema: 1` and three sets — **array 31**, **hash 15**,
  **string 26** selectors, each with the `lib/…: CONSTANT` it was lifted from.
- **Why it is derived rather than copied:** upstream has no data file for it.
  `core.yml` names the sets BY REFERENCE (`mutators: array`) and upstream's
  internal spec makes that normative — "The data file MUST NOT re-spell a
  selector list" (`docs/internal-spec/effect-summaries.md:169`) — because the
  widening rules and the effect model share one hand-audited list and must never
  drift apart. The port needs the contents: a selector in its class's set is a
  receiver mutation on the ROW path (`catalog.rb:259`'s `in_mutator_set`) AND on
  the POSTURE path (`catalog.rb:194`).
- **Extraction hazard, for whoever next reads the generator:** `[]=` is a member
  of all three sets and spells a balanced `[` `]` INSIDE the Ruby literal, which
  is how `%i[…]` reads it. The extractor counts bracket DEPTH; a regex stopping
  at the first `]` truncates `ARRAY_MUTATORS` at `fill` and silently drops 13
  selectors. The pinned counts are what catch that.

## The carve-outs — this copy is COMPLETE, not truncated

The data file deliberately does not re-spell two things, and a reader who sees
`mutators: array` with no selector list will otherwise assume the copy was cut
short. Both are **code, not data**; the second is out of ADR-0043's scope
entirely.

Slice 2 CLOSED the first of the three slice 1 recorded — the mutator sets, now
`mutators.yml` above. The set NAME is still what `core.yml` carries and what
`ClassEntry::mutator_set` reports; what changed is that the name is now
EXPANDED, so `Array#push` answers `mutates_receiver == true` as upstream does.

1. **The narrowing handler BODIES.** A row's `narrow:` names one of the **7**
   handlers in `Effects::Narrowing::HANDLERS`
   (`lib/rigor/effects/narrowing.rb:55`) — `kernel_open file_open
   pathname_open time_new random_new uri_open sql_verb`. `core.yml` uses **6**
   of them across **7** rows; `sql_verb` has no `core.yml` row at all and serves
   PLUGIN rows. This crate carries the handler NAMES (validated against that list
   at load, as upstream does) and none of the bodies, so `Catalog::lookup`
   answers a narrowed row's **unnarrowed** `effects:` — exactly upstream's own
   answer when it is handed no call node, and the sound upper bound the handler
   degrades to.

   Slice 2 implements the six bodies in `crates/rigor-cli/src/effects/`, not
   here, because they read a **Prism call node** and this crate depends on no
   crate of ours. `Catalog::resolve` is the seam: it hands the `Row` back
   un-collapsed so the consumer branches on `Row::narrow` itself. A consumer
   that reads `lookup` instead gets the parent label, which ADR-0043 § 2 grades
   as an OVER rather than as a coarser truth.
2. **The plugin effect layer.** `plugins/*/lib/rigor/plugin/*/effects.rb` (1,107
   lines across 9 Rails plugins) contributes its own rows, attributions, edges,
   entry-point presets and an `effect_labels:` root extension. It is the sole
   consumer of the 7th narrowing handler and of the `io.db.*` / `job.enqueue` /
   `email.send` / `cache.*` labels. ADR-0043 names it in no slice.

## Regenerate

**Re-sync at every pin bump** — `UPSTREAM.md` step 3, where this is the THIRD
pin-tracking surface alongside `crates/rigor-index/vendor/rbs/overlay/` and
`crates/rigor-index/vendor/plugins/`. The recorded cost of not doing so is not
hypothetical: the `activesupport-core-ext` copy sat unmoved for two months and
the drift was **10 live false positives** that neither sweep tool could see.

```sh
python3 harness/vendor_effects.py --check   # drift gate: exit 1 on ANY byte difference
python3 harness/vendor_effects.py           # re-vendor from the PINNED submodule
```

Then read the diff as a **semantic** change, not a copy. `retired:` gaining an
entry and `vocabulary:` bumping are the two that can invalidate a committed
`.rigor-effects.yml`; a `schema:` bump changes the row grammar; and a re-audit
that moves `IO#write` from `io` to `io.fs.write` changes every summary with no
source change on either side. A `mutators.yml` diff is the same kind of event
one layer down: a selector entering `ARRAY_MUTATORS` makes that call a proven
`mutate.*` in every project that makes it, so the generator REFUSES to write a
set whose size moved and the crate pins 31 / 15 / 26 in a test.

Never source this from a local rigor checkout — that is `UPSTREAM.md` hazard 3,
and the vendored plugin RBS is the recorded case of that hazard applied to a
file.

## What grades this tree

Three layers, and only the middle one is coverage-independent:

1. **The ported upstream data specs** — `crates/rigor-effects/tests/upstream_data_specs.rs`,
   a case-for-case port of upstream's `spec/rigor/effects/registry_data_spec.rb`
   and `catalog_data_spec.rb` over these bytes. The wholesale assertion is that
   every label all 420 rows and every posture can emit is in the grammar AND
   `Registry::known`.
2. **`harness/vendor_effects.py --check`** — byte-for-byte against the pinned
   submodule. Independent of what any corpus exercises, and the one that fails
   the instant the pin moves under an unchanged copy.
3. **The embedded-bytes digest assertion** — `src/lib.rs`'s
   `the_embedded_bytes_match_the_provenance_digests`, which catches a hand-edit
   of any of the three files under plain `cargo test`, in a checkout whose
   `reference/rigor` is empty. `mutators.rs`'s count test (31 / 15 / 26) sits
   beside it and is what a truncated extraction fails.

`harness/effects_diff.py` is the *behavioural* gate and grades **6 of the 420
rows (1.4 %)** today. It cannot be the drift gate, which is why layer 2 exists —
and slice 2's own composition probes (a scratch project measured against the
oracle, `docs/notes/20260826-effects-s2-impl.md`) exist for the same reason.
