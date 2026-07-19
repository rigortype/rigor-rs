# rigor-rs — Current Work

The session-to-session baton: **what is in flight, what to pull next, and a
one-line ledger of what landed**. The complete per-subsystem port map is
[PORT_BACKLOG.md](PORT_BACKLOG.md); measured outcomes and narratives live in
`docs/notes/` + `docs/adr/`; history is `git log`.

**Contract (gated by `harness/docs_check.py`):** a landed/closed arc gets ONE
ledger line here — verdict + numbers + link — and its detail goes to a dated
note or ADR *first*. No status essays; this file has a hard byte budget.

Last updated: 2026-07-19.

## Now / Next

**Track B (productization) taken 2026-07-19**: coverage precision mode SHIPPED
(PR #33) + **LSP §12 tier-1 COMPLETE** (S1–S4, PRs #35–#38; two-tier loop,
200ms debounce, rayon workers + 3-axis stale-drop, ProjectContext
invalidation). **▶ NEXT: LSP §12 S4b** — the only tier-1 item left
(cross-file overlay for open buffers); needs its own mini-spec before build
(overlay the dirty buffer over its file's indexed contribution against
`analyze_with_source_and_folder`'s project-source param). Then LSP §12 moves
to v4+ features (`::` completion, visibility filters) or another track. Option
A (`Process::Status` tuple-return + destructuring, fixtures→100%) remains the
large orthogonal inference arc.
- LSP §12 known limitation (reference-parity, ADR-0029): editing `.rigor.yml`
  `disable:`/`plugins:`/`paths:` needs an LSP restart — `invalidate` re-reads
  sig-dir CONTENT but not the parsed YAML (matches the reference's
  `ProjectContext#invalidate!`). Improving on the reference here is a future
  call, out of S4 scope.
- Clippy verify MUST use `CARGO_TARGET_DIR=<fresh> cargo clippy --workspace --
  -D warnings` (the incremental cache hides `only_used_in_recursion` etc. —
  cost a CI red on PR #32).
- Coverage-tool parity lesson (binding for measurement tools): audit at NODE
  granularity — per-file histograms net over-claims out against under-claims.

Default track is **productization** (measurement-proven highest ROI; the
parity-port arc has bottomed out — see Standing conclusions):

- **ADR-0042 core migration DONE** (Slices 1–4, PRs #31/#32; ADR now accepted).
  Defect-2 unsoundness fixed, Util MERGE split, nested project-sig witnessing,
  gitlab UM 148→145, 0 FP. Remaining items deferred as NON-core: the last
  fixture gap (`Process::Status` needs tuple-return + destructuring = orthogonal
  general inference) and the guard retirement (a near-no-op consolidation).
- **ACTIVE: compat next stage** — [plan](notes/20260718-compat-next-stage-plan.md);
  Phase 0+1 DONE ([findings](notes/20260718-phase0-m1-m2-findings.md)): Phase 2
  CLOSED by measurement (M1: 0 added diags on ~17k); M2-GO slices 1–4 + 4b
  built (UM 179→148, 0 FP; declaration-driven per the set direction — no
  reflection-tier chasing). Slice 5 (namespaced singletons, ~5 sites) parked
  under [ADR-0042](adr/0042-qualified-key-index-registration.md) (proposed).
  Phase 3 DONE (1 ported / 1 absent / 1 deferred to `--bleeding-edge`) — the
  compat plan is exhausted; next work returns to the productization track
  (LSP §12, `--bleeding-edge` + CLI §7, re-pin at the v0.3.0 tag).
- **LSP §12 two-tier** — tier-1 DONE (S1–S4, PRs #35–#38); only S4b
  (cross-file overlay) left, needs a mini-spec.
  [plan](notes/20260719-lsp-s12-two-tier-impl-plan.md).
- **CLI surface from the v0.3.0 RC** — `--bleeding-edge` + severity
  profile/overrides + `coverage` precision mode DONE; remaining: plugins
  inflection probe. `--protection`/`--mutation` (ADR-63/70) + `type-scan`
  deferred by [scoping call](notes/20260719-coverage-command-scoping.md).
- **Re-pin at the v0.3.0 tag** when upstream tags it (per `UPSTREAM.md`; note
  BOTH oracle hazards there — #194 plugin path AND the non-version-scoped
  result cache). Pin `7a69f142`; tip `e447cb86` self-diff 0/0 (no tag yet).
- Deferred RC deltas (documented): interprocedural mutation floor (P6),
  plugin-only changes (no plugin engine). The UM-residual INVESTIGATION and the
  remaining RC inference deltas are absorbed into the compat plan (M2 / Phase 2).

State: harness **70 fixtures / 216 matched / 1 gap / 0 FP** (live + snapshot, vs
pin `7a69f142`; the 1 gap = fixture 68 `Process::Status`); fp_audit 0 FP on
mastodon, gitlab-foss lib (UM gaps 145) + app/models, conference-app + survey
(dependabot/rails at baseline); explain catalog 29 rules. Clippy: workspace
`-D warnings`, verify in a FRESH `CARGO_TARGET_DIR`.

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

- **2026-07-19 LSP §12 two-tier tier-1 COMPLETE** (S1–S4, PRs #35–#38, each design→implement→adversarial-review→merge) — S1 BufferTable + `select!` loop + worker-results channel (pure refactor, byte-identical); S2 200ms per-URI debounce (clockless injectable Debouncer, non-flaky); S3 rayon dispatch + version stale-drop + one-in-flight/no-lost-update + shared Mutex'd sidecar; S4 generation+epoch stale-drop (3-axis) + ProjectContext synchronous-rebuild invalidation (didChangeWatchedFiles/Configuration) + dynamic registration + reopen-nit closed. 318 workspace tests, harness 216/218 byte-identical throughout, 0 FP. Design refinements vs the plan: generation moved S3→S4 (lands with its trigger), rebuild is synchronous not lazy-async (rare events). Only S4b (cross-file overlay) left. Notes: [s1](notes/20260719-lsp-s12-s1.md)/[s2](notes/20260719-lsp-s12-s2.md)/[s3](notes/20260719-lsp-s12-s3.md)/[s4](notes/20260719-lsp-s12-s4.md).
- **2026-07-19 coverage broader over-claim audit** (PR #40, node-level) — 0 factually-wrong over-claims across 1217 new files / 186k nodes (binpacker, ruby-date/io-console/openssl/strscan, rbs, rbs-inline, mastodon app/{controllers,lib,services,…} + lib, conference-app lib); harness anchors reproduced exactly (fixtures 0, gitlab-foss 27); 8 new over-claims all provably sound (nominal-where-ref-dynamic, enumerated). Confirms the coverage command's sound-superset parity holds broadly. Also PR #39: cleared 5 test-code clippy lints outside the CI-gated `--tests`-less form. [audit](notes/20260719-coverage-broader-audit.md).
- **2026-07-19 `coverage` precision mode + MCP tool** (PR #33, 3 review rounds) — reference precision-tier scan ported on rayon (`--workers` = pool size, byte-identical any N); denominators byte-equal on ALL targets (70 fixtures + conference-app 4235 + mastodon 31381 + gitlab lib 624,233 nodes); node-level audit 0 over-claims except 27 gitlab nodes ACCEPTED as reviewer-verified sound-superset (AGENTS.md anti-convergence); 15+ over-claim defect classes found/fixed across rounds — histogram-level audits provably mask over-claims. [scoping](notes/20260719-coverage-command-scoping.md) / [results](notes/20260719-coverage-precision-mode.md).
- **2026-07-19 upstream tracking `48a26c20..e447cb86`** (10 commits: the #194 loader stack landed+closed upstream, doctor skew check, cache-validation auto) — hardened self-diff **0/0 on all four battery corpora**; plugin-loader-only surface, nothing to port; pin `7a69f142` held (no v0.3.0 tag yet). NEW oracle hazard 2 recorded in `UPSTREAM.md`: the reference result cache is not version-scoped — pin-vs-tip self-diffs REQUIRE `--no-cache` + isolated cwds.
- **2026-07-19 LSP §12 impl plan** (design only) — ADR-0029 mapped to the sync `lsp-server` + rayon substrate: single-writer `select!` loop, per-URI 200ms debounce, stale-drop via version+generation stamps, shared Mutex'd sidecar (check-pipeline pattern), slices S1–S4b. [plan](notes/20260719-lsp-s12-two-tier-impl-plan.md).
- **2026-07-19 ADR-0042 Slices 3–4** (branch `adr-0042-instance`) — qualified INSTANCE witnessing: fixture 70 shadow-sig unsoundness fix (`Status.exited?` now witnessed via the isolated qualified surface) + fixture 69 nested project-sig `.new` typo (`Outer::Inner.new.spni`); live 213→216 matched, gaps 4→1, 0 FP all corpora; narrow project-sig-only changes (configless untouched). [note](notes/20260719-adr0042-slices-3-4.md).
- **2026-07-19 ADR-0042 Slices 1–2** (branch `adr-0042-impl`) — qualified-key substrate (additive, gates byte-unchanged) + qualified singleton witnessing; fixture 68 six singleton cases byte-match incl. the ERB::Util/CGI::Util MERGE split, gitlab UM 148→145, 0 FP all core corpora; measure-first per the ratified approach. [note](notes/20260719-adr0042-slices-1-2.md).
- **2026-07-19 upstream tracking `b70adcb5..48a26c20`** (9 commits: transitive-void ADR-100 WD4, type-of plugin-env parity, IO/File line-iteration non-escaping) — hardened self-diff (fixtures 70 + gitlab lib 4676 + mastodon models + conference-app) **0 added / 0 dropped** on default surfaces (transitive void stays bleeding-edge-gated); nothing to port; pin `7a69f142` held (tag-gated).
- **2026-07-19 ADR-0042 gate SATISFIED** (branch `adr-0042-gate`, subagent-parallel) — oracle matrix (12 scenarios) → fixtures 68–70 pin the 9-gap nested-class surface; consumer inventory: no unsound consumer under alias-collapse, +2 latent-FP sites the migration fixes free, +1 real scope item (reference-name resolution) absorbed into the ADR; the s5 bare-door oracle-FP shape CLOSED (witness gate → `knows_toplevel_class` only). [deliverables](notes/20260719-adr0042-gate-deliverables.md).
- **2026-07-19 #194 root-caused: stale-gem plugin hijack** — the 3 "upstream regressions" were artifacts of `rigortype 0.2.4`'s pre-gate plugin copy hijacking the auto-wire require; corrected wave delta 0/0; oracle invocations hardened (`harness/lib.rb`, `fp_audit.py`, `UPSTREAM.md`); upstream keeps #194 for the version-skew hazard. [note](notes/20260718-upstream-rbs-inline-autowire-regressions.md).
- **2026-07-18 upstream rbs-inline auto-wire: 3 single-file regressions measured** (in-source chain inference lost ×2, interprocedural folds lost + cross-owner singleton FP on an explicit negative); pin HELD at `7a69f142`; feedback package ready. [note](notes/20260718-upstream-rbs-inline-autowire-regressions.md).
- **2026-07-18 severity-resolution machinery** (branch `severity-profile-machinery`) — `severity_profile:`/`severity_overrides:` were silently IGNORED (real incompatibility for configured projects); reference `SeverityProfile.resolve` + `SeverityStamp` ported (verbatim 28-row profile tables, family overrides, `:off` drop, internal-error bypass, memoized void-rule gate); 8/8 live config byte-diffs IDENTICAL, default output unchanged. [note](notes/20260718-severity-profile-machinery.md).
- **2026-07-18 bleeding-edge + `static.value-use.void`** (branch `bleeding-edge-void-rule`) — ADR-50 WD2 surface end-to-end (`bleeding_edge:` config, `--bleeding-edge[=LIST]`/`--no-bleeding-edge`, `show-bleedingedge` byte-identical) + the ADR-100 void rule (index void flags, value-context collector, feature-gated); probe byte-identical to the reference, default gates unchanged (flag-off). Closes the Phase-3 deferral. [note](notes/20260718-bleeding-edge-void-rule.md).
- **2026-07-18 compat Phase 3** (branch `phase3-new-rule-surfaces`) — unknown-config-key warning ported byte-exact (verbatim DidYouMean port, 13-case stdlib parity pin); `rbs.coverage.environment-build-failed` structurally absent (union-merge env cannot collapse); `static.value-use.void` deferred to the `--bleeding-edge` productization item (`:off` in every shipped profile — verified). [note](notes/20260718-phase3-new-rule-surfaces.md).
- **2026-07-18 M2-GO receiver-typing batch, slices 1–4 + 4b** (branch `m2-receiver-typing-batch`) — freeze-unwrap + Kernel#Array + rand + singleton RBS returns + declaration-driven `.new`/witnessing (the reference's `meta_new` lifts reproduced as mint-declines; witness gate → `knows_toplevel_class ∪ project-sig`); gitlab UM 179→148, mastodon models 5→3, 0 FP (one `Clusters::Instance` FP caught+fixed by the defect-2 guard); fixture 67 pins the batch. Slice 5 (namespaced singletons, ADR-0023) = the one open design call. [note](notes/20260718-m2-receiver-typing-batch.md).
- **2026-07-18 compat Phase 0 (M1+M2)** — M1 ref self-diff: RC inference deltas add 0 diags on ~17k measured (Phase 2 CLOSED ex ante); M2: gitlab UM 179 characterized — no AS-leniency in rigor-rs, silence is receiver-typing substrate; 5 GO mechanisms proven by minimal repro. [findings](notes/20260718-phase0-m1-m2-findings.md).
- **2026-07-18 compat Phase 1** (PR #24) — fixture parity 100%: S1 fold-decline nominal fallback (53×3) + S2 last_match arity `String|nil` (65×1); live+snapshot 0 gaps / 0 FP, fp_audit 5 corpora clean.
- **2026-07-18 upstream RC bump `47ec8625→7a69f142`** (80 commits) — two parity divergences closed 0 FP: `suppression.unknown-marker` (new rule, upstream `4e0ca475`) + Kernel intrinsic explicit-`Kernel.`-receiver fold (`c9d2e473`); live 188 matched / 193 ref, snapshots re-baselined, core corpora + survey FP-clean. Rest of the RC's inference precision deferred as coverage-only. [note](notes/20260718-upstream-rc-bump-47ec8625-7a69f142.md).
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
