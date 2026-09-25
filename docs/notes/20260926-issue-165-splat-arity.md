# `call.wrong-arity` declines on any non-plain-positional argument (issue #165)

Closes [#165](https://github.com/rigortype/rigor-rs/issues/165), merged as PR
[#175](https://github.com/rigortype/rigor-rs/pull/175) (`ceadce2`). Split out
of #153 via the #148 review.

The reference's `wrong_arity_diagnostic` is gated by `plain_positional_call?`
(`check_rules.rb:1680`): every entry of `call_node.arguments.arguments` must be
`simple_positional?` — no `SplatNode`, `KeywordHashNode`, `BlockArgumentNode`
or `ForwardingArgumentsNode`. The port computed that test already (as
`args_all_plain`, for argument-type-mismatch) but `check_wrong_arity` never
read it: it counted `args.len()`, so any splat/kwarg/`...` past the first was
witnessed as one positional. `[1, 2].first(*[5], *[5])` fired `given 2` where
the reference is silent.

## The fix

`Node::Call` gains `args_plain_positional`, computed over `call.arguments()`
exactly as the reference reads `call_node.arguments.arguments`.
`args_all_plain` is now `args_plain_positional && !block_is_pass` — the same
value as before, so ATM and `dead_version_guard` are unchanged. The split
matters: Prism puts `&blk` in `block()`, never `arguments()`, so the oracle
still arity-checks `first(1, 2, &)` and `def m(&) = first(1, 2, &)` (the
`BlockArgumentNode` arm of `simple_positional?` is unreachable at this pin but
is mirrored for faithfulness). ATM keeps its extra `block_is_pass` decline.

Fixture `116_splat_arg_arity.rb` shows 11 unregistered FPs on the master
binary, 0 after.

## What the dual review changed

Grok 4.6 and Opus 5.5 (both `high`, independent) returned **Approved** after
re-probing every table row plus ~30 extra shapes (interpolated/multibyte
receivers, `&.`, `super`, no-paren calls, braced `{a: 1}` staying positional,
splat+kwarg mixes, `def m(*)`/`m(**)`/`m(...)` forwarding). Two stale comments
in the diff were corrected (`ceadce2`):

- the `has_block` deferral comment claimed the reference reads the block
  overload's own arity — in fact its `compute_arity_envelope` is the same
  min/max collapse and it FIRES on `map(1) { |z| z }` (`given 1, expected 0`);
- `args_all_plain`'s comment claimed oracle silence on block-pass — the real
  divergence is `fetch("x", &b)` / anonymous `&`, which fire
  `call.argument-type-mismatch` on the reference and stay silent here.

Both are pre-existing safe-side gaps →
[#176](https://github.com/rigortype/rigor-rs/issues/176). The brief's
acceptance criterion 3 was also written expecting a block-pass decline that
the oracle does not make; noted on the PR.

## Measured

- `cargo test --workspace --locked`, clippy 1.88 `-D warnings` (fresh target),
  `run.rb` + `run_snapshot.rb`: all exit 0; 0 unregistered FP, 49 expected
  gaps, 630/680 = 92.6%.
- `fp_audit --gaps --sweep`: 0 FP over the standing 8 corpora.
- CI: all five checks green on the merged head.
