# Narrowing 3a-2 / 3a-3 window remeasure (2026-08-08)

BUILD/DEFER verdict for the two remaining halves of
[stage-3 spec](20260807-narrowing-stage3-spec.md): **3a-2** (`Logical` minting
in statement / LV-write-RHS position) and **3a-3** (single-hop chain guards,
local roots). 3b-1, 3a-1, and the `next`/`break` termination slice are already
BUILT (same spec doc); this note covers only the two undecided items, per the
spec's own lesson that an 8-line proximity window "measures PROXIMITY to a
guard, not the narrowing MECHANISM" (3a-1's shortfall analysis, same doc).

## Method

Fresh census (`cargo build --release --offline` then `gap_census.py --sweep
--dump`, release binary both sides): **1136 rows**, 8 corpora / 9204 files —
matches the expected baseline. `call.undefined-method` carries **389** of
those rows; every other rule is behind a closed decision and out of scope.

A naive 15-line-before proximity filter for `is_a?`/`kind_of?`/`instance_of?`
narrowed 389 → **37 candidates**. Each of the 37 was opened at its source
line and read by hand (not grepped near the column) to classify the actual
mechanism, per the chain-gap-prediction rule. Every row classified (a) 3a-2 or
(b) 3a-3 was reduced to a standalone repro and probed against both engines:
reference `v0.3.1` (pinned submodule, fresh temp cwd per probe, `--no-cache`,
plugin path pinned) and `target/release/rigor` (fresh release build, this
session).

## Per-row classification

| # | corpus | file:line | method | shape read | verdict |
|---|---|---|---|---|---|
| 1 | app | `mastodon/app/models/account.rb:177` | `group` | unrelated scope def; nearest `is_a?` (line 174) guards a different local in a different lambda | neither (false positive) |
| 2 | lib | `gitlab-foss/lib/api/helpers/packages/maven/api_error_formatter.rb:20` | `present?` | `message.is_a?(String) && message.present? && !hash['error']` as an **if-modifier CONDITION** (not statement/LV-write position) | neither — `.present?` is ActiveSupport, RBS-ingestion gap (named precedent, 3a-1 doc); unclosable by narrowing either way |
| 3 | lib | `gitlab-foss/lib/api/helpers/packages/nuget/warning_header.rb:17` | `present?` | same shape as #2 | neither — RBS gap |
| 4 | lib | `gitlab-foss/lib/bulk_imports/ndjson_pipeline.rb:98` | `presence` | `current_item = if sub_relation.is_a?(Array) … .presence … else … end` — `if`-expression LV-write, not a `Logical` | neither — not a Logical/chain shape; also RBS gap |
| 5 | lib | `gitlab-foss/lib/bulk_imports/object_counter.rb:52` | `symbolize_keys` | `return unless object_counters.is_a?(Hash)` then plain use — basic single-guard, already-covered mechanism | neither — RBS gap (`Hash#symbolize_keys` is ActiveSupport) |
| 6 | lib | `gitlab-foss/lib/bulk_imports/stage.rb:16` | `values` | `unless bulk_import_entity.is_a?(…); raise; end` guards `initialize`; flagged call is in the unrelated `pipelines` method | neither (false positive) |
| 7 | lib | `gitlab-foss/lib/gitlab/auth/o_auth/auth_hash.rb:44` (×2: `locality`, `country`) | chain off `location` after `location.is_a?(Hash)` | matches the shape, but named as a **standing PR #72 carrier-ALLOW-list decline** in the 3a-1 doc | **decline — counted, not probed** |
| 8 | lib | `gitlab-foss/lib/gitlab/ci/config/external/mapper/normalizer.rb:29` | `deep_symbolize_keys` | same carrier-ALLOW-list decline, named explicitly in the 3a-1 doc | **decline — counted, not probed** |
| 9 | lib | `gitlab-foss/lib/gitlab/ci/runner_releases.rb:87` | `present?` | `return if releases.empty? && response.parsed_response.present?` — chain shape present, but `.present?` on `Array` | neither — RBS gap |
| 10 | lib | `gitlab-foss/lib/gitlab/encrypted_redis_command.rb:18` | `demodulize` | `if instance.is_a?(Class); instance.name.demodulize…` — basic guard, `.demodulize` is on `.name`'s *result* (String), not the guarded local | neither — RBS gap, not a chain-guard shape (`instance.name` isn't guarded) |
| 11 | lib | `gitlab-foss/lib/gitlab/github_import/importer/note_attachments_importer.rb:156` | `starts_with?` | `file.is_a?(String) && file.starts_with?(…)` — if-condition Logical | neither — RBS gap (named precedent) |
| 12 | lib | `gitlab-foss/lib/gitlab/import_export/base/relation_factory.rb:200` | `present?` | `@original_users_map.is_a?(Hash) && @original_user.present?` — `.present?` receiver is a **different** ivar than the guarded one | neither (false positive) + RBS gap |
| 13 | lib | `gitlab-foss/lib/gitlab/import_export/group/relation_tree_restorer.rb:295` | `presence` | `if`-expression LV-write, not `Logical` | neither — not the shape; RBS gap |
| 14 | lib | `gitlab-foss/lib/gitlab/middleware/rack_attack_headers.rb:84` | `present?` | `return unless active_throttles.is_a?(Hash) && active_throttles.present?` — if-condition Logical | neither — RBS gap |
| 15 | lib | `gitlab-foss/lib/gitlab/pagination/keyset/order.rb:100` | `to_fs` | `if field_value.is_a?(Time) … field_value.to_fs(:inspect) …` — basic single-guard, no chain/Logical | neither — RBS gap (`to_fs` is ActiveSupport), not 3a-2/3a-3 shape at all |
| 16 | lib | `gitlab-foss/lib/gitlab/sidekiq_middleware/concurrency_limit/middleware.rb:14` | `safe_constantize` | `elsif worker.is_a?(String); worker.safe_constantize` inside an **ivar-write RHS** `if`-expression | **decline — named in the 3a-1/3b-1 doc as the d4-d7 carrier-fidelity decline's measured price; counted, not probed** |
| 17 | lib | `gitlab-foss/lib/omni_auth/strategies/cells_aware_openid_connect.rb:61` | `present?` | `return false unless state_param.is_a?(String) && state_param.present?` — if-condition Logical | neither — RBS gap |
| 18 | mail | `net-imap-0.6.4.1/lib/net/imap/search_result.rb:70` | `modseq` | bare `modseq` (implicit `self`) inside a `respond_to?`-guarded nested clause; the nearby `is_a?(Array)` guards `other`, unrelated | neither (false positive) |
| 19 | mail | `psych-5.4.0/lib/psych/visitors/to_ruby.rb:432` | `untaint` | `if key.is_a?(String); -(key.untaint) …` — basic single-guard, no chain/Logical | neither — `String#untaint` removed from modern RBS (Ruby-version gap, not narrowing) |
| 20 | mail | `rdoc-7.2.0/lib/rdoc/code_object/class_module.rb:427` | `record_location` | nearby `is_a?` (line 413) guards an unrelated variable in an unrelated branch | neither (false positive) |
| 21 | mail | `rdoc-7.2.0/lib/rdoc/code_object/context.rb:383` | `superclass=` | nearby `is_a?` (line 370) guards a different local (`existing`), not `klass` | neither (false positive) |
| 22 | mail | `rdoc-7.2.0/lib/rdoc/code_object/context.rb:693` | `definition` | `if method_attr.is_a? RDoc::Attr; "#{method_attr.definition} …"` — basic single-guard inside an interpolated string, same local | neither — not a Logical/chain shape (basic already-covered mechanism; gap is unrelated, likely project-method resolution) |
| 23 | mail | `rdoc-7.2.0/lib/rdoc/encoding.rb:114` | `encode!` | `if text.kind_of? RDoc::Comment; text.encode! encoding` — basic single-guard, same local | neither — not 3a-2/3a-3 shape |
| 24 | mail | `rdoc-7.2.0/lib/rdoc/generator/darkfish.rb:745` | `path` | `if ancestor.is_a?(RDoc::NormalClass); … ancestor.path …` — basic single-guard, same local | neither — not 3a-2/3a-3 shape |
| 25 | mail | `rdoc-7.2.0/lib/rdoc/markup/pre_process.rb:191` | `add_section` | guard is `RDoc::Context === code_object` (`===`, not `is_a?`/`kind_of?`/`instance_of?`) | neither — `===` is non-mintable per 3a-1's own finding; out of 3a-2/3a-3's enumerated guard family |
| 26 | mail | `rdoc-7.2.0/lib/rdoc/parser/prism_ruby.rb:152` | `location` | flagged chain (`current.last.location`) has no relevant `is_a?`; the visible guard (line 146) is on an unrelated local (`comment`) | neither (false positive) |
| 27 | mail | `rdoc-7.2.0/lib/rdoc/parser/ruby.rb:678` | `set_current_section` | `break unless container.kind_of?(RDoc::Context)` then use — basic single-guard + `break`-termination (already-built mechanism) | neither — not 3a-2/3a-3 shape |
| 28 | dependabot-core (v2) | `bundler/helpers/v2/lib/functions/lockfile_updater.rb:241` | `unlock!` | `elsif git_dependency?(dep) && defn_dep.source.is_a?(Bundler::Source::Git); defn_dep.source.unlock!` | **3a-3 — VERIFIED** |
| 29 | dependabot-core (v4) | `bundler/helpers/v4/lib/functions/lockfile_updater.rb:241` | `unlock!` | byte-identical duplicate of #28 (`diff` confirms) | **3a-3 — VERIFIED** (same mechanism, second corpus tag) |
| 30 | dependabot-core (v2) | `bundler/helpers/v2/lib/functions/version_resolver.rb:48` | `revision` | `details[:commit_sha] = dep.source.revision if dep.source.instance_of?(Bundler::Source::Git)` | **3a-3 — VERIFIED** |
| 31 | dependabot-core (v4) | `bundler/helpers/v4/lib/functions/version_resolver.rb:48` | `revision` | byte-identical duplicate of #30 | **3a-3 — VERIFIED** |
| 32 | dependabot-core (v2) | `bundler/helpers/v2/lib/functions/version_resolver.rb:136` | `fetchers` | `return unless dep.source.is_a?(::Bundler::Source::Rubygems)` then `dep.source.fetchers…` | **3a-3 — VERIFIED** (matches spec's own `h1` probe form exactly) |
| 33 | dependabot-core (v4) | `bundler/helpers/v4/lib/functions/version_resolver.rb:136` | `fetchers` | byte-identical duplicate of #32 | **3a-3 — VERIFIED** |
| 34 | dependabot-core | `bundler/lib/dependabot/bundler/file_parser.rb:314` | `revision` | `return spec.source.revision if spec.source.instance_of?(::Bundler::Source::Git)` | **3a-3 — VERIFIED** (matches spec's own `c7a` form) |
| 35 | concurrent-ruby | `lib/concurrent-ruby/concurrent/atomic_reference/numeric_cas_wrapper.rb:13` | `nan?` | `expected_nan = old_value.respond_to?(:nan?) && old_value.nan?` inside `if old_value.kind_of? Numeric` — looked like a 3a-2 LV-write-RHS `Logical` at first read | **neither — see control probe below** |
| 36 | concurrent-ruby | same file:20 | `nan?` | identical second occurrence (the `expected_nan` re-check inside the `while true` loop) | **neither — same control** |
| 37 | (duplicate scan artifact of #4) | — | — | — | — |

### Control probe that reclassified #35/#36 out of 3a-2

Row #35 looked like the textbook 3a-2 shape (`Logical` as LV-write RHS,
`respond_to?` as the unrecognized conjunct, `is_a?`-narrowed local as the
recognized one). Probed the full shape (`p5`) and it reproduced (ref=1,
rs=0) — but a reduction check is required before crediting 3a-2, because the
*outer* `if old_value.kind_of? Numeric` might already be sufficient by
itself. Stripped the `&&` entirely (`p5b`): **still ref=1, rs=0**. Stripped
down further to the bare form from the spec's own `c1a`-style positive control
(`if old_value.is_a?(Numeric); old_value.nan?; end`, no LV-write, no `&&`
at all, `p5e`): **still ref=1, rs=0**. This means the gap has nothing to do
with `Logical` minting — the most basic single-guard consumption already
misses it. Checked RBS content for both engines' vendored `core/numeric.rbs`
and `core_overlay/numeric.rbs`: byte-identical, neither declares `nan?` on
`Numeric`. The gap is real but its mechanism is orthogonal to 3a-2/3a-3
(likely a `Numeric`-nominal leniency or ancestor-conservatism rule elsewhere
in `rigor-rules`) — out of scope for this task, not counted toward either
stage.

## Standing declines counted (step 4, not probed)

| decline | rows | source |
|---|---:|---|
| PR #72 carrier-ALLOW-list (`auth_hash.rb:44` ×2, `normalizer.rb:29`) | 3 | named in 3a-1 doc's shortfall analysis |
| 3b-1's d4-d7 carrier-fidelity decline (`middleware.rb:14` `safe_constantize`) | 1 | named in 3b-1's "FP found mid-slice" section |

Total standing-decline rows in the candidate set: **4**.

## Verified counts

- **3a-2 (`Logical` minting, statement/LV-write-RHS): 0 verified rows.** No
  row in the 389-row `call.undefined-method` set exhibits the actual shape
  (`Logical` as a bare statement or the direct RHS of a `LocalVariableWrite`,
  with an `is_a?`/`kind_of?`/`instance_of?` conjunct feeding a use inside the
  *same* `Logical`). Every compound-`&&`/`||` candidate found is instead an
  **if/unless/return-modifier CONDITION** — a third position the original
  spec's probe matrix (c2c argument-position, f5 return-operand) never
  measured — and every one of those is additionally blocked by an
  ActiveSupport RBS-ingestion gap (`present?`, `presence`, `starts_with?`)
  that would suppress the diagnostic on rigor-rs even if minting were built.
- **3a-3 (single-hop chain guards, local roots): 7 verified rows**, all
  probed and confirmed reference=1 / rigor-rs=0 on a reduced repro: 3 distinct
  shapes (`elsif cond && chain.is_a?(C); chain.use` /
  `hash[:k] = chain.use if chain.instance_of?(C)` /
  `return unless chain.is_a?(C); …chain.use…`), each present in both the `v2`
  and `v4` dependabot-core bundler helpers (byte-identical vendored files),
  plus a fourth occurrence of the `return chain.use if chain.instance_of?(C)`
  shape in `dependabot/bundler/file_parser.rb`. This lands within the spec's
  own predicted bound (≤ 6, "1 pure") plus one additional occurrence from a
  file the original window scan did not enumerate.

## Recommendation

**3a-2: DEFER.** Zero verified rows and no FP-side value (nothing in the
corpus witnesses the reference's actual `Logical`-statement/LV-write-RHS
minting mechanism; every candidate resolves to a different position or a
non-narrowing RBS gap). Per the docs-economy precedent (a stage with <3
verified rows and no FP-side value defers), record this as a standing decline
rather than building. If a future slice wants to revisit compound-predicate
self-narrowing, the right target is the **if/unless-condition position**
probed nowhere in the current spec (distinct from both c2c/f5 and from
3a-2's statement/LV-write gate) — worth a fresh probe matrix, not a build
under the current 3a-2 design.

**3a-3: BUILD.** 7 verified rows, each independently confirmed against the
pinned reference on a reduced repro, cleanly matching the spec's designed
mechanism (`chain_env` keyed `(root local, method)`, mint on
`root.m.is_a?/kind_of?/instance_of?(C)`, consume on a re-read of the same
chain address, invalidate via the existing conservative superset rule). The
design in the stage-3 spec (`docs/notes/20260807-narrowing-stage3-spec.md`,
"3a-3 — single-hop chain guards (local roots only)") requires no revision
based on this remeasure — every verified row is a plain no-arg/no-block
chain off a local root, exactly the envelope it specifies.

## Gates

`python3 harness/docs_check.py` — this note is budget-exempt (dated note in
`docs/notes/`); links checked to resolve
(`20260807-narrowing-stage3-spec.md` in the same directory).
