# rigor-parse ast.rs module split — handoff spec

2026-09-28. `crates/rigor-parse/src/ast.rs` is **3,756 lines** at `origin/master`
`4753cf3` (unchanged since `d373915`, the test move of PR #230). Its unit tests
already live in `src/ast/tests.rs`. This spec splits the production code into
child modules of `ast`. It is a **move-only refactor**: no behaviour change, no
renames, and visibility changes only where rustc asks. The review checks "same
code, new files", not parity.

**Precedents:** #204 (`20260927-lib-rs-split-outcome.md`) and #234
(`20260928-rules-lib-split-spec.md`, outcome `20260928-rules-lib-split-outcome.md`).
The tooling is `harness/split/`.

**Why now:** `ast.rs` has 59 commits, 28 of them since 2026-08-01. Every Node or
lowering change edits it. No open PR touches `crates/rigor-parse/`. The two open
PRs, #251 and #254, are both in rigor-rules.

**Execution shape:** one prep PR (P0), then nine move-only PRs, leaf-first.
File an umbrella issue with this spec as the agent brief, as #204 and #234 did.

**Every number below was measured, not predicted.** The whole plan, P0 plus the
nine steps, was run in a scratch copy of `4bec095` with the real tools, four
times. Runs 1–3 checked the order and the doc links. Run 4 is the plan as
written here, and its numbers are the ones quoted. At every step:
- `verify_move` found nothing UNEXPECTED;
- the tests stayed at 59, and the rustdoc warnings stayed at 3;
- the 25 public pages did not change;
- clippy passed;
- rigor-infer, rigor-rules and rigor-cli built.

At the end state, clippy 1.88 passed and the rigor-parse tests (59) passed.
The rigor-infer (322) and rigor-rules (270) tests also passed, measured on the
run-2 end state, which differs from the final one only in doc lines and in the
order of the `mod` lines.

Line numbers are at `4753cf3`. P0 shifts them by a few lines. Selectors are by
name, so the shift does not matter to the tools.

## 1. File shape (verified against `4753cf3`)

**Header, lines 1–30:**
- `//!` docs (1–28);
- the file's only top-level `use`, on line 30:
  `use crate::ruby_prism::{self, Node as PrismNode, ParseResult};`.

There is no `#![…]` here (`#![allow(dead_code)]` is in `lib.rs`). There are no
section banners: no `//` comment line sits between two top-level items.

**Items, 32–3753:**

| lines | region | items |
|---|---|---|
| 32–42 | handles | `NodeId`, `Span` |
| 44–104 | def metadata types | `MethodBody`, `ParamShape` + `impl` (`is_trivial`) |
| 106–129 | | `Visibility` |
| 131–173 | hash-key types | `HashKeyTag`, `HashKey` |
| 175–201 | | `RescueClause` |
| 203–291 | multi-target types | `MultiTarget` + `impl`, `MultiTargets` + `impl` |
| **293–942** | **the node enum** | **`Node`: one item of 650 lines** |
| 944–1065 | tags | `StatementsKind`, `JumpKind`, `BlockParamKind` |
| 1067–1111 | | `impl Node` (`span`) |
| 1113–1168 | file identity | `static NEXT_ANONYMOUS_FILE_KEY`, `FileKey` + `impl` |
| 1170–1320 | the product | `LoweredAst`, `ConstMutation`, `impl std::fmt::Debug for LoweredAst`, and **two** `impl LoweredAst` blocks (1252–1265 with 2 methods, 1267–1320 with 8) |
| 1322–1380 | entry points | `lower`, `lower_with_key` |
| 1382–2671 | the walk | `struct Builder` (1382–1392), then `impl<'src> Builder<'src>` (1394–2671, 1,278 lines) |
| 2673–2697 | Prism decoding | `constant_string`, `constant_list_names`, `integer_value` |
| 2699–2762 | hash-key helpers | `parse_ruby_integer`, `ruby_inspect_string` |
| 2764–2854 | constant paths | `constant_path_string`, `self_anchored_constant_path`, `rooted_constant_path`, `strict_constant_path_string` |
| 2856–2975 | census | `collect_const_mutations` (104 lines, nested `Census` visitor), `constant_node_name` |
| 2977–3158 | class-body discovery | `direct_method_names`, `discover_visibilities_and_includes`, `process_visibility_stmt`, `visibility_of_modifier`, `collect_call_args`, `literal_symbol_or_string_name`, `back_patch_visibility`, `record_nested_defs` |
| 3160–3288 | def params | `plain_positional_params`, `param_shape_of`, `all_param_names` (nested `Names` visitor) |
| 3290–3500 | block params | `block_param_names`, `push_block_positional`, `push_block_other_positional`, `multi_target_names`, `keyword_param_name` |
| 3502–3529 | | `body_has_explicit_return` (nested `ReturnVisitor`), `span_of` |
| 3531–3626 | multi-target lowering | `lower_multi_targets`, `lower_multi_target`, `for_index_names` |
| 3628–3753 | recovery | `collect_recoverable_children`, `collect_defined_operand_children`, `collect_recoverable` (nested `Collector`) |
| 3755–3756 | tests | `#[cfg(test)] mod tests;` |

**The Builder impl.** The impl has only seven methods. `lower_node` is 1,169 of
its 1,278 lines. The seven methods are:
- `push` (6 lines);
- `line_at` (4);
- `hash_keys_of` (43);
- **`lower_node` (1,169 lines, 1451–2619)**;
- `lower_body` (5);
- `lower_optional_body` (17);
- `harvest_method_bodies` (26).

**Public surface: 19 `pub` items.** They are:
- `NodeId`, `Span`, `MethodBody`, `ParamShape`, `Visibility`, `HashKeyTag`,
  `HashKey`, `RescueClause`, `MultiTarget` and `MultiTargets`;
- `Node`, `StatementsKind`, `JumpKind` and `BlockParamKind`;
- `FileKey`, `LoweredAst` and `ConstMutation`;
- `lower` and `lower_with_key`.

The pub methods are `ParamShape::is_trivial`, `MultiTarget::{span, collect_bound_names}`,
`MultiTargets::{collect_bound_names, bound_names}`, `Node::span`,
`FileKey::{for_path, anonymous}` and 10 on `LoweredAst`.

`lib.rs` declares `pub mod ast;` and re-exports all 19 at the crate root
(`pub use ast::{lower, lower_with_key, BlockParamKind, …}`). The crate has 0
`pub(crate)` today.

**External callers: 52 files.** rigor-infer has 23, rigor-rules 15 and
rigor-cli 14. Every one of them names these items through the root re-export.
No file outside the crate writes a `rigor_parse::ast::` path. The number of
files that name each item:
- `Node` 32, `LoweredAst` 30, `NodeId` 24, `lower` 23, `Span` 18;
- `StatementsKind` 7, `FileKey` 4, `Visibility` 3, `lower_with_key` 3;
- 2 each: `JumpKind`, `MultiTarget` and `MultiTargets`;
- 1 each: `BlockParamKind`, `ConstMutation`, `HashKey`, `HashKeyTag`,
  `MethodBody`, `ParamShape` and `RescueClause`.

Inside the crate, `lib.rs` names `ast::lower` in a doc link. Everything stays
reachable at the same paths (`crate::X` and `crate::ast::X`) through `pub use`.
No new module is `pub`.

**What `ast.rs` keeps (end state, 80 lines, measured):**
- the `//!` docs, plus an optional module map written by hand in the last PR;
- `use crate::ruby_prism;`;
- 9 `mod X;` lines;
- 7 `pub use X::{…};` lines (the public surface);
- 6 `pub(crate) use X::*;` globs;
- `NodeId`, `Span`, `constant_string` and `span_of`;
- `#[cfg(test)] mod tests;`, unchanged, so the 58 `ast::tests::*` names stay
  the same.

Every module uses those four items. A child module sees its parent's private
items, so keeping them in the parent costs no `pub(crate)`.

## 2. Target layout

The rows are in extraction order, leaf-first:
- first, helper families that only the walk calls;
- then the providers they share (`recovery`, `constants`);
- then the types (`lowered_ast`, `node`);
- **last, the walk (`builder`)**.

`builder` goes last, not the types as in #234, so that Phase 2 (§2.3) can run
before it if it is approved.

"Moved" counts split_mod's lines after P0, gaps included. Selectors are listed
in file order. Every `impl:` selector must be listed explicitly (§3.7).

| # | module | selectors | moved | new file |
|---|---|---|---|---|
| P0 | (prep, not move-only) | see §4 | — | — |
| 1 | `block_params.rs` | `BlockParamKind block_param_names push_block_positional push_block_other_positional multi_target_names keyword_param_name` | 279 | 284 |
| 2 | `hash_keys.rs` | `HashKeyTag HashKey Builder::hash_keys_of parse_ruby_integer ruby_inspect_string` | 153 | 160 |
| 3 | `multi_target.rs` | `MultiTarget impl:MultiTarget MultiTargets impl:MultiTargets lower_multi_targets lower_multi_target for_index_names` | 189 | 194 |
| 4 | `definitions.rs` | `MethodBody ParamShape impl:ParamShape Visibility Builder::harvest_method_bodies direct_method_names discover_visibilities_and_includes process_visibility_stmt visibility_of_modifier collect_call_args literal_symbol_or_string_name back_patch_visibility record_nested_defs plain_positional_params param_shape_of all_param_names body_has_explicit_return` | 451 | 458 |
| 5 | `recovery.rs` | `collect_recoverable_children collect_defined_operand_children collect_recoverable` | 129 | 132 |
| 6 | `constants.rs` | `ConstMutation constant_path_string self_anchored_constant_path rooted_constant_path strict_constant_path_string collect_const_mutations constant_node_name` | 257 | 262 |
| 7 | `lowered_ast.rs` | `NEXT_ANONYMOUS_FILE_KEY FileKey impl:FileKey LoweredAst impl:std::fmt::Debug for LoweredAst impl:LoweredAst lower lower_with_key` | 228 | 233 |
| 8 | `node.rs` | `RescueClause Node StatementsKind JumpKind impl:Node` | 785 | 790 |
| 9 | `builder.rs` | `Builder impl:Builder<'src> constant_list_names integer_value` | 1,241 | 1,253 |

Put one selector per line in the selection file. `impl:std::fmt::Debug for LoweredAst`
is a single line; split_mod strips each whole line, so the spaces are fine
(measured).

The 9 rows hold 60 of the 64 items (all but the four the parent keeps) and 2
methods. They move 3,712 of the 3,769 lines that `ast.rs` has after P0.
The parent shrinks after each step to 3,494, 3,343, 3,157, 2,709, 2,582, 2,328,
2,102, 1,319 and finally 80 lines.

### 2.1 Why these seams

- **Types go with the family that builds them.** Each module then holds the
  record type and its lowering, so a change to one of them touches one file:
  - `HashKey` goes with `hash_keys_of`;
  - `MultiTargets` goes with `lower_multi_targets`;
  - `BlockParamKind` goes with `block_param_names`;
  - `ConstMutation` goes with the census;
  - `MethodBody`, `ParamShape` and `Visibility` go with the discovery code that
    fills them.

  `node.rs` holds only `Node` and the payloads the walk itself builds
  (`RescueClause`, `StatementsKind` and `JumpKind`).
- **Block params stand apart from def params.** Block params were the most-edited
  helper family since August: 4 commits on `block_param_names` for #140.
  Def params, visibility and the tier-4b harvest (`definitions`) are cold.
- **`lower`/`lower_with_key` go with `LoweredAst`, not with the Builder.** The
  entry point builds a `LoweredAst` literal. In the same module, the 7 private
  fields of `LoweredAst` stay private, and so do the invariants its accessors
  guard (sorted `local_read_starts` and `paren_unwrapped`). The cost falls on
  `Builder` instead (§3.1): 4 fields of a crate-private type become
  `pub(crate)`. Those fields reach nothing outside rigor-parse.
- **The parent keeps `NodeId`, `Span`, `constant_string` and `span_of`,** at 0
  visibility cost (§1).
- **Module names avoid the item names.**
  - There is no `lower.rs`. The root docs link [`lower`] twice. A module and
    a fn both named `lower` would make that link ambiguous. This is the
    `rule_catalog` lesson from #234.
  - `builder.rs` is named after the type it holds, as `typer.rs` was in #204.

**Lighter variant (8 PRs).** Merge `lowered_ast` into `builder`: one
`lowering.rs` of about 1,480 lines, with 3 `pub(crate)` instead of 7 (only the
siblings' `Builder`, `nodes` and `line_at`), and no hand step (§3.8).

The default keeps them apart for two reasons:
- `LoweredAst` is the public product type, while `Builder` is private
  machinery;
- the side-table features change together in `lowered_ast.rs` (230 lines).
  Examples are `local_read_starts`, `inert_spans` and the `LoweredAst` half of
  `paren_unwrapped`.

**Below ~200 lines:** `recovery` (132), `hash_keys` (160) and `multi_target`
(194). Each one is a seam the reference also draws.

**Proposed `//!` first lines** (humans write the headers):

1. "A literal block's parameter list, lowered to `(name, BlockParamKind)` pairs for `Node::Call`'s `block_params` (rigor-rs#140)."
2. "Value-pinned Hash-literal keys (`HashKey`), precomputed while lowering so `flow.duplicate-hash-key` stays source-free."
3. "Multiple-assignment and `for`-index targets: the owned `MultiTargets` tree and its structural (non-arena) lowering."
4. "What the `def`/`class`/`module` arms read off the Prism tree before lowering erases it: parameter shapes, the ADR-35 visibility table, and the tier-4b method-body harvest."
5. "The recovered-children walk behind the `Statements` carriers of Prism nodes that have no owned variant, and its call-suppressing `defined?` variant."
6. "Constant-path rendering (lenient, strict, rooted, `self::`-anchored) and upstream #540's constant-mutation census."
7. "The owned AST (`LoweredAst`), its file identity (`FileKey`), and the `lower` / `lower_with_key` entry points (ADR-0012)."
8. "The owned node shape (ADR-0012): `Node`, its `span`, and the `StatementsKind` / `JumpKind` / `RescueClause` payloads."
9. "The lowering walk: `Builder` turns one borrowed Prism node into owned arena nodes (`lower_node`)."

### 2.2 The two items a move cannot split

- **`Node` (650 lines).** It is one item. It moves whole to `node.rs`, with
  `impl Node`, `StatementsKind`, `JumpKind` and `RescueClause` (790 lines).
  - Keeping it in `ast.rs` would leave the parent at about 870 lines. Every
    variant edit would then land in the file that also carries the
    `mod`/`pub use` wiring.
  - Moved, `ast.rs` becomes an 80-line manifest that rarely changes.
  - Adding a variant touches `node.rs` and `builder.rs`, the same two regions
    as today.

  A note on sizes: the "about 869 lines" in the brief is 293–1111, which is
  the enum plus the three tag enums and `impl Node`.
- **`Builder::lower_node` (1,169 lines).** It cannot be split by
  `Builder::method` selectors. It is one `fn`: a chain of 61
  `if [let …] node.as_X_node() { … return … }` arms, then a fallback.
  - The only methods a selector can move are the other six. Two of them go to
    their families: `hash_keys_of` to `hash_keys` and `harvest_method_bodies` to
    `definitions`. The core (`push`, `line_at`, `lower_body`,
    `lower_optional_body`) stays with `lower_node`.
  - So after Phase 1, `builder.rs` is 1,253 lines, and 93% of that is
    `lower_node`.

### 2.3 Phase 2 (a separate issue, not move-only): split `lower_node` by family

This is what the brief asks for. It needs one small **non-move** PR that turns
the arm groups into `Builder` methods. After that, move-only PRs with
`Builder::lower_<family>` selectors put each method in its own file. Since
`builder` is the last step of Phase 1, the non-move PR can run in `ast.rs`
before step 9 (then step 9 moves the smaller core), or in `builder.rs` after it.

The arms' predicates are pairwise disjoint: each tests a different Prism node
type. So grouping them does not change which arm fires.

These are the measured families, at `4753cf3`. Lines include each arm's leading
comment. Churn counts commits since 2026-08-01.

| family | arms | lines | churn |
|---|---|---|---|
| expressions | program, statements, local writes and reads, `it`, multi-write, literals (1457–1617); collections, parens, ivar/cvar/gvar/constant/self, interpolation, lambda (2204–2454) | 389 | parens 2, constants 2, vars 2 |
| calls | `call_node` (1619–1736) | 118 | **6** |
| definitions | def, class, module, `class << self` (1738–1934) | 194 | def 2, sclass 2 |
| control | if, unless, else, case, case-in, when, in, while, until, for, begin, and, or (1936–2202) | 255 | 1 each |
| jumps and carriers | return, alias, next, break, redo, retry, `defined?`, BEGIN/END/super/yield, the fallback (2456–2618) | 158 | break 3, return 2, defined 2 |

After Phase 2 the core would be about 180 lines. The new methods need two
parameters, `(node, span)`:
- these arms read the outer `span`: call, return, alias, the four jumps,
  `defined?`, BEGIN/END/super/yield, and the fallback;
- these read the outer `node`: constant-path (four helpers take `node`),
  BEGIN/END/super/yield, and the fallback.

There are three ways to dispatch, to be decided in the Phase 2 mini-spec:
- **(a) Chain.** Each family holds its arms byte-for-byte, at the same
  indentation. Its last line hands off to the next family, and the last family
  ends in the fallback.
  - `verify_move` then cancels every arm line. The only new lines are the
    signatures, one tail line each, and the new `lower_node` body.
  - The cost: up to five family frames are live per nesting level.
- **(b) Dispatcher.** `lower_node` tests each family's node types and then
  calls the family. The arms stay verbatim, but each predicate is written
  twice.
- **(c) `-> Option<NodeId>`.** The families are tried in sequence, so only one
  frame is live per level. But about 70 `return self.push(…)` lines become
  `return Some(…)`, and `verify_move` cannot prove those.

Lowering recursion is bounded only by how deeply the source nests. The crate
has no stack-size handling. So whichever option is chosen, its gate is a
deep-nesting probe: for example, a few thousand nested array literals, in
debug and in release, before and after the change.

## 3. Coupling hazards (each verified by reading the code, then measured)

**3.1 Cross-module edges → `pub(crate)` (26 items, plus 6 globs; the crate has
0 today).** fixvis adds each one, except the two §3.8 marks as by hand. The
reviewer checks that the count per PR matches. `verify_move`'s
`pub(crate) added` counts the glob line too: it equals items + 1 when the glob
is kept. The measured counts are 2, 1, 3, 8, 3, 7, 0, 0 and 8.

| PR | new `pub(crate)` | because |
|---|---|---|
| 1 | `block_param_names` | `lower_node` (call arm) |
| 2 | `hash_keys_of` (method, E0624) | `lower_node` (hash and keyword-hash arms) |
| 3 | `lower_multi_targets for_index_names` | `lower_node` (multi-write and for arms) |
| 4 | `harvest_method_bodies` (method) `direct_method_names discover_visibilities_and_includes plain_positional_params param_shape_of all_param_names body_has_explicit_return` | `lower_node` (def, class and module arms) |
| 5 | `collect_recoverable_children collect_defined_operand_children` | `lower_node`; `lower_multi_target` in `multi_target` |
| 6 | `constant_path_string self_anchored_constant_path rooted_constant_path strict_constant_path_string constant_node_name collect_const_mutations` | `lower_node`; `process_visibility_stmt` in `definitions`; `lower_with_key` |
| 7, 8 | none | `LoweredAst`'s fields are read only in its own module; `node` is all `pub` |
| 9 | `struct Builder`, the fields `nodes source line_starts paren_unwrapped`, the methods `line_at lower_node` | see below |

The step-9 edges come from three siblings:
- `lowered_ast`: `lower_with_key` builds a `Builder { … }` literal, reads
  `builder.nodes` and `builder.paren_unwrapped`, and calls `lower_node`;
- `hash_keys`: `self.line_at`;
- `definitions`: `self.nodes`;
- `hash_keys` and `definitions` also name `Builder` in their
  `use super::{…}` and their impl headers.

Other hazards to know:
- Until step 9, `Builder`'s fields and methods are private to `ast.rs`. The
  children still reach them, because a child sees its parent's private items.
  That is why none of these become `pub(crate)` before step 9.
- Everything else stays private in its module. Examples: `collect_recoverable`,
  `push_block_positional`, `parse_ruby_integer`, `lower_multi_target`,
  `process_visibility_stmt`, `Builder::{push, lower_body, lower_optional_body}`
  and `constant_list_names`.
- No other struct field crosses a module boundary.

**3.2 Names the tests reach through `use super::*`.** `tests.rs` names only:
- 10 `pub` names: `lower` (69 lines), `Node` (63), `Visibility` (11),
  `MultiTarget` (5), `LoweredAst` (4), `StatementsKind` (4), `JumpKind` (3),
  `MultiTargets`, `MethodBody` and `Span`;
- `crate::parse`.

It names no private ast item. The `span_of` at `tests.rs:204` is a local
closure that shadows the fn. It reads no private field, only
`.const_mutations()` and other pub accessors. So no `pub(crate)` is added for
the tests, and no test file changes. The 7 `pub use` lines cover the
re-exported names.

**3.3 Test-only imports: none.** This was predicted and then measured: fixvis
reported no "unused in one build only" parent import in the 9 steps, apart
from the transient in §3.8.

The parent's `use` shrinks in two steps:
- step 7 drops `ParseResult` (its last user, `lower`, leaves);
- step 9 drops `Node as PrismNode`.

That leaves `use crate::ruby_prism;`, which `span_of`'s
`ruby_prism::Location` needs.

split_mod gives each child a copy of the parent's line, and fixvis prunes it.
The measured end state:
- `use crate::ruby_prism::{self, Node as PrismNode};` in `block_params`,
  `multi_target`, `definitions`, `recovery`, `constants` and `builder`;
- `use crate::ruby_prism;` in `hash_keys`;
- `use crate::ruby_prism::ParseResult;` in `lowered_ast`;
- none in `node`.

**3.4 Globs.**
- fixvis drops 3 as unused, because a `pub use` or a method call covers every
  name they carry: `hash_keys` (#2, whose only non-pub item is a method),
  `lowered_ast` (#7) and `node` (#8).
- 6 stay load-bearing, because siblings name their items as `super::NAME`:
  `block_params`, `multi_target`, `definitions`, `recovery`, `constants` and
  `builder`.
- Glob shadowing: all 65 top-level names in `ast.rs` are unique (checked).
  None of them collides with the parent's `NodeId`, `Span`, `constant_string`,
  `span_of` or `ruby_prism`. Re-check this in every PR, because a later parent
  item would shadow a glob silently.

**3.5 Statics, consts and nested items.**
- The only static is `NEXT_ANONYMOUS_FILE_KEY`. Its only reader is
  `FileKey::anonymous`, so it moves to `lowered_ast`. There are 0 consts.
- Four fns contain nested items and a fn-local `use ruby_prism::Visit;`, which
  move with them:
  - `collect_const_mutations`: `Census` and 2 impls;
  - `all_param_names`: `Names` and 2 impls;
  - `body_has_explicit_return`: `ReturnVisitor`;
  - `collect_recoverable`: `Collector`.

  In any module, the fn-local `use` resolves through the extern prelude.

**3.6 Absent shapes (grepped or listed).**
- Absent in `ast.rs`:
  - `macro_rules!`;
  - `line!`, `file!`, `column!`, `module_path!` and `include*!`;
  - `#[path]`;
  - `self::`/`super::`/`crate::` paths in code (line 30 is the only one);
  - a mid-file top-level `use`;
  - section banners.
- **#233 does not apply.** None of the 9 impls carries an attribute. The 5
  lines above `impl std::fmt::Debug for LoweredAst` are `///` docs, which
  `impl_attr_lines` skips. The only methods split out of an impl come from the
  bare `impl<'src> Builder<'src> {`.
- **#221's shapes:**
  - No impl is one line, and no impl has text after its `{`: every `brace`
    line ends in `{`.
  - The `{self, …}` shape with the rename `Node as PrismNode` on line 30 is
    handled by today's fixvis. This was measured: it pruned the line to
    `{self, Node as PrismNode}`, `{self, ParseResult}`,
    `ruby_prism::ParseResult` and `crate::ruby_prism` without an error.
- None of the 9 target paths is git-ignored (`git check-ignore`; the global
  excludes file is `~/.gitexclude`).

**3.7 Impls.**
- Each impl moves in the same PR as its type:
  - `ParamShape` → `definitions`;
  - `MultiTarget` and `MultiTargets` → `multi_target`;
  - `FileKey`, `Debug for LoweredAst` and `LoweredAst` → `lowered_ast`;
  - `Node` → `node`;
  - `Builder<'src>` → `builder`.
- An inherent impl left behind still compiles, so only the selection review
  catches it.
- **`impl:LoweredAst` fails as written.** The file has two `impl LoweredAst`
  blocks, and split_mod stops with `selector 'impl:LoweredAst' matches more than
  one item` (measured). P0 merges them into one.

**3.8 New tooling gap: fixvis cannot parse a grouped E0451 (step 9). Fixed by #257; the by-hand steps below apply only without it.** At step 9,
rustc reports the `Builder { … }` literal in `lowered_ast.rs` as `fields
`source` and `line_starts` of struct `builder::Builder` are private`. fixvis
takes the second backticked name (`line_starts`) as the struct, finds no
such struct, and exits 1. The other two fields arrive as separate E0616
errors, which fixvis fixes.

This is loud, not silent. The by-hand fix:
1. Add `pub(crate) ` to the two field lines in `builder.rs`.
2. Rerun `fixvis.py --prune`. It exits 0, and prunes `Node as PrismNode` from
   `ast.rs`.

While it exits 1, fixvis also prints "unused in one build only" hints for
imports that are simply not pruned yet. Ignore them (#233 item 2).

`verify_move` normalises `pub(crate) `, so the hand edits are proven too. The
fix: take `ns[-1]` as the struct and `ns[:-1]` as the fields. Add it to #233,
or fix it before step 9.

**3.9 Rustdoc links (P0).**
- Baseline: 5 warnings with `--document-private-items`:
  - `NodeId`→private `LoweredAst::nodes` (32);
  - unresolved `Prism::RescueNode` (180);
  - `MultiWrite`→private `collect_recoverable_children` (330);
  - `ConstantRead`→private `constant_path_string` (858);
  - unresolved `SelfArg` (1018).
- Ten link sites break when their doc leaves the target's scope. This was
  measured: without P0, the set grows from 5 to 13 warnings. Eight are new
  unresolved links, and at step `node` the two private-link warnings turn into
  unresolved ones.
- **The #249 lesson.** A reference-style target on a *public* doc that points
  at a *private* item makes rustdoc emit a dead `href="crate::…"`. This was
  measured here on the two `Node` variants. So those two become plain code
  spans instead.
- With P0, the set is **3 warnings, identical through all 9 steps**, and the
  public docs contain no `href="crate::`. Both were measured.

| link line | doc of | goes to | P0 fix |
|---|---|---|---|
| 256 | `MultiTargets` | multi_target | `/// [`RescueClause`]: crate::ast::RescueClause` |
| 330 | `Node::MultiWrite` | node | `[`collect_recoverable_children`]` → a code span |
| 858 | `Node::ConstantRead` | node | `([`constant_path_string`])` → a code span |
| 974 | `StatementsKind::Inert` | node | `    /// [`LoweredAst::in_inert_carrier`]: crate::ast::LoweredAst::in_inert_carrier` |
| 1003 | `BlockParamKind` | block_params | `/// [`Node::Call`]: crate::ast::Node::Call` |
| 1208, 1210 | `ConstMutation` | constants | `/// [`Node::Call`]: crate::ast::Node::Call` and `/// [`Node::Other`]: crate::ast::Node::Other` |
| 1390 | the `Builder.paren_unwrapped` field | builder | `    /// [`LoweredAst::paren_unwrapped`]: crate::ast::LoweredAst::paren_unwrapped` |
| 2869 | `collect_const_mutations` | constants | `/// [`Node::VariableRead`]: crate::ast::Node::VariableRead` |
| 3630 | `collect_recoverable_children` | recovery | `/// [`Builder::lower_node`]: crate::ast::Builder::lower_node` |

Each target goes at the end of its `///` block, after a bare `///` line.
`crate::ast::…` resolves at every stage. `Builder` is reached through the
`builder::*` glob after step 9.

Do not run `doclinks.py` for P0. It adds a target to *every* block that uses
the shortcut: `[`Node::Other`]` alone appears in 6 blocks, and only one of
them needs a target. Keep `doclinks.py` as the fallback for anything this scan
missed.

**3.10 split_mod's wiring and rustdoc stubs.**
- **The `mod` lines.** The parent has no `mod` line above its first item
  (`mod tests` is at the end). So step 1 inserts `mod block_params;` and a
  blank line before the `use`. Each later `mod X;` follows the previous one,
  and the `pub use` and glob lines append after the last `use`, all in
  extraction order. This is cosmetic: the last PR may reorder them, which is
  scaffold to `verify_move`.
- **The redirect stubs.** `ast` is a public module, so every moved `pub` item
  also gets a rustdoc redirect stub at `rigor_parse/ast/<mod>/*.html`. There
  are 17 by the end. The public-page gate therefore uses `-maxdepth 2`, which
  gives 25 pages, not `-maxdepth 1`.

**3.11 Misplaced docs: none.**
- Every doc opener was scanned: 65 top-level items and 26 impl members.
- Each opener describes the item below it.
- No doc block spans a blank line or a `//` line.
- split_mod printed no gap line in any step.
- One oddity stays inside `Node`, which moves whole: `ConstantRead`'s two doc
  paragraphs are split by `// TODO(spec): constant resolution (ADR-0019).` at
  856.

**3.12 Adjacent findings (out of scope; file separately).**
- **Stale text:**
  - `lower_node`'s fallback comment (2602–2612) lists `super`/`yield` and
    `return [entries, policy]` / `super(x: a)` as long-tail nodes with no
    variant. But `Node::Return` exists, and super/yield take the `Inert` arm
    above it.
  - `collect_recoverable_children`'s doc (3632) repeats the stale `return`
    example.
  - `harvest_method_bodies`' doc says `(name, body, has_explicit_return)`,
    but the harvest also records `params`.
  - `parse_ruby_integer`'s doc keeps draft wording ("… actually Ruby's
    leading-zero octal").
- **Dead code:** the field `Builder::source` is never read. Step 9 makes it
  `pub(crate)`, and `#![allow(dead_code)]` hides it.
- **Tooling:** §3.8. Also, `split_mod` could accept `impl:TYPE` when a type
  has two inherent impls (§3.7).

**P0 is needed:** the impl merge (§3.7) and the doc links (§3.9).

## 4. Per-PR process

**P0 (a small PR, not move-only, landed first):**
1. Merge the two `impl LoweredAst` blocks: delete line 1265 (`}`) and line
   1267 (`impl LoweredAst {`), and keep the blank line 1266.
2. Add the eight reference-style targets in the seven doc blocks listed in
   §3.9, each block ending with a `///` separator line.
3. Turn the two private links on the public `Node` docs into code spans (330
   and 858).
4. Optional (it takes the baseline from 3 warnings to 0):
   - make `NodeId`'s `[`LoweredAst::nodes`]` a code span;
   - make `[`Prism::RescueNode`]` a code span (it is Ruby's class);
   - change `[`SelfArg`]` to `[`Self::SelfArg`]`;
   - fix the stale comments in §3.12.

   If you take this option, every "3" below becomes "0".

Gates for P0 (measured with steps 1–3: +17/−4 lines, `ast.rs` 3,769 lines):
- the test list is identical (59);
- rustdoc goes from 5 warnings to **3**: the two `Node` private-link warnings
  disappear, and nothing is added;
- the public docs contain no `href="crate::`;
- `harness/gate.sh` passes.

**Each move PR (#1–#9).**

Setup:
1. Make a fresh worktree on `claude/issue-N-<slug>` from its base
   (`origin/master`, or the previous stacked branch). Pass that base to every
   step that takes one.
2. Run `git submodule update --init reference/rigor`.

`S` is a scratch directory.

```sh
# baselines, on the base commit
cargo test -q --locked -p rigor-parse -- --list 2>/dev/null | grep ': test$' | sort > $S/tests.before   # 59
cargo doc -q -p rigor-parse --no-deps --document-private-items 2>&1 | grep '^warning' | sort > $S/doc.before   # 3
rm -rf target/doc/rigor_parse && cargo doc -q -p rigor-parse --no-deps \
  && find target/doc/rigor_parse -maxdepth 2 -name '*.html' | sort > $S/pub.before   # 25 public pages
# move (sel = the row's selectors, one per line; doc = the //! header)
python3 harness/split/split_mod.py crates/rigor-parse/src/ast.rs MOD $S/MOD.sel $S/MOD.doc   # expect NO gap lines
python3 harness/split/fixvis.py --crate rigor-parse crates/rigor-parse/src/ast/MOD.rs --prune; echo $?  # 0 (step 9: see 3.8)
# proofs (run each bare and read its exit code)
git check-ignore -v crates/rigor-parse/src/ast/MOD.rs   # must print nothing
git add -A crates/rigor-parse/src
python3 harness/split/verify_move.py BASE crates/rigor-parse/src   # 0 UNEXPECTED; read every scaffold row
diff $S/tests.before <(cargo test -q --locked -p rigor-parse -- --list 2>/dev/null | grep ': test$' | sort)
diff $S/doc.before <(cargo doc -q -p rigor-parse --no-deps --document-private-items 2>&1 | grep '^warning' | sort)
rm -rf target/doc/rigor_parse && cargo doc -q -p rigor-parse --no-deps && \
  diff $S/pub.before <(find target/doc/rigor_parse -maxdepth 2 -name '*.html' | sort)
grep -rho 'href="crate::[^"]*"' target/doc/rigor_parse   # must print nothing
cargo check -q --locked -p rigor-infer -p rigor-rules -p rigor-cli --all-targets   # every external caller
harness/gate.sh --base BASE; echo $?
```

If the rustdoc diff is not empty, run `doclinks.py crates/rigor-parse/src/ast/MOD.rs X=crate::ast::X`
for each new "unresolved link", then re-diff. Do not add a target that points
from a public doc to a private item: use a code span instead (§3.9).

Then:
1. Commit, and push with an explicit refspec:
   `git push origin HEAD:refs/heads/claude/issue-N-<slug>`.
2. Open a draft PR with `Part of #umbrella` in the body.
3. Read `gh pr checks`. CI's clippy on 1.88 is the authority.
4. Before marking ready, run `cargo build --release` and
   `python3 harness/fp_audit.py --gaps --sweep`. It must show 0 FP, with
   per-corpus counts equal to master's.
5. Get the review, then run `gh pr ready`.

In the PR body, record:
- the moved-line count;
- `pub(crate) added: N` against §3.1;
- whether the glob was kept or dropped, against §3.4;
- the tests (59) and rustdoc (3) results;
- `verify_move`'s result;
- the sweep numbers;
- for step 9 only, the two hand-made `pub(crate)`s (§3.8).

**What the reviewer checks:**
1. The selection equals this spec's row, and no other item moved.
   split_mod printed no gap line (`ast.rs` has no banners, so any gap line is a
   surprise).
2. No `impl` was left behind (§3.7).
3. `verify_move` shows 0 UNEXPECTED rows. Read every scaffold row:
   - each `use super::{…}` name must point at the moved-away item;
   - the `mod` / `pub use` / glob lines;
   - the new `//!` header;
   - in steps 2 and 4, the copied `impl<'src> Builder<'src> {` wrapper and its
     `}`.
4. The added `pub(crate)`s equal §3.1, and each one is on the listed item or
   field.
5. Every moved `pub` item has a `pub use` in `ast.rs`. The 25 public pages are
   unchanged; the only new rustdoc files are redirect stubs under
   `ast/MOD/`. `cargo check -p rigor-infer -p rigor-rules -p rigor-cli
   --all-targets` passes.
6. The test list (59) and the rustdoc warning set (3) are identical.
7. No test file changed, and no `#[cfg(test)]` import was added (§3.2, §3.3).
8. Glob shadowing: grep the new module's item names in `ast.rs`.
9. The moved range contains none of the shapes in §3.6.
10. Order, which `verify_move` cannot see: the moved ranges appear in the new
    module byte-exact and in their original order. The #234 reviewers checked
    this.
11. CI is green, and the sweep equals master's.

**After the last PR:**
1. Optionally, hand-write a module map in `ast.rs`'s `//!`, as #249 did for
   rigor-rules.
2. Write `docs/notes/<date>-ast-rs-split-outcome.md` with the end state, the
   measured per-PR numbers, and any hazard this spec missed.
3. Add one ledger line to `docs/CURRENT_WORK.md`, folded into the existing
   module-splits line.

## 5. Measured facts (`4753cf3` = `4bec095` for this crate, 2026-09-28)

**`ast.rs`:**
- 3,756 lines.
- rsitems lists 67 top-level rows: 37 fn, 9 struct, 8 enum, 1 type,
  1 static, 9 impl (26 members), 1 use and 1 mod. That is 65 movable items, or
  64 after P0 merges the two impls.
- 19 `pub` items. The crate has 0 `pub(crate)`.
- `ast.rs` history: 59 commits, 28 of them since 2026-08-01, the last being
  `d373915` (the test move).
- The regions most edited since 2026-08-01, by commit count:
  - `Node::Call` 7;
  - 6 each: `LoweredAst`, `lower_with_key` and `lower_node`'s call arm;
  - `impl LoweredAst` 5, and `Node::span` 5 (one line per variant);
  - `block_param_names` 4;
  - 3 each: `ConstMutation`, `Node::Range` and the break arm.

**Tests:** 59 names, 58 `ast::tests::*` and 1 `tests::*` (`lib.rs`).

**Rustdoc** (`--document-private-items`): 5 warnings before P0 (listed in
§3.9), 3 after it. The public doc build shows the same set.

**Public pages:** 25 at `-maxdepth 2`:
- 5 at the root (`all`, `index`, `parse`, `comment_lines`,
  `looks_like_erb_template`);
- 20 under `ast/` (`index` and the 19 pub items).

**Tooling and paths:**
- `verify_move.py HEAD crates/rigor-parse/src`: pass (exit 0), with
  `pub(crate) added: 0`.
- The 9 target paths: none git-ignored.
- Open PRs touching `crates/rigor-parse`: 0.
- #233 (latent `impl-attr` gap): not applicable, since no impl is attributed.
- #221's shapes: absent; the `{self, rename}` `use` shape works.
- New gap: grouped E0451 (§3.8).

**Dry run, run 4 (P0 → steps 1–9, `builder` last):**
- **Per-step results.** Every step passed `verify_move` with 0 UNEXPECTED.
  `pub(crate) added` per step was 2, 1, 3, 8, 3, 7, 0, 0 and 8. Tests stayed
  at 59, rustdoc at 3 warnings, and the public pages at 25. Clippy passed
  locally, and the external `cargo check` passed.
- **Globs:** 6 kept, 3 dropped, as in §3.4.
- **Whole chain, P0 → end:** `verify_move` shows `pub(crate) added: 32`
  (26 items + 6 globs). clippy 1.88 passed on rigor-parse with
  `--all-targets`. The rigor-parse tests (59) passed.
- **Order.** Run 2 extracted `builder` 7th instead of 9th. It gives the same
  end state, apart from the order of the `mod` and glob lines. It passed the
  rigor-infer (322) and rigor-rules (270) tests.
- **End-state files:**
  - `ast.rs` 80;
  - `block_params` 284, `hash_keys` 160, `multi_target` 194;
  - `definitions` 458, `recovery` 132, `constants` 262;
  - `lowered_ast` 233, `node` 790, `builder` 1,253;
  - `tests.rs` 1,056, unchanged.
