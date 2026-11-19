# #431: command thread reserves its stack before the rayon pool

PR [#675](https://github.com/rigortype/rigor-rs/pull/675), merged as
`83e81420`. Closes
[#431](https://github.com/rigortype/rigor-rs/issues/431). Amends
[ADR-0045](../adr/0045-stack-totality.md).

## What changed

- `main` used to call `configure_global_pool()` **before**
  `run_command_thread(run_command)`. On a host that caps total stack
  reservations (`ulimit -v`, `vm.overcommit_memory=2`), the pool's
  ncpu × 256 MiB could spend the budget and leave the command thread on
  Rust's 2 MiB spawn default — smaller than pre-#426 master's 8 MiB main
  thread, so `check` on 3,000 nested arrays overflowed (exit 134).
- `main` now only calls `run_command_thread(run_command_with_pool)`; the
  pool is the first step inside the command thread. Reservation order is
  guaranteed by construction: the stack maps inside `pthread_create`
  before the closure runs.
- `COMMAND_THREAD_SIZES`: on Unix `[256 MiB, 64 MiB]` — if both fail the
  command runs inline on the process main thread rather than spawning at
  2 MiB. On `not(unix)` it keeps `STACK_FALLBACK` (the Windows main
  thread is only 1 MiB, so the 2 MiB spawn is still worth trying).
- Fallback messages name the actual stack via `StackKind`
  (`Reserved`, `RustSpawnDefault`, `ProcessMainThread`): "the platform
  default" is gone — `thread::Builder` with no `stack_size` is Rust's
  2 MiB spawn default, not the OS main-thread stack.
- `configure_global_pool` returns `bool`; on failure the command prints
  one error and returns `FAILURE` before dispatch, instead of printing
  the error and then panicking with `GlobalPoolAlreadyInitialized` at the
  first parallel stage (rayon's `build_global` burns the registry `Once`
  even on `Err`, so a retry was never available).
- `run_command_thread_with` now takes `FnOnce`, stored in
  `Arc<Mutex<Option<F>>>` so the command survives for the inline path
  (`Builder::spawn` consumes the closure even when it fails — the same
  reason `spawn_fallback` needs its own job slot). The lock is released
  before `command()` runs, so a panicking command cannot poison the slot.

## Review outcome

- **Opus 5.5 adversarial (the gate): Approved** on `5a81449d`. Verified
  reservation order end-to-end, run-exactly-once slot semantics, panic
  unwinding, `#[cfg(unix)]` scope, rayon 1.13 `build_global` Err path,
  and stale-wording sweep. Six non-blocking findings.
- **Fable 5.2 design companion:** advisory `Needs fix`, same families —
  the chosen structure is the root-cause fix (a first-come-budget
  priority inversion); alternative orderings analysed and rejected.
- **Grok 4.6 advisory:** quota-blocked (402), not run.

Findings filed: **#676** (ordering guarantee lives in two `main.rs`
lines; add an audit-style source check), **#677** (the `Once` warning is
racy and reports the first/mildest fallback, not the worst; suggested
monotone worst-rank atomic), **#678** (failed-pool exit 1 collides with
the "findings" code — reference uses 70; `--version` exits 1 on a fully
starved host; ADR "a large stack never stops a command" wording;
`coverage --workers` silent global-pool fallback), **#679** (the `None`
step honours `RUST_MIN_STACK`, so "2 MiB spawn default" can be wrong —
suggested `Some(2 << 20)` explicitly; inline 8 MiB assumes default
`ulimit -s`).

## Gates

- `harness/gate.sh`: pass (`cargo test -p rigor-cli`/`-p rigor-parse`,
  `run_snapshot.rb` 0 unregistered).
- `fp_audit.py --gaps --sweep`: **0 FP** on the corpora present
  (mastodon, gitlab-foss, dependabot-core, net-ssh ≈ 8.2K files; mail,
  Ruby, concurrent-ruby, haml not on this host — same coverage as the
  standing 8,251-file local set). No analysis code changed, so the count
  cannot move.
- CI on `5a81449d`: build + test + clippy 1.88 (ubuntu, macos),
  docs-check, snapshot parity, live-reference snapshots — all green.
- New unit tests: injected-spawn refusal per size, Unix never asks for
  `None`, exit code survives both paths, spawn-before-body ordering, the
  three message texts, the pool-failure error line.

## Caveat

No end-to-end capped-host test: macOS cannot cap reservations, so the
ordering is pinned by unit tests on injected spawn failures. The
`LD_PRELOAD` shim that would exercise it is #428.
