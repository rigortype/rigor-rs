# #677: stack-fallback warning reports each new worst (PR #703)

Last #431-review follow-up to land; #676 (ordering audit) remains open.

## Problem

`note_stack` printed once per process (`Once`), for the FIRST thread to
fall below 256 MiB. Two ways it lied:

- rayon spawns workers serially, so on a capped host they land 64 MiB,
  then 2 MiB — the line said "using 64 MiB" while `check` ran on 2 MiB.
- The inline command report raced the pool build; "using the process
  main thread" could win and hide the workers' 2 MiB entirely.

## Change

- `WORST_STACK_BYTES: Mutex<u64>` held across compare + print: a stack
  reports only when strictly smaller than every stack reported before
  it — one line per new worst (three on the real ladders), each true,
  the worst always reported, in worsening order. (A `fetch_min` +
  separate `eprintln!` could print the milder line second.)
- `spawn_fallback` returns the FIRST refusal alongside the handle; the
  line quotes it — "could not start threads with 256 MiB stacks
  (`EAGAIN`)" — instead of guessing "could not reserve" (spawn cannot
  tell stack memory from a pid limit; both are `EAGAIN`).
- The main thread ranks at its real `RLIMIT_STACK` (getrlimit on Unix,
  1 MiB PE reserve on Windows) capped just under 256 MiB: an inline run
  always reports, and a `ulimit -s` under 2 MiB still beats the workers'
  lines. The Opus gate caught the first version's `RLIM_INFINITY`
  silence: `ulimit -v` / `vm.overcommit_memory=2` refuse the spawns AND
  bound the main stack's growth, so unlimited does not mean safe.
- The line names the size too: "some analysis runs on the process main
  thread (8 MiB)". `RLIM_INFINITY` prints "(unlimited)" — on Linux,
  where it is `u64::MAX`; the macOS/FreeBSD variants would print a huge
  MiB count (cosmetic, folded into #705).

## Deferred

- #705: a worker reports as it spawns, so its line can precede the
  exit-70 one when a later spawn fails the whole pool build
  (report-after-build would need a per-build accumulator); the Windows
  1 MiB could read the real reserve via `GetCurrentThreadStackLimits`;
  the `RLIM_INFINITY` display variants above.

## Gates / review

gate.sh pass; `cargo +1.88.0 clippy --workspace --all-targets -D
warnings` clean; `fp_audit --gaps --sweep` 0 FP; CI green on the merge
head. Opus round 1: Needs fix (the `RLIM_INFINITY` silence); folded with
the design companion's findings into the round-2 head; Opus
fix-confirmation: **Approved**. Grok advisory and the Fable companion
were quota-blocked; the companion ran on Opus as the substitute, both
recorded in the PR.
