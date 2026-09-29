# 2026-09-29 — issue #136: operand entry-scope typing

**PR #296, merge `a498317`.** Closes #136. Upstream `e59b7b89`: rigor#1310 (`19df8143`, `OperandWalk` per-node scope index). Residuals filed: #306–#311.

## What landed

- The flat `TypeEnv` typed every use site from the end-of-file env — a same-statement mutation/rebind leaked backward to earlier operands. Now `CheckFlow` records the env before each top-level statement plus span-keyed rebind/mutation lists; `check_env_at` replays effects in evaluation order via `flow_children` (Uncond/Cond/Barrier edges): sequences, receiver→args, `if`-predicates, write values, literal elements unconditional; `&&`/`||` right sides, `if`/`case`/`when` branches, loops, rescue/else/ensure widen `Dynamic` as before; block/lambda/`def`/`class` bodies keep the flat env (closures may run later).
- `ScopedEnv::at(ast, typer, span, interner)`; `call_arguments`/`call_raise`/`void_value_use`/`driver` type each operand from its own entry env.
- `widen_mutated_locals` mints the nominal for any mutation on an all-unconditional path (`path_unconditional`) — `puts(b.unshift("s"), …); b.frobnicate` fires like the reference.
- Bonus fix: live master FP `s = "x"; s.upcase; s = 5` (end-of-file env read backward past a rebind) — now silent.

## Measured

- Headline + swapped control parity on both engines; ~19-row probe table incl. restored coverage (`x = b.first.upcase; b.unshift`, `b.unshift(b.first.upcase)`, `&&`-ordering, ternary arms).
- CI green on `d79d068`; sweep 0 FP across 8 corpora.
- Review (Opus adversarial, ~110 probes + dedicated master build for regression attribution): **Approved**.

## Residuals (filed)

- #306 NEW FP family (the one regression class): `path_unconditional`'s fallback mints unconditional nominals for effect spans unlinked in `flow_children` — `for`-header index stores, `rescue => h[:k]`, `Range` bounds. Fix direction: link as Cond/Uncond children.
- #307 rebind replay mints `Dynamic` (upstream's `[n += 1, n += 1] → [1, 2]` not delivered).
- #308 `case`/`when`/`rescue`-clause sibling-exclusion entry scope.
- #309 pre-existing rescue-carrier collection-snapshot FP.
- #310 systematic message drift: widened nominals render `for Array`/`for Hash` vs joined shapes.
- #311 pre-existing heredoc-interpolated effects unlinked.
