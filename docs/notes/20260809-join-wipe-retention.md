# Join-wipe retention — a fact must survive an unrelated conditional (2026-08-09)

`class_flow_if`'s `join_cenv` blanket-wiped every `Narrowed` local fact and
every chain fact at each conditional merge. The only clawback was the
early-return propagation's re-seed, which restores facts ONLY for the targets
named in that conditional's OWN carried guard map. So a fact minted by an
EARLIER statement died at ANY later intervening `if`/`unless`/`case` —
terminating or not, same local or a different one, in `case` and in expression
position too.

The reference's `Scope#join` (`scope.rb:680`) UNIONS the two edges per local, and
a union of a type with itself is that type. A fact established BEFORE the
conditional is on both edges by construction, so the reference keeps it. This
slice restores that.

## Probe matrix

Pin `v0.3.2` / `c6b91b9e`, one FRESH temp cwd per case, `--no-cache`, both
reference libs on `-I` (`<ref>/lib`, `<ref>/plugins/rigor-rbs-inline/lib`).
Every row below was RE-MEASURED for this slice; the 14 rows the spec carried
over from the `v0.3.1` measurement all reproduced unchanged. Guards are
`return unless <local>.is_a?(String)`; the use is `<local>.frobnicate_zzz`.
Counts are the whole file's diagnostics; `1` means the use fires.

| probe | shape | ref | master | branch |
|---|---|:--:|:--:|:--:|
| `baseline_single_guard` | guard(a), use(a) | 1 | 1 | 1 |
| `double_guard` | guard(a), guard(b), use(a) | 1 | 0 | **1** |
| `unrelated_nonterm_if` | guard(a), `if b; x=1; end`, use(a) | 1 | 0 | **1** |
| `unrelated_nonterm_if_else` | + `else` branch | 1 | 0 | **1** |
| `unless_intervening` / `modifier_if_intervening` | `unless` / modifier spellings | 1 | 0 | **1** |
| `nested_intervening_if` / `two_intervening_ifs` | nesting, repetition | 1 | 0 | **1** |
| `both_branches_terminate` | guard(a), `if b; return; else; return; end`, use(a) | 1 | 0 | **1** |
| `single_terminating_unrelated` | one terminating edge, EMPTY guard map | 1 | 0 | **1** |
| `case_intervening` / `case_in_intervening` | intervening `case` / `case`-`in` | 1 | 0 | **1** |
| `expr_position_ternary` / `expr_position_if` | conditional on an assignment RHS | 1 | 0 | **1** |
| `three_guard_chain` | guard(a), guard(b), guard(b.length), use(a) | 1 | 0 | **1** |
| `chain_intervening_if` | a CHAIN fact across an intervening `if` | 1 | 0 | **1** |
| `x_chain_after_case` | a CHAIN fact across an intervening `case` | 1 | 0 | **1** |
| `nonnarrowing_guard_same_var` | guard(x,Hash), `return if x.key?(:a)`, use(x) | 1 | 0 | **1** |
| `object_counter_reduced` | guard(x,Hash), `return if x.empty?`, use(x) | 1 | 0 | **1** |
| `guard_then_if_then_disjoint_guard` | guard(a,String), `if b`, guard(a,Hash), use(a) | 0 | **1 FP** | **0** |
| `unrelated_if_before_guard` | `if` BEFORE the guard | 1 | 1 | 1 |
| `intervening_method_call` | plain call statement between | 1 | 1 | 1 |
| `use_inside_nonterm_branch` | use(a) inside the branch | 1 | 1 | 1 |
| `guard_then_if_then_subclass_guard` | Numeric, `if b`, Integer, use | 1 `Integer` | 1 | 1 |

FP controls — all measured reference-SILENT, all silent on the branch:
`write_to_a_in_if`, `branch_rebind_one_side`, `rebind_in_else_only`,
`ternary_rebinds_target`, `case_in_rebinds_target`, `chain_root_rebind_in_if`,
`chain_call_on_root_in_branch`, `chain_mutator_on_root_in_branch`,
`own_guard_disjoint_after`, `t_both_terminate_negated`, `bot_intervening_if`,
`chain_caseeq_disjoint`.

Remaining DECLINES (the reference fires, the branch is silent — coverage, never
an FP), each pinned as a `d_*` row in the unit matrix:

| decline | why |
|---|---|
| `d_own_guard_target_after_join` | the conditional's own guard target after a NON-terminating merge; the edges disagree, which is exactly what makes hazard 2 safe |
| `d_t_both_terminate_positive` | the guard's own `if` with both branches terminating and the guard on the TRUTHY edge — no pre-join fact exists, and the propagation declines when both branches terminate |
| `d_case_subject_is_target` | the `case` SUBJECT is excluded from the restore by construction |
| `d_mutator_in_branch` | a MUTATION of the target inside a branch drops the fact (`kill_cenv_narrowed`) |
| `d_use_in_block_after_if`, `d_join_inside_block_use_outside` | a `Narrowed` fact still does not enter a BLOCK body, nor survive a block CALL. This is the block-boundary rule (`n_escape_after_if`, next/break p9/p13), deliberately untouched — the spec assumed facts already enter blocks; they do not (only `ClassFact::Bot` crosses, `lib.rs:3231`) |
| `chain_caseeq_same`, `chain_caseeq_subclass`, `chain_nilq` | a `===` / `nil?` on a CHAIN receiver is not recognised as a chain guard, so the predicate gate declines the restore (see below) |

## Design

Three changes in `crates/rigor-infer/src/lib.rs`.

**1. `retain_joined_facts`.** Called immediately after `join_cenv` in
`class_flow_if` and `class_flow_case` — and BEFORE the early-return propagation,
so a restored fact is what the carried guard map MEETS against. It puts back a
pre-join fact only when EVERY edge still carries it IDENTICALLY.

That one test subsumes the spec's separate criteria, and is the same
identical-value join `join_guards` and `join_flow_envs` already use:

- a REBIND inside a branch removed the fact from that edge's clone;
- the conditional's OWN guard targets moved on at least one edge whenever the
  guard did anything (`if a.is_a?(Hash)` after a `String` guard leaves `Bot` on
  the truthy edge and `String` on the falsey edge), so no sequential-meet or
  `Bot`-collapse result is ever resurrected;
- a call on a chain ROOT inside a branch fired `invalidate_chain_after_call` on
  that edge — invisible to `writes`, caught only here.

On top of it, the span-containment kill (`writes` + the construct's span) covers
the rebinds an edge cannot see: a `case`/`in` clause is not descended.

`join_cenv` itself is UNCHANGED. Its other four callers (`BeginRescue`, `Loop`,
the unmodeled-statement backstop, an expression-position block call) pass NO
edges, and with no edge evidence the blanket clear + span kill is the whole
FP-safety story there. Generalising the retain inside `join_cenv` would have
made the empty-edge case vacuously true and turned those wipes into keeps.

**2. The chain gate (`locals_in_span`).** A conditional's PREDICATE gets no edge
evidence of its own — the edges are clones taken after it ran. A predicate that
narrowed a chain address in a way `analyse_predicate` does not RECOGNISE leaves
both edges agreeing on the stale incoming fact. `guard_predicate` requires a
bare LOCAL operand, so `String === h.last` and `h.last.nil?` are not chain
guards at all — and `return unless Hash === h.last` after a disjoint `is_a?`
guard is reference-SILENT, so restoring there would be a live FP. Any mention of
the ROOT in the predicate therefore declines the restore for every address
rooted at it, mirroring the any-mention widening `kill_chains_rooted_at` already
applies. `class_flow_case` additionally requires that EVERY branch was descended
before enabling the chain half (a `case`/`in` clause is not).

**3. The `else`-carrier unwrap.** Prism models an `else` clause as its own node
and the arena lowers an `ElseNode` to a clause-less `BeginRescue`
(`ast.rs:1457`), so `If.else_body` is `vec![carrier]`. Walking the carrier as an
ordinary statement runs the `BeginRescue` arm, whose own `join_cenv(cenv, &[])`
blanket-wipes the edge's `Narrowed` facts — so EVERY `if` with an `else` lost
the incoming fact on its falsey edge, whatever the else contained. Without this,
`unrelated_nonterm_if_else`, `both_branches_terminate` and both expression-
position rows stay broken. The unwrap is exact: `subsequent` is only ever an
`elsif` (an `If`, untouched) or an `else`.

## Gates

- `cargo build --offline && cargo test --offline` — **all green** (1080 tests;
  `rigor-infer` 254, including the new 42-row
  `class_narrowing_join_retention_matrix`).
- `REFERENCE_RIGOR_DIR=$PWD/reference/rigor ruby harness/run.rb` — **PASS**,
  95 fixtures, 399/434 matched, **0 unregistered extras**, 0 registered.
- `ruby harness/snapshot.rb` + `ruby harness/run_snapshot.rb` — **PASS**,
  fixture 95 written (10 diagnostics), 94 unchanged.
- `cargo clippy --offline --all-targets -- -D warnings`, FRESH
  `CARGO_TARGET_DIR` — **clean**.
- `python3 harness/fp_audit.py --gaps --sweep` (release binary) — **TOTAL FP
  candidates: 0 / 9204 files**, all 8 corpora present.
- `python3 harness/gap_census.py --sweep --dump` vs a master-baseline dump from
  the same reference pin — **841 → 841, 0 rows closed, 0 opened, dumps
  byte-identical**.

## The census row that did not close (spec-vs-reality)

The spec named gitlab-foss `lib/bulk_imports/object_counter.rb:52`
(`Hash#symbolize_keys`) as the one census row this slice closes. It does not,
and the block is ORTHOGONAL to the join.

```ruby
object_counters = Gitlab::Cache::Import::Caching.values_from_hash(counter_key(tracker))

return unless object_counters.is_a?(Hash)
return if object_counters.empty?

empty_response.merge(object_counters.symbolize_keys.transform_values(&:to_i))
```

The reduced shape (`object_counter_reduced`, and the fuller `r5_full` with the
`class << self` nesting and the argument-position use) DOES close — it fires on
the branch and is silent on master. What blocks the real site is the CARRIER: a
local bound from a call whose receiver is an unresolved CONSTANT path is not a
narrowable carrier at all. Measured, with no conditional between the guard and
the use:

| RHS of the binding | ref | branch |
|---|:--:|:--:|
| `k.fetch(:a)` (call on a param) | 1 | 1 |
| `Hash.new` (call on a KNOWN constant) | 1 | 1 |
| `Gitlab.values_from_hash` (unresolved constant) | 1 | **0** |
| `Gitlab::Cache.values_from_hash` (unresolved path) | 1 | **0** |
| `helper_zzz` (bare self-call) | 1 | **0** |

So the row needs an unresolved-constant-receiver carrier slice, not a join fix.
This slice's measured value on the standing sweep is therefore 0 census rows,
and its real value is the FP class (`guard_then_if_then_disjoint_guard`: a
disjoint re-guard after an intervening `if` minted against the wiped env and
witnessed where the reference's meet had already reached `Bot`) plus the
coverage family the reduced probes show.
