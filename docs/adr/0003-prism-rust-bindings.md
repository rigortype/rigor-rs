# Parse Ruby with the official Prism Rust bindings

Status: accepted

rigor-rs parses Ruby source using the **official Prism Rust bindings** (the `ruby-prism` crate over libprism, the same C parser the reference implementation uses via the `prism` gem, pinned there at 1.9.0). libprism is linked statically so the single-binary distribution goal is preserved with no Ruby runtime. (Parsing itself never needs Ruby; the separate question of *executing* real Ruby for constant folding and plugins is handled by an optional sidecar — [ADR-0008](0008-real-ruby-sidecar.md).)

Using the identical parser is what makes [diagnostic-set parity](0002-diagnostic-set-parity.md) tractable: rigor-rs and the reference implementation see the same AST, so divergence cannot originate in parsing.

## Verification items

A comparison with Astral's ruff and Mago (both of which hand-wrote their parsers, partly for lossless/error-recovery reasons rigor-rs does not need) surfaced concrete things the Prism binding must be confirmed to expose, before committing:

- **Source ranges on every node** — needed for diagnostic placement and diagnostic-set parity (location half).
- **Error recovery** — does Prism continue parsing past a syntax error, so diagnostics still emit on the rest of the file? (ruff/Mago invested heavily here.)
- **Comments / trivia as recoverable tokens** — needed to read Rigor's `%a{rigor:v1:...}` RBS extended annotations and any in-source suppression pragmas. This overlaps with the [ADR-0004](0004-own-the-index-layer.md) verification spike (annotation preservation).

If Prism's spans/trivia are coarser than needed, add a thin span/trivia-recovery layer in `rigor-parse` rather than abandoning the binding.

**Status (2026-06-26): confirmed.** A spike verified comments-with-location, precise node spans (the `lenght` token in `s.lenght`), and error recovery — in both the Ruby Prism and the Rust `ruby-prism` binding (built offline; libprism C via clang). See [docs/notes/20260626-spike-findings.md](../notes/20260626-spike-findings.md).

## Considered options

- **A from-scratch or third-party Ruby parser in Rust** (e.g. lib-ruby-parser) — rejected: guarantees AST divergence from the reference and cannot track Ruby 4.0 + Prism edge cases; breaks parity at the root.
- **Pre-serialize the AST with Ruby-side Prism, consume it in Rust** — rejected: requires a Ruby runtime at analysis time, contradicting single-binary distribution.
