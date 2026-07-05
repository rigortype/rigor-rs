# Port the reference's static shape-typing tier (Tuple/HashShape) to Rust, not Ruby folding

Status: accepted (revised 2026-07-06 — syntactic-provenance fire rule, internal
ordering, cross-rule gate and probe-derived spec corrections made normative,
absorbing the [shape-tier audit](../notes/20260706-adr0039-shape-tier-audit.md))

The container-precision the reference gets from **static shape types** (Tuple /
HashShape) is ported to Rust as a native tier, NOT reproduced by executing Ruby in
the sidecar. This is the concrete resolution of the [Slice 1 array-fold blocker](../notes/20260706-slice1-array-fold-blocker.md):
the treemaps `call.possible-nil-receiver` gap is not a folding gap — it is a
**shape-typing** gap. It also refines [ADR-0038](0038-flow-substrate-incremental-narrowing.md)'s
parity goal: full 100% divergence-freedom is negotiable; **practical mismatches
are minimized**, and what is resolvable Ruby-free gets resolved Ruby-free.

## Context — the finding that forces this

Building the possible-nil collection-slice source (ADR-0038 Slice 1) surfaced FPs
on `arr = Array.new(10){…}; sub = arr[0..5]; sub.size` (rigor-rs fired, the
reference was silent). Tracing the reference showed WHY, and it is not what the
"array constant-folding" framing assumed:

- The reference types array literals and `Array.new(n)` (small constant `n`) as a
  **`Tuple`** — a static, per-position shape — via `expression_typer.rb`
  (`tuple_of(*elements…)`) and `method_dispatcher.rb`'s `array_new_lift`, capped at
  `ARRAY_NEW_TUPLE_LIMIT = 16`. Oversize / non-constant sizes stay `Nominal[Array]`
  — and so does **zero-arg `Array.new`** (`array_new_lift` declines empty
  `arg_types`; probe-confirmed: the reference fires possible-nil on its slice).
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

### 2. FP-safety: the set invariant AND the syntactic-provenance fire rule

Re-enabling the possible-nil array-slice source is sound only while

> **{arrays rigor-rs types `Nominal[Array]` (non-shape)} ⊆ {arrays the reference
> types `Nominal[Array]`}.**

A partial shape port CANNOT satisfy this invariant type-wise: until Tuple
propagation is complete, an unmodeled Tuple op (`[1,2,3].map{}`) falls back to
`Nominal[Array]` in rigor-rs while the reference keeps a Tuple — so **firing the
slice source off the type env would FP** on `[1,2,3].map{}[0..n].size`. The two
halves are reconciled by making the fire rule **syntactic, not type-based**:

- **Mint the nilable fact ONLY when the receiver's binding RHS is literally
  `Array.new(constant > 16 | non-constant | zero args)`** — the provenances
  probe-confirmed to stay `Nominal[Array]` in the reference.
- **The provenance travels on the tenv side** (the layer the ADR-0038 substrate
  inherits into block bodies — treemaps binds `Array.new` in an OUTER block and
  slices in an inner one), invalidated by exactly tenv's widen rules. The
  per-block-fresh `nenv` cannot carry it.
- The `Nominal[Array]` fallback for unmodeled Tuple ops is then safe: it serves
  undefined-method (a Tuple's method set equals Array's) and never feeds the
  possible-nil source. Type-based firing arrives only when propagation coverage
  makes the set invariant hold type-wise (a later, measured slice).

### 3. Slice 1 scope (static, internally ordered, gated)

Corollary of §2: **closing treemaps does not require `Type::Tuple` at all** — the
gap payload is the provenance rule; Tuple is element-precision groundwork. Slice 1
is therefore ordered internally so the lattice change never holds the gap hostage:

**Slice 1a — the provenance fire (closes treemaps):**
1. Thread the `Array.new`-provenance marker on the tenv side of the ADR-0038
   substrate (§2); re-enable the possible-nil array-slice source firing ONLY on
   it. `.map`/… receivers stay declined (recall gap, safe) until propagation.

**Slice 1b — Tuple groundwork (element precision):**
2. Add `Type::Tuple` (a fixed-length vector of element `TypeId`s) to the lattice +
   interner. **`class_name_of(Tuple) = "Array"`** — method-existence resolution
   delegates to Array, or every literal-array witness in today's matched set
   regresses the moment literals type as Tuple.
3. Type array literals and `Array.new(n ≤ 16, [fill])` as `Tuple` (port
   `ARRAY_NEW_TUPLE_LIMIT = 16` faithfully; oversize / non-constant / zero-arg ⇒
   `Nominal[Array]`). For the block-bearing form `Array.new(n){…}`, READ the
   reference's actual block path first (the audited `array_new_lift` excerpt does
   not mention blocks, though the 16-threshold was measured WITH blocks); mint
   elements as Dynamic rather than inferring the block return — the safe side.
4. Shape dispatch: `Tuple#[]` with a constant index → element type; with a static
   Range → sub-`Tuple` for **non-negative literal in-bounds ranges ONLY** —
   negative / beginless / endless / boundary (`start == size`) forms ⇒ decline
   (Ruby's slice semantics are subtle; a wrong element type is an
   undefined-method mis-witness). Statically out-of-range → `Constant[nil]`
   (parity note: the reference then fires nil-receiver undefined-method, which
   rigor-rs does not yet witness — fixture 08 gap — so this sub-feature pays
   little until that lands). `Tuple#size` → `Constant[len]`. Any unmodeled Tuple
   op falls back to `Nominal[Array]`.

**Gate (both sub-slices):**
- `fp_audit.py` 0-FP across the survey, harness green, matched non-regression —
  AND, because the ADR-0038 Slice 1 FP was **invisible to the survey audit**
  (real code rarely has the pattern; only a synthetic fixture caught it),
  **committed harness negative fixtures**, not session-local checks: literal /
  small-`Array.new` / two-arg / `.map`-result slices all silent; the positive
  treemaps shape firing.
- **Cross-rule differentials for every Constant-minting shape op** (probe-
  confirmed: the reference fires `flow.always-truthy-condition` on
  `if [1,2].size` and `if [1,2].size > 0`): `Tuple#size`/`#[]` feed the
  flow-constant snapshots, so Slice 1b must gate always-truthy behavior against
  the oracle on these shapes — an error-severity FP channel if conditions
  diverge, a parity gain if they match.
- **Pre-declared expectation** (so the measurement is not misread): algorithms
  possible-nil 50 → 49 and matched +1 — treemaps line 45 ONLY; lines 46–48
  remain declined by the substrate's same-block locality.
- **Then MEASURE** the tier's EV before committing to further shape slices.

## Considered options

- **Ruby-sidecar array folding** — rejected (§1): diverges from the reference's
  static model, full-fidelity-only, per-literal cost.
- **Tactical `Array.new`-provenance rule only, no Tuple type** — this IS Slice 1a,
  and it closes treemaps alone; the difference from "tactical only" is that the
  Tuple groundwork (1b) is planned, measurement-gated, and ordered second rather
  than abandoned.
- **Type-based provenance (fire on `Nominal[Array]` in the env)** — rejected (§2):
  under a partial port it violates the set invariant (`.map` fallback FP).
- **Accept as a permanent sound-subset decline** — rejected: it is resolvable
  Ruby-free, and the goal is to minimize practical mismatches, not to bank them.

## Slice 1a outcome + the measurement gate result (2026-07-06)

**Slice 1a shipped** (the syntactic-provenance possible-nil fire, no `Type::Tuple`):
closes the treemaps gap (algorithms possible-nil 50→49, matched +1 — line 45
only), 0 FP across ~1800 survey files, harness 54/54 with committed positive +
shape-negative fixtures (42/43), 435 tests. The FP-safe fire is gated on the
`penv` provenance set (`Array.new` zero-arg / constant size > 16), threaded on the
tenv side of the ADR-0038 substrate.

**Slice 1b (`Type::Tuple`) is DEFERRED by the measurement gate — thin EV.** Before
building the lattice change, the tier's would-be gains were measured against the
oracle in project (directory) mode:

- **always-truthy**: the real project-mode count is tiny (algorithms 3, rubocop-ast
  2, parser 4 — the "~117" figure was a per-file-isolation artifact), and every one
  is a `flow proves it truthy/falsey` case = **ivar value-flow / loop narrowing
  (Tier B/C)**, none a shape `if [1,2].size`.
- **undefined-method**: the gaps are dominated by **Rails/ActiveSupport plugin
  methods** on core types (`html_safe`/`blank?`/`present?`/`constantize`/`pluck`/
  `megabytes`/… — redmine alone had ~66, almost all this) plus project-class
  methods (`AST::Node#type?`) and Tier-B/C nil receivers. **Zero** need Tuple
  ELEMENT precision (`[1,"a"][0].typo` does not occur in real gaps; a Tuple shares
  Array's method set, so container typos already witness via `Nominal[Array]`).

Per §3's own rule ("if the EV is thin, the tier stays at Slice 1"), the shape tier
**stops at Slice 1a**. `Type::Tuple` / `HashShape` / propagation are not built. The
higher-EV frontiers the measurement surfaced instead: the **Rails/ActiveSupport
plugin** phase (the dominant undefined-method pool) and **Tier B/C possible-nil /
always-truthy** (ivar + loop flow — the dominant flow pool).

## Consequences

- Honest EV note: a `Tuple` shares `Array`'s method set, so this does NOT add
  `call.undefined-method` coverage over `Nominal[Array]` (probe P6's element-type
  witness — `[1,"a"][0].frobnicate` against Integer — is the exception where the
  ELEMENT type is what pays). Gains are element-type precision, the possible-nil
  FP-avoidance above, shape-method result types, and a measured always-truthy
  channel. Slice 1 is measurement-gated — if the EV is thin, the tier stays at
  Slice 1 (treemaps closed) rather than expanding speculatively.
- `HashShape` and shape-preserving method propagation are later slices, each
  0-FP-gated and each widening the type-wise invariant coverage monotonically
  (eventually retiring the syntactic-provenance restriction).
- `ARRAY_NEW_TUPLE_LIMIT` is a reference-implementation constant that can move
  silently on an upstream bump — UPSTREAM.md's bump procedure re-measures the
  shape thresholds.
