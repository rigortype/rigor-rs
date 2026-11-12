# rigor-rs — Current Work

The session-to-session baton: **what is in flight, what to pull next, and a
one-line ledger of what landed**. Port map: [PORT_BACKLOG.md](PORT_BACKLOG.md);
outcomes: `docs/notes/` + `docs/adr/`; history: `git log`.

**Contract (gated by `harness/docs_check.py`):** a landed/closed arc gets ONE
ledger line here — verdict + numbers + link — and its detail goes to a dated
note or ADR *first*. No status essays — hard byte budget.


## Now / Next

▶ **NEXT: the ADR-127 floor's coverage cost; follow-ups #489–#494.**
Older open: #130, #132, #156, #159, #169–#171; #160 blocked.
- **CLOSED arcs** (do not re-open): ADR-0042 migration + compat
  ([plan](notes/20260718-compat-next-stage-plan.md)).
- **CLI surface (v0.3.0 RC)** — `--bleeding-edge`, severity, `coverage` done;
  `--protection`/`--mutation` + `type-scan` deferred
  ([scoping call](notes/20260719-coverage-command-scoping.md)).
- **Pin is `v0.4.7`** (rbs 4.2.0 vendored). Both
  standing exception tables EMPTY; new entries are real findings
  (`UPSTREAM.md` hazards + overlay/`sig/shims` trap).

State (verified 2026-11-05, at `v0.4.7`): harness **170 fixtures / 0
unregistered / 0 divergent**; standing sweep **0 FP / 9,441 files / 623
gaps**, 8 corpora; effects gate 0 OVER. `--sweep` ~3 min.
Grading tools REFUSE stale builds. Clippy: `-D warnings`, FRESH `CARGO_TARGET_DIR`.

## Standing conclusions (do not re-litigate without new evidence)

- **Possible-nil / Tier B/C is CLOSED, not deferred** — the closing slice deletes
  the nameable-concrete-arm FP-safety mechanism; `fp_audit` would score it
  0 FP: the parity gate points the wrong way
  ([tier-bc](notes/20260717-tier-bc-track-closed.md)). **Its SCOPE: the 85
  `Dynamic`-arm rows, NOT the 93 concrete-arm ones**.
- **Reference-FP undefined-method clusters, CLOSED** — `pre_eval:` cross-file
  monkey-patch (closing INVERTS the ADR-0033 provenance gate), receiver-typed-`nil`,
  rdoc generated-parser `Hash` ([141](notes/20260807-gap-adjudication-141.md)).
  Re-adjudicated at `v0.3.8` ([799](notes/20260909-gap-adjudication-799.md)):
  **395 of 799 behind decisions; biggest = nested-scope receiver typing, 71 rows.**
- **Five consecutive FP-safe flow slices closed 0 survey gaps** — never build a
  coverage slice without a valid-mode `fp_audit --gaps` prediction
  ([flow-frontier](notes/20260706-flow-frontier-exhausted.md)).
- **The receiver-typing lever is open**: the gap census buckets gaps by
  MECHANISM ([census](notes/20260807-gap-census.md)); four slices closed
  **47 rows**. Re-run `gap_census.py --sweep` after each.
- **The effects TRANSITIVE LABEL lane is DECLINED — not portable at parity** (s4): four typer-free rules measured, best still 5 OVER; matching the reference outranks the ~2,000-method prize — labels stay UNDER. [probe](notes/20260826-effects-s4-probe.md).
- **sig-gen closed** — byte surface 0, `--write` sound.
- **Plugin work:** pure-RBS bundle track closed ([note](notes/20260710-pure-rbs-bundle-track-closed.md));
  sidecar + perf retired ([ADR-0037](adr/0037-sidecar-perf-slices-retired-by-measurement.md)).

## Build & gates

The gate tiers and their commands: `AGENTS.md` → Gates.

Reference oracle: pinned git submodule `reference/rigor` (see `UPSTREAM.md`);
run from a clean temp cwd. Sweep membership: `harness/sweep-corpora.yml`
(corpora under `/Users/megurine/repo/ruby/`). RBS is vendored + embedded at
build time (ADR-0007); `RIGOR_RBS_CORE_DIR` is the override seam and
`harness/vendor_rbs.py` regenerates the tree.

## Ledger (newest first; one line per arc/slice)

- **2026-11-12 #398 Kernel termination** (PR #575): dead-code termination uses `kernel_call_terminates`, not block timing's project-defines veto; explicit `Kernel.*` always terminates. 13+3 FP rows closed, 0 FP/658; follow-ups #581–#583. [note](notes/20261112-issue-398-kernel-termination.md)
- **2026-11-12 #349 Psych YAML errors** (PR #576): libyaml `context_mark`/problem/context render Psych positions/details incl. `hexdecimal`; BOM parity. 0 FP; follow-ups #579/#580. [note](notes/20261112-issue-349-yaml-fidelity.md)
- **2026-11-12 #384 ATM describe labels** (PR #573): nested union/optional/alias members flatten before `union_describe`; `Other` strips only root `::`. 12 rows byte-identical, 0 FP; follow-ups #577/#578. [note](notes/20261112-issue-384-render-other-drift.md)
- **2026-08-10 post-`if` write join** (issue #443 → PR #572) — `v = nil if p` joins `entry ∪ arm-write` (incl. `op=` + destructure binds) instead of a blanket `Dynamic`; `w = v ? 1 : "s"` unions correctly. Union-witness declines remain. [note](notes/20260810-if-write-join.md)
- **2026-08-10 `if`-edge narrowing: join per-edge narrowed types for predicate subjects** (issue #451 → PR #571) — `Empty` join semantics (`T ∪ Bot`), terminating-arm liveness (`return`/`throw`/`exit`/`abort`/`fail`), `expr_truthiness` on unmodeled predicates, `&.` on nil carriers, `bind_statement`/`if_edge_overlay` widened-on-`Other` only. 4 review rounds; 0 FP. [note](notes/20260810-if-edge-narrowing.md)
- **2026-11-06 `run_corpus.rb` REMOVED** (PR #495): a first-80-files sample of `fp_audit.py --sweep`; its one unique input, the reference's own trees, measures identically by path (examples 32 files, 0 FP). [PR](https://github.com/rigortype/rigor-rs/pull/495) / [CORPUS.md](../harness/CORPUS.md)
- **2026-11-05 walk follow-ups** (#483–#488): port-only rows fixed, `pre_eval:` declines; 0 FP / 623. [note](notes/20261105-walk-followups.md)
- **2026-11-05 re-pin walk → `v0.4.7` DONE** (#445…#481): 0 FP / 623 gaps. Last tag: callee floor, `core_root:`, CLI. [notes](notes/20261105-repin-v047.md)
- **2026-10-30 #436 CLOSED** (PR #440): reaching-write fan-out cap 256 → `UNKNOWN`; lexer-F0/F1 >1,600 s → 0.2 s, all 16,623 gem files ~10 s. Not decline-only (#442). 0 FP / 9,435; → #441–#443. [note](notes/20261030-fanout-cap-436.md)
- **2026-10-30 #422 CLOSED** (stack #434 = PRs #432/#433): `ReachIndex` hoist + exact `local_reach` memo; 10 of 12 slow gem files → <7 s (lexer-F0/F1 → #436), output byte-identical. 0 FP / 9,435; → #435–#439. [note](notes/20261030-reach-memo-422.md)
- **2026-10-28 #412 CLOSED** (PR #426): 256 MiB analysis stacks + a 25,000-edge nesting bound (deeper files "skipped", `internal-error`); the reference aborts (rigor#2455). 0 FP / 9,435; → #427–#431. [ADR-0045](adr/0045-stack-totality.md)
- **2026-10-27 splits** #204/#234/#258/#260/#279/#416/#418/#420: infer lib 14,084→144, rules 4,497→95; ast 3,756→80; source_index 4,650→668; lower_node→5 files (gh-stack). [note](notes/20261027-lower-node-family-split.md)
- **2026-10-08/14 dead-position + reach arc, folded** (#362/#363/#368/#379–#382/#386/#388, PRs #385–#404): `dead.rs` dead-position exclusion and its reach leaks, deferred-body descend, write-back drops, host-resolvable arg-class gate, `blocked`/`discarded` split. 0 FP. [dead](notes/20261008-i368-dead-positions.md) / [leaks](notes/20261014-i386-dead-position-leaks.md)
- **2026-09-30/10-01 CLI + config parity, folded** (#155/#157/#158/#360, PRs #335/#359/#371/#375): Psych config parity, value validation, OptionParser on all subcommands, deferred-subcommand grammar; → #345–#349, #376. [config](notes/20260930-issue-158-config.md) / [optparse](notes/20260930-issue-155-optparse.md)
- **2026-09-30/10-01 IndexedFlow arc, folded** (#304/#309/#325/#341–#343/#352/#357/#361/#366, PRs #333–#378): stored-slot narrowing and the writes that drop it (`[]=`, attr, `when` guards, blocked extents, blocks), `OperandEffects.any?`, `String#[]` post-write reads, `Range[::int]` rendering. 0 FP. [slots](notes/20260930-issue-325-slot-narrowing.md) / [blocks](notes/20261001-i366-block-attr-drop.md)
- **2026-09-28/30 index-write + scope arc, folded** (#134–#137/#139/#146/#306/#312/#332, PRs #284–#334): index and splice writes widen receivers, per-site operand env replay, closure shadowing, multi-valued args decline. 0 FP / 818. [index](notes/20260929-issue-134-index-widening.md) / [operand](notes/20260929-issue-136-operand-scope.md)
- **2026-09-28/30 output, config + RBS parity, folded** (#162/#163/#168/#194/#199/#201, PRs #281–#323): baseline, `text`/`github`/`sarif` bytes, project-`sig/` `use`, witness spelling, list keys, `exclude:` expansion. 0 FP. [formats](notes/20260928-issue-163-output-formats.md) / [rbs-use](notes/20260929-issue-168-rbs-use.md)
- **2026-09-27 #138/#164/#167 ABANDONED** (PRs #184/#177/#183 closed unmerged): full-parity review met the deepest infer file; branches keep the work (`6e86120`/`9502a3a`/`eb1edd3`). [note](notes/20260927-abandoned-infer-pr-stream.md).
- **2026-09-25/28 fold + binder slices, folded** (#121/#133/#140/#151/#153/#164–#166, PRs #148–#283): String lookup folds, nested-rebind decline, `tap` once, binder carrier kinds, splat arity, block-param shadowing; fixtures 112–119. 0 FP / 9,337. [rebind](notes/20260925-issue-133-jump-path-rebind.md) / [binder](notes/20260925-issues-151-153-binder-writes.md)
- **2026-09-26 #141 CLOSED** (PR #179): eval-block defs attribute to the receiver (`declaration_prefix` re-anchoring). **3,002 gaps (→827), 0 FP**; fx 118; → #185–#189/#193. [note](notes/20260926-issue-141-class-eval-defs.md).
- **2026-09-25 #129 CLOSED** (PR #150): `conforms-to` fires only if provable; 4 review rounds found 39 FP families; **0 FP / 9,337**; → #155–#163. [ADR-0044](adr/0044-conforms-to-directive.md), [note](notes/20260925-conforms-to-audit.md).
- **2026-09-25 re-pin `v0.3.9 → e59b7b89`** (1,030 commits): 0 FP / 9,337. Raw 8+9 FPs (#1021 reach analysis; `fb781023` unported, #141); 10 invisible retractions → #133–#138. [note](notes/20260925-repin-e59b7b89.md)
- **2026-09-21 #128 CLOSED — store values named by their TYPED answer** (PR #131) — member is the erased class of the value's `stmt_value_type`; union-carrier stores widen per-arm. **0 FP / 9,337**; #132. [note](notes/20260921-issue-128-store-value-typing.md).
- **2026-09-21 upstream re-pin `v0.3.8 → v0.3.9`** (447 commits): **0 FP / 9,337, gaps 799→892**, 108 fixtures, 0 OVER. Raw bump: 4 FPs in three families (`declines_bot?` shaped carriers, rooted `::RUBY_VERSION`, `MutationRejoin`). Residues: #128–#130. [note](notes/20260921-repin-v039.md).
- **2026-09-09 #123 CLOSED — 26 holes = THREE defects** — `overlay/` loads LAST, chain LAZY: **22** `module ::Kernel` keyed `Gem::Kernel`; **5** `prepend` uningested; **6** SELF-TYPE unrecorded. **26→0**, 0 FP / 9204 — DORMANT until `check_call`'s declaration-only conjunct drops. [note](notes/20260909-qualified-ancestor-closure-holes.md).
- **2026-09-09 the 799 gaps ADJUDICATED** — partitioned by MECHANISM: **395 sit behind decisions**; the 71-row bucket rests on ONE over-narrow rbs signature. `check` exited 0 on unparseable files (PR #125); ADR-0033's leniency premise expired → #123/#124. [799](notes/20260909-gap-adjudication-799.md) / [gems](notes/20260909-declared-unwitnessed-gem-classes.md).
- **2026-09-09 #118 CLOSED** (PR #120) — generic dispatch declines to `Dynamic[top]` when the join is at risk AND an argument is reference-untyped: **13 FPs closed, 0 matched lost**. Blanket "nilable declines" cost TEN folded rows — untypedness is what stops folding. [note](notes/20260909-generic-dispatch-untyped-arg.md).
- **2026-09-09 upstream re-pin `v0.3.4 → v0.3.8`** (924 commits / 4 releases) — **0 FP / 9204, gaps 820→799**, harness 98→**105** fixtures. Raw bump = 7 fixture retractions + 12 sweep FPs = **SIX families**, each bisected+ported. Residues: 20 resolvable-`super` rows. [note](notes/20260909-repin-v038.md) / [spec](notes/20260909-repin-v038-port-spec.md) / [feedback 4](notes/20260909-upstream-feedback-batch4.md).
- **2026-08-26/28 the effects GATE was lying, three times** (PRs #112/#115/#117) — deleted-arm byte-identity, `methods:{}` covering 2 of 4 producers, `omit?` inverting ADR-0043 §2. [s112](notes/20260826-s112-effects-instrument.md) / [s5](notes/20260826-effects-s5-probe.md) / [s116](notes/20260826-s116-snapshot-gate.md).
- **2026-08-26 LSP honours `rootUri` / `workspaceFolders`** (PR #110) — enters the client's root rather than threading one (the root IS a cwd in all consumers; threading missed `sig/` + regressed `exclude:`). [note](notes/20260826-s111-lsp-rooturi.md).
- **2026-08-25/26 two arcs CLOSED, folded** — effects slices 0–3 (35 MATCH / 11 UNDER / 0 OVER — [s3](notes/20260826-effects-s3-impl.md)); frozen-index arc (file order NORMATIVE, eviction BLOCKED, harvest NO-GO — [s113](notes/20260826-s113-fold-capture-impl.md)).
- **2026-08-23/25 re-pin `v0.3.2 → v0.3.4` + survey (HOLD)** — 0 FP / 9204, gaps 841→820; raw bump opened **50 FPs**, all upstream RETRACTIONS invisible to the snapshot diff ([note](notes/20260823-repin-v034.md)); vendored plugin RBS drift = 10 FPs (fixture 98 covers it), every `documentation_url` 404s ([survey](notes/20260825-upstream-survey-v034-master.md)).
- **2026-08-09 unresolved-const-receiver carrier REJECTED at 0 rows** (PR #89, closed). [note](notes/20260809-unresolved-const-receiver-carrier.md)
- **2026-08-09 era (3 slices, folded)** — re-pin `v0.3.1 → v0.3.2` (+rbs 4.1.1): 0 FP / 9204, gaps 1125→841; `sig/shims/` trap closed by `overlay/rbs_shims/` ([note](notes/20260809-repin-v032.md)); join-wipe retention ([note](notes/20260809-join-wipe-retention.md)); chain-guard meet ([note](notes/20260809-chain-guard-meet.md)).
- **2026-08-08 era, folded (0 FP / 9204)** — narrowing/shape trio (PRs #70/#75/#78/#80–#82, **26 rows closed** — [meet](notes/20260808-sequential-guard-meet.md) / [witnessing](notes/20260808-qualified-witnessing-mini-spec.md) / [shape](notes/20260807-collection-shape-slice-spec.md)); `Object` bucket adjudicated (PR #85 — [adjudication](notes/20260808-object-bucket-adjudication.md)); constant-value harvest + partial containers (PRs #83/#84 — [spec](notes/20260808-partial-constant-harvest-mini-spec.md)).
- **2026-08-07/08 the class-narrowing ARC, CLOSED at a measured stop** (PRs #63–#79) — `narrow_class_other` end-to-end: **19 gap closures + 11 master FP shapes**, 0 FP / 9204. Lesson: FP-safety argument WRONG 3× (position axis; carrier allow-list; disjoint→`Bot`). [spec](notes/20260807-class-narrowing-slice-spec.md) / [stage3](notes/20260807-narrowing-stage3-spec.md).
- **2026-08-01/08 instruments + adjudication, folded** — the 0-FP gate could pass VACUOUSLY (PR #65 — [note](notes/20260807-fp-audit-port-side-blind-spots.md)); the coverage-gap CENSUS buckets gaps by MECHANISM ([note](notes/20260807-gap-census.md)); `arity_eligible?` was never ported = a `call.wrong-arity` FP (fixture 80); LSP config reload keeps LAST GOOD ([lsp](notes/20260801-lsp-config-reload.md)).
- **2026-08-07 ADR-0042 S5: qualified return-lookup routing** (PR #64) — the 8-member return family routes namespaced receivers via the qualified registry (refs AS WRITTEN + lexical ctx; ambiguity DECLINES); **14 closures (→1179), 0 FP / 9204**; fixture 82 pins the Tier-3 instance boundary (gaps 3→4 on merge). [spec+outcome](notes/20260807-adr0042-s5-return-lookup-spec.md).
- **2026-08-07 upstream survey + feedback batch 2, folded** — the `v0.3.1`→`80aaf9bc` self-diff moved 2 diagnostics on 9204 (superseded by the `v0.3.2` re-pin) and retired GEM_HOME rbs selection ([note](notes/20260807-upstream-survey-v031-to-master.md)); batch 2 filed 3 reference-side defects with repros ([note](notes/20260807-upstream-feedback-batch2.md)).
- **2026-07-31 era (7 slices, folded)** — sig-gen `Data.define`/`Struct.new` members; `BigMath` blinded-oracle asymmetry CLOSED; survey FP triage 24→0 ([note](notes/20260731-survey-fp-triage-24.md)); project-`sig/` blind-spot → fixture 79; `-> self` on instance methods; standing sweep set CODIFIED ([CORPUS.md](../harness/CORPUS.md)).
- **2026-07-31 upstream pin `v0.3.0 → v0.3.1` + vendored rbs `4.0.3 → 4.1.0`** — **0 FP / 9153 files, gaps net −2**. 4.1.0's rewritten signatures broke two things, both FIXED: bounded type params resolve to their bound; `-> instance` resolves via the `SELF_RETURN` call-site sentinel. [note](notes/20260731-upstream-pin-v031-rbs41.md) / [survey](notes/20260731-v031-preflight-survey.md).
- **2026-07-25 era (3 slices, folded)** — MultiWrite substrate s1+s2 (PRs #46/#47: arena lowering + `MultiTargetBinder`, RBS tuple returns — [spec](notes/20260725-multiwrite-substrate-spec.md)); LSP `exclude:` parity (PR #45, matrix 24→144); LSP stage-3 tail (PR #44).
- **2026-07-18/19 era (5 arcs + the RC bump, folded)** — LSP §12 two-tier S1–S4b (PRs #35–#38/#42/#43); `coverage` precision mode + MCP + node-granularity audit (PRs #33/#40); ADR-0042 gate + Slices 1–4 ([deliverables](notes/20260719-adr0042-gate-deliverables.md)); compat arc + severity + `--bleeding-edge` ([findings](notes/20260718-phase0-m1-m2-findings.md)); RC bump `47ec8625→7a69f142` ([note](notes/20260718-upstream-rc-bump-47ec8625-7a69f142.md)).
- **2026-07-17 era (folded)**: docs economy (baton + [PORT_BACKLOG.md](PORT_BACKLOG.md), `docs_check.py`, #21); Tier B/C CLOSED ([note](notes/20260717-tier-bc-track-closed.md)); ATM arc + constant-shadow gate + C3a String-tail, gitlab UM 356→179 ([spec](notes/20260717-atm-substrate-arc-plan.md)).
- **2026-07-16 two MERGED inference slices** — `def.ivar-write-mismatch` (gitlab ivar gaps 2→0) + literal-tail return folding (gitlab always-truthy 28→16). 0 FP both. [ivar](notes/20260716-ivar-write-mismatch-spec.md) / [fold](notes/20260716-literal-tail-fold-spec.md).
- **2026-07-16 v0.3.0-RC arc: pin `47ec8625` + 7 slices, ALL MERGED** — syntactic rules, MutationWidening (killed 2 measured FPs), implicit-self dispatch + `p`/`pp`, scalar HashShape keys, Kernel `format`/casts, `raise-non-exception`, `shadowed-rescue-clause` + rbs.rs nesting root-fix. **v0.3.0 rule surface fully ported.** [specs](notes/20260716-v030-upstream-gap-survey.md).
- **2026-07-11 sig-gen arc CLOSED (13 slices) + periphery (4 items)** — `erase_to_rbs` → `--print` → union → singletons → `--write` → `--diff` → Writer merge → `--overwrite` → Data/Struct; 0 shared-method mismatch, `--write` sound. Periphery: MCP `sig_gen`; `--params=observed` SUBSTRATE-BLOCKED ([note](notes/20260711-siggen-params-observed-substrate-blocked.md)); coverage frontier bounded wins exhausted ([note](notes/20260711-coverage-frontier-remeasured.md)).
