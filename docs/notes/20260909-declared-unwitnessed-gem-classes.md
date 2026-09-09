# The 29 "declared but unwitnessed" gem-class gaps at `v0.3.8`

**2026-09-09.** Pin `ffb456b0` (`v0.3.8`), port HEAD `7323764`, rbs 4.2.0 /
ruby 4.0.5. Investigation only — no `crates/` change.

**Verdict: neither candidate explanation as stated.** Nothing regressed
(explanation 1), and the ADR-0033-era leniency is *not* still load-bearing for
these classes (explanation 2). What actually happened is a **stale premise**:
the leniency was measured and correct when it landed on **2026-07-25**, and the
fact it depends on — "rigor-rs does not vendor the reference's
`data/vendored_gem_sigs/`" — stopped being true **six days later**, on
2026-07-31, when that tree was vendored. Nobody re-measured. Two source comments
still assert the expired premise today.

The 29 rows are **not one mechanism**. They partition into four, and only 7 of
them are reachable by narrowing the gate this investigation was pointed at.

## 1. The measurement that decides it

The question is whether the port's method surface for these classes now matches
the reference's. It does — measured at the exact predicate the
`call.undefined-method` gate reads (`CoreIndex::qualified_class_has_method`),
not by reading the `.rbs` files.

Instruments: reference surfaces dumped through
`Rigor::Environment.for_project(root: <empty tmpdir>).rbs_loader`
(`#instance_definition(name).methods.keys`); port answers from a throwaway
binary linking `rigor-index` (scratchpad only, not committed) that pipes
`(class, kind, method)` triples through `qualified_class_has_method` /
`class_has_singleton_method`.

### Per-class instance surface, port vs reference

Vocabulary = 651 names: the union of the reference's own instance+singleton
surfaces for these eight classes, plus every `.method_name` token appearing in
the 21 corpus files the 29 rows come from. Each class is probed against the
whole 651, not just its own methods.

| class | ref methods | port witnesses a method the ref HAS (**FP**) | port has a method the ref lacks (missed, FP-safe) | control `frobnicate_zzz` |
|---|---|---|---|---|
| `Bundler` | 124 | **0** | 10 (`__id__`, `__send__`, `initialize`, …) | witnessed ✓ |
| `Bundler::Definition` | 165 | **0** | 0 | witnessed ✓ |
| `Gem::Specification` | 160 | **0** | 0 | witnessed ✓ |
| `Gem::Version` | 152 | **0** | 0 | witnessed ✓ |
| `Psych::DisallowedClass` | 146 | **0** | 0 | witnessed ✓ |
| `RBS::Unnamed::ENVClass` | 234 | **0** | 0 | witnessed ✓ |
| `StringIO` | 195 | **0** | 0 | witnessed ✓ |

The control column is load-bearing: `qualified_class_has_method` answers `true`
("assume present ⇒ stay silent") for an unknown or incomplete class, so a
0-difference row could be vacuous. It is not — every one of these classes
answers **`false`** for `frobnicate_zzz`, i.e. the port really can witness
absence there.

**Every one of these six classes' surfaces is COMPLETE relative to the
reference's.** The leniency is not protecting anything on them.

### The named FP is closed

`Gem::Version#segments` — the measured false positive that bought the
restriction (`source_index.rs`, `rigor-rules/src/lib.rs`) — is declared in
`data/vendored_gem_sigs/rubygems/rubygems_extras.rbs`, which the port has
carried at `crates/rigor-index/vendor/rbs/overlay/vendored_gem_sigs/rubygems/`
since `800b3a1` (2026-07-31).

```
qualified_class_has_method("Gem::Version", "segments")  ->  true
qualified_class_has_method("Gem::Version", "bump")      ->  true
qualified_class_has_method("Gem::Version", "frobnicate_zzz") -> false
```

End to end, both engines silent on `Gem::Version.new("1.0").segments` and on
`.bump`; both fire on `.frobnicate`. The FP cannot re-open.

### The whole namespaced surface, not just these six

Because a gate narrowing applies to **every** namespaced RBS class, the diff was
repeated over all **1202** namespaced classes the reference's environment
declares (the reference builds a definition for all 1202 — an independent
confirmation that `UNBUILDABLE_DEFINITIONS` is genuinely empty at this pin),
185 625 `(class, method)` probes plus a control per class.

- **612** classes the port cannot witness at all (control answers `true`) —
  silent, FP-safe, invisible.
- **590** classes the port *can* witness.
- Among those 590, the port's surface has exactly **26 holes** — pairs where the
  port would witness a method the reference resolves:

| method | classes | source of the hole |
|---|---|---|
| `gem` | 22 | `Bundler::Dependency`, 11 × `Nokogiri::{CSS,HTML,HTML5}::*`, 10 × `Resolv::DNS::*` |
| `breakable` `group` `text` | 1 | `PP::PPMethods` |
| `corrections` | 1 | `Gem::LoadError` |

Root-caused: the parent HAS the method and the child does not.
`qualified_class_has_method("Gem::Dependency", "gem") == true` but
`("Bundler::Dependency", "gem") == false`; likewise
`Nokogiri::XML::Document` vs `Nokogiri::HTML::Document`,
`Resolv::DNS::Resource` vs `Resolv::DNS::Resource::IN::MX`, `PP` vs
`PP::PPMethods`, `DidYouMean::Correctable` vs `Gem::LoadError`. `gem` is
declared `def self?.gem` inside a `module Kernel` reopen in
`overlay/rbs_shims/rubygems.rbs`, which `ingest_embedded` merges **last** — so
the port's qualified ancestor flattening does not propagate a late overlay
reopen down one more subclass level. That is an index-side ancestor-closure
defect, and it is the entire measured FP surface of the slice below.

## 2. Is the `rbs.rs` claim true?

`crates/rigor-index/src/rbs.rs:75-76`:

> Twelve entries → zero; rigor-rs resumes witnessing `Bundler*` / `Gem::*` /
> `BigMath` / `Nokogiri::CSS::Parser` receivers.

**Half true, and the false half was false on the day it was written**
(`240adba`, 2026-08-09). Probed on both engines:

| receiver | reference | port |
|---|---|---|
| `BigMath.frobnicate(1)` | fires | **fires** ✓ |
| `Bundler.frobnicate` | fires | **fires** ✓ |
| `Bundler.load_gemspec_uncached(x)` (real corpus file) | fires | **fires** ✓ |
| `Nokogiri::CSS::Parser.new.frobnicate` | fires | **SILENT** ✗ |
| `Gem::Requirement.new("x").frobnicate` | fires | **SILENT** ✗ |
| `Bundler::Dependency.new("a","b").frobnicate` | fires | **SILENT** ✗ |
| `Bundler::Definition.new(1,2,3,4).frobnicate` | fires | **SILENT** ✗ |

The claim holds for the TOP-LEVEL receivers `BigMath` and `Bundler`, which is
all `UNBUILDABLE_DEFINITIONS` ever blinded on the short-key map. It never held
for the NAMESPACED members the globs `Bundler*` / `Gem::*` /
`Nokogiri::CSS::Parser` read as covering: those are silenced by a **different,
older gate** — the declaration-only restriction in `check_call`, which landed
`2fe6493` on 2026-07-25, fifteen days *before* the comment. The comment is not a
record of a regression; it is an over-broad sentence that was never checked
against a namespaced probe.

**So: explanation 1 is wrong** (there is no regression; the namespaced half was
never witnessed) **and explanation 2 is wrong as stated** (the leniency is not
load-bearing — its premise expired). The comment needs the word "top-level" in
it, and the two leniency comments need their "rigor-rs does not vendor" clause
retired.

The two stale assertions, verbatim:

- `crates/rigor-infer/src/source_index.rs` (`is_declaration_only_class`):
  "the reference supplements the rbs gem with `data/vendored_gem_sigs/`
  (rubygems / cgi / nokogiri / prism / …), which rigor-rs does not vendor" —
  false since 2026-07-31 for every gem named except `prism`, which is
  deliberately excluded (`vendor/rbs/PROVENANCE.md`) and whose exclusion is the
  FP-safe direction.
- `crates/rigor-rules/src/lib.rs` (`check_call`): "for a namespaced GEM class
  rigor-rs's surface is knowingly weaker than the oracle's (the reference's
  `data/vendored_gem_sigs/`, which rigor-rs does not vendor)" — same.

## 3. What the 29 rows actually are

Re-measured today, file by file, both engines, from
`census_v038.json`. The parent's census also contained **2** rows that the port
**already fires** (`singleton(Bundler).load_gemspec_uncached`,
`dependabot-core/bundler/helpers/{v2,v4}/lib/functions/file_parser.rb:22`) —
stale rows, not gaps. The remaining 29 partition as:

| # | mechanism | rows | closable by the gate below? |
|---|---|---|---|
| A | the port implements **no singleton-receiver arity / argument-type checking at all** | **16** | no — unrelated feature |
| B | `ENV` is never minted as a `Nominal` | **3** | no — deliberate under-emit, `infer/lib.rs` |
| C | instance-side `call.undefined-method` on a namespaced RBS class | **7** | **yes** |
| D | singleton-side `call.undefined-method` on a namespaced RBS class | **3** | no — no qualified singleton predicate exists |

### A — 16 rows, nothing to do with gem classes

4 × `Gem::Specification.new` wrong-arity, 4 × `Psych::DisallowedClass.new`
wrong-arity, 8 × `Bundler::LockfileParser.new` argument-type-mismatch. The port
is silent on the same shapes for plain top-level core classes:

```ruby
Time.at()                 # ref: wrong-arity        port: SILENT
File.new()                # ref: wrong-arity        port: SILENT
Struct.new()              # ref: wrong-arity        port: SILENT
StringIO.new(1, 2, 3, 4)  # ref: wrong-arity + arg-type  port: SILENT
"abc".upcase(1, 2, 3)     # ref: wrong-arity        port: FIRES   <- instance side works
```

These 16 belong to a "singleton arity/argument-type" row, not to this one.

### B — 3 rows, an already-recorded deliberate under-emit

2 × `RBS::Unnamed::ENVClass#stubs` (net-ssh), 1 × `ENV[…]`
argument-type-mismatch (mail). `rigor annotate` gives `ENV #=> Dynamic[top]`.
The typer's object-constant arm (`crates/rigor-infer/src/lib.rs`, stage 2b)
says so in its own comment: "the constant itself is never minted as a
`Nominal`, so no new undefined-method witnessing surface appears for
`ENV.<anything>` itself (a strict under-emit vs the reference)". Two facts make
this the cheapest of the four: the port's `ENVClass` short-key entry is
`chain_complete` and witnessable (`class_has_method("ENVClass",
"frobnicate_zzz") == false`), and `object_constant_class("ENV")` already returns
`Some("ENVClass")`. The blocker is only that the arm types the *call's return*
and not the receiver.

### D — 3 rows, blocked on a missing index predicate

2 × `singleton(Gem::Specification).all=`, 1 × `singleton(Gem::Specification).map`.
The receiver IS typed correctly (`rigor annotate` on rdoc's `paths.rb` line 73
gives `singleton(Gem::Specification)`). But `class_has_singleton_method` is a
SHORT-key lookup: given the literal string `"Gem::Specification"` it finds
nothing and returns the conservative `true`. Measured over the same 651-name
vocabulary, it answers `true` for **441** names the reference's
`Gem::Specification` singleton surface does not have — i.e. it is vacuous for
every namespaced name. There is no `qualified_class_has_singleton_method`.
Closing these 3 needs an index addition first; it is a separate slice and its FP
surface has NOT been measured here.

## 4. Slice spec — C, the 7 closable rows

### The predicate change

`crates/rigor-rules/src/lib.rs`, `check_call`, the source-registry
`class_name_for_id_of` arm. Today:

```rust
if (bundled_toplevel
    || index.is_qualified_project_sig_class(name)
    || (index.knows_qualified_class(name)
        && typer.source().is_declaration_only_class(name)))
    && !index.qualified_class_has_method(name, method)
```

Drop the `&& typer.source().is_declaration_only_class(name)` conjunct from the
third disjunct, leaving `index.knows_qualified_class(name)`. Nothing else moves:
the arm already reads the ISOLATED qualified surface, so no short-key collision
is introduced, and the `unenumerable_instance_receiver` decline and the
completeness gate inside `qualified_class_has_method` both stay in front of it.
`is_declaration_only_class` then has no diagnostic caller and should be retired
with its doc comment rather than left asserting an expired fact.

### Predicted prize, counted from `census_v038.json`

**7 of the 29 rows**, all `call.undefined-method`:

| class | method | corpus | rows |
|---|---|---|---|
| `Bundler::Source::Git` | `specs` | dependabot-core | 2 |
| `Bundler::Dependency` | `source=` | dependabot-core | 2 |
| `Gem::BasicSpecification` | `full_gem_path` | dependabot-core | 2 |
| `Gem::Specification` | `doc_dir` | mail | 1 |

By corpus: dependabot-core 6, mail 1, net-ssh 0.
By family against the parent's census breakdown: Bundler 4 of 12, Gem 3 of 10,
Psych 0 of 4, ENV 0 of 3.

Each of the four `(class, method)` pairs was confirmed to answer `false` at the
predicate, and each receiver confirmed correctly typed by `rigor annotate` on
the real corpus file (`source = Bundler::Source::Git.new(…) #=>
Bundler::Source::Git`; `spec = Gem::Specification.find_by_name … #=>
Gem::Specification`).

The change is NOT limited to these 7 — it opens witnessing on all **590**
namespaced classes the port can witness. The 799-gap census only shows what the
sweep corpora happen to call.

### Must-still-fire controls

```ruby
Gem::Version.new("1.0").segments          # both SILENT  <- the bought FP
Gem::Version.new("1.0").bump              # both SILENT
Gem::Specification.new.name               # both SILENT
Bundler::Source::Git.new({}).uri          # both SILENT
Gem::BasicSpecification.new.gem_dir       # both SILENT
Gem::Version.new("1.0").frobnicate        # both FIRE
StringIO.new("x").frobnicate              # both FIRE    <- top-level unchanged
"abc".frobnicate                          # both FIRE
```

Plus the ADR-0042 defect-2 controls the arm already carries: a project
`Clusters::Instance` / `RSpec::Core::DidYouMean` must not start witnessing
against an unrelated stdlib leaf, and a bare nested-only `Inner` read must stay
silent. Those ride on `knows_toplevel_class` and `constant_shadowed`, neither of
which this change touches — but the gitlab app/models `fp_audit` run that caught
that shape the first time is the one to repeat.

### FP risk and the probe that catches it

**26 `(class, method)` pairs, listed in §1.** The one that lands inside this
slice's own row set:

```ruby
Bundler::Dependency.new("a", "b").gem("x")   # reference: SILENT
```

Two of the 7 rows are on `Bundler::Dependency`, the single census-relevant class
carrying a hole. `qualified_class_has_method("Bundler::Dependency", "gem")` is
`false` while `("Gem::Dependency", "gem")` is `true`, so the narrowed gate fires
there and the oracle does not.

Two orderings are acceptable:

1. Fix the ancestor closure first (propagate a late `overlay/` reopen of an
   ancestor down to already-flattened subclasses) and re-run the 185 625-probe
   diff to 0 holes, then narrow the gate. Preferred — the closure defect is a
   real bug independent of this slice.
2. Narrow the gate and accept the 26 pairs as a measured, enumerated exposure.
   The catching probe is a file containing all 26 calls run against both
   engines; the sweep will NOT catch them (`gem` as an instance call on a
   `Resolv::DNS::Resource::IN::*` receiver does not occur in 9204 files) — which
   is exactly the blindness `PROVENANCE.md` records for `Bundler.definition`.

The landing gate is still `fp_audit.py --gaps --sweep` (0 FP / 9204), not run
here (~80 min at this pin).

## 5. Reproduction

```sh
export PATH=/Users/megurine/.local/share/mise/installs/ruby/4.0.5/bin:$PATH
ruby -e 'require "rbs"; puts "ruby #{RUBY_VERSION} rbs #{RBS::VERSION}"'  # 4.0.5 / 4.2.0
```

Reference surface: `Rigor::Environment.for_project(root: <empty tmpdir>)`, then
`rbs_loader.instance_definition(name).methods.keys` over
`rbs_env.class_decls.keys` filtered to names containing `::`.
Port surface: a scratch crate with `rigor-index = { path = … }` calling
`CoreIndex::new()` and `qualified_class_has_method`. Both scripts lived in the
session scratchpad; neither is committed, and no `crates/` file was touched.

Oracle invocation, from a fresh temp cwd, INVALID-on-empty:

```sh
ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib \
     reference/rigor/exe/rigor check FILE --format json --no-cache
```
