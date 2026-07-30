# The 24 never-swept survey FPs: triage and close (2026-07-31)

The [v0.3.0 pin bump](20260731-upstream-bump-7a69f142-v030.md) recorded, as a
side finding, **24 false positives on rigor-survey corpora outside the standing
FP sweep set** — byte-identically present at BOTH the old pin (`7a69f142`) and
the new one (`5802c990`), so pre-existing rather than a bump regression. This is
that arc. All 24 are now closed; the sweep set is unchanged.

Reproduce:

```sh
python3 harness/fp_audit.py --gaps \
  ~/repo/ruby/rigor-survey/{mail,Ruby,dependabot-core}
```

## Result

| Corpus | files | FPs before | FPs after |
|---|---|---|---|
| survey `mail` | 874 | 14 | **0** |
| survey `Ruby` | 192 | 8 | **0** |
| `dependabot-core` | 1650 | 2 | **0** |
| mastodon `app` | 1236 | 0 | **0** |
| gitlab-foss `lib` | 4676 | 0 | **0** |
| survey `concurrent-ruby` | 345 | 0 | **0** |
| survey `net-ssh` | 180 | 0 | **0** |

Measured at BOTH pins (`7a69f142` and the `v0.3.0` tag `5802c990`) — 0 FP in
every cell. **Coverage did not regress anywhere**: a diagnostic-set diff of
rigor-rs's own output against the `5fcb692` baseline binary is EMPTY in both
directions on mastodon `app` (410) and gitlab-foss `lib` (1044), and the survey
corpora gained matches (mail 6652 → 6658, dependabot 138787 → 138789 — the
column fix below) while losing none.

Gates: `harness/run.rb` **76 fixtures, 232 matched, 0 FP** (was 70/232 — the 3
remaining gaps are fixture 72's parse diagnostics); `harness/snapshot.rb`
byte-identical for every pre-existing snapshot; `run_snapshot.rb` PASS;
`cargo test --workspace` green; `cargo clippy --workspace --all-targets` clean
in a fresh `CARGO_TARGET_DIR`.

## The nine root causes

The 24 sites clustered into nine distinct defects. Six of them are *scoping or
unit* bugs an all-ASCII, single-shape fixture corpus structurally cannot see.

### 1. Columns were counted in scalars, not bytes — 8 FPs

`line_col` (`crates/rigor-cli/src/main.rs`) counted Unicode scalars; the
reference reports `Prism::Location#start_column + 1`, and Prism's
`start_column` is a **byte** index into the line
(`reference/rigor/lib/rigor/source/node_locator.rb` documents the same unit for
the inverse mapping). The two agree on every ASCII line and diverge the moment a
multi-byte character precedes the token, so rigor-rs reported the *same*
diagnostic at a *different* column — which `fp_audit`, keyed on
`(path, line, column, rule)`, scores as one FP **and** one coverage gap.

Every site was an RSpec `it` description or a test string with an emoji or kana
in it (`address_spec.rb:230` `'💌@example.com'`, `common_address_spec.rb:153`
`"ミケル <test2@example.com>"`, `package_name_spec.rb:30` `"🤷"`). The
divergence had a standing `TODO(spec)` on it since the tracer bullet, waiting
for "a parity fixture to pin it".

Fixed in `line_col`, its duplicate in `mcp.rs` (now delegating — a second copy
is a second place to drift), and the inverse `type_of::position_to_offset`. The
LSP is untouched: it counts UTF-16 code units per the LSP position encoding,
which is correct there. Fixture `71_multibyte_columns.rb`.

### 2. Semantic rules ran on files Prism could not parse — 4 FPs

The reference's `analyze_file_body` returns on `parse_result.errors.any?` — it
emits the parse errors and never reaches `ScopeIndexer.index`, so every semantic
rule is off for that file. rigor-rs lowered Prism's **recovered** tree and ran
the rules over it. Recovery invents bindings: survey
`Ruby/searches/fibonacci_search.rb` opens with the syntax error `def
fibonacci_search int arr, int element` and recovers into a body referencing an
`element` nobody ever bound — reported four times.

rigor-rs now skips such a file entirely (index included, matching the
reference's dependency walker), in `check` and in both LSP paths, next to the
existing ERB-template skip. rigor-rs emits no parse diagnostics of its own, so
the file falls silent — a coverage gap against the reference's `rule: null`
errors (3 of them in fixture 72), not an FP. Fixture
`72_parse_error_no_rules.rb`.

### 3. Top-level locals leaked into `def` bodies — 4 FPs

A Ruby method body is an independent local scope. Prism already encodes that for
a bare name — `s` inside a `def` that does not bind `s` lowers to a CALL — so
the leak was invisible until a **name collision**: the rules walk types every use
site against ONE flat env keyed by name, built from the file's top-level writes,
and a parameter sharing a name with a top-level local read that local's TYPE.

`Ruby/data_structures/hash_table/anagram_checker.rb` runs a driver section
(`s = 'a'; t = 'ab'; puts is_anagram(s, t)`) and then *reopens* `def
is_anagram(s, t)` for the next approach — so the parameters typed as those two
driver strings, yielding `wrong number of arguments to 'count' on String` ×2 and
`undefined method 'each' for "a"` ×2.

`ScopedEnv` (`crates/rigor-rules/src/lib.rs`) now hands a use site inside any
method body an EMPTY env. This is a strict loss of information and so cannot add
a diagnostic; the bindings it withholds were all wrong, and method-body locals
are not typed by this walk at all today. Blocks are unaffected — a block DOES
capture the enclosing locals, and fixture 73 pins that.

### 4/5. Project reopenings of core classes were invisible — 3 FPs

Two gates the reference has and rigor-rs did not:

* **`Scope#discovered_method?`** runs BEFORE `Reflection.rbs_class_known?`, keyed
  by qualified class name, over EVERY `def` in the body — including one nested in
  a block or a conditional. rake's `class String` gains `#ext` and
  `#pathmap_explode` inside `rake_extension("…") do … end`, and both were
  reported undefined (`rake-13.4.2/lib/rake/ext/string.rb:42,146`). Ported as
  `SourceIndex::project_declares_method`, deliberately separate from
  `classes[..].methods` (direct children only, which several other analyses
  depend on): the new map is a pure SILENCER, only ever read to suppress, so
  widening it to nested defs cannot manufacture a diagnostic.
* **`ScopeIndexer#record_def_node`** keys a def under `<toplevel>` whenever its
  lexical prefix is empty, and `def_singleton?` excludes only a `self` receiver
  — so a top-level `def IO.foo` makes a later bare `foo` resolve.
  `io-console-0.8.2`'s `size.rb` calls `default_console_size` from inside `def
  IO.console_size`. That is not Ruby's runtime semantics, but parity is the
  contract; probed both ways against the oracle (`def self.x` at top level is
  NOT registered, and neither tool registers `def self.x` inside a class body).
  Lowering gained `Node::Definition::receiver_def_name` for the method name a
  non-`self` receiver-bearing def otherwise discards.

Fixture `74_core_class_reopen.rb`.

### 6. A namespaced project class lost to a same-named RBS class — 1 FP

rspec-core defines `RSpec::Core::DidYouMean`; the rbs stdlib set has a top-level
`module DidYouMean`. The `Nominal` rigor-rs mints carries only the SHORT key, so
the unrelated stdlib module looked like a legitimate witnessing surface and
`DidYouMean.new(relative_file).call` was reported undefined
(`rspec-core-3.13.6/lib/rspec/core/configuration.rb:2147`). This is the
defect-2 short-key asymmetry the `check_call` gate already guards in the other
direction, seen from the project's side.

The gate is the **same lexical predicate** the C1 constant-shadow gate uses, and
only on the bundled-RBS arm. Both restrictions were forced by measurement:

* A global "the project defines this name somewhere" test cost a real
  gitlab-foss diagnostic — `Gitlab::Database::Partitioning::Time` silenced
  `Time.parse(x).in_time_zone` over in `Gitlab::GithubImport`, where the project
  `Time` is not lexically visible and the oracle does fire.
* Applying it to the project-**sig** arm cost fixture 70's four
  `Status`/`Instance` witnesses — a project sig is authoritative for its own
  name.

Fixture `75_short_key_namespaced_class.rb`.

### 7. The reference's supplementary gem signatures were not vendored — 1 FP

`::DidYouMean.formatter` (guarded by `respond_to?(:formatter)` in
`rake-13.4.2/lib/rake/task_manager.rb:73`) is not in upstream rbs-4.0.3 — and
the reference resolves it anyway, because it loads
`data/vendored_gem_sigs/did_you_mean/did_you_mean_extras.rbs` in **every** run,
alongside `data/core_overlay/`. Not vendoring those made rigor-rs's surface
strictly weaker than the oracle's, which is an FP source by construction. This
was the open follow-up recorded by
[MultiWrite slice 2](20260725-multiwrite-substrate-s2.md).

Both directories are now vendored under
`crates/rigor-index/vendor/rbs/overlay/` and ingested LAST, mirroring the
reference's own load order so an upstream declaration always wins on conflict.

**`prism` is deliberately excluded.** Its file is a *supplement* to the prism
gem's own `sig/`, which the reference loads via `DEFAULT_LIBRARIES` and this tree
does not vendor. Loading the supplement alone declares `module Prism` **without**
`Prism.parse`, turning a class rigor-rs was silent about into a witnessed-absent
one: **8 fresh `call.undefined-method` FPs** on `Prism.parse` across
`dependabot-core` and `rdoc-7.2.0` the first time it was included. A supplement
is only safe when the set it supplements is also vendored. Recorded in
`vendor/rbs/PROVENANCE.md`; the guard is a unit test.

### 8. `flow.dead-assignment` looked inside the parameter list — 1 FP

The reference's `DeadAssignmentCollector` gathers writes from `def_node.body`
only. rigor-rs lowers parameter DEFAULTS into the arena (so the call rules reach
`def f(t = Time.current)`), which is what puts a default's write inside the def's
span for the span-scanning port — so `def in_range(start, limit = (not_set =
true))` reported `not_set` dead
(`rspec-benchmark-0.6.0/lib/rspec/benchmark/complexity_matcher.rb:50`).
`Node::Definition::param_span` now excludes that region. WRITES only: the extra
read names are kept, which only suppresses more and so stays inside the
reference's witness set.

### 9. Two flow facts the reference invalidates and rigor-rs did not — 2 FPs

* **An argument the callee mutates in place.** `rspec-core`'s
  `world.rb:179` builds `filter_announcements = []`, hands it to
  `announce_inclusion_filter`, which shovels into it — so the `[]`-pinned length
  is stale and `filter_announcements.length == 1` must not fold. This is the
  caller-side half of the reference's `MutationWidening` (the ported half only
  covered the local as RECEIVER). Probed against the oracle to pin the envelope
  exactly: `def m(x, a); a << 1; end` widens `m(5, xs)` but **not** `m(xs, 5)`; a
  mutator on a different local inside the callee widens nothing; a pure callee
  and an unresolved callee widen nothing. Recorded as
  `SourceIndex::mutated_params`, keyed by method NAME with no owner resolution —
  widening only FORGETS a fact, so over-widening costs coverage and can never add
  a diagnostic.
* **A `rescue => e` inside a BLOCK body.** `net-imap`'s `imap.rb:1470` sets
  `error = nil`, rescues into `error` inside the `send_command` block, then tests
  `if error`. `RescueClause::bound_name` is not a `LocalVariableWrite` node, so
  the per-node flow-write scan never saw it. The new entry is keyed by the
  enclosing BLOCK CALL's span, **not** the clause's: a method-level `begin …
  rescue => e; end` must keep folding, because the reference keeps folding it
  (probed both ways), and keying on the clause would widen that case too.

Fixture `76_flow_invalidation.rb` carries a negative control for each — the
shapes the reference still folds, which no positive-only fixture would catch.

## What this says about the fixture corpus

Six of the nine defects (1, 2, 3, 4/5, 8, 9) are invisible to a corpus of small,
well-formed, all-ASCII, single-approach files. They needed ~2900 files of real
third-party Ruby — vendored gems, RSpec suites, and a textbook repository whose
files carry syntax errors and redefine the same method three times — to surface.
The standing sweep set (mastodon `app`, gitlab-foss `lib`) is *application*
Ruby and had 0 FPs throughout; it would never have found any of these. The
survey corpora used here are worth adding to the standing sweep.

## Open

* rigor-rs still emits no parse diagnostics (`rule: null`). 9 such gaps on survey
  `Ruby`, 3 in fixture 72. Adding them is coverage, and would need its own
  column/message parity work.
* `SourceIndex::build_method_body_env` has no callers — superseded by
  `nilable_receiver_snapshots` (ADR-0038 slice 1). It starts from the top-level
  env and so carries the same leak defect 3 fixed; dead, but worth deleting.
* The overlay vendoring closes the `vendored_gem_sigs` half of the ingestion
  asymmetry. The other half (`BigMath` — signatures rigor-rs has that the
  reference does not) is untouched.
