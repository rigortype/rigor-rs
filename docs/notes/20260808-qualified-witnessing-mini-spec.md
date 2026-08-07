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
