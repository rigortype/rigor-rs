# Completing the ADR-0022 flow substrate: FP-safe incremental narrowing

Status: accepted

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

### 3. Slice sequence

- **Slice 1 — scope foundation + nilability + possible-nil.** Thread a local
  `nilable` fact through `flow_eval`, INCLUDING block bodies (the real gaps are in
  blocks). Sources: `method_return_nilable` + the arg-aware collection slice
  (`arr[Range]`/`str[Range]` → nilable self-collection, which the multi-overload
  `method_return_nilable` collapses to None). Also type block-bearing `X.new(…){…}`
  as an `X` instance (shared `.new` typing). Fire possible-nil on a straight-line,
  unguarded, certainly-nilable local receiver; **decline** if any branch / guard /
  reassignment lies between source and use. Closes the unguarded block-scope
  cluster (treemaps class).
- **Slice 2+ — truthy/falsey narrowing, one construct at a time**: `if`/`unless`
  → `&&`/`||` → `.nil?` / early-return guard → `case` → loops. Each admits more
  guarded flows (growing recall); each gated to 0-FP + measured gap reduction.
- **Later — always-truthy onto the same substrate** (narrowing proves
  truthy/falsey), then ivar value-flow (the deque `@size == 1` class,
  ADR-58-adjacent — the deepest, deferred).

### 4. Gate

Every slice: the differential harness (53/53, 0 unregistered FP) AND
`harness/fp_audit.py` at **0 FP across the survey corpora**, plus a measured gap
reduction (`--gaps`). A slice that closes no gaps is not shipped (the recon
lesson). FP-safety is not argued — it is measured against the oracle.

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

## Revisiting

Supersede if a construct proves un-portable FP-safely under the backstop, or if
the ivar value-flow tier (Slice “later”) needs its own ADR (likely — it is
ADR-58-scale on the reference side).
