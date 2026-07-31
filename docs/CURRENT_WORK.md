# rigor-rs — Current Work

The session-to-session baton: **what is in flight, what to pull next, and a
one-line ledger of what landed**. The complete per-subsystem port map is
[PORT_BACKLOG.md](PORT_BACKLOG.md); measured outcomes and narratives live in
`docs/notes/` + `docs/adr/`; history is `git log`.

**Contract (gated by `harness/docs_check.py`):** a landed/closed arc gets ONE
ledger line here — verdict + numbers + link — and its detail goes to a dated
note or ADR *first*. No status essays; this file has a hard byte budget.

Last updated: 2026-07-31.

## Now / Next

**Track B (productization) 2026-07-19/25** — all in the ledger: coverage
precision mode (#33), LSP §12 COMPLETE (S1–S4b) + stage-3 parity tail (#44),
MultiWrite substrate (#46/#47), LSP `exclude:` parity (#45). ▶ NEXT: the `BigMath` half of the RBS ingestion asymmetry
(`vendored_gem_sigs` is now vendored,
[note](notes/20260725-multiwrite-substrate-s2.md)); LSP config reload
(`.rigor.yml` is read once at startup — the highest-value remaining LSP item);
LSP v4+ (`::` completion, visibility filter, `rootUri`); or `-> self` on an
INSTANCE method (only block returns resolve it; the `SELF_RETURN` sentinel makes
it small — measure first). The sweep set is now CODIFIED
(`harness/sweep-corpora.yml` + `fp_audit.py --sweep`).
- LSP §12 known limitation (reference-parity, ADR-0029): editing `.rigor.yml`
  needs an LSP restart — `invalidate` re-reads sig-dir CONTENT, not the parsed
  YAML (matches `ProjectContext#invalidate!`). Beating the reference here is a
  future call.
- Clippy verify MUST use `CARGO_TARGET_DIR=<fresh> cargo clippy --workspace --
  -D warnings` (the incremental cache hides `only_used_in_recursion` etc. —
  cost a CI red on PR #32).
- Coverage-tool parity lesson (binding for measurement tools): audit at NODE
  granularity — per-file histograms net over-claims out against under-claims.

Default track is **productization** (measurement-proven highest ROI; the
parity-port arc has bottomed out — see Standing conclusions):

- **CLOSED arcs** (in the ledger; do not re-open): ADR-0042 core migration
  (PRs #31/#32, accepted) and the compat next-stage plan (Phases 0–3 done,
  exhausted — [plan](notes/20260718-compat-next-stage-plan.md)). Their last
  remnant, fixture 68, closed in #47.
- **LSP §12 two-tier** — COMPLETE (S1–S4b) + the stage-3 parity tail (#44).
  [plan](notes/20260719-lsp-s12-two-tier-impl-plan.md).
- **CLI surface from the v0.3.0 RC** — `--bleeding-edge` + severity
  profile/overrides + `coverage` precision mode DONE; remaining: plugins
  inflection probe. `--protection`/`--mutation` (ADR-63/70) + `type-scan`
  deferred by [scoping call](notes/20260719-coverage-command-scoping.md).
- **Pin is `v0.3.1`** (+ vendored rbs 4.1.0). Upstream master is +49 with no tag
  and a measured delta of ONE diagnostic, so the next re-pin waits for a tag
  (`UPSTREAM.md`, all THREE oracle hazards).
- Deferred RC deltas (documented): interprocedural mutation floor (P6),
  plugin-only changes (no plugin engine). The UM-residual INVESTIGATION and the
  remaining RC inference deltas are absorbed into the compat plan (M2 / Phase 2).

State: harness **76 fixtures / 0 FP / 3 gaps** (live + snapshot, vs pin
`v0.3.1`; the 3 are fixture 72's `rule: null` parse diagnostics). Standing sweep
`fp_audit.py --gaps --sweep`: **0 FP across 9204 files**, 8 corpora, baselines in
`harness/CORPUS.md`. Neither sweep tool sees project-`sig/` behaviour. Clippy:
workspace `-D warnings`, verify in a FRESH `CARGO_TARGET_DIR`.

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
ruby harness/run_corpus.rb                           # scaled real-corpus gate
python3 harness/fp_audit.py --gaps --sweep           # STANDING sweep set (0 FP bar)
python3 harness/docs_check.py                        # docs budget gate
```

Reference oracle: pinned git submodule `reference/rigor` (see `UPSTREAM.md`);
run from a clean temp cwd. Sweep membership: `harness/sweep-corpora.yml`
(corpora under `/Users/megurine/repo/ruby/`). RBS is vendored + embedded at
build time (ADR-0007); `RIGOR_RBS_CORE_DIR` is the override seam and
`harness/vendor_rbs.py` regenerates the tree.

## Ledger (newest first; one line per arc/slice)

- **2026-07-31 survey FP triage — 24 → 0, nine root causes** — the pin bump's side finding, closed. Six of the nine are scoping/unit bugs an all-ASCII well-formed fixture corpus structurally cannot see: **columns counted in scalars, not Prism's BYTES** (8 FPs — same diagnostic, wrong column, on every emoji/kana spec line; a standing `TODO(spec)`); **semantic rules ran on Prism's RECOVERED tree** for unparseable files (4; the reference bails before `ScopeIndexer`); **top-level locals leaked into `def` bodies** via the flat name-keyed env (4; a driver `s = 'a'` typed a later `def is_anagram(s, t)`'s parameter); project reopenings of CORE classes invisible + top-level `def IO.foo` not registered (3); a namespaced project class losing to a same-named RBS class (1); dead-assignment reaching into the PARAMETER list (1); and two flow facts the reference invalidates — an argument the callee mutates, and `rescue => e` inside a block (2). Also **vendored the reference's `data/vendored_gem_sigs/` + `data/core_overlay/`** (1 FP — the ingestion-surface follow-up) — `prism` EXCLUDED: supplementing a sig set we do not vendor added 8 fresh `Prism.parse` FPs. 7 corpora / ~9150 files **0 FP at BOTH pins**, sweep-set output diff EMPTY both directions, harness 76 fixtures / 232 matched / 0 FP. [note](notes/20260731-survey-fp-triage-24.md).
- **2026-07-31 standing FP sweep set CODIFIED** — `harness/sweep-corpora.yml` is the single membership list for BOTH `fp_audit.py --sweep` and `run_corpus.rb` (each entry carries the `why:` it earns its place with; an absent corpus reports SKIPPED, never drops silently). Closes the ▶NEXT item the 24-FP triage left. Also fixed `run_corpus.rb`'s `REFERENCE_RIGOR_DIR` default — it pointed at the WORKING checkout, hazard 3 baked into a gate. Baseline **0 FP / 9204 files / 8 corpora** ([CORPUS.md](../harness/CORPUS.md)).
- **2026-07-31 upstream HEAD survey (`v0.3.1`→`ece06a0d`, 49 commits) + Tuple set-op folds** — the whole delta moves **1 diagnostic** on 9204 files, one rigor-rs never emitted; pin STAYS at `v0.3.1` (no tag past it). Ported the one real item (#121 `a2867efd`): set ops fold on the pinned Tuple with `eql?` not `==` (`[1] & [1.0]` is EMPTY); NaN + OOB `at` decline. **0 FP / 9204 files**; no fixture until the pin passes `a2867efd`. [note](notes/20260731-head-survey-and-set-op-folds.md).
- **2026-07-31 upstream pin `v0.3.0 → v0.3.1` + vendored rbs `4.0.3 → 4.1.0`** (one commit — an oracle on other core signatures than the port reads is not an oracle; same day, via an intermediate `7a69f142 → v0.3.0` tag bump that was behaviour-neutral bar the ADR-93 inline-RBS auto-wire, [note](notes/20260731-upstream-bump-7a69f142-v030.md)) — **0 FP / 9153 files, gaps net −2** (mastodon 49→48, gitlab 330→329, concurrent 87→86). 4.1.0's rewritten signatures broke two things, both FIXED rather than accepted as the survey assumed: bounded method type params (`Array#fetch`'s `[I < _ToInt] (I index)` admitted everything) now resolve to their bound, and `-> instance` on an INSTANCE method (`Hash#compact`) now resolves to the receiver's class via the `SELF_RETURN` call-site sentinel. New `harness/vendor_rbs.py` makes the vendoring recipe executable — proven by reproducing the committed 4.0.3 tree byte-for-byte before writing 4.1.0. Upstream logic delta itself: ZERO. [note](notes/20260731-upstream-pin-v031-rbs41.md) / [survey](notes/20260731-v031-preflight-survey.md).
- **2026-07-25 MultiWrite substrate, slices 1+2** (PRs #46/#47) — `a, b = rhs` had NO arena lowering, so `collect_flow_writes` never saw the rebind: a MEASURED live `check` FP. Added `Node::MultiWrite` (+ `target_exprs` for locals inside non-local targets — an FP the harness, unit tests and 3 corpora all missed) + a rule-for-rule port of `MultiTargetBinder`; ~32k-file sweep 2 removed / 5 added / 0 new FPs. Slice 2 then closed the LAST fixture gap (68) by surviving RBS tuple returns as `RbsReturnShape{Class|Tuple|Unknown}` beside an untouched `method_signature`, + Pass 2b minting ids for the 17 classes an RBS tuple names. **An FP found and fixed mid-build**: naive witness widening fired `Gem::Version#segments` where the oracle is silent — gated the WITNESS on `is_declaration_only_class`. 14 corpora / 35,706 files bit-identical. [spec](notes/20260725-multiwrite-substrate-spec.md) / [s1](notes/20260725-multiwrite-substrate-s1.md) / [s2](notes/20260725-multiwrite-substrate-s2.md).
- **2026-07-25 LSP `exclude:` parity** (PR #45) — an excluded file opened in the editor published markers `check` never produces. Decision rule: a buffer is excluded iff EVERY discovery spelling of it is excluded, expressed as 3 tiers — overlay membership (fast path), all-roots × 3 name forms (decoded/named/canonical, `all` load-bearing), then a confirm pass over the SHARED `discovery_spellings` walk so it cannot drift from `project_files`. Review round 1 found 3 regressions (symlinked `.rb`, symlink whose target is excluded, overlapping roots — the first-match + canonicalize-only gate); matrix widened 24→144 runs (+ symlink and multi-root axes). `check` byte-identical under 3 configs incl. an invalid glob. [note](notes/20260725-lsp-exclude-parity.md).

- **2026-07-25 LSP stage-3 parity tail** (PR #44) — the LSP never applied the ADR-8 SeverityStamp: a rule resolved `off` still published markers `check` DROPS (PRESENCE mismatch), severities were authored not profile-resolved, bleeding-edge unplumbed. The stamp now rides `ProjectContext` beside `disable:` (rebuilt by `invalidate` under the S4 generation guard); composition order verified against `main.rs` statement-by-statement. 4 E2E tests vs real `check` output, each proven non-vacuous by re-breaking. Remaining divergences enumerated in the note. [note](notes/20260725-lsp-stage3-parity.md).

- **2026-07-19 LSP §12 two-tier COMPLETE (S1–S4b)** (PRs #35–#38/#42/#43 + `16bfb9e`) — `select!` loop + BufferTable; 200ms debounce; rayon workers, 3-axis stale-drop, no-lost-update; generation-counter `ProjectContext` invalidation; cross-file overlay (swap-and-rebuild under a hysteresis scale guard; incremental `.rb` re-harvest 111ms→0.2ms under a conservative held-entry-only rule). Harness 216/218 0 FP throughout. 6 adversarial-review rounds found 8 real defects (3 live FPs); the mandated differential test (entries + ORDER + AST fingerprint × `paths:` shapes × root spellings) found a 4th itself. [plan](notes/20260719-lsp-s12-two-tier-impl-plan.md) / [S4b spec](notes/20260719-lsp-s4b-overlay-mini-spec.md) / notes `20260719-lsp-s12-s{1..4,4b}.md`.

- **2026-07-19 baseline drift/prune positional-roots + scope guard** (PR #41) — found via a real-product baseline check (conference-app, no `.rigor.yml`): drift/prune ignored positional roots (config `paths:` only), so `generate .` wrote 1956 diags but `drift .` silently reported all 98 buckets "Cleared" (a `prune`-acts-on-it footgun). Fix: (a) drift/prune honor positional roots like generate (additive; no-positional path unchanged); (b) `paths_explicitly_declared()` guard refuses a scope-less audit vs a non-empty baseline (exit 64). 4 regression tests, 0 FP. [note](notes/20260719-baseline-drift-roots-fix.md).
- **2026-07-19 upstream tracking `e447cb86..ff6b6158`** (10 commits: ADR-58 massign-ivar seeding, ADR-67 WD6 call-site param inference, handoff docs) — hardened self-diff **0 added / 0 dropped** on all four battery corpora, runtimes symmetric. STILL no v0.3.0 tag. Both clusters are substrate-blocked large arcs AND precision-additive (0 new diagnostics; cluster 2 off-by-default) — pin `7a69f142` HELD, nothing to port.
- **2026-07-19 coverage broader over-claim audit** (PR #40, node-level) — 0 factually-wrong over-claims across 1217 new files / 186k nodes (binpacker, ruby-date/io-console/openssl/strscan, rbs, rbs-inline, mastodon app/{controllers,lib,services,…} + lib, conference-app lib); harness anchors reproduced exactly (fixtures 0, gitlab-foss 27); 8 new over-claims all provably sound (nominal-where-ref-dynamic, enumerated). Confirms the coverage command's sound-superset parity holds broadly. Also PR #39: cleared 5 test-code clippy lints outside the CI-gated `--tests`-less form. [audit](notes/20260719-coverage-broader-audit.md).
- **2026-07-19 `coverage` precision mode + MCP tool** (PR #33, 3 review rounds) — reference precision-tier scan ported on rayon (`--workers` = pool size, byte-identical any N); denominators byte-equal on ALL targets (70 fixtures + conference-app 4235 + mastodon 31381 + gitlab lib 624,233 nodes); node-level audit 0 over-claims except 27 gitlab nodes ACCEPTED as reviewer-verified sound-superset (AGENTS.md anti-convergence); 15+ over-claim defect classes found/fixed across rounds — histogram-level audits provably mask over-claims. [scoping](notes/20260719-coverage-command-scoping.md) / [results](notes/20260719-coverage-precision-mode.md).
- **2026-07-19 upstream tracking `48a26c20..e447cb86`** (10 commits: the #194 loader stack landed+closed upstream, doctor skew check, cache-validation auto) — hardened self-diff **0/0 on all four battery corpora**; plugin-loader-only surface, nothing to port; pin `7a69f142` held (no v0.3.0 tag yet). NEW oracle hazard 2 recorded in `UPSTREAM.md`: the reference result cache is not version-scoped — pin-vs-tip self-diffs REQUIRE `--no-cache` + isolated cwds.
- **2026-07-19 ADR-0042 Slices 3–4** (branch `adr-0042-instance`) — qualified INSTANCE witnessing: fixture 70 shadow-sig unsoundness fix (`Status.exited?` now witnessed via the isolated qualified surface) + fixture 69 nested project-sig `.new` typo (`Outer::Inner.new.spni`); live 213→216 matched, gaps 4→1, 0 FP all corpora; narrow project-sig-only changes (configless untouched). [note](notes/20260719-adr0042-slices-3-4.md).
- **2026-07-19 ADR-0042 Slices 1–2** (branch `adr-0042-impl`) — qualified-key substrate (additive, gates byte-unchanged) + qualified singleton witnessing; fixture 68 six singleton cases byte-match incl. the ERB::Util/CGI::Util MERGE split, gitlab UM 148→145, 0 FP all core corpora; measure-first per the ratified approach. [note](notes/20260719-adr0042-slices-1-2.md).
- **2026-07-19 upstream tracking `b70adcb5..48a26c20`** (9 commits: transitive-void ADR-100 WD4, type-of plugin-env parity, IO/File line-iteration non-escaping) — hardened self-diff (fixtures 70 + gitlab lib 4676 + mastodon models + conference-app) **0 added / 0 dropped** on default surfaces (transitive void stays bleeding-edge-gated); nothing to port; pin `7a69f142` held (tag-gated).
- **2026-07-19 ADR-0042 gate SATISFIED** (branch `adr-0042-gate`, subagent-parallel) — oracle matrix (12 scenarios) → fixtures 68–70 pin the 9-gap nested-class surface; consumer inventory: no unsound consumer under alias-collapse, +2 latent-FP sites the migration fixes free, +1 real scope item (reference-name resolution) absorbed into the ADR; the s5 bare-door oracle-FP shape CLOSED (witness gate → `knows_toplevel_class` only). [deliverables](notes/20260719-adr0042-gate-deliverables.md).
- **2026-07-19 #194 root-caused: stale-gem plugin hijack** — the 3 "upstream regressions" were artifacts of `rigortype 0.2.4`'s pre-gate plugin copy hijacking the auto-wire require; corrected wave delta 0/0; oracle invocations hardened (`harness/lib.rb`, `fp_audit.py`, `UPSTREAM.md`); upstream keeps #194 for the version-skew hazard. [note](notes/20260718-upstream-rbs-inline-autowire-regressions.md).
- **2026-07-18 compat arc (Phases 0/1/3) + three subsystems, folded** — Phase 0 characterisation ([findings](notes/20260718-phase0-m1-m2-findings.md)); Phase 1 fixture parity 100% (PR #24); Phase 3 unknown-config-key warning byte-exact ([note](notes/20260718-phase3-new-rule-surfaces.md)); the M2-GO receiver-typing batch, gitlab UM 179→148 ([note](notes/20260718-m2-receiver-typing-batch.md)); severity-resolution machinery, 8/8 config byte-diffs identical ([note](notes/20260718-severity-profile-machinery.md)); `--bleeding-edge` + `static.value-use.void` ([note](notes/20260718-bleeding-edge-void-rule.md)); and the rbs-inline auto-wire regression measurement that HELD the pin ([note](notes/20260718-upstream-rbs-inline-autowire-regressions.md)).
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
- **2026-07-11 sig-gen periphery (4 closed items)** — MCP `sig_gen` tool (`e7ae83e`, read-only, byte-identical to CLI `--print --format json`); `--params=observed` SUBSTRATE-BLOCKED, not built (needs the ScopeIndexer; a literal-only port is a net regression, [note](notes/20260711-siggen-params-observed-substrate-blocked.md)); conditional-assign nilability BUILT but NOT merged (branch `flow-cond-assign-nilability` `7b7fe3d` — correct + FP-safe, closes 0 survey gaps, [spec](notes/20260711-conditional-assign-nilability-spec.md)); coverage frontier re-measured — bounded wins exhausted ([note](notes/20260711-coverage-frontier-remeasured.md)).
- **2026-07-11 sig-gen arc CLOSED (13 slices)** — `erase_to_rbs` `ee60d41` → `--print` `7f01322` → return-union `929ff74` → singletons `8db1bed` → `--write` create `af4f42f` → initialize stub `25d82eb` → `--diff` `968c10c` → module_function `95f490d` → Writer merge+LayoutIndex `c02dcdc` → env classification `a268a6c` → `--overwrite` `9e85e07` → qualified naming `0f122b6` → Data/Struct shells `33f9436`. 0 shared-method mismatch on the full sweep; `--write` sound; remaining thin coverage-only (attr_*, merge-path shells, `--params=observed`).
- **2026-07-10 three closed items** — pure-RBS bundle track CLOSED (`activesupport-core-ext` is the only pure-RBS plugin and is byte-current, [note](notes/20260710-pure-rbs-bundle-track-closed.md)); MCP triage+annotate (`c6c1094`); ADR-72 Gemfile.lock auto-overlay (`96d7f47`) — Rails projects auto-get the AS overlay, FP-safe by construction.
- **2026-07-01…07-06 foundation era (9 arcs, all closed — details in the linked notes/ADRs)** — sidecar COMPLETE ([ADR-0036](adr/0036-ruby-sidecar-default-reversal.md)) + perf slices retired ([ADR-0037](adr/0037-sidecar-perf-slices-retired-by-measurement.md)); RBS ingestion legs ([ADR-0033](adr/0033-project-sig-ingestion.md)/[0034](adr/0034-rbs-collection-ingestion.md), inline deferred [0035](adr/0035-inline-rbs-deferred.md)); `fp_audit` + 4 real FP clusters → **0 FP across ~4000 files / 20+ libs**; the productization cluster (triage/annotate/diff/config-audit/baseline area/`check <dir>`); the flow-substrate arc ([ADR-0038](adr/0038-flow-substrate-incremental-narrowing.md)/[0039](adr/0039-shape-typing-tier.md), [0041](adr/0041-project-method-nilable-return.md) deferred by measurement) with the standing finding **no cheap FP-safe flow wins left** ([note](notes/20260706-flow-frontier-exhausted.md)); possible-nil/ivar expansion CONFIRMED zero-EV; rayon (~2.4×) + LSP v1/v2 + MCP + `flow.always-truthy-condition`/`call.unresolved-toplevel`.
- **2026-06-26…06-30 pre-foundation (3 arcs, all closed)** — rustfmt stance ([ADR-0032](adr/0032-source-formatting-policy.md): hand-formatted, clippy blocking); v0.0.1 release prep (version/CI/gem/Homebrew wired, tag-gated on maintainer infra, snapshot-mode CI parity job); leniency alignment + coverage passes (witnesses restricted to RBS-known core-surface receivers per the ref's tier-4 leniency, 2 FPs fixed; lowering-traversal +54, singleton witnessing + cross-file project index; audit R1–R5 addressed).
