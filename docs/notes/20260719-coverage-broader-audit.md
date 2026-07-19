# Coverage command — broader over-claim audit (2026-07-19)

Extends the node-level over-claim audit that shipped `rigor coverage`
([20260719-coverage-precision-mode.md](20260719-coverage-precision-mode.md)) to
the corpora that slice did NOT cover. Method unchanged: audit at NODE
granularity (never per-file histograms — they net over-claims out against
under-claims), positionally align both tools' DFS-pre-order node streams, assert
span equality (the denominator check), and flag every node where rigor-rs is
strictly MORE precise than the reference. The one unacceptable class is a
FACTUALLY-WRONG over-claim (rigor-rs typing a node more precisely than the value
can actually be at runtime); provably-sound extra precision is ACCEPTED
(sound-superset, AGENTS.md anti-convergence).

Oracle = pinned reference `7a69f142`, plugin path hardened per `UPSTREAM.md`
(#194). Read-only measurement — no analysis/inference/coverage source changed,
pin unchanged.

## Harness (scratch)

- rigor-rs side: the shipped `RIGOR_COVERAGE_NODE_DUMP=1 … --workers=1` stderr
  hook (`crates/rigor-cli/src/coverage.rs`), one `start end tier` line per
  counted node.
- reference side: a dumper reopening `Inference::PrecisionScanner#scan` to yield
  `(node, tier)` instead of tallying — same `ScopeIndexer.index`, same
  `Source::NodeWalker` DFS pre-order, same `NON_EXPRESSION_NODE_TYPES`
  exclusion, same `classify`; scope built exactly as the default-mode
  `CoverageScan.precision_report` does (`Scope.empty(environment:
  project_environment(configuration))`, plugin-free). Emits Prism byte offsets
  (`location.start_offset/end_offset`), which the rigor-rs hook also emits.
- over-claim := rigor-rs tier ∈ {constant, nominal, shaped, refined, bot} while
  the reference tier is not (precise-where-ref-dynamic). Nominal-where-ref-
  refined/shaped rank inversions are excluded by construction (both sides
  precise), matching the shipped metric.

## Harness validation (reproduce the known-good anchors)

Both anchors reproduced BEFORE trusting any new-corpus output:

| anchor | files | nodes | over-claims | expected | match |
|---|---|---|---|---|---|
| harness fixtures | 70 (69 scanned; `41_erb_template_skip.rb` is a deliberate parse-error fixture, excluded both sides) | 2268 | 0 | 2268 / 0 | ✓ |
| gitlab-foss/lib | 4676 | 624233 | 27 (all sound) | 624233 / 27 | ✓ |

Node count, denominator equality (0 mismatches, 0 alignment errors), and the
over-claim count all equal the figures recorded in the precision-mode note. The
dumper is trustworthy.

## New-corpus results (per corpus)

Denominator equal (rigor-rs nodes == reference nodes) on EVERY file of EVERY
corpus — 0 denominator mismatches, 0 span-alignment errors anywhere.

| corpus | files | nodes | denom match | over-claims (factually-wrong / sound) |
|---|---|---|---|---|
| binpacker/lib | 16 | 4296 | ✓ | 0 / 0 |
| ruby-date/lib | 1 | 110 | ✓ | 0 / 0 |
| ruby-io-console/lib | 8 | 1868 | ✓ | 0 / 0 |
| ruby-openssl/lib | 12 | 2928 | ✓ | 0 / 0 |
| ruby-strscan/lib | 3 | 978 | ✓ | 0 / 0 |
| rbs/lib | 108 | 52961 | ✓ | **0** / 2 |
| rbs-inline/lib | 13 | 8509 | ✓ | 0 / 0 |
| conference-app/lib | 3 | 188 | ✓ | 0 / 0 |
| mastodon/lib | 65 | 15691 | ✓ | 0 / 0 |
| mastodon/app/controllers | 333 | 29350 | ✓ | 0 / 0 |
| mastodon/app/helpers | 39 | 5719 | ✓ | 0 / 0 |
| mastodon/app/lib | 162 | 23736 | ✓ | **0** / 6 |
| mastodon/app/services | 98 | 18164 | ✓ | 0 / 0 |
| mastodon/app/workers | 116 | 6457 | ✓ | 0 / 0 |
| mastodon/app/serializers | 144 | 9478 | ✓ | 0 / 0 |
| mastodon/app/policies | 45 | 1277 | ✓ | 0 / 0 |
| mastodon/app/presenters | 16 | 1324 | ✓ | 0 / 0 |
| mastodon/app/validators | 22 | 1494 | ✓ | 0 / 0 |
| mastodon/app/mailers | 6 | 988 | ✓ | 0 / 0 |
| mastodon/app/chewy | 6 | 654 | ✓ | 0 / 0 |
| mastodon/app/inputs | 1 | 137 | ✓ | 0 / 0 |
| **total (new)** | **1217** | **186307** | ✓ | **0** / 8 |

conference-app's `app/` (all subdirs, incl. controllers/helpers) was already
audited by the precision-mode slice; only `conference-app/lib` is new here.
mastodon `app/models` was already audited; every other `app/` subdir + `lib` is
new. `app/views` and `app/javascript` carry no `.rb` (haml/erb/JS) — nothing to
scan.

## The 8 new over-claims — all PROVABLY SOUND, enumerated

Every one is precise (nominal) where the reference declines to `dynamic_top`,
and inspection of the real source line shows the value provably HAS that type at
runtime. Same principled line as the accepted gitlab-27: transient reference
gaps, not permanent Ruby semantics; encoding them would be anti-convergence.

rbs/lib (2) — `.new` on a lexically-visible in-source `Struct` subclass returns
an instance of that class (nominal); the reference declines the Struct-subclass
`.new` return:

- `rbs/lib/rbs/environment_loader.rb` 1269–1313 — `Library.new(name:, version:)`;
  `class Library < Struct.new(:name, :version, keyword_init: true)` is defined at
  line 17 of the same file.
- `rbs/lib/rbs/prototype/rb.rb` 963–1068 — `Context.new(module_function:, …)`;
  `class Context < Struct.new(…)` is defined at line 8 of the same file.

mastodon/app/lib (6):

- `entity_cache.rb` 616–680 / 709–833 / 1137–1239 —
  `shortcodes.map{…}` / `shortcodes.each do…end` / `shortcodes.filter_map{…}`,
  all inside `def emoji(shortcodes, domain)` whose line 19 rebinds
  `shortcodes = Array(shortcodes)`. `Kernel#Array` unconditionally returns an
  Array, and `Array#map`/`#each`/`#filter_map` return an Array (each returns the
  receiver, map/filter_map a new Array) — Nominal[Array] cannot be wrong at
  runtime. The reference does not carry the `Array()` rebind through to these
  block-calls.
- `request.rb` 10107–10163 and 10118–10163 (the same
  `::Socket.pack_sockaddr_in(port, address.to_s)` call and its enclosing
  `sockaddr = …` write) — `Socket.pack_sockaddr_in` returns `String`
  unconditionally per RBS (same family as the gitlab `File.join → String` set).
- `admin/account_statuses_filter.rb` 68–89 — the superclass constant
  `AccountStatusesFilter` in `class Admin::AccountStatusesFilter <
  AccountStatusesFilter`. The constant resolves at runtime to the real top-level
  class defined at `mastodon/app/lib/account_statuses_filter.rb`; its value is a
  Class object, so the nominal (singleton) tier is correct. This is NOT the
  precision-note defect-#4 shape (a short-name constant resolving to NOTHING /
  NameError) — here the constant genuinely denotes a defined, in-scope class.

## Verdict

**Coverage command is over-claim-clean on the broader corpus. 0 factually-wrong
over-claim classes found.** Across 1217 new files / 186307 new nodes, plus the
two reproduced anchors (626501 anchor nodes), denominators are byte-equal
everywhere and the only extra precision is 8 provably-sound nodes across two
files-families (in-source Struct-subclass `.new`, `Array()`-rebind Array-method
chains, `Socket.pack_sockaddr_in → String`, a resolvable superclass constant) —
all in the accepted sound-superset class, none a value the node cannot hold. No
new future-fix defect candidates.

## Repro

```sh
# reference dumper (scratch), hardened plugin path:
ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib \
  scratch/ref_dump.rb <abs-path>...
# rigor-rs node dump:
RIGOR_COVERAGE_NODE_DUMP=1 target/release/rigor coverage <abs-path> --workers=1 --format json
```
