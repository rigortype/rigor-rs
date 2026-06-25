# LSP architecture (extends ADR-0006)

Status: accepted

rigor-rs implements the Language Server Protocol as a single `rigor lsp [--transport=stdio]` subcommand bundled in the main binary. The server is in-process: it boots the analysis engine once at `initialize`, pre-warms the worker pool, and serves the full session lifetime — avoiding the per-keystroke Ruby VM / RBS environment startup cost that the CLI shell-out floor pays on every request. Source authority: [design/20260517-language-server.md](../../../../ruby/rigor/docs/design/20260517-language-server.md) and [design/20260517-lsp-hover-completion.md](../../../../ruby/rigor/docs/design/20260517-lsp-hover-completion.md).

## Decisions

**Transport.** stdio JSON-RPC only (`--transport=stdio`). No TCP / Unix socket in v1. `--log=PATH` is accepted and reserved; until wired, server-side logs go to stderr.

**Binary packaging.** The LSP is part of the rigor-rs binary, not a separate crate or artifact. Same config-discovery and `cache.path` path as `rigor check` ([ADR-0009](0009-config-baseline-compatibility.md), [ADR-0017](0017-analysis-cache.md)).

**Two-tier invalidation.** A `ProjectContext` (synthetic-method index, project-patched-methods registry, plugin registry, RBS environment) carries a generation counter and rebuilds only on `workspace/didChangeWatchedFiles` for `.rigor.yml`, `Gemfile.lock`, or a project `.rb` file, or on `workspace/didChangeConfiguration`. It does **not** rebuild on `didChange` for open buffers — buffer edits are virtual and cheap at single-file scope.

**BufferTable.** A URI→`{bytes, version, dirty}` map. On diagnostic publish, each dirty entry is materialised as a temp file (`BufferBinding`) so the existing file-analysis path is reused bit-for-bit ([ADR-0006](0006-incremental-computation.md) — file-level recompute; Salsa still not required). The temp file is unlinked when the buffer entry is dropped.

**Debounce.** 200 ms after the last `didChange` before a `textDocument/publishDiagnostics` notification fires. Each new `didChange` resets the timer. Diagnostics are per-buffer (single-file scope); `didClose` publishes an empty array to clear inline markers.

**Worker pool.** Pre-warmed at `initialize`, not lazily. `publishDiagnostics` dispatches into the pool; `hover` and `documentSymbol` run on the main thread (cheap, no per-buffer inference).

**Hover content** (per [lsp-hover-completion.md](../../../../ruby/rigor/docs/design/20260517-lsp-hover-completion.md)):

| Node class | Body shape |
|---|---|
| `CallNode` | receiver type + RBS signature (params → return) + source location |
| `ConstantReadNode` / `ConstantPathNode` | FQN + singleton type + source location |
| `LocalVariableReadNode` / `WriteNode` | inferred / narrowed type + bound-at line |
| `InstanceVariableReadNode` / `WriteNode` | ivar type + enclosing class |
| default | `type:` / `erased:` / `node:` |

**Completion.** `textDocument/completion` triggers on `.` and `::`. The server returns the **full unfiltered candidate set**; client-side fuzzy matching applies per editor UX. For Union receivers, method completion uses the **intersection** of each member's methods (the only methods guaranteed to dispatch on every case). Server-side visibility filter (private methods on non-`self` receivers) is the only correctness-bearing filter.

**Severity mapping.** Rigor `:error` → LSP `Error (1)`, `:warning` → `Warning (2)`, `:info` → `Information (3)`. `source` field = `"rigor"`; `code` = the rule id.

**Incrementality.** Per [ADR-0006](0006-incremental-computation.md), Salsa is **not** required. The two-tier model (stable `ProjectContext` + per-keystroke single-file recompute via `BufferBinding`) keeps per-keystroke cost off the pre-pass. Salsa is wrapped only when profiling shows cross-file invalidation dominates editor latency.

**Performance targets** (warm session, 8-core, 32 GB, 5 K-file project):

| Operation | Target |
|---|---|
| Cold start (`initialize` → first publish) | < 3 s |
| `didChange` → `publishDiagnostics` | < 250 ms p50 / < 500 ms p95 |
| `hover` | < 100 ms p95 |
| Memory steady-state | < 600 MB |

## Considered options

- **CLI shell-out per request (editor mode v1 floor)** — rejected as the LSP target: pays Ruby VM + RBS env startup (~500 ms–1.5 s) on every keystroke; `ProjectContext` and worker pool cannot be reused across requests.
- **Polyglot: Rust protocol shell + Ruby daemon (architecture C)** — rejected: wins on protocol-side latency but loses on analyzer interop, plugin-fact sharing, and codebase footprint; adds IPC schema and cross-language marshalling. Revisit only if protocol latency dominates.
- **Adopt Salsa immediately** — deferred per [ADR-0006](0006-incremental-computation.md): the two-tier model achieves per-keystroke targets without Salsa; the trigger is empirical profiling, not the LSP phase itself.
- **Incremental `didChange` (UTF-16 diff)** — deferred: `TextDocumentSyncKind::FULL` resends the whole buffer; local stdio bandwidth is irrelevant, and UTF-16 offset bookkeeping is correctness-sensitive non-trivial work queued for a later slice.
