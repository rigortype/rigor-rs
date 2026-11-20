# Stack totality: 256 MiB analysis threads and a 25,000-level depth bound

Status: accepted 2026-10-28 (issue [#412](https://github.com/rigortype/rigor-rs/issues/412)).
Amended 2026-11-19 ([#431](https://github.com/rigortype/rigor-rs/issues/431)): the command
thread reserves its stack before the rayon pool; fallback wording names the
actual stack (never "the platform default").
Amended 2026-11-19 ([#679](https://github.com/rigortype/rigor-rs/issues/679)): the last
resort is an explicit 2 MiB `stack_size`, never `thread::Builder`'s no-size
default, which honours `RUST_MIN_STACK`.
Amended 2026-11-19 ([#678](https://github.com/rigortype/rigor-rs/issues/678)): a pool
with no worker at all exits 70 (the reference's `EXIT_INTERNAL_ERROR`),
not 1; `help` / `version` / `upgrade` skip the pool build and instead
poison the one-shot registry so a skipped path that reached rayon panics;
and `coverage --workers` discloses a scoped-pool build failure.
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

1. **Every analysis thread gets a 256 MiB stack**
   (`rigor_parse::ANALYSIS_STACK_BYTES`, spawned by `rigor-cli/src/threads.rs`):
   - `main` first starts the command thread (named `main`, so panic and
     overflow messages read as before). That thread reserves its stack
     **before** the rayon pool, so a host that caps total reservations
     (`ulimit -v`, `vm.overcommit_memory=2`) cannot leave the command on a
     smaller stack than master's 8 MiB main thread (rigor-rs#431).
   - On Unix the command thread tries 256 MiB then 64 MiB; if both fail it
     runs inline on the process main thread (`ulimit -s`, 8 MiB by default
     but possibly less — a host could leave it under a 2 MiB spawn) rather
     than spawning at 2 MiB. On Windows the main thread is only 1 MiB, so
     the command thread still tries the explicit 2 MiB spawn before
     running inline.
   - The command thread then builds the global rayon pool, once, with a
     spawn handler that gives each worker the stack — unless the command
     only prints static text (`help`, `version`, `upgrade`, or the bare
     command list), which never touches rayon and so skips the build; on a
     host too starved for even one worker those commands still answer
     (rigor-rs#678). The skip is fail-closed: instead of building, it runs
     a `build_global` whose spawn handler always fails — the failed build
     still consumes the one-shot registry, so a skipped path that reached
     rayon panics rather than silently building a default pool on std's
     small stacks (`disable_global_pool`; `tests/static_commands_no_pool.rs`
     runs `help <cmd>` for every catalogue command). `build_global` is
     one-shot: rayon marks the registry as set even when building fails, so
     a retry with a smaller size would leave no pool at all. The fallback
     therefore happens per thread, inside the handler: 256 MiB, then 64 MiB,
     then an explicit 2 MiB (`stack_size(2 << 20)` — not the OS main-thread
     stack, and never the no-size default, which `RUST_MIN_STACK` could
     inflate past the point the last-resort spawn itself fails). If no
     worker can be
     created even at that size, the process prints one error (`rigor: could
     not start the analysis thread pool: …`) and exits 70, the reference's
     `EXIT_INTERNAL_ERROR` — not 1, which means findings; it does not
     continue into a later `GlobalPoolAlreadyInitialized` panic. (A
     per-command degrade to a one-thread pool on the command thread's own
     256 MiB stack — rayon's `use_current_thread`, the reference's
     sequential default — was considered and deferred: it cannot serve
     `lsp`/`mcp`, so it would be a per-command mode. Exit 70 is uniform.)
   - The `coverage --workers=N` pool uses the same handler.
   - The command's own stack never stops a command from starting (it can
     always fall back to the process main thread); a pool with no worker
     at all does stop it, with the exit-70 line above. A stack that falls
     back prints one stderr line when it is strictly smaller than every
     stack reported before it — at most one per new worst, so a mild
     early fallback cannot hide the worst case (rigor-rs#677); the normal
     path prints nothing. Each line names what was actually used and its
     size: "64 MiB", "2 MiB", or "the process main thread (8 MiB)" —
     never "the platform default" — and quotes the first refusal's OS
     error, since `spawn` cannot tell stack memory from a pid limit. The
     process main thread ranks at its real `RLIMIT_STACK` capped just
     under 256 MiB (Unix) / its 1 MiB PE reserve (Windows), so an inline
     run always reports as on master: `unlimited` does not mean safe,
     because `ulimit -v` and `vm.overcommit_memory=2` bound the main
     stack's growth as surely as they refuse the spawns. A worker reports
     as it spawns, so a line can precede the exit-70 one when a later
     worker's spawn then fails the whole build.
2. **A depth bound of 25,000** (`rigor_parse::MAX_NESTING_DEPTH`):
   - Depth is counted in Prism child edges; the program node is depth 1.
   - `rigor_parse::parse_bounded` measures it right after the parse, before
     any walker runs. Every production parse site calls it: `check` stage 1,
     every LSP handler, `coverage`, `sig-gen`, `type-of`, `annotate`,
     `effects` and the MCP tools.
   - In `check`, a file past the bound is skipped the way a panicked file is
     (ADR-0016): an `internal-error` `:info` finding, no harvest, and a
     failing exit. The wording is the refusal's own, since nothing panicked.
     The finding's message is "source nesting depth N exceeds the analysis
     limit of 25000 (rigor-rs#412); file not analysed". The stderr line is
     `rigor check: skipped <path>: <that message>`.
   - The LSP publishes the same finding for the buffer. Hover, completion and
     documentSymbol answer null.
   - The other commands treat such a file as they treat an unreadable one:
     `type-of` and `annotate` print the message and exit 1, `coverage` lists
     it with the files it could not scan, `sig-gen` and `effects` skip it, and
     an MCP tool returns the message as its error.
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

The 5/19 MiB chain floors are the Oct #412 aarch64 numbers. Regenerating
with `probe_min.py` on the current lowering (`--kib`) reads ~6 MiB / ~21 MiB
(~10–20 % drift, same toolchain, more code in the frames).

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

- **The bound counts syntactic depth only.** Inferred-type depth is a
  second axis it does not cover (#427). Rebinding `x` 70 times to
  `[[[…9000 levels…[x]…]]]` measures 9,004 child edges, yet `check`
  overflows in `rigor_types::display::describe_named` (about 440 B per type
  level), and `rigor lsp` dies on didOpen. Master already overflows on this
  shape with 2 lines; the 256 MiB stacks raise the threshold about 30×.
- **A file past the bound that also has syntax errors** reports the depth
  refusal instead of its parse errors. The bound is checked before the
  parse-error branch, so a refused tree is never walked. This also applies
  to LSP buffers: `didOpen` / `didChange` publish the refusal finding, not
  the buffer's parse errors. On master, `x.a…×30000` followed by `)`
  reported `unexpected ')', ignoring it`. This sits inside the divergence
  region, where the reference exits 70.
- **Prism's own parse is quadratic on `&&`, `||`, `and` and `or` chains**, so
  refusing such a file still pays for the parse: `Prism.parse_success?` takes
  0.8 s at 30k links, 4.0 s at 60k and 14.8 s at 120k. "Time binds before
  stack does" above is about rigor's own passes; this cost is Prism's.
- **Two surfaces differ on purpose.**
  - In the LSP, a buffer past the bound publishes its `internal-error`
    finding, while a buffer that panics publishes nothing, as it did before.
    The refusal is deterministic and the user can act on it; a panic is a
    rigor bug, and the LSP's panic path is unchanged.
  - `coverage` lists a refused file under "Parse errors:". That list is
    coverage's existing bucket for every file it could not scan, including
    unreadable ones.
- **Linux strict overcommit.** Under `vm.overcommit_memory = 2`, every
  thread's 256 MiB reservation counts against the commit limit: one command
  thread plus one per core. The command thread claims first (rigor-rs#431),
  so the pool's ncpu × 256 MiB cannot spend the budget and leave the
  command on a 2 MiB spawn. Default heuristic overcommit,
  macOS and Windows only reserve. Windows creates threads with
  `STACK_SIZE_PARAM_IS_A_RESERVATION`.
- **Debug builds** reuse the release-calibrated 256 MiB. Debug frames are
  10–40× larger, so a debug binary still overflows on some capped kinds near
  Prism's cap. That is out of scope. The regression test
  (`rigor-cli/tests/deep_nesting.rs`) uses depths that a debug binary
  carries.
- **x86_64 Linux** is measured by the committed tools
  (`harness/stack/`, workflow `stack-measure`, rigor-rs#428; numbers below
  from run 44651765611 on head `4b49407f`, with the post-review
  rung-gated columns still landing). The per-pass minima track aarch64
  (`lower defined` 13 MiB, `parse array` 8 MiB / 8257 KiB, the C-walk
  1,178 KiB); every capped and refusal cap is identical. The 64 MiB
  rung is a narrow, scheduling-noisy band on the GH runner (`at64` is
  often `n/a`); when it *is* forced, some capped kinds (`class`/`module`
  at 4999) still die with SIGABRT whose stderr is *not* a stack overflow
  (typically glibc's per-thread malloc arenas counting against
  `RLIMIT_AS`). The 2 MiB rung likewise dies on many capped and bound
  shapes by the same signal, not a confirmed overflow. The inline
  (`ulimit -s`) floor is only a number when the trial landed on `main`:
  `defined:9998` measured 16 MiB (`rung=main+2`); bound-depth chains
  (`ifmod:12490`, `elsifchain:24990`) could not be forced onto `main`
  once the scaled `ulimit -v` budget admitted a 64 MiB spawn, so those
  rows report `missed`, not 345–384 MiB. The aarch64 35 MiB bound-budget
  figure is the pipeline floor from #412, not regenerated on x86_64.
- **Windows** is unmeasured: the port does not yet compile on
  `windows-latest` (rigor-rs#707); `stack-measure` runs the
  `deep_nesting` tests there once it does.
- **Below the bound nothing changes.** A file past it reports
  `internal-error`, where the reference aborts the whole run.
