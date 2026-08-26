# rigor-rs — Current Work

The session-to-session baton: **what is in flight, what to pull next, and a
one-line ledger of what landed**. The complete per-subsystem port map is
[PORT_BACKLOG.md](PORT_BACKLOG.md); measured outcomes and narratives live in
`docs/notes/` + `docs/adr/`; history is `git log`.

**Contract (gated by `harness/docs_check.py`):** a landed/closed arc gets ONE
ledger line here — verdict + numbers + link — and its detail goes to a dated
note or ADR *first*. No status essays; this file has a hard byte budget.

Last updated: 2026-08-26.

## Now / Next

▶ **NEXT (2026-08-26): fix the effects INSTRUMENT, then slice 5/6** — the slice-4 probe
measured that the 7-project corpus cannot distinguish a tuned arm from a NO-OP one, so a
real project + the `08_resolved` discriminators come first; the transitive LABEL lane is
DECLINED (see Standing conclusions).
Pin HOLDS at `v0.3.4` (master surveyed: 0 diagnostics on 9204).
The narrowing frontier stays OUT OF CARRIER
LEVERS ([verdicts](notes/20260809-deferred-slices-and-upstream-feedback.md),
[carrier](notes/20260809-unresolved-const-receiver-carrier.md)). **Before the
next bump, diff our OPEN upstream issues against the release notes** — batch 3
came due all at once as 50 FPs ([note](notes/20260823-repin-v034.md)).

- Measurement-tool lesson (binding): audit at NODE granularity — per-file
  histograms net over-claims out against under-claims.
- **CLOSED arcs** (in the ledger; do not re-open): ADR-0042 core migration
  (PRs #31/#32) and the compat next-stage plan (Phases 0–3 done, exhausted —
  [plan](notes/20260718-compat-next-stage-plan.md)).
- **CLI surface from the v0.3.0 RC** — `--bleeding-edge` + severity
  profile/overrides + `coverage` precision mode DONE; remaining: plugins
  inflection probe. `--protection`/`--mutation` (ADR-63/70) + `type-scan`
  deferred by [scoping call](notes/20260719-coverage-command-scoping.md).
- **Pin is `v0.3.4`** (+ vendored rbs 4.1.1; re-pinned 2026-08-23). Both standing
  exception tables are still EMPTY — `UNBUILDABLE_DEFINITIONS` and the divergence
  registry — so a new entry in either is a real finding, not maintenance
  (`UPSTREAM.md`, all THREE hazards + the overlay/`sig/shims` trap).
- Deferred RC deltas: interprocedural mutation floor (P6), plugin-only changes
  (no plugin engine); the RC inference deltas sit in the compat plan (M2).

State (verified 2026-08-23, post the `v0.3.4` re-pin): harness **97 fixtures / 0
unregistered extras / 0 registered divergences**, coverage 405/441; standing
sweep **0 FP / 9204 files / 820 gaps**, 8 corpora, baselines in
`harness/CORPUS.md`. Gap totals move mostly with upstream retractions, not
coverage. Neither sweep tool sees project-`sig/` behaviour. EVERY grading
tool prints its binary's path + build time and REFUSES one older than the
rigor-cli path-dep CLOSURE (PR #65; closure-scoped by PR #100) — corpus
tools on release, the fixture harness on debug. Clippy: workspace
`-D warnings`, verify in a FRESH `CARGO_TARGET_DIR`.

## Standing conclusions (do not re-litigate without new evidence)

- **Possible-nil / Tier B/C is CLOSED, not deferred** — 16/16 sampled coverage
  gaps are REFERENCE FPs; the only closing slice deletes rigor-rs's
  nameable-concrete-arm FP-safety mechanism, and `fp_audit` (which measures
  against the reference) would score that deletion 0 FP: the parity gate points
  the wrong way there. [tier-bc-track-closed](notes/20260717-tier-bc-track-closed.md).
- **141 more undefined-method gaps are REFERENCE FPs, adjudicated and CLOSED** —
  `pre_eval:` cross-file monkey-patch (49; the reference fires *having located*
  the project's definition, and closing would require INVERTING the ADR-0033
  provenance gate), receiver-typed-`nil` (63; 63/63 runtime-correct, the
  exactly-nil corner of Tier B/C), rdoc generated-parser `Hash` receivers (29;
  a wrong flow-insensitive ivar-arm collapse). 717 of 1168 gaps now sit behind
  decisions. [note](notes/20260807-gap-adjudication-141.md).
- **Five consecutive FP-safe flow slices closed 0 survey gaps** — never build a
  coverage slice without a valid-mode `fp_audit --gaps` prediction (AGENTS.md;
  [flow-frontier](notes/20260706-flow-frontier-exhausted.md)).
- **The receiver-typing lever is NOT exhausted — that 2026-07 conclusion is
  RETIRED.** The 2026-08-07 census re-opened it by asking which MECHANISM each
  gap is (not which rule), and the four slices that followed closed **47 rows**
  on shapes the port already had signatures for. Pick slices from the census's
  mechanism buckets, and re-run `gap_census.py --sweep` after each — the gap
  set's SHAPE moves even when its total barely does.
- **The effects TRANSITIVE LABEL lane is DECLINED — not portable at parity** (2026-08-26 slice-4 probe). Four progressively stricter typer-free rules were built and MEASURED; the best is still 5 OVER on gitlab-foss/lib. Upstream's edge set IS the set of call nodes its typer visited, and those visits are not characterisable (`return` values, interpolation, `next`/`break`, compound-write receivers — and, corrected by PR #112, `unless`/`elsif` arms go blind only when the condition FOLDS, sometimes THROUGH A CALL, which is less portable still). The inversion that settles it: more edges ⇒ more TAINT (sound) but more edges ⇒ more LABELS (unsound), and upstream's `absorb` moves both in ONE pass. Slice 3's over-taint stand-in stays; the labels stay UNDER, which is always safe. The ~2,000-method prize would need a registered-divergence device weakening the OVER gate — **REJECTED 2026-08-26: matching the reference outranks coverage**. Labels stay UNDER permanently; any future proposal must show it MATCHES the oracle, not merely scores better. [probe](notes/20260826-effects-s4-probe.md).
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

- **2026-08-26 the effects gate could not tell a correct implementation from a DELETED one** (PR #112) — an arm with the `unresolved-self-call` taint deleted scored **byte-identically** on the 7-project corpus. Now `mastodon/app` (6,948 methods) runs by DEFAULT and `gitlab-foss/lib` behind `--scale`, each COPIED into a temp project (residue structurally impossible, the ancestor-`.rigor.yml` ambush closed); `08_resolved` adds 44 discriminators. The wrong variant now fails by **76 OVER**, the shipped binary is **0 OVER on both**. Same family as the vacuous-0-FP find, found the same way — by trying to make the gate lie. [note](notes/20260826-s112-effects-instrument.md).

- **2026-08-26 LSP honours `rootUri` / `workspaceFolders`** (PR #110) — the server took the process CWD as the project root, so an editor spawning it elsewhere got the wrong config, `sig/` and discovery. It now **ENTERS** the client's root (`workspaceFolders` → `rootUri` → `rootPath` → cwd) rather than threading one: the root IS a cwd in all five consumers, so parity with `cd <root> && rigor check` holds by construction. **The probe is why**: threading would have missed `sig/` AND regressed `exclude:` (its spellings are deliberately RELATIVE), vacuously passing the existing matrix. Multi-root takes the first folder and discloses. [note](notes/20260826-s111-lsp-rooturi.md).

- **2026-08-25/26 the EFFECT-SYSTEM arc, slices 0–3** (PRs #91/#100/#105/#107/#108/#111) — a summary is graded per METHOD as a **sound subset** by `harness/effects_diff.py`, the first PROJECT-level instrument here. s1 vendored the 420-row catalogue crate (implied-ancestor `known?`, three drift layers); s2 added a **typer-free** Prism collector + `rigor effects --format=json` (**the subset argument failed a FOURTH time** — a ROW makes upstream NARROWER where the port loses the receiver type, so `[]=` is suppressed uncatalogued); s3 emits upstream's **TRANSITIVE** exhaustive bit (emitting the direct bit is 986 OVER on mastodon). **35 MATCH / 11 UNDER / 0 OVER** on the four pinned projects; mastodon extra-taint 945→242. Corpus now 7: `05_posture`/`07_mutators` are GENERATED from the vendored tables (#106's lesson, after a live OVER hand fixtures could not see), `06_edge` is mutation-proven to be the only gate against the direct bit. `declared:` is SOLVED (the CALLER's lane) for slice 6. [s2](notes/20260826-effects-s2-impl.md) / [s3](notes/20260826-effects-s3-impl.md) / [#106](notes/20260826-s106-posture-over-fix.md) / [s110](notes/20260826-s110-mutator-corpus.md).

- **2026-08-25/26 frozen-index ARC — COMPLETE** (PRs #95/#97-#99/#101/#103/#109/#113) — the per-file harvest/merge split: #92's keystone (stage 2 −29%, pre-#92 path kept as a `cfg(test)` oracle), #94's ancestor closure (4,675: 164.9→82.5ms), LSP held harvests (112.8→69.8ms — **OverlayGuard stays ON; cross-file diagnostics live at gitlab scale**), the per-URI cross-file cache, #102's `FileKey`, #96's fp_audit determinism, and **#113's Pass-4b fold capture**: an OWNED mini-tree per def ⇒ Pass 4b reads NO AST and `FoldSite::ast_idx` (the last slice-POSITION) is gone — exact by construction, every decline being SYNTACTIC, so **no subset argument**. `FOLD_DEPTH_CAP` finally HAS coverage (8/10 mutations killed); harvest +338 B/f. **Standing**: file order is NORMATIVE in `merge` (never sort — it reaches diagnostics); eviction stays BLOCKED — **Pass 3** + stage 3 need every tree, and 3's sub-arena is DEFERRED behind an ADR-0029 budget trigger; harvest cache **NO-GO** (#104: prize 1.2% of wall, reads 10×). [s92](notes/20260825-s92-harvest-merge-impl.md) / [s94](notes/20260825-s94-ancestor-closure-impl.md) / [lsp](notes/20260826-lsp-crossfile-cache-impl.md) / [spec](notes/20260826-fold-capture-mini-spec.md) / [s113](notes/20260826-s113-fold-capture-impl.md).
- **2026-08-25 upstream survey `v0.3.4` → master — HOLD the pin** — 64 commits + rbs 4.1.1→4.1.3 move **0 diagnostics on 9204 files** (both axes; the rbs trees are byte-identical). But the survey found two live rigor-rs defects in surfaces no standing gate reaches: the **vendored plugin RBS had drifted since 2026-06-26 = 10 FPs** (a THIRD pin-tracking surface, sourced from a local checkout — hazard 3 applied to a file; fixture 98 + ritual step 3 now cover it), and every `documentation_url` we emit **404s** (`blob/main`; upstream #438). [survey](notes/20260825-upstream-survey-v034-master.md).
- **2026-08-23 upstream re-pin `v0.3.2 → v0.3.4`** (151 commits) — **0 FP / 9204, gaps 841→820**, harness 97 fixtures / 0 extras; rbs AND `data/` overlay both UNCHANGED, both exception tables still empty. The raw bump opened **50 FPs** — all upstream RETRACTIONS (#319, #318) that the snapshot diff cannot show, and all from OUR batch-3 reports, now due. [note](notes/20260823-repin-v034.md).
- **2026-08-09 unresolved-const-receiver carrier — BUILT, REJECTED at 0 rows** (PR #89, closed) — sound but **841→841 on 9204**, and its first allow-list member needs both engines' INDEXES to agree — invisible to a core+stdlib sweep. [note](notes/20260809-unresolved-const-receiver-carrier.md).
- **2026-08-09 era (3 slices, folded)** — re-pin `v0.3.1 → v0.3.2` (+rbs 4.1.1): 0 FP / 9204, gaps 1125→841 (upstream retracting possible-nil FPs, #297); BOTH exception tables emptied; **trap: bundler/rubygems sigs depend on the rbs gem's `sig/shims/` — 2 FPs the sweep CANNOT SEE**, closed by `overlay/rbs_shims/` ([note](notes/20260809-repin-v032.md)); join-wipe retention (`retain_joined_facts` + the `else`-carrier unwrap; 1 FP closed, 15 probe shapes ref-matched — [note](notes/20260809-join-wipe-retention.md)); chain-guard meet (`chains` carries `ClassFact`, `narrow_nominal_to_class` shared by both arms; 2 FPs closed — [note](notes/20260809-chain-guard-meet.md)).
- **2026-08-08 the narrowing/shape trio, folded** — sequential-guard meet (PR #78: R3 → `narrow_nominal_to_class`, disjoint→`Bot`; 5 FPs closed — [note](notes/20260808-sequential-guard-meet.md)); qualified-name WITNESSING (PRs #80-#82: the witness fires for namespaced AND non-`CORE_CLASSES` guards, 1136→1127, unblocked by two probe-forced index fixes — [mini-spec](notes/20260808-qualified-witnessing-mini-spec.md) / [probes](notes/20260808-qualified-witnessing-probes.md)); the collection-shape ARC (PRs #70 / #75: literal-seeded locals keep their collection nominal through mutation; chain roots incl. `ENV` ingestion with nilable returns REFUSED; **26 rows closed** — [spec](notes/20260807-collection-shape-slice-spec.md)). 0 FP / 9204 throughout.
- **2026-08-08 `Object` bucket ADJUDICATED — 30 rows, all behind decisions, NO slice** (PR #85) — 18 REFERENCE FPs (`Class.new do…end` block bodies, `class << Const`), 3 = ADR-0035's deferred leg, 9 = one-file mocha rows. The 18 are FIXED upstream at `v0.3.4` (#319/#320) and ported. [adjudication](notes/20260808-object-bucket-adjudication.md).
- **2026-08-08 constant-value harvesting: per-file gate + partial containers** (PRs #83 / #84) — the reference never declines a partially-literal constant, and its constant-VALUE typing is per-FILE (source-confirmed): C5's project-wide consumption was a live over-emission class. A gates consumption per-file; B harvests partial containers as INERT bare nominals. **1127→1125, 0 FP / 9204.** C (chain constants) DECLINED — needs return resolution at index build. [mini-spec+log](notes/20260808-partial-constant-harvest-mini-spec.md).
- **2026-08-07/08 the class-narrowing ARC, CLOSED at a measured stop** (PRs #63, #68, #71-#74, #76, #77, #79) — ported `narrow_class_other` end-to-end (snapshot pass, statement-form descent, compound predicates, `next`/`break`, chain guards): **19 gap closures + eleven master FP shapes**, 0 FP / 9204 at every step. Three probe-forced lessons: the FP-safety argument was WRONG THREE TIMES (position AXIS; carrier ALLOW-list; disjoint→`Bot`); census windows measure PROXIMITY not mechanism; **verify the CONSUMPTION gate can witness the class before crediting rows**. [spec](notes/20260807-class-narrowing-slice-spec.md) / [stage3](notes/20260807-narrowing-stage3-spec.md).
- **2026-08-01/08 instruments + adjudication, folded** — the 0-FP gate could pass VACUOUSLY (PR #65: corpus tools measured `target/release` while `cargo build` writes debug, and `run_rs` swallowed failures into `[]` — [note](notes/20260807-fp-audit-port-side-blind-spots.md)); the coverage-gap CENSUS buckets gaps by MECHANISM, not rule, and half sit behind decisions already made ([note](notes/20260807-gap-census.md)); `arity_eligible?` was never ported = a `call.wrong-arity` FP (fixture 80); LSP config reload keeps LAST GOOD ([lsp](notes/20260801-lsp-config-reload.md)).
- **2026-08-07 ADR-0042 S5: qualified return-lookup routing** (PR #64, MERGED) — the 8-member return family routes namespaced receivers via the qualified registry (refs AS WRITTEN + lexical ctx; ambiguity DECLINES); **14 closures (→1179), 0 FP / 9204**; fixture 82 pins the Tier-3 instance boundary (gaps 3→4 on merge). [spec+outcome](notes/20260807-adr0042-s5-return-lookup-spec.md).
- **2026-08-07 upstream survey + feedback batch 2, folded** — the `v0.3.1`→`80aaf9bc` 2×2 self-diff moved 2 diagnostics on 9204 (superseded by the `v0.3.2` re-pin) and RETIRED GEM_HOME rbs selection, which had silently dropped 1650 files ([note](notes/20260807-upstream-survey-v031-to-master.md)); batch 2 filed 3 reference-side defects with paste-ready repros — the `c7f28da1` master FP, the `Dynamic|nil` possible-nil FP class, and the fail-soft definition build blinding 12 classes ([note](notes/20260807-upstream-feedback-batch2.md)).
- **2026-07-31 era (7 slices, folded — detail in the linked notes)** — sig-gen `Data.define`/`Struct.new` members ([note](notes/20260731-siggen-data-struct-members.md)); `BigMath` blinded-oracle asymmetry CLOSED, `UNBUILDABLE_DEFINITIONS` keyed pin × gems ([note](notes/20260731-bigmath-ingestion-asymmetry.md)); LSP v4 `Foo::` const completion + private-decl visibility ([note](notes/20260731-lsp-v4-const-completion-visibility.md)); survey FP triage 24→0, nine root causes, SIX unreachable by the fixture corpus ([note](notes/20260731-survey-fp-triage-24.md)); project-`sig/` blind-spot probe → first registered divergence + fixture 79 ([note](notes/20260731-project-sig-blind-spot-probe.md)); `-> self` on instance methods, 1 gap ([note](notes/20260731-self-return-instance-methods.md)); the standing sweep set CODIFIED in `harness/sweep-corpora.yml` + the `REFERENCE_RIGOR_DIR` hazard-3 fix ([CORPUS.md](../harness/CORPUS.md)).
- **2026-07-31 upstream pin `v0.3.0 → v0.3.1` + vendored rbs `4.0.3 → 4.1.0`** — **0 FP / 9153 files, gaps net −2**. 4.1.0's rewritten signatures broke two things, both FIXED rather than accepted: bounded method type params now resolve to their bound, and `-> instance` on an INSTANCE method resolves via the `SELF_RETURN` call-site sentinel. New `harness/vendor_rbs.py` makes the vendoring recipe executable (proven by reproducing the 4.0.3 tree byte-for-byte first). Upstream logic delta: ZERO. [note](notes/20260731-upstream-pin-v031-rbs41.md) / [survey](notes/20260731-v031-preflight-survey.md).
- **2026-07-25 era (3 slices, folded)** — MultiWrite substrate s1+s2 (PRs #46/#47: `a, b = rhs` arena lowering + rule-for-rule `MultiTargetBinder`, RBS tuple returns as `RbsReturnShape`; mid-build FP gated on `is_declaration_only_class`; 14 corpora / 35,706 files bit-identical — [spec](notes/20260725-multiwrite-substrate-spec.md) / [s1](notes/20260725-multiwrite-substrate-s1.md) / [s2](notes/20260725-multiwrite-substrate-s2.md)); LSP `exclude:` parity (PR #45: excluded iff EVERY discovery spelling is; matrix 24→144 — [note](notes/20260725-lsp-exclude-parity.md)); LSP stage-3 parity tail (PR #44: ADR-8 SeverityStamp rides `ProjectContext`; 4 E2E tests vs real `check`, each proven non-vacuous — [note](notes/20260725-lsp-stage3-parity.md)).

- **2026-07-19 era (4 arcs, folded)** — LSP §12 two-tier S1–S4b (PRs #35–#38/#42/#43 + `16bfb9e`; + baseline drift/prune positional-roots fix, PR #41 — [plan](notes/20260719-lsp-s12-two-tier-impl-plan.md)); upstream tracking `b70adcb5..ff6b6158` (29 commits, 0 added / 0 dropped self-diff every wave — nothing to port); `coverage` precision mode + MCP tool + node-granularity over-claim audit (PRs #33/#40); ADR-0042 qualified-key gate + Slices 1–4 ([ADR](adr/0042-qualified-key-index-registration.md) / [deliverables](notes/20260719-adr0042-gate-deliverables.md)).

- **2026-07-18 compat arc (Phases 0/1/3) + three subsystems, folded** — Phase 0 characterisation ([findings](notes/20260718-phase0-m1-m2-findings.md)); Phase 1 fixture parity 100% (PR #24); Phase 3 unknown-config-key byte-exact ([note](notes/20260718-phase3-new-rule-surfaces.md)); M2-GO receiver typing, gitlab UM 179→148 ([note](notes/20260718-m2-receiver-typing-batch.md)); severity machinery ([note](notes/20260718-severity-profile-machinery.md)); `--bleeding-edge` ([note](notes/20260718-bleeding-edge-void-rule.md)); the rbs-inline auto-wire measurement that HELD the pin ([note](notes/20260718-upstream-rbs-inline-autowire-regressions.md)).
- **2026-07-18 upstream RC bump `47ec8625→7a69f142`** (80 commits) — two parity divergences closed 0 FP: `suppression.unknown-marker` (new rule, upstream `4e0ca475`) + Kernel intrinsic explicit-`Kernel.`-receiver fold (`c9d2e473`); live 188 matched / 193 ref, snapshots re-baselined, core corpora + survey FP-clean. Rest of the RC's inference precision deferred as coverage-only. [note](notes/20260718-upstream-rc-bump-47ec8625-7a69f142.md).
- **2026-07-17 docs economy + Tier B/C track CLOSED** — CURRENT_WORK.md 184KB→baton + [PORT_BACKLOG.md](PORT_BACKLOG.md) split, byte-budget gate `harness/docs_check.py` + docs CI (port of upstream rigor#119, issue #21); and the Tier B/C / ScopeIndexer no-go, evidence-backed — see Standing conclusions, [note](notes/20260717-tier-bc-track-closed.md).
- **2026-07-17 ATM arc + two undefined-method receiver-typing wins, MERGED** — `call.argument-type-mismatch` on both channels (per-overload RBS retention + acceptance walk, byte-exact, 0 FP); plus the constant-shadow gate and the C3a String-tail, together taking gitlab UM 356→179. [spec](notes/20260717-atm-substrate-arc-plan.md).
- **2026-07-16 two MERGED inference slices** — `def.ivar-write-mismatch` (`a2098d7`: ivar-write lowering + rescue binding + Kernel cast fallback + collector; gitlab ivar gaps 2→0) and literal-tail return folding (`0721943`: interprocedural singleton-method literal fold, depth-16, ancestry-scoped; gitlab always-truthy 28→16). 0 FP both. [ivar](notes/20260716-ivar-write-mismatch-spec.md) / [fold](notes/20260716-literal-tail-fold-spec.md).
- **2026-07-16 v0.3.0-RC arc: pin `47ec8625` + 7 slices, ALL MERGED** — syntactic rules (dup-hash-key, return-in-ensure, suppression.*, `Node::Lambda`), MutationWidening (killed 2 measured FPs), implicit-self dispatch + `p`/`pp`, scalar HashShape keys + projection folds, Kernel `format`/casts folding, `raise-non-exception` + `class_ordering`, `shadowed-rescue-clause` + rbs.rs nesting root-fix. **v0.3.0 rule surface fully ported.** [specs](notes/20260716-v030-upstream-gap-survey.md).
- **2026-07-11 sig-gen arc CLOSED (13 slices) + periphery (4 items)** — `erase_to_rbs` → `--print` → return-union → singletons → `--write` → initialize stub → `--diff` → module_function → Writer merge+LayoutIndex → env classification → `--overwrite` → qualified naming → Data/Struct shells (`ee60d41`…`33f9436`); 0 shared-method mismatch on the full sweep, `--write` sound. Periphery: MCP `sig_gen` tool (`e7ae83e`); `--params=observed` SUBSTRATE-BLOCKED, not built ([note](notes/20260711-siggen-params-observed-substrate-blocked.md)); conditional-assign nilability BUILT not merged (`7b7fe3d`, 0 survey gaps); coverage frontier re-measured — bounded wins exhausted ([note](notes/20260711-coverage-frontier-remeasured.md)).
- **2026-06-26…07-10 pre-/foundation era (15 arcs, all closed)** — sidecar COMPLETE + perf slices retired by measurement ([ADR-0036](adr/0036-ruby-sidecar-default-reversal.md)/[0037](adr/0037-sidecar-perf-slices-retired-by-measurement.md)); the RBS ingestion legs ([ADR-0033](adr/0033-project-sig-ingestion.md)/[0034](adr/0034-rbs-collection-ingestion.md), inline deferred [0035](adr/0035-inline-rbs-deferred.md)); `fp_audit` + 4 real FP clusters → 0 FP across ~4000 files; the productization cluster (triage/annotate/diff/config-audit/baseline, `check <dir>`); the flow substrate ([ADR-0038](adr/0038-flow-substrate-incremental-narrowing.md)/[0039](adr/0039-shape-typing-tier.md)) with the standing finding **no cheap FP-safe flow wins left**; rayon (~2.4×), LSP v1/v2, MCP; leniency alignment; rustfmt stance ([ADR-0032](adr/0032-source-formatting-policy.md)); v0.0.1 release prep; pure-RBS bundle track CLOSED; ADR-72 Gemfile.lock auto-overlay (`96d7f47`).
