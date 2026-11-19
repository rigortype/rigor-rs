# #427: type display walks the interned DAG on an explicit stack

PR #648 → merged 2026-11-19 (`4de00c6`). Refs #427 (the `.map` acceptance
row still does not finish: type *building*, #424). Follow-up #650.

## What overflowed

`describe_named` / `erase_to_rbs_named` / `describe` recursed once per
type level (~440 B/frame). Repeated wrapping (`x = [[[…[x]…]]]`) builds
a Tuple chain hundreds of thousands of levels deep and overflowed even
the 256 MiB analysis stack — SIGABRT in `check`, server death on `lsp`
`didOpen`. The 25,000 Prism-edge bound does not count inferred type
depth (ADR-0045).

## Port shape

One `render_postorder` in `crates/rigor-types/src/display.rs`:

- Pass 1 counts in-degree of the reachable set (`HashMap`/`HashSet`,
  not `interner.len()`).
- Pass 2 `Enter`/`Exit` combines each `TypeId` once; a child's string
  is freed when its last parent combines (O(live strings)).
- Per-renderer child lists (`named_children` / `erase_children` /
  `describe_children`) match what each combiner reads via `at()`.

Interned `Type` children are `TypeId`s, so `Drop` is not recursive in
type depth. `Interner::cmp` still recurses on the Tuple axis but is
not on the wrap-crash path (a 2M-deep join finished on a 256 MiB stack).

No depth cap, no message change.

## Measured (release, aarch64)

| input | master | PR |
|---|---|---|
| wrap70 (630k, acceptance) | exit 1, 188 s / 711 MB | exit 1, 190 s / 505 MB, JSON identical |
| wrap90 | SIGABRT 183 s | exit 1, 364 s |
| wrap200 (1.8M) | SIGABRT 3m45s | exit 1, 546 s / 1.2 GB |
| lsp `didOpen` wrap70 | 1 diagnostic, alive | same, shutdown 0 |
| fuzz 10,020 × 3 renderers | — | 0 diffs vs master |
| `fp_audit --gaps --sweep` | — | 0 FP; gaps unchanged |

Display is still O(depth²) string copies (180k ≈ 7 s, 630k ≈ 50–65 s) —
#650. `.map { [[…[y]…]] }` does not finish: type construction (#424).
