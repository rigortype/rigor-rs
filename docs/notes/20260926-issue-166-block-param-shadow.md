# Block/lambda-bound names shadow toplevel locals (issue #166)

Closes [#166](https://github.com/rigortype/rigor-rs/issues/166), merged as PR
[#174](https://github.com/rigortype/rigor-rs/pull/174) (`07dec0c`, merge
`e31ed97`). A regression introduced by the #133/`#148` binder-writes work: the
toplevel-locals table treated every `x = …` at file scope as a rebind of the
outer `x`, even when `x` is bound by the enclosing block/lambda parameter
list. `x = 1; [1].each { |x| x = "s" }; x.first` produced a wrong
`for "s"`/`nil` env where the reference keeps `x` at `Integer` and fires
`for 1`.

## The fix

Prism already computes the exact bound set: `BlockNode#locals` /
`LambdaNode#locals` cover every parameter form (required, optional, splat,
post, destructured, keyword, kw-rest, `&`, block-locals after `;`, numbered
params, `it`). `Node::Block`/`Node::Lambda` expose `locals`, and
`toplevel_rebinds` skips a write whose name is bound by a body that
structurally contains it. Writes captured from the OUTER scope stay real
rebinds — the port still declines where it lacks the reference's join, so the
conservative controls are unchanged.

## What the dual review changed

Opus 5.5's first pass (high) found a real FP in the span-containment version:
`foo(<<~X) do |w|` puts a `#{w = 2}` in the heredoc body *lexically inside*
the block's span while Prism parents it to the heredoc argument — outside the
block body. The write was shadowed, turning a master silence into `for "s"`
where the reference fires `for 2`. The fix is structural membership, not
spans: `descendants_of` walks `node_child_ids` over the arena from each
block/lambda body root; orphan arena nodes (unreachable from any root) cannot
shadow. Grok 4.6 (high) approved the structural version and flagged two stale
comments, corrected in `07dec0c`.

Fixture `116_block_param_shadow.rb` (207 lines): every parameter form,
captured-outer controls, nested blocks, heredoc/xstring interpolation
controls.

## Measured

- 117 fixtures, 651 matched, 57 expected gaps, 0 unregistered FP.
- `cargo test --workspace --locked`, clippy 1.88 `-D warnings` (fresh
  target), `run.rb` + `run_snapshot.rb`: all exit 0.
- `fp_audit --gaps --sweep`: 0 FP over the standing 8 corpora.
- CI: all five checks green on `07dec0c`.
