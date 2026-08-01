# LSP config reload — `.rigor.yml` re-parsed on every structural invalidate (2026-08-01)

Closes the LSP §12 known limitation carried since [S4](20260719-lsp-s12-s4.md):
editing `.rigor.yml` needed an editor restart. Blast radius:
`crates/rigor-cli/src/lsp.rs` + `crates/rigor-cli/src/config.rs`. No
`rigor_index` / `rigor_infer` / `rigor_rules` / `rigor_parse` change; the `check`
pipeline's observable behaviour is byte-unchanged (gates below).

## 1. What the reference does TODAY (not what the S4 note said)

Read at the pin `v0.3.1` (`c39e6675`) **and** re-checked against upstream
`origin/master`, because upstream shipped LSP work after the pin.

- `lib/rigor/language_server/project_context.rb` takes `configuration:` at
  construction and never re-reads it. `#invalidate!` bumps `@generation` and drops
  `@environment` + `@project_scan`; the retained `@configuration` is what the
  rebuilt `Environment.for_project(...)` is fed. Its own comment enumerates the
  invalidation contract and the config is not in it.
- `server.rb#handle_did_change_configuration(_params)` — the parameter is
  underscore-prefixed — "ignores the payload and invalidates the context so the
  next read picks up any external config changes". The rebuild re-reads
  **signature-dir content**; the parsed YAML is not touched.
- Upstream delta since the pin on this surface is **one commit**, `9594732b`
  "Publish the whole project to open buffers on save" (#246): a `didSave`
  whole-project round, an `IncrementalSession` on `ProjectContext`, a `dirty?`
  bit on `BufferTable`. It adds state to `ProjectContext` and **still does not
  re-read the configuration**. So the parity baseline did NOT move here; the S4
  note's description of the reference is still accurate.

## 2. Decision: BEAT the reference

**Decided: re-parse `.rigor.yml` on every structural invalidation.**

Legitimate here in a way it would not be for a rule: the LSP is not the `check`
pipeline, it has no diagnostic-set parity obligation, and neither harness nor
`fp_audit` can see it — so "match the reference" buys nothing measurable, while
matching it costs a real user a real restart.

The decisive argument is that matching was **worse than doing nothing**. The
watcher was already registered on `**/.rigor.yml`; the event already classified
as `Structural`; `invalidate` already rebuilt the `CoreIndex`, re-harvested the
whole overlay, bumped the generation and re-published every open buffer — and
then published a byte-identical stale answer. A user who edits `disable:`, saves,
and watches the markers *not* change reads that as "rigor ignored my config", not
as "restart me". The reference's CLI-first posture (its AGENTS.md: "Do not assume
an LSP server or a long-running daemon") makes a restart cheap in its own mental
model; it is not cheap in an editor session that has paid a ~100–300 ms RBS build.

**What a user moving between the two implementations notices:** on rigor-rs, an
edited `.rigor.yml` takes effect on the next publish; on the reference, it takes
effect after restarting the language server. Everything else about the config is
identical — same keys, same semantics, same resulting diagnostics. The
divergence is one-directional (rigor-rs is strictly more responsive) and
self-cancelling if upstream ever adopts it: nothing here encodes a reference gap
as a guard, so a converging upstream needs no rigor-rs change (AGENTS.md's
anti-convergence rule).

## 3. The invalid-config case — the one that actually mattered

An editor writes `.rigor.yml` on every save, so a server that reloads on save
sees half-written YAML constantly. `Config::load`'s answer — silently substitute
`Config::default()` — is right for a one-shot `check` (the run is ending, warn on
stderr and analyze) and **wrong** for a long-lived reader: it would drop the
user's entire `disable:` list mid-keystroke and flood the buffer with markers
that vanish again the moment the file parses.

**Behaviour, made explicit rather than incidental:**

| on disk | in force | disclosure |
| --- | --- | --- |
| parses | the new config | INFO **only** if it was previously broken ("reloaded") |
| absent (deleted) | the DEFAULTS | none — a missing config is not an error |
| exists, will not parse | the **last good** config | WARNING **once**, on the good→broken transition |
| unreadable (perms, a directory) | the last good config | as above |
| broken at STARTUP | the defaults (there is no last good) | WARNING naming DEFAULTS, not "last good" |

Two things this forced:

- **`ConfigRead` in `config.rs`** — `Parsed` / `Absent` / `Unreadable` /
  `Malformed`. `Config::load` deliberately collapses all four; the LSP cannot,
  because *absent* means "the defaults genuinely ARE the configuration" (a
  DELETE must reload to defaults) while *malformed* means "I do not know what the
  configuration is" (keep the last good one). `Config::load` is now expressed on
  top of `read`, so there is ONE loader and no second parse path that could
  drift; its stderr wording is unchanged.
- **Transition-only disclosure.** A `config_broken` bit on `Session`, seeded from
  the startup read. One modal per save of a file the user is still fixing is
  worse than the staleness the slice removes.

**Rejected: publishing a diagnostic on the `.rigor.yml` URI.** It needs a range
this loader does not compute, and it would make the LSP own publishing *and
clearing* markers on a non-Ruby URI that may not even be open.
`window/showMessage` is already this server's disclosure channel (sidecar
posture, overlay scale guard), so the reload joins it.

## 4. Implementation — reusing S4/S4b, not adding a second mechanism

- `invalidate` calls `reload_config` **first**, then rebuilds. Ordering is
  load-bearing and tested: `build_core_index` consumes `plugins:` +
  `signature_paths:`, `build_overlay` consumes `paths:` + `exclude:`, and
  `swap_project` consumes `disable:` + the severity axes — a reload anywhere later
  ships a context built half from each config.
- `invalidate` now returns `Vec<(MessageType, String)>` (one invalidation can owe
  both a config-state and a guard-posture disclosure) instead of `Option<String>`.
- **No new concurrency reasoning.** Rebuilds are synchronous on the loop thread
  (S4's decision), so there is no rebuild in flight for a reload to race. The only
  in-flight work is diagnostics workers, and the reload rides inside the same
  `swap_project` generation bump that already covers an index rebuild: a worker
  dispatched under the old config is generation-stale, dropped, and re-dispatched
  by `handle_result`'s three-axis guard. That case is the ordering test.
- **A `.rb` save does NOT reload.** It takes the cheap `reharvest_sources` path
  (S4b review N3); a source file cannot change the config, and routing it through
  the config read would reintroduce the 121 ms loop-thread stall that path exists
  to avoid. Asserted.
- Startup now reads `<root>/.rigor.yml` through the same `read_project_config`
  the reload uses — in production `root` is `.`, so this is byte-identical to the
  `Config::load(None)` cwd discovery it replaces, and startup/reload can no longer
  disagree about which file the project config is.

**Harness change worth flagging to a reviewer:** `start_project_with_config` used
to inject a PARSED `Config` with no file behind it. That was "the same thing minus
the file" only while the config was read exactly once — under a reload the
injected config would be read away on the first invalidation, a
production divergence the assertions could not see. The harness now writes real
YAML under the temp root and the server reads it through the production loader.

## 5. Tests — protocol level, each re-broken once

Eight new tests (7 protocol-level via the in-memory `Connection`, 1 unit on
`Config::read`). Non-vacuity proven by seven mutations, each reverted:

| mutation | fails |
| --- | --- |
| `invalidate` stops reloading (pre-slice behaviour) | all 7 protocol tests |
| malformed adopts the DEFAULTS | malformed-keeps-last-good, broken-at-startup |
| warn on every broken save, not the transition | malformed-warns-once |
| `Absent` treated as broken | delete-reloads-to-defaults (+ 6 more) |
| drop the generation axis from the stale-drop | reload-beats-a-worker-in-flight |
| reload placed AFTER the rebuild | all 7 protocol tests |
| `read` collapses `Malformed` into `Absent` | the `Config::read` unit test |

## 6. Gates

- `cargo test --offline --workspace` — green.
- `CARGO_TARGET_DIR=<fresh> cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `ruby harness/run.rb` + `run_snapshot.rb` — **79 fixtures / 0 unregistered FP /
  3 gaps / 1 registered divergence**, unmoved.
- `python3 harness/fp_audit.py --gaps --sweep` — **0 FP / 9204 files**, gap table
  reproducing `CORPUS.md` entry-for-entry. An LSP-only change moving either would
  have meant something leaked into the check pipeline.
