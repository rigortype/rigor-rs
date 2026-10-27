# `lower_node` family split (#279): outcome

#279 was Phase 2 of the #258 `ast.rs` split. It split `Builder::lower_node`,
the hottest function in rigor-parse (1,822 lines, 64 arms plus a fallback),
into five family methods, one file each. It landed as the repo's first
gh-stack: #413 (the method grouping) and #414 (move-only), merged together
as `c7c5a72` on 2026-10-27. Both PRs were Approved in review round 1.
Previous splits: [20260928-ast-index-split-outcome.md](20260928-ast-index-split-outcome.md).

| file | before | after |
|---|---|---|
| `ast/builder.rs` | 2,399 | 609 (core, `lower_node` dispatch, helpers) |
| `ast/builder/calls.rs` | – | 373 (index and attribute writes, `call`) |
| `ast/builder/expressions.rs` | – | 488 (35 arms) |
| `ast/builder/control.rs` | – | 554 (13 arms) |
| `ast/builder/definitions.rs` | – | 218 (def, class, module, sclass) |
| `ast/builder/jumps.rs` | – | 284 (jumps, carriers, unmodeled writes, fallback) |

## Dispatch: a sentinel plus `#[inline(never)]`

The brief offered three options:
- (a) a chain of family calls;
- (b) a dispatcher;
- (c) `-> Option<NodeId>`.

A measuring agent invented a fourth, (d). Each family method keeps its arms
byte-identical and returns `UNMATCHED = NodeId(u32::MAX)` when none of them
matches. It generated all four variants from master with a script.

Every variant lowers a 19,085-file corpus byte-identically to master. The
variants differ in two ways:

- **Review surface.** Measured as `verify_move` leftovers: (a) +29, (c)
  +152/−111 (it rewrites the `return`s), (d) +49.
- **Stack depth.** This is where the inlining attribute matters:
  - Without `#[inline(never)]`, LLVM inlines a family back into `lower_node`.
    Variant (d) is then no better than master, and def and class get worse.
  - With it, `lower_node`'s release frame drops from 1,664 B to 80 B, and
    only one family frame is live per level.
  - A `c-noinline` variant measured identical to (d) with the attribute, so
    (d) won on reviewability.

Each arm's predicate is a pure `as_X_node()` chain. Together the arms test
90 distinct Prism types, and in ruby-prism 1.9.0 each `as_X_node` matches only
its own variant. So the try order cannot change which arm fires. The reviewer
showed this directly: a mutant with the order swapped lowered 0 of 182,289
files differently.

`push` gained `debug_assert!(len < u32::MAX)`, so no real id equals the
sentinel.

Largest nesting that survives, release build, 2 MiB thread, master → split:

| toolchain | array | def | if | `and` | interp |
|---|---|---|---|---|---|
| 1.95 | 1096 → 2089 | 1070 → 1496 | 1114 → 1827 | 579 → 1020 | 564 → 1020 |
| 1.88 (shipped binaries) | 1175 → 1995 | 1144 → 1371 | 1174 → 1853 | – | 595 → 974 |

On debug 1.95, master survives only 22–45 levels. Its `lower_node` frame is
44 KB. The split survives 55–169 levels, and Prism's own `Visit` walk then
binds first. Parse+lower time was unchanged.

## Verification

- **Implementer:**
  - `gate.sh`, clippy 1.88, and the rustdoc warning sets (5 public, 11
    private) all matched master;
  - `verify_move` left only the 55 scaffold lines, and 0 UNEXPECTED rows for
    #414;
  - 128 depth cells, none below master;
  - the 19k-file differential;
  - release sweeps equal to master: 0 FP / 9,435 / 828 gaps.
- **Opus reviewer, independently:**
  - its own arm-identity script (65/65 in order);
  - its own differential over 182,289 files plus 4,500 broken ones;
  - depth re-measured on 1.88.
- **Fable design companion:** "ship, then follow-ups".

## Found along the way

- **The split makes the disjointness invariant easier to break.** Two PRs that
  add the same Prism type to different family files merge without conflict,
  and the first family tried wins silently. Fix: a debug check that runs the
  later families after a match, plus `lower_fallback`, docs and
  `pub(super)`. Filed as #416.
- **`rigor check` overflows its stack on deep nesting.** This happens at about
  1,100 levels on 2 MiB rayon workers and at 44 levels in debug. After the
  split, the next crash sites are in rigor-infer (`class_flow_if`,
  `coll_flow_expr`). Filed as #412.
- **`cargo fmt --check` is not a gate.** Master is hand-formatted (ADR-0032),
  so the implementer compared rustfmt hunks instead (24 on master, 24 on
  #413).
- **`python3` on the dev machine is now 3.15 without PyYAML.**
  `fp_audit.py --sweep` needs a Python that has PyYAML, such as Homebrew's 3.14.
- **A measuring run died on a rate limit, and its report was lost.** The
  re-run committed its tools and data to a local branch after each task. Do
  that from the start for any run over an hour.

## gh-stack pilot

The pilot used the stack commands below. Mechanics and hazards are in
`harness/split/README.md` → "Stacking the PRs".

| step | command | result |
|---|---|---|
| create | `init` / `add` | worked |
| open PRs | `submit --auto` | opened both as drafts; stack #415, which shares the issue/PR number space |
| merge | `gh pr ready` ×2, then `merge 415 --yes --merge` | one atomic merge; both PRs MERGED, #279 closed by #414 |

The old stacked-PR trap (an upper PR left OPEN because its base branch never
reached master) did not occur.

`rebase` and `sync` were not needed. The implementer ran
`git config rerere.enabled true` to skip the init prompt; that setting is
shared repo config. Verdict: use it for multi-PR arcs, from one worktree that
owns every branch in the stack.

## Follow-up: #416 (PR #417, merge `43fcd83`)

The fallback moved out of `lower_jumps` into `#[inline(never)] lower_fallback`, so all five families now end in `UNMATCHED`. In debug builds, `check_families_disjoint` re-runs the later families after a match and asserts that none of them claims the node. Mutants that copy the `nil` arm into a later or an earlier family both fail `cargo test`.

The family methods became `pub(super)`. `lower_node` keeps one reassigned `let mut id`, because shadowing lets cost 16 B of debug frame.

Release lowering is byte-identical. Release depth is equal to master on 1.95 and 1.88. Shapes that recurse through the fallback improve (`f(*f(*…))` 855 → 1020).

**The 432 B finding is toolchain-dependent.** Without `#[inline(never)]` on the fallback, 1.95 inlines it and `lower_node`'s frame grows to 432 B. 1.88 does not inline it. Frame claims must name the toolchain; CI and the shipped binaries use 1.88.

**The runtime check's coverage gap is #418.** 19 of the 90 claimed types never occur in a fixture, so a duplicate of one of them passes (Mutant C). #418 adds the static inventory test, F3.
