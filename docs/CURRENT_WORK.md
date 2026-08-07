# rigor-rs — Current Work

The session-to-session baton: **what is in flight, what to pull next, and a
one-line ledger of what landed**. The complete per-subsystem port map is
[PORT_BACKLOG.md](PORT_BACKLOG.md); measured outcomes and narratives live in
`docs/notes/` + `docs/adr/`; history is `git log`.

**Contract (gated by `harness/docs_check.py`):** a landed/closed arc gets ONE
ledger line here — verdict + numbers + link — and its detail goes to a dated
note or ADR *first*. No status essays; this file has a hard byte budget.

Last updated: 2026-08-08.

## Now / Next

**Track B (productization) 2026-07-19/25** — all in the ledger: coverage
precision mode (#33), LSP §12 COMPLETE (S1–S4b) + stage-3 parity tail (#44),
MultiWrite substrate (#46/#47), LSP `exclude:` parity (#45), the RBS ingestion
asymmetry (**BOTH halves closed**). ▶ NEXT: LSP v4+ (`rootUri`, UTF-16 incremental
sync); or the next upstream tag when it lands. Sweep
set CODIFIED (`sweep-corpora.yml` + `--sweep`).
- Clippy verify MUST use `CARGO_TARGET_DIR=<fresh> cargo clippy --workspace --
  -D warnings` (the incremental cache hides `only_used_in_recursion` etc. —
  cost a CI red on PR #32).
- Coverage-tool parity lesson (binding for measurement tools): audit at NODE
  granularity — per-file histograms net over-claims out against under-claims.

Default track is **productization** (measurement-proven highest ROI; the
parity-port arc has bottomed out — see Standing conclusions):

- **CLOSED arcs** (in the ledger; do not re-open): ADR-0042 core migration
  (PRs #31/#32, accepted) and the compat next-stage plan (Phases 0–3 done,
  exhausted — [plan](notes/20260718-compat-next-stage-plan.md)).
- **CLI surface from the v0.3.0 RC** — `--bleeding-edge` + severity
  profile/overrides + `coverage` precision mode DONE; remaining: plugins
  inflection probe. `--protection`/`--mutation` (ADR-63/70) + `type-scan`
  deferred by [scoping call](notes/20260719-coverage-command-scoping.md).
- **Pin is `v0.3.1`** (+ vendored rbs 4.1.0). Upstream master is +150 with no
  tag, a measured delta of TWO diagnostics and an rbs bump that moves no
  signature, so the re-pin waits for a tag (`UPSTREAM.md`, all THREE hazards).
- Deferred RC deltas (documented): interprocedural mutation floor (P6),
  plugin-only changes (no plugin engine). The UM-residual INVESTIGATION and the
  remaining RC inference deltas are absorbed into the compat plan (M2 / Phase 2).

State: harness **82 fixtures / 0 FP / 4 gaps / 1 registered divergence** (live +
snapshot, vs pin `v0.3.1`; 3 are fixture 72's `rule: null` parse diagnostics, 1
is fixture 82's deliberate Tier-3 boundary). Standing sweep: **0 FP / 9204
files / 1168 gaps**, 8 corpora, baselines in `harness/CORPUS.md`. Neither sweep
tool sees project-`sig/` behaviour. The sweep measures `target/release/rigor`
and REFUSES a binary older than `crates/` (PR #65). Clippy: workspace
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

- **2026-08-08 harness integrity: the 0-FP gate could pass VACUOUSLY** (PR #65) — `fp_audit`/`gap_census` measured `target/release/rigor` while `cargo build` writes debug (a 6-day-old binary made PRs #63/#64 read as "closed nothing"), and `run_rs` swallowed every failure into `[]`, so a panicking port binary scored 0 FP. `run_corpus.rb` carried BOTH defects; the reference side was only half-hardened (a SKIPPED oracle still exited 0). Now: auto-build, STALE-binary refusal vs newest `crates/` file, path+mtime in the header, `None`-not-`[]` on failure, INVALID corpora fail the run. Review killed an exit-code-vs-emptiness check that false-fired on warning-only batches. [note](notes/20260807-fp-audit-port-side-blind-spots.md).
- **2026-08-07 ADR-0042 S5: qualified return-lookup routing** (PR #64, MERGED) — the 8-member return family routes namespaced receivers via the qualified registry (refs AS WRITTEN + lexical ctx; ambiguity DECLINES); **14 closures (→1179), 0 FP / 9204**; fixture 82 pins the Tier-3 instance boundary (gaps 3→4 on merge). [spec+outcome](notes/20260807-adr0042-s5-return-lookup-spec.md).
- **2026-08-07 `is_a?`/`case-when` class narrowing** (PR #63, MERGED) — snapshot-pass port of `narrow_class_other` + `Node::When` arena split; **11 closures (1193→1182 verified), 0 FP / 9204**; 3 unprobed edges declined on review; `to_s` slice REFUTED. Its safe-nav decline was the WRONG AXIS — the real rule is positional and 8 FP shapes shipped with it: [position rule](notes/20260807-block-narrowing-position-rule.md). [spec+outcome](notes/20260807-class-narrowing-slice-spec.md).
- **2026-08-07 upstream feedback batch 2** — 3 reference-side defects with paste-ready repros: the `c7f28da1` (#271) master FP, the `Dynamic|nil` possible-nil FP class, the fail-soft definition build blinding 12 classes. [note](notes/20260807-upstream-feedback-batch2.md).
- **2026-08-07 coverage-gap CENSUS — gaps are six MECHANISMS, not one number** — `harness/gap_census.py` buckets every gap by the mechanism in the reference's own message; the per-rule histogram hides which. Half the set sits behind decisions already made (possible-nil Tier B/C, always-truthy flow frontier), so quoting the total invites re-litigating them. The actionable pool is `undefined-method`, dominated by receiver typing on CORE classes where the port HAS the signature. Two of its three named slices survived probing; `X.to_s` → String was REFUTED. [note](notes/20260807-gap-census.md).
- **2026-08-01 `Object#Nokogiri` asymmetry was already CLOSED — and the sibling sweep found a live FP next door** — both engines silent at the pin (`800b3a1` had vendored `data/vendored_gem_sigs/`); stale notes now carry dated corrections, non-vacuity proven by ablation. The 14-name conversion-family sweep found ingestion at parity but exposed a `call.wrong-arity` FP: **`arity_eligible?` was never ported** (507 of 11,115 methods; no envelope on required-keyword / trailing-positional / `UntypedFunction` overloads). Sweep OUTPUT-NEUTRAL (0 FP / 9204); fixture 80 pins both mechanisms. [note](notes/20260801-nokogiri-ingestion-asymmetry-closed.md).
- **2026-08-01 LSP config reload — `.rigor.yml` re-parsed by every structural `invalidate`** (S4 limitation CLOSED; decision BEAT the reference, which rebuilds from the retained `@configuration` at `v0.3.1` AND master). Broken file keeps LAST GOOD + one warning; deleted reloads defaults (`ConfigRead`). 8 tests / 7 mutations; gates UNMOVED (79 / 0 FP / 3 gaps / 1 divergence; sweep 0 FP / 9204). [note](notes/20260801-lsp-config-reload.md).
- **2026-07-31 sig-gen reads `Data.define` / `Struct.new` classes** (upstream #227 `da9b045e`, past the `v0.3.1` pin) — an empty class SHELL is worse than none: declaring the class narrows dispatch to a nominal, and the inherited `::Data.new: () -> bot` then makes every construction an arity error. Both spellings now emit member readers (+ Struct writers), a `.new`/`.[]` pair matching the forms the class accepts, and the `::Data`/`::Struct[untyped]` ancestry; a `do…end` block's defs bind on the NEW class, and `--print` stops printing a module as a `class`. **Byte-identical to upstream `master`** on `--print`/`--write`; corpus match 20→30 files, 0 regressions; diagnostic gates UNMOVED (0 FP, gap table identical). [note](notes/20260731-siggen-data-struct-members.md).
- **2026-07-31 `BigMath` ingestion asymmetry CLOSED — the oracle is BLINDED, not ignorant** — the s2 note's cause was wrong: the reference LOADS `module BigMath`, the DEFINITION build raises and fails soft to nil, so it is silent on every method while `class_known?` stays true. Mirrored by `UNBUILDABLE_DEFINITIONS` (12 classes, instance/singleton apart), regenerable via `harness/unbuildable_classes.rb --check`. **The set is keyed to pin × installed gems** — measured A/B: omit the `bigdecimal` gem and the same pinned oracle FIRES; 1 of the 12 entries is env-supplied. 0 FP / 9204 files, output-neutral. [note](notes/20260731-bigmath-ingestion-asymmetry.md).
- **2026-07-31 LSP v4 completion slice** — `Foo::` now offers NESTED CONSTANTS via `namespace_children` over the ADR-0042 qualified registry, split on the CASE of the name after `::` (Ruby's own rule, without the reference's second parse). Private RBS declarations are no longer offered on an explicit receiver, while the DIAGNOSTIC predicates deliberately still see them as present (`send(:foo)` dispatches ⇒ witnessing absence would be an FP) — hence the sweep is unchanged. Union receivers complete the per-arm INTERSECTION. [note](notes/20260731-lsp-v4-const-completion-visibility.md).
- **2026-07-31 survey FP triage — 24 → 0, nine root causes** — the pin bump's side finding, closed. SIX of the nine are scoping/unit bugs an all-ASCII well-formed fixture corpus structurally cannot contain (columns counted in scalars rather than Prism's BYTES; scope/lowering slips), which is the lesson: run `fp_audit --sweep` on the survey corpora after ANY scoping, position or lowering change. [note](notes/20260731-survey-fp-triage-24.md).
- **2026-07-31 project-`sig/` blind-spot probe — 7 shapes, 3 findings** — no sweep touches project sigs (both tools run core+stdlib from a clean cwd), so a matrix was run by hand. Found a **latent FP against the pin**: a `sig/` referencing an undeclared interface/alias makes the pinned reference discard its WHOLE stub batch silently (upstream #237, fixed after `v0.3.1`) while rigor-rs stubs per kind and fires — now gated as the registry's FIRST excused divergence + fixture 79 (delete both when the pin passes `9515c8f8`). Two more left as their own slices: a broken `sig/` is silently ignored, and an unqualified nested-namespace call is a gap. [note](notes/20260731-project-sig-blind-spot-probe.md).
- **2026-07-31 `-> self` on INSTANCE methods** — resolved only for SINGLETONs and BLOCK overloads before, so an instance method's `-> self` collapsed to Dynamic and killed the chain. Now rides the `SELF_RETURN` sentinel → the RECEIVER's class; a SINGLETON `-> self` is the class OBJECT and keeps declining (fixture 77 pins that as a negative control). **1 gap closed, 0 FP / 9204 files** (gitlab 329→328). The textual prediction was NEGATIVE and wrong: a chain breaks at its FIRST unresolved link. [note](notes/20260731-self-return-instance-methods.md).
- **2026-07-31 standing FP sweep set CODIFIED** — `harness/sweep-corpora.yml` is the single membership list for BOTH `fp_audit.py --sweep` and `run_corpus.rb` (each entry carries the `why:` it earns its place with; an absent corpus reports SKIPPED, never drops silently). Closes the ▶NEXT item the 24-FP triage left. Also fixed `run_corpus.rb`'s `REFERENCE_RIGOR_DIR` default — it pointed at the WORKING checkout, hazard 3 baked into a gate. Baseline **0 FP / 9204 files / 8 corpora** ([CORPUS.md](../harness/CORPUS.md)).
- **2026-08-07 upstream survey `v0.3.1`→`80aaf9bc` (150 commits + rbs 4.1.1) — pin HOLDS** (supersedes the +49 survey, whose one portable item, the #121 Tuple set-op folds `a2867efd`, is ported: `eql?` not `==`, so `[1] & [1.0]` is EMPTY; no fixture until the pin passes it). 2×2 self-diff (checkout × rbs) over the sweep set: upstream logic moves **2 diagnostics on 9204 files** — a drop (#239 rdoc) and an ADD bisected to `c7f28da1` (#271), which is a NEW UPSTREAM FP (a nested `Struct.new` pins an empty member, `.first` folds nil on correct code); **rigor-rs silent at both**. rbs 4.1.0→4.1.1 is **0/0 and structurally so**: the two gems' `core/`+`stdlib/` are BYTE-IDENTICAL, so the re-vendor is a no-op. Port side under a 4.1.1 mirror (built via `vendor_rbs.py` + `carry_into`, so `overlay/` is present — 660 classes, vs 539 without it, the pre-flight survey's confound) **0/0**; `unbuildable_classes.rb --check` OK/12 at pin, at master, and under 4.1.1. E2E: rigor-rs is **0 FP / 1193 gaps vs BOTH** oracles, corpus for corpus — the bump buys nothing, and no new parity-set rule landed. GEM_HOME rbs selection is RETIRED: it broke dependabot-core (`parser >= 3.3.7.2`), silently dropping 1650 files; use `ruby -I <rbs>/lib -I <ext>`. [note](notes/20260807-upstream-survey-v031-to-master.md).
- **2026-07-31 upstream pin `v0.3.0 → v0.3.1` + vendored rbs `4.0.3 → 4.1.0`** (one commit — an oracle on other core signatures than the port reads is not an oracle; same day, via an intermediate `7a69f142 → v0.3.0` tag bump that was behaviour-neutral bar the ADR-93 inline-RBS auto-wire, [note](notes/20260731-upstream-bump-7a69f142-v030.md)) — **0 FP / 9153 files, gaps net −2** (mastodon 49→48, gitlab 330→329, concurrent 87→86). 4.1.0's rewritten signatures broke two things, both FIXED rather than accepted as the survey assumed: bounded method type params (`Array#fetch`'s `[I < _ToInt] (I index)` admitted everything) now resolve to their bound, and `-> instance` on an INSTANCE method (`Hash#compact`) now resolves to the receiver's class via the `SELF_RETURN` call-site sentinel. New `harness/vendor_rbs.py` makes the vendoring recipe executable — proven by reproducing the committed 4.0.3 tree byte-for-byte before writing 4.1.0. Upstream logic delta itself: ZERO. [note](notes/20260731-upstream-pin-v031-rbs41.md) / [survey](notes/20260731-v031-preflight-survey.md).
- **2026-07-25 MultiWrite substrate, slices 1+2** (PRs #46/#47) — `a, b = rhs` had NO arena lowering, so `collect_flow_writes` never saw the rebind: a MEASURED live `check` FP. Added `Node::MultiWrite` (+ `target_exprs` for locals inside non-local targets — an FP the harness, unit tests and 3 corpora all missed) + a rule-for-rule port of `MultiTargetBinder`; ~32k-file sweep 2 removed / 5 added / 0 new FPs. Slice 2 then closed the LAST fixture gap (68) by surviving RBS tuple returns as `RbsReturnShape{Class|Tuple|Unknown}` beside an untouched `method_signature`, + Pass 2b minting ids for the 17 classes an RBS tuple names. **An FP found and fixed mid-build**: naive witness widening fired `Gem::Version#segments` where the oracle is silent — gated the WITNESS on `is_declaration_only_class`. 14 corpora / 35,706 files bit-identical. [spec](notes/20260725-multiwrite-substrate-spec.md) / [s1](notes/20260725-multiwrite-substrate-s1.md) / [s2](notes/20260725-multiwrite-substrate-s2.md).
- **2026-07-25 LSP `exclude:` parity** (PR #45) — an excluded file opened in the editor published markers `check` never produces. Decision rule: a buffer is excluded iff EVERY discovery spelling of it is excluded, expressed as 3 tiers — overlay membership (fast path), all-roots × 3 name forms (decoded/named/canonical, `all` load-bearing), then a confirm pass over the SHARED `discovery_spellings` walk so it cannot drift from `project_files`. Review round 1 found 3 regressions (symlinked `.rb`, symlink whose target is excluded, overlapping roots — the first-match + canonicalize-only gate); matrix widened 24→144 runs (+ symlink and multi-root axes). `check` byte-identical under 3 configs incl. an invalid glob. [note](notes/20260725-lsp-exclude-parity.md).

- **2026-07-25 LSP stage-3 parity tail** (PR #44) — the LSP never applied the ADR-8 SeverityStamp: a rule resolved `off` still published markers `check` DROPS (PRESENCE mismatch), severities were authored not profile-resolved, bleeding-edge unplumbed. The stamp now rides `ProjectContext` beside `disable:` (rebuilt by `invalidate` under the S4 generation guard); composition order verified against `main.rs` statement-by-statement. 4 E2E tests vs real `check` output, each proven non-vacuous by re-breaking. Remaining divergences enumerated in the note. [note](notes/20260725-lsp-stage3-parity.md).

- **2026-07-19 LSP §12 two-tier COMPLETE (S1–S4b)** (PRs #35–#38/#42/#43 + `16bfb9e`) — `select!` loop + BufferTable, 200 ms debounce, rayon workers, 3-axis stale-drop, generation guard. Alongside it, **baseline drift/prune positional roots + scope guard** (PR #41), found by a real-product baseline check (conference-app, no `.rigor.yml`): drift/prune ignored positional roots. [plan](notes/20260719-lsp-s12-two-tier-impl-plan.md).

- **2026-07-19 upstream tracking `b70adcb5..ff6b6158`** (29 commits, three waves: transitive-void ADR-100 WD4, type-of plugin-env parity, the #194 loader stack landed+closed upstream, ADR-58 massign-ivar seeding, ADR-67 WD6 call-site param inference) — hardened self-diff **0 added / 0 dropped** on all four battery corpora every wave; nothing to port.
- **2026-07-19 `coverage` precision mode + MCP tool + over-claim audit** (PRs #33/#40) — the reference precision-tier scan on rayon (`--workers`) + the MCP `rigor_coverage` tool; the node-granularity audit found 0 wrong claims (its lesson is in Now/Next).
- **2026-07-19 ADR-0042 qualified-key migration, gate + Slices 1–4** (branches `adr-0042-gate`/`-impl`/`-instance`, subagent-parallel) — the gate first: a 12-scenario oracle matrix → fixtures 68–70 pin the 9-gap nested-class surface, a consumer inventory found no unsound consumer under alias-collapse (+2 latent-FP sites the migration fixes free, +1 scope item absorbed into the ADR) and the s5 bare-door oracle-FP shape CLOSED (witness gate → `knows_toplevel_class` only). Then the substrate itself (additive, gates byte-unchanged) + qualified singleton and INSTANCE witnessing, incl. the fixture-70 shadow-sig unsoundness fix. [ADR](adr/0042-qualified-key-index-registration.md) / [deliverables](notes/20260719-adr0042-gate-deliverables.md).
- **2026-07-18 compat arc (Phases 0/1/3) + three subsystems, folded** — Phase 0 characterisation ([findings](notes/20260718-phase0-m1-m2-findings.md)); Phase 1 fixture parity 100% (PR #24); Phase 3 unknown-config-key warning byte-exact ([note](notes/20260718-phase3-new-rule-surfaces.md)); the M2-GO receiver-typing batch, gitlab UM 179→148 ([note](notes/20260718-m2-receiver-typing-batch.md)); severity-resolution machinery, 8/8 config byte-diffs identical ([note](notes/20260718-severity-profile-machinery.md)); `--bleeding-edge` + `static.value-use.void` ([note](notes/20260718-bleeding-edge-void-rule.md)); and the rbs-inline auto-wire regression measurement that HELD the pin ([note](notes/20260718-upstream-rbs-inline-autowire-regressions.md)).
- **2026-07-18 upstream RC bump `47ec8625→7a69f142`** (80 commits) — two parity divergences closed 0 FP: `suppression.unknown-marker` (new rule, upstream `4e0ca475`) + Kernel intrinsic explicit-`Kernel.`-receiver fold (`c9d2e473`); live 188 matched / 193 ref, snapshots re-baselined, core corpora + survey FP-clean. Rest of the RC's inference precision deferred as coverage-only. [note](notes/20260718-upstream-rc-bump-47ec8625-7a69f142.md).
- **2026-07-17 docs economy + Tier B/C track CLOSED** — CURRENT_WORK.md 184KB→baton + [PORT_BACKLOG.md](PORT_BACKLOG.md) split, byte-budget gate `harness/docs_check.py` + docs CI (port of upstream rigor#119, issue #21); and the Tier B/C / ScopeIndexer no-go, evidence-backed — see Standing conclusions, [note](notes/20260717-tier-bc-track-closed.md).
- **2026-07-17 ATM arc + the two undefined-method receiver-typing wins, MERGED** — `call.argument-type-mismatch` on both channels (per-overload/per-param RBS retention + acceptance walk, byte-exact messages, 0 FP) with the P2 `Regexp.last_match` nilable source; plus the C1+C2+C5 constant-shadow gate and the C3a String-tail, which together took gitlab UM 356→179 at 0 FP. [spec](notes/20260717-atm-substrate-arc-plan.md).
- **2026-07-16 two MERGED inference slices** — `def.ivar-write-mismatch` (`a2098d7`: ivar-write lowering + rescue binding + Kernel cast fallback + collector; gitlab ivar gaps 2→0) and literal-tail return folding (`0721943`: interprocedural singleton-method literal fold, depth-16, ancestry-scoped; gitlab always-truthy 28→16). 0 FP both. [ivar](notes/20260716-ivar-write-mismatch-spec.md) / [fold](notes/20260716-literal-tail-fold-spec.md).
- **2026-07-16 v0.3.0-RC arc: pin `47ec8625` + 7 slices, ALL MERGED** — syntactic rules (dup-hash-key, return-in-ensure, suppression.*, `Node::Lambda`), MutationWidening (killed 2 measured FPs), implicit-self dispatch + `p`/`pp`, scalar HashShape keys + projection folds, Kernel `format`/casts folding, `raise-non-exception` + `class_ordering`, `shadowed-rescue-clause` + rbs.rs nesting root-fix. **v0.3.0 rule surface fully ported.** [specs](notes/20260716-v030-upstream-gap-survey.md).
- **2026-07-11 sig-gen arc CLOSED (13 slices) + periphery (4 items)** — `erase_to_rbs` → `--print` → return-union → singletons → `--write` → initialize stub → `--diff` → module_function → Writer merge+LayoutIndex → env classification → `--overwrite` → qualified naming → Data/Struct shells (`ee60d41`…`33f9436`); 0 shared-method mismatch on the full sweep, `--write` sound. Periphery: MCP `sig_gen` tool (`e7ae83e`); `--params=observed` SUBSTRATE-BLOCKED, not built ([note](notes/20260711-siggen-params-observed-substrate-blocked.md)); conditional-assign nilability BUILT not merged (`7b7fe3d`, 0 survey gaps); coverage frontier re-measured — bounded wins exhausted ([note](notes/20260711-coverage-frontier-remeasured.md)).
- **2026-06-26…07-10 pre-/foundation era (15 arcs, all closed — detail in the linked notes/ADRs)** — sidecar COMPLETE + perf slices retired by measurement ([ADR-0036](adr/0036-ruby-sidecar-default-reversal.md)/[0037](adr/0037-sidecar-perf-slices-retired-by-measurement.md)); the RBS ingestion legs ([ADR-0033](adr/0033-project-sig-ingestion.md)/[0034](adr/0034-rbs-collection-ingestion.md), inline deferred [0035](adr/0035-inline-rbs-deferred.md)); `fp_audit` + 4 real FP clusters → 0 FP across ~4000 files; the productization cluster (triage/annotate/diff/config-audit/baseline, `check <dir>`); the flow substrate ([ADR-0038](adr/0038-flow-substrate-incremental-narrowing.md)/[0039](adr/0039-shape-typing-tier.md)) with the standing finding **no cheap FP-safe flow wins left**; rayon (~2.4×), LSP v1/v2, MCP; leniency alignment; rustfmt stance ([ADR-0032](adr/0032-source-formatting-policy.md)); v0.0.1 release prep; pure-RBS bundle track CLOSED; ADR-72 Gemfile.lock auto-overlay (`96d7f47`).
