# rigor-rs — Current Work

The session-to-session baton: **what is in flight, what to pull next, and a
one-line ledger of what landed**. The complete per-subsystem port map is
[PORT_BACKLOG.md](PORT_BACKLOG.md); measured outcomes and narratives live in
`docs/notes/` + `docs/adr/`; history is `git log`.

**Contract (gated by `harness/docs_check.py`):** a landed/closed arc gets ONE
ledger line here — verdict + numbers + link — and its detail goes to a dated
note or ADR *first*. No status essays; this file has a hard byte budget.


## Now / Next

▶ **NEXT (2026-09-09): the `v0.3.8` re-pin is MERGED** (PR #119, `0cd4749`). **#118 is CLOSED** (PR #120: tier 3's flat slot drops the nil bit and measures
agreement after ERASURE — 13 FPs closed, no matched row lost, 9204 corpus files
byte-identical); its residue is **#121** (a class-guarded argument, which needs the
port's folder to learn `String#[]` first). Upstream took all five reports and
fixed them the same day (#870/#872, #871, #877, #878, #879). **Master surveyed at
`80b086a5`, 219 commits: HOLD the pin** — 10 diagnostics on 9204 files, port silent on
all ten, rbs / `data/` / plugin sig all unchanged. One obligation comes due at the
bump: **#122** (#877's rooted version guard — the port would be louder than the new
oracle, and no corpus file exercises it). The rufo perf regression is fixed upstream,
so `--sweep`'s 80-minute budget lapses then
([survey](notes/20260909-survey-v038-master.md)). Upstream master is 112 commits past `v0.3.8`;
survey it before the next tag, bisect-first. The effect-system arc stays at its clean
stop (slices 4/6 CLOSED by measurement; 20 resolvable-`super` rows are slice-4 debt);
the narrowing frontier stays OUT OF CARRIER LEVERS.

- Measurement-tool lesson (binding): audit at NODE granularity — per-file
  histograms net over-claims against under-claims.
- **CLOSED arcs** (in the ledger; do not re-open): ADR-0042 core migration
  (PRs #31/#32) and the compat next-stage plan (Phases 0–3 done, exhausted —
  [plan](notes/20260718-compat-next-stage-plan.md)).
- **CLI surface from the v0.3.0 RC** — `--bleeding-edge` + severity
  profile/overrides + `coverage` precision mode DONE; remaining: plugins
  inflection probe. `--protection`/`--mutation` (ADR-63/70) + `type-scan`
  deferred by [scoping call](notes/20260719-coverage-command-scoping.md).
- **Pin is `v0.3.8`** (`ffb456b0`; + vendored rbs 4.2.0; re-pinned 2026-09-09). Both
  standing exception tables are EMPTY — `UNBUILDABLE_DEFINITIONS` and the divergence
  registry (#437 retired) — so a new entry in either is a real finding, not maintenance
  (`UPSTREAM.md`: three hazards + the overlay/`sig/shims` trap). The version-guard
  port folds against `HOST_RUBY_VERSION` 4.0.5 / `ruby` (`RIGOR_RUBY_VERSION` /
  `RIGOR_RUBY_ENGINE` override) — the oracle's own host dependence, mirrored.
- Deferred RC deltas: interprocedural mutation floor (P6), plugin-only changes
  (no plugin engine); the RC inference deltas sit in the compat plan (M2).

State (verified 2026-09-09, post-#125): harness **107 fixtures / 0
unregistered extras / 0 registered divergences**, coverage 515/563; standing
sweep **0 FP / 9204 files / 799 gaps**, 8 corpora, baselines in
`harness/CORPUS.md`; effects gate 0 OVER (report and snapshot). Gap totals move mostly with upstream retractions, not
coverage. Neither sweep tool sees project-`sig/` behaviour. EVERY grading
tool prints its binary's path + build time and REFUSES one older than the
rigor-cli path-dep CLOSURE (PR #65; closure-scoped by PR #100) — corpus
tools on release, the fixture harness on debug. Clippy: workspace
`-D warnings`, verify in a FRESH `CARGO_TARGET_DIR`.

## Standing conclusions (do not re-litigate without new evidence)

- **Possible-nil / Tier B/C is CLOSED, not deferred** — the closing slice deletes
  rigor-rs's nameable-concrete-arm FP-safety mechanism and `fp_audit` would score it
  0 FP: the parity gate points the wrong way
  ([tier-bc](notes/20260717-tier-bc-track-closed.md)). **Its SCOPE, measured per row
  at `v0.3.8` with the oracle's `type-of`: the 85 `Dynamic`-arm rows, NOT the 93
  concrete-arm ones** (gated on nested-scope typing, then a 36-source tail).
- **Reference-FP undefined-method clusters, CLOSED** — `pre_eval:` cross-file
  monkey-patch (closing INVERTS the ADR-0033 provenance gate), receiver-typed-`nil`,
  rdoc generated-parser `Hash` ([141](notes/20260807-gap-adjudication-141.md)).
  Re-adjudicated whole at `v0.3.8` ([799](notes/20260909-gap-adjudication-799.md)):
  rdoc is **93 rows**, all naming a class the port's RBS does not declare (vendoring
  it is anti-parity); plus `Class.new(X) do…end` bodies read at TOP-LEVEL scope,
  nested `def` never indexed, gvar/OpenStruct. **395 of 799 sit behind decisions;
  the only mechanism above 13 rows is nested-scope receiver typing, 71 rows sized
  with no build by LIFTING the block body to top level.**
- **Five consecutive FP-safe flow slices closed 0 survey gaps** — never build a
  coverage slice without a valid-mode `fp_audit --gaps` prediction (AGENTS.md;
  [flow-frontier](notes/20260706-flow-frontier-exhausted.md)).
- **The receiver-typing lever is NOT exhausted** (the 2026-07 conclusion is RETIRED):
  the 2026-08-07 census re-opened it by asking which MECHANISM each gap is, not which
  rule, and four slices closed **47 rows** on shapes the port already had signatures
  for. Pick from mechanism buckets; re-run `gap_census.py --sweep` after each — the
  gap set's SHAPE moves even when its total barely does.
- **The effects TRANSITIVE LABEL lane is DECLINED — not portable at parity** (2026-08-26 slice-4 probe). Four progressively stricter typer-free rules were MEASURED; the best is still 5 OVER on gitlab-foss/lib. Upstream's edge set IS the set of call nodes its typer visited, and that set is not characterisable — the blind positions depend on the condition FOLDING, sometimes THROUGH A CALL. The inversion that settles it: more edges ⇒ more TAINT (sound) but more edges ⇒ more LABELS (unsound), and `absorb` moves both in ONE pass. The ~2,000-method prize would need a registered-divergence device weakening the OVER gate — **REJECTED 2026-08-26: matching the reference outranks coverage**. Labels stay UNDER permanently; any future proposal must show it MATCHES the oracle, not merely scores better. [probe](notes/20260826-effects-s4-probe.md).
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

- **2026-09-09 the 799 gaps ADJUDICATED, and the two findings that beat the coverage slice** — all 799 partitioned by MECHANISM: **395 sit behind decisions**, and the only bucket above 13 rows (nested-scope receiver typing, 71) rests on ONE over-narrow rbs signature an upstream fix would retract — not recommended. The audit found better: (a) `rigor check` answered `[]` and **exited 0** on a file Prism cannot parse; ported 1:1 as `rule: null` rows (PR #125, 106→107 fixtures, corpus diff exactly +9), the SKIP decision untouched. (b) the ADR-0033 declaration-only leniency's **premise expired** 2026-07-31 when `vendored_gem_sigs/` landed — the port's surface is complete (0 false witnesses / 651-name vocabulary per class), two comments still assert the stale fact, and its 29 rows are FOUR mechanisms → **#123** (a late overlay `Kernel` reopen is not carried to subclasses; 26 holes, blocks the 7 closable rows) and **#124** (no arity/ATM check on a SINGLETON receiver at all — `Time.at()` silent; ≥23 rows, under-counted by bucketing on the receiver). [799](notes/20260909-gap-adjudication-799.md) / [gems](notes/20260909-declared-unwitnessed-gem-classes.md) / [parse](notes/20260909-parse-error-reporting.md).
- **2026-09-09 #118 CLOSED — the generic dispatch stops pinning the flat slot** (PR #120) — the issue's premise was half wrong: the port has no per-call-site overload selector, so the defect is tier 3's flat slot DROPPING the nil bit (`String#[]`'s arms all return `String?`, they "agree", the class survives and the optionality does not) and measuring agreement AFTER erasure. Declines to `Dynamic[top]` when the join is at risk AND an argument is reference-untyped: **13 FPs closed, 0 matched rows lost, 9204 corpus files byte-identical** (the 0 FP / 799 gaps baseline is unchanged). **The narrowing IS the finding**: a blanket "nilable declines" closed the same FPs, left the harness unmoved, and silently cost TEN matched rows — the folded-literal ones the reference reports by CONSTANT-FOLDING; untypedness is exactly what stops it folding. Residue **#121**. Same PR: fixture 103's live-equality row tested `RUBY_VERSION == "4.0.5"`, which flips verdict on a host patch bump while the port folds the baked constant — now `RUBY_ENGINE`, hazard in `UPSTREAM.md`. [note](notes/20260909-generic-dispatch-untyped-arg.md).
- **2026-09-09 upstream re-pin `v0.3.4 → v0.3.8`** (924 commits / 4 releases; branch `upstream-pin-v0.3.8`) — **0 FP / 9204, gaps 820→799**, harness 98→**105** fixtures / 0 extras, effects gate 0 OVER. Every re-sync half moved (rbs 4.2.0, plugin sig ×2, effects catalogue + snapshot schema 2). Raw bump = 7 fixture retractions + 12 sweep FPs = **SIX families**, each bisected to its commit (#537 untyped-arg overloads, #533 unorderable guards, #739 module receivers, #619 constant-write meta bodies, #627 dead version-guard arms, #540 mutated constants) and ported by four worktree agents; plus the effects gate's own retraction (#446 `super` taints). **Three spec claims were wrong and the must-fire controls caught them** (the `Dynamic[top]` gate; "decline `Psych::VERSION`" — drop BOTH arms; F-C's site is `check_narrowed_call`). Residues: #118 (`"abc"[u]`, pre-existing FP), 20 resolvable-`super` rows. **Upstream perf regression** (rufo `formatter.rb`, `acd35612`/#547) makes `--sweep` 80 min. [note](notes/20260909-repin-v038.md) / [spec](notes/20260909-repin-v038-port-spec.md) / [feedback 4](notes/20260909-upstream-feedback-batch4.md).
- **2026-08-26/28 the effects GATE was lying, three times** (PRs #112/#115/#117) — (a) an arm with the `unresolved-self-call` taint DELETED scored byte-identically, so `mastodon/app` runs by default and `gitlab-foss/lib` behind `--scale`, each COPIED into a temp project — the wrong arm now fails by **76 OVER**; (b) slice 2's `methods: {}` guard covered 2 of the **4** declared-lane producers, and (a)'s synthesised config had ERASED the only shape exercising them — **a normalisation that removes a confound can remove coverage**; (c) `omit?` INVERTS ADR-0043 §2, so the writer drops its clause 3 (and, since `v0.3.8`, reads clause 1's exhaustive term as true): the port's taint bit can only DROP a row, never manufacture one. All three found by trying to make the gate lie. [s112](notes/20260826-s112-effects-instrument.md) / [s5](notes/20260826-effects-s5-probe.md) / [s116](notes/20260826-s116-snapshot-gate.md).


- **2026-08-26 LSP honours `rootUri` / `workspaceFolders`** (PR #110) — the server took the process CWD as the project root, so an editor spawning it elsewhere got the wrong config, `sig/` and discovery. It now **ENTERS** the client's root (`workspaceFolders` → `rootUri` → `rootPath` → cwd) rather than threading one: the root IS a cwd in all five consumers, so parity with `cd <root> && rigor check` holds by construction. **The probe is why**: threading would have missed `sig/` AND regressed `exclude:` (its spellings are deliberately RELATIVE), vacuously passing the existing matrix. Multi-root takes the first folder and discloses. [note](notes/20260826-s111-lsp-rooturi.md).

- **2026-08-25/26 two arcs CLOSED, folded** — the EFFECT-SYSTEM slices 0–3 (PRs #91/#100/#105/#107/#108/#111: summaries graded per METHOD as a sound subset; a typer-free Prism collector; upstream's TRANSITIVE exhaustive bit — the direct bit is 986 OVER on mastodon; **35 MATCH / 11 UNDER / 0 OVER**, and `05_posture`/`07_mutators` are GENERATED from the vendored tables after a live OVER hand fixtures could not see — [s3](notes/20260826-effects-s3-impl.md) / [#106](notes/20260826-s106-posture-over-fix.md)); and the frozen-index arc (PRs #95/#97-#99/#101/#103/#109/#113: harvest/merge split, ancestor closure 164.9→82.5ms, LSP held harvests with OverlayGuard ON, #113's Pass-4b fold capture making the decline SYNTACTIC. **Standing**: file order is NORMATIVE in `merge`; eviction stays BLOCKED; harvest cache NO-GO — [s113](notes/20260826-s113-fold-capture-impl.md)).
- **2026-08-23/25 re-pin `v0.3.2 → v0.3.4` + master survey (HOLD)** — 151 commits: **0 FP / 9204, gaps 841→820**, harness 97 fixtures; rbs and `data/` unchanged, exception tables empty; the raw bump opened **50 FPs**, all upstream RETRACTIONS (#319, #318) from OUR batch-3 reports, invisible to the snapshot diff ([note](notes/20260823-repin-v034.md)). The 64-commit survey moved 0 diagnostics but found the **vendored plugin RBS had drifted since 2026-06-26 = 10 FPs** (a THIRD pin-tracking surface; fixture 98 + ritual step 3 now cover it) and that every `documentation_url` 404s (upstream #438) ([survey](notes/20260825-upstream-survey-v034-master.md)).
- **2026-08-09 unresolved-const-receiver carrier — BUILT, REJECTED at 0 rows** (PR #89, closed) — sound but **841→841 on 9204**, and its first allow-list member needs both engines' INDEXES to agree — invisible to a core+stdlib sweep. [note](notes/20260809-unresolved-const-receiver-carrier.md).
- **2026-08-09 era (3 slices, folded)** — re-pin `v0.3.1 → v0.3.2` (+rbs 4.1.1): 0 FP / 9204, gaps 1125→841 (upstream retracting possible-nil FPs, #297); BOTH exception tables emptied; **trap: bundler/rubygems sigs depend on the rbs gem's `sig/shims/` — 2 FPs the sweep CANNOT SEE**, closed by `overlay/rbs_shims/` ([note](notes/20260809-repin-v032.md)); join-wipe retention (`retain_joined_facts` + the `else`-carrier unwrap; 1 FP closed, 15 probe shapes ref-matched — [note](notes/20260809-join-wipe-retention.md)); chain-guard meet (`chains` carries `ClassFact`, `narrow_nominal_to_class` shared by both arms; 2 FPs closed — [note](notes/20260809-chain-guard-meet.md)).
- **2026-08-08 era, folded (0 FP / 9204 throughout)** — the narrowing/shape trio (PRs #70/#75/#78/#80–#82: sequential-guard meet, qualified-name WITNESSING 1136→1127, the collection-shape ARC — **26 rows closed**; [meet](notes/20260808-sequential-guard-meet.md) / [witnessing](notes/20260808-qualified-witnessing-mini-spec.md) / [shape](notes/20260807-collection-shape-slice-spec.md)); the `Object` bucket ADJUDICATED, 30 rows behind decisions, 18 of them reference FPs fixed upstream at `v0.3.4` (PR #85, [adjudication](notes/20260808-object-bucket-adjudication.md)); constant-value harvesting per-file gate + partial containers (PRs #83/#84: 1127→1125; chain constants DECLINED — [mini-spec](notes/20260808-partial-constant-harvest-mini-spec.md)).
- **2026-08-07/08 the class-narrowing ARC, CLOSED at a measured stop** (PRs #63, #68, #71-#74, #76, #77, #79) — ported `narrow_class_other` end-to-end (snapshot pass, statement-form descent, compound predicates, `next`/`break`, chain guards): **19 gap closures + eleven master FP shapes**, 0 FP / 9204 at every step. Three probe-forced lessons: the FP-safety argument was WRONG THREE TIMES (position AXIS; carrier ALLOW-list; disjoint→`Bot`); census windows measure PROXIMITY not mechanism; **verify the CONSUMPTION gate can witness the class before crediting rows**. [spec](notes/20260807-class-narrowing-slice-spec.md) / [stage3](notes/20260807-narrowing-stage3-spec.md).
- **2026-08-01/08 instruments + adjudication, folded** — the 0-FP gate could pass VACUOUSLY (PR #65: corpus tools measured `target/release` while `cargo build` writes debug, and `run_rs` swallowed failures into `[]` — [note](notes/20260807-fp-audit-port-side-blind-spots.md)); the coverage-gap CENSUS buckets gaps by MECHANISM, not rule, and half sit behind decisions already made ([note](notes/20260807-gap-census.md)); `arity_eligible?` was never ported = a `call.wrong-arity` FP (fixture 80); LSP config reload keeps LAST GOOD ([lsp](notes/20260801-lsp-config-reload.md)).
- **2026-08-07 ADR-0042 S5: qualified return-lookup routing** (PR #64, MERGED) — the 8-member return family routes namespaced receivers via the qualified registry (refs AS WRITTEN + lexical ctx; ambiguity DECLINES); **14 closures (→1179), 0 FP / 9204**; fixture 82 pins the Tier-3 instance boundary (gaps 3→4 on merge). [spec+outcome](notes/20260807-adr0042-s5-return-lookup-spec.md).
- **2026-08-07 upstream survey + feedback batch 2, folded** — the `v0.3.1`→`80aaf9bc` 2×2 self-diff moved 2 diagnostics on 9204 (superseded by the `v0.3.2` re-pin) and RETIRED GEM_HOME rbs selection, which had silently dropped 1650 files ([note](notes/20260807-upstream-survey-v031-to-master.md)); batch 2 filed 3 reference-side defects with paste-ready repros — the `c7f28da1` master FP, the `Dynamic|nil` possible-nil FP class, and the fail-soft definition build blinding 12 classes ([note](notes/20260807-upstream-feedback-batch2.md)).
- **2026-07-31 era (7 slices, folded)** — sig-gen `Data.define`/`Struct.new` members; `BigMath` blinded-oracle asymmetry CLOSED (`UNBUILDABLE_DEFINITIONS` keyed pin × gems); LSP v4 const completion + private-decl visibility; survey FP triage 24→0, nine root causes, SIX unreachable by the fixture corpus ([note](notes/20260731-survey-fp-triage-24.md)); the project-`sig/` blind-spot probe → fixture 79; `-> self` on instance methods; the standing sweep set CODIFIED ([CORPUS.md](../harness/CORPUS.md)).
- **2026-07-31 upstream pin `v0.3.0 → v0.3.1` + vendored rbs `4.0.3 → 4.1.0`** — **0 FP / 9153 files, gaps net −2**. 4.1.0's rewritten signatures broke two things, both FIXED rather than accepted: bounded method type params now resolve to their bound, and `-> instance` on an INSTANCE method resolves via the `SELF_RETURN` call-site sentinel. New `harness/vendor_rbs.py` makes the vendoring recipe executable (proven by reproducing the 4.0.3 tree byte-for-byte first). Upstream logic delta: ZERO. [note](notes/20260731-upstream-pin-v031-rbs41.md) / [survey](notes/20260731-v031-preflight-survey.md).
- **2026-07-25 era (3 slices, folded)** — MultiWrite substrate s1+s2 (PRs #46/#47: arena lowering + `MultiTargetBinder`, RBS tuple returns; 14 corpora / 35,706 files bit-identical — [spec](notes/20260725-multiwrite-substrate-spec.md)); LSP `exclude:` parity (PR #45, matrix 24→144); LSP stage-3 parity tail (PR #44, 4 E2E tests vs real `check`, each proven non-vacuous).

- **2026-07-18/19 era (5 arcs + the RC bump, folded)** — LSP §12 two-tier S1–S4b (PRs #35–#38/#42/#43); upstream tracking `b70adcb5..ff6b6158` (0 added / 0 dropped, nothing to port); `coverage` precision mode + MCP tool + node-granularity audit (PRs #33/#40); ADR-0042 gate + Slices 1–4 ([deliverables](notes/20260719-adr0042-gate-deliverables.md)); the compat arc Phases 0/1/3 + M2 receiver typing + severity machinery + `--bleeding-edge` ([findings](notes/20260718-phase0-m1-m2-findings.md)); and the RC bump `47ec8625→7a69f142` (80 commits, 2 parity divergences closed at 0 FP — [note](notes/20260718-upstream-rc-bump-47ec8625-7a69f142.md)).
- **2026-07-17 docs economy + Tier B/C track CLOSED** — CURRENT_WORK.md 184KB→baton + [PORT_BACKLOG.md](PORT_BACKLOG.md) split, byte-budget gate `harness/docs_check.py` + docs CI (port of upstream rigor#119, issue #21); and the Tier B/C / ScopeIndexer no-go, evidence-backed — see Standing conclusions, [note](notes/20260717-tier-bc-track-closed.md).
- **2026-07-17 ATM arc + two undefined-method receiver-typing wins, MERGED** — `call.argument-type-mismatch` on both channels (per-overload RBS retention + acceptance walk, byte-exact, 0 FP); plus the constant-shadow gate and the C3a String-tail, together taking gitlab UM 356→179. [spec](notes/20260717-atm-substrate-arc-plan.md).
- **2026-07-16 two MERGED inference slices** — `def.ivar-write-mismatch` (`a2098d7`: ivar-write lowering + rescue binding + Kernel cast fallback + collector; gitlab ivar gaps 2→0) and literal-tail return folding (`0721943`: interprocedural singleton-method literal fold, depth-16, ancestry-scoped; gitlab always-truthy 28→16). 0 FP both. [ivar](notes/20260716-ivar-write-mismatch-spec.md) / [fold](notes/20260716-literal-tail-fold-spec.md).
- **2026-07-16 v0.3.0-RC arc: pin `47ec8625` + 7 slices, ALL MERGED** — syntactic rules (dup-hash-key, return-in-ensure, suppression.*, `Node::Lambda`), MutationWidening (killed 2 measured FPs), implicit-self dispatch + `p`/`pp`, scalar HashShape keys + projection folds, Kernel `format`/casts folding, `raise-non-exception` + `class_ordering`, `shadowed-rescue-clause` + rbs.rs nesting root-fix. **v0.3.0 rule surface fully ported.** [specs](notes/20260716-v030-upstream-gap-survey.md).
- **2026-07-11 sig-gen arc CLOSED (13 slices) + periphery (4 items)** — `erase_to_rbs` → `--print` → return-union → singletons → `--write` → initialize stub → `--diff` → module_function → Writer merge+LayoutIndex → env classification → `--overwrite` → qualified naming → Data/Struct shells (`ee60d41`…`33f9436`); 0 shared-method mismatch on the full sweep, `--write` sound. Periphery: MCP `sig_gen` tool (`e7ae83e`); `--params=observed` SUBSTRATE-BLOCKED, not built ([note](notes/20260711-siggen-params-observed-substrate-blocked.md)); conditional-assign nilability BUILT not merged (`7b7fe3d`, 0 survey gaps); coverage frontier re-measured — bounded wins exhausted ([note](notes/20260711-coverage-frontier-remeasured.md)).
- **2026-06-26…07-10 pre-/foundation era (15 arcs, all closed)** — sidecar COMPLETE + perf slices retired by measurement ([ADR-0036](adr/0036-ruby-sidecar-default-reversal.md)/[0037](adr/0037-sidecar-perf-slices-retired-by-measurement.md)); the RBS ingestion legs ([ADR-0033](adr/0033-project-sig-ingestion.md)/[0034](adr/0034-rbs-collection-ingestion.md), inline deferred [0035](adr/0035-inline-rbs-deferred.md)); `fp_audit` + 4 real FP clusters → 0 FP across ~4000 files; the productization cluster (triage/annotate/diff/config-audit/baseline, `check <dir>`); the flow substrate ([ADR-0038](adr/0038-flow-substrate-incremental-narrowing.md)/[0039](adr/0039-shape-typing-tier.md)) with the standing finding **no cheap FP-safe flow wins left**; rayon (~2.4×), LSP v1/v2, MCP; leniency alignment; rustfmt stance ([ADR-0032](adr/0032-source-formatting-policy.md)); v0.0.1 release prep; pure-RBS bundle track CLOSED; ADR-72 Gemfile.lock auto-overlay (`96d7f47`).
