# The `BigMath` divergence is not an ingestion gap — the oracle cannot BUILD the definition

2026-07-31. Closes the `BigMath` half of the RBS ingestion-surface asymmetry that
[the MultiWrite slice-2 note](20260725-multiwrite-substrate-s2.md) left open
(§ "Follow-up: why is rigor-rs's RBS surface not the oracle's?").

## Reproduction (pin `v0.3.1`, vendored rbs 4.1.0)

Both shapes still diverge at the current pin — the note predates the `v0.3.0 →
v0.3.1` and rbs `4.0.3 → 4.1.0` bumps, so this was re-measured, not assumed:

```ruby
require "bigdecimal/math"
BigMath.sqrt(BigDecimal("2"), 10).frobnicate   # rigor-rs 2:35 "for BigDecimal", oracle SILENT
BigMath.frobnicate(1)                          # rigor-rs 3:9 "for singleton(BigMath)", oracle SILENT
```

Oracle invocation: pinned submodule, pinned `rigor-rbs-inline` plugin path,
`--no-cache`, fresh cwd (`UPSTREAM.md` hazards 1–3).

## Root cause — and the previous note's stated cause was wrong

The slice-2 note concluded "the oracle does not model `BigMath` at all". It does.
`Environment::RbsLoader.build_env_for(libraries: DEFAULT_LIBRARIES,
signature_paths: [])` yields 1356 `class_decls`, and `::BigMath` is one of them;
`class_known?("BigMath")` is `true` and `rigor type-of` prints
`singleton(BigMath)` for the receiver. The declaration is loaded.

What fails is the **definition build**:

```
::BigMath.build_instance:  RBS::DuplicatedMethodDefinitionError:
  .../gems/bigdecimal-4.1.2/sig/big_math.rbs:28 ::BigMath#E has duplicated definitions in
  .../gems/rbs-4.1.0/stdlib/bigdecimal-math/0/big_math.rbs:24
::BigMath.build_singleton: (same)
```

The chain, mechanism by mechanism:

1. `DEFAULT_LIBRARIES`
   (`reference/rigor/lib/rigor/environment/default_libraries.rb:29`) lists BOTH
   `bigdecimal` and `bigdecimal-math`.
2. `RbsLoader.build_env_for`
   (`reference/rigor/lib/rigor/environment/rbs_loader.rb:66-89`) adds each name
   through `RBS::EnvironmentLoader#add(library:)`. That resolver prefers an
   INSTALLED GEM's own `sig/` over `rbs`'s `stdlib/<lib>/` copy — so `bigdecimal`
   resolves to `bigdecimal-4.1.2/sig`, which ships `big_math.rbs` as well as
   `big_decimal.rbs`. (`bigdecimal-math`'s own `manifest.yaml` names `bigdecimal`
   as a dependency, so the pairing is not even avoidable by dropping one name.)
3. `bigdecimal-math` itself has no gem of that name, so it resolves to
   `rbs-4.1.0/stdlib/bigdecimal-math/0` — a SECOND `module BigMath` declaring the
   same `E`/`PI`/`sqrt`/… set.
4. `RBS::DefinitionBuilder` raises on the duplicate. `RbsLoader#instance_definition`
   / `#singleton_definition` (`rbs_loader.rb:728`, `:802`) rescue and memoise
   `nil`, so `MethodDispatcher` has no surface to dispatch against and degrades
   every call to `Dynamic[Top]`.

So the oracle is silent on **every** method of `BigMath` — the real ones as much
as the typo — and `BigMath.sqrt(…)` returns Dynamic, which is why the CHAINED
first line is silent too. rigor-rs vendors only the `rbs` stdlib copy
(`crates/rigor-index/vendor/rbs/PROVENANCE.md`), has exactly one declaration,
builds cleanly, and therefore witnesses. That is rigor-rs emitting what the
oracle does not: a false positive under ADR-0002.

It is worth being precise about the direction: this is **not** rigor-rs knowing
more than the oracle in any useful sense. Both engines hold the same signatures.
The oracle holds them TWICE and is thereby blinded.

## Sibling sweep — the mechanism reaches 12 classes, 2 of them observably

`harness/unbuildable_classes.rb` builds the reference's configless env and probes
`build_instance` / `build_singleton` for every one of the 1356 declarations.
Twelve fail, in three mechanisms:

| class | instance | singleton | mechanism |
| --- | --- | --- | --- |
| `BigMath` | fails | fails | `bigdecimal` gem `sig/` × `rbs` `stdlib/bigdecimal-math` |
| `Bundler` | ok | fails | `rbs` gem `sig/shims/bundler.rbs` × the reference's `data/vendored_gem_sigs/bundler/` |
| `Bundler::{Definition,Dependency,LazySpecification,LockfileParser}` | fails | fails | same |
| `Gem::{Dependency,DependencyInstaller,Specification}` | fails | fails | `rbs` gem `sig/shims/rubygems.rbs` × `data/vendored_gem_sigs/rubygems/` |
| `Gem::Requirement` | ok | fails | same |
| `Gem::SourceList` | fails | fails | `NoTypeFoundError`: `rubygems_extras.rbs:175` references an undeclared `SourceList` |
| `Nokogiri::CSS::Parser` | fails | fails | `NoSuperclassFoundError`: `Racc::Parser` is not declared |

Every one involves a signature source rigor-rs deliberately does not vendor (the
`bigdecimal` gem's `sig/`, the `rbs` gem's `sig/`) or a dangling reference the
reference resolves differently — which is exactly why rigor-rs's copy builds.

Probed against both engines, only **two** of the twelve were observably
divergent: `BigMath.frobnicate(1)` and `Bundler.frobnicate`. The other ten are
namespaced, and rigor-rs's `knows_toplevel_class` / declaration-only witness
gates already keep it silent on them — but they were silent for an unrelated
reason, and would have become live the moment those gates widened.

## Decision: match the oracle

Per ADR-0002 `check` is a strict zero-FP subset of the reference; ADR-0011
(registered divergence) is for cases where the reference is defensibly wrong and
an upstream issue exists. Neither applies as an escape here — the reference's
silence is its own ADR-5 robustness contract behaving as designed (an unbuildable
definition must not produce diagnostics), even though the CAUSE is an accidental
self-collision. rigor-rs matches.

**Model.** A class in `UNBUILDABLE_DEFINITIONS`
(`crates/rigor-index/src/rbs.rs`) stays KNOWN — `knows_class` /
`knows_toplevel_class` are untouched, mirroring the reference's `class_known?`,
which reads `class_decls` and is unaffected by a failed build. Dropping the class
instead would trade this false positive for a `call.unresolved-toplevel` one.
What is removed is its METHOD SURFACE: the entry's tables are emptied (so no
return type, arity, overload or tuple shape resolves) and a flag makes the
existence gates answer "assume present ⇒ stay silent" rather than reading the
emptied tables as proven-absent. Chains passing THROUGH such a class are marked
incomplete for the same reason.

**The two sides are tracked independently.** The reference builds instance and
singleton definitions separately and they fail separately — `Bundler` and
`Gem::Requirement` build their instance definition fine. Conflating them would
still be FP-safe (more silence, never more noise) but would stop witnessing
instance methods the oracle does witness, so the table carries
`(name, instance_fails, singleton_fails)`.

**Why a table and not a derivation.** rigor-rs cannot compute this set from its
own tree: the colliding declaration is precisely the one it does not carry.
Mirroring the reference's real load set instead — vendoring the `bigdecimal` and
`rbs` gems' `sig/` so the collision reproduces — was rejected: it would pull
those gems' entire class surface into `knows_class`, which is the failure mode
`PROVENANCE.md` already records for the `prism` supplement (8 fresh false
positives). The set is therefore DATA, on the same footing as the vendored
signatures, regenerated from the pinned oracle by `harness/unbuildable_classes.rb`
(`--check` belongs in the pin-bump ritual alongside `vendor_rbs.py --check`). If
upstream fixes a collision, regeneration drops the entry and rigor-rs resumes
witnessing — the table converges rather than freezing a gap.

## Measurement

A binary self-diff (old vs new rigor-rs, no reference involved, so every delta is
attributable to this change alone) over the whole standing sweep set:

| corpus | files | added | removed |
| --- | --- | --- | --- |
| mastodon/app | 1236 | 0 | 0 |
| gitlab-foss/lib | 4676 | 0 | 0 |
| rigor-survey/mail | 874 | 0 | 0 |
| rigor-survey/Ruby | 192 | 0 | 0 |
| rigor-survey/dependabot-core | 1650 | 0 | 0 |
| rigor-survey/concurrent-ruby | 345 | 0 | 0 |
| rigor-survey/net-ssh | 180 | 0 | 0 |
| rigor-survey/haml/lib | 51 | 0 | 0 |

**Output-neutral on 9204 files.** That is the same finding the slice-2 note
recorded from the other direction — `BigMath.` appears in 12 swept files, all
vendored `bigdecimal` copies, and none chains a call onto a `BigMath` return —
and it means fixture 78 is the ONLY regression guard this change has. It also
means the corpus sweep can never have caught this class of divergence: a firing
shape that no real file writes is invisible to a sweep and visible only to a
synthetic probe against the oracle.

| gate | result |
| --- | --- |
| `cargo test --offline` | PASS (all suites; +3 tests, each proven non-vacuous by re-breaking) |
| `CARGO_TARGET_DIR=<fresh> cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `ruby harness/run.rb` | 78 fixtures, 237 matched, **0 unregistered FP**, 3 gaps (unmoved) |
| `ruby harness/run_snapshot.rb` | identical |
| `python3 harness/fp_audit.py --gaps --sweep` | **0 FP / 9204 files / 8 corpora**; every per-corpus gap count unchanged |
| `ruby harness/unbuildable_classes.rb --check` | OK: 12 classes, matches the pinned reference |
| `python3 harness/docs_check.py` | PASS |

## What this leaves open

- **The `Object#Nokogiri` half is NOT closed.** The slice-2 note pairs `BigMath`
  with the inverse case — the unvendored `nokogiri` extras put `Object#Nokogiri`
  on the ORACLE's surface and not rigor-rs's, so `"abc".Nokogiri` fires here and
  is silent there. That is a genuine ingestion gap (rigor-rs knows LESS), needs a
  vendoring decision rather than a surface mask, and is unaffected by this change.
- **The `UNBUILDABLE_DEFINITIONS` set is host-sensitive in principle.** It is a
  property of (reference pin × rbs version × installed gem set): the `BigMath`
  collision requires the `bigdecimal` gem to ship `sig/`, which every supported
  Ruby currently does, and the `Bundler`/`Gem::*` ones require the `rbs` gem's
  `sig/shims/`. Re-run `--check` on any pin, rbs, or Ruby bump; a `STALE` line
  means upstream fixed something and rigor-rs should resume witnessing.
- **The reference has a real bug here**, whether or not this port registers it:
  `bigdecimal-math` in `DEFAULT_LIBRARIES` is self-defeating — it is the entry
  that destroys `BigMath` rather than the entry that supplies it. Worth an
  upstream issue; not a blocker for this change, which is about matching observed
  behaviour, not endorsing it.
