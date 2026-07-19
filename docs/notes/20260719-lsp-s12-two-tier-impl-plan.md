# LSP §12 two-tier — Rust implementation plan (2026-07-19)

[ADR-0029](../adr/0029-lsp-architecture.md) (accepted) fixes the WHAT: a
generation-counter `ProjectContext` invalidated by watched files/config, a
200 ms per-URI `didChange` debounce, a pre-warmed worker pool for
`publishDiagnostics` (hover/completion/symbols stay on the main thread), and
a `BufferTable` of `{bytes, version, dirty}` with temp-file `BufferBinding`.
This note fixes the HOW on rigor-rs's actual substrate: the synchronous
`lsp-server` scaffold (deliberately no async runtime), rayon as the worker
pool (ADR-0028's substrate ruling), and the existing `Mutex`-wrapped
`SidecarFolder` that the check pipeline already shares across rayon workers
(`sidecar.rs:203-206`, `main.rs:241-242`) — so no new sidecar machinery is
needed; only the "sequential loop, single worker suffices" comment at
`lsp.rs:174` retires.

## Architecture: single-writer event loop + stateless workers

1. **The event loop stays the sole owner of mutable state** (BufferTable,
   pending-debounce map, the current `Arc<ProjectContext>`) and the sole
   sender of `publishDiagnostics`. Workers never touch the connection; they
   return results over an internal crossbeam channel. This yields ordering
   and stale-result dropping by construction — the Rust equivalent of the
   reference's `SynchronizedWriter`.
2. **Loop structure**: replace `for msg in &connection.receiver` with a
   `crossbeam_channel::select!` over (a) the connection receiver, (b) the
   worker-results channel, with a timeout equal to the earliest pending
   debounce deadline (recv-deadline pattern); on timeout, dispatch the due
   publish.
3. **BufferTable** per ADR-0029: `Uri → { bytes, version: i32, dirty }`.
   `didChange` bumps the version and the deadline only — no analysis on the
   loop thread. Temp-file `BufferBinding` is materialized by the WORKER at
   analysis time (unlinked on drop), keeping the file-analysis path
   bit-for-bit.
4. **Debounce**: pending map `Uri → deadline` (last change + 200 ms).
   `didOpen` publishes immediately (fast first paint); `didClose` cancels
   pending + clears diagnostics (existing behavior). Hover / completion /
   symbols are served synchronously on the loop thread from the current
   buffer — they never wait for the debounce.
5. **Workers = `rayon::spawn`** onto the global pool, pre-warmed at
   `initialize` (build `ProjectContext`, touch the pool). A dispatch
   captures: buffer snapshot, uri, version, `Arc<ProjectContext>`,
   generation. The worker returns `Computed { uri, version, generation,
   diags }`; the loop publishes only if version AND generation still match
   current state, else drops. At most one in-flight dispatch per URI; edits
   during flight just reset the deadline (re-dispatch after the stale result
   is dropped).
6. **Sidecar under concurrency**: share the one `SidecarFolder` behind `Arc`
   as `&(dyn RubyFolder + Sync)` exactly as the check pipeline does. Folds
   are memoized; accept the mutex contention and measure before considering
   per-worker sidecars.
7. **ProjectContext (tier 1)**: `{ generation: u64, CoreIndex, SuppressSet,
   project SourceIndex (S4b) }` behind `Arc`. `invalidate!` bumps the
   generation and marks stale; the REBUILD runs lazily on a worker at the
   next dispatch (the loop keeps serving hover from the old `Arc` until the
   stamped replacement lands) — matching the reference's lazy
   `project_context.invalidate!`. Triggers: `workspace/didChangeWatchedFiles`
   (patterns `**/*.rb`, `**/.rigor.yml`, `**/Gemfile.lock`, `sig/**/*.rbs` —
   project sig dirs matter for rigor-rs per ADR-0033) and
   `workspace/didChangeConfiguration`. Buffer `didChange` NEVER invalidates.
   Dynamic registration: after `initialized`, if the client advertises
   `workspace.didChangeWatchedFiles.dynamicRegistration`, send
   `client/registerCapability` for those globs; otherwise degrade to today's
   behavior (no regression).
8. **Cross-file context for open buffers (S4b)**: tier 1 builds the project
   `SourceIndex` (check stage 2 equivalent); per-buffer analysis overlays the
   dirty buffer's contribution over its file's indexed contribution. The
   overlay mechanism must be designed against
   `analyze_with_source_and_folder`'s project-source parameter — S4b gets its
   own mini-spec before build.

## Slicing (each an independently gated PR)

**Status: S1–S4 DONE + MERGED 2026-07-19 (PRs #35–#38). S4b remaining.**
Two design refinements emerged during implementation (both applied):
generation stale-drop moved **S3→S4** (it lands with its trigger —
ProjectContext invalidation — so guard + trigger are tested as one unit); the
ProjectContext rebuild is **synchronous on the loop thread**, not the plan's
lazy-async-worker rebuild (invalidations are rare config/sig saves — a
~100-300ms inline build is acceptable and avoids a second concurrency hazard).
Also: the close+reopen version-reuse identity nit (surfaced in S3 review) is
closed in S4 by a **per-URI open-epoch** (generation is project-scoped and
doesn't bump on reopen); the LSP `initialized` notification is consumed by
lsp-server's handshake, so dynamic registration is sent at the top of
`main_loop` (post-handshake). Known limitation: `invalidate` re-reads sig-dir
CONTENT but does not re-parse `.rigor.yml` (reference-parity — restart needed
for disable/plugins/paths key changes).

- **S1** ✅ (PR #35) — BufferTable + `select!` loop refactor + worker-results
  channel, inline synchronous executor. Pure refactor, byte-identical.
- **S2** ✅ (PR #36) — 200 ms per-URI debounce (didOpen immediate, didClose
  cancels). Clockless injectable Debouncer; non-flaky tests.
- **S3** ✅ (PR #37) — rayon dispatch + VERSION stale-drop + one-in-flight/
  no-lost-update lifecycle + shared Mutex'd sidecar.
- **S4** ✅ (PR #38) — ProjectContext generation+epoch (3-axis stale-drop) +
  synchronous-rebuild watched-files/config invalidation + dynamic
  registration + reopen-epoch nit closure.
- **S4b** ⬜ — cross-file overlay for open buffers (see item 8). The ONLY
  tier-1 item left; needs its own mini-spec before build.

## Non-goals (ADR-0029 rejected options — unchanged)

Incremental UTF-16 sync (FULL stays), Salsa, TCP/socket transport, `--log`
wiring (separate small item), `::` namespace completion and completion
filters (LSP v4 features, not two-tier).

## Acceptance

ADR-0029 targets: `didChange`→publish < 250 ms p50 / < 500 ms p95 warm;
hover < 100 ms p95; cold start < 3 s; memory < 600 MB at 5 K files. Measure
S3/S4 on gitlab-foss `lib` as the 5 K-file stand-in. All existing gates
(workspace tests, harness live + snapshot, clippy in a fresh
`CARGO_TARGET_DIR`) stay binding per slice.
