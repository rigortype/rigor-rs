# Ship the RBS stdlib pre-parsed and embedded; merge project and gem RBS at analysis time

Status: accepted

rigor-rs ships Ruby's core/stdlib RBS as **vendored RBS, pre-parsed at build time and embedded** into the binary (a `rigor-vendored`-style crate). This keeps startup instant and preserves the single-binary, Ruby-free distribution of [ADR-0001](0001-rust-reimplementation-strategy.md) / [ADR-0003](0003-prism-rust-bindings.md). At analysis time the type environment is the **merge** of: embedded stdlib RBS ⊕ project `sig/` ⊕ gem RBS (bundler / `rbs_collection` auto-detection) ⊕ inline RBS, with precedence matching the reference implementation's `signature_paths` model. Version alignment is by `target_ruby` (selecting the stdlib RBS matching the project's Ruby version).

No new stub format is invented: **RBS is Ruby's typeshed-equivalent.** Users add gem stubs by placing RBS under `sig/` or via `rbs_collection` — the same surfaces the reference already supports.

## Rationale

Convergent evidence from comparable analyzers: **pzoom** embeds `.phpstub` files (rust-embed) plus JSON dictionaries; **ty** vendors typeshed and processes `.pyi` at build time into a loadable, cached form; **selene** embeds default stdlib definitions and lets users drop overrides in the project, merged via a `std = "lua51+custom"` chain (base inheritance + wildcards). All three embed defaults, allow project/user overrides, merge, and version-target. rigor-rs follows the same shape, with RBS as the (given) format.

Pre-parsing at build time matters for the performance + low-startup-latency driver: the large stdlib is the dominant cold-start cost, so paying it once at build and embedding the parsed form avoids it per run. pylyzer's experience warns that *incomplete* stdlib stubs turn into false positives and erode trust — so stdlib coverage and its merge precedence are correctness-critical (they feed [diagnostic-set parity](0002-diagnostic-set-parity.md)), not an implementation detail.

## Considered options

- **Ship raw RBS, parse on first run, cache to disk** — rejected: smaller binary but slow first run and cache-invalidation complexity, against the low-startup-latency driver.
- **Invent a compact custom stub format** (selene/pzoom style) — rejected: RBS already *is* the format; a custom one would fragment rigor-rs from the Ruby ecosystem and from the reference's own RBS sources.
