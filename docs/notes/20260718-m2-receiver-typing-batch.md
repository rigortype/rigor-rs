# M2-GO receiver-typing batch — slices 1–4 (2026-07-18)

The four mechanically-safe slices from the
[Phase 0 M2 characterization](20260718-phase0-m1-m2-findings.md), one branch.
Measured outcome: **gitlab-foss lib UM gaps 179 → 155 (−24), mastodon models
5 → 3, 0 FP everywhere**; fixture 67 pins all three witnessing mechanisms
byte-for-byte (10 diagnostics, set-identical to the reference).

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

## Where slice 4's payoff is gated (the two open DESIGN DECISIONS)

Slice 4 types the value (chained typing works) but most of its UM-gap payoff
(Date 5 / Time 2 / singleton-return chains inside the String-51 cluster) does
NOT witness yet: `Time`/`Date` are outside the 9-class core-Nominal table, so
the minted instance is a SOURCE-RANGE Nominal, and the UM rule's ADR-0033
provenance gate witnesses those only for `is_project_sig_class`. Widening that
gate re-opens the exact FP class it was built against (`Pathname.new.typo`) —
NOT done unilaterally. Options: (a) a distinct "RBS-return-minted" provenance
the rule witnesses, (b) widen to `knows_class` for non-`.new` mints, (c) leave
typing-only. Similarly, GO-slice 5 (namespaced `ERB::Util` singletons) touches
the ADR-0023 ConstantRead zero-FP gate. Both need an explicit design call.

## Evidence

- live + snapshot gates: 67 fixtures / **202 matched / 0 gaps / 0 FP**
  (fixture 67 added; its `unresolved-toplevel` differs only in EMIT ORDER —
  set-identical).
- fp_audit 0 FP: gitlab lib (155 UM gaps, −24) + app/models, mastodon
  models (−2) + lib, conference-app.
- workspace tests green (+4: index unanimous/divergent returns, infer
  Array/rand/singleton-return typing); clippy clean on touched crates.
- Message-wording deltas (locations match): `for Array` vs
  `for Array[Dynamic[top]]`, `for [1, 2]` vs `for Array[1 | 2]` — the
  ADR-0002 wording latitude, same as `literal-string`.
