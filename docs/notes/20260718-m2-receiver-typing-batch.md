# M2-GO receiver-typing batch — slices 1–4 (2026-07-18)

The four mechanically-safe slices from the
[Phase 0 M2 characterization](20260718-phase0-m1-m2-findings.md), one branch.
Measured outcome (with slice 4b below): **gitlab-foss lib UM gaps 179 → 148
(−31), mastodon models 5 → 3, 0 FP everywhere**; fixture 67 pins the
witnessing mechanisms byte-for-byte (set-identical to the reference).

| # | mechanism | where |
|---|---|---|
| 1 | `CONST = <literal>.freeze` unwrap (identity, any depth) | `const_lit_of`, C5 harvest |
| 2 | `Kernel#Array`: Tuple identity / nil collapse / scalar wrap / nominal Array | `type_implicit_self_call` |
| 3 | `rand`: `() -> Float`; ANY non-Range 1-arg `-> Integer` (the reference's measured overload pick — even a Float-pinned arg); Range/multi-arg decline | `type_implicit_self_call` |
| 4 | singleton-method RBS returns (`Time.now -> Time`, `Date.today -> Date`), late-bound `instance` | `method_signature` tri-state + `CoreData::singleton_method_return` + `type_call` |

Slice-4 FP keystone: the stored singleton return is collapsed under
**all-overloads-agree** (class + nil bit + instance-ness), so
`Regexp.last_match` (`MatchData?` vs `String?`) declines by construction —
unit-pinned. RBS `-> instance` resolves late-bound to the QUERIED class
(`DateTime.today` → DateTime), tracked as a flag so every instance-method
consumer and the sig-gen surface (`singleton_return_lookup`, byte-parity
frozen) see exactly the old values.

## Slice 4b — declaration-driven witnessing (the resolved design decision)

Direction set by the user: reproduce the REPRODUCIBLE side of the reference's
inference — its RBS-declaration-driven logic — and do not chase its
runtime-reflection tier (machine-dependent; the known FP source).

Probing the reference's actual `.new` mechanism (`meta_new`,
`method_dispatcher.rb`) dissolved the ADR-0033 "stdlib `.new` leniency" into
two exact declaration-driven rules:

1. **Default**: EVERY `Singleton[C].new` falls to `nominal_of(C)` — a typed,
   witnessable instance (`Time.new`, `StringIO.new`, `IPAddr.new("…")` all
   fire there — probed live).
2. **Curated constant-constructor lifts** produce pinned VALUE carriers the
   UM stays silent on: `CONSTANT_CONSTRUCTORS = {Pathname}` (exactly 1 arg,
   pinned String; `:sym` RAISES in the lift and falls back to Nominal),
   `date_new_lift` (`Date`/`DateTime`, 1..=8 args all pinned Int/Rational/Str,
   validated by CONSTRUCTION), `set_new_lift` (`Set.new` / pinned-Tuple arg) —
   plus Array/Hash/Range/Regexp lifts rigor-rs already models or safely
   under-emits. rigor-rs mirrors the lifts as mint-DECLINES (Dynamic — the
   observable equivalent; fixture 38's `Pathname.new("x").nope` stays silent).

Ported: the UM witness gate for source-range Nominals widened from
`is_project_sig_class` to `knows_toplevel_class ∪ project-sig` (the defect-2
short-key guard is LOAD-BEARING: `knows_class`-wide witnessing FP'd on
gitlab's `Clusters::Instance` model whose bare name collides with an RBS
short key — caught by fp_audit, pinned in the gate comment). Singleton
ALIASES resolve through their target (`alias self.pwd self.getwd` →
`Dir.pwd -> String`).

Documented residual divergences (~0 corpus sites, fp_audit-clean):
UNDER-emits — Range-constant instances (no core-table id, the 9-class Nominal
ceiling) and invalid pinned dates (`Date.new(2020, 99)` raises in the
reference's lift and falls to Nominal there). One synthetic-edge OVER-fire
risk — `Pathname.new(<expr the reference folds but rigor-rs doesn't>)`
(`"x".to_s`): the reference lifts, rigor-rs mints; no real-corpus occurrence
(gitlab/mastodon/conference all 0 FP).

GO-slice 5 (namespaced `ERB::Util` singletons, ~5 gitlab sites) is BLOCKED on
a real design decision: `A::B` receivers already lower to a full-path
`ConstantRead` ("ERB::Util"), but the index registers nested RBS decls by
SHORT key — and `module Util` exists in BOTH erb.rbs and cgi.rbs, so the two
merge into one ambiguous "Util" entry. Sound resolution needs QUALIFIED-key
registration in the index (the defect-2 root fix) — an ADR-scale key-space
change touching reopen/merge semantics across every knows_class consumer,
not a slice. Parked with this note as the evidence.

## Evidence

- live + snapshot gates: 67 fixtures / **205 matched / 0 gaps / 0 FP**
  (fixture 67 added + extended; its `unresolved-toplevel` differs only in
  EMIT ORDER — set-identical).
- fp_audit 0 FP: gitlab lib (148 UM gaps, −31) + app/models, mastodon
  models (−2) + lib, conference-app.
- workspace tests green (+4: index unanimous/divergent returns, infer
  Array/rand/singleton-return typing); clippy clean on touched crates.
- Message-wording deltas (locations match): `for Array` vs
  `for Array[Dynamic[top]]`, `for [1, 2]` vs `for Array[1 | 2]` — the
  ADR-0002 wording latitude, same as `literal-string`.
