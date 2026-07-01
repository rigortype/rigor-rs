# Source formatting: hand-formatted, rustfmt not enforced

Status: accepted

rigor-rs source is **hand-formatted**. `cargo fmt` is intentionally NOT run over
the tree and `cargo fmt --check` is NOT a CI gate. This is a rigor-rs-internal
decision, **not** a [parity surface](../../CONTEXT.md) — formatting has no bearing
on the diagnostic-set parity bar ([ADR-0002](0002-diagnostic-set-parity.md)); the
reference is Ruby and shares no formatter with this codebase.

## The decision

- The codebase keeps a deliberately **compact** hand style: short struct literals
  on one line, packed `match` arms, method chains kept inline up to the line
  budget, and other density choices `cargo fmt` would expand. `max_width` is the
  conventional 100 columns.
- CI does **not** enforce `fmt --check`. The blocking style gate is clippy
  (`-D warnings`, [ci.yml](../../.github/workflows/ci.yml)); formatting is left to
  authoring discipline and review.
- `rustfmt.toml` exists only to **document** this policy in-tree (a loud header
  comment) and to pin `edition` / `max_width` for any tool that insists on a
  config. Its presence is NOT an invitation to run rustfmt.

## Why not adopt `cargo fmt`

Measured against the tree at decision time, `cargo fmt` rewrites **239 hunks
across 25 files** — nearly every source file. Adopting it would land one large
reformatting commit that erases the maintained compact style and pollutes
`git blame` for no analysis or correctness benefit.

Tuning `rustfmt.toml` to *preserve* the hand style was investigated and rejected
as infeasible: the deviations are not reducible to a single stable config.
`use_small_heuristics = "Max"` only moved 239 → 222 hunks and introduced **new**
diffs in the opposite direction (rustfmt re-inlining chains the hand style had
broken), confirming the hand style is not internally consistent with any one
rustfmt rule. Several deviations (e.g. trailing-`;` after a `return`, some chain
packing) are only expressible via **unstable** rustfmt options that require
nightly. There is therefore no stable-rustfmt configuration that round-trips the
existing code.

## `#[rustfmt::skip]`

Because rustfmt is never run, `#[rustfmt::skip]` is not required to protect
hand-laid blocks. Reach for it only as a defensive marker on a block whose exact
layout is load-bearing (e.g. an aligned table) when a contributor is known to run
a format-on-save editor; the policy, not the attribute, is the primary guard.

## Revisiting

If the tree ever does adopt `cargo fmt` (a one-time reformat + a `fmt --check` CI
job), supersede this ADR rather than silently turning the gate on — the value here
is that the absence of a formatter is a *recorded decision*, not an oversight.

## Considered options

- **Adopt `cargo fmt` repo-wide + enforce `fmt --check`** — rejected: a 239-hunk
  reformat across 25 files that discards the deliberate compact style and churns
  `git blame`, with no parity or correctness gain.
- **Tune `rustfmt.toml` to match the hand style, then enforce it** — rejected as
  infeasible: no stable config round-trips the code (see above); enforcing it
  would still rewrite the tree.
- **Leave it undocumented (status quo before this ADR)** — rejected: an unenforced
  formatter reads as an oversight. Recording it as a decision makes the intent
  discoverable and gives a clear supersession path.
