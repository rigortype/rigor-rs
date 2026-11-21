# Mutation-content args on widened carriers — the #771 file-scope half (2026-11-21)

Issue #771: the widened `Hash`/`Array` binding renders its joined type
parameters on the reference and a bare nominal on the port — same
`(rule, line, col)` key, different message. PR #776 ports the
content-evidence half of `MutationWidening.widen_for_mutator`
(`mutation_widening.rb`, `content_join.rb`, `mutation_rejoin.rb`,
`rewrite_mutation.rb`) as `crates/rigor-infer/src/mutation_content.rs`:
the seed mints its content args (`Array[1]`, `Hash[Symbol, 1]`), the
content-adder joins grow them with the call's typed `arg_types` plus the
`gradual_floor` `Dynamic[top]` arm, a `Nominal` re-joins
(`MutationRejoin.widen_nominal`), and `widen_value_pinned`'s union arm
takes the reference's verbatim `union(*…)`.

## Measured outcome

- 23 probe rows byte-identical (full tuples: the issue's `||=`/`=` rows,
  push/`<<`/`store`/`concat`/`insert`/splice/range/unknown-key/literal-send,
  rejoin chains, rewrite mutators `map!`/`fill`/`merge!`/`transform_values!`,
  alias unknown stores, the String arm) + the #309 two-distinct-arms
  silence control.
- Adversarial review (substitute reviewer; Opus and the Fable companion are
  quota-blocked here): ~100 added counterexamples (splats, kwargs, blocks,
  `&.`, i64-past literals, multi-byte sources, alias chains, union
  carriers, haml project `sig/`) — parity or pre-existing declines, zero
  port FPs. Approved on 8c0ed240.
- `fp_audit --gaps --sweep`: **0 FP / 437 gaps, identical to master** —
  message accuracy moved, coverage did not (as designed).
- Time: the pathological `rufo/formatter.rb` checks in 0.12 s; the sweep
  wall stayed in band once my concurrent cargo builds stopped.

## The structural discovery: FOUR port seams where the reference has one

The reference has ONE `widen_for_mutator`; the port's collection-mutation
widening lives in **four** places, and only the first is now
content-faithful. Probing `#771`-shaped rows through each seam is the
fastest way to find the remaining drift:

1. **File-scope flat env** (`flow_eval.rs`'s `widen_mutated_binding`) —
   fixed by #776.
2. **Def bodies** — `check_collection_call` renders from `CollSnap.class`
   and re-mints a BARE nominal for widened carriers; the coll pass's own
   mint (`coll_grown_carrier`) approximates the join (erased classes, a
   `top` marker, no rewrite table). **#781.**
3. **Blocks** — `coll_block_mutations` mints bare nominals, no slice-C
   join. **#777.**
4. **Loops** — the post-loop union declines where the reference's loop
   writeback joins without a floor. **#782.**

Beside them, the review filed #783 (possible-nil on collection mutators),
#784 (`widen_precise_array`/`:synthesised` decline), #785 (def-return
collection channel), and the probing session filed #778 (`delete` pair
removal / RemoverNarrowing), #779 (`SetMutation`), #780 (constructor
default pinning).

## The gate lesson

`gate.sh` runs `cargo test -p` only for CRATES THE BRANCH CHANGED; CI runs
the workspace. This branch changed rigor-infer, but a rigor-RULES test
pinned the port's old drifted message (`for Hash`) — CI caught it that
gate.sh could not see. **A message-changing PR must run
`cargo test --workspace` locally before push.**

## Issue state

#771 stays OPEN (the PR says `Refs`): the issue's phenomenon is the drift
CLASS, and the def-body half (#781) is the largest surviving piece.
