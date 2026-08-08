# The `Object`-receiver bucket, adjudicated — 30 rows, one slice-sized mechanism (2026-08-08)

The [bare-class characterisation](20260808-bare-class-bucket-characterisation.md)
left the `Object` bucket as the one sampled cluster it refused to score
("adjudicate before building"). This is that pass: every row read at its real
source, reduced, and probed against both engines.

Measured on a fresh census (`gap_census.py --sweep`, **1125 rows**, pin
`v0.3.1`, reference `c39e6675`). Selecting rows whose reference-reported
receiver renders as bare `Object` yields **30**, not the 26 the earlier
histogram showed: 26 `call.undefined-method` **plus 4 `call.wrong-arity`**
(`Object#new`), which belong to the same mechanism and are adjudicated with it.

## Verdict summary

| verdict | rows | mechanism |
|---|---:|---|
| **REFERENCE FP** | 16 | `Class.new do … end` anonymous class — the block body's `def`s are lost |
| **REFERENCE FP** | 2 | `class << Const = Object.new` singleton reopening |
| **decline (ADR-0035, inline RBS deferred)** | 3 | `#: (Object)` rbs-inline param annotation |
| **closable, named mechanism** | 9 | def-body local seeded by `<Const>.new` — sibling of the collection-shape snapshot family |

18 of 30 are runtime-wrong diagnostics; 3 sit behind a standing decision; 9 are
mechanically closable and, per the recommendation below, still not worth a slice.
**No row needs machinery rigor-rs lacks** — the "new mechanism" class is empty.

## Per-row table

`corpus / file:line` relative to `/Users/megurine/repo/ruby/`.

| corpus | file:line | rule | method | mechanism | verdict | probe |
|---|---|---|---|---|---|---|
| concurrent-ruby | `rigor-survey/concurrent-ruby/spec/concurrent/agent_spec.rb:104` | arity | `new` | A anon-class | REFERENCE FP | ref fires / rs silent |
| concurrent-ruby | `…/agent_spec.rb:261` | UM | `count` | A | REFERENCE FP | ditto |
| concurrent-ruby | `…/agent_spec.rb:302` | UM | `count` | A | REFERENCE FP | ditto |
| concurrent-ruby | `…/agent_spec.rb:810` | UM | `count` | A | REFERENCE FP | ditto |
| concurrent-ruby | `…/agent_spec.rb:866` | UM | `count` | A | REFERENCE FP | ditto |
| concurrent-ruby | `…/async_spec.rb:51` | arity | `new` | A | REFERENCE FP | ditto |
| concurrent-ruby | `…/async_spec.rb:52` | UM | `args` | A | REFERENCE FP | ditto |
| concurrent-ruby | `…/async_spec.rb:65` | UM | `block` | A | REFERENCE FP | ditto |
| concurrent-ruby | `…/async_spec.rb:295` | UM | `async` | A | REFERENCE FP | ditto |
| concurrent-ruby | `…/async_spec.rb:296` | UM | `await` | A | REFERENCE FP | ditto |
| concurrent-ruby | `…/async_spec.rb:297` | UM | `bucket` | A | REFERENCE FP | ditto |
| concurrent-ruby | `…/async_spec.rb:308` | UM | `async` | A | REFERENCE FP | ditto |
| concurrent-ruby | `…/async_spec.rb:309` | UM | `await` | A | REFERENCE FP | ditto |
| concurrent-ruby | `…/concern/dereferenceable_shared.rb:124` | arity | `new` | A | REFERENCE FP | ditto |
| mail | `rigor-survey/mail/…/erb-6.0.4/lib/erb/compiler.rb:481` | UM | `c` | A | REFERENCE FP | in-situ ref fires / rs silent |
| mail | `…/erb/compiler.rb:481` | arity | `new` | A | REFERENCE FP | census row; same site |
| lib (haml) | `rigor-survey/haml/lib/haml/parser.rb:458` | UM | `merge_attributes!` | B singleton-const | REFERENCE FP | in-situ ref fires / rs silent |
| lib (haml) | `…/haml/lib/haml/parser.rb:464` | UM | `merge_attributes!` | B | REFERENCE FP | ditto |
| mail | `rigor-survey/mail/…/rdoc-7.2.0/lib/rdoc/markup/table.rb:26` | UM | `header` | C inline RBS | decline (ADR-0035) | reduction reproduces |
| mail | `…/rdoc/markup/table.rb:27` | UM | `align` | C | decline (ADR-0035) | ditto |
| mail | `…/rdoc/markup/table.rb:27` | UM | `body` | C | decline (ADR-0035) | ditto |
| net-ssh | `rigor-survey/net-ssh/test/integration/test_password.rb:19` | UM | `expects` | D def-body local | closable | top-level control fires in BOTH |
| net-ssh | `…/test_password.rb:20` | UM | `expects` | D | closable | ditto |
| net-ssh | `…/test_password.rb:21` | UM | `expects` | D | closable | ditto |
| net-ssh | `…/test_password.rb:33` | UM | `expects` | D | closable | ditto |
| net-ssh | `…/test_password.rb:34` | UM | `expects` | D | closable | ditto |
| net-ssh | `…/test_password.rb:35` | UM | `expects` | D | closable | ditto |
| net-ssh | `…/test_password.rb:45` | UM | `expects` | D | closable | ditto |
| net-ssh | `…/test_password.rb:46` | UM | `expects` | D | closable | ditto |
| net-ssh | `…/test_password.rb:47` | UM | `expects` | D | closable | ditto |

## Mechanism A — `Class.new do … end` (16 rows): REFERENCE FP

Every concurrent-ruby row and the erb pair are the same idiom: an anonymous
class built with a block, then constructed. Reduction:

```ruby
observer_class = Class.new do
  attr_reader :count
  def initialize(bucket); @bucket = bucket; end
end
o = observer_class.new([])
p o.count
```

Reference: `wrong number of arguments to 'new' on Object (given 1, expected 0)`
and `undefined method 'count' for Object`. rigor-rs: silent. **`ruby` runs the
file clean** — both diagnostics are runtime-wrong.

The reference's own output names the root cause: it also emits `unresolved
toplevel call to 'attr_reader'` inside the block, i.e. it analyses the
`Class.new` block body in **top-level scope**, not as a class body. The block's
`initialize` and `attr_reader` are therefore invisible, the anonymous class
degrades to bare `Object`, and `Object.new`'s zero-arity signature plus
`Object`'s method table produce both diagnostics. Closing these would mean
rigor-rs emitting the same wrong output.

## Mechanism B — `class << Const = Object.new` (2 rows): REFERENCE FP

haml's private `AttributeMerger` (`parser.rb:881`) is
`class << AttributeMerger = Object.new`. Reduction:

```ruby
class << Merger = Object.new
  def merge_attributes!(a, b) = a
end
Merger.merge_attributes!({}, {})
```

Reference: `undefined method 'merge_attributes!' for Object`. rigor-rs: silent.
`ruby` runs it clean. The reference resolves the constant to `Object` from its
initialiser and never attaches the singleton body opened on the same node.
Runtime-wrong → do not close.

## Mechanism C — rbs-inline `#: (Object)` param (3 rows): decline, ADR-0035

rdoc's `Table#==` carries `#: (Object) -> bool`. The reference honours the
annotation, types `other` as `Object`, and fires on `other.header/.align/.body`.
Two probe findings:

- Deleting only the `#:` line silences the reference — the annotation is the
  sole carrier.
- The reference fires **without** `-I plugins/rigor-rbs-inline/lib`. It
  **auto-wires** the bundled plugin whenever the `rbs-inline` gem is resolvable
  (`configuration.rb:238-252`, upstream ADR-93 WD2). So
  [ADR-0035](../adr/0035-inline-rbs-deferred.md)'s premise "a default run
  ingests no inline RBS … the corpus differential never enables the plugin" is
  **stale at this pin** — the sweep does enable it. The *decision* still holds
  (deferring can only cost coverage, never an FP), but the rationale text should
  be corrected.

Scale of the deferral, measured: across all 1125 gap rows only **4** sit in a
file carrying any `#:` / `# @rbs` annotation (these 3, plus one rdoc
`heading.rb` row already behind the ADR-17 `pre_eval:` decision). Inline RBS is
worth ≤4 rows on the standing sweep.

## Mechanism D — def-body local seeded by `<Const>.new` (9 rows): closable

All 9 net-ssh rows are `ps = Object.new` / `pt = Object.new` inside a `def`,
then `.expects(…)` (mocha). Bisected:

| shape | ref | rigor-rs |
|---|---|---|
| top level `x = Object.new; x.expects(:ask)` | fires | **fires** |
| top level `x = String.new; x.frobnicate_zzz` | fires | **fires** |
| in a `def`, same two lines | fires | silent |
| in a `def`, `a = []; a.frobnicate_zzz` | fires | **fires** (collection-shape slice) |
| in a `def`, `s = "hello"; s.frobnicate_zzz` | fires | silent |
| in a `def`, `Object.new.frobnicate_zzz` (no local) | fires | **fires** |

The top-level control is the crediting evidence the ledger's narrowing-arc
lesson demands: the consumption gate **does** witness `Object` and **does**
witness `expects`' absence — the only missing piece is the receiver's type at
the use site.

Root cause, located in source: `ScopedEnv::at`
(`crates/rigor-rules/src/lib.rs:2763-2787`) hands every use site inside a
`Definition` span an **empty** env, because `build_toplevel_env`
(`crates/rigor-infer/src/lib.rs:1975`) never descends into method bodies and
leaking top-level names into `def`s produced 4 measured FPs
([survey-FP triage](20260731-survey-fp-triage-24.md)). The one pass that *does*
re-type locals inside `def` bodies is `collection_shape_snapshots`
(`crates/rigor-infer/src/lib.rs:4180`), whose fresh-env descent binds **any**
RHS type but whose *recording* step filters through `coll_carrier` (`:4505`),
allow-listed to `Array`/`Hash`. So the named family is the **collection-shape
snapshot family**, and this would be its nominal/scalar sibling: same
`coll_flow_*` descent, same `Dynamic`-only consumption gate in
`check_collection_call` (`crates/rigor-rules/src/lib.rs:1531-1564`), a widened
carrier predicate.

**Blast radius, bounded.** A heuristic scan of the whole gap set for
"bare-identifier receiver, inside a `def`, seeded earlier in that `def` by
`<Const>.new` or a scalar literal" returns **33 rows** — an upper bound
(proximity, not mechanism — the census-window lesson). Of those: 9 are this
bucket (verified); 6 carry the reference's `pre_eval:` hint and 12 more are
`RDoc::*` project classes, all behind the [141-row
adjudication](20260807-gap-adjudication-141.md) and the ADR-0033 provenance
gate; 2 are `Class.new(super) do … end` (mechanism A, a REFERENCE FP); leaving
**≤4** adjacent rows (2 `String` chain/interpolation seeds, 2
`Bundler::Source::Git`). So the realistic ceiling for the whole slice is **9
verified + ≤4 speculative**.

## Upstream-feedback repros

Two paste-ready, both runtime-clean under `ruby`, both silent in rigor-rs.

**1. `Class.new do … end` block body analysed in top-level scope** — the
`Class.new` block's `def`s and `attr_*` never reach the anonymous class, which
degrades to `Object`; the stray `unresolved toplevel call to 'attr_reader'` is
the tell. Produces both a bogus `wrong-arity … on Object` and a bogus
`undefined method … for Object`. 16 rows on the standing sweep across
concurrent-ruby specs and erb's `WARNING_UPLEVEL`. Repro: mechanism A above.

**2. `class << Const = Object.new` singleton body dropped** — the constant is
typed from its `Object.new` initialiser and the singleton class opened on the
same node is never attached, so every method defined there reads as undefined.
2 rows (haml's `AttributeMerger`). Repro: mechanism B above.

## Recommendation

**The `Object` bucket goes behind decisions entirely — do not build a slice for
it.** 18 of its 30 rows are runtime-wrong reference output that rigor-rs is
correctly silent on (closing them would import two upstream defects), 3 are
ADR-0035's deferred inline-RBS leg (whose total cost across the sweep is 4
rows), and the one genuinely closable mechanism — nominal/scalar local seeding
inside `def` bodies, the collection-shape snapshot family's sibling — is worth
**9 verified rows in one file of one corpus, plus ≤4 speculative rows
elsewhere**. That is the same "one file, no generality" profile that the
[characterisation note](20260808-bare-class-bucket-characterisation.md) used to
reject the OpenStruct rows, and the 9 rows are all mocha's `Object#expects`, a
DSL the configless environment cannot know — a diagnostic class both engines
already agree on at top level, so closing it buys parity on output that is
itself runtime-hostile. Two cheap non-slice follow-ups do have value: send the
two upstream repros, and correct ADR-0035's now-false claim that the corpus
differential never enables the rbs-inline plugin. If the def-body local
mechanism is ever built, build it for its own sake (it is a real, FP-safe,
already-templated extension) — not on the strength of this bucket.
