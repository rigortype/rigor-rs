# #679: the last stack fallback is an explicit 2 MiB (PR #686)

Follow-up from the #431 review (PR #675).

## Problem

`STACK_FALLBACK`'s last step was `None` — `thread::Builder` with no
`stack_size`. That default honours `RUST_MIN_STACK`, so "Rust's 2 MiB
spawn default" was wrong in both directions, and a large value could fail
the last-resort spawn itself — a stack fallback becoming a pool-build
failure. The Unix inline fallback also assumed an 8 MiB main thread;
`ulimit -s` can leave less than the 2 MiB spawn it declined.

## Change

- `STACK_FALLBACK` is `[usize; 3]` = 256/64/2 MiB; `Option<usize>` out of
  `spawn_fallback`, `std_spawn`, `COMMAND_THREAD_SIZES`; `size_to_kind`
  deleted; `StackKind` = `Reserved(usize) | ProcessMainThread`.
- The warning prints "using 2 MiB" — true by construction.
- `Builder::stack_size` replaces `RUST_MIN_STACK` (not `max`) on the 1.88
  toolchain, verified from std source and empirically in review:

  | RUST_MIN_STACK | stack_size(2 MiB) | no stack_size (old) |
  |---|---|---|
  | unset | 2,109,440 | 2,109,440 |
  | 4194304 | 2,109,440 | 4,206,592 |
  | 1e15 | Ok, 2,109,440 | Err (os error 35) |

- ADR-0045 amended: explicit-2 MiB wording + one sentence on the
  `ulimit -s` assumption.

## Gates / review

`cargo test -p rigor-cli` pass; CI green on 3a5887a9; sweep 0 FP
(3 of 7 corpora — haml absent locally). No diagnostic path touched.
Opus Approved + Fable companion Approved.

## Left for #677/#678 (review + companion nits, filed not fixed)

- A `LAST_RESORT_STACK_BYTES` const would keep the Unix negative test
  from going vacuous if the last resort ever moves off 2 MiB.
- Module doc "never a spawned thread smaller than that could be" is
  convoluted; test name `unix_command_thread_does_not_spawn_below_8_mib`
  overclaims (it pins ≤64 MiB + inline).
- #677's worst-rank atomic can now key on bytes: every spawned kind is
  totally ordered; `ProcessMainThread` is the only sentinel — rank it
  worst (its true size is `ulimit -s`, unknown).
