# lib.rs module split (#204) — outcome and recipe

2026-09-27. `crates/rigor-infer/src/lib.rs` went from **14,084 lines to 144**
in eleven stacked move-only PRs (#205–#215), one extraction each, in one
session. A doc-only follow-up (#216) came after them. The spec was
`20260927-lib-rs-module-split-spec.md`. This note records the end state,
how each step was proved move-only, and the hazards the spec did not list.

## End state

`lib.rs` is the crate root only: docs with a module map, `mod` declarations,
re-exports, `TypeEnv`, the free shims `type_of`/`build_toplevel_env`, and
the test-module declarations. `Typer` is one struct whose `impl` is split by
pass, one `impl<'i> Typer<'i>` block per module:

| module | lines | holds |
|---|---|---|
| `class_narrowing` | 2,619 | `is_a?` / `case`-`when` narrowing |
| `expr_type` | 1,431 | `type_of`, dispatch by node variant, projection folds |
| `block_call` | 1,425 | calls carrying a literal block |
| `reach` | 1,034 | argument reach (untyped-argument declines) |
| `collection_shape` | 953 | collection-shape receiver survival |
| `call_dispatch` | 578 | block-less call typing |
| `flow_writes` | 541 | span-keyed rebind/mutation tables (free fns) |
| `nilable` | 464 | possible-nil receivers |
| `flow_eval` | 363 | top-level env builders, always-truthy |
| `typer` | 139 | struct, constructors, accessors |

The seven test modules moved first (step 1) to `src/*tests.rs`, with module
paths unchanged. Only two root globs remain: `flow_writes::*` and
`expr_type::*`. They exist because siblings and `source_index.rs` name
those items as `crate::NAME`. Every other module's items are private to it
or reached by method.

## How each step was proved move-only

The scripts are now in `harness/split/` (see its README).

Each step used the same mechanical pipeline:

1. **Extraction.** A syn-based span lister found each item, with its
   attributes, and the comment/blank gap above it. A script moved the
   selected items verbatim, in their original order, and wrapped moved
   methods in a new `impl<'i> Typer<'i>` block.
2. **Imports and visibility.** A compile-driven fixer added `use crate::{…}`
   for unresolved names, added `pub(crate)` where rustc reported a private
   item, method or field, and then pruned unused imports. No visibility was
   chosen by hand.
3. **Proof.** A line-multiset proof compared the parent and the head with
   `pub(crate) ` normalised away. Every line had to be either moved or
   scaffolding (`//!`, `use`, `mod`, the `impl` wrapper, blank lines,
   doc-link targets). Step 1 was proved byte-for-byte by re-inlining the
   files.
4. **Gates.** Each step also passed:
   - an identical `cargo test -- --list` (322 names) and an identical rustdoc
     warning set (28 with `--document-private-items`);
   - `gate.sh` and clippy 1.88;
   - a release sweep (0 FP, with per-corpus counts identical to master's);
   - one Opus review, all eleven Approved in round 1.

## Hazards the spec did not list

- **Misplaced doc comments (6).** A later insertion between a doc block
  and its item leaves the doc on the wrong item. `Typer` was fixed in
  `51420a9`. `type_call`, `block_entry_env`, `BlockJump`, `SourceIndex` and
  `Harvest` were fixed in #216, found by the reviews plus a scan. A
  move-only split keeps each such doc on the wrong item. Scan for this
  first: a doc paragraph that describes something other than the item
  below it.
- **Test-only root imports.** When the last production user of a root
  import moves out, the non-test build reports the import unused, but
  tests still reach it through `use super::*`. Fix: import the name in the
  test file that uses it (`Scalar`/`ShapeKey`/`ShapeMember`/`Type` in
  `tests.rs`, `Node` in six files). Do not add `#[cfg(test)]` imports at
  the root.
- **Doc-only names.** An import used only by an intra-doc link trips
  `unused_imports`. Fix: add a reference-style target
  (`/// [`X`]: path`).
- **Private type across the boundary.** `self.arg_reach(..).untyped` from
  `lib.rs` needed `pub(crate)` on `struct Reach` itself, not only on its
  fields: rustc reports "type `Reach` is private" with no error code.
- **Glob shadowing** (review of #206). A root item added later under the
  same name as a globbed item silently shadows the glob. Check this
  whenever a glob is added.

## Next candidates

The same recipe applies to the other large files. Extracting the inline
(mostly test) modules comes first, because it is cheap and byte-provable.
In the top three files those modules are 44–56% of the lines:

| file | lines | inline `mod` bodies |
|---|---|---|
| `rigor-infer/src/source_index.rs` | 8,456 | 3,812 (3) — moved out in #220 (→ 4,650 lines) |
| `rigor-rules/src/lib.rs` | 7,985 | 3,494 (3) — moved out in #224 (→ 4,497 lines) |
| `rigor-cli/src/lsp.rs` | 7,809 | 4,340 (1) — moved out in #225 (→ 3,471 lines) |
| `rigor-index/src/rbs.rs` | 6,399 | 1,287 (8) |
| `rigor-parse/src/ast.rs` | 4,813 | 1,059 (1) |
| `rigor-cli/src/sig_gen.rs` | 3,535 | 1,086 (1) |
| `rigor-cli/src/main.rs` | 3,640 | 983 (1) |

Outside a crate root, `mod tests;` resolves to `src/<file>/tests.rs`. Raw
string fixtures (LSP JSON, Ruby sources) must not be de-indented, so check
multi-line literals before moving.

## Follow-on (2026-09-27)

- **#219** committed the tooling as `harness/split/`, generalised beyond
  `lib.rs`. Its first review round found four ways it could silently
  produce a wrong result or pass a wrong one:
  - blank-line tidying reached inside string literals;
  - `fixvis` edited imports with a file-wide regex;
  - `verify_move` judged scaffold by line shape, so it passed `true` →
    `false`;
  - a glob that only the tests used was dropped, with exit 0.
  All four were fixed in round 2. `verify_move` now tags each line by its
  syntactic place. A replay of the merged steps still passes all ten.
- **#220** used `split_tests.py` on `source_index.rs`. The 151 interior
  lines of Ruby-source byte strings with literal newlines stayed
  byte-for-byte. The reviewer re-proved the move without the tool.
- **#221** holds the limits round 2 left open: impl attributes are not
  copied onto method wrappers; the impl-frame tag over-reaches; one-line
  impls crash; `use P::self`. None of these shapes is in `crates/` today, so
  land #221 before splitting a file that has them.
- **#224 / #225** (2026-09-28) took the test modules out of rigor-rules
  `lib.rs` and `lsp.rs` the same way. Every multi-line literal in them is
  `\`-continued, so none stayed verbatim. A global git exclude with a bare
  `lsp/` hid the new `src/lsp/tests.rs` from `git add`, so check new paths
  with `git check-ignore` (noted on #221).

