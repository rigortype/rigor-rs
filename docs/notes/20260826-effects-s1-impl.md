# Effects slice 1 — the vendored catalogue crate SHIPPED (2026-08-26)

Implements [the slice-1 mini-spec](20260826-effects-s1-mini-spec.md) from
[the catalogue probe](20260826-effects-s1-catalogue-probe.md);
[ADR-0043](../adr/0043-effect-system-port-parity-model.md) § 5 slice 1, gated on
"catalogue parses; label subsumption unit-tested".

Headline: **the catalogue ships as `crates/rigor-effects`, with no consumer and
no behaviour change** — `rigor check` is byte-identical to a master-built binary
on mastodon/app, and every standing gate is green. The three-layer drift gate is
in, and both mechanical layers were proven to FIRE, not just to pass.

**One defect the slice's own shape created, fixed at the root:** a crate under
`crates/` that nothing links makes every harness's stale-binary guard abort
FOREVER — cargo never rebuilds `rigor` for it, so the binary's mtime can never
catch up. See § 4.

## 1. What shipped

`crates/rigor-effects` — `serde` + `serde_yaml` only, both already in
`Cargo.lock`, so **zero new dependencies** and `--offline`-safe. No `build.rs`,
no `Box::leak`, no codegen: the two files are `include_str!`d (the
`vendor/plugins/` precedent, not `vendor/rbs/`'s) and parsed lazily into a
`LazyLock`, exactly as upstream memoises `Catalog.default`.

**Nothing depends on it.** ADR-0043 § 1's "the effects work may not change
`rigor-infer`'s answers" is now a dependency-graph fact rather than a review
promise. The workspace `crates/*` glob picks the member up and `Cargo.lock`
gains entries; that is the whole footprint outside the new directory.

| surface | ported | source |
|---|---|---|
| `label::{valid, segments, subsumes, parent, ancestors, root}` | yes | `lib/rigor/effects/label.rb` |
| `Registry::{known, labels, roots, retired, suggest, vocabulary_version}` | yes | `lib/rigor/effects/registry.rb` |
| `Catalog::{lookup, lookup_with, class_entry, class_names, universal, object_constant, posture_labels, schema, vocabulary, digest, identity}` | yes | `lib/rigor/effects/catalog.rb` |
| `Registry#with` + root ownership | **no** — the declared lane, ADR-0043 slice 6 (probe § 6) | |
| `LabelSet`'s lattice (`TOP`, `join`, `admits?`, `excluding_subsumed_by`) | **no** — summary-lane machinery for slices 2-6; slice 1 carries only `LabelSet.new`'s normalisation, as a sorted / de-duplicated / grammar-checked `Vec<String>` | |

Vendored verbatim under `crates/rigor-effects/vendor/effects/`, at the `v0.3.4`
pin (`b10bd5df`), with a hand-authored `PROVENANCE.md`:

| file | lines | bytes | sha256 |
|---|---|---|---|
| `registry.yml` | 67 | 2,217 | `bb0eb3f0…9d7104` |
| `core.yml` | 843 | 52,785 | `85778dd3…da9c31` |

`Catalog::identity()` reproduces upstream's own cache identity string
`1:85778dd3433fcb5561a933c9b2b22fb07048af980e35f93091f545655bda9c31` — measured
against the pinned Ruby loader in the probe § 1c, and asserted as a test. The
provenance anchor and upstream's invalidation key are one number.

`PROVENANCE.md` records the pin, both source paths, that identity anchor, and
the **three carve-outs that are slice-2 CODE and not data** — the mutator sets
(`ARRAY_MUTATORS` 31 / `HASH_MUTATORS` 15 / `STRING_MUTATORS` 26), the seven
narrowing handler BODIES, and the plugin effect layer — so a future reader who
sees `mutators: array` with no selector list does not conclude the copy was
truncated.

### The shape, re-measured from the vendored bytes

80 classes, **420 rows** (216 instance + 204 singleton), 14 postures, 34
universal selectors, 77 explicit `effects: []` rows, 8 `mutates: receiver`, 7
`narrow:` rows over 6 handlers, 5 `kind: object` constants, 3 `mutators:`
references, 1 `singleton_posture:` (`Kernel`). Every count matches the probe.

## 2. The two traps, and the test evidence

### 2a. `known?` is declared ∪ ANCESTORS — the four implied roots

`Registry#known?` (`registry.rb:161`) admits a label that is an ancestor of a
declared row. Four of the ten roots — `global`, `email`, `job`, `cache` — are
declared by NO row. `core.yml`'s `global` posture emits the bare `global`, so a
port validating the catalogue against the 36 declared rows rejects the shipped
catalogue.

Two tests pin it from both ends:

- `registry::tests::known_admits_the_four_implied_roots` — each of the four is
  `known()` AND is **not** in `labels()`.
- `catalog_emits_exactly_one_label_no_row_declares` — sweeps every label the
  catalogue can emit and asserts the set difference against the 36 declared rows
  is exactly `["global"]`.

The wholesale gate the mini-spec names is
`catalog_spells_every_label_in_the_grammar_and_in_the_shared_vocabulary`: all
**420 rows** plus every class's instance and singleton posture plus all 14
`defaults:` postures — **22 distinct labels**, each `Label::valid` and each
`Registry::known`. `catalog_answers_every_universal_selector_as_nothing` covers
the third answer source (the 34 universal selectors contribute no label at all).

### 2b. The lookup precedence — row → universal → posture

Three tests, one per arm, each measured against the pinned loader in probe § 1c:

| test | evidence |
|---|---|
| `precedence_1_a_class_own_row_wins` | `Kernel#print` → `[io.output.stdout]`, `posture == false` — the row beats the universal ∅ (`print` is rowed only; `freeze` / `dup` are both) |
| `precedence_2_the_universal_list_beats_the_posture` | `IO#class`, `Socket#respond_to?`, … → `[]`, `posture == false` — the universal list beats the `world` / `net` posture, without which the posture would put a wrong label on the most-called methods in Ruby |
| `precedence_3_the_posture_answers_last` | `IO#some_uncatalogued` → `[io]`, `posture == true`; `String#some_uncatalogued` → `[]`, `posture == true` |

Plus: `an_unlisted_class_contributes_nothing_at_all` (`Foo::Bar#baz` → `None` —
not a taint), `suppressing_the_posture_asks_for_a_row_only` (`posture = false`
is how upstream's collector stops an implicit-self `Kernel#name` colouring the
world), `the_singleton_side_is_a_separate_bucket` (`Kernel.Float` → `[]` under
the `value` singleton posture while `Kernel#puts` → `[io.output.stdout]`),
`a_posture_answer_is_produced_for_a_class_with_no_row_at_all`
(`TCPSocket.new` → `[io.net]` from the posture), and
`from_posture_provenance_is_carried_not_inferred_from_emptiness` (`Thread.new`'s
explicit ∅ ROW vs `String#…`'s ∅ POSTURE — both empty, only one keeps the
project edge slice 4 propagates).

`the_merge_key_selector_survives_the_parse` pins the `!!str "<<"` spelling:
`serde_yaml` reads it as the string `<<` rather than resolving YAML's MERGE key,
and all four rows (`IO`, `File`, `SizedQueue`, `Logger`) file correctly.

## 3. The drift gate — three layers, two proven to FIRE

`effects_diff.py` grades **6 of 420 rows (1.4 %)** and cannot be the gate. Ship:

1. **Ported upstream data specs** — `crates/rigor-effects/tests/upstream_data_specs.rs`,
   a case-for-case port of `spec/rigor/effects/registry_data_spec.rb` and
   `catalog_data_spec.rb` over the vendored bytes (24 tests).
   `spec/rigor/effects/label_spec.rb` is ported alongside as `label.rs`'s unit
   tests — the ADR's named subsumption gate.
2. **`harness/vendor_effects.py`**, modelled on `vendor_rbs.py`. Proven by
   **deleting both vendored files and regenerating them from the pinned
   submodule byte-identically** before committing, then `--check` green.
   Negative control: appending two bytes to `registry.yml` produces
   `MISMATCH` + `exit 1` with both digests printed.
3. **The embedded-bytes sha256 assertion** — the only layer that works in a
   checkout whose `reference/rigor` is empty. Negative control: the same
   two-byte edit fails
   `tests::the_embedded_bytes_match_the_provenance_digests` under plain
   `cargo test`, printing both digests and the "re-run vendor_effects.py"
   instruction.

A fourth, free layer: `the_two_vendored_files_agree_on_the_vocabulary` asserts
`core.yml`'s `vocabulary:` equals `registry.yml`'s — a HALF re-vendor (one file
copied, one not) is exactly what a hand-copy produces.

`UPSTREAM.md` step 3 gains the third re-sync bullet, alongside the rbs/overlay
and plugin-sig steps, quoting step 3's standing advice: drive it off the diff,
both directions, and read a moved row as a semantic change — a re-audit moving
`IO#write` from `io` to `io.fs.write` changes every summary with no source
change on either side.

## 4. The defect this slice's shape created — permanently stale binaries

`harness/lib.rb`, `harness/run_corpus.rb`, `harness/fp_audit.py` and
`harness/effects_diff.py` each refuse to measure a binary older than the newest
file under `crates/` (the stale-binary lesson,
[note](20260807-fp-audit-port-side-blind-spots.md)). `crates/*` is a workspace
GLOB, so a member **nothing links** enters that scan — and cargo never rebuilds
`rigor` for it, so the binary's mtime can never catch up. Measured: after
committing the crate, `ruby harness/run_snapshot.rb` aborted with

```
ERROR: STALE BINARY — target/debug/rigor was built 23:48:29 but
       crates/rigor-effects/tests/upstream_data_specs.rs changed 23:54:02.
```

and `cargo build -p rigor-cli` could not clear it. Every standing gate was
blocked.

Fixed at the root in all four tools: the scan is now the **`rigor-cli`
path-dependency closure**, read out of the manifests (`crate_source_dirs`), not
the `crates/*` glob. Derived, never an exclusion list — the day a slice adds the
dependency edge, `rigor-effects` re-enters the scan on its own, so this cannot
become a blind spot. Verified: all four resolve
`[rigor-cli, rigor-index, rigor-infer, rigor-parse, rigor-rules, rigor-types]`,
and the guards now report `crates/rigor-types/src/ty.rs` as the newest linked
source.

(An unrelated environment sharpness found while doing it: `File.read` on a
manifest raises `invalid byte sequence in US-ASCII` when the default external
encoding is not UTF-8, so both Ruby copies read with an explicit encoding.)

## 5. Gate verdicts — all BARE, in the mini-spec's order

| # | gate | verdict |
|---|---|---|
| 1 | `cargo test -p rigor-effects` | **PASS** — 52 unit + 24 integration, 0 failed |
| 1 | `cargo test --workspace` | **PASS** — 1186 tests, 0 failed |
| 2 | `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy --workspace --all-targets -- -D warnings` | **PASS** — clean, fresh target dir |
| 3 | `python3 harness/vendor_effects.py --check` | **PASS** — both files byte-identical to the pin |
| 4 | `python3 harness/effects_diff.py --self-test` | **PASS** — 46 methods, 0 OVER / 0 DECLARED-MISMATCH on all four projects |
| 5 | `rigor check` vs a master-built binary on `mastodon/app` | **BYTE-IDENTICAL** — 420 lines, stdout `c1747483…47ed6` both sides, stderr empty both sides, exit 1 both sides |
| 5 | `ruby harness/run_snapshot.rb` | **PASS** — 98 fixtures, 407 matched, 2 registered, **0 unregistered** |
| 6 | `python3 harness/docs_check.py` | **PASS** — 4 budgets, links resolve |

Run beyond the mini-spec's list, because `harness/lib.rb` changed: `ruby
harness/run.rb` (live differential) **PASS**, identical numbers to the snapshot
gate; `ruby harness/run_corpus.rb` **PASS**, 667 files / **0 FP**;
`python3 harness/fp_audit.py` on `mastodon/app/models/concerns` **0 FP**.

The master baseline binary was built from `git archive master` into a scratch
tree with its own `CARGO_TARGET_DIR`, so the two binaries share no build state.

## 6. Deviations from the mini-spec, with reasons

1. **`deny_unknown_fields` on the raw shapes** — deliberately STRICTER than
   upstream's tolerant loader. This tree's whole slice-1 job is detecting drift,
   and an ignored new key is precisely the silent kind. No behaviour risk:
   nothing consumes the crate and every key at the pin parses. Tests pin the
   refusal.
2. **Upstream's mutator-set spec case is not ported as written.** It asserts
   that all 72 selectors of `ARRAY_MUTATORS` / `HASH_MUTATORS` /
   `STRING_MUTATORS` answer `mutates_receiver? == true` — Ruby CODE the port has
   nothing to compare against until slice 2. Replaced by the strongest
   assertion the vendored data supports
   (`catalog_names_the_three_mutator_sets_by_reference`: the three classes name
   the three sets, no other class names one, and the YAML re-spells no selector
   list). The consequence is recorded as a carve-out and PINNED by
   `posture_path_does_not_expand_the_mutator_set`, so slice 2 must change it
   deliberately: on the posture path `Array#push` answers
   `mutates_receiver == false` where upstream answers `true`. That is an
   UNDER-claim, the safe direction under ADR-0043 § 2, and inert.
3. **`Registry#suggest` IS ported** although the probe § 6 lists only
   `known?` / `roots` / the grammar as slice-1 needs. Upstream's registry data
   spec asserts it, and the mini-spec mandates porting that spec; dropping the
   two assertions would have weakened a drift layer for 20 lines of bounded
   Levenshtein. (Tie-break order differs — a `BTreeSet` scan rather than
   upstream's insertion order — deterministic either way, and no shipped case
   ties.)
4. **`Catalog::vocabulary()` added**, which upstream's `Catalog` does not carry
   (its `Registry` twin holds the authority). It exists to make the half-vendor
   assertion in § 3 possible.
5. **The four harness stale-binary guards changed** (§ 4). Not in the
   mini-spec's file list, but gate 5 is unreachable without it.

## 7. Not done — one carried finding

The probe § 4b flags a stale line in
`crates/rigor-index/vendor/plugins/PROVENANCE.md:6`: it claims the plugin RBS is
embedded "by `crates/rigor-index/build.rs` (the `EMBEDDED_PLUGIN_RBS` table)".
No such table exists — `build.rs` generates only `EMBEDDED_RBS`, and the plugin
payload is a direct `include_str!` in `src/plugins.rs:40`. Harmless, but it is
the file a slice-1 author reads to copy the precedent. Left untouched here to
keep this diff to the mini-spec's scope; it is a one-line doc fix.
