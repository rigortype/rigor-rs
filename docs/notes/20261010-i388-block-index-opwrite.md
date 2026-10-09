# #388 — in-block `h[k] op= v` left a stale indexed record past the block

PR #391, merged `2fb2950` (squash of `fd7e07d`).

## Symptom

```ruby
h = {a: 1}
h[:a] ||= "s"
[1].each { h[:a] ||= "t" }
h[:a].frobnicate          # master: FP `"s" | 1`; oracle: silent
```

Compound index writes (`||=`, `&&=`, `+=`, …) land in
`IndexedFlow.slot_writes` and replay through `apply_mutation_effects`
with `drop_key == None` — the code path deliberately dropped nothing
("compound writes never reach `invalidate_indexed_write`"). Inside a
deferred/iterated body the stale outer record then survived into the
post-body read.

## Oracle mechanism

The reference treats literal block/lambda/proc bodies and `while`/`until`
bodies as **content write-back** positions (`content_writeback_block_captures`,
ADR-56 slice C): a content mutation of `h` inside the body rebinds `h`
for the continuation, so every indexed narrowing rooted at `h` dies.
Joined/conditional arms (`if`, `case`/`when`/`in`, `rescue`/`else`/`ensure`,
`&&`/`||`/`?:`, `while` predicates, **`for` bodies**) instead keep the
record through the join — both engines still fire there, with the
disclosed `"s"|"t"|1` vs `"s"|1` message drift.

## Fix

`path_content_writeback` walks the `flow_children` path from the flow
root to the mutation and reports whether it crosses a Barrier body edge
or a `while`/`until` body. `Node::Loop` carries no `for`/`while` kind, so
`while` is inferred from `index.is_empty() && index_writes.is_empty()`.
In `apply_mutation_effects`, a `[]=` mutation at a write-back position
calls `drop_indexed_narrowings` for the receiver — **all** rooted keys,
per the oracle (a `h[:b] = "t"` body write also kills the `h[:a]`
record). Everything else keeps `drop_indexed_mutation` unchanged.

## Measured outcome

- Headline + `&&=`/`+=`/plain-`=`/nested-block/`loop do`/lambda/proc/
  `while`/`until`/cross-key rows all silent on both engines.
- Joined controls still fire on both (`if`/`case`/`when`/`rescue`/`&&`/
  `?:`/`for`/`while`-predicate) — message drift only, pre-existing.
- `fp_audit --gaps --sweep` on the final head: **0 FP**, 9,337 files,
  815 gaps (unchanged).
- Reviewers: Opus 5.5 Approved, Grok 4.6 Approved.

## Accepted coverage losses (filed / disclosed)

- **In-body read after a same-body write**: the write-back drop lands in
  `flow.env` (the end-of-body env a diagnostic inside the same body also
  descends into), so `each { h[:a] ||= "t"; h[:a].frobnicate }` goes
  silent where the oracle fires `"s"|"t"|1`. Site-aware invalidation is
  the follow-up.
- **`for` with a binding-less target** (`for @a`, `for A`, `for ::A`,
  `for $g`, `for a.b`, `for *w`, `for *`): inferred as `while`, coverage
  loss — filed #392 (needs an explicit loop-kind on `Node::Loop`).
- **`retry` operand leaking its indexed write** — pre-existing on
  master, verified; filed #393.

## Tests

`block_index_opwrite_drops_indexed_narrowing` in
`crates/rigor-cli/tests/check.rs` — 11 silent rows (`||=`/`&&=`/`+=`/`=`,
nested blocks, `while`/`until`/`loop do`, lambda, cross-key) plus joined
controls (`if`/`case`/`rescue`/`for`/`while`-predicate) asserting fire.
