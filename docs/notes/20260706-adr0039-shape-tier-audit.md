# ADR-0039 / shape-tier Slice 1 audit — concerns before implementation (2026-07-06)

> **Status: absorbed.** ADR-0039 was revised the same day to make these concerns
> normative — the syntactic-provenance fire rule (+ tenv-side travel), the
> internal ordering (provenance fire before Tuple minting), the cross-rule
> always-truthy gate, committed negative fixtures, the probe-derived spec
> corrections (zero-arg `Array.new`, Range-form restriction, block-form fill,
> `Constant[nil]` expectation), and `class_name_of(Tuple) → "Array"`. ADR-0038
> gained a goal-refinement cross-reference; UPSTREAM.md's bump checklist gained
> the shape-threshold re-measure. This note remains the review record.

An audit of the grill-with-docs decisions (D1 parity softening → mechanism choice
A → ADR-0039 Slice 1 scope), grounded in six fresh oracle probes (P1–P6) run
against the pinned reference before writing this note. Ranked by severity.

**Verdict up front:** the mechanism choice itself is sound — the session
identified the reference's ACTUAL mechanism (static shape tier, not folding) by
reading `method_dispatcher.rb` / `shape_dispatch.rb` / `expression_typer.rb`, and
the three sidecar-rejection grounds hold. The concerns are about Slice 1's spec
having one genuine design hole (#1) and several gate/spec gaps.

## 1. (CRITICAL — design hole) "provenance" cannot be type-based; it must be syntactic + travel with the inherited env

ADR-0039 §3.4's "fire only on a receiver bound directly to `Array.new(>16 /
non-constant)`" left HOW to detect provenance unspecified. The naive
implementation — fire when the receiver's type env says `Nominal[Array]` —
produces FPs, because §3.3 and §3.4 CONTRADICT each other under a type-based
reading:

- Slice 1 has no Tuple propagation, so `[1,2,3].map{}` is **rigor-rs: Tuple →
  unmodeled op → `Nominal[Array]` fallback** but **reference: still a Tuple**.
  Type-based firing on that `Nominal[Array]` ⇒ FP on
  `[1,2,3].map{}[0..n].size` (reference silent).
- The resolution: **syntactic provenance** — mint the nilable fact only when the
  receiver's binding RHS is literally `Array.new(constant > 16 / non-constant /
  zero args)`. Then the `Nominal[Array]` fallback for unmodeled Tuple ops is safe
  (it serves undefined-method, where Tuple's method set = Array's) and never
  feeds the possible-nil source.
- **Travel:** in treemaps the `Array.new` binding is in an OUTER block and the
  slice in an inner block; the ADR-0038 substrate's `nenv` is fresh per block, so
  the provenance must travel on the **tenv side** (the layer inherited into
  blocks), invalidated by exactly tenv's widen rules (a parallel inherited map or
  a marker carried with the binding).

Corollary: with syntactic provenance, **closing treemaps does not require
`Type::Tuple` at all**. Slice 1's gap payload is the provenance rule; the Tuple
machinery is element-precision groundwork. Order the work internally as
(1) provenance fire — small, 0-FP-gated, closes the gap; (2) Tuple minting +
dispatch — so the lattice change never holds the gap hostage.

## 2. (measured) Tuple opens a cross-rule side channel — gate must include always-truthy differentials

Probes P1/P2: the reference fires `flow.always-truthy-condition` on
`if [1,2].size` AND `if [1,2].size > 0` (rigor-rs today: silent). `Tuple#size →
Constant[len]` feeds the flow-constant snapshots, so every Constant-minting shape
op (size, `[]`-const-index, …) is a new always-truthy exposure — a parity
opportunity, but an error-severity FP if rigor-rs's firing conditions diverge.
The Slice 1 gate must include **synthetic always-truthy differentials for each
Constant-minting shape op**, not just possible-nil checks.

## 3. (process lesson) survey 0-FP did NOT catch the Array-source FP — commit adversarial negative fixtures

The ADR-0038 Slice 1 Array-slice FP was invisible to `fp_audit.py` across the
whole survey (real code rarely has the pattern) and surfaced only via a synthetic
fixture attempt. "FP-safety is measured, not argued" therefore needs BOTH
measurements: corpus audit AND **committed harness negative fixtures** (literal /
small-`Array.new` / `.map` slices all silent; the shape-op always-truthy
negatives from #2). Session-local checks don't prevent regressions; fixtures do.

## 4. (probe-derived spec corrections)

- **Zero-arg `Array.new`** (P3): the reference FIRES (`array_new_lift` returns
  nil on empty `arg_types` ⇒ stays Nominal). Add "zero args" to the Slice 1 fire
  provenance (the ">16 or non-constant" wording missed it).
- **Out-of-range slice → `Constant[nil]`** (P5): the reference fires
  `call.undefined-method` (nil receiver) — but rigor-rs's nil-receiver
  undefined-method is a known coverage gap (fixture 08), so minting
  `Constant[nil]` yields silence = gap, not FP. Safe, but the expected payoff of
  that sub-feature is limited until nil-receiver witnessing exists.
- **Range forms**: Slice 1 must model ONLY non-negative literal, in-bounds
  ranges (inclusive/exclusive) → sub-Tuple; anything else (negative, beginless /
  endless, boundary `start == size`) ⇒ decline. Ruby's slice semantics are
  subtle; a wrong element type is an undefined-method mis-witness (FP).
- **Block-bearing `Array.new(n){}`**: lift inferred from measurement (the 16
  threshold was observed WITH blocks), but the read `array_new_lift` code doesn't
  mention blocks — read the reference's actual block path before implementing.
  Mint elements as Dynamic (do not infer the block return) — safe side.

## 5. (regression risk) `class_name_of(Tuple) → "Array"` is mandatory

Literal arrays witness `.frist` etc. TODAY via `Nominal[Array]` (in matched).
The moment literals type as Tuple, method-existence resolution must delegate to
Array (`class_name_of(Tuple) = "Array"`) or existing matched regresses. The
matched non-regression gate would catch it late; the spec should say it up front.

## 6. (doc consistency)

- ADR-0038's Context still claims "no divergence / 100% parity" as the goal; D1
  softened this to "minimize practical mismatches". Cross-reference ADR-0039's
  refinement from ADR-0038.
- `ARRAY_NEW_TUPLE_LIMIT` can silently change on an upstream bump — add a
  "re-measure shape thresholds" line to UPSTREAM.md's bump procedure.
- Pre-declare Slice 1's measurement expectation so results aren't misread:
  algorithms possible-nil 50 → 49, matched +1 (treemaps line 45 ONLY; lines
  46–48 remain declined by same-block locality).

## Probe log (all vs the pinned reference)

| # | shape | reference |
|---|-------|-----------|
| P1 | `x=[1,2]; if x.size` | fires always-truthy |
| P2 | `x=[1,2]; if x.size > 0` | fires always-truthy (folds the comparison) |
| P3 | `Array.new` (zero args) → slice → `.size` | fires possible-nil |
| P4 | `Array.new(3, 0)` (two-arg, small) → slice | silent (lifted) |
| P5 | `[1,2][5..6].size` | fires undefined-method (nil receiver) |
| P6 | `[1,"a"][0].frobnicate` | fires undefined-method (element type) |
