# Bleeding-edge surface + `static.value-use.void` (2026-07-18)

The productization item that unblocks the Phase-3 deferral
([note](20260718-phase3-new-rule-surfaces.md)): the ADR-50 WD2 bleeding-edge
overlay end-to-end, and the first `static.*` rule riding it (ADR-100).

## Surface (all byte-matched against the live reference)

- **Config** `bleeding_edge:` — `false` (default) / `true` / `[ids]` /
  `{ all: true, except: [ids] }` → `BleedingEdgeSelector`; unrecognized shapes
  degrade to `None` (config never aborts here; the reference raises).
- **CLI** `--bleeding-edge[=LIST]` / `--no-bleeding-edge` on `check`
  (`=LIST`, not ` LIST`, so a bare flag never swallows a positional path);
  CLI > config.
- **`rigor show-bleedingedge`** — overlay + adoption, byte-identical output
  (both the `(none)` and adopting spellings diffed against the reference).
- The registry carries BOTH features verbatim; `reject-unparseable-signatures`
  is adoption-inert here (it promotes rules rigor-rs does not emit — the env
  cannot collapse, see the Phase-3 note).

## The rule (`static.value-use.void`, ADR-100)

- **Index**: `-> void` tracked as a `method_signature` flag under the same
  all-overloads-agree collapse as class/nil/instance-ness; per-entry
  `void_methods` / `void_singleton_methods` sets with first-definer-wins
  reopen/merge semantics; `method_return_is_void` /
  `singleton_method_is_void` resolve at the first defining ancestor.
- **Collector** (`void_value_use_diagnostics`): value context read top-down
  from the consumer — assignment RHS (local/ivar/constant writes; the
  reference also covers cvar/gvar/const-path writes, lowered opaquely here —
  under-emit), call receiver, positional arguments. Bare statements silent.
  Direct-dispatch only (resolvable receiver class; never guess).
- **Gate**: the reference authors `:warning` but resolves `:off` in every
  shipped profile, promoting via the feature; rigor-rs runs the collector only
  when `use-of-void-value` is active — the same observable. Produced before
  `filter_suppressed` (self-suppressible), subject to `disable:`; family
  `static` joins RULE_FAMILIES so `# rigor:disable static` works.
- **Parity**: 3-diagnostic probe (`x = w.fire` / `puts(w.fire)` /
  `w.fire.to_s` against a project sig) byte-identical to the reference under
  `--bleeding-edge=use-of-void-value`; silent without the flag on both sides.

## Evidence

Workspace tests green (+6: index void flags, selector coercion/activation,
collector contexts + negatives); explain catalog 28→29 (`static.value-use.void`);
live + snapshot gates unchanged (205 matched / 0 gaps / 0 FP — the rule is
flag-gated, harness runs configless); fp_audit spot 0 FP; clippy clean.
