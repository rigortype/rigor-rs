# #629: plain index writes classify where the projection settles

PR #786 → merged 2026-11-21 (`81167086`). Closes #629; follow-ups filed as
#787 (the class-call `Claimless` edge — one port OVER) and #788
(`assign_class` does not unwrap a parenthesised assignment RHS).

## What diverged

`NON_MUTATING_ROWED_SELECTORS` suppressed the whole ownership judgment for
`[]=` (and the two `Encoding` writers) on the uncatalogued path, because
`Thread#[]=` and `ENV#[]=` answer the selector as a NON-mutation and the port
cannot tell a Thread-typed local from any other. The cost was every ordinary
index write: `Thing.new[1] = 2`, `h = {}; h[:k] = 1`, the singleton bare
`new` — all silent — plus the `unknown-ownership` taint on the untyped shapes.

## The oracle fact the fix rests on

The reference's claim path runs BEFORE the ownership judgment, and **a
catalogued ROW is the only answer that can preempt it**: across every measured
receiver shape the POSTURE tier never claimed — `Time.new[1] = 2` classifies
(`[mutate.local, nondet.time]`), `x = 5; x[1] = 2` taints, while
`Thread.new[1] = 2` answers `Thread#[]=`'s row (`[global.write]`, exhaustive).
So the port's existing row-only `resolve` (posture deliberately off, #106) is
not a decline here — it is upstream's own answer, and the question reduces to
WHICH projections the syntax can settle.

## Port shape

- `catalog_target` answers upstream's third arm (`record.receiver_class`) for
  the syntax-settled subset only: literals (`{}`/`[]`/`""`/lambda),
  `K.new` on a written class constant (an OBJECT constant's `new` cannot
  resolve — `ENV.new`/`Warning.new` stay claimless), bare/`self.` `new` where
  `self` is the unit's owner class, and a local whose every assignment
  allocates and names the one class (`union_descriptor` keeps a mixed pair
  claimless, so the port requires all-same-class).
- `ownership_facts` records per-assignment classes beside the owned set
  (`AssignClass`: `Claimless` / `Class(K)` / `Unsettled` — `dup`/`clone` are
  Unsettled because the projection is the inner receiver's).
- `universal_row_declined` keeps the blanket rule's safety edge on exactly
  the shapes a row can still claim through a type the port cannot see:
  parameters (a sig typing `t: Thread` makes the reference answer the row —
  measured with a project `sig/`), ivars/cvars (`@t = Thread.new` in one unit
  claims the row in every other — measured), and `dup`/`clone` (measured both
  direct and assigned). Everything else classifies or taints.
- The settled class also feeds the per-class mutator sets exactly as
  `record&.receiver_class` does (`{}.merge!`, `Array.new.push`,
  `b = []; b << 1` → `mutate.local`), and a settled receiver's record is not
  `Dynamic`, so the site keeps its EDGE instead of the port's blanket
  `dynamic-receiver` taint — `h = {}; h[:k] = 1` and the literal receivers are
  byte-identical, exhaustive. The port's selector-keyed edge resolves against
  every unit of the name where upstream's resolves against the one class, so
  every new exhaustive claim is upstream's own or narrower (probed in both
  directions; no port-exhaustive/reference-tainted row was found).

## Measured outcome

- The issue's five rows: three label-restored to byte-identity, `foo[1] = 2`
  and implicit-instance `new[1] = 2` carry `unknown-ownership`; the param row
  stays declined (the sig-typed OVER is measured, review round 1).
- `effects_diff.py` (default corpus + mastodon/app): master MATCH=5693
  UNDER=2315 OVER=0 → branch **MATCH=5812 UNDER=2196 OVER=0** (+92 on
  `07_mutators`, +27 on mastodon/app). Snapshot: OVER=0, `unresolved-only`
  608→633 — count drift on rows whose totals balanced the port's own taint
  noise coincidentally, never fatal (Normalisation 4).
- `fp_audit.py --gaps --sweep`: 0 FP (the diff touches `rigor-cli`'s effects
  collector only; the `check` surface never moved).
- Review round 1 Approved; its falsification attempt that FAILED to
  reproduce: a project override of `Thread.new` does not re-route the
  reference's claim — `class Thread; def self.new; []; end; end` +
  `Thread.new[1] = 2` is `[global.write]` on both engines.

## Declines worth keeping in mind

The ivar/param declines cost ordinary coverage (`@t[1] = 2` untyped → the
oracle answers `mutate.self`); a follow-up could classify ivars whose visible
assignments never name a catalogued class. `x = Time.now`-shaped call
assignments stay unsettled on purpose: `def make_thread` returning
`Thread.new` makes the reference claim the row through cross-unit return
typing the port cannot see.
