# Gap adjudication: pre_eval / nil-receiver / rdoc-Hash clusters — all three CLOSED (2026-08-07)

Adjudication pass over three `call.undefined-method` clusters from the
post-merge gap census re-dump (1168 rows, pin `v0.3.1`, measured 2026-08-07):
**141 rows, 33% of the 421-row undefined-method pool**. Method follows
[the Tier B/C note](20260717-tier-bc-track-closed.md): every sampled site
adjudicated by reading the actual source; mechanisms confirmed against the
pinned oracle (fp_audit recipe: fresh temp cwd, `--no-cache`, pinned plugin
path). **Verdict: all three clusters CLOSE as no-go. 141/141 adjudicated rows
are diagnostics on runtime-correct code; 0 real bugs found. Closing any of
them would import the reference's FPs or delete a named FP-safety mechanism.**

| cluster | rows | sites read | real bugs | verdict |
|---|---:|---:|---:|---|
| A. `pre_eval:` cross-file monkey-patch (mail/rdoc) | 49 | 8 cited def sites verified + 4 oracle probes | 0 | **no-go** |
| B. receiver typed `nil` | 63 | all 63 (27 distinct shapes) + 5 probes | 0 | **no-go** |
| C. rdoc `Hash[Symbol, Dynamic[top]]` receivers | 29 | binding traced through generated code | 0 | **no-go** |

## Cluster A — 49 `pre_eval:` rows: the reference fires on methods it can see are defined

**Mechanism, probed and minimized to 2 files.** Three facts compose:

1. `RDoc::*` is RBS-known in the reference's configless environment — but only
   **transitively**: `DEFAULT_LIBRARIES` has no `rdoc`; it has `rbs`, and the
   *installed* rbs gem's own `sig/manifest.yaml` declares `dependencies: -
   name: rdoc`, so `RBS::EnvironmentLoader` pulls rbs-stdlib `rdoc/0/*.rbs` in.
   Probe: configless `RDoc::Constant.new(...).is_alias_for` fires. (Note the
   oracle-env flavor: this knowledge is a fact about the installed rbs gem's
   manifest, the same host-dependence family as the BigMath entry.)
2. Those sigs are **stale vs rdoc 7.2.0** — e.g. `rdoc/0/rdoc.rbs:299` gives
   `RDoc::Constant` only `attr_writer is_alias_for`, no reader; the gem source
   defines the reader at `code_object/constant.rb:88`.
3. ADR-17 by design (`check_rules.rb:2486-2503` at the pin): when the project
   defines the missing method **in another file**, the reference names the
   definition site in the message and *"the diagnostic still fires"*, pointing
   at `pre_eval:`. Same-file def suppresses (probed silent); cross-file def
   fires (probed: 2-file `class RDoc::Constant; def is_alias_for; …` +
   `RDoc::Constant.new(…).is_alias_for` reproduces the exact hint message).

Every one of the 49 rows carries the hint, i.e. **by construction the
reference located the project's definition of the very method it reports**.
Spot-verified 8 cited sites by reading rdoc 7.2.0 source — all real
(`constant.rb:88`, `alias.rb:87`, `attr.rb:76`, `comment.rb:125/132/217`,
`markup/document.rb:40 <<`, `store.rb:264`). The vendored rdoc is shipped,
working production code: 49/49 diagnostics are on correct code — the
reference's configless answer is a config nudge rendered as an `error`.

**Can rigor-rs close these without deleting the ADR-0033 leniency? No — it
would have to invert it.** rigor-rs's silence is triply determined: (a) no
rdoc RBS at all (the vendored stdlib mirrors `DEFAULT_LIBRARIES` without the
installed-gem transitive closure); (b) no cross-file block-param/attr receiver
typing at these sites; (c) the witnessing tail requires *"the project does not
reopen `C` with the method"* (`project_declares_method`,
`crates/rigor-rules/src/lib.rs` check_narrowed_call contract) — the exact
2026-06-26 FP-safety mechanism ("SourceIndex is never a witnessing surface")
that eliminated the measured `Struct.new`/`Alba` FP family. Closing the
cluster means firing precisely **when `project_declares_method` is true** and
the def happens to sit in a different file — deleting (c) and re-importing the
FP family it was built to stop, in exchange for 49 diagnostics that are all
wrong. **No-go. Retires 49 rows.**

## Cluster B — 63 nil-receiver rows: 63/63 runtime-correct, 0 latent bugs

All 63 rows read (27 distinct site shapes; the big repeats are one shape × N
lines). Family breakdown:

| family | rows | archetype (read at the site) |
|---|---:|---|
| guard ignored (raise-`unless`, `&&`-chain `any?`, `\|\|` after `empty?`, `if x.nil? … else`, guard via intermediate boolean, `Array === x`, `if RSpec.current_example`) | 8 | gitlab `parser.rb:75`: `raise … unless operators.last` on the *preceding line*; diff-lcs `previous_hunk.nil? \|\|` folded through a local |
| literal/empty-collection fold ignoring later mutation (the upstream-#271 polarity) | 30 | `field = nil; expect { field = Mail::Field.parse(…) }…; field.name` (block assign, 6); `ps = []` + `ps <<` in the same loop then `ps.last *` behind `count > 0` (2); cross-file `@sorted_nodes = []` → `.index` folds nil → `nil < nil` ×9; `\|\|=` on the line above (net-imap lambda, composer `json["config"] \|\|= {}`); memoization ivar (`sbt fetch`, 3); optparse-mutated `$conf` hash literal (2); nil-init ivar + setter (`ctr.rb iv=`, 2; rdoc `marshal_dump` `@parent`/`@section`, 4) |
| `Array.new(n) { block }` fill ignored → element typed nil | 5 | concurrent `Array.new(web_crawler_count) { AtomicFixnum.new }` then `counter[i].increment`; probed: `Array.new(2) { "s" }; xs[0].upcase` fires `undefined method 'upcase' for nil` |
| cross-file toplevel-`def` mis-binding | 16 | dependabot: `bundler/helpers/v2/run.rb:31` `def output(obj); print JSON.dump(obj); end` (returns nil) captures the **RSpec `output` matcher** in 16 unrelated spec files → `output(/…/).to_stdout_from_any_process` fires on nil. Probed: 2-file minimal repro reproduces exactly |
| protocol-invariant conservative (stream header arrives first) | 3 | gitaly blobs/conflict-files stitchers — runtime-correct under the RPC contract, unprovable locally |
| `defined?` argument treated as evaluated | 1 | net-ssh test: `reset_subject({}) if defined? @subject && !@subject.options.empty?` — Ruby parses the whole `&&` as the `defined?` operand and **never evaluates it**; probed 1-file repro fires |

Zero rows are latent bugs. The nearest candidates (gitaly stitchers, ctr.rb)
are the Tier B/C "unprovable-invariant conservatism" family — correct code the
analyzer can't prove, not defects. This cluster is literally the exactly-nil
corner of the closed Tier B/C imprecision: where the fold stops at `X?` the
reference emits possible-nil (435 rows, track closed); where it folds all the
way to `nil` it emits undefined-method — same machinery, harder collapse.
Closing requires porting literal/empty-collection nil-folding **without** the
guard narrowing and mutation widening that make it wrong — i.e. importing all
63. **No-go. Retires 63 rows.**

## Cluster C — 29 rdoc `markdown.rb` rows: the receiver is provably an Array

All 29 sit in rdoc's kpeg-**generated** `lib/rdoc/markdown.rb` (16k lines), on
`a << b` / `a.join` with receiver reported `Hash[Symbol, Dynamic[top]]`.
Traced in the generated code: every site has the shape

```ruby
_tmp = _StartList()
a = @result
unless _tmp … break end   # a is used only when _StartList succeeded
…
@result = begin;  a << b ; end
```

and `_StartList` (line 14818) unconditionally sets `@result = begin;  [] ; end`
— so at every use site `a` is an **Array**; `<<` and `join` are valid. The
`Hash[Symbol, …]` comes from the single Hash assignment among the file's
hundreds — line 11276 `@result = begin;  { label: label, link: link } ; end` —
i.e. a flow-insensitive ivar-arm join collapsed to the wrong arm. A minimal
two-method probe does *not* reproduce (the pin emits only the ivar
previously-assigned-Array warning), so the collapse needs the full generated
`apply/@result` plumbing — but the in-situ reading is decisive: 29/29 are
receiver-typing errors on correct generated code. Closing = replicating a
wrong arm-collapse. **No-go. Retires 29 rows.**

## The crux (same as Tier B/C — recorded again because it recurs)

`fp_audit` measures FP **against the reference**, so a port of any of these
three behaviours would score 0 FP, +141 matched, and pass the standing battery
clean. The parity gate points the wrong way for exactly this class of gap:
ADR-0002's contract is a *sound subset*, and 141/141 of these diagnostics are
unsound on the measured corpora. Closing them would be gaming the metric.
This is the second track where the divergence appears; the census's warning —
"48% of the gap set sits behind decisions already made" — now extends to
**717 of 1168 rows** (435 possible-nil + 141 always-truthy is prior art; this
note adds 141 undefined-method).

## Bookkeeping — what this retires

- Actionable `call.undefined-method` pool: 421 → **280** rows.
- Census re-labels: the "receiver typed nil" (63) and "project monkey-patch"
  (49) mechanism buckets are closed outright; the shape/collection bucket
  (92) loses its 29-row rdoc-generated core, leaving ~63 to re-examine.
- Nothing here touches the live frontier: class-narrowing slice (census
  mechanism 1) and ADR-0042 S5 return-lookup stay the measured, buildable
  targets — none of their predicted closures intersect these 141 rows.

## Do instead (ROI order)

1. Ship the already-specced buildable slices: class narrowing
   ([spec](20260807-class-narrowing-slice-spec.md)) and ADR-0042 S5
   ([spec](20260807-adr0042-s5-return-lookup-spec.md)).
2. Re-examine the ~63 non-rdoc shape/collection rows and the 147 bare-class
   rows — that is where the census says the port "has the answer and cannot
   see the question".
3. Optional investigation (not a slice): the reference's transitive
   RBS-manifest loading (`rbs` → installed gem manifest → `rdoc`) is an
   ingestion asymmetry rigor-rs does not mirror. Before ever mirroring it,
   note it is host-dependent (installed rbs gem's manifest) and its measured
   effect on this sweep is 49 FPs + 29 mis-typed receivers — mirroring is
   currently **anti-parity** for a sound subset.
4. Upstream: file the three new repros below.

## Upstream-report-worthy repros (formatted for appending to
[20260807-upstream-feedback-batch2.md](20260807-upstream-feedback-batch2.md))

### N. A project-wide toplevel `def` captures same-named DSL methods in every
   other file and types their calls by its return

2-file repro (fresh cwd, `--no-cache`, pin `v0.3.1`):

```ruby
# helper.rb — an executable script, never loaded by the specs
require "json"
def output(obj)
  print JSON.dump(obj)
end
# a_spec.rb
RSpec.describe "x" do
  it "prints" do
    expect { puts "ok" }.to output(/ok/).to_stdout_from_any_process
  end
end
```

Reference: `a_spec.rb:3: error: undefined method 'to_stdout_from_any_process'
for nil` — the toplevel `def output` (return `nil` via `print`) is bound in an
unrelated file where runtime resolves RSpec's `output` matcher (included
module beats `Object`'s private toplevel def in the MRO). Live instances: 16
diagnostics across dependabot-core's updater specs from
`bundler/helpers/v2/run.rb:31`. rigor-rs: silent.

### N+1. `Array.new(n) { block }` element type ignores the block fill

```ruby
xs = Array.new(2) { "s" }
xs[0].upcase
```

Reference: `error: undefined method 'upcase' for nil` — elements typed by the
no-block nil-fill despite the block. Live instances: concurrent-ruby
`docs-source/medium-example*.rb` (`AtomicFixnum` counters),
`spec/concurrent/cancellation_spec.rb` (`future.value!`). rigor-rs: silent.

### N+2. The `defined?` operand is analyzed as evaluated code

```ruby
class C
  def setup
    reset if defined? @subject && !@subject.options.empty?
  end
  def reset
    @subject = nil
  end
end
```

Reference: `error: undefined method 'options' for nil`. Ruby parses the whole
`&&` expression as the `defined?` operand and **never evaluates it** —
`defined?` is a non-evaluating operator; no call can raise there. Live
instance: net-ssh `test/authentication/methods/test_keyboard_interactive.rb:14`.
rigor-rs: silent.

### Observation (not a defect repro): stale transitive rdoc sigs

The 49 mail/rdoc `pre_eval:` hints and the 29 `markdown.rb` Hash-receiver
errors both stem from rbs-stdlib `rdoc/0/*.rbs` entering the configless
environment via the installed rbs gem's `sig/manifest.yaml` dependency, then
losing to rdoc 7.2.0's real source (e.g. `rdoc.rbs:299` has writer-only
`is_alias_for`). ADR-17 makes the resulting fires by-design; upstream may
still want to know that a configless sweep over any project vendoring rdoc
produces 78 diagnostics on correct code, and that the sig staleness itself
belongs to ruby/rbs.
