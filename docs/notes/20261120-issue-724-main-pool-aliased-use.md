# #724 — pool-ordering audit: aliased `use` of a pool token inside `fn main` (PR #739)

`main_pool_findings` (rigor-rs#676, PR #723) masked every `use` extent
inside the production `fn main`'s body as "not a call", so the ordering
inversion could come back through an import alias:

```rust
fn main() -> ExitCode {
    use threads::configure_global_pool as go;
    if !go() { return code() }
    threads::run_command_thread(body)
}
```

`go()` names no pool token, and the `use` that introduced it was masked —
the token matcher never saw the call. The repo-wide pass already flagged
this shape for its own roots (`flagged_uses`' `use:alias-or-glob`); the
`fn main` pass did not mirror it.

## What landed (PR #739, `analysis_stack_audit.rs`, `#[cfg(test)]` only)

`pool_use_hides_token` runs on each `use` item inside `fn main`'s body
and flags the ones whose tree reaches a `MAIN_POOL_TOKENS` token behind:

- an `as` alias of the leaf — `use threads::configure_global_pool as
  go`, `use rayon::{ThreadPoolBuilder as TPB}`, and the `self` spelling
  `use …::configure_global_pool::{self as go}`;
- a glob of the token's module — `use threads::*`,
  `use threads::{run_command_thread, *}`, `use crate::threads::*`,
  `use rayon::*` (`POOL_USE_ROOTS = ["threads", "rayon"]`).

Deliberately silent (the call still carries the token, so nothing is
hidden): a plain `use` of the token, and an aliased *segment* — `use
threads as t; t::configure_global_pool()` flags on the call itself,
exactly once. The use extent stays masked from the token scan, so an
aliased `use` reports once; alias plus bare call reports both.

`MAIN_POOL_ADVICE`'s comment now names the remaining blind spot per the
issue's optional ask: a helper `fn` defined *outside* `fn main`'s body
that makes the pool call — deliberate indirection no source audit of the
body can see.

## Reviewer-verified surface (Opus, 22 extra shapes)

Flagged as required: braced/`self::`/leading-`::`/`pub(crate)` spellings,
`r#` raw identifiers, comments and line splits inside the tree, nested
`mod m { pub use … }` and block-local uses, two-step aliasing
(`use crate::threads as t` + `use t::… as go`), `super::` and
`crate::{threads::*}` globs. Silent as required: `rayon::prelude::*`,
`use threads::{configure_global_pool, x as y}` (the `as` must follow the
token directly), `#[cfg(test)]` uses in the body, `use std::thread::*`.

## Disclosed limits (non-blocking, from review)

- `use threads::{self as t, configure_global_pool}` flags although the
  token comes in unrenamed — the `self as` and token-presence checks are
  independent. Fails closed; no such shape in the repo.
- `use threads::{{*}}` stays silent (rigor-rs#654 nested-anonymous-group
  limit) — harmless: a glob never renames, so the bare call still flags.
- Globs of modules outside `POOL_USE_ROOTS` that re-export a pool token
  are invisible to a text matcher (same fixed-root limit `flagged_uses`
  carries).

Gates: 642 `rigor-cli` tests, gate.sh green, CI green on `582bcaa9`,
`fp_audit --gaps --sweep` 0 FP (test-only change). Design companion:
Fable quota-blocked; opencode/Grok substitutions also quota-blocked —
recorded in the PR body.
