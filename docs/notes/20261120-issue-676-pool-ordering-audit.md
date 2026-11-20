# #676 — the pool-ordering audit: `fn main` never builds the rayon pool (PR #723)

The #431 review (PR #675) left the ordering guarantee unpinned: the
command thread must reserve its stack before the rayon pool is built,
but that ordering lived only in `main`'s two-line shape
(`threads::run_command_thread(run_command_with_pool)`). The threads
unit test `command_thread_is_spawned_before_the_command_body` pins only
`run_command_thread_with`'s own spawn-before-body sequence — planted the
#431 inversion (pool built in `fn main`, command body poolless, runtime
behaviour identical), the whole rigor-cli suite stayed green (642 unit
+ 54 integration, exit 0).

## The fix

`main_never_builds_the_pool_itself` in `analysis_stack_audit.rs` reads
`rigor-cli/src/main.rs`'s blanked production text and fails on a
`configure_global_pool` / `ThreadPoolBuilder` token in `fn main`'s own
body — called, taken as a value or built. No allow-list: a
stack-proven builder (`.spawn_handler(spawn_pool_thread)`) in `fn main`
is still the inversion, a shape the repo-wide pass exempts via
`STACK_PROOF`; in `fn main` the pool must not be built at all. Zero or
several production `fn main`s is itself a finding, so a rename or a
merge cannot let the check pass vacuously. `use` items inside the body
are masked (an import is not a call); `#[cfg(test)]` siblings masked;
whole-identifier boundaries both sides (`pool.configure_global_pool()`,
`xconfigure_global_pool()` are not hits); whitespace around `::` /
before `(` cannot bypass. 14-row synthetic table pins the boundaries.

## Measured (all on head `7ad4a125`)

- Planted inversion, post-fix: the audit FAILS naming
  `main.rs:113: 'configure_global_pool' in 'fn main'`; the by-construction
  threads unit test still passes — the audit is the pin. Clean tree:
  all green. Reviewer re-probed both rows plus 12 adversarial shapes
  (globs, raw strings, spaced attrs, nested fns, impl-block siblings).
- `harness/gate.sh`: pass — docs_check, `cargo test -p rigor-cli`,
  `run_snapshot.rb` 174 fixtures / 0 unregistered FP.
- clippy 1.88 workspace `--all-targets -D warnings` clean in a fresh
  `CARGO_TARGET_DIR` (CI's both-OS build+test jobs embed the same
  blocking clippy and are green on the head).
- Sweep (release): 8/8 standing corpora, **0 FP candidates**; the gap
  census is unchanged — the diff lives wholly inside the `#[cfg(test)]`
  audit module, so the release binary is untouched by construction.
- CI on the head: all five checks pass; self-merged (Approved, no
  unresolved findings, CLEAN, non-overlapping with held PR #706).
- Environment: system ruby 2.6 cannot parse `harness/lib.rb`; the
  Ruby-side gates need mise's 4.0.5 on PATH
  (`PATH="$(mise where ruby@4.0.5)/bin:$PATH" harness/gate.sh`), and
  `fp_audit --sweep` needs a PyYAML python (mise 3.15.2 has it). CI is
  unaffected.

## Review (one round, Approved)

Adversarial reviewer probed bare-exit in its own worktree: the brief's
rows held; 12 added adversarial rows held (glob imports can't hide the
call; raw strings/comments can't phantom-hit; nested fns in main flag;
impl-block `fn main` siblings and `cfg(any(test,…))` fail closed as
"several production `fn main`s"). Non-blocking findings, filed or
disclosed per `docs/agents/review.md`:

- **#724 (filed)**: `use threads::configure_global_pool as go; go()`
  renames the call site out of the token matcher's reach — a follow-up
  can mirror `flagged_uses`' alias handling for pool tokens inside
  main's body; globs cannot hide the call (the bare call carries the
  token).
- Accepted limits (deliberate indirection, disclosed in the PR body): a
  helper shim outside main's body; a const-generic brace in main's
  signature stealing the body span (absurd for `fn main() -> ExitCode`);
  a `#[cfg(test)]` item nested inside main's body over-flags (fails
  closed).
- Body corrections carried into the PR body: the table is 14 rows (the
  draft said 15); the suite is 642 unit + 54 integration (the commit
  message's "24 lib + 26 integration" miscounted).

No repeated Needs-fix family, so no Fable design companion round was
warranted.
