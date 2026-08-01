# `Object#Nokogiri` was already closed — and the sweep for its siblings found a live one next door

2026-08-01. Retires the second half of the RBS ingestion-surface asymmetry
([slice-2 note](20260725-multiwrite-substrate-s2.md) §"Follow-up", carried
forward by [the `BigMath` note](20260731-bigmath-ingestion-asymmetry.md)
§"What this leaves open").

## Verdict

**The asymmetry does not exist.** Both engines are silent on both shapes, at the
current pin, in the gate environment:

```console
$ cat nok.rb
x = "abc".Nokogiri
y = Nokogiri("<p/>")

$ ruby -I reference/rigor/lib -I …/rigor-rbs-inline/lib \
    reference/rigor/exe/rigor check --no-cache nok.rb
No diagnostics
$ rigor check nok.rb          # no output, exit 0
```

It was closed by `800b3a1` (2026-07-31 04:52, the 24-FP survey triage), which
vendored the reference's whole `data/vendored_gem_sigs/` tree into
`crates/rigor-index/vendor/rbs/overlay/`. That tree contains the ONE `class
Object` reopen in the reference's `data/`, and it is now byte-identical here:

```rbs
class Object
  def Nokogiri: (untyped string, ?untyped url, ?untyped encoding, ?untyped options) -> (…)
              | () { (Nokogiri::HTML::Builder) -> untyped } -> Nokogiri::XML::Node
end
```

`diff -r reference/rigor/data/vendored_gem_sigs
crates/rigor-index/vendor/rbs/overlay/vendored_gem_sigs` reports only `README.md`
and `prism` — the latter a deliberate exclusion (`PROVENANCE.md`: the supplement
without the set it supplements cost 8 fresh false positives).

**Why both notes were wrong is worth recording, because the failure mode is
cheap to repeat.** The slice-2 claim was TRUE when written (2026-07-25; the
vendoring landed six days later). The `BigMath` note then restated it on
2026-08-01 without re-measuring — the very discipline that note applies to its
own subject ("this was re-measured, not assumed") stopped at the paragraph
describing somebody else's open item. An inherited claim is not evidence.

## The silence is not vacuous

Silence proves nothing on its own: rigor-rs's witness gates decline for many
reasons, and ten of the twelve `UNBUILDABLE_DEFINITIONS` classes were silent
"for an unrelated reason" exactly like this. Two controls pin it.

**In-band control.** On the same file, in the same position, `"abc".Zzzzz` and
`"abc".frobnicate` both fire, byte-identical on both engines. The witness gate is
live on this receiver, at this call site, for a capitalized method name.

**Ablation.** Strip the `class Object` block out of the vendored copy and point
`RIGOR_RBS_CORE_DIR` at the result — nothing else changes:

```console
$ RIGOR_RBS_CORE_DIR=<tree minus the Object reopen> rigor check nok2.rb
nok2.rb:1:7: error: undefined method `Nokogiri' for "abc"
nok2.rb:3:3: error: undefined method `Nokogiri' for 1
nok2.rb:4:12: error: undefined method `Nokogiri' for Object
nok2.rb:2:1: warning: unresolved toplevel call to `Nokogiri`. …
```

Four diagnostics the oracle never emits — including a `call.unresolved-toplevel`
neither note predicted, because the bare `Nokogiri(…)` form is a SECOND
consumer of the same declaration. That is the divergence as it stood before
`800b3a1`, and it is what fixture 80 now guards.

## Sibling sweep — the whole `Object#`-level conversion-function family

The interesting class is not `Nokogiri` but the idiom: a method on
`Object`/`Kernel` whose name starts with a capital
(`Integer()`, `Array()`, `Nokogiri()`). Enumerated from the ORACLE's own built
definitions of `::Object` / `::Kernel` / `::BasicObject` — 67 rows, 14 distinct
names — then probed on both engines in the receiver form (`"abc".Name("x")`) and
the toplevel form (`Name("x")`).

| name | declared by | on rigor-rs's surface | `"abc".Name("x")` | `"abc".Name` (0 args) |
| --- | --- | --- | --- | --- |
| `Array` | `[pin]` rbs `core/kernel.rbs` | yes | silent = silent | arity fires = fires |
| `BigDecimal` | `[env]` `bigdecimal-4.1.2/sig` (oracle) / vendored `stdlib/bigdecimal` (here) | yes | silent = silent | arity fires = fires |
| `Complex` | `[pin]` rbs `core/kernel.rbs` | yes | silent = silent | **DIVERGED** — fixed below |
| `DelegateClass` | `[pin]` rbs `stdlib/delegate` | yes | silent = silent | arity fires = fires |
| `Digest` | `[pin]` rbs `stdlib/digest` | yes | silent = silent | arity fires = fires |
| `Float` | `[pin]` rbs `core/kernel.rbs` | yes | silent = silent | **DIVERGED** — fixed below |
| `Hash` | `[pin]` rbs `core/kernel.rbs` | yes | silent = silent | arity fires = fires |
| `Integer` | `[pin]` rbs `core/kernel.rbs` | yes | silent = silent | **DIVERGED** — fixed below |
| `JSON` | `[pin]` rbs `stdlib/json` | yes | silent = silent | arity fires = fires |
| **`Nokogiri`** | `[pin]` reference `data/vendored_gem_sigs/nokogiri` | **yes** (the item under test) | silent = silent | silent = silent¹ |
| `Pathname` | `[pin]` rbs `core/pathname.rbs` | yes | silent = silent | arity fires = fires |
| `Rational` | `[pin]` rbs `core/kernel.rbs` | yes | silent = silent | **DIVERGED** — fixed below |
| `String` | `[pin]` rbs `core/kernel.rbs` | yes | silent = silent | arity fires = fires |
| `URI` | `[pin]` rbs `stdlib/uri` | yes | silent = silent | arity fires = fires |

¹ `Object#Nokogiri`'s block overload takes zero positionals, so `min` is 0 and
the call is in range on both engines. Not a divergence; noted so the row is not
read as one.

**Ingestion verdict: the whole family is at parity, not just `Nokogiri`.** With
correct arity, a 15-line probe over all 14 names plus a `Zzzzz` control is
byte-identical between the engines. `Nokogiri` is also the family's only `class
Object` REOPEN — every other name is a `def self?.Name:` on `Kernel`, which
rigor-rs has always carried — so closing it closed the mechanism, not one name.

## What the sweep DID find: `arity_eligible?` was never ported

Four names diverge in the zero-argument column, and they are exactly the four the
reference refuses to arity-check.

`compute_arity_envelope` (`reference/rigor/lib/rigor/analysis/check_rules.rb`)
returns nil — no arity diagnostic in either direction — as soon as ANY overload
fails `arity_eligible?`: a REQUIRED KEYWORD, a trailing positional, or an
`UntypedFunction` (`(?) -> untyped`, which exposes no arity accessors at all).
`Kernel#Integer` / `Float` / `Rational` / `Complex` each carry an
`(…, exception: bool) -> …` overload — a required keyword. Their siblings
`Array` / `Hash` / `String` carry none. That single bit is the whole
discriminator, and it explains the column exactly.

rigor-rs computed the positional envelope regardless and fired:

```
nok.rb:1:7: error: wrong number of arguments to `Integer' on String (given 0, expected 1..2)
```

— a `call.wrong-arity` false positive under ADR-0002, on a rule that is not the
one this arc was about. Fixed by porting the gate: `method_signature`
(`crates/rigor-index/src/rbs.rs`) now yields an `ArityEnvelope` = `Option<Arity>`,
`None` for an ineligible method, and `CoreData::method_arity` folds that into the
`None` `check_wrong_arity` already treats as "do not check". The
`UntypedFunction` arm is included: it used to `continue`, silently leaving the
REMAINING overloads' envelope standing to be checked against.

**Blast radius, measured rather than guessed:** 507 of the 11,115 methods in the
oracle's configless universe are ineligible — `Proc#call`, `IO#read_nonblock`,
`CSV.read`, `MatchData#named_captures`, `OptionParser#on`, `BigDecimal#round`,
most of `RBS::AST::*`'s constructors. rigor-rs was already silent on the
realistic ones for unrelated reasons (variadic envelopes, untyped receivers), which
is why 9204 swept files never caught this. **The class of finding is the same one
the `BigMath` note ended on: a firing shape no real file writes is invisible to a
corpus sweep and visible only to a synthetic probe against the oracle.**

## Decision: retire the item, do not vendor anything

The slice-2 note framed this direction as needing "a vendoring decision, not a
surface mask". That decision was already taken and executed by `800b3a1`, on the
right terms — vendor the reference's OWN `data/` tree (whose provenance is the
pin), not the third-party gem `sig/` trees the reference happens to resolve to on
this host. The `prism` exclusion in `PROVENANCE.md` is the standing boundary: a
supplement is safe only when the set it supplements is vendored too. Nothing here
argues for moving that boundary.

**The guard is fixture 80** (`80_object_level_conversion_functions.rb`), which
pins both mechanisms in one file: the ingestion half (both `Nokogiri` forms silent
on both engines), a `Zzzzz` negative control, the four required-keyword names, and
the three keyword-free siblings as a POSITIVE control so the arity gate cannot
degrade into a blanket retreat from the family. Proven non-vacuous twice — the
fixture was red with 4 unregistered FPs before the arity fix, and both new unit
tests were re-broken (the gate disabled; the `class Object` block deleted from the
vendored tree).

## Measurement

| gate | result |
| --- | --- |
| `cargo test --offline` | PASS, 970 tests (+2, each proven non-vacuous by re-breaking) |
| `CARGO_TARGET_DIR=<fresh> cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `ruby harness/run.rb` | **80 fixtures**, 241 matched, **0 unregistered FP**, 3 gaps (unmoved), 1 registered divergence |
| `ruby harness/run_snapshot.rb` | identical |
| `python3 harness/fp_audit.py --gaps --sweep` | **0 FP / 9204 files / 8 corpora**; every per-corpus gap count unchanged, `call.wrong-arity` gaps 21 → 21 |
| `ruby harness/unbuildable_classes.rb --check` | OK: 12 classes, matches this environment's oracle |
| `python3 harness/docs_check.py` | PASS |

The sweep is **output-neutral**: the arity gate removes no diagnostic any of the
9204 files produced, and adds no coverage gap — the reference declines on exactly
the same methods, so parity holds in both directions.

Two incidental hazards fixed in passing, both found by running the gates rather
than reading them:

- `harness/snapshot.rb --check` reported permanent phantom drift on fixtures 48
  and 54 — the two snapshots whose reference messages carry non-ASCII (`—`,
  `’`). It read the committed file without an explicit encoding, so a
  byte-identical file compared UNEQUAL while the WRITE path rewrote it to the
  same bytes. A future reader would have read that as "the reference moved". The
  same trap `unbuildable_classes.rb` already documents for `rbs.rs`. CI runs
  `run_snapshot.rb`, not this, so it was never red.
- `harness/README.md` still documented `REFERENCE_RIGOR_DIR`'s default as
  `/Users/megurine/repo/ruby/rigor` — literally `UPSTREAM.md` hazard 3, written
  down as the default. The code has defaulted to the pinned submodule since
  `dfb5971`.

## What this leaves open

- Nothing on this item. Both halves of the ingestion asymmetry are closed:
  `BigMath` by `UNBUILDABLE_DEFINITIONS`, `Object#Nokogiri` by the vendored
  overlay. The general question the slice-2 note posed — "diff the two engines'
  loaded surfaces and decide per family" — has now been answered for the one
  family that had a live divergence.
- The environment dependence recorded in the `BigMath` note is untouched and
  still stands: `BigDecimal` in the table above is `[env]`-sourced on the oracle
  (the installed gem's `sig/`) and vendored-`stdlib` here. The two happen to
  declare the same arity, so the row matches — but that is a coincidence of this
  host's gem set, not a property of the pin.
