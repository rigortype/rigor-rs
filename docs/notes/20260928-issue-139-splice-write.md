# #139 — `Array#[]=` splice writes + top-level mutation widening

PR #284 (merged, head `0a4d943`).

## The bug

The check env kept the pre-mutation literal carrier: `a = []; a[0, 2] = [1, 2]`
left `a` at `Tuple[]`, so `a.last` folded to `nil` and `x.succ` fired
`call.undefined-method` `for nil` where the reference is silent — an FP. The
same applied to `a << 1`, `h["k"] = 1`, and any other top-level mutator.

## What landed

- `flow_writes::toplevel_mutations` collects `local.<mutator>(…)` statements
  under the same def/class/module + block-binding scope filters as
  `toplevel_rebinds` (shared walk factored to `toplevel_scope_filters`).
- `Typer::widen_mutated_locals` — the port of `MutationWidening.widen_for_mutator`:
  statement-position (unconditional) mutation mints the nominal
  (`Tuple` → `Array`, `HashShape` → `Hash`); a branch/block/value-nested one
  widens to `Dynamic` and hands the convergence question to the
  collection-shape pass. A `Constant` (String literal seed) stays put —
  widening it defeated `call_dispatch`'s stale nilable-fold guard and FP'd
  `x << "d"; x.getbyte(3).to_a` (corpus 119, caught only by CI's parity gate
  after the #164 rebase).
- `collection_shape.rs`: `[]=` classified by `index_store_form`
  (`content_join.rb:320`) — scalar index stores the value; `a[i, n]` /
  `a[range]` splice the RHS's elements (`coll_splice_members`: Tuple → its
  elements, `Nominal[Array]` → its type args, definite Array subclass → none,
  else the member itself); unclassifiable (`Either`) contributes both.
- `Node::BeginRescue` gained a flow arm: prism lowers an `if`'s `else` clause
  as a `BeginRescue` carrier, so `if c; a << 1; else; a << 1; end` converged
  on only one edge and the join declined. `main_body`/`ensure_body` evaluate
  in place; rescue clauses evaluate on a scratch env and widen.
- `driver.rs`: `check_collection_call` now gates on the widened check env
  (`ScopedEnv::at`) rather than `gate_at` — `gate_at` deliberately keeps the
  unwidened env for class narrowing, which closed the Dynamic/Top gate on
  every mutation-widened local forever. The snapshot itself witnesses
  convergence.

## Measured outcome

Fresh-dir probes vs pinned reference `e59b7b89` (keyed on rule/line/col; the
port renders `for Array` where the reference renders
`for Array[Dynamic[top] | Integer]` — member-erasure message drift):

| probe | before | after | reference |
|---|---|---|---|
| `a[0,2]=[1,2]; x=a.last; if x; x.succ` | `succ for nil` | silent | silent |
| `a[0,1]=[1]; a.first.upcase` | `upcase for nil` | silent | silent |
| `a[0]=1; a.frobnicate` | `for []` | `for Array` | fires (same site) |
| `h["k"]=1; h.frobnicate` | `for {}` | `for Hash` | fires (same site) |
| `if c; a<<1; else; a<<1; end; a.frobnicate` | silent | fires 8:3 | fires 8:3 |
| same, inside `def` | silent | fires | fires |
| one-edge / `a << 1 if c` variants | silent | silent | silent |

Gates: `gate.sh` pass; CI green on the final head (incl. clippy `-D warnings`
on 1.88); pre-ready `fp_audit --gaps --sweep` over 8 corpora: **0 FP**.

Residual (pre-existing, not regressed): union-receiver diagnostics
(`if c; a="s"; else; a<<1; end` — reference fires `for "s" | Array[…]`, port
has no union-witness arm); Hash `[]=` key-side join is a documented residual
of the flat member set.

Review: OpenCode GLM-5.2 primary `[PASS_PRIMARY]` after two revision rounds
(clone removal where borrows allow, `while let`, `ShadowScope` alias,
`push_unique`); DeepSeek V4 Pro final 【MERGE_APPROVED】.
