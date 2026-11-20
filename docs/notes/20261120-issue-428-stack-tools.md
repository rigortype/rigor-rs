# #428 — the #412 stack-measurement tools committed under `harness/stack/` (PR #706)

Folded post-merge: the arc ran outside the async threads (human-merged at
`c7556e6e`), so this note and the ledger line are the fold step
AGENTS.md's loop requires, written from the PR body and the diff. The PR
body carries the full tables and the review history.

## What landed (PR #706, `Refs #412`, ADR-0045)

- **`harness/stack/depthprobe/`** — the standalone probe crate (own
  workspace + committed lockfile, `rigor-parse` by path so it measures
  the current parser). New `pguard` mode runs the production
  `parse_bounded`; `cguard`/`guardbench` read `MAX_NESTING_DEPTH`.
- **`harness/stack/*.py`** — the screen / refusal / diff / bench tools,
  all repo-relative: data under `RIGOR_STACK_DATA` (default
  `${TMPDIR}/rigor-stack-data`), binaries as explicit arguments.
  `bench.py` uses `/bin/bash` (dash's `ulimit -v` is a no-op on Ubuntu).
- **`.github/workflows/stack.yml`** — `workflow_dispatch` +
  `pull_request` on the tools / the workflow / the bound-stack
  mechanics; the `pull_request` trigger exists because
  `workflow_dispatch` cannot fire until the file reaches the default
  branch.
- **`harness/stack/README.md`** — maps every ADR-0045 number to the
  command that regenerates it. **`docs/adr/0045`** — the "unmeasured
  platforms" bullet replaced with the measured x86_64 results.

The #412-era `minw`/`minm` bisects are dead knobs after #431's fixed
256 → 64 → 2 MiB ladder (`threads.rs`); `RUST_MIN_STACK` moves nothing.
The `maybe_grow` prototype machinery stays on `proto/412-tools-src`; the
`PI_BUDGET` pthread shim is not in the repo.

## Measured (x86_64 Linux, stack-measure run 44666957483, `e6bc4f11`)

Per-pass minima track aarch64: `lower defined` 13 MiB, `parse array`
8 MiB / **7,506 KiB**, `lower chain` ~6/21 MiB (~10–20 % drift, recorded
in the ADR), C-walk 1,178 KiB. Rung screen (`ulimit -v`):

- `at64` (64 MiB workers): carries every screened kind except the
  24,990 chains and `class`/`module`/`blockparam`, which die **`alloc`**
  — a heap failure under the shared `RLIMIT_AS`, not a stack overflow.
- `at2` (2 MiB workers): carries the capped kinds at their Prism caps
  and 5k chains; `alloc` on 20k+ chains. **A 2 MiB stack overflow was
  never observed — the heap hits the wall first.**
- `minm`: `defined:9998` → **16 MiB**; `ifmod`/`elsifchain` → `missed`
  (the scaled budget admits a 64 MiB spawn).

**Windows:** `rigor` does not compile on `windows-latest`
(`rbs_parse_error.rs:50` `i32`/`u32` vs `c_int`/`c_uint`) → filed
**#707**; the `deep_nesting` job is `continue-on-error`.

## Gates and review

`harness/gate.sh` pass; `fp_audit.py --gaps --sweep` **0 FP** (no
analysis code touched). CI green on `e6bc4f11` except the recorded
Windows compile failure. Two Opus review rounds (8 + 3 items, all
landed; round 2 re-measured the 8,257 KiB aarch64 figure as x86_64
7,506 KiB and re-cited the ADR to the run). Self-merge did not apply —
`.github/` + an ADR are human-only (`docs/agents/review.md`); a human
merged it.
