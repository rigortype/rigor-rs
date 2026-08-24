# rigor-rs — Current Work

The session-to-session baton: **what is in flight, what to pull next, and a
one-line ledger of what landed**. The complete per-subsystem port map is
[PORT_BACKLOG.md](PORT_BACKLOG.md); measured outcomes and narratives live in
`docs/notes/` + `docs/adr/`; history is `git log`.

**Contract (gated by `harness/docs_check.py`):** a landed/closed arc gets ONE
ledger line here — verdict + numbers + link — and its detail goes to a dated
note or ADR *first*. No status essays; this file has a hard byte budget.

Last updated: 2026-08-25.

## Now / Next

▶ **NEXT (2026-08-25): the EFFECT-SYSTEM arc is open at slice 0** — ADR + the
project-level differential are in; slice 1 is the vendored effect catalogue
([ADR-0043](adr/0043-effect-system-port-parity-model.md) names the order).
Pin HOLDS at `v0.3.4` (master surveyed: 0 diagnostics on 9204). Also open:
LSP v4+ (`rootUri`, UTF-16 sync). The narrowing frontier stays OUT OF CARRIER
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
coverage. Neither sweep tool sees project-`sig/` behaviour. EVERY
tool that grades the port now prints its binary's path + build time and
REFUSES one older than `crates/` — corpus tools on release, the fixture
harness on debug (PR #65 + the 08-08 follow-up; a stale debug binary reported
18 phantom FPs before the guard). Clippy: workspace `-D warnings`, verify in a
FRESH `CARGO_TARGET_DIR`.

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

- **2026-08-25 effect-system port OPENED — ADR + differential, slice 0** ([ADR-0043](adr/0043-effect-system-port-parity-model.md)) — a summary is not a diagnostic set: graded per METHOD as a **sound subset** (proven ⊆ oracle, taint ≥ oracle, declared exact) by `harness/effects_diff.py`, the first PROJECT-level instrument here — it runs IN the project, so it sees `.rigor.yml` / `sig/` / plugins. Self-test all-MATCH; baseline debt **46 methods / 28 labels**, 0 OVER. Slice order follows upstream's (snapshot first). OPEN: when the oracle populates `declared:` — 4 hypotheses refuted.
- **2026-08-25 upstream survey `v0.3.4` → master — HOLD the pin** — 64 commits + rbs 4.1.1→4.1.3 move **0 diagnostics on 9204 files** (both axes; the rbs trees are byte-identical). But the survey found two live rigor-rs defects in surfaces no standing gate reaches: the **vendored plugin RBS had drifted since 2026-06-26 = 10 FPs** (a THIRD pin-tracking surface, sourced from a local checkout — hazard 3 applied to a file; fixture 98 + ritual step 3 now cover it), and every `documentation_url` we emit **404s** (`blob/main`; upstream #438). [survey](notes/20260825-upstream-survey-v034-master.md).
- **2026-08-23 upstream re-pin `v0.3.2 → v0.3.4`** (151 commits) — **0 FP / 9204, gaps 841→820**, harness 97 fixtures / 0 extras; rbs AND `data/` overlay both UNCHANGED, both exception tables still empty. The raw bump opened **50 FPs** — all upstream RETRACTIONS (#319, #318) that the snapshot diff cannot show, and all from OUR batch-3 reports, now due. [note](notes/20260823-repin-v034.md).
- **2026-08-09 unresolved-const-receiver carrier — BUILT, REJECTED at 0 rows** (PR #89, closed) — sound but **841→841 on 9204**, and its first allow-list member needs both engines' INDEXES to agree — invisible to a core+stdlib sweep. [note](notes/20260809-unresolved-const-receiver-carrier.md).
- **2026-08-09 join-wipe retention (`Scope#join` fidelity)** — `join_cenv` wiped EVERY `Narrowed`/chain fact at each conditional merge, so a fact died at any later intervening `if`/`case`. `retain_joined_facts` puts back what ALL edges still carry identically, plus the `else`-carrier unwrap (an `ElseNode` lowers to a clause-less `BeginRescue` whose own wipe cost every `if`-with-`else` its falsey-edge facts). **1 FP closed, 15 probe shapes ref-matched, 0 FP / 9204.** [note](notes/20260809-join-wipe-retention.md).
- **2026-08-09 upstream re-pin `v0.3.1 → v0.3.2` (+ rbs 4.1.1)** — **0 FP / 9204, gaps 1125→841** (−284 = upstream retracting possible-nil FPs, #297 — NOT coverage); BOTH exception tables emptied. rbs byte-identical; the `data/` OVERLAY is what moved. **Trap: bundler/rubygems sigs now DEPEND on the rbs gem's `sig/shims/` — 2 live FPs the sweep CANNOT SEE**, hand-probed, closed by `overlay/rbs_shims/`. [note](notes/20260809-repin-v032.md).
- **2026-08-09 chain-guard meet — `Bot` sentinel + refinement** — 3a-3 CHAIN facts were on the superseded blind compare and `chains: …String` could not hold a collapse (2 live FPs). `chains` now carries `ClassFact`; `narrow_nominal_to_class` extracted and SHARED by both arms. **2 FPs closed, +4 matched, 23/26 probe rows ref-matched**; census flat, 0 FP / 9204. [note](notes/20260809-chain-guard-meet.md).
- **2026-08-08 sequential-guard meet** (PR #78) — S2's re-seed + R3 drop was the WRONG meet: R3 → `narrow_nominal_to_class` (disjoint→`Bot`, subclass refines, `Unknown` splits project-vs-RBS space). **5 FPs closed, +6 matched; census flat, 0 FP / 9204.** [note](notes/20260808-sequential-guard-meet.md).
- **2026-08-08 `Object` bucket ADJUDICATED — 30 rows, all behind decisions, NO slice** (PR #85) — 18 REFERENCE FPs (`Class.new do…end` block bodies, `class << Const`), 3 = ADR-0035's deferred leg, 9 = one-file mocha rows. The 18 are FIXED upstream at `v0.3.4` (#319/#320) and ported. [adjudication](notes/20260808-object-bucket-adjudication.md).
- **2026-08-08 constant-value harvesting: per-file gate + partial containers** (PRs #83 / #84) — the reference never declines a partially-literal constant, and its constant-VALUE typing is per-FILE (source-confirmed): C5's project-wide consumption was a live over-emission class. A gates consumption per-file; B harvests partial containers as INERT bare nominals. **1127→1125, 0 FP / 9204.** C (chain constants) DECLINED — needs return resolution at index build. [mini-spec+log](notes/20260808-partial-constant-harvest-mini-spec.md).
- **2026-08-08 qualified-name WITNESSING** (PRs #80-#82) — the narrowing witness now fires for namespaced AND non-`CORE_CLASSES` guard classes. **1136→1127: 9 closed, 0 opened, 0 FP / 9204.** Unblocked by TWO probe-forced fixes (ADR-0042 registry double-prefixed depth-≥3 decls; the qualified absence-check under-reported inherited methods). Also root-fixed: the shaped-carrier collapse FP family and the sequential-disjoint LOCAL re-guard FP. [mini-spec+log](notes/20260808-qualified-witnessing-mini-spec.md) / [probes](notes/20260808-qualified-witnessing-probes.md).
- **2026-08-08 the collection-shape ARC** (PRs #70 / #75) — a literal-seeded local keeps its collection NOMINAL through `<<`/`[]=`/block mutation (`widen_after_block` is a SYNTACTIC walk, not a scope join — oracle-re-probed mid-build); then the chain ROOTS (`Dir.glob`/`String#split` block-overload split, `ENV` object-constant ingestion — nilable returns REFUSED, 13/15 first-cut FPs — qualified C5 paths). **26 rows closed, 0 opened, 0 FP / 9204 throughout.** [spec+outcomes](notes/20260807-collection-shape-slice-spec.md).
- **2026-08-07/08 the class-narrowing ARC, CLOSED at a measured stop** (PRs #63, #68, #71-#74, #76, #77, #79) — ported `narrow_class_other` end-to-end: snapshot pass + `Node::When` split, 3b-1 statement-form descent, 3a-1 compound predicates, `next`/`break` termination, 3a-3 chain guards. **19 gap closures + eleven master FP shapes closed**; 0 FP / 9204 at every step. Three probe-forced lessons, each load-bearing: the FP-safety argument was WRONG THREE TIMES (position AXIS; carrier ALLOW-list; disjoint→`Bot`); census windows measure PROXIMITY not mechanism; and **verify the CONSUMPTION gate can witness the class before crediting rows**. [position](notes/20260807-block-narrowing-position-rule.md) / [carriers](notes/20260808-narrowing-carrier-fidelity-fp.md) / [stage3](notes/20260807-narrowing-stage3-spec.md) / [remeasure](notes/20260808-narrowing-3a23-window-remeasure.md) / [spec](notes/20260807-class-narrowing-slice-spec.md).
- **2026-08-08 harness integrity: the 0-FP gate could pass VACUOUSLY** (PR #65) — `fp_audit`/`gap_census` measured `target/release/rigor` while `cargo build` writes debug (a 6-day-old binary made PRs #63/#64 read as "closed nothing"), and `run_rs` swallowed every failure into `[]`, so a panicking port binary scored 0 FP. Now: auto-build, STALE-binary refusal, path+mtime in the header, `None`-not-`[]` on failure, INVALID corpora fail the run. [note](notes/20260807-fp-audit-port-side-blind-spots.md).
- **2026-08-07 ADR-0042 S5: qualified return-lookup routing** (PR #64, MERGED) — the 8-member return family routes namespaced receivers via the qualified registry (refs AS WRITTEN + lexical ctx; ambiguity DECLINES); **14 closures (→1179), 0 FP / 9204**; fixture 82 pins the Tier-3 instance boundary (gaps 3→4 on merge). [spec+outcome](notes/20260807-adr0042-s5-return-lookup-spec.md).
- **2026-08-07 upstream feedback batch 2** — 3 reference-side defects with paste-ready repros: the `c7f28da1` (#271) master FP, the `Dynamic|nil` possible-nil FP class, the fail-soft definition build blinding 12 classes. [note](notes/20260807-upstream-feedback-batch2.md).
- **2026-08-07 coverage-gap CENSUS — gaps are six MECHANISMS, not one number** — `harness/gap_census.py` buckets every gap by the mechanism in the reference's own message; the per-rule histogram hides which. Half the set sits behind decisions already made (possible-nil Tier B/C, always-truthy flow frontier), so quoting the total invites re-litigating them. The actionable pool is `undefined-method`, dominated by receiver typing on CORE classes where the port HAS the signature. Two of its three named slices survived probing; `X.to_s` → String was REFUTED. [note](notes/20260807-gap-census.md).
- **2026-08-01 two 08-01 slices** — `Object#Nokogiri` was already CLOSED, but the sibling sweep exposed a `call.wrong-arity` FP: **`arity_eligible?` was never ported** (507/11,115 methods); fixture 80 pins it. LSP config reload: `.rigor.yml` re-parsed on every structural `invalidate` (deliberately BEATS the reference); broken file keeps LAST GOOD, deleted reloads defaults. [nokogiri](notes/20260801-nokogiri-ingestion-asymmetry-closed.md) / [lsp](notes/20260801-lsp-config-reload.md).
- **2026-07-31 era (7 slices, folded — detail in the linked notes)** — sig-gen `Data.define`/`Struct.new` members ([note](notes/20260731-siggen-data-struct-members.md)); `BigMath` blinded-oracle asymmetry CLOSED, `UNBUILDABLE_DEFINITIONS` keyed pin × gems ([note](notes/20260731-bigmath-ingestion-asymmetry.md)); LSP v4 `Foo::` const completion + private-decl visibility ([note](notes/20260731-lsp-v4-const-completion-visibility.md)); survey FP triage 24→0, nine root causes, SIX unreachable by the fixture corpus ([note](notes/20260731-survey-fp-triage-24.md)); project-`sig/` blind-spot probe → first registered divergence + fixture 79 ([note](notes/20260731-project-sig-blind-spot-probe.md)); `-> self` on instance methods, 1 gap ([note](notes/20260731-self-return-instance-methods.md)); the standing sweep set CODIFIED in `harness/sweep-corpora.yml` + the `REFERENCE_RIGOR_DIR` hazard-3 fix ([CORPUS.md](../harness/CORPUS.md)).
- **2026-08-07 upstream survey `v0.3.1`→`80aaf9bc` (150 commits + rbs 4.1.1) — pin HOLDS** (superseded by the `v0.3.2` re-pin) — 2×2 self-diff (checkout × rbs) over the sweep set: upstream logic moves **2 diagnostics on 9204 files**, rigor-rs silent at both; rbs 4.1.0→4.1.1 is **0/0 and structurally so** (`core/`+`stdlib/` BYTE-IDENTICAL). GEM_HOME rbs selection is RETIRED (it silently dropped 1650 files); use `ruby -I <rbs>/lib -I <ext>`. [note](notes/20260807-upstream-survey-v031-to-master.md).
- **2026-07-31 upstream pin `v0.3.0 → v0.3.1` + vendored rbs `4.0.3 → 4.1.0`** — **0 FP / 9153 files, gaps net −2**. 4.1.0's rewritten signatures broke two things, both FIXED rather than accepted: bounded method type params now resolve to their bound, and `-> instance` on an INSTANCE method resolves via the `SELF_RETURN` call-site sentinel. New `harness/vendor_rbs.py` makes the vendoring recipe executable (proven by reproducing the 4.0.3 tree byte-for-byte first). Upstream logic delta: ZERO. [note](notes/20260731-upstream-pin-v031-rbs41.md) / [survey](notes/20260731-v031-preflight-survey.md).
- **2026-07-25 MultiWrite substrate, slices 1+2** (PRs #46/#47) — `a, b = rhs` had NO arena lowering, so `collect_flow_writes` never saw the rebind: a MEASURED live `check` FP. Added `Node::MultiWrite` + a rule-for-rule `MultiTargetBinder` port; s2 survived RBS tuple returns as `RbsReturnShape`. **An FP found mid-build**: naive witness widening fired where the oracle is silent — gated on `is_declaration_only_class`. 14 corpora / 35,706 files bit-identical. [spec](notes/20260725-multiwrite-substrate-spec.md) / [s1](notes/20260725-multiwrite-substrate-s1.md) / [s2](notes/20260725-multiwrite-substrate-s2.md).
- **2026-07-25 LSP `exclude:` parity** (PR #45) — a buffer is excluded iff EVERY discovery spelling of it is excluded: overlay membership, all-roots × 3 name forms, then a confirm pass over the SHARED `discovery_spellings` walk so it cannot drift from `project_files`. Review found 3 symlink/multi-root regressions; matrix 24→144 runs. `check` byte-identical under 3 configs. [note](notes/20260725-lsp-exclude-parity.md).

- **2026-07-25 LSP stage-3 parity tail** (PR #44) — the LSP never applied the ADR-8 SeverityStamp: a rule resolved `off` still published markers `check` DROPS, severities were authored not profile-resolved, bleeding-edge unplumbed. The stamp now rides `ProjectContext` beside `disable:` (rebuilt by `invalidate` under the S4 generation guard); composition order verified against `main.rs`. 4 E2E tests vs real `check` output, each proven non-vacuous by re-breaking. [note](notes/20260725-lsp-stage3-parity.md).

- **2026-07-19 LSP §12 two-tier COMPLETE (S1–S4b)** (PRs #35–#38/#42/#43 + `16bfb9e`) — `select!` loop + BufferTable, 200 ms debounce, rayon workers, 3-axis stale-drop, generation guard. Alongside it, **baseline drift/prune positional roots + scope guard** (PR #41), found by a real-product baseline check (conference-app, no `.rigor.yml`): drift/prune ignored positional roots. [plan](notes/20260719-lsp-s12-two-tier-impl-plan.md).

- **2026-07-19 upstream tracking `b70adcb5..ff6b6158`** (29 commits, three waves: transitive-void ADR-100 WD4, type-of plugin-env parity, the #194 loader stack landed+closed upstream, ADR-58 massign-ivar seeding, ADR-67 WD6 call-site param inference) — hardened self-diff **0 added / 0 dropped** on all four battery corpora every wave; nothing to port.
- **2026-07-19 `coverage` precision mode + MCP tool + over-claim audit** (PRs #33/#40) — the reference precision-tier scan on rayon (`--workers`) + the MCP `rigor_coverage` tool; the node-granularity audit found 0 wrong claims (its lesson is in Now/Next).
- **2026-07-19 ADR-0042 qualified-key migration, gate + Slices 1–4** (subagent-parallel) — the gate first (12-scenario oracle matrix → fixtures 68–70; a consumer inventory found no unsound consumer under alias-collapse, +2 latent-FP sites fixed free), then the substrate (additive, gates byte-unchanged) + qualified singleton and INSTANCE witnessing. [ADR](adr/0042-qualified-key-index-registration.md) / [deliverables](notes/20260719-adr0042-gate-deliverables.md).
- **2026-07-18 compat arc (Phases 0/1/3) + three subsystems, folded** — Phase 0 characterisation ([findings](notes/20260718-phase0-m1-m2-findings.md)); Phase 1 fixture parity 100% (PR #24); Phase 3 unknown-config-key byte-exact ([note](notes/20260718-phase3-new-rule-surfaces.md)); M2-GO receiver typing, gitlab UM 179→148 ([note](notes/20260718-m2-receiver-typing-batch.md)); severity machinery ([note](notes/20260718-severity-profile-machinery.md)); `--bleeding-edge` ([note](notes/20260718-bleeding-edge-void-rule.md)); the rbs-inline auto-wire measurement that HELD the pin ([note](notes/20260718-upstream-rbs-inline-autowire-regressions.md)).
- **2026-07-18 upstream RC bump `47ec8625→7a69f142`** (80 commits) — two parity divergences closed 0 FP: `suppression.unknown-marker` (new rule, upstream `4e0ca475`) + Kernel intrinsic explicit-`Kernel.`-receiver fold (`c9d2e473`); live 188 matched / 193 ref, snapshots re-baselined, core corpora + survey FP-clean. Rest of the RC's inference precision deferred as coverage-only. [note](notes/20260718-upstream-rc-bump-47ec8625-7a69f142.md).
- **2026-07-17 docs economy + Tier B/C track CLOSED** — CURRENT_WORK.md 184KB→baton + [PORT_BACKLOG.md](PORT_BACKLOG.md) split, byte-budget gate `harness/docs_check.py` + docs CI (port of upstream rigor#119, issue #21); and the Tier B/C / ScopeIndexer no-go, evidence-backed — see Standing conclusions, [note](notes/20260717-tier-bc-track-closed.md).
- **2026-07-17 ATM arc + two undefined-method receiver-typing wins, MERGED** — `call.argument-type-mismatch` on both channels (per-overload RBS retention + acceptance walk, byte-exact, 0 FP); plus the constant-shadow gate and the C3a String-tail, together taking gitlab UM 356→179. [spec](notes/20260717-atm-substrate-arc-plan.md).
- **2026-07-16 two MERGED inference slices** — `def.ivar-write-mismatch` (`a2098d7`: ivar-write lowering + rescue binding + Kernel cast fallback + collector; gitlab ivar gaps 2→0) and literal-tail return folding (`0721943`: interprocedural singleton-method literal fold, depth-16, ancestry-scoped; gitlab always-truthy 28→16). 0 FP both. [ivar](notes/20260716-ivar-write-mismatch-spec.md) / [fold](notes/20260716-literal-tail-fold-spec.md).
- **2026-07-16 v0.3.0-RC arc: pin `47ec8625` + 7 slices, ALL MERGED** — syntactic rules (dup-hash-key, return-in-ensure, suppression.*, `Node::Lambda`), MutationWidening (killed 2 measured FPs), implicit-self dispatch + `p`/`pp`, scalar HashShape keys + projection folds, Kernel `format`/casts folding, `raise-non-exception` + `class_ordering`, `shadowed-rescue-clause` + rbs.rs nesting root-fix. **v0.3.0 rule surface fully ported.** [specs](notes/20260716-v030-upstream-gap-survey.md).
- **2026-07-11 sig-gen arc CLOSED (13 slices) + periphery (4 items)** — `erase_to_rbs` → `--print` → return-union → singletons → `--write` → initialize stub → `--diff` → module_function → Writer merge+LayoutIndex → env classification → `--overwrite` → qualified naming → Data/Struct shells (`ee60d41`…`33f9436`); 0 shared-method mismatch on the full sweep, `--write` sound. Periphery: MCP `sig_gen` tool (`e7ae83e`); `--params=observed` SUBSTRATE-BLOCKED, not built ([note](notes/20260711-siggen-params-observed-substrate-blocked.md)); conditional-assign nilability BUILT not merged (`7b7fe3d`, 0 survey gaps); coverage frontier re-measured — bounded wins exhausted ([note](notes/20260711-coverage-frontier-remeasured.md)).
- **2026-06-26…07-10 pre-/foundation era (15 arcs, all closed)** — sidecar COMPLETE + perf slices retired by measurement ([ADR-0036](adr/0036-ruby-sidecar-default-reversal.md)/[0037](adr/0037-sidecar-perf-slices-retired-by-measurement.md)); the RBS ingestion legs ([ADR-0033](adr/0033-project-sig-ingestion.md)/[0034](adr/0034-rbs-collection-ingestion.md), inline deferred [0035](adr/0035-inline-rbs-deferred.md)); `fp_audit` + 4 real FP clusters → 0 FP across ~4000 files; the productization cluster (triage/annotate/diff/config-audit/baseline, `check <dir>`); the flow substrate ([ADR-0038](adr/0038-flow-substrate-incremental-narrowing.md)/[0039](adr/0039-shape-typing-tier.md)) with the standing finding **no cheap FP-safe flow wins left**; rayon (~2.4×), LSP v1/v2, MCP; leniency alignment; rustfmt stance ([ADR-0032](adr/0032-source-formatting-policy.md)); v0.0.1 release prep; pure-RBS bundle track CLOSED; ADR-72 Gemfile.lock auto-overlay (`96d7f47`).
