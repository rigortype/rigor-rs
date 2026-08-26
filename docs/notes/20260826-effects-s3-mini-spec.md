# Effects slice 3 mini-spec — the taint bit + `causes` (2026-08-26)

Implements ADR-0043 slice 3 from [the probe](20260826-effects-s3-probe.md),
whose §1 producer table and §2 classification are NORMATIVE. Builds on the
#106 posture fix (that OVER must be gone first — under the taint bit all
eight of its classes become *exhaustiveness* OVERs).

## The two facts that shape the slice

1. **`exhaustive` in the JSON is the TRANSITIVE bit**, not the direct one:
   direct = `causes.empty?` (`unit_scan.rb:138-145`), joined across reopens
   (`summary.rb:89-98`), then ANDed along every resolved project edge to a
   fixpoint (`propagator.rb:128-145`), and *that* is what the report prints.
   Emitting the direct bit scores **10 OVER on the corpus and 986 on
   mastodon/app**. A catalogue-CLAIMED call still keeps a project edge
   (`keeps_project_edge?`, `:409`).
2. **Consuming the Typer does NOT help and would hurt.** rigor-rs is
   deliberately more robust than the reference on shapes it degrades to
   `untyped` (`sig_gen.rs:20-23`), and `untyped == Dynamic` is exactly what
   upstream taints on — so the Typer's answer is usable only in the
   direction that ADDS taint. There is also no `DynamicOrigin` analogue, so
   `dynamic-receiver` details cannot be produced at all. Slice 3 therefore
   stays typer-free like slice 2, and a Typer-consuming `effects` would cost
   ≈3× (592ms → the serial 1,110ms shape the probe measured at 4,676 files).

## Rule

**A method is exhaustive iff the collector can see that no producer fires,
counting every undecidable site as firing.** Per the probe's table:

| producer | slice-3 treatment |
|---|---|
| `dynamic-send` (non-literal selector) | decide EXACTLY |
| `opaque-callable` — eval-family ≥1 positional, bare `binding`, `&expr` that is neither a symbol nor the unit's own `&blk` | decide EXACTLY (thread the block-param name through) |
| `opaque-callable` — the `.call` arm | taint (SOUND, over-taints) |
| `unknown-ownership` | EXACT at the six compound-write node types and on the claimed path; **taint** on the uncatalogued path |
| `dynamic-receiver` | **taint** — undecidable without typing |
| `unresolved-self-call` | **taint** unless the call resolves to a unit the collector itself collected in the receiver's own class |
| `method-missing`, `budget`, plugin/template/collector causes | never emitted (no producer at the pin, or out of scope) |

**Transitive closure**: a **selector-set edge taint at `push_edge`** — the
probe's stand-in for slice 4's real closure; measured cost **zero on the
graded corpus**. This is what makes the emitted bit the transitive one.

**Plugins** make upstream LESS exhaustive, and slice 2's self-defense only
covers annotations. Proportionate fix (do NOT reuse the blunt
`methods: {}`): when `.rigor.yml` configures `plugins:`, never emit
`exhaustive: true`. Annotation self-defense unchanged. Both need
must-still-fire controls: a plugin-free project MUST still reach
exhaustiveness.

**`causes` is ungraded** (`lanes()` reads three keys; `compare()` reads
nothing else) but slice 5 will render it, so emit the real shape now:
`[[cause, detail], …]`, deduped, sorted by `[cause, detail]`, `cause` from
upstream's closed 10-member enum, `detail` = the selector for
`unresolved-self-call`, `null` otherwise. **Retire the out-of-enum
`port-incomplete` marker** — it breaks the port's own
`causes.empty? == exhaustive` invariant.

## Acceptance

**The probe's predicted table, exactly** — derived with the grader's own
`compare()` and pinned against the real port's proven lane:

| project | MATCH | UNDER | OVER | DM |
|---|---|---|---|---|
| 01_core_origins | **16** | 0 | 0 | 0 |
| 02_propagation | **10** | 5 | 0 | 0 |
| 03_taint | **9** | 2 | 0 | 0 |
| 04_declared | 0 | 4 | 0 | 0 |
| **TOTAL (these four)** | **35** | **11** | **0** | **0** → PASS |

**`05_posture` (added after this table by [#106](20260826-s106-posture-over-fix.md), 133 methods) is NOT predicted here.** Its
slice-3 behaviour was never simulated, so it carries a different bar: **0
OVER is mandatory**, and its MATCH count is REPORTED, not pinned. Derive a
prediction for it first if you want one — do not retrofit the table to
whatever the build produces.

A HIGHER MATCH on the four predicted projects is a stop-and-re-derive
event, not a win. The 4 remaining
non-04 UNDERs are known and structural: 2 need receiver typing
(`Pipeline#transform`, `Taint#through_a_ghost`), 2 need the `resolved` bit
plus slice 4's real closure (`Recursive#mutual_b`, `Recursive#walk`).

**Real-scale report** (not a hard gate — a live corpus drifts, but report
it): mastodon/app should move MATCH 4,517 → 5,234 and extra-taint 945 →
243 at **0 OVER**.

Also promote the probe's **`p_edge`** project into
`harness/effects-corpus/` — it isolates the transitive-vs-direct trap (three
methods direct-exhaustive but transitively tainted, via `Kernel#format`
shadowing and a posture edge) and no existing gate sees it.

## Gates (BARE)

1. `cargo test --workspace`; clippy fresh `CARGO_TARGET_DIR` `-D warnings`.
2. `harness/effects_diff.py` — the table above, plus `--self-test` PASS,
   plus the new `p_edge` project. Any OVER anywhere is a hard failure; fix
   the port, never the grader or the corpus.
3. `rigor check` byte-identical vs a master binary on mastodon/app (both
   thread modes); `harness/run_snapshot.rb` PASS 0 unregistered; release
   rebuild + `harness/fp_audit.py --gaps --sweep` 0 FP / 9,204 with the gap
   set unchanged.
4. `harness/docs_check.py`.

## Non-goals

The transitive `effects` lane itself (slice 4 — this slice only taints
along edges, it does not propagate LABELS); the `resolved` bit; the declared
lane (slice 6); snapshot/update/text surfaces (slice 5); the plugin effect
layer; any Typer consumption; any `rigor check` change.
