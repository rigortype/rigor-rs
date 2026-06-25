# Incremental computation: defer Salsa, stay Salsa-ready

Status: accepted

rigor-rs does **not** adopt the Salsa incremental-computation framework for the MVP and CLI-first phases. Instead the [inference engine](../../CONTEXT.md) is written as **pure query functions** that take their database/context explicitly (as the first argument, with no hidden global state), and parallelism comes from rayon with file-level caching. Salsa is deferred until LSP work begins, at which point the pure query functions can be wrapped as Salsa queries with minimal disruption.

## Rationale (from comparable Rust type checkers)

The reference landscape splits on this:

- **Astral's ty and ruff are built on Salsa** (`#[salsa::tracked]` inference queries, cycle detection, a `'db` lifetime throughout). Their reported lesson: Salsa is what makes LSP incrementality high-quality, and **retrofitting it is painful** — adopt early or architect for it.
- **Mago (a production PHP analyzer) uses no Salsa**: rayon map-reduce (scan → merge codebase metadata → analyze) plus per-file content-hash caching. Sufficient for CLI/CI throughput on a dynamic language.

We take the cheap insurance from ty's lesson — keep inference Salsa-ready *by construction* (explicit db threading) — without paying Salsa's cost now: its learning curve, a `'db` lifetime that pervades every signature, and friction integrating with [Rubydex](0004-own-the-index-layer.md) (if later adopted as an accelerator), whose index model is session-rebuild, not Salsa-tracked.

The trigger to adopt Salsa is **empirical, not phase-based**. Even the LSP/editor phase starts *without* Salsa — per-file recompute on change, backed by the file-level cache and the [sidecar cache](0008-real-ruby-sidecar.md) — following oxc, whose language server is fast without Salsa (ty/ruff use Salsa; oxc and Mago do not). Salsa (or a lighter memoization layer) is wrapped over the pure query functions **only when profiling shows cross-file invalidation dominates** editor latency. This keeps the Salsa-ready insurance from costing anything until measurement justifies it.

## Considered options

- **Adopt Salsa from day 1 (ty/ruff model)** — rejected for now: best incremental LSP quality and avoids ty's painful retrofit, but the learning curve, pervasive `'db` lifetime, and Rubydex (non-Salsa) integration friction outweigh the benefit before any LSP work exists.
- **Ignore incrementality entirely (naive functions, redesign later)** — rejected: ty warns that an unstructured core is the most painful thing to retrofit. Pure query functions cost little now and preserve the option.
