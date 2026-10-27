# Stack totality: 256 MiB analysis threads and a 25,000-level depth bound

Status: accepted 2026-10-28 (issue [#412](https://github.com/rigortype/rigor-rs/issues/412)).
Measured on release builds (aarch64 macOS, toolchain 1.88, the toolchain the
release binaries use).

## Context

`rigor check` and `rigor lsp` died with SIGABRT ("has overflowed its stack")
on deeply nested source that Prism accepts. That killed the whole run, or the
editor's language server. `catch_unwind` (ADR-0016) cannot catch an overflow.

Two kinds of input matter:

- **Capped.** Prism stops recursing at 10,000 depth units: 9,999 arrays,
  4,999 `begin`s, 3,332 `if`s, 2,500 blocks.
- **Uncapped.** Prism parses left-nested chains iteratively, with no cap:
  `x.a.a…`, `1 + 1 + …`, `a && a && …`, `A::A::…`, `1 if a if a…`, `elsif`
  chains, and long `rescue` clause lists. Every later walker still recurses
  once per link. So does Prism's own `pm_node_destroy`, which runs when a
  `ParseResult` is dropped.

The threads ran with the platform stacks:
- the main thread: 8 MiB on macOS and Linux, 1 MiB on Windows. It runs
  single-file `check`, the LSP loop thread, `sig-gen`, `type-of`,
  `annotate`, `effects` and `mcp`.
- rayon workers: 2 MiB.

The reference aborts on the same inputs at about 700–1,300 levels (exit 70,
an empty report). See
[20261028-deep-nesting-divergence.md](../notes/20261028-deep-nesting-divergence.md).

## Decision

1. **Every analysis thread gets a 256 MiB stack** (`ANALYSIS_STACK_BYTES`,
   `rigor-cli/src/main.rs`):
   - `main` first builds the global rayon pool with that stack. An `Err`
     there is a bug, so it is not ignored.
   - `main` then runs the whole command on a spawned thread named `main`, so
     panic and overflow messages read as before.
   - The `coverage --workers=N` pool gets the same size.
2. **A depth bound of 25,000** (`rigor_parse::MAX_NESTING_DEPTH`):
   - Depth is counted in Prism child edges; the program node is depth 1.
   - `rigor_parse::parse_bounded` measures it right after the parse, before
     any walker runs. Every production parse site calls it: `check` stage 1,
     every LSP handler, `coverage`, `sig-gen`, `type-of`, `annotate`,
     `effects` and the MCP tools.
   - In `check`, a file past the bound is skipped exactly like a panicked file
     (ADR-0016): the `internal-error` `:info` finding, the `internal panic
     on …` stderr line, no harvest, and a failing exit. Its message names the
     depth and the limit.
   - The LSP publishes the same finding for the buffer. Hover, completion and
     documentSymbol answer null.
   - The other commands treat such a file as they treat an unreadable one.
3. **A refused tree is never dropped on the caller's stack.** It is measured
   in full and dropped on a thread sized from the source length: 16 MiB plus
   256 B per byte in release, 4 KiB per byte in debug. If that thread cannot
   be created, the tree is leaked.

### Why the depth walk runs in C

The walk calls Prism's exported `pm_visit_child_nodes` with a callback; no
recursion goes through ruby-prism's `Visit`. Three reasons:

- **Coverage.** The C walk reaches every child field. `Visit` reaches
  `RescueNode.subsequent` without passing through `Visit::visit`, so a
  counter on `visit_branch_node_enter` never sees a rescue-clause chain.
  Those chains are uncapped: 100,000 clauses parse without error.
- **No unwinding.** The callback stops descending once the bound is passed.
  Unwinding out of a `Visit` walk would need a panic payload. Unwinding
  through C frames is undefined behaviour.
- **Cost.** A level costs about 93 B in release and 1.5 KiB in debug, against
  160 B and 12 KiB through `Visit`. A debug binary can therefore walk to the
  bound inside its own stack. Over 19,085 real files the walk adds 0.055 s,
  next to 0.84 s of parsing.

## Measurements

**Capped kinds at Prism's cap.** Minimum stack in MiB, the same for the main
thread and for a worker, from 35 kinds:

| kinds | min stack (MiB) | the walker that binds |
|---|---|---|
| `defined?` | 13 | lowering, `lower_jumps` (1,344 B/level) |
| ternary | 12 | `Typer::flow_eval_scope` |
| array, paren, `case` | 11 | lowering (1,056–2,128 B/level) |
| `!`, `not`, unary `-` | 10 | `Typer::coll_flow_expr` (960 B/level) |
| `[*` splat | 9 | Prism's C parser |
| `def`, `class`, `module`, `class <<` | 8 | lowering, `lower_definitions` |
| the other 22 kinds | 3–7 | Prism's C parser (11 kinds), lowering (7), `class_flow_if` (2), `coll_flow_expr` (2) |

- Prism's own parser needs up to 8.1 MiB (848 B per unit). That alone
  overflows the 8 MiB main thread at the array cap.
- The deepest capped construct measures 19,999 child edges (`((…))`,
  `[*[*…]]`, `[*(…)]`).
- The deepest of 19,085 real files measures 61.

**Uncapped chains.** Minimum stack for `rigor check`:

| shape | per level | min stack at 5k | at 20k |
|---|---|---|---|
| `x.a.a…`, `a && a…`, `x[1][1]…`, `A::A…` | ~0.95 KiB (lowering) | 5 MiB | 19 MiB |
| `1 if a if a…`, `elsif` chains | ~1.43 KiB (`class_flow_if`) | 7 MiB | 28 MiB |
| `rescue` clause lists | small | 1 MiB | – |

- Prism's destroy costs 48 B per call link and 144 B per `if`-modifier
  level: 46 MiB and 138 MiB at 1M links.
- **Time binds before stack does.** `rigor check` is quadratic on chains: a
  chain of 10k links takes 9.8 s, 20k takes 40 s, and 100k takes 1,093 s on
  a big enough stack. `1 + 1 + …` takes 137 s at 20k. The quadratic passes
  are tracked separately from #412.

**The bound's stack budget.**
- An `elsif` chain costs one edge and about 1.43 KiB per level. The
  smallest stack that carries 24,990 levels measures 35 MiB.
- An `if`-modifier chain costs two edges per level, so it stops at 12,500
  levels.
- The bounded walk itself is 2.3 MiB.
- Total: under 40 MiB against a 256 MiB stack, a margin of about 6×.
- Just under the bound, the release build analyses all of these, as one file
  and as two: `elsif` ×24,990 (22 s), `rescue` clauses ×24,990, `if a`
  modifiers ×12,490, `&&` ×24,990, and an `x.a` chain ×24,990 (45 s).

**The cost of reserving the stack.** A larger stack changes neither time nor
memory. Medians over interleaved runs; every variant produced identical output:

| corpus (files) | 2 MiB | 64 MiB | 256 MiB |
|---|---|---|---|
| gitlab-foss/lib (4,676) | 0.67 s, 389 MiB | 0.70 s, 383 MiB | 0.70 s, 387 MiB |
| mastodon/app (1,236) | 0.25 s, 108 MiB | 0.24 s, 108 MiB | 0.24 s, 108 MiB |
| dependabot-core (1,859) | 2.18 s, 7,200 MiB | 2.25 s, 7,199 MiB | 2.20 s, 7,201 MiB |

## Rejected options

- **Larger stacks alone.** They cover the capped kinds, but no fixed size
  covers an unbounded chain. A 1M-link chain needs 1–1.4 GiB, and the
  analysis would take about 30 hours anyway.
- **`stacker::maybe_grow`.** It is in neither `Cargo.lock` nor the local
  registry, so adding it means a network fetch plus `psm`'s assembly build.
  A dependency-free prototype showed the cost:
  - 16 Rust walkers needed wrapping, plus a `visit` override with a copy of
    ruby-prism's 151-arm dispatch on 7 `Visit` impls.
  - The same chain placed inside a `def`, an assignment or an `if`
    condition reached three more walkers.
  - It cannot wrap Prism's C destroy, and it does not fix the quadratic time.

  Its fast path cost nothing measurable, and its lowering output was
  byte-identical on 19,085 files.
- **Iterative rewrites** of the 20-odd walkers above. A large rewrite, and
  still bounded by time.
- **`RUST_MIN_STACK` or `env::set_var`.** std reads the variable once, and
  `set_var` is `unsafe` under edition 2024. Explicit sizes are local and
  visible.
- **A depth guard through `Visit`, stopped by `resume_unwind`.** It cannot see
  `rescue` chains, a debug level costs 12 KiB, and it would need the same
  sized drop thread anyway.

## Consequences

- **Linux strict overcommit.** Under `vm.overcommit_memory = 2`, every
  thread's 256 MiB reservation counts against the commit limit: one command
  thread plus one per core. Default heuristic overcommit, macOS and Windows
  only reserve. Windows creates threads with
  `STACK_SIZE_PARAM_IS_A_RESERVATION`.
- **Debug builds** reuse the release-calibrated 256 MiB. Debug frames are
  10–40× larger, so a debug binary still overflows on some capped kinds near
  Prism's cap. That is out of scope. The regression test
  (`rigor-cli/tests/deep_nesting.rs`) uses depths that a debug binary
  carries.
- **x86_64 Linux and Windows** were not measured locally; the margins above
  are aarch64 numbers. CI's x86_64 Linux job runs the regression test, in
  debug, on every push. Windows is unmeasured.
- **Below the bound nothing changes.** A file past it reports
  `internal-error`, where the reference aborts the whole run.
