# S113 — Pass-4b fold capture (impl, 2026-08-26)

Implements [the mini-spec](20260826-fold-capture-mini-spec.md), the cheap half of
[the AST-eviction probe](20260826-ast-eviction-probe.md) §5.2 step 2. Branch
`claude/s113-fold-capture`, cut from `0cfc68c`. `HarvestedFoldDef` now carries an
**owned mini-tree** of its def's tail; `FoldSite::ast_idx` — the last
slice-POSITION in the identity-hazard family #102/#103 retired — is gone, and so
is the `&[&LoweredAst]` argument to the whole Pass-4b call chain.

**No diagnostic changes anywhere**, under any file order or thread count.

## 1. The STOP condition: the syntax-alone property HELD, end to end

The spec's escape was "if the mini-tree cannot be expressed without reaching
outside `fold_expr`'s 10 variants, STOP". It did not have to fire. Every field
the pre-capture `fold_expr` read is available at harvest time:

| what `fold_expr` read | where it now lives |
|---|---|
| the seven literal payloads (`value` / the bare tag) | `FoldExpr::Scalar(Scalar)` — converted at harvest |
| `Call{receiver: None}.method`, `block_body.is_empty()` | `FoldExpr::SelfCall { method }` |
| `method == "!"`, `args.is_empty()` | `FoldExpr::Not { operand }` |
| `ast.get(r)` is `ConstantRead{name}`, `!name.is_empty()`, `args.is_empty()` | `FoldExpr::ConstCall { owner, method }` |
| `method`, `args`, the receiver `r` | `FoldExpr::CoreCall { recv, method, args }` |
| every other tag ⇒ `_ => None` | `FoldTail::Decline` |

The reads that are NOT syntactic — `resolve_instance_owner` (the merged override
ancestry), `resolve_fold_key` (the cross-file def table), the overridable
degrade, `folding::fold` — are exactly what survives as an unresolved node for
the merge to apply. So the split is total: `capture_fold_tail` is a pure function
of one AST node, and `SourceIndex::fold_tail` is a pure function of a mini-tree
plus merged state. No subset argument was needed and none was made.

**One thing NOT reached that the probe's table implies is reachable.** `fold_expr`
stripped a leading `::` off a `ConstantRead` receiver name
(`name.strip_prefix("::")`), and it guarded against an EMPTY name. Both branches
are **dead in the lowering**: `constant_path_string`
(`crates/rigor-parse/src/ast.rs:2169-2194`) renders `::Foo` as `"Foo"` and
`::Foo::Bar` as `"Foo::Bar"`, never with a prefix, and every path it can build is
non-empty (probed over seven spellings incl. `::X`, `self.class::X.y`,
`Foo.bar::Baz.qux`). Both are carried into the capture VERBATIM anyway — a
bit-identity slice is the wrong place to delete a branch — and both show up
below as mutations no test kills, which is the honest reading: the branch is
unreachable, not the harness thin. Recorded so a future reader does not mistake
the un-killed mutations for a coverage gap.

## 2. What changed (all in `crates/rigor-infer/src/source_index.rs`)

```rust
enum FoldTail { Decline, Expr(Box<FoldExpr>) }          // 8 bytes, niche-packed
enum FoldExpr { Scalar(Scalar), SelfCall{..}, ConstCall{..},
                Not{ operand: FoldTail },
                CoreCall{ recv: FoldTail, method, args: Vec<FoldTail> } }
```

* `HarvestedFoldDef.tail: NodeId` → `tail: FoldTail`, built by the new
  `capture_fold_tail(ast, tail, 0)` inside `walk_fold_defs` — i.e. at harvest
  time, in stage 1's rayon closure, where the AST is already hot.
* `FoldSite` lost `ast_idx` and became `FoldSite<'a> { tail: &'a FoldTail,
  has_explicit_return }`; the merge's `defs` map (`type FoldDefs<'a>`) BORROWS
  each site's mini-tree from its `Harvest` — nothing is cloned into the merge.
* `compute_literal_returns` / `resolve_fold_key` / `fold_key_sites` all lost
  their `asts: &[&LoweredAst]` parameter. `fold_expr` was replaced by `fold_tail`,
  which has no `&LoweredAst`, no `&[&LoweredAst]` and **no `depth`**.
* M3 still materialises `asts` — for Pass 3 alone. Pass 4b reads no AST at all.

**`FOLD_DEPTH_CAP` moved, deliberately, and is now applied exactly once.**
`fold_expr` checked `depth > 16` on entry and `fold_key_sites` always entered at
0, so nodes at depth `0..=16` were evaluated and a node at depth 17 declined
unexamined. `capture_fold_tail` cuts at exactly that boundary and `fold_tail`
carries no depth. Keeping a second check downstream would have MASKED a
capture that truncated one level too deep; with one check, both directions of
off-by-one are observable — and the mutation table below shows they are.

The cap still resets per SITE, because each `HarvestedFoldDef`'s mini-tree is
rooted at its own tail with depth 0 — the same reset `fold_key_sites` performed
by always passing 0. `fold_depth_cap_resets_per_site` pins it (a 16-deep tail
calling a 16-deep method still folds).

## 3. Parity evidence

### 3.1 The oracle

`probes_s92` now keeps the **whole pre-capture fold path verbatim** under
`#[cfg(test)]`: `LegacyFoldSite { ast_idx, tail: NodeId, .. }`,
`legacy_compute_literal_returns`, `legacy_resolve_fold_key`,
`legacy_fold_key_sites`, `legacy_fold_expr` — the AST-walking recursion with its
`depth` counter, unchanged except for being free functions over `&SourceIndex`.
`build_project_legacy`'s Pass 4 calls them, so `assert_paths_agree` grades the
capture against an INDEPENDENT copy of the old fold rather than a rename of
itself. The shared primitives it calls (`resolve_instance_owner`,
`overridden_in_project`, `folding::fold`) are the production ones on purpose: the
capture did not touch them, and the claim under test is that the mini-tree
reaches the same calls with the same arguments.

### 3.2 New tests (rigor-infer 279 → 284)

| test | what it grades |
|---|---|
| `merge_equals_legacy_over_the_fold_corpus` | the capture vs the verbatim oracle over a fold-RICH 4-file corpus, forward, reversed, and under **all 24** permutations |
| `fold_capture_is_non_vacuous` | 14 kept shapes fold to their exact scalars; 11 pruned shapes have NO entry; `literal_returns.len() == 14` |
| `fold_depth_cap_boundary_agrees_with_the_oracle` | two depth axes (`!`-chain and arg-nested `+`-chain) at n = 0/1/14/15/16/17/18/20, graded against the oracle |
| `fold_depth_cap_fires_at_seventeen` | the 16-in / 17-out step ASSERTED with values, so agreement-on-nothing cannot pass it |
| `fold_depth_cap_resets_per_site` | 32 syntactic levels fold across a call boundary |

The existing six `merge_equals_legacy_*` corpora fold almost nothing, which is
why the fold corpus and the non-vacuity floor exist: without them the equivalence
tests would pass a capture that silently declined everything.

### 3.3 The `FOLD_DEPTH_CAP` boundary harness (spec evidence item 2)

The cap had **zero coverage** before this slice — it has never been observed to
fire on any corpus, and the capture makes it load-bearing. Two structurally
different axes, because the recursion reaches depth two different ways:

* `!`-chain — `def m; !!!…!true; end`. `n` bangs put the literal at depth `n`;
  the recursion is through `Not`'s single operand.
* `+`-chain — `def m; 1 + (1 + (… + 1)); end`. The recursion is through
  `CoreCall`'s ARGS, and every level also fans out (a receiver AND an arg).

Both agree with the oracle at every n tested, and both step at the same place:
n ≤ 16 folds, n ≥ 17 declines.

### 3.4 Mutation table (the #92/#94/#108 standard)

Each mutation applied to `capture_fold_tail` alone, `cargo test -p rigor-infer`
run, then reverted.

| # | mutation | tests failed |
|---|---|---|
| M1 | prune one variant too many — drop the `SymbolLit` arm | **2** — `fold_capture_is_non_vacuous`, `merge_equals_legacy_over_the_fold_corpus` |
| M2 | prune one variant too many — drop `ConstCall` detection | **2** — same two |
| M3 | prune one variant too many — drop the implicit-self arm | **4** — those two + `fold_depth_cap_resets_per_site` + `tests::depth_two_bang_of_singleton_call_folds` |
| M4 | off-by-one depth, one too SHALLOW (`>` → `>=`) | **3** — `fold_depth_cap_boundary_agrees_with_the_oracle`, `fold_depth_cap_fires_at_seventeen`, `fold_depth_cap_resets_per_site` |
| M5 | off-by-one depth, one too DEEP (`> CAP + 1`) | **2** — the two cap tests |
| M6 | args captured at `depth` instead of `depth + 1` | **2** — the two cap tests |
| M7 | capture one variant too MANY — keep block-bearing implicit-self calls | **2** — `fold_capture_is_non_vacuous`, `merge_equals_legacy_over_the_fold_corpus` |
| M8 | capture one variant too MANY — keep block-bearing receiver calls | **2** — same two |
| M9 | drop the `::` strip on a const receiver | **0** — the mutated branch is UNREACHABLE (§1) |
| M10 | drop the empty-const-name guard | **0** — likewise unreachable (§1) |

M7 initially killed **nothing**: the corpus's only block-bearing call was
receiver-bearing (`[1].map { |x| x }`), so the receiverless guard had no
discriminator. `blocky_self` (`flag { 1 }`) and `blocky_const`
(`Flags.label { 1 }`) were added for it — a real harness gap the mutation found,
and the over-capture direction is the FP direction, so it mattered.

## 4. Harvest-size delta

The mini-trees are new bytes in every harvest and the LSP holds harvests, so this
was measured, not estimated. Instrument: a temporary `#[cfg(test)]` deep-size
walk over `SourceIndex::harvest` of every file in a corpus (added, measured,
**removed before commit** — reproduce by re-adding a walk that sums
`size_of` + `String::len` + element counts). The BASE is approximate the same way
on both sides (it under-counts `HashMap` capacity), so the DELTA is the exact
part.

| | mastodon/app (1,236 f) | gitlab-foss/lib (4,676 f) |
|---|---|---|
| fold defs | 6,483 (5.2/file) | 22,159 (4.7/file) |
| of which `Decline` | 3,164 (**48.8 %**) | 11,663 (**52.6 %**) |
| captured nodes | 7,668 (1.18/def) | 20,625 (0.93/def) |
| mini-tree heap | 520,171 B (420 B/file) | 1,406,384 B (300 B/file) |
| inline delta (`HarvestedFoldDef` 56 → 64 B) | 51,864 B (42 B/file) | 177,272 B (38 B/file) |
| harvest, pre-capture | 2,436 B/file | 3,154 B/file |
| harvest, post-capture | 2,899 B/file | 3,493 B/file |
| **delta** | **+462 B/file (+19.0 %)** | **+338 B/file (+10.7 %)** |

`size_of::<FoldTail>() == 8` (the `Box` null niche carries `Decline`), so a
declining def costs 8 inline bytes and **no heap node at all**; `FoldExpr` is 56.

**Is it large enough to matter? No — and the honest framing is that it is small
against the term it lands in, not that it is zero.** At the LSP's 4,675-file
steady state the delta is 338 B × 4,675 ≈ **1.6 MB**, against the probe's
measured held-`Harvest` term of 18.9 MB and a 428.3 MB total: **+8 % of the
harvest term, +0.4 % of steady state.** The held-AST term the line is ultimately
aimed at is 207–241 MB, i.e. 130–150× the cost of this capture. It does not
change any verdict: #104's harvest cache stays NO-GO (a 10.7 % bigger harvest
makes its cache-read cost slightly worse, which only deepens the existing
negative), and ADR-0029's budget is untouched.

**Two corrections to the probe's estimate, worth recording.** It predicted "for
most methods the capture is that one byte" — measured, only **~50 %** of def
tails decline syntactically, not the "overwhelming majority". The other half are
calls whose foldability is a MERGED question (`SelfCall`/`CoreCall` roots that
mostly decline later, at fold time), which the capture cannot decide and must
keep. Its size estimate ("plausibly a few hundred bytes per file") was right —
300–420 B/file — but for a different reason than it gave.

An available trim, not taken: a def with `has_explicit_return` declines in
`fold_key_sites` before its tail is read, so its mini-tree could be skipped. That
is a provably-inert conditional, and this slice's whole value is that it added
none.

## 5. Gate verdicts (BARE, in the spec's order)

| # | gate | verdict |
|---|---|---|
| 1 | `cargo test --workspace` | **PASS** — 447 / 4 / 3 / 15 / 58 / 24 / 94 / 284 / 47 / 251 / 48, **0 failed**. Only `rigor-infer` moves: 279 → 284 (the five new tests) |
| 2 | `cargo clippy --workspace --all-targets -- -D warnings`, fresh `CARGO_TARGET_DIR` | **PASS** — clean, exit 0 |
| 3 | release rebuild + `PYTHONHASHSEED=0 harness/fp_audit.py --gaps --sweep` | **PASS** — **0 FP across 9,204 files** (8 corpora, 0 absent), and the gap set is **byte-identical** to the branch-point baseline: all 67 lines, every corpus's reference / rigor-rs / matched / FP / gap count and every per-rule total unchanged. Only the build stamp and wall-clock timings differ. The staleness guard ran and the sweep measured this tree |
| 4 | `rigor check` vs the master baseline binary | **PASS** — `mastodon/app` (420 findings) and `gitlab-foss/lib` (1,093 findings): stdout + stderr + exit byte-identical under default threads AND `RAYON_NUM_THREADS=1`, and the branch's own two thread modes agree with each other (ADR-0020) |
| 5 | `harness/run_snapshot.rb` | **PASS** — 98 fixtures, 407 matched / 443 reference, 35 gaps, 2 registered divergences, **0 unregistered** (the pre-change numbers exactly) |
| — | `harness/docs_check.py` | **PASS** — 4 budgets, links resolve |
| — | `cargo doc --no-deps --document-private-items` | no NEW unresolved intra-doc links (the four that remain are pre-existing, verified by re-running on a stashed tree) |

The sweep baseline was taken at the branch point with a master binary saved
aside, `PYTHONHASHSEED=0` on both runs, against a `reference/rigor` submodule
populated in this worktree at the pin `b10bd5df` (v0.3.4) — never
`REFERENCE_RIGOR_DIR`, never a different checkout.

## 6. Deviations from the spec

1. **`fold_tail` keeps `#[allow(clippy::too_many_arguments)]`.** It is 8
   arguments (down from `fold_expr`'s 11), still over clippy's 7. Bundling
   `defs`/`memo`/`visiting`/`closures` into a context struct is a real
   refactor and does not belong in a bit-identity slice.
2. **The fold corpus grew two methods mid-slice** (`blocky_self`,
   `blocky_const`) because mutation M7 killed nothing without them (§3.4). The
   spec asked for mutation testing; this is what it found.
3. **The harvest-size instrument was temporary and is not in the diff.** The
   numbers in §4 are reported, the walker is not shipped — same discipline as
   the #92 probe's per-pass timing instrumentation.

## 7. Not done (spec non-goals, unchanged)

Pass 3 still reads every file's own tree at merge time, so **AST eviction stays
blocked** and the LSP's held table cannot shrink — this slice's value is the
retired position hazard and the AST-free Pass 4b, exactly as
[the mini-spec](20260826-fold-capture-mini-spec.md) framed it. No Pass-3
sub-arena, no `lsp.rs` change, no `harness/` change, no reference work, and no
change to what `compute_literal_returns` concludes.
