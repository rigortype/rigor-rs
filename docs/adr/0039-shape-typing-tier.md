# Port the reference's static shape-typing tier (Tuple/HashShape) to Rust, not Ruby folding

Status: accepted

The container-precision the reference gets from **static shape types** (Tuple /
HashShape) is ported to Rust as a native tier, NOT reproduced by executing Ruby in
the sidecar. This is the concrete resolution of the [Slice 1 array-fold blocker](../notes/20260706-slice1-array-fold-blocker.md):
the treemaps `call.possible-nil-receiver` gap is not a folding gap — it is a
**shape-typing** gap.

## Context — the finding that forces this

Building the possible-nil collection-slice source (ADR-0038 Slice 1) surfaced FPs
on `arr = Array.new(10){…}; sub = arr[0..5]; sub.size` (rigor-rs fired, the
reference was silent). Tracing the reference showed WHY, and it is not what the
"array constant-folding" framing assumed:

- The reference types array literals and `Array.new(n)` (small constant `n`) as a
  **`Tuple`** — a static, per-position shape — via `expression_typer.rb`
  (`tuple_of(*elements…)`) and `method_dispatcher.rb`'s `array_new_lift`, capped at
  `ARRAY_NEW_TUPLE_LIMIT = 16`. Oversize / non-constant sizes stay `Nominal[Array]`.
- Shape-preserving methods (`map`/`select`/`reject`/`flatten`/…) **propagate the
  Tuple** (`expression_typer.rb:2853…`), which is why `[1,2,3].map{…}[0..1]` is
  silent too.
- `Tuple#[]` with a static Range returns a **sub-Tuple / `Constant[nil]`**
  (`shape_dispatch.rb`) — NON-nil — so no possible-nil fires; only
  `Nominal[Array]#[](Range) : Array?` fires (e.g. `Array.new(300000)` — treemaps).

So the behavior is governed entirely by a **static shape tier** ("Slice 5 phase 2"
in the reference), which rigor-rs deferred at [ADR-0023](0023-dispatch-cascade.md)
("TODO: Tuple / HashShape"). Strings are already aligned because a string is a
value `Constant` in BOTH tools, and the possible-nil source declines `Constant`
receivers — no shape type is needed for strings.

## The decision

### 1. Port statically to Rust; do not fold arrays via the sidecar

The shape tier is Ruby-process-free, runtime-hot (array/hash literals are
everywhere), and a bounded, well-understood model — so it goes to Rust natively.
This instances a general boundary rule for the [Ruby sidecar](0036-ruby-sidecar-default-reversal.md):

> **Port to Rust the logic that is resolvable without a Ruby process AND is
> runtime-hot AND low-risk. Reserve the sidecar for what genuinely needs Ruby**
> (the long tail of value constant-folds, plugin target-library calls).

Sidecar-executing `Array.new(n){…}` was rejected: it is a DIFFERENT mechanism from
the reference's static shape model (so it diverges on non-constant / propagated
cases), it is full-fidelity-only (a `--no-ruby` run would lose array possible-nil
entirely, and literals are too common for that), and it pays a Ruby round-trip per
literal. A native shape tier converges with the reference by construction and
keeps the diagnostic in the **sound subset**.

### 2. The FP-safety invariant (binds every shape slice)

Re-enabling the possible-nil array-slice source is sound only while

> **{arrays rigor-rs types `Nominal[Array]` (non-shape)} ⊆ {arrays the reference
> types `Nominal[Array]`}.**

If rigor-rs leaves an array `Nominal` that the reference made a `Tuple`, the slice
source fires where the reference is silent — an FP (the `.map` case above). So a
partial shape port must NEVER let the possible-nil source fire on an array whose
provenance it has not proven the reference also keeps `Nominal`.

### 3. Slice 1 scope (static, gated by ADR-0038 §5)

1. Add `Type::Tuple` (a fixed-length vector of element `TypeId`s) to the lattice +
   interner.
2. Type array literals and `Array.new(n ≤ 16)` as `Tuple` (port
   `ARRAY_NEW_TUPLE_LIMIT = 16` faithfully; oversize / non-constant ⇒
   `Nominal[Array]`).
3. Shape dispatch: `Tuple#[]` (constant index → element type; static Range →
   sub-`Tuple`; out-of-range → `Constant[nil]`), `Tuple#size` → `Constant[len]`.
   Any unmodeled Tuple op falls back to `Nominal[Array]` (decline).
4. Re-enable the possible-nil array-slice source, but fire it ONLY on the
   provenance the invariant permits in Slice 1: a receiver bound directly to
   `Array.new(size > 16 or non-constant)`. `.map`/`select`/… propagation is a
   LATER slice; until it lands, those receivers stay declined (recall gap, safe).
5. Gate: `fp_audit.py` 0-FP across the survey (synthetic literals / small
   `Array.new` / `.map` slices all silent), harness 53/53, treemaps line 45
   matches the reference, matched non-regression. **Then MEASURE** the tier's EV
   before committing to further shape slices.

## Considered options

- **Ruby-sidecar array folding** — rejected (§1): diverges from the reference's
  static model, full-fidelity-only, per-literal cost.
- **Tactical `Array.new`-provenance rule only, no Tuple type** — closes treemaps
  cheaply but is ad-hoc and gives none of the shape tier's element-precision; the
  §3 slice subsumes it (the provenance fire IS Slice 1's possible-nil rule) while
  laying the real lattice groundwork.
- **Accept as a permanent sound-subset decline** — rejected: it is resolvable
  Ruby-free, and the goal is to minimize practical mismatches, not to bank them.

## Consequences

- Honest EV note: a `Tuple` shares `Array`'s method set, so this does NOT add
  `call.undefined-method` coverage over `Nominal[Array]`. Its gains are
  element-type precision (`[1,"a"][0].typo` witnesses against the element), the
  possible-nil FP-avoidance above, and shape-method result types. Slice 1 is
  therefore measurement-gated — if the EV is thin, the tier stays at Slice 1
  (treemaps closed) rather than expanding speculatively.
- `HashShape` and shape-preserving method propagation are later slices, each
  0-FP-gated and each widening the invariant-permitted possible-nil fire set
  monotonically.
