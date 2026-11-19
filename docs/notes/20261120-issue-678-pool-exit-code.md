# #678: pool-build failure exits 70; static-text commands poison the pool (PR #702)

Third #431-review follow-up landed (after #679); #676/#677 remain open.

## Problem

- A rayon pool that could not build any worker returned
  `ExitCode::FAILURE` (1) — indistinguishable from "findings" to a CI
  wrapper. The reference reserves `EXIT_INTERNAL_ERROR` = 70
  (`rigor/cli.rb:30`, #609) for "a run the analyzer could not finish".
- `rigor --version` / `help` exited 1 on a host too starved to spawn
  even one 2 MiB worker, though pre-#412 they never touched rayon.
- `coverage --workers=N` fell back to the global pool silently.
- ADR-0045 said "a large stack never stops a command" a few lines after
  describing the pool-failure exit.

## Change

- `EXIT_INTERNAL_ERROR = 70` in `main.rs`; pool-build failure returns it.
  The two unreachable `unwrap_or(ExitCode::FAILURE)` fallbacks in
  `run_command_thread_with` moved to 70 too (a fired one means the
  command never produced a code — a could-not-run).
- `needs_analysis_pool(argv)`: no-args / `-h` / `--help` / `help` /
  `version` / `--version` / `-v` / `-V` / `upgrade` skip the pool build.
  Whitelist, not lazy ensure: a missed call site on the lazy shape
  silently builds rayon's default pool on 2 MiB/`RUST_MIN_STACK` stacks
  — the #412 regression.
- **Fail-closed skip** (design-companion finding, second commit): the
  skip branch calls `disable_global_pool()` — a `build_global` whose
  spawn handler always fails. A failed `build_global` still consumes the
  one-shot registry (rayon-core 1.13 `set_global_registry`), so a
  skipped path that reached rayon panics instead of analyzing on a small
  default pool. `help <cmd>` re-dispatches `<cmd> --help`, so the hole
  covered every catalogue command until pinned.
- `tests/static_commands_no_pool.rs` parses `CATALOGUE_NAMES` out of
  `main.rs` and runs `rigor help <cmd>` for all 28 commands under the
  poison: exit 101 anywhere means the skip leaked. `lens`→2 (not yet a
  dispatch arm, pre-existing) and `playground`→64 (the missing-gem
  surface, matching the reference).
- `coverage --workers` fallback now discloses
  `rigor coverage: could not build the --workers pool (…); using the
  global pool` on stderr (the `rigor {cmd}:` machinery-warning family,
  not the file's `coverage:` reference-usage lines).
- ADR-0045 reworded: the command's own stack never stops a command; a
  pool with zero workers does (exit 70). Records the poison and the
  deferred `use_current_thread` degrade (a one-thread pool on the
  command thread's 256 MiB stack — the reference's sequential default —
  cannot serve `lsp`/`mcp`, so it would be a per-command mode).

## Gates / review

`gate.sh` pass; CI green on 5deeec88 (build+test macOS/Linux, snapshot
parity, live-reference snapshot, docs-check); `fp_audit --gaps --sweep`
0 FP over 8 corpora. No diagnostic path touched. Opus Approved
(7dec7e7a) + fix-confirmation Approved (5deeec88); Fable companion:
fail-open → poisoned. Self-merged under the owner's ADR-override.

## Left (filed / deferred)

- #677: the fallback warning prints the first degraded stack, not the
  worst — `fetch_max` worst-rank, one line per degradation level.
- #676: nothing pins the command-thread-before-pool ordering
  (`analysis_stack_audit`-style source check).
- `use_current_thread` degrade: recorded in ADR-0045, not built.
- `rigor help lens` exits 2 (lens is in `CATALOGUE_NAMES` but not a
  dispatch arm) — pre-existing, pinned by the new test's expected table.
