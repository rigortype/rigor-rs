# sig-gen reads `Data.define` / `Struct.new` classes (upstream #227) — 2026-07-31

Port of upstream `da9b045e` ("Read Data.define / Struct.new classes in
sig-gen"), which landed on `master` past the `v0.3.1` pin. Before it, both
engines' output for a value class was **wrong, not merely thin**.

Measured outcome: **byte-identical to upstream `master` on every probe; 0
movement on every diagnostic gate.**

## What was wrong

rigor-rs recognised `Data`/`Struct` receivers by name and ignored the argument
list, so it knew only that a constant existed. Three consequences:

1. **No members, no constructors, no ancestry.** Declaring the class at all
   narrows its dispatch from `Dynamic` to a nominal — and from that point
   anything the runtime synthesises but RBS does not declare reads as *missing*.
   `::Data.new` is declared `() -> bot` and `::Struct`'s is the three-argument
   factory, so a subclass inheriting either turns every construction into an
   arity error; `.[]` is absent from `::Data`'s RBS entirely. An empty shell is
   therefore WORSE than no declaration.
2. **`class Point < Data.define(:x, :y)` was invisible.** Only the
   constant-assigned spelling was recognised at all.
3. **A `do ... end` block's methods were lost.** rigor-rs never descended into
   the block (upstream mis-attributed them to the enclosing namespace instead —
   the same defect, a different symptom). A method-bearing leaf then defaults to
   the `class` keyword, so upstream printed a MODULE as a class: an
   `RBS::DuplicatedDeclarationError` waiting for the real declaration to load
   beside it. rigor-rs's `--print` hard-coded `class` for every group, so it had
   that half of the bug outright.

## What is emitted now

Per layout-carrying class: one reader per member, plus writers for a Struct
(members are mutable), plus a `.new` / `.[]` pair sharing one overload list, plus
the `::Data` / `::Struct[untyped]` ancestry (`::Struct` is generic — RBS rejects
a bare `< ::Struct` with `InvalidTypeApplicationError`).

Constructor forms follow what the class actually accepts: **required** positions
for Data, **optional** for Struct (an omitted member fills with `nil`), and
keyword-only under a literal `keyword_init: true`. Everything else emits BOTH
forms, because the flag reads `false` for an *absent* `keyword_init:` as well as
a literal `false` — and since Ruby 3.2 the absent case accepts both. Emitting
both is the false-positive-free reading of an ambiguity the layout cannot
resolve.

Suppression of an already-declared member is gated on the declaration sitting on
**this** class (`SigEnv::declares_directly`), never on the lookup merely
succeeding: `::Data.new` and `::Struct`'s factory answer the `.new` lookup for
every value class, and deferring to them is exactly what leaves the inherited
arity false positive in place.

A degenerate form declares **nothing at all** — a member-less `Data.define`, the
`Struct.new("Legacy", :a)` named-factory spelling, a splatted member list. No
layout, no class, matching upstream.

## The one deliberate divergence: where the layout comes from

Upstream reads the ADR-48 member layouts `ScopeIndexer` already builds, so
sig-gen's view of a value class cannot drift from the analyser's — and that
drift is precisely what let the two disagree in #227.

**rigor-rs has no ported equivalent.** Neither `SourceIndex` nor `CoreIndex`
carries a member layout (`Type::DataInstance` exists but nothing populates it
from source), and the lowered AST drops a class's superclass EXPRESSION
entirely — only its written name survives, so `class Point < Data.define(:x, :y)`
is unreadable there. So `crates/rigor-cli/src/sig_gen/meta_class.rs` keeps
sig-gen's own recogniser and walks Prism directly, with the rules ported
one-for-one from `ScopeIndexer#build_data_member_layouts` /
`#build_struct_member_layouts`: both spellings, a `::Data` receiver, literal
Symbol members only, the `keyword_init:` flag, the `Struct.new` trailing-hash
strip, and the lexical qualification of the class name. `meta_class::collect` is
the single call site to re-point when a layout table lands in `rigor-infer`.

**This is an index-crate-shaped gap deliberately not taken** (another agent owns
`crates/rigor-index/`, and the analyser-side table belongs in `rigor-infer`
anyway). The cost of the divergence is that a future change to the checker's
recogniser would not automatically reach sig-gen.

Second, smaller divergence: upstream attaches the whole per-file
`namespace_kinds` / `class_superclasses` maps to every candidate and renders the
header off `methods.first`. rigor-rs stamps the RESOLVED header string
(`Candidate::decl_header`) once the file's `NamespaceInfo` is complete. Same
output, one field instead of two maps.

## Byte-comparison results

The pinned reference is `v0.3.1`, which does **not** have #227, so the
byte-comparison target for the new behaviour is upstream `master`
(`origin/master` = `ece06a0d`; the only sig-gen commits past `v0.3.1` are
`da9b045e` itself and `fdd8b621`, an invalid-UTF-8 refusal that does not touch
output). The pin was NOT moved; the comparison ran from a throwaway
`git worktree` of the submodule at `origin/master`, from a fresh temp cwd, with
the checkout's own plugin path pinned (UPSTREAM.md hazards 1–3).

| probe | engine compared against | result |
| --- | --- | --- |
| 6-form fixture (`Data.define`, `Struct.new`, `keyword_init: true`, `::Data.define`, `class X < Data.define`, `do ... end` block), `--print` | upstream `master` | **byte-identical** |
| edge fixture (member-less `Data.define`, `Struct.new("Legacy", …)`, splat, `keyword_init: false`, `private` in a block, nested + top-level layouts), `--print` | upstream `master` | **byte-identical** |
| same 6-form fixture, `--write` | upstream `master` | **byte-identical**; `rbs -I sig validate` clean |
| construction + member-write call sites against the generated `sig/` | upstream `master` `check` | both engines: no diagnostics |
| 65-file real corpus (`lib/rigor/**.rb` at `origin/master`), `--print` | upstream `master` | identical file count **20 → 30**; **0 regressions** (no file that matched before stopped matching) |

The corpus's remaining 35 differing files are the pre-existing sound-superset /
under-emit divergences the module docs already enumerate (`def self?.name`
module_function spelling, def-local bindings, generic-arity elaboration) — none
of them moved.

## Diagnostic gates: unmoved, as required

`sig-gen` is generative output, not the diagnostic set, so the parity gates must
not move. They did not:

- `cargo test --offline`: 954 passed, 0 failed.
- clippy, fresh `CARGO_TARGET_DIR`, `--workspace --all-targets -D warnings`: clean.
- `harness/run.rb` + `harness/run_snapshot.rb`: 235 matched / 3 gaps / **0
  unregistered FP**, both, identical to the pre-change run.
- `fp_audit.py --gaps --sweep`: **0 FP**, and the whole gap table
  (446 undefined-method / 435 possible-nil / 141 always-truthy / 101 ATM / 21
  wrong-arity / 13 / 11 / 9 / 9 / 2 / 2 / 2 / 1) is byte-for-byte what the
  pre-change binary produces. Verified by running the sweep with BOTH binaries.

## Left out on purpose

- **`--params=observed` member typing.** Upstream types a member from
  `Point.new(...)` call sites (keyword by name, positional by index, and only
  from call sites whose arity covers the whole member list). That path is
  substrate-blocked in this port
  ([note](20260711-siggen-params-observed-substrate-blocked.md)), so members
  render `untyped` — which is what upstream also produces without the flag, so
  the byte-comparison above is unaffected.
- **`attr_*` inside a value-class block.** Upstream's `walk_attr_calls` descends
  into the block too; rigor-rs generates no `attr_*` candidates at all yet
  (pre-existing deferral), so there is nothing to descend for.
- **`is_class_defining_call` was left alone.** It feeds return-type
  QUALIFICATION (`Rigor::Triage::Selector`), not declaration, and narrowing it to
  layout-carrying constants would change qualification for a member-less
  `Data.define` — orthogonal to #227 and unmeasured.
