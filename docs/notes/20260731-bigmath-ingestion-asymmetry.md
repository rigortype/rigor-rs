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

## The oracle's silence is ENVIRONMENT-dependent, not a property of the pin

Step 2 above turns on "the installed `bigdecimal` gem ships `sig/`", which is a
fact about the host, not about the pin. Take the gem away and the same pinned
reference behaves like an entirely different oracle. Measured — same submodule
commit, same file, only `GEM_HOME`/`GEM_PATH` differing:

```console
$ cat bm.rb
require "bigdecimal/math"
BigMath.sqrt(BigDecimal("2"), 10).frobnicate
BigMath.frobnicate(1)

# arm A — ambient dev environment (bigdecimal-4.1.2 installed, sig/ ships big_math.rbs)
$ ruby -I reference/rigor/lib -I …/rigor-rbs-inline/lib \
    reference/rigor/exe/rigor check --no-cache bm.rb
No diagnostics

# arm B — GEM_HOME/GEM_PATH pointed at the same gem set MINUS the bigdecimal gem
$ GEM_HOME=/tmp/bm-gems-nobd GEM_PATH=/tmp/bm-gems-nobd ruby -I … check --no-cache bm.rb
bm.rb:2:35: error: undefined method `frobnicate' for BigDecimal
bm.rb:3:9:  error: undefined method `frobnicate' for singleton(BigMath)
```

(Arm B's `GEM_HOME` is a symlink farm of the ambient gem set with the
`bigdecimal-*` entries omitted, so nothing but that gem's presence changes. The
loaded class count is 1356 in BOTH arms — the class *set* is identical; only the
duplicate declaration differs.)

Arm B fires on **both** shapes, byte-identical to what rigor-rs emitted before
this change. So the pre-change rigor-rs was exactly right in arm B and exactly
wrong in arm A.

Re-deriving the whole set under arm B: **11 classes instead of 12 — `BigMath` is
the only entry that leaves.** That is the precise split, and it is worth stating
as the durable fact:

| entries | ingredients | moves with |
| --- | --- | --- |
| `BigMath` (1) | the **installed `bigdecimal` gem's** `sig/big_math.rbs` × `rbs`'s `stdlib/bigdecimal-math` | **the host's gem set** |
| the other 11 | the reference's own `data/vendored_gem_sigs/` × the `rbs` gem's `sig/shims/` + `core/rubygems/` | the pin (the `data/` tree is the reference's; the rbs version is locked by its `Gemfile.lock`) |

Three consequences, all of which the design has to own rather than hide:

1. **On a host without that gem's `sig/`, the oracle FIRES and rigor-rs (with the
   table) stays silent** — a coverage gap rather than a false positive. FP-safe,
   so the change is still correct under ADR-0002, but it is drift keyed to an
   environment rather than to the pin, which no other part of this port is.
2. **The false positive this removes exists only in environments like this
   project's dev machine** — which is the environment the gates are measured in.
   Removing it is therefore right on this project's own terms; a project pinning
   a gem-free environment would want the opposite entry.
3. **`harness/unbuildable_classes.rb --check` inherits the dependence.** Run
   outside the gate environment it will legitimately disagree, and the
   disagreement is a fact about the two environments, not a bug in either. The
   script therefore tags every colliding source `[env]` (host-installed gem) or
   `[pin]` (the reference's own tree, or the version-locked rbs gem) so a diff
   reads as "your bigdecimal gem changed" rather than "upstream changed":

   ```
   ("BigMath",  true,  true), // build_instance=DuplicatedMethodDefinitionError, …
   //     [env] bigdecimal-4.1.2/sig/big_math.rbs
   //     [pin] rbs-4.1.0/stdlib/bigdecimal-math/0/big_math.rbs
   ```

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
  — **WRONG WHEN WRITTEN; see the correction below.**
- **The environment dependence is a standing property, not a one-off.** It is
  measured above rather than hypothesised, and it is the one axis of this table
  that is not pinned. Re-run `--check` in the gate environment on any pin, rbs,
  Ruby *or gem* bump; read the `[env]` / `[pin]` tags before concluding upstream
  changed. Whether the port should eventually pin the oracle's gem environment
  outright (a `Gemfile`/`bundle exec` wrapper around every oracle invocation, so
  the set is a pure function of the pin) is a real follow-up question this note
  deliberately leaves open — it would touch every harness entry point, and the
  measurement here shows the blast radius is currently one class.
- **The reference has a real bug here**, whether or not this port registers it:
  `bigdecimal-math` in `DEFAULT_LIBRARIES` is self-defeating — it is the entry
  that destroys `BigMath` rather than the entry that supplies it. Worth an
  upstream issue; not a blocker for this change, which is about matching observed
  behaviour, not endorsing it.

## Correction (2026-08-01): the `Object#Nokogiri` bullet was wrong when written

The first bullet above is false, and was already false when this note was
committed. `800b3a1` (2026-07-31 04:52, the 24-FP survey triage — the same
commit this note's ledger line credits) vendored the reference's whole
`data/vendored_gem_sigs/` tree, including the `class Object` reopen that declares
`def Nokogiri:`. Both engines are silent on `"abc".Nokogiri` and on
`Nokogiri("<p/>")` at this pin; there is no vendoring decision left to take.

The bullet was inherited verbatim from the slice-2 note (where it was TRUE, six
days earlier) instead of re-measured — the exact discipline this note applies to
its own subject in §"Reproduction" ("this was re-measured, not assumed"), not
extended to the open item it hands on. Measurement, ablation, the sibling sweep
over the whole `Object#`-level conversion-function family, and the guard fixture
are in
[20260801-nokogiri-ingestion-asymmetry-closed.md](20260801-nokogiri-ingestion-asymmetry-closed.md).

The rest of this note stands unamended: the `BigMath` finding, the
environment-dependence measurement, and the `UNBUILDABLE_DEFINITIONS` design were
all measured here and reproduce.
