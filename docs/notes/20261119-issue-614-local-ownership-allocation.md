# #614: `LocalOwnership` reads receiver + singleton before counting `.new`

PR #624 → merged 2026-11-19 (`4bbb544`). Closes #614; follow-up filed as #629
(`[]=` suppression also drops the `unknown-ownership` taint on `recv[k] = v`).

## What diverged

The port's `LocalOwnership` treated every `.new` call as an allocation witness
(`ALLOCATING_SELECTORS = [new, dup, clone]`). Upstream's
`allocation?(node, singleton:)` (`local_ownership.rb:52-83`, pin v0.4.8) routes
`new` through `constructor?`, which reads the receiver AND `context.singleton?`
(`scanner.rb:254` threads it through): a `new` allocates only on a constant
path, an `x.class` receiver, or `self` — explicit or implicit — **in a
singleton body**. `assoc.items.new` and bare `new` on an instance body only
share the name, so `o` stays unowned and `o.x = 1` taints `unknown-ownership`
where the port proved `mutate.local` — two OVER rows in the proven lane, found
by the Fable design companion during the PR #613 review.

Second arm in the same remediation: upstream's `else` arm of
`MutationClassifier#ownership` (`mutation_classifier.rb:88`) — a receiver that
is itself an allocation (`{}.x = 1`, `Thing.new.x = 1`, `expr.dup.x = 1`) earns
`mutate.local`; the port tainted `unknown-ownership` there (an UNDER).

## Port shape

- `owned_locals`/`OwnershipScan`/`allocation` carry a `singleton` flag,
  threaded from `ctx.singleton()` at the one call site (`collect.rs:657`) — the
  same bit `MutationClassifier` already took.
- `constructor`/`class_call` ported as written: `name == :new`, then receiver
  `nil`/`SelfNode → singleton`, `ConstantRead`/`ConstantPath → true`, `CallNode
  → class_call?` (`:class`, no arguments, no block), else false.
  `COPYING_SELECTORS` keeps `dup`/`clone` on any receiver.
- `label_for` gained the else arm AFTER the local arm — arm order matches
  upstream's `ownership` exactly (a local the classifier cannot own never
  falls through to `allocation`).

## Measured outcome

`effects --full` vs the oracle, pin v0.4.8:

| row | oracle | port before | port now |
|---|---|---|---|
| `o = assoc.items.new; o.x = 1` | `[]` + UO | `[mutate.local]` OVER | `[]` + UO |
| `o = new` (instance) | `[]` + UO | `[mutate.local]` OVER | `[]` + UO |
| `o = new` (`def self.s`) | `[mutate.local]` | same | `[mutate.local]` |
| `o = Thing.new` / `other.class.new` | `[mutate.local]` | same | `[mutate.local]` |
| `{}.x = 1`, `Thing.new.x = 1`, `expr.dup.x = 1` | `[mutate.local]` | `[]` UNDER | `[mutate.local]` |

Gates: `gate.sh` pass; CI green; `effects_diff.py` PASS 0 OVER / 0
DECLARED-MISMATCH (mastodon corpus absent locally). `fp_audit --sweep`
unmeasurable — 0/8 corpora on this machine; `check` does not read effects
summaries.

Review: Opus 5.5 **Approved** (54-unit re-probe, 52/54 — both diffs are the
documented `[]=` under-report → #629; 15 master overclaims removed, 0 new).
Fable 5.2 design companion: structurally faithful arm-for-arm; its
cite/test findings landed in `c0a2c34` (fix-confirmation **Approved**). Grok
4.6 advisory **Approved** (82 counterexample units, 0 OVER / 0 UNDER / 0 UO
mismatch; parenthesised/begin/if-wrapped receivers are a shared both-engine
UNDER, not a port miss). PR #623 merged meanwhile → rebased, PR diff
byte-identical, re-verified.

Residuals (pre-existing, outside the arms): `{}.compare_by_identity` needs a
receiver class the typerless collector cannot name; `…?` reason-count diffs
are the documented no-typer over-taints; two pre-existing stale cites
(`ownership.rs:94`, `collect.rs:1116`) noted for the cite-fix family (#616).
