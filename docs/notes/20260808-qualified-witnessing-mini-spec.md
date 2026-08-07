# Qualified-name witnessing — mini-spec (2026-08-08)

Turns the [probe evidence](20260808-qualified-witnessing-probes.md) into an
ordered, individually-gated slice plan. Goal: the class-narrowing witness
(`check_narrowed_call`) fires for a guard class that is namespaced (or
top-level but outside `CORE_CLASSES`), exactly where the reference does, and
the §8 shaped-carrier FP family is fixed. Everything cited as a probe row below
lives in the probes note; every rule here is measured, not reasoned.

## Ordered slices (each lands only with its own gates green)

### S0 — fix the qualified registry's depth-≥3 double-prefix (FIRST, blocking)

`ingest_class`/`ingest_module` (`crates/rigor-index/src/rbs.rs:3322-3324`,
`:3348-3350`) build `child_enclosing = enclosing ++ [qual]` where `qual` is
already fully qualified, so `Bundler::Source::Git` registers as
`Bundler::Bundler::Source::Git` (probes-note §2d.2, verified both ways).
Without this fix the 7 blocked corpus rows close ZERO again.

- Fix: append the LEAF (or replace, not extend, the enclosing path).
- Verify: index unit tests asserting `knows_qualified_class` for
  `Bundler::Source::Git`/`Rubygems` (true) and the doubled spelling (false);
  depth-2 (`URI::HTTP`) and self-qualified declarations
  (`Nokogiri::CSS::Parser`) unchanged.
- **This changes behaviour of every existing qualified consumer** (PR #64's
  return routing starts resolving depth-≥3 receivers). Full gates + sweep
  (0 FP bar) + a gap-census diff on S0 ALONE; any closure or new row is
  oracle-spot-checked individually before proceeding.

### S1 — make `qualified_class_has_method` ancestor-sound (blocking for S2)

The qualified ancestor walk resolves ancestors via the SHORT-key chain
(`rbs.rs:1075-1094`) and under-reports inherited methods: `Digest::SHA256`'s
`hexdigest`/`digest`, `URI::HTTP#host`, `URI::Generic#host` are all "proven
absent" while the reference is silent (§2d.1). Shipping S2 on top of this is a
measured FP.

- Resolve each ancestor reference AS WRITTEN against the declaring entry's
  lexical context via the PR #64 machinery (`superclass_written` /
  `includes_written` / `member_ctxs` + `resolve_short_unambiguous`); residual
  ambiguity ⇒ treat the method as PRESENT (decline-to-witness is the safe
  direction for an absence-witness).
- Must-stay-silent controls: v1/v2/v3/p7b. Must-still-fire: q4
  (`Digest::Class#superclass` — NO fallback to top-level `::Class`), q4c
  (ambiguous leaf `Base` is no obstacle), q5 (module `Digest::Instance`).

### S2 — route the narrowing witness through qualified resolution

`check_narrowed_call` (`crates/rigor-rules/src/lib.rs:1456,1467`) currently
requires `knows_toplevel_class` AND `CoreIndex::class_id` — a 9-name list, the
SECOND independent blocker (§2b): `Time`/`Range`/`Struct`/`Pathname` guards are
reference-firing and rigor-rs-silent today (probes u1-u5). Replace the pair
with ONE resolution path; do NOT invert or widen `knows_toplevel_class` itself
(the defect-2 rule stays for every other consumer).

- **Resolution** of the guard name (as carried by the fact — see below): strip
  a leading `::`; try the name as written against the use site's
  `enclosing_prefix` through the PR #64 resolve-as-written machinery; accept a
  hit in (core/stdlib qualified registry ∪ project-`sig/` qualified classes ∪
  the existing top-level surface). Residual ambiguity DECLINES (coverage-only —
  the reference resolves lexically per q9/q9b, so this is a strict subset).
- **The fact must round-trip the SPELLING**: `apply_guards` /
  `resolved_static_constant` today carry `ConstantRead.name` verbatim —
  `"::File::Stat"` keeps its `::`, and a RELATIVE `HTTP` inside `module URI`
  is never lexically resolved (§2b, apply_guards row). Resolution therefore
  happens at MINT time (where `enclosing_prefix(span)` is available), and the
  fact stores the RESOLVED qualified key; §3 probes pin all three spellings to
  the same rendering.
- **Absence check**: S1-fixed `qualified_class_has_method` (or the existing
  top-level path for unqualified hits) + `project_declares_method` (keyed as
  written — q6's reopen-merge must keep silencing own methods) +
  `constant_shadowed` unchanged (do NOT widen it to reopens, q6).
- **Modules are witnessable guard targets** (q5); the module-only restriction
  on `qualified_class_has_singleton_method` (ADR-0042's 36-FP guard) is the
  SINGLETON side and must not be touched.
- **ClassId / rendering**: the witness needs no `CORE_CLASSES` id — render the
  FULL resolved path (`for URI::HTTP`, §1/§3; fixture expectations key on it).
  If an interned nominal is required, use `SourceIndex::class_id` ("verify,
  do not assume" that guard-site `ConstantRead`s register — §2b) or carry the
  name through the snapshot map as today.
- **Declines (free per the envelope)**: unresolvable names (p2/p2b),
  in-source-only project classes (ADR-0033, p5/q1), residual ambiguity.
- Chain facts (3a-3) get the same routing — the probes fired identically for
  chain addresses (p1e/p1f).

### S3 — shaped-carrier collapse parity (fixes the §8 LIVE FP family)

`v = [1,2]; return unless v.is_a?(File::Stat); v.frobnicate_zzz` fires
`for Array` on master where the reference is silent (r1/r1b/r1f) — and it
fires even for UNRESOLVABLE guards (r1d/r1e) and in-source project classes
(r1g), so a `class_ordering` fix alone is NOT sufficient.

- FIRST read the reference's `narrow_shape_to_class` (`narrowing.rb`, the PR
  #73 site) and probe its actual collapse condition for `Tuple`/`HashShape`
  carriers vs an unresolvable class — the r1d/r1e evidence suggests the
  collapse does not require the class to resolve. Mirror the MEASURED
  condition in `guard_collapses`; do not guess an ordering-based rule.
- `class_ordering` additionally becomes qualified-aware (it strips only a
  leading `::` today and answers `Unknown` for every `::` name — §2b) so the
  resolvable-disjoint path (r1 with `File::Stat`) also collapses.
- Must-still-fire controls: q7/s3 (known-class guards on the same carriers),
  s4 (no guard at all), and the PR #73 fixture rows unchanged.
- r1/r1b/r1f/r1d/r1e/r1g become unit + fixture rows.

## Verification (binding, per slice and at the end)

- Unit tests reproducing the probes note's ENTIRE matrix for the touched
  paths, controls included (notably: p4a/p4b in-source ambiguity stays silent,
  q9/q9b project-sig lexical resolution, q6 reopen-merge, §7 all).
- New fixture `harness/corpus/91_qualified_witnessing.rb` (verify 91 free):
  positives p1a/p1c/r8 + a project-sig q3 row + a module q5 row + a
  Time/Range row (u-family) + the r1 FP fix as an ABSENT-line control;
  negative controls p2/p5/q6/p7b/v1. Oracle-verify every line first;
  regenerate snapshots.
- Gates per slice, all green, `docs_check.py` run BARE: build, `cargo test`,
  `run.rb` (0 unregistered extras), `run_snapshot.rb`, release build then
  `fp_audit.py --gaps --sweep` (**0 FP / 9204**), fresh-target clippy.
- Gap census pre/post per slice (baseline 1136). Expected total: the 7
  dependabot rows close (S0+S2), plus any Time/Range/Struct/Pathname and
  bare-local namespaced rows the census holds (each bonus oracle-spot-checked);
  ZERO new rows. r1-family FP fix is proven by unit/fixture rows (the shape is
  sweep-invisible).

## Explicit non-goals

- No change to `knows_toplevel_class` itself, `check_collection_call`,
  sig-gen's gates, or the possible-nil/always-truthy/arity/ATM paths.
- No leaf-fallback for a qualified name (q4: `Digest::Class` must NOT see
  `::Class` methods).
- No widening of `constant_shadowed` to reopens.
- 3a-2 stays DEFERRED; 3a-4/3b-2 stay parked (remeasure note).

## Build log (2026-08-08)

Baseline for every census diff below: **1136** rows
(`gap_census.py --sweep --dump`, re-measured on master `7d7ac1b`, matching the
figure the spec cites).

### S0 — depth-≥3 double-prefix (PR A, `claude/qualified-registry-depth-fix`)

The fix landed one level DEEPER than the spec located it. `child_enclosing =
enclosing ++ [qual]` is CORRECT: `enclosing` is a chain of lexical scopes,
innermost last, each element already a full path, and that is exactly the shape
`resolve_written_ref` / `resolve_leaf_unique` walk (`"{scope}::{ref}"`,
innermost-outward). The defect was in `qualified_name`, which JOINED the whole
chain instead of taking its innermost element. Taking `enclosing.last()` fixes
depth ≥ 3 and is a no-op at depth ≤ 2 (a one-element chain joins to itself) —
so the spec's "replace, not extend" is implemented on the qualification side,
leaving the resolver's scope chain intact. Appending only the LEAF, the spec's
other suggestion, would have produced the right key but broken the resolver
(scope `"Source"` instead of `"Bundler::Source"`).

Gates, all green:

| gate | result |
|---|---|
| `cargo test --offline` | 1061 pass / 0 fail (incl. 2 new registry tests) |
| `ruby harness/run.rb` | PASS — 0 unregistered extras (90 fixtures, 28 gaps, 1 registered extra) |
| `ruby harness/run_snapshot.rb` | PASS |
| `fp_audit.py --gaps --sweep` | **0 FP** |
| `docs_check.py` (bare) | PASS, exit 0 |
| clippy `-D warnings`, fresh `CARGO_TARGET_DIR` | clean |
| gap census (solo diff) | 1136 → **1136**; 0 closed, 0 new |

The solo census diff is EMPTY in both directions — the stop-the-line risk the
spec flagged (PR #64's return routing starting to resolve depth-≥3 receivers)
did not materialise on the sweep corpora. S0 pays nothing by itself, exactly as
predicted; it is the enabler the 7 blocked rows need from S2.

### S1 — ancestor soundness (PR B, commit 1)

Two measured causes, not one. The probes note attributed the whole under-report
to the SHORT-key ancestor walk; that is only half of it.

1. **The written-chain half, as specced.** `qualified_class_has_method` now also
   consults `qualified_ancestors` — the PR #64 `collect_qualified` /
   `resolve_written_ref` walk — as an ADDITIONAL present-source, with a
   truncated walk (`complete == false`) reading as PRESENT. That is the spec's
   "residual ambiguity ⇒ treat the method as present" rule, and it closes
   `Digest::SHA256#hexdigest`/`#digest` (both chain links are ambiguous leaves:
   `Base` ∈ {`Random::Base`, `Digest::Base`}, `Class` ∈ {`::Class`,
   `Digest::Class`}). The short-key result is left untouched, so this can only
   ever REMOVE an absence witness.

2. **NEW, not in the spec: rigor-rs never ingested RBS ATTRIBUTE members.**
   `URI::Generic#host` is `attr_reader host: String?`
   (`stdlib/uri/0/generic.rbs:245`), not a `def` — so probes v1/v3 were not an
   ancestor-walk failure at all, they were a hole in `collect_members`, which
   matched `Node::MethodDefinition` / `Include` / `Extend` / `Alias` and
   silently dropped every `Node::AttrReader` / `AttrWriter` / `AttrAccessor`.
   47 such members in the vendored stdlib, 58 in the overlay;
   `Gem::Specification` and `Bundler::Source::Git` are almost entirely
   attributes. Shipping S2 on top of this would have been a live FP on any of
   them.

   Fix: `ClassEntry::attr_methods` / `singleton_attr_methods` — reader ⇒ `x`,
   writer ⇒ `x=`, accessor ⇒ both — recorded as bare EXISTENCE and deliberately
   kept OUT of `methods` (that map feeds return typing, arity envelopes, the ATM
   overload substrate and sig-gen, none of which this slice measured for
   attributes). Consulted by `class_has_method`, `qualified_class_has_method`,
   both singleton paths and `instance_method_names`. It can only ever remove a
   diagnostic: the reference models attributes, so it never fires on one.

3. **NEW, oracle-driven: a MODULE gets `Object`'s surface.** RBS gives a module
   declaration an implicit self-type of `::Object`, so the pre-S1
   `!entry.is_module` guard on the implicit-superclass default made every Object
   method on a module guard target read as proven-absent. Measured on the pinned
   reference: `is_a?(Digest::Instance)` then `v.frozen?` is SILENT while the same
   shape with a typo FIRES (probe q5). Without this, S2 would have FP'd on every
   `frozen?`/`inspect`/`respond_to?` after a module guard.

| gate | result |
|---|---|
| `cargo test --offline` | 1065 pass / 0 fail (4 new soundness tests) |
| `ruby harness/run.rb` | PASS — 0 unregistered extras |
| `ruby harness/run_snapshot.rb` | PASS |
| `fp_audit.py --gaps --sweep` | **0 FP** |
| `docs_check.py` (bare) | PASS, exit 0 |
| clippy `-D warnings`, fresh target dir | clean |
| gap census | 1136 → **1136**; 0 closed, 0 new |

S1 is pure leniency, so it closes nothing — and, measurably, costs nothing
either: the extra "assume present" answers did not silence a single row the
reference emits.

### S3 collapse condition — PROBED, and it is not what the spec guessed

`narrow_shape_to_class` (`narrowing.rb:2403`) is one line: the shape survives
iff `subclass_of?(projected_class, class_name)`, and `subclass_of?` is
`class_ordering(lhs, rhs)` in `{:subclass, :equal}` — so `:unknown` COLLAPSES.
The collapse does NOT require the guard class to resolve, exactly as r1d/r1e
suggested. Discriminating probe (pinned reference, fresh cwd, `--no-cache`), an
`[1, 2]` / `{a: 1}` carrier under six guards:

| guard | `class_ordering(carrier, guard)` | ref | rs (S0+S1) |
|---|---|:--:|:--:|
| `Enumerable` | subclass | **1** ``… for [1, 2]`` | 1 |
| `Object` | subclass | **1** | 1 |
| `Array` | equal | **1** | 1 |
| `File::Stat` | unknown in rigor-rs | 0 | **1 — FP** |
| `Foo::Bar::Baz` | unknown | 0 | **1 — FP** |
| `Enumerable`, Hash carrier | subclass | **1** ``… for { a: 1 }`` | 1 |

So the rule to mirror is per-CARRIER-KIND, not per-ordering-value: for
`Constant`/`Tuple`/`HashShape` the shape survives ONLY on `Subclass`/`Equal`
(everything else, `Unknown` included, is `Bot`), while for `Nominal` the
reference keeps its `Disjoint`-only collapse (`narrow_nominal_to_class`
preserves the bound on `:subclass` and stays conservative on `:unknown`).

**Deviation from the spec, with its evidence.** The spec additionally asked for
a qualified-aware `class_ordering`. Under the measured condition that buys
nothing for the r1 family — `ordering(Array, File::Stat) = Unknown` already
collapses — and `class_ordering` is also read by `call.raise-non-exception`,
where widening it from `Unknown` to real answers is an unprobed behaviour change
on an unrelated rule. It is therefore NOT part of this slice. The Nominal path
with a qualified guard, the only place the ordering would matter, cannot produce
a narrowing FP anyway: `check_narrowed_call` gate (3) declines any use site whose
receiver is already a concrete carrier.

### S2 — routing the narrowing witness through qualified resolution (PR B, commit 2)

Two independent blockers removed at once, as the probes note's §2b required:
`knows_toplevel_class` (refuses every namespaced name — the defect-2 rule, left
in force for every OTHER consumer) and `CoreIndex::class_id` (interns over the
nine-element `CORE_CLASSES` array, which is why `Time`/`Range`/`Struct`/
`Pathname` guards were silent despite passing the first gate). Both are replaced
by one resolution path over three accepted surfaces — the existing top-level one,
project `sig/` (nested included), and the bundled qualified registry — plus the
ISOLATED `qualified_class_has_method` and the unchanged `project_declares_method`
silencer. No `ClassId` is needed: the render is the resolved path itself, which
for a core name is the same string `render_receiver` produced.

Resolution happens at MINT time (`Typer::resolve_constant_as_written`, called
from `resolved_static_constant`), where the use site's `enclosing_prefix` is
available: strip a leading `::`, then try the name against each enclosing lexical
scope innermost-outward, then the root. First hit wins — deterministic, the
reference's own rule, so there is no residual ambiguity to decline on. Verified
live: all four spellings render `for URI::HTTP`.

**One thing the spec did not predict: an r7 regression, fixed at the root.** With
qualified names witnessable, the sequential DISJOINT re-guard `return unless
v.is_a?(File::Stat)` / `return unless v.is_a?(URI::HTTP)` started firing
``for URI::HTTP`` where the reference is silent. Probing it showed the SAME shape
on two CORE names (`Hash` then `String`) was ALREADY firing before this slice —
the pre-existing local-side FP the next/break build note recorded as
`s1_two_returns_sequential`, and the exact defect the PR #73 chain re-seed
documents as "the LOCAL-side defect … on the same `join_cenv`-before-propagation
ordering". Fixed by the LOCAL twin of that chain re-seed: the early-return
propagation now restores the PRE-JOIN fact for the locals its carried map
touches, so R3 sees the prior fact, conflicts, and drops it. Both spellings are
silent now.

The one thing that costs: r3, a SUBCLASS re-guard (`Digest::Base` then
`Digest::SHA256`), where the reference narrows DOWN and fires. rigor-rs's R3 is
coarser — any class change drops the fact — so r3 is a DECLINE. It was silent on
master too (qualified names were not witnessable at all), so this is a
never-had-it coverage gap, not a regression, and it is in the FP-safe direction.
Recovering it needs a qualified-aware `class_ordering`; see the S3 section.

| gate | result |
|---|---|
| `cargo test --offline` | 1068 pass / 0 fail (3 new rules-layer test groups, ~40 rows) |
| `ruby harness/run.rb` | PASS — 0 unregistered extras |
| `ruby harness/run_snapshot.rb` | PASS |
| `fp_audit.py --gaps --sweep` | **0 FP**; `call.undefined-method` gaps 389 → 380 |
| `docs_check.py` (bare) | PASS, exit 0 |
| clippy `-D warnings`, fresh target dir | clean |
| gap census | 1136 → **1127**; **9 closed, 0 new** |

The 9 closures, each oracle-spot-checked:

| corpus | row | note |
|---|---|---|
| dependabot-core ×7 | `unlock!`/`revision` for `Bundler::Source::Git`, `fetchers` for `Bundler::Source::Rubygems` | THE 7 blocked rows the arc set out to close (v2/v4 helper copies + `file_parser.rb`) — depth-3 classes, so S0 + S2 together |
| gitlab-foss | `to_fs` for `Time` | bonus: top-level, outside `CORE_CLASSES` (the u-family blocker) |
| concurrent-ruby | `nan?` for `Numeric` | bonus, same cause |

Zero new rows.

### S3 — shaped-carrier collapse parity (PR B, commit 3)

Implements the measured condition from the section above: in `guard_collapses`,
a `Constant`/`Tuple`/`HashShape` carrier now survives ONLY on
`Subclass`/`Equal`, while a `Nominal` keeps its `Disjoint`-only collapse. The
whole r1 family goes silent, including the two rows a `class_ordering` fix could
never have reached (r1d/r1e, unresolvable guards).

r1g needed a second, smaller change. An in-source project class made
`resolved_static_constant` decline the WHOLE predicate (`constant_shadowed`), so
there was no fact to collapse against and the FP survived. The `is_a?` arm now
takes the shadowed case as a NON-MINTABLE fact instead — the existing carrier for
"assert but do not narrow", already used by `===` and `nil?`. rigor-rs still
never narrows to a project nominal; it just stops pretending the guard was not
written.

Fixture `harness/corpus/91_qualified_witnessing.rb` (91 was free) carries the
whole arc: 9 positives (p1a/p1c/r8/q5/u1/u2/p3c/p1e/p9a), 4 present-method
controls (p7a/p7b/v1/q4b), 3 unwitnessable-guard controls (p2/p2b/p5), the 6
collapsed shape rows (r1/r1b/r1f/r1d/r1e/r1g) as ABSENT lines, and 2
anti-over-suppression rows. Every line was measured on the pinned reference
first; live diff after S3 is byte-identical on all 11 diagnostics (line AND
column), 0 gaps, 0 extras. Snapshot regenerated.

| gate | result |
|---|---|
| `cargo test --offline` | 1068 pass / 0 fail |
| `ruby harness/run.rb` | PASS — 0 unregistered extras; 350/379 (was 339/368) |
| `ruby harness/run_snapshot.rb` | PASS |
| `fp_audit.py --gaps --sweep` | **0 FP** |
| `docs_check.py` (bare) | PASS, exit 0 |
| clippy `-D warnings`, fresh target dir | clean |
| gap census | 1127 → **1127**; 0 closed, 0 new |

The r1 family is sweep-invisible (the shape does not occur in the corpora), so
the census standing still is the expected result; the fix is proven by the unit
and fixture rows.

**One decline S3 costs, measured and accepted.** `h = []` then `h << 1` under an
UNRESOLVABLE guard: the reference widens that carrier to a NOMINAL
`Array[Dynamic[top]]` and therefore stays conservative and FIRES, while rigor-rs
keeps the more precise SHAPE carrier, which now collapses. Silence, never a false
positive — the fixture-85 carrier-fidelity family, out of scope here. The
neighbouring `Array.new` and `h = *spec` spellings widen to a nominal on BOTH
engines and stay pinned as must-fire. Zero census rows moved, so it costs nothing
measurable.

## Arc result

| slice | census | sweep | notes |
|---|---|---|---|
| baseline (master `7d7ac1b`) | 1136 | 0 FP | |
| S0 registry depth fix | 1136 | 0 FP | 0 closed, 0 new — the enabler |
| S1 ancestor + attribute soundness | 1136 | 0 FP | 0 closed, 0 new — pure leniency |
| S2 witness routing | **1127** | 0 FP | **9 closed, 0 new** |
| S3 shaped-carrier collapse | 1127 | 0 FP | 0 closed, 0 new; fixes 6 FP shapes |

Zero new gap rows at every step, sweep 0 FP at every step. The 7 blocked
dependabot rows closed, plus 2 oracle-verified bonuses, and three FP families
were removed on the way: the r1 shaped-carrier family (6 shapes), the sequential
disjoint re-guard (pre-existing, both spellings), and the latent attribute /
module-Object holes S1 closed before S2 could turn them into live ones.
