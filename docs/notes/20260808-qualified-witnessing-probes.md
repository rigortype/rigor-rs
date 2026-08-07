# Qualified-name witnessing — evidence for the mini-spec (2026-08-08)

Measurement only; no product change. Inputs for the "qualified-name witnessing"
slice that would close the 7 rows the class-narrowing arc left blocked
([stage-3 spec](20260807-narrowing-stage3-spec.md), "3a-3 BUILT"), following the
[ADR-0042 Slice 5 return-lookup](20260807-adr0042-s5-return-lookup-spec.md)
precedent.

Method: pinned reference `v0.3.1` (submodule `reference/rigor`), a FRESH temp
cwd per probe, `--no-cache`, plugin path pinned
(`ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib
reference/rigor/exe/rigor check --no-cache <file>`); `rs` is
`target/release/rigor check` on master (`67478c1`), release build verified fresh.
Guard classes were verified present in `crates/rigor-index/vendor/rbs` first
(`File::Stat`, `Encoding::Converter`, `Enumerator::Lazy` core;
`URI::HTTP`/`URI::Generic`, `Digest::*` from `DEFAULT_LIBRARIES`).
Index-level rows come from a throwaway `rigor-index` integration test over the
real `CoreIndex` (not committed).

## Part 1 — oracle probe matrix

`ref`/`rs` = number of diagnostics; the message column is the reference's exact
rendering.

### 1. Real vendored-RBS namespaced classes, undefined method after the guard

| probe | shape | ref | message | rs |
|---|---|:--:|---|:--:|
| p1a | `return unless v.is_a?(File::Stat)` then `v.frobnicate_zzz` | 1 | ``undefined method `frobnicate_zzz' for File::Stat`` | **0** |
| p1b | same, `Digest::SHA256` | 1 | ``… for Digest::SHA256`` | **0** |
| p1c | same, `URI::HTTP` | 1 | ``… for URI::HTTP`` | **0** |
| p1d | same, `Encoding::Converter` | 1 | ``… for Encoding::Converter`` | **0** |
| p1e | CHAIN address `h.last.is_a?(File::Stat)` then `h.last.frobnicate_zzz` | 1 | ``… for File::Stat`` | **0** |
| p1f | chain, `Enumerator::Lazy` | 1 | ``… for Enumerator::Lazy`` | **0** |
| p1g | `if` form instead of `return unless` | 1 | ``… for File::Stat`` | **0** |
| r8 | `Bundler::Source::Git` (the corpus row's class) | 1 | ``… for Bundler::Source::Git`` | **0** |
| v4 | `Bundler::Source::Rubygems` | 1 | ``… for Bundler::Source::Rubygems`` | **0** |
| v6 | `Gem::Version` | 1 | ``… for Gem::Version`` | **0** |

**Rendering rule: always the FULL qualified path**, never the leaf, and
independent of how the guard was spelled at the site (see §3).

### 2. Nonexistent classes

| probe | shape | ref | rs |
|---|---|:--:|:--:|
| p2 | `is_a?(Foo::Bar::Baz)` (exists nowhere) | 0 | 0 |
| p2b | `is_a?(Zorkmid)` (nonexistent top-level) | 0 | 0 |

### 3. Spelling of the guard constant

| probe | shape | ref | message | rs |
|---|---|:--:|---|:--:|
| p3a | `module URI; def self.f(v); return unless v.is_a?(HTTP)` — RELATIVE inside its own namespace | 1 | ``… for URI::HTTP`` | 0 |
| p3b | same, fully qualified `URI::HTTP` | 1 | ``… for URI::HTTP`` | 0 |
| p3c | `::File::Stat` (leading `::`) at top level | 1 | ``… for File::Stat`` | 0 |
| p3d | `::URI::HTTP` inside `module URI` | 1 | ``… for URI::HTTP`` | 0 |

All three spellings resolve to the same class and render identically. The
reference resolves a relative name against the enclosing lexical namespace.

### 4. Ambiguous leaf across namespaces

| probe | shape | ref | rs |
|---|---|:--:|:--:|
| p4a | in-source `A::C` + `B::C`, guard bare `C` inside `A`, call `B::C`'s method | 0 | 0 |
| p4b | in-source, guard `B::C` from inside `A`, call `A::C`'s method | 0 | 0 |
| q9 | project-`sig/` `A::C` + `B::C`, guard bare `C` inside `A`, call `bbb` (only on `B::C`) | **1** ``undefined method `bbb' for A::C`` | 0 |
| q9b | project-`sig/`, guard `B::C` from inside `A`, call `aaa` (only on `A::C`) | **1** ``… for B::C`` | 0 |
| q9c | project-`sig/`, guard bare `C` inside `A`, call `aaa` (control) | 0 | 0 |

The reference picks the LEXICALLY enclosing one for a bare leaf (`A::C` from
inside `A`), and honours an explicit qualification (`B::C`). The in-source-only
rows (p4a/p4b) are silent for the ADR-0033 provenance reason, not ambiguity —
see §5.

### 5. Project-defined namespaced classes

| probe | shape | ref | rs |
|---|---|:--:|:--:|
| q1 | in-source TOP-LEVEL `class Thing`, guard + typo (control) | 0 | 0 |
| p5 | in-source `Proj::Thing`, guard + typo | 0 | 0 |
| q2 | project-`sig/` top-level `Thing` | **1** ``… for Thing`` | 0 |
| q3 | project-`sig/` `Proj::Thing` | **1** ``… for Proj::Thing`` | 0 |
| q3b | same, method PRESENT (control) | 0 | 0 |
| q3c | same, guard spelled relative (`Thing` inside `module Proj`) | **1** ``… for Proj::Thing`` | 0 |
| q8 | project-`sig/` three-level `A::B::C` | **1** ``… for A::B::C`` | 0 |

ADR-0033 provenance leniency applies unchanged at the namespaced level:
in-source-only ⇒ silent on BOTH engines; project-`sig/` ⇒ the reference is
authoritative and fires.

### 6. Shadowing / reopening a gem namespace

| probe | shape | ref | rs |
|---|---|:--:|:--:|
| p6 | project reopens `module URI; class HTTP; def mine_zzz`, guard `URI::HTTP`, call `frobnicate_zzz` | **1** ``… for URI::HTTP`` | 0 |
| q6 | same reopen, call the project's OWN `mine_zzz` | 0 | 0 |

A project reopen MERGES with the RBS surface (it does not replace it): the
reopened method silences, the still-absent method still fires.

### 7. Must-stay-silent controls (method present)

| probe | shape | ref | rs |
|---|---|:--:|:--:|
| p7a | `File::Stat` then `v.directory?` (own method) | 0 | 0 |
| p7b | `Digest::SHA256` then `v.hexdigest` (ANCESTOR — `Digest::Instance` via `Digest::Class`) | 0 | 0 |
| p7c | `File::Stat` then `v.frozen?` (Object) | 0 | 0 |
| q4b | `Digest::Class` then `v.digest` (own) | 0 | 0 |
| q5b | `Digest::Instance` (a MODULE) then `v.hexdigest` | 0 | 0 |
| v1 | `URI::HTTP` then `v.host` (inherited from `URI::Generic`) | 0 | 0 |
| v2 | `Digest::SHA256` then `v.digest` | 0 | 0 |
| v3 | `URI::Generic` then `v.host` | 0 | 0 |
| v5 | `Gem::Version` then `v.segments` | 0 | 0 |
| q4 | `Digest::Class` then `v.superclass` — a `::Class` method, NOT on `Digest::Class` | **1** ``undefined method `superclass' for Digest::Class`` | 0 |
| q4c | `Random::Base` (leaf `Base` shared with `Digest::Base`) + typo | **1** ``… for Random::Base`` | 0 |
| q5 | `Digest::Instance` (MODULE) + typo | **1** ``… for Digest::Instance`` | 0 |

q4 pins that the reference does NOT fall back to the top-level `::Class`; q4c
pins that an ambiguous LEAF is no obstacle to the reference. A qualified MODULE
is a witnessable guard target too (q5), not only a class.

### 8. Disjoint / precise carrier (the PR #73 analogue) — a LIVE rigor-rs FP

| probe | shape | ref | rs |
|---|---|:--:|:--:|
| r1 | `v = [1, 2]; return unless v.is_a?(File::Stat); v.frobnicate_zzz` | 0 | **1** ``… for Array`` |
| r1b | same with `v = { a: 1 }` / `URI::HTTP` | 0 | **1** ``… for Hash`` |
| r1f | same as r1 in `if` form | 0 | **1** ``… for Array`` |
| r1e | same carrier, NONEXISTENT `Foo::Bar::Baz` | 0 | **1** ``… for Array`` |
| r1d | same carrier, nonexistent TOP-LEVEL `Zorkmid` | 0 | **1** ``… for Array`` |
| r1g | same carrier, in-source project `Proj::Thing` | 0 | **1** ``… for Array`` |
| s5 | `v = Array.new` instead of the literal | 0 | **1** ``… for Array`` |
| q7 | control: same carrier, KNOWN top-level `String` | 0 | 0 |
| s3 | control: same carrier, `Integer` | 0 | 0 |
| r1c / s1 / s2 | `v = "x"` / `v = 1` carriers with an unwitnessable guard | 0 | 0 |
| q7b | `instance_of?(File::Stat)` on the `[1,2]` carrier | 0 | 0 |
| q7c / s6 | the CHAIN spelling of r1 | 0 | 0 |
| s4 | control, no guard at all: `v = [1,2]; v.frobnicate_zzz` | 1 (`for [1, 2]`) | 1 (`for Array`) |

**This is a pre-existing false positive on master**, not a coverage gap, and it
is not qualified-specific: any guard class rigor-rs cannot resolve
(`class_ordering ⇒ Unknown`) over an `Array`/`Hash`-shaped LOCAL carrier leaves
the carrier standing where the reference has collapsed it to `Bot`. It is
invisible to the sweep only because the shape does not occur in the corpora.
The chain spelling is already safe (3a-3 declines the mint instead of minting a
`Bot`), and String/Integer carriers are safe only incidentally (rigor-rs does
not type a scalar-literal local inside a `def` — s4-style control: t1/t2 are
reference-fires/rs-silent). Making qualified names resolvable FIXES this row —
`guard_collapses` would prove `Array` vs `File::Stat` disjoint — so the slice
should carry it as a required outcome, with r1/r1b/r1f as fixture rows.

### 9. `instance_of?` and other guard forms

| probe | shape | ref | rs |
|---|---|:--:|:--:|
| p9a | `instance_of?(File::Stat)` + typo | **1** ``… for File::Stat`` | 0 |
| p9b | `instance_of?(File::Stat)` + `v.directory?` (control) | 0 | 0 |
| r4 | `return unless c && v.is_a?(File::Stat)` | **1** | 0 |
| r5 | `case v when File::Stat` | **1** | 0 |
| r6 | `if !v.is_a?(File::Stat) … else USE` | **1** | 0 |
| r2 | ELSE edge of a positive guard (control) | 0 | 0 |
| r3 | `is_a?(Digest::Base)` then `is_a?(Digest::SHA256)` (subclass re-guard) | **1** ``… for Digest::SHA256`` | 0 |
| r7 | `is_a?(File::Stat)` then `is_a?(URI::HTTP)` (sequential DISJOINT) | 0 | 0 |

Every composition the top-level narrowing already supports (`&&`, `case/when`,
`!`/else, sequential re-guard, `instance_of?`) behaves identically with a
qualified class — the class NAME is the only variable.

## Part 2 — rigor-rs consumer inventory

### 2a. Every `knows_toplevel_class` call site

| site | role | impact if qualified names become witnessable |
|---|---|---|
| `crates/rigor-rules/src/lib.rs:1456` (`check_narrowed_call`) | THE gate this slice targets. Step (4) of the class-narrowing witness | must route to `knows_qualified_class` (∪ project-sig) for a `::` name. See 2b for the second blocker at `:1467` |
| `crates/rigor-rules/src/lib.rs:1530` (`check_collection_call`) | same tail, but `class_name` is the literal `"Array"`/`"Hash"` | none — never namespaced. Leave alone |
| `crates/rigor-rules/src/lib.rs:1337` (`check_call`, source-registry branch) | bundled-toplevel arm of the `.new`/registry witness | ALREADY qualified-aware (`is_qualified_project_sig_class`, `knows_qualified_class && is_declaration_only_class`, `qualified_class_has_method`). This is the reusable template |
| `crates/rigor-infer/src/lib.rs:512` (`ConstantRead`, C1 singleton) | bare-name singleton minting + shadow gate | none; its namespaced twin is `:528` |
| `crates/rigor-infer/src/lib.rs:528` (`ConstantRead`, ADR-0042 Slice 2) | qualified singleton minting | ALREADY routed; the precedent for the shadow-gate ordering |
| `crates/rigor-cli/src/sig_gen/sig_env.rs:170,250,317` | generative tool (different bar, AGENTS.md) | out of scope; do not touch |
| `crates/rigor-index/src/lib.rs:177` / `rbs.rs:891` | the definition (`toplevel_classes`, empty-namespace only) | unchanged — the defect-2 rule stays; the slice adds a path, it does not invert the gate |

### 2b. The witness path's other members

| site | role | impact |
|---|---|---|
| `CoreIndex::class_id` (`rigor-index/src/lib.rs:441`) | interns a name to a `ClassId` over `CORE_CLASSES` — **a 9-element array** (`String Integer Float Symbol Array Hash NilClass TrueClass FalseClass`) | **SECOND, INDEPENDENT BLOCKER.** `check_narrowed_call:1467` does `index.class_id(class_name)?`, so the narrowing witness today fires for those 9 names ONLY. Measured: `Time`, `Range`, `Struct`, `Pathname` guards (all `knows_toplevel_class = true`, all reference-firing) are rigor-rs-SILENT (probes u1/u2/u4/u5; u3 `String` is the firing control). A qualified-only fix closes nothing without a `ClassId` source for the narrowed nominal |
| `SourceIndex::class_id` (`rigor-infer/src/source_index.rs:857`) | registry ids for arbitrary names, incl. qualified ones (Pass 2, `:570`) | the available carrier: `render_receiver` already resolves core ids then source-registry ids (`rigor-rules/src/lib.rs:3059`). Registration is keyed off constants the SOURCE reads, so a guard-site `ConstantRead` of `File::Stat` does register — verify, do not assume |
| `CoreIndex::class_has_method` (`lib.rs:194` / `rbs.rs:1036`) | SHORT-key surface | wrong surface for a qualified name (defect-2). Must become `qualified_class_has_method` on the `::` path |
| `CoreIndex::qualified_class_has_method` (`lib.rs:204` / `rbs.rs:1060`) | isolated qualified surface, ancestor walk over SHORT-key ancestors | reusable, but **not sound as-is** — see 2d |
| `SourceIndex::project_declares_method` (`source_index.rs:642`) | pure silencer, keyed by the qualified class name as written | works unchanged for `Proj::Thing`; keep it in the chain (probe q6 depends on it or on the RBS merge) |
| `render_receiver` (`rigor-rules/src/lib.rs:3053`) → `describe_named` | message spelling | must print the FULL path (`for URI::HTTP`) per §1/§3. Presentation-only per ADR-0030, but the fixture expectations key on it |
| `Typer::guard_collapses` (`rigor-infer/src/lib.rs:3798`) → `CoreIndex::class_ordering` (`rbs.rs:1146`) | the PR #73 `Bot` suppression | `class_ordering` looks up `self.classes` (SHORT keys) and only strips a leading `::`, so **every qualified name answers `Unknown`** — the direct cause of the §8 FP. Needs a qualified-aware ordering (or an ancestor walk over the qualified registry) |
| `narrowing.dead` consumption (`rigor-rules/src/lib.rs:583`) | skips the WHOLE call site for all receiver rules | where the §8 fix lands; no change needed here itself |
| `Typer::apply_guards` (`rigor-infer/src/lib.rs:3517`) / `resolved_static_constant` (`:3836`) | mint the narrowing fact | already namespace-agnostic — the fact carries whatever `ConstantRead.name` holds (`"URI::HTTP"`, `"::File::Stat"`). Two spec items: the LEADING `::` is not stripped anywhere on this path, and a RELATIVE spelling (`HTTP` inside `module URI`) is never lexically resolved. §3 shows both must map onto the same qualified key |
| `SourceIndex::constant_shadowed` | declines the guard when the project declares the name | keep as-is; probe q6 shows a project REOPEN must still allow the RBS surface to witness, so the shadow test must not be widened to reopens of qualified names |
| possible-nil / always-truthy / wrong-arity / ATM | — | none share this gate (`knows_toplevel_class` has no call site in them); they are only affected through `narrowing.dead` |

### 2c. What ADR-0042 already offers (reusable)

Measured over the real `CoreIndex`:

- `knows_qualified_class` is **true** for every core/stdlib namespaced class
  probed (`File::Stat`, `URI::HTTP`, `URI::Generic`, `Digest::SHA256`,
  `Digest::Class`, `Digest::Base`, `Digest::Instance`, `Encoding::Converter`,
  `Enumerator::Lazy`, `Process::Status`, `Random::Base`) and for vendored gem
  classes (`Gem::Version`, `Gem::Specification`, `Nokogiri::CSS::Parser`);
  **false** for `Foo::Bar::Baz` (a free decline matching §2).
- `qualified_class_has_method` witnesses the ABSENCE correctly on all of them
  (`frobnicate_zzz ⇒ false`) and is the isolated (non-merged) surface.
- `resolve_short_unambiguous` + the PR #64 `superclass_written` /
  `includes_written` / `member_ctxs` machinery is the existing answer to
  "resolve a reference as written against a lexical context, decline on
  ambiguity" — the same problem §3/§4 pose for the GUARD constant. That routing
  is directly reusable; only its entry point (a guard-site name + the use site's
  `enclosing_prefix`) is new.

### 2d. Two measured defects the spec must own

1. **`qualified_class_has_method` under-reports inherited methods ⇒ would FP.**
   The qualified entry's ancestors resolve through the SHORT-key chain
   (`rbs.rs:1075-1094`). Measured `false` ("proven absent") where the reference
   is SILENT: `Digest::SHA256#hexdigest`, `Digest::SHA256#digest` (inherited via
   `Digest::Base → Digest::Class → include Digest::Instance`; leaf `Base` is
   ambiguous between `Random::Base` and `Digest::Base`), `URI::HTTP#host` and
   `URI::Generic#host`. The short-key twin is equally wrong, so this is not
   qualified-specific — but it becomes a live FP the moment the witness fires on
   qualified classes. Probes v1/v2/v3 and p7b are the must-stay-silent controls.
2. **The qualified registry double-prefixes at lexical depth ≥ 3.**
   `ingest_class`/`ingest_module` (`rbs.rs:3322-3324`, `:3348-3350`) build
   `child_enclosing = enclosing ++ [qual]` where `qual` is already the FULL
   qualified name, so a class nested two modules deep registers under
   `Bundler::Bundler::Source::Git`. Verified directly:
   `knows_qualified_class("Bundler::Source::Git") = false` while
   `knows_qualified_class("Bundler::Bundler::Source::Git") = true`; same for
   `Bundler::Source::Rubygems`. Depth 2 is unaffected (`enclosing` is empty at
   the outer level), and a fully-qualified declaration
   (`class Nokogiri::CSS::Parser`) is unaffected because its path rides the
   node's own namespace. **This is why the 7 blocked corpus rows would still
   close ZERO after a qualified-witnessing slice**: their classes are exactly
   the doubly-prefixed ones. Fix this first, or the slice pays nothing again.

## Envelope observations (free declines for the spec)

- A class that exists NOWHERE — qualified (`Foo::Bar::Baz`) or bare
  (`Zorkmid`) — is reference-silent. Declining an unresolvable guard costs
  nothing.
- An IN-SOURCE-only namespaced class is reference-silent (ADR-0033 provenance,
  unchanged by namespacing). Only project-`sig/` classes need witnessing, and
  `is_qualified_project_sig_class` already exists for them.
- A leaf that is ambiguous ACROSS namespaces is not a decline for the reference
  — it resolves lexically (`A::C` from inside `A`) and honours an explicit
  qualification. rigor-rs may decline on residual ambiguity (coverage loss, not
  FP), matching the PR #64 rule.
- The ELSE edge of a positive guard, and a sequential DISJOINT re-guard, are
  silent on both engines already — no new work.
- A guard on a qualified MODULE (`Digest::Instance`) fires on the reference
  exactly like a class; a witness path restricted to `is_module == false` would
  miss it (and note `qualified_class_has_singleton_method`'s module-only
  restriction is the SINGLETON side and must not be disturbed — ADR-0042's
  measured 36-FP guard).
- Message rendering is the full qualified path in every spelling, so the
  narrowed nominal must round-trip its qualified name, not its leaf.
