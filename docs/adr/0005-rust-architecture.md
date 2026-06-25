# Rust architecture: separated workspace, type lattice as an interned enum

Status: accepted

rigor-rs is a Cargo **workspace** whose crate boundaries follow the analysis pipeline and the [index layer](../../CONTEXT.md) / [inference engine](../../CONTEXT.md) split:

- `rigor-types` — the type lattice and its lattice operations (join/meet/widen). Pure and independently unit-testable.
- `rigor-parse` — a thin wrapper over the `ruby-prism` crate ([ADR-0003](0003-prism-rust-bindings.md)) that supplies the AST.
- `rigor-index` — the index-layer trait and rigor-rs's own implementation built on the `ruby-rbs` parser ([ADR-0004](0004-own-the-index-layer.md)); Rubydex is an optional accelerator behind the same trait.
- `rigor-infer` — the inference engine (expression typing, narrowing, typed dispatch, RBS method-type translation, extended annotations).
- `rigor-rules` — diagnostic rules and the `Diagnostic` type.
- `rigor-cli` — CLI dispatch, reporters, and the binary.
- A differential-harness target (`xtask` or `rigor-difftest`, [ADR-0002](0002-diagnostic-set-parity.md)).

Making the index↔inference seam a **crate** boundary (not just a module) enforces that Rubydex-derived types never leak into the inference engine, keeping the backend swappable as ADR-0004 requires.

The type lattice is represented as a **single `enum Type`** whose variants cover every carrier (nominal, singleton, literal/constant, union, tuple, hash-shape, refined, difference, intersection, dynamic, top, bottom, …), **interned** in an arena and passed around as a copyable `TypeId` handle. Lattice operations are written as exhaustive `match`. This is idiomatic Rust, gives the compiler exhaustiveness checking over carriers, and makes structural equality, caching, and ownership simple given the large number of literal and union types inference produces.

## Lessons borrowed from comparable Rust type checkers

Validated against Astral's ty (`enum Type<'db>`, 30+ variants, interned) and Mago (`TAtomic` + `TUnion` with pre-computed shared atomics):

- **Union normalization is non-trivial.** Both projects route construction through a dedicated builder that flattens and de-duplicates (prevents `int | str | int`). rigor-rs should construct unions/intersections through a normalizing builder, not by assembling variants directly.
- **Pre-intern common atomics.** Mago keeps shared constants for hot atomics (`BOOL`, `INT`, …); rigor-rs should pre-intern the frequently used core types.
- **Start narrow with variants.** ty regrets special-casing `BoundMethod` / `WrapperDescriptor` ("should have been intersection + callable"). Add carrier variants reluctantly; prefer composing existing ones.
- **Don't let any one module become a monolith.** ty's `types.rs` grew to ~375 KB; split callable / member-lookup / narrowing bridges into submodules early.
- **Intern identifiers/symbols, not literal values.** pzoom interns class names but stores literal strings inline (Rust's `String` is already heap-allocated; interning literals buys nothing). Intern Ruby identifiers and symbols; keep literal carriers' values inline.
- **Prefer interned handles over an arena `'a` lifetime for the lattice.** oxc arena-allocates its AST behind a pervasive `'a`; for a *type checker* that recirculates types across passes, that lifetime becomes as painful as Salsa's `'db` ([ADR-0006](0006-incremental-computation.md)). Per-file AST lowering may still use an arena, but the type lattice stays interned-handle-based.
- **Run all rules in a single converged AST walk.** oxc dispatches 500+ lint rules in one traversal (a `Rule::run(node, ctx)` per node, plus a `should_run` per-file filter), matching the reference implementation's converged single-walk. `rigor-rules` should do one walk feeding every rule, not one pass per rule.

## Considered options

- **Type carriers as `Box<dyn TypeKind>` trait objects** — rejected: mirrors the Ruby class hierarchy but loses exhaustiveness checking and adds dynamic-dispatch cost on the hottest path.
- **Single crate, modules only** — rejected for the start: lower initial friction, but lets the index↔inference boundary erode, weakening parity control and ADR-0004's swappability.
- **Many fine-grained crates (per type, per rule)** — rejected: dependency-management overhead without early benefit.
