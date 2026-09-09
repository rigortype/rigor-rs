# Gap adjudication at `v0.3.8` — all 799 rows partitioned by mechanism (2026-09-09)

Full adjudication of the standing sweep's coverage gaps at pin `v0.3.8`
(`ffb456b0`, rbs 4.2.0; 0 FP / 9204 files / **799 gaps**). The last pass was at
1168 rows and three pins ago
([141-row adjudication](20260807-gap-adjudication-141.md)); the census's
mechanism buckets have moved enough that the per-rule histogram is now actively
misleading — see "What the rule histogram hides".

Method: every row assigned to exactly one MECHANISM bucket by reading the site,
not the message string; each bucket's mechanism confirmed by running BOTH
engines on the real file and on a minimal reduction, from a fresh temp cwd with
`--no-cache` and the pinned Ruby 4.0.5 / rbs 4.2.0. Receiver types for the
possible-nil bucket come from the reference's own `type-of` at the receiver
node's position (178/178 positions resolved, 177 typed), not from proximity.

**Headline: 395 of 799 rows (49%) sit behind decisions already taken or are
adjudicated here as reference false positives. Exactly one mechanism is
actionable above 13 rows, and its entire measured prize is 71 rows produced by
ONE narrow rbs signature in ONE vendored gem.**

## The partition (mutually exclusive, all 799)

| # | mechanism | rows | files | corpora | verdict |
|---|---|---:|---:|---|---|
| 01 | Prism parse-error diagnostics not reported | 9 | 4 | Ruby | **ACTIONABLE** (9) |
| 02 | `rbs.coverage.definition-build-failed` on `.rigor.yml` | 1 | 1 | mail | **CLOSED** (host/env report, not inference) |
| 03 | stale transitive **rdoc** RBS (`RDoc::*` receivers) | 93 | 22 | mail | **CLOSED** (extends the 141-note's cluster A) |
| 04 | `Class.new(X) do … end` body analysed in TOP-LEVEL scope | 22 | 1 | dependabot-core | **CLOSED** (reference FP) |
| 05 | nested `def` inside a `def` never indexed | 9 | 3 | Ruby | **CLOSED** (reference FP, new) |
| 06 | receiver typed `nil` | 38 | 20 | all 6 | **CLOSED** ([141-note](20260807-gap-adjudication-141.md) cluster B) |
| 07 | possible-nil, **`Dynamic` arm** | 85 | — | all | **CLOSED** ([Tier B/C](20260717-tier-bc-track-closed.md)) |
| 07b | possible-nil, **concrete non-nil arm** | 93 | 37 | 5 | **BLOCKED** on #10's substrate, then a 36-source long tail |
| 08 | always-truthy / always-falsey | 129 | 71 | all | **CLOSED** ([flow frontier](20260706-flow-frontier-exhausted.md)) |
| 09 | global-variable typing (`$conf = OpenStruct.new`) | 18 | 1 | concurrent-ruby | **CLOSED** (reference FP; +9 more rows inside #08) |
| 10 | **nested-scope receiver typing** (RBS block params) | 86 | 11 | 5 | **ACTIONABLE** — 71 verified |
| 11 | `def.*` rules (return/ivar/visibility) | 25 | 18 | mail, lib | long tail, no mechanism |
| 12 | `flow.dead-assignment` | 2 | 1 | mail | tail |
| 13 | receiver-typing tail | 189 | 125 | all | **no mechanism ≥ 9 rows** |
|  | **total** | **799** | | | |

Corpora: `app` = mastodon/app, `lib` = gitlab-foss/lib, the rest under
`rigor-survey/` (mail, concurrent-ruby, dependabot-core, net-ssh, Ruby).

### What the rule histogram hides

`call.undefined-method` (296) is seven unrelated mechanisms; `call.possible-nil`
(178) splits 85/93 across the ONE line that decides whether the Tier B/C closure
applies; and all 31 `call.unresolved-toplevel` rows carry the ADR-17 `pre_eval:`
hint in their message text **without belonging to the `pre_eval:` cluster at
all** — that hint is boilerplate on every `unresolved-toplevel` message. Sorting
by "carries the `pre_eval:` message" yields 82 rows; the actual cross-file
monkey-patch cluster is 51. Message-string bucketing is wrong here by 31 rows.

## CLOSED buckets

### 03 — the rdoc bucket is 93 rows, not 49, and it is a KNOWLEDGE gap (CLOSED)

The 141-note closed 49 `pre_eval:`-hinted rows. At this pin **93 rows name an
`RDoc::*` receiver**: 51 carry the hint, 42 do not — and the 42 are the same
mechanism seen one ancestor up. `record_location` (12 rows) is defined at
`rdoc-7.2.0/lib/rdoc/code_object.rb:319` on the ancestor `RDoc::CodeObject`, so
the reference cannot name a def site on `RDoc::Attr` itself and emits the plain
message; `store=`, `full_name`, `token_stream`, `parser`, `path`, `all_files`
are likewise all present in rdoc 7.2.0 source. Same root cause, same verdict.

**The decisive probe:** `RDoc::Attr.new(…).frobnicate_zzz`
— reference fires, **port silent**. The port does not know the class *at all*.
A static diff of the port's vendored RBS tree against every receiver class named
in the gap set is exact:

> **93 of 799 rows name a class the port's vendored RBS does not declare, and
> all 93 of them are `RDoc::*`.** No other namespace in the gap set is a
> knowledge gap.

Provenance: both engines' `DEFAULT_LIBRARIES` list `prism` and `rbs`, but the
port's vendored `stdlib/` has no `prism/` or `rbs/` directory (`rbs.rs:40`,
"an absent lib … is skipped"), and the installed rbs gem's
`sig/manifest.yaml` declares `dependencies: - name: rdoc` — which is how rdoc
enters the reference's configless environment and never the port's. Vendoring it
is the ingestion asymmetry the 141-note item 3 already priced as **anti-parity**
(49 FPs plus mis-typed receivers), and `PROVENANCE.md` records 8 fresh FPs from
the one time the prism supplement was vendored. **No-go. Retires 93 rows.**

### 04 — `Class.new(X) do … end` body analysed at top level (22 rows, reference FP)

All 22 sit in dependabot-core's
`updater/spec/dependabot/updater/operations/refresh_group_update_pull_request_multi_dir_spec.rb`,
inside `Dependabot::FileParsers.register("terraform", Class.new(Base) do … end)`.
Reduced (both engines run on it):

```ruby
RSpec.describe "x" do
  before do
    Reg.register("terraform", Class.new(Unknown::Base) do
      define_method(:parse) { source }
    end)
  end
end
```

Reference: `unresolved toplevel call to 'define_method'` **and** `… to 'source'`.
The port reports only the shared `before` row. `ruby` runs it clean —
`define_method` is a `Module` method on the anonymous class and `source` an
instance method. This is mechanism A of the
[Object-bucket adjudication](20260808-object-bucket-adjudication.md), still
live at `v0.3.8` in its `unresolved-toplevel` flavour. **Reference FP. Retires 22.**

### 05 — a nested `def` is never indexed (9 rows, reference FP, NEW)

`rigor-survey/Ruby/data_structures/binary_trees/{in,pre,post}order_traversal.rb`
each define `def traverse` **inside** `def inorder_traversal`, then call it.
Reduced:

```ruby
def outer(root)
  def traverse(node) = node
  traverse(root)          # reference: unresolved toplevel call to `traverse`
end
```

At runtime the inner `def` executes before the call and defines the method on
`Object`. Reference fires, port silent, `ruby` clean. **Reference FP. Retires 9.**
Paste-ready upstream repro.

### 06 — receiver typed `nil` (38, unchanged)

Same families as the 141-note's cluster B, re-confirmed by sampling. The 11 new
`Ruby`-corpus rows are the note's own named archetype: `topological_sort_test.rb`
does `sorted_items.index(:x) < sorted_items.index(:y)` where the cross-file
`@sorted_nodes = []` folds `index` to `nil`, giving `nil < nil` nine times.
**CLOSED.**

### 07 — possible-nil splits 85 / 93 on the receiver's TYPE

The Tier B/C closure rests on one fact: the reference's
`method_present_anywhere?` treats a `Dynamic` arm as satisfying the non-nil-arm
check for EVERY method name, and rigor-rs's requirement of a *nameable concrete
arm* is its FP-safety mechanism. So the closure applies exactly to rows whose
receiver has a `Dynamic` arm. That had never been measured per row. It is now —
the oracle's own `rigor type-of` at each receiver node:

| receiver type at the site | rows | verdict |
|---|---:|---|
| `Dynamic[top]` / `Dynamic[top]?` / union with a `Dynamic` arm | **85** | CLOSED — the Tier B/C cliff |
| concrete non-nil arm (`String?` 27, `Hash[…]?` 24, `MatchData?` 5, `Array[…]?`, `Mutex?`, `Time?`, …) | **93** | NOT the closed track — see below |

The 93 concrete-arm rows satisfy rigor-rs's own FP-safety requirement, so the
standing conclusion does not reach them. They are **not** a new slice either;
see bucket 07b under BLOCKED.

### 08 — always-truthy/falsey (129, unchanged)

129 rows over 71 files, largest file 11 rows, no mechanism. 101 of them are the
newer "always falsey" arm, dominated by mail (66). The one legible sub-family is
dependabot's `bin/dry-run.rb` (10 rows): `$options = {…}` folded as a hash
literal, then `$options[:write]` read as absent → falsey, while OptionParser
callbacks set those keys at runtime. That is the literal/mutation-widening
family the 141-note already named. **CLOSED**, and it takes bucket 09 with it.

### 09 — global-variable typing is worth 27 rows and all 27 are runtime-wrong

The port has no global-variable typing at all (`$s = "abc"; $s.zz` → reference
fires, port silent). The measured prize: 18 UM rows in
`concurrent-ruby/examples/benchmark_read_write_lock.rb` where
`$options = OpenStruct.new; $options.threads = 100` — accessors that
`OpenStruct#method_missing` creates at runtime — plus the 9 `dry-run.rb`
always-falsey rows above. Closing the mechanism imports 27 diagnostics on
correct code, in 3 files of 2 corpora. **CLOSED, not deferred.**

## BLOCKED

### 07b — possible-nil with a concrete arm: gated on #10, then a 35-source tail

93 rows, 37 files, 5 corpora. Two findings, both probed:

**(i) The gate is scope descent, not the nilable-source allow-list.** The port
already has a general nilable-source path — `rigor-infer/src/lib.rs:3236-3300`
mints from `Regexp.last_match`, `String#[Range]`, `Array#[Range]`, and any
`method_return_nilable(cls, meth)`. Reline's 7 rows bisect cleanly
— four variants, one construct apart:

| shape | ref | port |
|---|---|---|
| `class W; def m; recs = 0.chr*20*80; 0.upto(3) { \|i\| r = recs[i*20,20]; r[0,2] } ; end; end` | fires | **fires** |
| the same with the seed inside `if @x != 0` | fires | silent |
| the same with the seed inside `while @buf.empty?` | fires | silent |

**(ii) Beyond that, the sources are a long tail.** Grouping by the nilable's
origin (measured on the 79 rows whose seed assignment resolves) gives **36
distinct sources**; the largest is
`RubyVM::YJIT.runtime_stats` at **24 rows in one method of
`gitlab-foss/lib/gitlab/metrics/samplers/ruby_sampler.rb`** — and `RubyVM` is
another class the reference has and the port does not resolve (bucket 13's
namespace family). After it: `String#[](int,int)` 7, `Array#pop` 4,
`Regexp#match` 5, `ENV[]`/`ENV.fetch` 3, `$1`/`$2`/`$~` 3, then ones and twos.

Verdict: **BLOCKED on #10's substrate**, and even with it this is a 1–7-rows-per-
source grind — the flow-frontier note's "deep, per-cluster effort for a handful
of gaps", now measured rather than asserted. Do not open it as a slice.

### 13 — the receiver-typing tail has no mechanism

189 rows, **125 files**. Grouped by (file, receiver class, method): **111 rows
are singleton groups**, the largest group is 9, and only 4 groups exceed 3:

| rows | site | adjudication |
|---:|---|---|
| 9 | net-ssh `test/integration/test_password.rb` — `Object#expects` | the Object-note's mechanism D, already rejected (mocha DSL, one file) |
| 8+5 | concurrent-ruby `spec/concurrent/actor_spec.rb` — `Integer#ask!`/`#ask` | the reference types `AdHoc.spawn!` as `Integer`; `spawn!` returns an actor `Reference` → reference mis-type |
| 4 | dependabot `definition_ruby_version_patch.rb` — arity on `Gem::Specification#new` | namespace family below |
| 3 | mastodon `base_measure.rb` — `bool#[]` | reference mis-type |

A named sub-family worth an INVESTIGATION (not a slice): **29 rows name a class
the port's RBS DECLARES but does not resolve** — `Bundler::*` (12), `Gem::*`
(10), `Psych::DisallowedClass` (4), `ENV`/`RBS::Unnamed::ENVClass` (3). Probe: the port fires on `StringIO.new.zz`,
is silent on `Gem::Specification.new.zz` and `Psych::DisallowedClass.new("a").zz`
where the reference fires, and both engines are silent on `Pathname.new("x").zz`
and `Psych::Parser.new.zz`. Since `core/rubygems/*.rbs` and the
`overlay/vendored_gem_sigs/bundler` tree ARE vendored, and `rbs.rs`'s
`UNBUILDABLE_DEFINITIONS` comment explicitly says "rigor-rs resumes witnessing
`Bundler*` / `Gem::*`" since `v0.3.2`, this is either a regression or the
ADR-0033 leniency applying wider than intended. **Determine which before
costing it.** 29 rows, 3 corpora.

## ACTIONABLE

### 10 — nested-scope receiver typing: 86 rows in the bucket, **71 verified**

**The shape.** The port types a local receiver only at TOP LEVEL. Inside a
`def`, inside a block, or as a block parameter, it has nothing. Two probes make
the whole substrate visible.

Block parameters — the reference types a block param from
the RBS block signature, from array/hash literal element types, through `do…end`,
through reassignment and through nested blocks; **the port fires on none of the
twelve shapes**:

```ruby
OptionParser.new { |a| a.zz_a }   # REF: undefined method `zz_a' for OptionParser   RS: silent
"abc".each_char  { |c| c.zz_b }   # REF: … for String                               RS: silent
File.open("x")   { |f| f.zz_c }   # REF: … for File                                 RS: silent
```

Locals inside any nested scope — reference fires on all
six, **port on none**, including the straight-line `def m1; s = "abc"; s.zz1; end`.
Root cause is on record: `ScopedEnv::at` (`rigor-rules/src/lib.rs:2763`) hands
every use site inside a `Definition` span an empty env, and the one pass that
does re-type locals there — `collection_shape_snapshots`
(`rigor-infer/src/lib.rs:4180`) — filters its *recording* step through
`coll_carrier` (`:4505`), allow-listed to `Array`/`Hash`.

**The worked example.** `rdoc-7.2.0/lib/rdoc/options.rb:821` —
`opt.separator nil` inside `opts = OptionParser.new do |opt|` (line 727).
`OptionParser#separator: (String string) -> void`, so the literal `nil`
mismatches. Scope is the whole story:

| receiver binding | ref | port |
|---|---|---|
| top-level `o = OptionParser.new` | fires | **fires** |
| inside a `def` | fires | silent |
| inside any block | fires | silent |
| block param of `OptionParser.new do \|o\|` | fires | silent |

The top-level control is the crediting evidence the narrowing-arc lesson
demands: the ATM machinery already witnesses `OptionParser#separator` and the
`nil` argument. Only the receiver's type at the use site is missing.

**Predicted row count: 71, and it is MEASURED, not predicted.** The cheapest
verification needs no build — the port types top-level locals, so *lift* the
block body out of its scope and hand it the same type the slice would derive
(both engines on the lifted file):

| file | reference in situ | port in situ | port on the LIFTED file |
|---|---:|---:|---:|
| `rdoc/options.rb` | 48 `separator` ATM | 0 | **48, byte-identical to the reference** |
| `rdoc/ri/driver.rb` | 23 `separator` ATM | 0 | **23, byte-identical to the reference** |

The other 15 rows in the bucket need a SECOND mechanism each and are **not**
credited: `options.rb`'s 4 `opt.accept Template` rows need `Template = Object.new`
constant typing (control probe: the port stays silent on `accept` even with a
top-level typed receiver), 5 need rdoc RBS, and the rest are ActiveSupport
methods reached through collection-element typing.

**Upper bound for the substrate, measured the same way:** across all 799 rows,
**291 have a receiver bound inside a `def` or block body**; removing the
rdoc-unknown rows and the CLOSED flow buckets leaves **130** as the substrate's
ceiling — 43 files, 7 corpora. 71 verified, 9 already-rejected mocha rows, and a
55-row tail each needing its own extra mechanism.

**Risks, in order.**

1. **The whole verified prize is one narrow rbs signature.** `opt.separator nil`
   *runs clean* (`ruby -roptparse -e 'o=OptionParser.new; o.separator nil; o.help'`
   → OK); `optparse/0/optparse.rbs:879` declares `(String string) -> void` while
   the implementation accepts anything. If ruby/rbs widens it to `(String?)`,
   **all 71 rows retract at the next rbs bump and the slice's measured prize
   goes to zero** — the [upstream-retraction](20260909-repin-v038.md) hazard,
   pointed at us. Worth filing upstream at ruby/rbs either way; file it *after*
   deciding, not before.
2. **Concentration.** 71 of 71 are one method in two files of one vendored
   bundle. Excluding rdoc's OptionParser pair, the substrate's whole remaining
   pool is 55 rows over 41 files — real generality, but one to two rows at a time.
3. **FP surface.** The port's silence inside `def` bodies is protective: leaking
   top-level names into defs produced 4 measured FPs
   ([survey-FP triage](20260731-survey-fp-triage-24.md)). The slice must reuse
   `collection_shape_snapshots`' fresh-env descent (which already binds any RHS
   type) and widen only the recording carrier — never leak the enclosing env.

### 01 — Prism parse diagnostics (9 rows)

`rigor-survey/Ruby/searches/{binary,linear,ternary}_search.rb` and
`fibonacci_search.rb` do not parse (`puts if cond` followed by a dangling
`else`). The reference reports Prism's errors (`unexpected 'else', ignoring it`);
the port reports nothing. In-situ on `binary_search.rb`: REF 2, RS 0. A
reporting-surface gap with no standing decision recorded anywhere in
`AGENTS.md`, `docs/adr/` or `PORT_BACKLOG.md`. 9 rows, 4 files, 1 corpus.
Cheap, but it is diagnostics about broken files, not inference.

## Ranked recommendation

**1. Nested-scope receiver typing (bucket 10). Predicted prize 71 rows,
verified by lifting, upper bound 130.** This is the only mechanism in the gap
set above 13 rows that is both actionable and general, and the substrate it
builds — block parameters typed from RBS block signatures, plus a nominal/scalar
carrier for locals inside `def`/block bodies — is the precondition for bucket
07b and a good part of bucket 13. Build it in that order: carrier widening
first (it is the Object-note's already-templated mechanism D and has its own
9-row control), block-param typing second (it is what the 71 rows actually
need). Gate on the standing 0-FP sweep; the FP risk is real and named above.

**Before starting, decide risk 1.** If the project's answer to "our largest
coverage slice is worth 71 rows of one over-narrow rbs signature" is that a
future rbs fix retracting them is acceptable, build it — the substrate keeps its
value after the rows go. If not, the honest answer is that **nothing in the 799
is worth building**, and this note is the evidence for that.

**2. Investigate the declared-but-unresolved namespaces (29 rows).** Not a
slice until someone determines whether `Gem::*` / `Bundler::*` /
`Psych::DisallowedClass` / `ENV` silence is a regression against `rbs.rs`'s own
"rigor-rs resumes witnessing `Bundler*` / `Gem::*`" claim, or the ADR-0033
leniency reaching further than intended. One afternoon, and it may be a bug fix
rather than a coverage slice.

**3. Nothing else.** Every remaining bucket is either CLOSED, or a tail whose
largest group is 9 rows in one file of one corpus.

## What this retires, and the running total

- **395 of 799 rows (49%)** are behind decisions taken or reference FPs
  adjudicated here: rdoc 93, always-truthy 129, possible-nil `Dynamic` arm 85,
  receiver-`nil` 38, `Class.new` scope 22, gvar/OpenStruct 18, nested `def` 9,
  rbs-build 1.
- **New closures in this pass: 92 rows** — the rdoc bucket's 42 unhinted rows,
  the 22 `Class.new` rows, the 18 OpenStruct/gvar rows, the 9 nested-`def` rows,
  and the 1 `rbs.coverage` row. Against that, **93 possible-nil rows are
  RE-OPENED** out of the Tier B/C closure: the closure is measured to cover the
  85 `Dynamic`-arm rows, not the bucket. The two roughly cancel; the useful
  change is that both halves are now measured per row instead of assumed.
- Two paste-ready upstream repros: the nested `def` (bucket 05) and the
  `Class.new(X) do … end` top-level-scope body in its `unresolved-toplevel`
  flavour (bucket 04, the `undefined-method` flavour was already filed).
- One ruby/rbs observation: `OptionParser#separator: (String string)` is
  narrower than the implementation, and it is worth 71 rows of this sweep.

## Probes that surprised us

- **The `pre_eval:` hint is boilerplate on every `unresolved-toplevel` message.**
  Bucketing 799 rows by that string over-counts the monkey-patch cluster by 31.
- **A single-file probe over-reports against the sweep.** In situ,
  `rdoc/options.rb` gives the reference 57 diagnostics; the census has 52. The 5
  missing rows are `singleton(RDoc::Parser)` / `singleton(RDoc::RI::Paths)`
  calls that the sweep's project context resolves from rdoc's own source and an
  isolated probe cannot. Never size a project-class bucket from an isolated file.
- **The port fires `separator nil` for a top-level local and for nothing else** —
  the four-row scope table above. The mechanism was never "ATM coverage".
- **Handing the port `opt = OptionParser.new` *inside* the block changed
  nothing** (0 → 0). The first attempt at verifying the prize was itself blocked
  by the same substrate; only lifting the body to top level measured it.
- **`Class.new(Base) do define_method(:parse) { source } end` alone does not
  reproduce bucket 04** — the reference is silent. The RSpec `describe`/`before`
  nesting is load-bearing in the repro.
- **`Psych::DisallowedClass` and `Gem::Specification` are in the port's vendored
  RBS and still unwitnessed**, while `StringIO` in the same tree is witnessed.
- **`3.times { |i| … }` types `i` as `int<0, 2>` in the reference** — block
  parameters carry refined literal ranges, so a port of the mechanism must
  decline to a plain class rather than try to match that.

## Reproducing

Census dump reused as-is (`gap_census.py --sweep` costs ~40-80 min at this pin
because of the upstream rufo perf regression). Every probe: pinned Ruby first —

```sh
export PATH=~/.local/share/mise/installs/ruby/4.0.5/bin:$PATH   # rbs 4.2.0
ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib \
     reference/rigor/exe/rigor check <abs-file> --format json --no-cache   # from a fresh temp cwd
target/release/rigor check <abs-file> --format json
```

The default `ruby` on this host is 4.0.6 with a broken rbs 4.0.2 and fails in a
way that reads as "the reference found nothing" — a probe runner MUST print
INVALID and exit non-zero when the reference emits no parseable JSON.
