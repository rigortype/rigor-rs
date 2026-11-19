# harness/stack — the #412 stack measurements, re-runnable

The tools behind ADR-0045's numbers (rigor-rs#412, moved here by rigor-rs#428
from the `proto/412-tools-src` branch). ADR-0045's stack budgets were measured
once, on aarch64 macOS with uncommitted tools; this directory is the
regeneration path after a toolchain bump, a lowering change, or on a new
platform.

**Generated data never enters the repo.** Trials, TSVs and the probe build
live under `RIGOR_STACK_DATA` (default `${TMPDIR}/rigor-stack-data`).

## The probe

`depthprobe/` is a standalone Rust binary (its own `[workspace]`, own
`Cargo.lock`) that generates every deep-nesting shape and measures parse /
`Visit` / lower / C-walk stack use in-process. It depends on `rigor-parse` by
path, so it always measures the *current* parser, lowering and depth bound.
The scripts build it on first use into `RIGOR_STACK_DATA/target` with
`cargo +1.88.0` (`RIGOR_STACK_CARGO` overrides where rustup is absent).

```
depthprobe gen KIND N        print the deep source (all shapes below)
depthprobe errors KIND N     error count + messages + Prism depth, 4 GiB thread
depthprobe cap KIND MAXN     largest N <= MAXN with 0 parse errors
depthprobe child MODE KIND N KIB   one trial on a KIB-sized spawned thread
depthprobe bisect MODE KIND KIB CAP  max surviving N at KIB stack
depthprobe guardbench LIST REPS    parse vs parse+guard cost over a file list
```

MODE: `parse` (parse + Prism destroy), `visit`, `lower`, `cvisit` / `cguard`
(the C depth walk, unbounded / bounded), `pguard` (the production
`parse_bounded`). KINDs are the `match` arms in `src/main.rs` — the `common.py`
sets (`CAPS`, `UNCAPPED`, `UNCAPPED_IN_SCOPE`, `TYPED_FIELD`, `MIXED`, `BOUND`)
name the ~80-shape screen.

## Scripts

Every rigor argument is `release` / `debug` (`target/<name>/rigor`) or a path.

| script | does |
|---|---|
| `caps.py [KIND…]` | Prism's error-free cap per shape (`cap`), plus error/depth spot-checks on the uncapped kinds |
| `e2e.py CLI KIND[:N]…` | end-to-end survival per shape: `main`, `worker`, and the forced fallback rungs `at64` / `at2` / `inline` / `minm` — see *The post-#431 knobs* |
| `probe_min.py MODE KIND N…` | smallest spawned-thread stack that survives MODE — the granular per-mode min-stack (`--kib` bisects in KiB) |
| `timeit.py CLI KIND N…` | wall time of one `check` on deep input (the super-linear-pass spotter) |
| `accept.py CLI KIND[:N]…` | refusal behaviour: exit code, `internal-error` finding, stderr line, one file and two |
| `surfaces.py CLI [N]` | the refusal as each surface reports it: check text/json, type-of, annotate, coverage, the MCP tools, LSP |
| `lsp_probe.py` / `lsp_matrix.py` | `rigor lsp` on a deep buffer / on-disk project: diagnostics, hover, completion, documentSymbol |
| `crashsite.py` + `lldb_site.py` | where an overflow dies: the lldb report of the recursion cycle, bytes/cycle, top stack consumers (macOS) |
| `frames.py BINARY…` | static prologue frame sizes of the `lower_*` family (aarch64 disassembly) |
| `hm_diff.py` | head-vs-master byte diff: `HM_HEAD`/`HM_MASTER` env vars, each run in a fresh cwd |
| `cmp_bins.py A B` | two binaries' `check` output byte-compared on generated kinds and given files |
| `bench.py CORPUS REPS VARIANT…` | interleaved wall/RSS/output-digest bench; `ULIMIT_V=MiB` in a variant forces the fallback rungs (Linux) |
| `batch.py NAME TOOL [ARGS] -- KINDSET` | run a tool over a named set (`capped`, `uncapped`, `inscope`, `typed`, `mixed`, `bound`, `screen`), marker file at the end |
| `scale.py KIND N…` | `depthprobe errors` wall time over N (spot quadratic parses) |
| `profile.py CLI KIND N` | macOS `sample` of the busy thread mid-run |

## The post-#431 knobs

rigor's analysis stacks are a fixed ladder now: the command thread tries
256 MiB then 64 MiB then runs **inline** on the process main thread (Unix);
pool workers try 256, 64, then an explicit 2 MiB; a pool with no worker exits
70 (`crates/rigor-cli/src/threads.rs`). `RUST_MIN_STACK` moves nothing — every
spawn names an explicit size — so the #412-era minimum-stack bisects are dead.
What the screen measures instead:

- `e2e.py` probes the host's rung-forcing `ulimit -v` budgets once, then runs
  each kind at the budget that lands it on the 64 MiB / 2 MiB / inline rungs
  (read off the stderr warning). `minm` bisects `ulimit -s` on the inline run —
  the check pipeline's real floor, the old table's `minm` equivalent.
- `ulimit -v` is honoured on Linux; **macOS ignores it**, so forced-rung
  columns report `n/a` there and their numbers come from CI (`.github/
  workflows/stack.yml`, ubuntu-latest).
- `probe_min.py` is platform-independent: it sizes the spawned thread
  directly, so the parse / visit / lower / C-walk minima measure anywhere.

## Regenerating ADR-0045's numbers

| ADR-0045 claim | command |
|---|---|
| Prism caps (9,999 array, 4,999 `begin`, 3,332 `if`, 2,500 block, …) | `caps.py` → `caps.tsv` |
| deepest capped construct = 19,999 child edges | `depthprobe errors splat 9998` (or `paren`/`arrparen`): `prism_depth` |
| `defined?` 13 MiB, `lower_jumps` ~1.34 KiB/level | `probe_min.py lower defined 9998` |
| `def`/`class`/`module`/`class <<` 8 MiB lowering | `probe_min.py lower {def,class,module,sclass} 4999` |
| Prism parser 8.1 MiB / 848 B per unit | `probe_min.py parse array 9999` |
| chains ~0.95 KiB/level lowering, 5 MiB @5k / 19 MiB @20k | `probe_min.py lower chain 5000 20000` (plus `andand`, `index`, `cpath`) |
| `1 if a…` / `elsif` ~1.43 KiB/level (pipeline) | `e2e.py` `minm` on `ifmod` / `elsifchain` at 5k/20k (Linux) |
| Prism destroy ~48 B/link, ~144 B/`if`-modifier level | `probe_min.py parse` on `chain` vs `ifmod`, minus the parse-only part |
| the bounded walk ~93 B/level release (2.3 MiB at the bound) | `probe_min.py --kib cguard elsifchain 24990` |
| bound budget: `elsif` 24,990 → ~35 MiB, `if` modifier → 12,490 | `e2e.py release elsifchain:24990 ifmod:12490` (`minm`/`at64` columns) |
| "analyses all of these just under the bound" (+ times) | `accept.py release elsifchain:24990 ifmod:12490 rescuechain:24990 andand:24990 chain:24990`, `timeit.py` same |
| refusal wording per surface | `surfaces.py release 30000`, `accept.py`, `lsp_probe.py` |
| guard cost 0.055 s / 19,085 files, 0.84 s parse, max real depth 61 | `depthprobe guardbench FILES_TXT 3` over a corpus list |
| reservation is free (2/64/256 MiB ≈ same time+RSS) | `bench.py CORPUS 5 base=release v64=release,ULIMIT_V=<b64> v2=release,ULIMIT_V=<b2>` with budgets from `e2e.py`'s probe (Linux) |
| time binds before stack (chain 10k/20k/100k) | `timeit.py release chain 10000 20000 100000` |

The rows the tools cannot re-measure end-to-end — the check pipeline's
per-shape minima on workers (the ADR's `minw` column and "walker that binds"
for `Typer::*` walkers) — need the #431-review `pthread_create` budget shim
(`PI_BUDGET`, Linux `LD_PRELOAD`), which is not in the repo. Without it the
granular pipeline floor comes from `minm` on the inline run; the per-walker
binding comes from `crashsite.py` on a **debug** binary (which still
overflows near Prism's cap, per ADR-0045).

## What did not move

The `maybe_grow` prototype machinery stayed on `proto/412-tools-src`:
`gen_dispatch.py`, `wrapfn.py`, `growloop.py`, `typed_cycles.py`,
`lowerdiff.py`, `mkprobes.py` generate/apply the `stacker::maybe_grow` patch
ADR-0045 rejected. `ladder.py` and `tables.py` were dropped rather than
moved: the stack sizes the ladder bisected are fixed rungs now, and the table
pivot belonged to the scratch session's filenames.

## Requirements

Python 3.14+; `cargo +1.88.0` (rustup) or `RIGOR_STACK_CARGO`; `lldb` and
`objdump`/`nm` for `crashsite.py`/`frames.py` (macOS); a Bourne `ulimit` for
the forced rungs (Linux).
