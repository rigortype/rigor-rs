# Port the effect system as a SOUND-SUBSET summary, graded by a project-level differential

Status: accepted 2026-08-25 (scoping ADR; the slices it names are not yet built)

The reference shipped an opt-in **effect system** in `v0.3.4` (upstream
[ADR-103](../../reference/rigor/docs/adr/103-effect-labels.md), umbrella issue
#376, 18 slices): `rigor effects` reports what each method *does* —
`io.output.stdout`, `nondet.time`, `mutate.self` — beside what it returns,
`rigor effects update` commits that report as `.rigor-effects.yml` the way
`db/schema.rb` commits a schema, and `%a{pure}` / `%a{rigor:v1:effect …}` become
contracts. rigor-rs ports it.

This ADR fixes the two things every slice needs decided first and nothing else:
**what parity means for a report that is not a diagnostic**, and **what
instrument grades it**. It deliberately does not design the collector.

## Context — why the existing parity apparatus does not reach

[ADR-0002](0002-diagnostic-set-parity.md) is the whole of the port's parity
contract, and it is stated over `rigor check`'s **diagnostic set**: for a given
input, rigor-rs's `(rule id, location)` set must equal the reference's, and
rigor-rs must never emit what the reference does not. Every instrument the repo
owns implements exactly that comparison — `harness/run.rb`,
`harness/run_snapshot.rb`, `harness/fp_audit.py`, `harness/gap_census.py` all
reduce a run to a set of `(path, line, column, rule)` keys.

An effect summary is not in that shape and cannot be forced into it:

- **It is not a diagnostic.** Upstream WD7 is explicit that the snapshot "emits
  no diagnostic and never enters `rigor check`'s stream". The `effect.*`
  diagnostic family (WD8) is a later, separately-gated layer that *reads*
  summaries; the summaries themselves are the primary artifact.
- **It has no location.** The key is a method (`Report#render`), not a
  `(line, column)`. Two engines can agree perfectly on a method's effects while
  disagreeing about every span in the file.
- **It is not a set — it is a lattice point with a taint bit.** Each method
  carries a *proven* lane (labels), a *declared* lane (`≤`, copied from
  annotations) and an **exhaustiveness** bit tainted by any unresolved or
  dynamic call, plus the causes of that taint. "Equal or not" throws away the
  only structure that says which direction of disagreement is safe.
- **It only exists for a PROJECT.** `rigor effects` reads `.rigor.yml` and
  analyses a project's own call graph as a closed world (WD4). Every corpus tool
  in this repo deliberately runs both sides from a clean temp cwd with no
  config, which is why the recorded blind spots exist
  ([fp_audit is blind to project `sig/`](../notes/20260731-head-survey-and-set-op-folds.md);
  the plugin surface, [note](../notes/20260825-upstream-survey-v034-master.md)).
  An effects differential cannot be built that way — it is structurally a
  *project* harness.

## Decision

### 1. `rigor check` parity is unchanged, and gains one new obligation

ADR-0002 stands exactly as written for `rigor check`. Upstream WD13 promises
that a project without an `effects:` block gets a **byte-identical** `rigor
check` at unchanged cost. rigor-rs adopts the same promise as a **gate**: the
existing fixture harness and the 9204-file sweep must not move by one diagnostic
when effects collection is present-but-off, and must not move when it is *on*
either — collection is observational, it "records what the typer already decided
and never asks it to decide more".

Practically: the effects work may not change `crates/rigor-infer`'s answers. If a
slice needs the typer to decide something new, that is a different slice with a
different gate.

### 2. Effect summaries are a SOUND SUBSET, not an equal set

The port's diagnostic posture is a sound subset — never emit what the reference
does not; missing is fine, extra is a bug. The effect analogue is not "the same
labels", because the two disagreement directions have opposite cost. For each
method `m` present on both sides:

| lane | contract | why this direction |
|---|---|---|
| **proven** | `proven_rs(m) ⊆ proven_ref(m)` | The proven lane is the only lane a verdict may read (upstream's discriminating criterion). An envelope check fires when the proven lane exceeds a declared bound, so an extra proven label is exactly a false positive in waiting. A missing one is a coverage gap. |
| **exhaustive** | `exhaustive_rs(m) ⇒ exhaustive_ref(m)` | The taint bit means "there is a call I could not resolve". A method marked non-exhaustive produces no finding, so rigor-rs may be **more** tainted than the reference and stay sound. Claiming exhaustiveness the reference does not claim is the unsafe direction. |
| **declared** | `declared_rs(m) = declared_ref(m)` | The declared lane is *copied from the author's annotation*, not inferred, and upstream WD17 rules that it is never judged. Two engines reading the same `%a{…}` must read it the same way; a difference here is a parse bug, not a coverage gap, and is graded as an exact match. |

Name it the **sound-subset summary**, and note it is the mirror image of
`sig-gen`'s sound-*superset* model (AGENTS.md "Generative-tool parity"): a
generated signature may be weaker than the truth, an asserted effect may not be
stronger.

Two more rules that follow, and are stated so a slice cannot quietly break them:

- **A method rigor-rs does not report at all is an under-claim, never a
  failure.** The port will start with an empty `methods:` map and grow.
- **A method rigor-rs reports that the reference does not is an OVER-CLAIM and
  fails the gate**, on the same reasoning as an extra proven label: the method
  exists on our side of the closed world and not on theirs, so any envelope over
  it is judged against evidence the oracle never had.

### 3. `.rigor-effects.yml` is the parity artifact; the JSON report is the detail

Two surfaces, both already diffable, and the port owes both:

- **`rigor effects update` → `.rigor-effects.yml`** holds `methods:` as
  **direct** summaries (a diff is attributable to the PR's own lines) and
  `reach:` as the transitive footprint at entry points. It is the primary
  validation upstream ships first, and it is a normalised YAML file — the
  cheapest possible differential.
- **`rigor effects --format=json`** carries the transitive `effects`, the
  `declared` lane, the `exhaustive` bit, the `causes` of taint, and the per-origin
  `direct` breakdown (`catalogue:Kernel#puts`, `construct:ivar-write`). This is
  what the differential actually grades, because it is the only surface that
  exposes the taint bit and the origin attribution — a summary that is right for
  the wrong reason is a slice away from being wrong.

The snapshot header (`rigor:` version, `vocabulary:`, `config_digest:`) is
**excluded** from the comparison: the version string necessarily differs
(`0.3.4` vs `0.0.1`), and the digest covers config both sides read identically.
Excluding it is a normalisation, and the harness states it rather than hiding it.

### 4. `harness/effects_diff.py` is the instrument

A project-level differential, mirroring `fp_audit.py`'s hard-won contracts and
adding what this surface needs:

- **Inherited unchanged**: the reference is the PINNED submodule; its own
  `rigor-rbs-inline` plugin lib is pinned onto `-I` (UPSTREAM.md hazard 1); the
  measured rigor-rs binary is `target/release/rigor`, auto-built, refused when
  older than the newest `crates/` file, with path and build time in the header
  (the [stale-binary lesson](../notes/20260807-fp-audit-port-side-blind-spots.md));
  a side that produces no parseable output is **INVALID, never empty** — an empty
  port result would make the over-claim count 0 by construction, the exact shape
  that let a crashing binary pass the central gate once already.
- **New, and forced by the surface**: the unit of measurement is a **project
  directory**, not a file list, and each arm runs **in that directory** rather
  than in a fresh temp cwd — the config is the point. *(Amended 2026-08-26:
  the original text claimed the result cache "is disabled" — `rigor effects`
  accepts no `--no-cache`; instead the instrument clears the project's
  `.rigor/cache` around each reference run, and upstream's effects cache key
  composes the engine digests so a pin bump invalidates on its own — see the
  [slice-1 probe](../notes/20260826-effects-s1-catalogue-probe.md).)*
  Because it runs in-project, this is the
  first instrument in the repo that sees project `sig/`, `.rigor.yml` and
  plugins; the standing sweep's blind spots do not apply to it, and it should be
  pointed at project-shaped corpora deliberately.
- **The verdict is four-way, not two-way**: `MATCH`, `UNDER` (missing method,
  missing proven label, or more taint — expected, reported, never fatal),
  `OVER` (extra method, extra proven label, or claimed exhaustiveness the oracle
  does not claim — **the gate**), and `DECLARED-MISMATCH` (an exact-match lane
  differing — also fatal, and a different bug class).

Gate: **0 `OVER`, 0 `DECLARED-MISMATCH`.** `UNDER` is the arc's progress metric,
the way coverage gaps are for diagnostics.

### 5. Slice order follows upstream's, and the first slice is the snapshot

Upstream sliced this as 18 tracer bullets and shipped the snapshot first,
deliberately "ahead of any envelope syntax" (WD7). The port follows that order,
because the snapshot is what makes every later slice measurable:

| # | slice | gate |
|---|---|---|
| 0 | **this ADR + `effects_diff.py` + a project fixture set** | the harness runs, self-diffs clean, and reports the reference's baseline as the debt |
| 1 | vocabulary + the vendored effect catalogue (`data/effects/registry.yml`, `core.yml`), as a pin-tracking surface with a PROVENANCE and a re-sync step | catalogue parses; label subsumption unit-tested |
| 2 | **direct** summaries: catalogue rows + the construct origins (backticks, `$gvar`, `@@cvar`, `@ivar` writes, `alias`/`undef`, `define_method`) | 0 OVER on the fixture set |
| 3 | the taint bit and its causes (unresolved / dynamic receivers) | 0 OVER; taint at least as strict as the oracle's |
| 4 | transitive propagation over the project call graph, overrides joined | 0 OVER |
| 5 | `rigor effects` + `--format=json` + `effects update` / `check` / `diff` | snapshot byte-comparable modulo the excluded header |
| 6+ | the declared lane, envelopes, and the `effect.*` diagnostics | ADR-0002's existing gate, once the diagnostics exist |

### 6. Explicitly out of scope for the port's v1

- **The `effect.*` diagnostic family** until slice 6. It is opt-in and
  author-directed upstream, and it reads summaries the port does not have yet.
- **Views as effect units** (WD11). It needs the ADR-16 Tier-D macro seam, which
  this port does not have.
- **The engine consumers** (WD9) — ivar-reset skip, computed purity, the
  constant-folding gate. Each is a change to what `crates/rigor-infer` decides,
  which § 1 forbids the effects work from doing; each is its own ADR when its
  slice pays.
- **`effects-on-by-default`.** Upstream previews it as bleeding-edge for v0.4.0;
  the port follows the pin, not the preview.

## Open at accepted

One, and it is the declared lane's — the lane § 2 grades as an exact match, so
it has to be settled before slice 6 and it is cheap to get wrong quietly.

**When does the oracle populate `declared:`?** Measured at the `v0.3.4` pin on
`harness/effects-corpus/04_declared`: of three annotated methods the reference
surfaces `declared:` for exactly one. `%a{pure}` reading as the empty envelope
(WD5) explains one of the two silences and nothing explains the other — the two
`%a{rigor:v1:effect io.db}` methods have the same annotation spelling and the
same comment shape, and one reports the bound while the other reports `[]`.
Four hypotheses were probed against a scratch project and all four are refuted:
not "only when exceeded", not "only when the proven lane is non-empty", not
"only when the method has a project-method edge", and not a stale effects cache.

Until it is explained, an implementer must not infer the rule from the fixture.
The fixture records the reproducer; `effects_diff.py` already captures the lane,
so the answer is one measurement away and it is not on this slice's critical
path.

## Consequences

- The repo gains a second parity model, and the two must not be confused. The
  one-line discriminator: **a diagnostic is graded as a set, a summary as a
  lattice point with a direction.**
- `effects_diff.py` is the first project-level instrument, so it is also the
  first place a project-`sig/` or plugin regression can be caught by a standing
  gate rather than by hand. That is a side benefit worth spending on: the two
  most recent live-FP findings in this repo were both in surfaces no sweep tool
  could reach.
- The port will read as very far behind for several slices — the reference
  reports a summary for every method in a project, and rigor-rs will report
  none. `UNDER` counts are the arc's odometer, not a failure, and the ADR says so
  in advance so a future reader does not mistake the first measurement for a
  regression.

## Relationship to other ADRs

- [ADR-0002](0002-diagnostic-set-parity.md) — unchanged, and now explicitly
  scoped to `rigor check`.
- [ADR-0011](0011-reference-oracle-exceptions.md) — the divergence registry
  excuses *diagnostics*. An `OVER` summary has no registry; it is fixed or the
  slice does not land.
- Upstream [ADR-103](../../reference/rigor/docs/adr/103-effect-labels.md) is the
  design of record. This ADR does not restate it and does not fork it: where the
  two differ, upstream is authoritative and this file is stale.
