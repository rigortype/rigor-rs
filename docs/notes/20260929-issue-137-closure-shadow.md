# 2026-09-29 — issue #137: block params shadow outer locals in unentered closures

**PR #305, merge `7839706`.** Closes #137. Upstream `e59b7b89`: rigor#1245 (`25b0925f`/`bffeaab0`). Residuals filed: #315–#317.

## What landed

- `closure_shadow_scopes` (flow_writes.rs) — the #166 shadow-scope census, now owned: bound set = `block_locals` ∪ `block_params` (+ `it`, `;` locals, body-assigned names), plus recovered children lowered under a CROSSED block/lambda (`super { |o| … }`) via the `LoweredAst::closure_bindings` side table.
- `ScopedEnv::at(ast, typer, site, id, interner) -> Cow<TypeEnv>` — composes #136's per-site entry-scope replay (`site` span) with bound-name shadowing to `Dynamic[top]` (`id` descendant of a closure body). `gate_at` stays unshadowed — Dynamic-only gates read the env their facts were computed on.
- collection_shape/nilable/class_narrowing walkers drop bound names into a scratch env for recovered children; bound-name writes/mutations drop out of `collect_flow_writes`/`collect_rebind_writes`/`bind_statement`.
- class_narrowing's block-body `btenv` KEEPS the outer binding — shadowing to Dynamic would let a disjoint guard mint a narrowing the reference's concrete element type renders disjoint (FP vector; verified only needed for entered blocks, see #317).

## Measured

- Issue row + lambda/`it`/`;`-local/`do…end`/nested/crossed-`super`/`->(a=o.f, o=nil)`/`when ->(o)` variants: silent on both. Captured control `map { |x| o + 1 }` fires `for { x: 1 }` on both — silence is real shadowing, not dead bodies.
- Composition with #136 verified: `puts(o = 9, [1].map { |o| o })`, `puts(b.unshift("s"), [1].map { |b| b })` — silent on both.
- Sweep: 0 FP / 818 gaps byte-identical vs `03e657c` baseline (gap-neutral — corpus doesn't hold these shapes). CI green on `905fba4`.
- Review (Opus adversarial, ~80 rows, zero FP-direction divergences): **Approved**.

## Residuals (filed)

- #315 entered-block bound params shadow instead of binding the element type (`[1].each { |o| o.f }` — ref fires `for 1`).
- #316 bound set wider than upstream's params ∪ `;`-locals — body-assigned names should capture/write outer.
- #317 unentered-closure `is_a?` guards should narrow the shadowed param (ref fires `for String`/`for Hash`).
