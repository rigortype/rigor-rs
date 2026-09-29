# 2026-09-29 — issue #135: in-place mutator element precision + compound index writes

**PR #297, merge `0a2f25c`.** Closes #135; resolves #298. Upstream `e59b7b89`: rigor#1277 (`1e1f331e` — in-place mutators drop old element types) + rigor#1296 (`30615cd1`, `operand_effects.rb`); the brief rows were already silent via `e4f1481`. Live FP found in the same seam: compound index writes. Residuals filed: #312–#314.

## What landed

- `Node::IndexWrite { receiver, indices, value, span }` — all three Prism kinds (`IndexOrWrite`/`IndexAndWrite`/`IndexOperatorWrite`) lower to a real node, NOT a `Call`: the reference's `eval_index_or_write` widens/types without dispatching `call.*` rules on synthesized `[]`/`[]=` (`c[0] ||= 1` on a bare class must stay silent).
- Routed into `widen_for_mutator` as `[]=` — same widening `h[k] = v` gets (upstream `IndexWriteWidening`, rigor#560).
- `collect_flow_writes`/`toplevel_mutations` record bare-local index-write receivers keyed on statement span; `coll_flow_expr` arm mirrors `Call` via shared `coll_grown_carrier`; `class_narrowing`/`nilable`/`flow_eval` arms restore operand descent (the recovered carrier previously provided it); store kills narrowed facts by span.
- Rebase over #134 (`e5613cc`): `Index{Or,And,Operator}Write` removed from `is_unmodeled_write`; `Node::IndexWrite` added to the block-fold decline set (`fold_body_has_unmodelled_write`) — `x[i] += 1` inside a folded block can't be replayed by the flat overlay.

## Measured

- `h[:a] +=/||=/&&= 1; h[:a].upcase` — silent on both; `a[0] += 1` idem; conditional-contained writes silent; `t[:x][0] += 1` fires `for 1` on both; `c[0] ||= 1` on `class C; end` silent both.
- #134 rows (multi-assign/`for`/`rescue` index targets) coexistence verified; #194 fold declines still correct (`for Array` vs ref's folded tuples).
- CI green on `5cc322a`; sweep 0 FP / 818 gaps; gate.sh pass.
- Review (Opus adversarial): **Approved**.

## Residuals (filed)

- #312 compound index writes under recovery wrappers (`x = foo rescue (h[:a] ||= 1)`) — pre-existing FP; fix = `visit_index_*_write_node` in `collect_recoverable_children`.
- #313 `with_indexed_narrowing`/`index_write_stored_type` unported — `||=` slot narrowing + IndexWrite value type; includes a flagged test-comment fix in rules/tests.rs.
- #314 `when`-condition writes leak into the case join (pre-existing for `Call`, extends to `IndexWrite`).
