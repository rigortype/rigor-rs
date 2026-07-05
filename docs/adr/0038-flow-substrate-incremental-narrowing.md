# Completing the ADR-0022 flow substrate: FP-safe incremental narrowing

Status: accepted (revised 2026-07-06 — block-scope semantics made normative +
gate hardened, absorbing the [slice-1 audit](../notes/20260706-adr0038-slice1-audit.md);
Slice 1 landed as an FP-safe SUBSTRATE, see "Slice 1 outcome" below)

[ADR-0022](0022-control-flow-scopes-and-facts.md) accepts the edge-aware scope /
fact-bucket model as a **faithful port** of the reference's control-flow analysis
("the normative semantics are ported faithfully; only the Rust representation is
ours"). Today only fragments exist — straight-line constant snapshots for
`flow.always-truthy-condition` and a span-scan under-approximation for
`call.possible-nil-receiver`. The two biggest coverage-gap clusters vs the
reference (`possible-nil` ~118, `always-truthy` ~117; see the
[2026-07-06 recon](../notes/20260706-nil-flow-substrate-recon.md)) are gated on
completing this substrate. This ADR records HOW to complete it without ever
breaching the zero-false-positive bar ([ADR-0002](0002-diagnostic-set-parity.md)).

## Context — why the goal forces the full model

The end goal is **no divergence from the reference** (100% parity), not merely
zero-FP-with-gaps. That disqualifies extending the structural under-approximation
(decline-on-any-guard): it leaves permanent gaps by construction AND its span-scan
code does not transfer to the edge-aware model — a dead end / throwaway. The
recon confirmed this empirically: a bounded structural source-resolution slice was
0-FP but closed ZERO real gaps (every real occurrence is in a block/top-level
scope the span-scan defers on). So the substrate must be the real edge-aware
model, built on the threaded `flow_eval` (not the span-scan).

The recon also names WHERE the FP risk concentrates: **possible-nil across block /
top-level scopes**, where capture and re-entrancy make narrowing delicate. A
pre-implementation audit ([2026-07-06](../notes/20260706-adr0038-slice1-audit.md))
confirmed the original slice plan was thin exactly there; §3 below makes those
semantics normative rather than leaving them to implementation judgment.

## The decision

### 1. Faithful edge-aware port on the threaded `flow_eval`

Complete ADR-0022's model incrementally on the existing threaded flow-eval
(`Typer::flow_eval_*`), unifying both rules onto it — not the span-scan
(`enclosing_def` + guard-scan), which is retired as each rule migrates.

### 2. The "unmodeled construct ⇒ decline" backstop (the FP-safety keystone)

Every value carries whether its flow is **fully modeled**. A diagnostic fires
only when the value is `certainly nilable` (for possible-nil) / `certainly
constant-truthy` (for always-truthy) AND every construct its flow passed through
is in the currently-modeled set. If the flow touches ANY not-yet-modeled construct
(an unmodeled control-flow form, guard, reassignment, escape), the fact becomes
`unknown` and the rule **declines** (stays silent).

Consequences: **every partial state is a sound subset (0 FP)** — an unmodeled
construct never yields a wrong narrowing, so never a false positive; and growing
the modeled-construct set **monotonically closes gaps** toward 100% parity. This
generalizes the established completeness discipline (`class_has_method` witnesses
absence only when the ancestor chain is fully loaded; the current possible-nil
declines on any guard).

### 3. Block-scope semantics (normative — the FP-delicate core)

Descending `flow_eval` into block bodies is where the real gaps live AND where
the FP risk concentrates. A block is not straight-line code that happens to be
indented: it may run **zero times** (`[].each`), **many times** (loop-shaped
re-entrancy — a read lexically *before* the block's own capture-write observes
the post-write value on iteration 2), or **later / elsewhere** (a stored
callback, where lexical position carries no fact at all). These rules are
therefore part of the decision, not implementation detail:

- **Entry-widen.** Before descending into a block body, widen every
  capture-write whose span falls inside that block (the same
  `widen_flow_writes` discipline, applied to the block span *on entry* — exit
  widening alone is unsound under re-entrancy).
- **Fact locality (until narrowing justifies more).** A fact may only support a
  fire when its source and its use sit in the **same block body** (or the same
  non-block scope). A fact crossing a block boundary in either direction is
  `unknown` ⇒ decline. Later slices may relax this per construct, each
  relaxation individually 0-FP-gated.
- **Shadow-clearing.** On descent, clear every name bound by the block's
  parameters — including destructured, numbered (`_1`…) and `it` params. A
  same-named block param is a different variable; a leaked outer fact is a
  guaranteed FP class. `def`/`class`/`module` bodies keep their existing
  fresh-state treatment for every fact kind threaded through `flow_eval`.
- **Descend XOR widen.** A Call-with-block that is descended must NOT also pass
  through the `other`-arm span-widen (else every descended fact is immediately
  destroyed — a silent no-op, which the "no gap closed ⇒ not shipped" gate
  would catch late and expensively).
- **Per-rule flow state.** Each rule threads its own fact map and its own
  record/suppress flags. The existing `in_loop_or_block` flag (always-truthy
  suppression) is NOT overloaded to govern nilability — one flag serving two
  rules with opposite in-block behavior is how a suppression bug ships.

### 4. Slice sequence

- **Slice 1 — scope foundation + nilability + possible-nil.** Thread a local
  `nilable` fact through `flow_eval`, INCLUDING block bodies under the §3 rules.
  Sources: `method_return_nilable` + the arg-aware collection slice
  (`arr[Range]`/`str[Range]` → nilable self-collection, which the multi-overload
  `method_return_nilable` collapses to None) — applied ONLY when the receiver is
  a confirmed core `Array`/`String` (user-defined `[]` ⇒ decline), with the
  two-arg `arr[i, len]`, endless-range and `str[Regexp]` forms explicitly out of
  scope (⇒ decline). Also type block-bearing `X.new(…){…}` as an `X` instance
  via a shared `type_dot_new` — which MUST carry the 2026-06-26 leniency
  invariant with it (**non-core `.new` instances are typed but never
  witnessed**; the witnessing gate moves with the extraction, or the
  `Struct.new(...).new` FP class returns). Fire possible-nil on a straight-line,
  unguarded, certainly-nilable local receiver; **decline** if any branch /
  guard / reassignment lies between source and use. Rewiring retires the
  `enclosing_def` gate, so top-level receivers become fireable — confirm E2E
  that the reference fires at top level BEFORE enabling it there. Closes the
  unguarded block-scope cluster (treemaps class).
- **Slice 2+ — truthy/falsey narrowing, one construct at a time**: `if`/`unless`
  → `&&`/`||` → `.nil?` / early-return guard → `case` → loops. Each admits more
  guarded flows (growing recall); each gated to 0-FP + measured gap reduction.
- **Later — always-truthy onto the same substrate** (narrowing proves
  truthy/falsey), then ivar value-flow (the deque `@size == 1` class,
  ADR-58-adjacent — the deepest, deferred).

**Recorded debt:** the hand-recognized `arr[Range]` form is a bounded stand-in
for the reference's real overload selection (which picks the `Range` overload by
argument shape). The "no divergence" goal ultimately wants that selection logic
ported; the special case is acceptable only while its guards above hold.

### 5. Gate

Every slice, all four:
1. the differential harness (53/53, 0 unregistered FP);
2. `harness/fp_audit.py` at **0 FP across the survey corpora**;
3. a measured gap reduction (`--gaps`) — a slice that closes no gaps is not
   shipped (the recon lesson);
4. **matched non-regression** — the existing possible-nil fixtures AND the
   corpus matched count do not drop. Migrating a rule off the span-scan replaces
   its firing path; a stricter new path silently dropping a currently-firing
   def-scope case is a regression the gap metric alone does not see.

FP-safety is not argued — it is measured against the oracle.

## Considered options

- **Extend the structural under-approximation (decline-on-guard, broadened)** —
  rejected: permanent gaps (can't reach no-divergence), and the span-scan doesn't
  transfer to the edge-aware model (throwaway). Empirically 0 gaps closed.
- **Big-bang port the whole edge-aware model** — rejected: partial narrowing can
  be MORE wrong than none (a missed guard ⇒ FP), and an all-at-once port can't be
  gated incrementally. The decline-backstop + per-construct slicing is what makes
  the port FP-safe at every step.
- **Full 5-edge/6-bucket model from Slice 1** — deferred within slices: Slice 1
  needs only the normal edge + local-nilability; the truthy/falsey edges and the
  other buckets arrive as narrowing constructs are added.
- **Treat block descent as plain straight-line descent (the pre-audit plan)** —
  rejected: it ignores 0-/many-/deferred-execution and param shadowing, the
  exact FP classes the recon flagged. §3 exists because "straight-line within
  the block" is only sound under entry-widen + locality + shadow-clearing.

## Slice 1 outcome (2026-07-06) — landed as an FP-safe substrate; §5-gap-gate re-interpreted

Slice 1 was implemented and is **FP-safe and harness-clean (53/53, 0 FP)** but
closes **zero measured survey gaps**. Recorded here because it revises the plan.

**Landed (kept, merged):** the shared `type_dot_new` (block-bearing `X.new{}` types
as an `X` instance) and `Typer::nilable_receiver_snapshots` — the threaded
flow-eval (type env inherited into blocks, nilability facts fresh per block,
straight-line + block descent, decline-all on any unmodeled construct) that
REPLACES the `enclosing_def` span-scan. `check_nil_receiver` now fires from the
snapshot map. The def-scope fixtures (27 fire / 28 negatives) still pass.

**The gap that didn't close — treemaps is fold-gated, not just scope-gated.** The
one Slice-1-reachable survey gap (treemaps `select_subset = random_array[0..n];
select_subset.size`) needs `Array#[](Range) -> Array?`. Measuring against the
oracle showed the reference gates that entirely on its **constant-folding**: it
folds `Array.new(n ≤ 16)` and every array literal to a concrete array (hiding the
`Array?`), firing only on `Array.new(n ≥ 17)`. rigor-rs types arrays as
`Nominal[Array]` (it does not fold them), so an Array-slice source **over-fires**
on the folded cases (confirmed FPs). The fold threshold (16) is a
reference-implementation constant. So the Array-slice source is dropped;
**String** slices are FP-safe (rigor-rs's `Constant`/`Nominal` split for String
aligns with the reference's fold/nominal split, verified 6/6) but no
String-slice-in-scope gap exists in the survey. Full analysis:
[the array-fold blocker note](../notes/20260706-slice1-array-fold-blocker.md).

This refines the [recon](../notes/20260706-nil-flow-substrate-recon.md)'s "block
scope is the paying piece": block scope is necessary but not sufficient — treemaps
also needs array-folding, and the rest of the survey possible-nil cluster is
Tier B/C (project nilable-return inference + ivar typing + loop narrowing), which
Slice 1's source model does not reach.

**Gate re-interpretation (maintainer decision, 2026-07-06).** §5's "a slice that
closes no gaps is not shipped" is relaxed ONCE, deliberately, for this substrate
landing: retiring the span-scan and threading block-aware flow-eval is the FP-safe
foundation every later slice builds on, and it is worth landing even though it
closes 0 gaps today. Future slices remain gap-gated by §5 as written. The next
paying possible-nil work is array constant-folding (unblocks treemaps) OR Tier B/C
(the bulk of the cluster) — not more of Slice 1's source model.

## Revisiting

Supersede if a construct proves un-portable FP-safely under the backstop, or if
the ivar value-flow tier (Slice “later”) needs its own ADR (likely — it is
ADR-58-scale on the reference side). Note also that the monotonic-gap-closure
claim holds against the **pinned** oracle ([UPSTREAM.md](../../UPSTREAM.md)); an
upstream bump can move the flow semantics themselves, so re-measure the gap
landscape after every pin bump before trusting the trend line.
