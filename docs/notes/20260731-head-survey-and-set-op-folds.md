# Upstream HEAD survey (`v0.3.1` → `ece06a0d`) + Tuple set-operation folds (2026-07-31)

No tag exists past `v0.3.1`, so the pin **stays** at `v0.3.1` (the tag
convention in `UPSTREAM.md`). This surveys the 49 commits on upstream `master`
to find what the port actually owes, and lands the one item that was real.

Headline: the 49 commits move **one diagnostic** across 9204 corpus files, and
it is one rigor-rs never emitted. The only portable item is the set-operation
folds (upstream #121), now implemented — 0 new firings measured.

## Measured delta (reference self-diff, both sides on rbs 4.1.0)

| corpus | files | v0.3.1 | HEAD |
|---|---|---|---|
| mastodon `app` | 1236 | 458 | 458 |
| gitlab-foss `lib` | 4676 | 1373 | 1373 |
| survey `mail` | 874 | 7198 | **7197** (−1) |
| survey `concurrent-ruby` / `dependabot-core` / `Ruby` / `net-ssh` / `haml/lib` | 2418 | = | = |

The single drop is `call.undefined-method` at rdoc `pre_process.rb:161` —
upstream #239 (a class defining the same name on both the instance and the class
side erased one of them from the in-source method table). **rigor-rs is silent
there already**, so the fix costs the port nothing.

## What the changelog offers, and why most of it is not port work

- **`plugin.rbs-inline.source-rbs-annotation-not-honoured`** (#229) — a genuinely
  new rule, but `info` severity (outside the parity set, which is
  error/warning) *and* inline-RBS, which the port defers
  ([ADR-0035](../adr/0035-inline-rbs-deferred.md)). Not port work.
- **`parameter_inference:` × `--incremental`** (#204) and the ATM
  inferred-receiver guard (#205) — the gate is opt-in and off in every shipped
  profile; upstream **declined** to flip it. Inert.
- **Editor mode whole-project scope** (#146), `--verify-incremental` refusing a
  buffer, the RBS-scan allocation work (#207) — CLI/perf, not diagnostics.
- **sig-gen `Data.define` / `Struct.new`** (#227) — the port already generates
  Data/Struct shells (sig-gen arc, `33f9436`); worth a targeted comparison
  later, but not a parity item.
- **Set operations on known-value arrays** (#121) — the one real item. Ported
  below.

## Probe: #237 (undeclared interface / alias in a project `sig/`)

A project whose `sig/` references an undeclared **interface** (`_Writable`) or
**type alias** (`serialized_node`) lost EVERY synthesized stub upstream, so its
own signatures contributed nothing. Built the fixture and ran all three:

| | result on `r.emit_typo("hi")` |
|---|---|
| reference `v0.3.1` (the pin) | **No diagnostics** — the sig batch was discarded |
| reference HEAD | `undefined method 'emit_typo' for Report` |
| **rigor-rs** | `undefined method 'emit_typo' for Report` |

The port already behaves like HEAD. Note what this implies: against the CURRENT
pin, rigor-rs is *ahead* on this shape — and the corpus sweep cannot see it,
because `fp_audit.py` runs both sides from a clean cwd (core+stdlib only, no
project config). **Any project-`sig/` behaviour is invisible to the standing
sweep**; only the harness fixtures (69/70) and hand-built projects reach it.

## Landed: Tuple set-operation folds (upstream #121 / `a2867efd`)

An array of statically known values kept its precision through concatenation and
slicing, then lost it at the first set operation. Now folded on the value-pinned
`Tuple` carrier: `&` / `intersection`, `|` / `union`, `-` / `difference`,
`intersect?`, plus `at`, the no-block `one?`, and `deconstruct`.

Behaviour is checked against real Ruby, not intuition:

| expression | folds to | real Ruby |
|---|---|---|
| `[1, 2] & [2]` | `[2]` | `[2]` |
| `%w[a b] \| %w[b c]` | `["a", "b", "c"]` | same |
| `[1, 1, 2] - [2]` | `[1, 1]` (no dedup) | same |
| **`[1] & [1.0]`** | **`[]`** | **`[]`** |
| `[nil, false, 3].one?` | `true` | `true` |

The `[1] & [1.0]` row is the one that matters: `Array#&` decides membership with
`eql?`, not `==`, so an equality-based reimplementation folds the WRONG answer
for exactly the mixed-numeric case. rigor-rs gets it for free — `Scalar`'s
equality never crosses variants and compares floats by raw bits — with one
exception handled explicitly: `Float::NAN.eql?(Float::NAN)` is **false** in Ruby
while identical bits compare equal here, so a NaN element declines the fold.

Deliberate declines, all matching upstream:

- **`at` is not an alias of `[]`.** `Array#at` raises `ArgumentError` on the
  `(start, length)` pair its sibling accepts, and a fold must never invent a
  value for a call that raises. An out-of-range index also declines rather than
  folding to `nil`: Ruby does return nil, but proving nil on a receiver the RBS
  tier calls optional would newly SURFACE diagnostics — a different decision
  from removing a `Dynamic`.
- An argument that is not itself a pinned `Tuple` declines (`Array#&` also
  accepts anything answering `to_ary`, which a shape cannot prove), as does any
  unpinned element on either side.
- Arity is capped at 8 arguments and the result at 64 elements.

### Parity position

The pinned oracle (`v0.3.1`) does NOT have these folds, so the port is
deliberately *ahead* of the pin — permitted by
[ADR-0011](../adr/0011-reference-oracle-exceptions.md) ("rigor-rs MAY implement
the corrected behaviour ahead of the upstream fix, but only with the registry
entry in place"), and here the upstream change is already merged rather than
merely reported.

No registry entry was needed, because nothing fires: **0 FP candidates across
9204 files** (mastodon `app`, gitlab-foss `lib`, survey `mail` / `Ruby` /
`dependabot-core` / `concurrent-ruby` / `net-ssh` / `haml lib`) with every gap
count unchanged, and the harness stays 76 fixtures / 0 FP / 3 gaps. Upstream
measured the same "zero new firings" on its own eight-project gate.

**No harness fixture yet, by design.** A fixture exercising these folds would
make rigor-rs emit what the `v0.3.1` oracle does not — an unregistered extra, a
red gate. The fixture belongs to the pin bump that passes `a2867efd`; the
behaviour is pinned by unit tests until then.

The surfaces that CAN newly fire are `intersect?` and `one?`, which fold to a
pinned bool and can therefore prove a condition constant
(`if %w[a].intersect?(%w[b])` → always falsey). That is the shape to watch when
the pin moves.
