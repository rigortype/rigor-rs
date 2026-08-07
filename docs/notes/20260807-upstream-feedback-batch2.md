# Upstream feedback, batch 2 — reference-side defects since the 2026-07-16 note (2026-08-07)

Batch 1 is [20260716-upstream-feedback.md](20260716-upstream-feedback.md) (five
items, RC-era, all still standing as far as we know). Everything below was found
*after* it by the differential harness / sweep work, is **reference-side** (the
port is silent or correct at each), and was live-verified in this repo on the
date of the linked note. Items are ordered by how much upstream is likely to
care. Already-fixed-upstream findings are listed at the end so the loop is
closed and nobody re-reports them.

## 1. NEW master regression: `c7f28da1` (#271) pins a nested `Struct.new`'s
   member to the empty literal and fires on correct code

Found by the `v0.3.1 → 80aaf9bc` survey
([note](20260807-upstream-survey-v031-to-master.md)) — it is one of only TWO
diagnostics that 150 commits move across 9204 swept files, and it is a false
positive. Minimal repro (2 files, fresh cwd):

```ruby
# lib.rb
module Pkg
  class Parser
    Result = Struct.new(:items, :errors)
    def self.parse(text)
      r = Result.new([], [])
      text.split(",").each { |t| r.items << Item.new(t) }
      r
    end
  end
  class Item
    def initialize(name) = @name = name
    def local = @name
  end
end
# use.rb
p Pkg::Parser.parse("a,b").items.first.local
```

| engine | verdict |
|---|---|
| reference `v0.3.1` | silent |
| reference master (bisected to `c7f28da1`) | `error: undefined method 'local' for nil` |
| rigor-rs | silent |

`c7f28da1` is itself an FP *fix* (cross-file nested `Data`/`Struct` constant
resolution, #271), but typing the constant also lets `Result.new([], [])` pin
`items` to the **empty** array literal; `.first` then folds to `nil` and the
next call errors — on code whose array is filled by `<<` one line later. Live
instance in the wild: mail's
`spec/mail/parsers/address_lists_parser_spec.rb:32`. Suggested upstream action:
widen (or refuse to fold `.first`/`.last` on) a literal-pinned member that is
mutated after construction — the same mutation-widening polarity Rigor already
applies to locals.

## 2. Systemic possible-nil FP class: a `Dynamic` union arm satisfies the
   method-presence check for EVERY method name

From the Tier B/C adjudication ([note](20260717-tier-bc-track-closed.md), pin
was the v0.3.0-RC `47ec8625`; mechanism re-confirmed by probe).
`union_method_present_on_non_nil?` → `method_present_anywhere?`
(`check_rules.rb:1219` / `:1226` at that pin) returns permissive `true` when the
arm's concrete class name is nil (Dynamic/Top/Bot). Consequence: once a local is
typed `Dynamic[top]?` — which interprocedural nilable-return threading produces
constantly on idiomatic Ruby — **every** call on it fires possible-nil. Probe:
the reference fires on `scope.frobnicate_xyz`, a method that exists nowhere, so
the "is the method even present on the non-nil arm" gate is vacuous exactly
where it is needed most.

Measured cost: a 1-in-8 sample of gitlab-foss `lib` (585 files) gave 16
reference-only possible-nil diagnostics; **all 16 adjudicated by reading the
code are false positives** (6 nil-safe `present?`/`blank?`, 7 correctly guarded
incl. raising helpers, 3 unprovable-invariant conservatisms). Upstream's own
ADR-58 already attributes "94% of possible-nil errors" to this shape; the
concrete suggestion is narrower than ADR-58's demand-gating: require a nameable
concrete arm before witnessing (that requirement is exactly what keeps the
port's possible-nil at 0 FP on the same corpora).

## 3. A failed definition build silently blinds a whole class — `class_known?`
   stays true, every method call degrades to Dynamic

From the BigMath investigation
([note](20260731-bigmath-ingestion-asymmetry.md)).
`RbsLoader#instance_definition` / `#singleton_definition` (`rbs_loader.rb:728` /
`:802` at pin `v0.3.1`) rescue `RBS::DefinitionBuilder` errors and memoise
`nil`, so the class stays "known" while every call on it — real methods and
typos alike — goes unwitnessed, with no warning anywhere. Twelve of the 1356
configless declarations fail this way at the pin
(`harness/unbuildable_classes.rb --check` regenerates the list). Two aggravating
properties:

- **Environment-dependence:** the `BigMath` entry exists only because the
  *installed* `bigdecimal` gem's `sig/big_math.rbs` collides with rbs's own
  `stdlib/bigdecimal-math/0/big_math.rbs` (`DEFAULT_LIBRARIES` lists both names,
  and `RBS::EnvironmentLoader` prefers an installed gem's `sig/`). Remove the
  gem and the same pinned Rigor fires on the same file — the oracle's answer is
  a fact about the host, not the version. A/B measurement is in the note.
- The other 11 entries collide the reference's own `data/vendored_gem_sigs/`
  with rbs's `sig/shims/` + `core/rubygems/`, so they are pin-stable but still
  silent.

Suggested upstream action: when a definition build raises, surface it (a
one-line warning naming the class and the two colliding files would do — the
information is in the exception) instead of failing soft; optionally prefer one
source when the duplicate is byte-identical modulo location.

## Already fixed upstream — no action, listed to close the loop

- **#237** — a `sig/` referencing an undeclared interface/alias made the whole
  stub batch silently unparseable and discarded (project loses its entire
  `sig/`). Fixed by `9515c8f8` + `5bd0aac2`; we carry it as the divergence
  registry's first excused entry until the pin passes the fix
  ([probe note](20260731-project-sig-blind-spot-probe.md)).
- **Invalid-UTF-8 `sig/` file crashed the analyzer** (`internal analyzer error:
  ArgumentError`) — same probe matrix, observed fixed upstream.
- **#239** — instance/class same-name method resolution (haml
  `ScriptCompiler.find_and_preserve`; the rdoc `pre_process.rb:161` drop in the
  survey is its fix landing, `138fedd7`).

## Reporting notes

Item 1 needs no port-side change ever (the port should NOT chase that gap after
a future pin bump — the survey note says the same). Items 2–3 are long-standing
behaviours, not regressions; each has a self-contained repro above or in the
linked note that can be pasted into an upstream issue as-is.
