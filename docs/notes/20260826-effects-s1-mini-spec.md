# Effects slice 1 mini-spec — the vendored catalogue crate (2026-08-26)

Implements ADR-0043 slice 1 from [the catalogue probe](20260826-effects-s1-catalogue-probe.md).
ADR §1 is binding: NOTHING in `rigor-infer`/`rigor-rules` changes, no consumer
is wired, no `effects` subcommand exists after this slice. The deliverable is
the catalogue as a correctly-loaded, drift-gated, pin-tracking surface.

## Shape

New crate **`crates/rigor-effects`** (the workspace member glob picks it up).
Dependencies: `serde`/`serde_yaml` only — a separate crate makes ADR-0043 §1
a dependency-graph fact (nothing depends on it yet).

Vendored verbatim under `crates/rigor-effects/vendor/effects/`:
- `registry.yml` (67 lines, sha256 `bb0eb3f0…` at pin `b10bd5df`)
- `core.yml` (843 lines, sha256 `85778dd3…`)
- `PROVENANCE.md` — pin, source paths, upstream's own `Catalog#identity`
  anchor (`1:85778dd3…`), and the THREE stated carve-outs that are slice-2
  CODE, not data: the mutator sets (`ARRAY_MUTATORS` 31 / `HASH_MUTATORS`
  15 / `STRING_MUTATORS` 26), the 7 narrowing handler BODIES, and the plugin
  effect layer (out of ADR scope entirely).

Loaded via direct `include_str!` (no build.rs), parsed LAZILY on first use so
nothing pays for it until an effects surface exists.

## Semantics slice 1 ships (each mirrored from the reference, cited)

1. **`Label`** — segment-aware prefix subsumption (`io` subsumes `io.fs.read`
   but NOT `iox`), validity, `ancestors`. Port upstream's label spec as unit
   tests (the ADR's stated gate: "label subsumption unit-tested").
2. **`Registry`** — the 36 declared labels, groups, roots, `retired`, and
   `known?` = declared ∪ their ANCESTORS (`registry.rb` `build_known`). The
   trap the probe proved: four roots (`global`, `email`, `job`, `cache`)
   exist ONLY as implied ancestors, and `core.yml`'s `global` posture emits
   bare `global` — a port validating against the 36 declared rows rejects
   the shipped catalogue. A test MUST pin exactly this (bare `global` is
   `known?`).
3. **`Catalog`** — parse the 14 postures, 34 universal selectors, 80
   classes / 420 rows; preserve the distinctions: explicit `effects: []` vs
   NO row, `mutates: receiver`, `narrow:` (an opaque handler NAME in slice
   1), `kind: object`, by-reference mutator-set references (opaque names).
   **`lookup` implements the row → universal → posture PRECEDENCE** as data
   access (`catalog.rb:122`'s order) — precedence is mechanical lookup
   order and is testable now; what stays slice-2 is EVALUATING handlers and
   EXPANDING mutator sets. `lookup`'s result carries the posture provenance
   (`from_posture`) the way upstream's does.
4. Every label the catalogue emits must satisfy `Registry::known?` — assert
   it over all 420 rows + postures + universals in a test (this is what
   catches a bad vendor or a bad ancestor rule wholesale).

No `Box::leak`; owned data, lazily parsed, plain structs.

## The drift gate (the pin-surface hazard — three layers, all mandatory)

The vendored plugin RBS drifted for 2 months and cost 10 FPs; effects_diff
exercises 6 of 420 rows (1.4%) and CANNOT be the gate. Ship:

1. Upstream's two data specs (registry spec + catalog spec) ported as unit
   tests over the VENDORED bytes.
2. **`harness/vendor_effects.py`** modeled on `vendor_rbs.py`: regenerates
   `vendor/effects/` from the pinned submodule; `--check` compares
   byte-for-byte and fails on drift. Prove it by regenerating the current
   tree byte-identically first (the vendor_rbs precedent).
3. A unit test asserting the sha256 of the embedded bytes against the
   PROVENANCE constants — so `cargo test` alone catches a hand-edit.

**UPSTREAM.md**: add the third re-sync bullet to the ritual step that
covers the vendored surfaces (`vendor_effects.py --check` at every re-pin,
alongside the rbs and plugin-sig steps).

## Acceptance gates (BARE)

1. `cargo test -p rigor-effects` (all semantics + digest tests), then
   `cargo test --workspace`.
2. `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy --workspace --all-targets -- -D warnings`.
3. `python3 harness/vendor_effects.py --check` — green on the committed tree.
4. `harness/effects_diff.py --self-test` still PASSES (nothing consumed the
   crate, so this is a tripwire).
5. `rigor check` byte-identical vs a master binary on one corpus (tripwire:
   the crate must be inert), `harness/run_snapshot.rb` PASS.
6. `python3 harness/docs_check.py` (UPSTREAM.md edit stays in budget).

## Non-goals

Slice 2+ (direct summaries, taint, propagation, commands, declared lane —
note the `declared:` semantics are now SOLVED in the probe §7 for slice 6),
mutator-set expansion, narrowing handler bodies, the plugin effect layer,
any consumer wiring.
