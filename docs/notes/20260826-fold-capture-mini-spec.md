# Pass-4b fold capture mini-spec (2026-08-26)

The AST-eviction line, SPLIT by evidence. Two halves with very different
risk, and this spec takes only the cheap one.

## The decision

[The probe](20260826-ast-eviction-probe.md) + [the f measurement](20260826-ast-eviction-f-measurement.md):

- **Pass 4b (`compute_literal_returns`) — SPECIFY (this note).** Bounded to
  10 of 37 `Node` variants, depth-capped at 16 (reset per key), and — the
  rare property that makes this cheap — **every decline is decided by
  SYNTAX alone** (tag, `args.is_empty()`, `block_body.is_empty()`,
  `name.is_empty()`). A harvest-time prune is therefore exact **by
  construction**: no subset argument, which matters because that argument
  has failed four times in this project. Standalone value even if eviction
  never happens: it removes `FoldSite::ast_idx` (a slice position — the
  last member of the identity-hazard family #102/#103 retired) and drops
  `&[&LoweredAst]` from the fold.
- **Pass 3's sub-arena — DEFERRED, with its trigger and its number on
  record.** f = 0.214 aggregate (over-counted, so true f ≤ that) says a
  capture IS viable, and 33% of files are fully droppable. But `Node` is a
  fixed-size ~240B enum in a `Vec`, so a capture must COMPACT and RENUMBER,
  and a renumbering is a position change the fixture corpus structurally
  cannot catch. Pay that only when memory actually binds: LSP steady state
  is 428MB at 4,675 files against ADR-0029's 600MB (71%), and the cheapest
  dial is the cross-file cache cap (8 → fewer frees ~101MB, one constant,
  no parity surface) — deliberately unspent because it trades cross-file
  hover on open tabs for headroom we currently have. **Trigger**: a project
  scale or a budget change that makes 600MB bind. Do NOT re-measure f.

## Scope

`HarvestedFoldDef` gains an owned mini-tree carrying exactly what
`fold_expr` can reach from that def's tail: the seven scalar literals,
`Call{receiver: None}` and `Call{receiver: Some(_)}` (both only while
`block_body.is_empty()`), and `ConstantRead` reachable as an args-empty
receiver. Everything `fold_expr` declines becomes a one-byte `Decline` at
harvest time. `FoldSite::ast_idx` and the merge's `asts` argument to the
fold go away; `resolve_fold_key` keeps resolving into `defs`, which is
already harvest-built.

**Not in scope**: Pass 3, any AST eviction, any change to what
`compute_literal_returns` CONCLUDES, the LSP.

## Parity evidence (this is a bit-identity refactor, not a behaviour slice)

1. Extend the `probes_s92` equivalence harness to cover the fold: the
   pre-capture path kept verbatim under `#[cfg(test)]` as the oracle,
   compared field by field over the existing probe corpora, forward and
   reversed, all permutations. Mutation-test it — the #92/#94/#108
   standard: at least prune-one-variant-too-many and off-by-one-depth must
   each fail a test that claims that property.
2. **A `FOLD_DEPTH_CAP` boundary harness.** The cap has never been observed
   to fire on any corpus, so it has no coverage today; the capture makes it
   load-bearing (a mini-tree truncated at the wrong depth changes a fold).
   Synthetic chains at depth 15/16/17 must agree pre- and post-capture.
3. `harness/fp_audit.py --gaps --sweep` — 0 FP / 9,204 with the gap set
   **byte-unchanged**. Mandatory and non-negotiable: a capture is a
   position change, and `fixture-corpus-blind-spot` says fixtures cannot
   contain those.
4. `rigor check` byte-identical vs a master binary on mastodon/app under
   both thread modes; `harness/run_snapshot.rb` PASS 0 unregistered.
5. Report the harvest-size delta (the mini-trees are new bytes in every
   harvest — if they are large enough to matter to the LSP's held table,
   say so; that would be an argument against the slice, not a detail).

## Gates (BARE)

`cargo test --workspace`; clippy fresh `CARGO_TARGET_DIR` `-D warnings`;
then 2–4 above; `harness/docs_check.py`.

## Stop condition

If the mini-tree turns out NOT to be expressible without reaching outside
`fold_expr`'s 10 variants — i.e. if the "syntax alone" property does not
hold end to end — STOP and report. The entire cheapness of this slice
rests on that property; without it this becomes Pass 3's problem and
belongs behind the same deferral.
