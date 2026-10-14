# Issue #363 — break-arg attr mutations no longer propagate past the locals-only join

PR #404, merged `e28498b` (head `5a839ee`). Reviewed by Opus + Grok (both
Approved); two rounds of oracle-derived follow-on fixes. Implemented by a
parallel session; folded here after merge.

## What landed

`break`'s argument scope lands on `join_break_scopes`
(`statement_evaluator.rb:1918`), which unions the break scope's **local
bindings only** — the indexed-narrowing table is not part of the join. An
attr write (`h.default ||= 0`), slot mutation (`h[:a] << 2`) or mutator-call
record drop inside `while c; break …; end` must be invisible to the
continuation: post-loop scope, later in-body reads, and reads inside the
argument itself. The port replayed the mark into the joined env and went
silent — a coverage loss.

- `rigor-parse`: `lexical_loop` counter (`while`/`until`/`for` bodies —
  `eval_for` also joins through `join_break_scopes` — masked inside
  block/lambda bodies where `break` targets the yielding call) plus three
  span tables on `LoweredAst`:
  - `locals_only` — valued `break` spans inside an iterated `while`/`until`
    body (`lexical_loop > 0 && recovery_iterative > 0`);
  - `bare_locals_only` — spans under `for` only (no
    `loop_content_writeback` rescue applies);
  - `iterated_body_spans` — every iterated body, the textual
    `NodeWalker` scan region.
- `rigor-infer`: inside `locals_only` every shape-mutator mark's
  `drop_indexed_mutation` is suppressed **except the `CONTENT_MUTATORS`
  scan set** (content adders + String-only mutators) which the textual
  writeback sees; inside `bare_locals_only` all drops suppressed.
  `DeadPositions::mutation_reaches` extends `write_reaches` for scanned
  mutations in folded arms / join-dropped clauses / nested blocks.
  Valued `next` args get the same treatment paired with their owning
  loop-body span. `scanned_inert` carriers (`END`/`BEGIN`/`super`/`yield`
  under iterated bodies) keep scanned mutation marks through collection.

## Measured

~70-row probe matrix in the PR body: mutator matrices for Hash/Array/String,
`for` (no writeback) vs `while`/`until`, `next` args, nested/shadowed loops,
scanned-inert positions, in-arg vs post-join positional drops — all parity.
Sweep 0 FP; CI 5/5 green.

## Residuals (filed in the PR)

- **#407** — `next h[:a] << 2`: oracle widens the record to
  `"s" | 1 | String`, port drops it (slot widening mechanism).
- **#408** — `break (h[:a] = "s")` record minting inside args; `for`
  operand/index-write minting gaps.
- **#406** — attr writes in a break-*terminated arm* (not the argument):
  per-construct arm-merge rules (`if` merges an exited arm when an else
  exists, `case` never does, loop-body tail always lands).
