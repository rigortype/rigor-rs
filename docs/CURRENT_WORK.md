# rigor-rs — Current Work

The session-to-session baton: **what is in flight, what to pull next, and a
one-line ledger of what landed**. The complete per-subsystem port map is
[PORT_BACKLOG.md](PORT_BACKLOG.md); measured outcomes and narratives live in
`docs/notes/` + `docs/adr/`; history is `git log`.

**Contract (gated by `harness/docs_check.py`):** a landed/closed arc gets ONE
ledger line here — verdict + numbers + link — and its detail goes to a dated
note or ADR *first*. No status essays; this file has a hard byte budget.

Last updated: 2026-07-17.

## Now / Next

Default track is **productization** (measurement-proven highest ROI; the
parity-port arc has bottomed out — see Standing conclusions):

- **LSP §12 two-tier** — watched-files invalidation, debounce, worker pool.
- **CLI surface from the v0.3.0 RC** — `--bleeding-edge`, `coverage --workers`,
  plugins inflection probe; full config schema (§7 remainder).
- **Re-pin at the v0.3.0 tag** when upstream tags it (per `UPSTREAM.md`; expect
  snapshots unchanged, then re-run `fp_audit`). Current pin: RC commit `47ec8625`.
- **INVESTIGATION (not a slice):** the AS-overlay-dominated undefined-method
  residual (~179 on gitlab-foss lib) — characterize before building anything.
- Deferred RC deltas (documented): interprocedural mutation floor (P6),
  `Kernel.format` explicit-receiver folds, float sprintf directives, plugin-only
  changes (no plugin engine).

State: harness 66 fixtures / 186 matched / 0 FP (live + snapshot); fp_audit 0 FP
on mastodon, gitlab-foss lib + app/models, conference-app + 27/28 survey
projects; 765 workspace tests; explain catalog 27 rules.

## Standing conclusions (do not re-litigate without new evidence)

- **Possible-nil / Tier B/C is CLOSED, not deferred** — 16/16 sampled coverage
  gaps are REFERENCE FPs; the only closing slice deletes rigor-rs's
  nameable-concrete-arm FP-safety mechanism, and `fp_audit` (which measures
  against the reference) would score that deletion 0 FP: the parity gate points
  the wrong way there. [tier-bc-track-closed](notes/20260717-tier-bc-track-closed.md).
- **Five consecutive FP-safe flow slices closed 0 survey gaps** — never build a
  coverage slice without a valid-mode `fp_audit --gaps` prediction (AGENTS.md;
  [flow-frontier](notes/20260706-flow-frontier-exhausted.md)).
- **undefined-method receiver-typing lever is exhausted**; pick new rules by
  measured corpus rule-frequency, not plausibility.
- **sig-gen arc is closed** — byte-mismatch surface 0, `--write` sound;
  remaining items are thin coverage-only. Parity model: sound-superset
  (AGENTS.md "Generative-tool parity").
- **Plugin work:** the pure-RBS bundle track is closed
  ([note](notes/20260710-pure-rbs-bundle-track-closed.md)); the code engine is a
  major separate ADR-backed track, not a slice.
- **Sidecar is functionally complete**; perf slices retired by measurement
  ([ADR-0037](adr/0037-sidecar-perf-slices-retired-by-measurement.md)).

## Build & gates

```sh
cargo build --offline && cargo test --offline        # workspace tests
ruby harness/run.rb                                  # live differential gate (0 FP)
ruby harness/run_snapshot.rb                         # reference-free gate (CI parity job)
ruby harness/run_corpus.rb <dir...>                  # scaled real-corpus gate
python3 harness/fp_audit.py --gaps <project-dir>     # survey-corpus FP/coverage audit
python3 harness/docs_check.py                        # docs budget gate
```

Reference oracle: pinned git submodule `reference/rigor` (see `UPSTREAM.md`).
`ruby -I reference/rigor/lib reference/rigor/exe/rigor check <path> --format json`
from a clean temp cwd. Survey corpora live under `/Users/megurine/repo/ruby/`.
RBS is vendored + embedded at build time (ADR-0007); `RIGOR_RBS_CORE_DIR` is the
override seam.

## Ledger (newest first; one line per arc/slice)

- **2026-07-17 docs economy** — CURRENT_WORK.md 184KB→baton + [PORT_BACKLOG.md](PORT_BACKLOG.md) split, byte-budget gate `harness/docs_check.py` + docs CI; port of upstream rigor#119 (issue #21).
- **2026-07-17 P2 `Regexp.last_match` nilable source** (MERGED `6592ead`) — gitlab lib possible-nil 169→162, 0 FP; broad P2 hypothesis REFUTED (the ref's wide firing rides its permissive `Dynamic|nil` arm — the thing our substrate deliberately cannot mint). [spec](notes/20260717-p2-optional-local-nil-spec.md).
- **2026-07-17 Tier B/C / ScopeIndexer track CLOSED** (no-go, evidence-backed) — see Standing conclusions. [note](notes/20260717-tier-bc-track-closed.md).
- **2026-07-17 ATM arc, 3 slices** (`atm-substrate-1/2`, `atm-rule`) — `call.argument-type-mismatch` both channels + per-overload/per-param RBS retention + acceptance walk; 0 FP all corpora + 27/28 survey, byte-exact messages (msgdiff gate); 3 named gaps stay open (typer substrate). [plan](notes/20260717-atm-substrate-arc-plan.md).
- **2026-07-17 C3a String-tail** (MERGED `b6d13e9`) — `self.class.name`/`to_s` tail + core-Singleton `name`; gitlab UM 200→179, 0 FP; first impl's 12 FPs caught by per-part fp_audit, design narrowed. [spec](notes/20260717-c3a-nominal-return-tail-spec.md).
- **2026-07-17 C1+C2+C5 constant-shadow gate** (MERGED) — lexical ConstantRead suppression + param-default lowering + literal `CONST=` harvest; gitlab UM 356→200 (−156, the port's largest single win), 0 FP. [spec](notes/20260717-constant-shadow-gate-spec.md).
- **2026-07-16 `def.ivar-write-mismatch`** (MERGED `a2098d7`) — ivar-write lowering + rescue binding + Kernel cast fallback + collector; gitlab ivar gaps 2→0, 0 FP. [spec](notes/20260716-ivar-write-mismatch-spec.md).
- **2026-07-16 literal-tail return folding** (MERGED `0721943`) — interprocedural singleton-method literal fold (depth-16, ancestry-scoped); gitlab always-truthy 28→16, 0 FP. [spec](notes/20260716-literal-tail-fold-spec.md).
- **2026-07-16 v0.3.0-RC arc: pin `47ec8625` + 7 slices, ALL MERGED** — syntactic rules (dup-hash-key, return-in-ensure, suppression.*, `Node::Lambda`), MutationWidening (killed 2 measured FPs), implicit-self dispatch + `p`/`pp`, scalar HashShape keys + projection folds, Kernel `format`/casts folding, `raise-non-exception` + `class_ordering`, `shadowed-rescue-clause` + rbs.rs nesting root-fix. **v0.3.0 rule surface fully ported.** [specs](notes/20260716-v030-upstream-gap-survey.md).
- **2026-07-11 MCP `sig_gen` tool** (MERGED `e7ae83e`) — read-only, byte-identical to CLI `--print --format json`; `rigor_coverage` deferred (needs coverage command).
- **2026-07-11 sig-gen arc CLOSED (13 slices)** — `erase_to_rbs` `ee60d41` → `--print` `7f01322` → return-union `929ff74` → singletons `8db1bed` → `--write` create `af4f42f` → initialize stub `25d82eb` → `--diff` `968c10c` → module_function `95f490d` → Writer merge+LayoutIndex `c02dcdc` → env classification `a268a6c` → `--overwrite` `9e85e07` → qualified naming `0f122b6` → Data/Struct shells `33f9436`. 0 shared-method mismatch on the full sweep; `--write` sound; remaining thin coverage-only (attr_*, merge-path shells, `--params=observed`).
- **2026-07-11 `--params=observed` SUBSTRATE-BLOCKED** (not built) — needs the ScopeIndexer; a literal-only port is a net regression. [note](notes/20260711-siggen-params-observed-substrate-blocked.md).
- **2026-07-11 conditional-assign nilability BUILT, NOT merged** (branch `flow-cond-assign-nilability` `7b7fe3d`) — correct + FP-safe, closes 0 survey gaps (4th consecutive); ADR-0038 Slice 2 substrate preserved on branch. [spec](notes/20260711-conditional-assign-nilability-spec.md).
- **2026-07-11 coverage frontier re-measured** — bounded wins exhausted; next is deep-substrate or productization. [note](notes/20260711-coverage-frontier-remeasured.md).
- **2026-07-10 pure-RBS bundle track CLOSED** — `activesupport-core-ext` is the only pure-RBS plugin and is byte-current. [note](notes/20260710-pure-rbs-bundle-track-closed.md).
- **2026-07-10 MCP triage+annotate** (MERGED `c6c1094`); **ADR-72 Gemfile.lock auto-overlay** (MERGED `96d7f47`) — Rails projects auto-get the AS overlay, FP-safe by construction.
- **2026-07-06 possible-nil/ivar expansion CONFIRMED zero-EV** — rigor-rs is at the FP-safe optimum for free (the ref's ADR-58 suppresses the 109 FPs ivar typing manufactures).
- **2026-07-06 remaining-commands assessment** — trace/coverage/type-scan substrate-blocked or structurally divergent (sig-gen since built).
- **2026-07-06 productization cluster, all merged** — triage hints (portable subset), case/if-union + tuple-projection precision slices, `annotate`, HashShape typing, type-display layer + value-pinned arrays (PR #12), `triage` (ADR-23), `diff`, config-audit (PR #8), ADR-22 baseline area complete (`regenerate`/`drift`/`prune`/`--baseline-strict`), `check <dir>` + config `paths:` ([ADR-0040](adr/0040-directory-path-argument-support.md)).
- **2026-07-06 flow-substrate arc** — [ADR-0038](adr/0038-flow-substrate-incremental-narrowing.md) Slice 1a landed; [ADR-0039](adr/0039-shape-typing-tier.md) Slice 1a landed, Tuple deferred by measurement; [ADR-0041](adr/0041-project-method-nilable-return.md) deferred by measurement (branch `tier-bc-nilable-return`). Strategic finding: no cheap FP-safe flow wins left. [note](notes/20260706-flow-frontier-exhausted.md).
- **2026-07-06 coverage-gaps + fp-audit sweeps** — `fp_audit.py --gaps` added; parenthesized-receiver unwrap (+13, `b98c658`); ERB-template skip (~58 FPs, `00c8734`); **0 FP across the full surveyed corpus (~4000+ files, 20+ libs)**.
- **2026-07-05 fp_audit tool + 4 real FP clusters fixed** (singleton-class bodies, `Kernel#gem`, `Regexp.compile` alias, singleton-def receiver) — 0 FP across 12 corpora; perf slices retired ([ADR-0037](adr/0037-sidecar-perf-slices-retired-by-measurement.md)).
- **2026-07-05 sidecar arc COMPLETE** (merged `2aa5ce6`) — [ADR-0036](adr/0036-ruby-sidecar-default-reversal.md) full-fidelity-default + `--ruby` surface, transport/handshake/fold worker/exit-69 teeth, allowlisted parity-safe folds; measured delta flat ~0.06s, diagnostic sets identical.
- **2026-07-05 RBS ingestion legs resolved** — project `sig/` ([ADR-0033](adr/0033-project-sig-ingestion.md)) + `rbs collection` ([ADR-0034](adr/0034-rbs-collection-ingestion.md)) implemented; gem-sig + inline RBS deferred with rationale ([ADR-0035](adr/0035-inline-rbs-deferred.md)); pin v0.2.6→v0.2.7 (no drift).
- **2026-07-01 productization + rules** — rayon parallelism (~2.4×, byte-identical), LSP v1/v2 (diagnostics/hover/completion/symbols), MCP server, `flow.always-truthy-condition` + first ADR-0022 substrate, `call.unresolved-toplevel`, dead-assignment block-pass fix. Net-new-rule coverage exhausted by v0.2.6 tally.
- **2026-06-30 rustfmt stance recorded** ([ADR-0032](adr/0032-source-formatting-policy.md)) — hand-formatted, clippy is the blocking gate.
- **2026-06-27 v0.0.1 release prep** — version/CI/gem/Homebrew wired, tag-gated on maintainer infra; musl/Windows targets wired pending a real tag run; clippy `-D warnings` blocking; snapshot-mode CI parity job.
- **2026-06-26 leniency alignment + coverage passes** — undefined-method witnesses only RBS-known core-surface receivers (matches the ref's tier-4 leniency; 2 FPs fixed); lowering-traversal +54, interpolated strings, singleton witnessing + cross-file project index, block-overload return recovery; external design audit R1–R5 all addressed.
