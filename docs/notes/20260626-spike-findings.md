# Verification spike findings (2026-06-26)

De-risks the gates of [ADR-0003](../adr/0003-prism-rust-bindings.md) and
[ADR-0004](../adr/0004-own-the-index-layer.md) using the local toolchain
(Rust 1.95.0, Ruby 4.0.5 +PRISM, clang). Probes live under `spike/`.

## Environment

- `rustc`/`cargo` 1.95.0; `ruby` 4.0.5 with built-in Prism; Apple clang 21.
- Ruby gems present: `prism` 1.9.0, `rbs` 4.0.2 / 4.0.3 (reference pins 1.9.0 / 4.0.2).
- **Network: crates.io is blocked here (HTTP 403).** But `ruby-prism` 1.9.0 +
  `ruby-prism-sys` are already in the local cargo cache, so the Prism path
  builds **offline**. `ruby-rbs` is **not** cached → its Rust-API confirmation
  is network-gated.

## ADR-0003 (Prism) — CONFIRMED (Ruby and Rust)

`spike/probe_prism.rb` and `spike/prism_probe/` (Rust, built `--offline`):

- **Comments / trivia**: exposed with text + precise location (e.g. `# rigor:`
  pragma at offset 0..23). → in-source pragmas/suppression are readable.
- **Source ranges**: the `s.lenght` call is located precisely — `message_loc`
  at the `lenght` token (offset 34..40), `receiver = "s"`. Exactly what the
  tracer bullet needs.
- **Error recovery**: broken input still yields a `ProgramNode` + structured
  errors and recovers later statements.
- **Rust binding**: `ruby-prism` 1.9.0 builds offline (libprism C via clang),
  and its `Visit` trait + `CallNode::{name,message_loc,receiver}` API works.

Remaining ADR-0003 nuance: `%a{rigor:v1:...}` annotations are RBS-level, not
Ruby comments — handled by the RBS path below.

## ADR-0004 (own the index) — premise CONFIRMED; Rust API gated

`spike/probe_rbs.rb` (Ruby `rbs` gem, same grammar the Rust `ruby-rbs` crate
parses):

- RBS **carries typed method definitions** in both the parse AST and the
  resolved builder: return types, parameter types (required/optional),
  **variance** (`out T` → covariant), generics (`[U]`), block types, unions
  (`T | nil`), and real-stdlib overloads (`Integer#+ -> Integer/Float/Rational/
  Complex`, `String#upcase`).
- So gate item 1's *premise* holds: the type data exists in RBS. What remains
  (network-gated) is whether the **Rust `ruby-rbs` crate's public API** surfaces
  it directly; if not, a thin extraction layer over its parse AST suffices
  (ADR-0004 fallback). RBS annotations (`%a{...}`) are exposed on RBS AST
  members — to be confirmed in the same Rust pass.

## Scaffold

A compiling Cargo workspace (ADR-0005) was created and builds `--offline`:
`rigor-types` (interned `Type` skeleton), `rigor-parse` (the verified `ruby-prism`
wrapper), `rigor-index`, `rigor-infer`, `rigor-rules` (`Diagnostic` skeleton),
`rigor-cli` (the `rigor` binary presenting the full command surface per
ADR-0015, reporting unimplemented commands clearly).

## Next

- When network is available: add `ruby-rbs` (or git submodule), confirm its API
  exposes typed method defs + annotations; otherwise write the thin extraction
  layer. Optionally evaluate Rubydex against Ruby 4.0.
- Build the tracer bullet: lower Prism → owned AST (ADR-0012), a minimal RBS-
  backed index for core classes, expression typing + method-existence dispatch,
  the `call.undefined-method` rule, and the snapshot differential harness —
  catching `s.lenght`.
