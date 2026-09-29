# 2026-09-29 — issue #194: witness rendering (quoted symbols, bigints, block folds)

**PR #291, merge `6544527`.** Closes #194. Residuals filed: #292 (decoded-vs-raw symbol escape bytes), #293 (fold coverage gaps — `&:sym`, ranges, destructured params), #294 (mutation-written join arms).

## What landed

- `Scalar::BigInt(String)` keeps the exact decimal spelling of integer literals past `i64` — witness `[99999999999999999999]` instead of `[Integer]`; maps to `Integer` for class/dispatch; arithmetic folds decline; sidecar pad-width args are gated (Bignum width → decline before the Ruby sidecar can allocate).
- `symbol_inspect`/`bare_symname` mirror `Symbol#inspect`/`rb_str_symname_p`: `:"a b"` quotes; `:$` tails are bare only for `is_special_global_name` forms (one punct byte, `-$x` with one identchar, all-digit tail) or plain global identifiers; `?`/`!`/`=` sigils for locals/constants only; identchars are ASCII-only on the pinned toolchain (`:"café"` quotes).
- `ruby_float_to_s` reproduces CRuby `flo_to_s`'s scientific switch (E ≤ -5 or ≥ 16, integral corner at E == 15), signed ≥2-digit exponents; `-0.0`/`NaN`/infinities preserved. Ported from the abandoned #167 branch.
- `per_element_block_fold`: literal `Tuple` receivers fold a simple block body once per element for `map`/`collect`/`filter_map`/`flat_map`/`select`/`filter`/`reject`/`find`/`detect`/`find_index`/`index` — the reference's `PER_ELEMENT_TUPLE_METHODS` subset. Declines: `&:sym`, explicit args, jumps, non-single-positional params, and (after FINAL review) every write/mutation the flat per-position env overlay cannot replay.
- `Node::UnmodeledWrite`: span-only sibling marker for write forms the lowering cannot reproduce (`@x += 1`, `K::V = v`, `a[i] ||= v`, `x.f &&= v`, `expr => pat`/`in` binds); the old `Statements{Recovered}`/`Inert` carriers are preserved so every other consumer is unchanged.

## Safety shape (FINAL review, 3 rounds)

- r1 caught: op-writes (`x += 1`) minted stale entry constants (`first.even?` → `false` vs ref `true` — a real FP vector); param mutation (`x << 3`) answered pre-mutation tuples; `$`-grammar over-accepted printable tails.
- Fix: the fold gate (`fold_body_has_unmodelled_write`) declines on any write the overlay can't replay — op-writes, multiwrites, loops, ivar/cvar/gvar/const writes, `UnmodeledWrite`, `rescue => e`, non-direct/nested writes, and mutator/setter calls whose receiver contains a binding read (`x[i] << 3`, `x.f = v`).
- r2 flagged `yield`/`super` inside folded blocks; oracle probes showed the reference never enters `YieldNode`/`SuperNode` (`yield (x = 9); x` → `[1, 2]` on both) — declining would have *created* drift. Contents of `Statements{Inert}` carriers (`yield`/`super`/`defined?`/`BEGIN`/`END`) are exempt from the gate — parity-exact.
- r3: `【MERGE_APPROVED】`. PRIMARY `[PASS_PRIMARY]` (r2, sidecar BigInt gate fix `e6d414b`). Reviewers: Codex `gpt-5.6-sol` throughout (OpenCode ACP dead at initialize ×4, Grok 402 quota).

## Measured

- Brief rows byte-identical: `[:"a b"]`, `[99999999999999999999]`, `[1,2].map{|x| x+1}` → `[2, 3]`; floats `1e-5`→`1.0e-05`, `1e16`→`1.0e+16`, `-0.0`→`-0.0`.
- 60+ `Symbol#inspect` rows match CRuby exactly (incl. `:$00`→quoted, `:$0`→bare, `:$-!`→quoted).
- Sweep: **0 FP** / 818 gaps over 8 corpora (unchanged vs #168 head). CI green on `698e46f` (ubuntu+macos, snapshot parity, live reference, docs).
