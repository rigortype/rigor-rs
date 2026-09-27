# ast.rs and source_index.rs module splits (#258, #260) — outcome

2026-09-28. Two more files went the way of #204 and #234
(`20260928-rules-lib-split-outcome.md`), each from a measured spec, in one
session with `harness/split`:

| file | before | after | PRs | spec |
|---|---|---|---|---|
| `rigor-parse/src/ast.rs` | 3,756 | 80 | P0 #261, steps #262–#270 | `20260928-ast-split-spec.md` |
| `rigor-infer/src/source_index.rs` | 4,650 | 668 | P0 #271, steps #272–#278 | `20260928-source-index-split-spec.md` |

All 18 PRs passed review in round 1 (batched Opus reviews), and the release
sweep of every head matched master's: 0 FP, the same per-corpus counts and
gap totals.

## End state

`ast/` (`ast.rs` keeps `NodeId`, `Span`, `constant_string`, `span_of`, the
`pub use` surface and 6 globs):

| module | lines | holds |
|---|---|---|
| `builder` | 1,254 | `Builder` and its walk, `lower_node` (1,169 lines) |
| `node` | 791 | `Node`, `RescueClause`, `StatementsKind`, `JumpKind` |
| `definitions` | 460 | def metadata, visibility discovery, def params |
| `block_params` | 285 | block-parameter lowering |
| `constants` | 263 | constant paths, `ConstMutation` census |
| `lowered_ast` | 234 | `LoweredAst`, `FileKey`, `lower`, `lower_with_key` |
| `multi_target` | 195 | `MultiTarget(s)` and their lowering |
| `hash_keys` | 161 | `HashKey(Tag)`, `hash_keys_of` |
| `recovery` | 133 | recoverable-children collection |

`source_index/` (`source_index.rs` keeps the 17-item data model):

| module | lines | holds |
|---|---|---|
| `def_attribution` | 1,566 | Passes 1c/1d, the def walk, the declared-constant census |
| `literal_fold` | 612 | the Pass-4b fold (ADR-0038) and its capture |
| `harvest` | 559 | `build`, `build_project`, `harvest`, `merge` |
| `constants` | 377 | C5 literal constants and their queries |
| `override_index` | 362 | ADR-35 ancestry, `qualify`, `lexical_scopes` |
| `registry` | 328 | the class registry and def/existence queries |
| `method_returns` | 285 | tier-4b return and param-bound inference |

## The specs held

- **ast.rs:** after P0 the parent shrank 3,769 → 3,494 → 3,343 → 3,157 →
  2,709 → 2,582 → 2,328 → 2,102 → 1,319 → 80, the spec's sequence exactly.
  Over the whole chain `verify_move` found 0 UNEXPECTED, with 32
  `pub(crate)` added (26 items, 6 globs). Tests (59), rustdoc warnings (3)
  and the 25 public pages did not change.
- **source_index.rs:** every file's line count and the 47 `pub(crate)`
  (42 items and fields, 5 globs) match the spec's §5. Tests (322) and
  rustdoc warnings (28) did not change. One count in the spec was wrong:
  §1's "12 walk and filing fns" is 14 (`decl_body_cx` … `qualify_vec`),
  which is what makes step 5's 29 items add up.
- The tooling gaps the source_index dry run found were closed before the
  split ran (#257 grouped private fields, #259 parent types in
  `private_interfaces` and the empty-impl refusal). The four bumps the spec
  called "hand bumps" were made by `fixvis` with no hand step.

## Beyond the gates

The reviewers ran behaviour differentials the per-step gates do not:
- **ast:** a probe crate lowered 6,350 Ruby files (2,348,685 nodes) at the
  base, mid-chain and end commits and hashed the AST, `const_mutations`,
  `file_key` and the per-node side tables. All byte-identical.
- **source_index:** release `check` (json and text) over the 8 sweep corpora
  (9,337 files), `sig-gen` in four modes plus in-project `check` on 6 survey
  projects with `sig/`, and `coverage`/`annotate` across steps 2–4. All
  byte-identical. The rustdoc-JSON API is identical except that
  `lexical_scopes` and `method_body_spans` now have their canonical path under
  `override_index`; both old paths still resolve.

## Found along the way

- **Reviewing from `git archive` exports** can silently reuse a stale build
  when the exports share `CARGO_TARGET_DIR`. `harness/split/README.md` now
  says so.
- **Dependency doc links:** in a `cargo doc --no-deps` build, P0's
  reference-style targets into dependencies render as raw hrefs
  (`href="rigor_types::ClassId"`); with dependencies documented they resolve.
  A page check should grep `href="[a-z_]+::`, not only `href="crate::`.
- **Phase 2 is filed, not done:** `Builder::lower_node` is still one fn of
  1,169 lines. #279 splits it by node family; that is not move-only.
