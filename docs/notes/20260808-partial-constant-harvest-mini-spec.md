# Partially-literal constant harvesting — mini-spec (2026-08-08)

Turns the [probe evidence](20260808-partial-constant-harvest-probes.md) into an
ordered slice plan. The probes overturned the framing this track was parked
under: the reference NEVER declines a partially-literal constant (it keeps a
full shape and types a lambda as `Proc`), so C5's all-or-nothing decline is a
rigor-rs artifact, and — the bigger finding — **C5's project-wide harvest is a
live FP class on master**: the reference's constant-value typing is per-FILE
(a cross-file read of even a fully-literal constant is reference-silent;
probes-note "cross-file" row). Priority order below follows FP-first.

## Ordered slices

### A — per-file constant-value consumption (FP hygiene, FIRST)

Restrict the C5 bare-name arm, the 2e qualified twin, and any other
`ConstLit` consumer to constants harvested from the SAME FILE as the use
site (the harvest may stay project-wide; the CONSUMPTION gate is per-file).

- FIRST widen the probe base: the one probed shape was a same-namespace bare
  read. Probe cross-file with a qualified path, via `include`, and a
  same-file-different-class read, both engines, before fixing the gate shape.
- Census expectation: **no legitimate row can reopen** — a gap row is
  reference-fired, the reference is per-file, so every reference-fired
  constant row is same-file. Any row that disappears from the MATCHED set is
  by definition a rigor-rs-only emission (a latent FP) — record the count.
- Sweep stays 0 FP; fixture rows pin same-file fires + a cross-file silence.

### B — keep partially-literal containers as INERT bare nominals

Widen `const_lit_of`'s container arms: a hash/array literal whose ELEMENTS
are non-literal (lambda values, dynamic elements, splats, non-static keys)
harvests as a bare `Nominal[Hash]`/`Nominal[Array]` — **elements NEVER typed**
(probe z2: the reference leaves a const-read element `Dynamic`; element-typed
harvesting would out-precise the oracle at exactly the projection sites it
declines — an FP generator). Fully-literal harvesting is unchanged
(value-pinned rendering must not regress).

- Safety is the probed INERTNESS: `fold_tuple_projection`/
  `fold_hash_shape_projection` match `Tuple`/`HashShape` only, arity/ATM/
  possible-nil/always-truthy unreachable; the only surfaces that light up are
  `call.undefined-method` (the intent) and `call.raise-non-exception`
  (parity-positive — a container is never an Exception). Pin both with tests.
- **Message rendering diverges** on these rows (`for Hash` vs the reference's
  `for { c: Proc }`): check `harness/run.rb`'s match key BEFORE registering
  fixture rows; if the message is load-bearing, register the divergence
  explicitly rather than skipping the row.
- Yield: the routable_token row (real-row probe fires at `:61:14`); census
  says up to 292 constants newly harvest (+31%) — the diff must be walked and
  any bonus closure oracle-spot-checked; ZERO new rows.
- Reassignment: the reference unions duplicate assignments (probe). C5's
  existing single-assignment gate is a strict under-emit — keep it.

### C — literal-rooted chain constants (ASSESS, likely small or deferred)

`SEV = %w[…].map.with_index.to_h.freeze` types bare `Hash` in the reference.
Candidate mechanism: type the RHS with the existing typer folds (empty env,
same file) and keep a resulting bare `Nominal[Hash]`/`Nominal[Array]`.
The codequality row ALSO needs `Hash#keys` off an argument-less nominal to
produce `Array` (probes-note envelope §2 — two mechanisms). Assess after B:
build only if the fold path already produces the nominal and the `keys`
projection is a small, probed extension; otherwise record the decline with
the two-mechanism evidence. 357 chain constants in the corpora bound the
blast radius — any build must walk its census diff row by row.

## Verification (binding, per slice)

Full gates (`docs_check.py` BARE), sweep 0 FP / 9204, census pre/post with
every moved row explained, fixture `harness/corpus/92_partial_constant_harvest.rb`
(verify 92 free) with oracle-verified lines incl. cross-file controls.

## Non-goals

- No element typing anywhere in B/C. No shape unions for reassignment.
- No change to the ADR-0033 provenance gates or the 2b `ENV` arm's negative
  check (it consults the harvest — verify it still declines what it must).
