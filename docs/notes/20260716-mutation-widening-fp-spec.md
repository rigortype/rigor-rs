# Binding spec — MutationWidening FP fix (`flow.always-truthy-condition` on mutated collection locals)

Oracle: reference `47ec8625` (v0.3.0 RC). Sonnet investigation 2026-07-16
(source read + v0.2.7 isolated build + paired probes + 6200-file sweep).

## The measured FP

gitlab-foss `lib/gitlab/ci/pipeline/expression/parser.rb:41:69` and `:42:69` —
rigor-rs fires `flow.always-truthy-condition` on `results.count > 1` /
`< 1` where `results = []` is content-mutated (`push`/`pop`) inside a nested
`case` in an `each` block. The reference is silent. **NOT RC-new**: an isolated
v0.2.7 build already declines (the `MutationWidening` subsystem landed upstream
2026-05-27, `12a8b68d`, + ADR-56 slices 2026-06-11/12 — all pre-v0.2.7).
rigor-rs never ported it; the gitlab `lib` sweep surfaced it. Sweep result:
this is the ONLY live FP cause across gitlab-foss lib (4676) + app/models
(1224) + mastodon app/models (248) + lib (65).

## Reference semantics

Two composable passes in `StatementEvaluator#eval_call`
(`statement_evaluator.rb:1342,1353`), consuming
`lib/rigor/inference/mutation_widening.rb`:

1. `widen_after_call` — on EVERY call (block or not, bare or in a condition):
   receiver is a bare local/ivar read bound to a literal-shape carrier
   (`Tuple`/`HashShape`) AND method ∈ mutator set ⇒ widen the binding:
   `Tuple[…]` → `Array[union(elems)]`, `HashShape` → `Hash[K, V]`.
2. `widen_after_block` — walks a block body (any nesting depth) for mutator
   calls on captured OUTER locals/ivars and widens the outer binding.
3. ADR-56 slice C (join appended element types) is a precision refinement, NOT
   needed to kill this FP (arity-forgetting alone kills the `Tuple[]` count
   fold).

Mutator sets (`mutation_widening.rb:70-87`):
- ARRAY: `<< push append prepend unshift concat insert pop shift delete
  delete_at delete_if reject! clear compact! replace fill []= map! collect!
  select! filter! keep_if uniq! flatten! sort! sort_by! reverse! rotate!
  shuffle! slice!`
- HASH: `[]= store delete delete_if reject! select! filter! keep_if clear
  compact! merge! update transform_keys! transform_values! replace`
- Excluded (`PURE_SELF_RETURNERS`): `freeze dup clone itself`.

This is a **general env type-widening**, not a rule-local gate — downstream it
feeds always-truthy AND possible-nil AND tuple-projection folds. The port must
match that shape.

## Probe matrix (reference vs rigor-rs current)

| # | shape | ref | rs | verdict |
|---|---|---|---|---|
| P1 | parser.rb (block + nested case, push/pop) | silent | fires ×2 | the FP |
| P2 | `results = []` no mutation, `.count >1/<1` | fires ×2 | fires ×2 | rail — must keep firing |
| P3 | straight-line `results.push(1)` (no block) | silent | fires | same cause |
| P4 | `push` under an `if` modifier | silent | fires | same cause |
| P5 | `results = results + [x]` in block (rebind) | silent | silent | already correct |
| P6 | helper method mutates arg (`def add(a,x); a.push(x)`) | silent (RC-new, `af3efef3` 2026-07-11; v0.2.7 FIRED) | fires | interprocedural — SEPARATE ticket, defer; 0 live corpus instances |
| P7 | `results << x` in block | silent | fires | same cause |
| P8 | `a=[]; xs.each{a<<x}; a.first.frobnicate` | silent | silent | no live FP; see follow-up note |

## rigor-rs fix (minimal, general)

`crates/rigor-infer/src/lib.rs`:

- Extend **`collect_flow_writes`** (`:1335-1343`; currently only
  LocalVariableWrite/OpWrite rebind spans) to ALSO record
  `(call.span, receiver_name)` for every
  `Node::Call { receiver: Some(r → LocalVariableRead{name}), method, .. }`
  with method ∈ `MUTATOR_METHODS` (ARRAY ∪ HASH above, minus the
  pure-self-returners). `ast.iter()` walks nested block/case bodies already, so
  P1 comes free; the call is its own containing span so P3/P4 resolve through
  the existing catch-all/`If` widen arms.
- NO consumer changes: `widen_flow_writes`/`widen_penv_writes` are generic and
  shared by the always-truthy pass (`:854-959`) and the possible-nil pass
  (`:1023-1219`).
- Follow-up (flagged, separate): the possible-nil pass's straight-line
  `Node::Call` arm (`:1115-1117`) never applies `widen_flow_writes` for
  non-block calls — pre-existing, no live FP found (P8 silent both sides);
  probe-sweep before assuming parity there.
- P6 (interprocedural callee-mutates-arg floor, RC-new) is OUT OF SCOPE.

Widening only needs to make the local non-value-pinned for the flow passes
(the existing widen helpers already do exactly this); do not implement the
ADR-56 slice C element-join.

## Gates

- New harness fixture (mutation-widening arity): straight-line push,
  the parser.rb block shape (both count directions), and the load-bearing
  negative control (NO mutation ⇒ always-truthy MUST still fire both
  directions). Snapshot + run.rb + run_snapshot.rb.
- `cargo test` (add unit tests: P1–P5, P7 shapes), clippy on touched files.
- `fp_audit.py` gitlab-foss `lib` → the 2 FPs GONE, 0 new; mastodon `app` →
  0 FP, matched count unchanged (397).
- parser.rb E2E: rigor-rs silent on 41/42 like the reference.
