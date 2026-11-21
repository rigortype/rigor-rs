# rigor-rs — Current Work

The session-to-session baton: **what is in flight, what to pull next, and a
one-line ledger of what landed**. Port map: [PORT_BACKLOG.md](PORT_BACKLOG.md);
outcomes: `docs/notes/` + `docs/adr/`; history: `git log`.

**Contract (gated by `harness/docs_check.py`):** a landed/closed arc gets ONE
ledger line here — verdict + numbers + link — and its detail goes to a dated
note or ADR *first*. No status essays — hard byte budget.


## Now / Next

▶ NEXT: the ADR-127 floor's coverage cost; follow-ups #491/#493, #600, #605;
the #771 def-body half #781; mutation-message seams →#777–#785.
- **CLOSED arcs** (do not re-open): ADR-0042 migration + compat
  ([plan](notes/20260718-compat-next-stage-plan.md)).
- **CLI surface (v0.3.0 RC)** — `--bleeding-edge`/severity/`coverage` done;
  `--protection`/`--mutation`/`type-scan` deferred
  ([scoping](notes/20260719-coverage-command-scoping.md)).
- **Pin `v0.4.8`** (rbs 4.2.0). Both standing exception tables EMPTY —
  new entries are real findings (`UPSTREAM.md` hazards +
  overlay/`sig/shims` trap).

State (2026-11-21, `v0.4.8`): harness **174 fixtures / 0
unregistered**; standing sweep **0 FP / 437 = master**; effects gate 0
OVER.
Grading tools REFUSE stale builds. Clippy: `-D warnings`, FRESH `CARGO_TARGET_DIR`.

## Standing conclusions (do not re-litigate without new evidence)
- **Possible-nil / Tier B/C CLOSED, not deferred** — the closing slice deletes
  the nameable-concrete-arm FP-safety mechanism; `fp_audit` scores it 0 FP
  (the parity gate points the wrong way,
  [tier-bc](notes/20260717-tier-bc-track-closed.md)). **SCOPE: the 85
  `Dynamic`-arm rows, NOT the 93 concrete-arm ones**.
- **Reference-FP undefined-method clusters, CLOSED** — `pre_eval:` cross-file
  monkey-patch (closing INVERTS the ADR-0033 provenance gate), receiver-typed-`nil`,
  rdoc generated-parser `Hash` ([141](notes/20260807-gap-adjudication-141.md)).
  Re-adjudicated `v0.3.8` ([799](notes/20260909-gap-adjudication-799.md)):
  **395 of 799 behind decisions; biggest: nested-scope receiver typing, 71 rows.**
- **5 consecutive FP-safe flow slices closed 0 survey gaps** — no coverage
  slice without a valid-mode `fp_audit --gaps` prediction
  ([flow-frontier](notes/20260706-flow-frontier-exhausted.md)).
- **The receiver-typing lever is open**: gap census buckets by MECHANISM
  ([census](notes/20260807-gap-census.md)); four slices closed **47
  rows**. Re-run `gap_census.py --sweep` after each.
- **Effects TRANSITIVE LABEL lane DECLINED — not portable at parity** (s4): four typer-free rules measured, best 5 OVER — labels stay UNDER; parity outranks the ~2,000-method prize. [probe](notes/20260826-effects-s4-probe.md).
- **sig-gen closed** — byte surface 0, `--write` sound.
- **Plugin work:** pure-RBS bundle track closed ([note](notes/20260710-pure-rbs-bundle-track-closed.md));
  sidecar + perf retired ([ADR-0037](adr/0037-sidecar-perf-slices-retired-by-measurement.md)).

## Build & gates

The gate tiers and their commands: `AGENTS.md` → Gates.

Oracle: pinned `reference/rigor` (`UPSTREAM.md`; fresh temp cwd); sweep set
`harness/sweep-corpora.yml`; RBS vendored (ADR-0007; `RIGOR_RBS_CORE_DIR`
seam, `harness/vendor_rbs.py` regenerates).

## Ledger (newest first; one line per arc/slice)

- **2026-11-21 #492 CLOSED** (PR #789): override-replaced parents decline (#2013 `contest_replaced`) + heredoc closure env (`CheckFlow.escaped`/`Barrier`); 13/13 `=`; 0 FP/437=master; →#793 (diff-caused)/#794–#798. [note](notes/20261121-issue-492-override-heredoc-env.md)
- **2026-11-21 #328 CLOSED** (PR #790): per-file diagnostics in the reference's stage order (span key + emission-array slot, `emission_stage` at the callers); all brief rows `=`; 0 FP/437=master; found #791/#792. [note](notes/20261121-issue-328-diagnostic-stage-order.md)
- **2026-11-21 #629 CLOSED** (PR #786): effects `catalog_target` settles the typer arm's syntax subset; rows claim where upstream claims, judgment runs rowless; effects MATCH 5693→5812, 0 OVER; →#787/#788. [note](notes/20261121-issue-629-index-write-ownership.md)
- **2026-11-21 #771 file-scope half CLOSED** (PR #776): content args on widened carriers; 23 rows byte-identical; 0 FP/437=master; →#777–#785 (def-body #781). [note](notes/20261121-issue-771-mutation-content-args.md)
- **2026-11-21 #424 CLOSED** (PR #764): nested-class time — 4 rescans → containment index/trie; 4k 100s→3.7s; 0 FP, gaps=master; →#772. [note](notes/20261121-issue-424-nested-class-time.md)
- **2026-11-21 #685 CLOSED** (PR #761): `Algebra::union`/`flatten_union_members` member-set builders (`top`/`bot` verbatim); 4 raw mints routed. 0 FP. [note](notes/20261121-issue-685-union-canonicalization.md)
- **2026-11-21 #639 CLOSED** (PR #756): loop-cap jump/once bodies + rebindable ivars; 8 master FPs closed; 0 FP; →#757–#759. [note](notes/20261121-issue-639-loop-cap-jump-ivar.md)
- **2026-11-21 #439 CLOSED** (PR #753): reach-memo audit in CI; 0 FP. →#754. [note](notes/20261121-issue-439-reach-memo-audit-gate.md)
- **2026-11-21 #683 CLOSED** (PR #751): loop-cap self-write `compound` re-derived per operand (x-polarity gate, dominating-write cut); 0 FP; →#684/#752. [note](notes/20261121-issue-683-loop-cap-self-write.md)
- **2026-11-21 #667 CLOSED** (PR #742): `singleton_truncated` counts the self-extend's instance-side entry + RBS fallback; live FP gone; 0 FP; →#748–#750. [note](notes/20261121-issue-667-singleton-self-extend-budget.md)
- **2026-11-20 #446 CLOSED** (PR #741): refinement decline — refine-body census + `own_method_answers` walk; 0 FP; →#743–#747. [note](notes/20261121-issue446-refinement-decline-spec.md)
- **2026-11-20 #428 CLOSED** (PR #706): `harness/stack/` = #412 tools on the post-#431 ladder + stack.yml; ADR-0045 x86_64 rungs (alloc, never 2 MiB overflow); Windows →#707. [note](notes/20261120-issue-428-stack-tools.md)
- **2026-11-20 #724 CLOSED** (PR #739): pool-audit `use` alias/glob of the pool builders in `fn main` flags (#676 hole); test-only, 0 FP. [note](notes/20261120-issue-724-main-pool-aliased-use.md)
- **2026-11-20 #696 CLOSED** (PR #730): ShapeDispatch String carriers (`decimal-int-string`/`non-empty-string`); drift 47→46, 0 FP. →#733–#738, #600. [note](notes/20261120-issue-696-decimal-int-string.md)
- **2026-11-20 #427 CLOSED** (PR #726): env memo stores uid-scoped interner-bound answers; `.map` repro doubled per link → never finishes; check 68 s; byte-identical, 0 FP. →#729/#731/#732. [note](notes/20261120-issue-427-map-chain-memo.md)
- **2026-11-20 #643 CLOSED** (PR #725): named Finding struct + conformance start_offset; refactor, 0 FP / 437 gaps = master; →#727/#728. [note](notes/20261120-issue-643-findings-tuple.md)
- **2026-11-20 #676 CLOSED** (PR #723): pool-ordering audit — planted inversion: threads test passes, audit fails at the line; →#724. [note](notes/20261120-issue-676-pool-ordering-audit.md)
- **2026-11-20 #345 CLOSED** (PR #720): pre_eval glob→dir earns not-found row; `.uniq`/order ported; 0 FP; →#721/#722. [note](notes/20261120-issue-345-preeval-glob-dir.md)
- **2026-11-20 #652 CLOSED** (PR #719): brace globs `::{*}`/`::{a,*}` flag like `::*`; child globs leaf-silent; test-only, 0 FP. →#654. [note](notes/20261120-issue-652-brace-glob-audit.md)
- **2026-11-20 #714 CLOSED** (PR #716): rooted bundled-class guard: RBS mint outranks the census; dependabot 215→212, 0 FP. →#709/#717/#718. [note](notes/20261120-issue-714-rooted-bundled-guard.md)
- **2026-11-20 #347 CLOSED** (PR #713): `.rigor.yml` safe_load crash mirror; 0 FP every head, unmodelled arms decline. →#715. [note](notes/20261120-issue-347-yaml-psych-rejects.md)
- **2026-11-20 #672 CLOSED** (PR #708): rooted guard operands top-level; 0 FP, +3 dependabot rows. →#709/#710/#714. [note](notes/20261120-issue-672-rooted-class-guard.md)
- **2026-11-20 #673 CLOSED** (PR #711): dynamic-base rescue designator (`k::X`) opaque — `dynamic_base` gates all five consumers. 0 FP. →#712. [note](notes/20261120-issue-673-rescue-dynamic-base.md)
- **2026-11-20 #677 CLOSED** (PR #703): fallback warning: one line per new worst stack; `RLIMIT_STACK` rank, refusal quoted. →#676/#705. [note](notes/20261120-issue-677-stack-fallback-warn.md)
- **2026-11-20 #441 CLOSED** (PR #698): `local_reach` absorption; scanner.rb 19→0.6s, onevar_48 46m→0.3s. →#699/#700/#704. [note](notes/20261120-issue-441-absorption.md)
- **2026-11-20 #678 CLOSED** (PR #702): pool-build failure exits 70; `help`/`version`/`upgrade` skip+poison. →#676/#677. [note](notes/20261120-issue-678-pool-exit-code.md)
- **2026-11-20 #424 seq-modif** (PR #697): kill-scan folds + env checkpoints + delta joins; 8k 35.8→2.1s (oracle 5.5). →#701. [note](notes/20261120-issue-424-seq-modif-rows.md)
- **2026-11-19 #682 CLOSED** (PR #693): integer-fold overflow promotes to `Scalar::BigInt`; decimal `**`/shifts + `fold_bigint`. 0 FP; →#694–#696. [note](notes/20261119-issue-682-folding-bigint.md)
- **2026-11-19 #437 MERGED** (PR #687): contextual fold memo + `loop_pass` if-arms; snapshot 952→959; 0 FP. →#688-91. [note](notes/20261119-issue-437-fold-memo.md)
- **2026-11-19 #653 CLOSED** (PR #689): `#[cfg(test)]` span — `pub` looks past its visibility group; `item_end` stops at unopened closers. 0 FP. →#692. [note](notes/20261119-issue-653-pub-field-cfg-span.md)
- **2026-11-19 #679 CLOSED** (PR #686): last stack fallback explicit `stack_size(2 << 20)`; ADR-0045 amended. [note](notes/20261119-issue-679-explicit-2mib-fallback.md)
- **2026-11-19 #427 walkers** (PR #680): stackless `Interner::cmp` + iterative `literal_seed`/sig-gen predicates; 0 FP. →#685. [note](notes/20261119-issue-427-iterative-walkers.md)
- **2026-11-19 #638 MERGED** (PR #674): exact-gated loop fixpoint replay; structural fallback on declines. 0 FP. →#681-84. [note](notes/20261119-issue-638-loop-fixpoint.md)
- **2026-11-19 #431 CLOSED** (PR #675): command stack reserved before the rayon pool. ADR-0045 amended; →#676–#679. [note](notes/20261119-issue-431-stack-fallback-order.md)
- **2026-11-19 #424 modifier-if** (PR #669): subsequent-first exit + compact_children; 10k `if true` 2.16s→0.52s; 0 FP. [note](notes/20261119-issue-424-modifier-if.md)
- **2026-11-19 #670 CLOSED** (PR #671): class-guard `dynamic_base` — 13 master FPs gone, 0 FP. →#672/#673. [note](notes/20261119-issue-670-dynamic-base-class-guard.md)
- **2026-11-19 #655+#661 CLOSED** (PR #668): dynamic-base `ConstantRead` skips C5/C1; 0 FP. [note](notes/20261119-issue-655-dynamic-base-const.md)
- **2026-11-19 #637 CLOSED** (PR #664): CHAIN_LIMIT counts the root; 100-module include silent; 0 FP. [note](notes/20261119-issue-637-chain-limit.md)
- **2026-11-19 #657 CLOSED** (PR #660): ConstName prefix-share + bounded `knows_*`; 20k 0.74s/749MB→0.26s/44MB; 0 FP. [note](notes/20261119-issue-657-const-read-name-storage.md)
- **2026-11-19 #658 CLOSED** (PR #659): PathMarks + CoreData + env-fp type_of; 2.5k push 49.5s→1.7s; 0 FP. [note](notes/20261119-issue-658-mutator-chains.md)
- **2026-11-19 #429 CLOSED** (PR #601): `analysis_stack_audit` — unbounded parses + unsized threads. →#652–#654. [note](notes/20261118-issue-429-analysis-stack-audit.md)
- **2026-11-19 #424 const** (PR #651): `lower_constant_path`; 20k→0.72s; 0 FP. → #655–#658. [note](notes/20261119-issue-424-const-path.md)
- **2026-11-19 #427 display stack** (PR #648): `render_postorder`; wrap90 SIGABRT→exit 1; 0 FP. [note](notes/20261119-issue-427-iterative-type-display.md)
- **2026-11-19 #424 `**` row** (PR #649): env-free call memos — 800 `**` 38s→0.5s; 20k under bar; 0 FP. [note](notes/20261119-issue-424-pow-chain.md)
- **2026-11-19 #642 CLOSED** (PR #644): shared `LineIndex` on `LoweredAst`; LSP 2.89s→0.36s, MCP 4.26s→0.54s; 0 FP. [note](notes/20261119-issue-642-shared-line-index.md)
- **2026-11-19 #435 CLOSED** (PR #634): loop-cap widen (ADR-56 slice B); m1/m2/o8 match; 0 FP. → #638–#640. [note](notes/20261119-issue-435-loop-cap-widen.md)
- **2026-11-19 #453 CLOSED** (PR #645): one-pass Logical `subject_edges` — 34 terms 14s→0.10s; 0 FP. → #646/#647. [note](notes/20261119-issue-453-logical-one-pass.md)
- **2026-11-19 #636 CLOSED** (PR #641): findings `(line, column)` at production; 60k-diag 169s/49GB → 1.8s/141MB; 0 FP. → #642/#643. [note](notes/20261119-issue-636-findings-line-index.md)
- **2026-11-19 #464 CLOSED** (PR #635): `object_mixin_may_define` collects the mixin closure once — 4.7s → 0.66s; 0 FP. [note](notes/20261119-issue-464-object-mixin-memo.md)
- **2026-11-19 #346 CLOSED** (PR #630): config `~` via `Dir.home` NSS; `~name` literal. → #631–#633. [note](notes/20261119-issue-346-tilde-dir-home.md)
- **2026-11-19 #614 CLOSED** (PR #624): `LocalOwnership` ports `constructor?`/`class_call?`/`singleton`. →#629. [note](notes/20261119-issue-614-local-ownership-allocation.md)
- **2026-11-19 #603 CLOSED** (PR #623): alias floor `selection_branches` + `ReceiverAlias.candidates` + carrier marks; 4 FP. → #625–#628. [note](notes/20261119-issue-603-alias-floor-links.md)
- **2026-11-19 #615 CLOSED** (PR #620): `walk_namespace` ports `declaration_prefix`/`rooted?`; 0 OVER. →#621/#622. [note](notes/20261119-issue-615-rooted-class-header.md)
- **2026-11-18 #604 UNNAMED mixin marker** (PR #612): `"?"` rides `"*"`; computed `include`/`send` opens the class. 0 OVER. →#617/#619. [note](notes/20261118-issue-604-unnamed-mixin-marker.md)
- **2026-11-18 #448 frame-local specials + DefinitionContext** (PR #613): `UNCOLOURED_READS`; `{$~,$_}` writes on `!shared_slot`; `BodyCtx` ports `definition_context.rb`. 0 OVER. → #614/#616. [note](notes/20261118-issue-448-frame-local-specials.md)
- **2026-11-18 #376 bundled-name tables tied to the pin** (PR #609): `BUNDLED_SKILL_NAMES` desynced (oracle 18, port 17); test re-derives both. 0 FP; →#610/#611. [note](notes/20261118-issue-376-bundled-name-tie.md)
- **2026-11-18 #490 singleton-slot writers + severity key naming** (PR #602): `class <<`/eval-block writer + composition contexts ported; `psych_key_to_s` mirrors `ToRuby#deserialize`. → #606–#608. [note](notes/20261118-issue-490-singleton-slot-writers.md)
- **2026-11-18 re-pin `v0.4.7 → v0.4.8`** (PR #599, human-merged) — mixin-call recording + #2677 alias-floor copy-link; 13 rows closed; **0 FP; 959/1063**; →#600/#603–#605. [note](notes/20261115-repin-v048.md)
- **2026-11-12 #405 class-guard record drop** (PR #591): class guards drop rooted `h[k]` records. 18 FP closed, 0 FP/658; →#595/#596. [note](notes/20261112-issue-405-class-guard-records.md)
- **2026-11-12 #449 match-global early-exit guard** (PR #589): `=~`/`!~` record `$~` + `$N`. 0 FP/658; → #597/#598. [note](notes/20261112-issue-449-match-globals.md)
- **2026-11-12 #438 `class_ivar_scope` memo key** (PR #588): reads in one class body share the `IvarScope`; ~pre-#432 RSS; byte-identical. [note](notes/20261112-issue-438-ivar-scope-memo.md)
- **2026-11-12 #393 retry-edge indexed narrowings** (PR #590): retry-rebound/mutated locals drop rooted `h[k]` records. ~22 FP rows closed, 0 FP/658; → #592/#593. [note](notes/20261112-issue-393-retry-index-drop.md)
- **2026-11-12 #450 project-sig singleton returns** (PR #574): buildable sig beats body fold; lexical receiver + nil bit; 0 FP/658; → #584–#587. [note](notes/20261112-issue-450-sig-singleton-return.md)
- **2026-11-12 #398 Kernel termination** (PR #575): dead code uses `kernel_call_terminates`, not block-timing veto. 13+3 FP closed, 0 FP/658; →#581–#583. [note](notes/20261112-issue-398-kernel-termination.md)
- **2026-11-12 #349 Psych YAML errors** (PR #576): libyaml `context_mark`/problem/context render Psych positions/details; BOM parity. 0 FP; → #579/#580. [note](notes/20261112-issue-349-yaml-fidelity.md)
- **2026-11-12 #384 ATM describe labels** (PR #573): nested union/optional/alias members flatten before `union_describe`; `Other` strips root `::`. 12 rows byte-identical, 0 FP; → #577/#578. [note](notes/20261112-issue-384-render-other-drift.md)
- **2026-08-10 `if`-edge joins** (#451 PR #571, #443 PR #572): per-edge narrowing (`Empty` join, live-arm cut, `&.` on nil); post-`if` write joins `entry ∪ arm-write`, not blanket `Dynamic`. [edge](notes/20260810-if-edge-narrowing.md)/[join](notes/20260810-if-write-join.md)
- **2026-11-06 `run_corpus.rb` REMOVED** (PR #495): a first-80-files `--sweep` sample; its unique input measures identically. [CORPUS.md](../harness/CORPUS.md)
- **2026-11-05 walk follow-ups** (#483–#488): port-only rows fixed, `pre_eval:` declines; 0 FP / 623. [note](notes/20261105-walk-followups.md)
- **2026-11-05 re-pin walk → `v0.4.7` DONE** (#445…#481): 0 FP / 623 gaps. Last tag: callee floor, `core_root:`, CLI. [notes](notes/20261105-repin-v047.md)
- **2026-10-30 #436 CLOSED** (PR #440): reaching-write fan-out cap 256 → `UNKNOWN`; lexer-F0/F1 >1,600 s → 0.2 s. 0 FP; → #441–#443. [note](notes/20261030-fanout-cap-436.md)
- **2026-10-30 #422 CLOSED** (stack #434 = PRs #432/#433): `ReachIndex` hoist + exact `local_reach` memo; slow files <7 s; byte-identical. 0 FP / 9,435; → #435–#439. [note](notes/20261030-reach-memo-422.md)
- **2026-10-28 #412 CLOSED** (PR #426): 256 MiB stacks + a 25,000-edge nesting bound. 0 FP; → #427–#431. [ADR-0045](adr/0045-stack-totality.md)
- **2026-10-27 splits** #204/#234/#258/#260/#279/#416/#418/#420: infer lib 14,084→144, rules 4,497→95; ast 3,756→80; source_index 4,650→668; lower_node→5 files. [note](notes/20261027-lower-node-family-split.md)
- **2026-10-08/14 dead-position + reach arc, folded** (#362–#388, #385–#404): `dead.rs` exclusion + reach leaks, arg-class gate, `blocked`/`discarded` split. 0 FP. [dead](notes/20261008-i368-dead-positions.md) / [leaks](notes/20261014-i386-dead-position-leaks.md)
- **2026-09-30/10-01 CLI + config parity, folded** (#155/#157/#158/#360, PRs #335–#375): Psych config, OptionParser everywhere, deferred-subcommand grammar; →#345–#349/#376. [config](notes/20260930-issue-158-config.md) / [optparse](notes/20260930-issue-155-optparse.md)
- **2026-09-30/10-01 IndexedFlow arc, folded** (#304–#366, #333–#378): stored-slot narrowing + the writes that drop it. 0 FP. [slots](notes/20260930-issue-325-slot-narrowing.md) / [blocks](notes/20261001-i366-block-attr-drop.md)
- **2026-09-28/30 index-write + scope arc, folded** (#134–#332, #284–#334): index/splice widening, operand replay, closure shadowing, multi-valued args. 0 FP / 818. [index](notes/20260929-issue-134-index-widening.md)/[operand](notes/20260929-issue-136-operand-scope.md)
- **2026-09-28/30 output, config + RBS parity, folded** (#162–#201, #281–#323): baseline, output bytes, project-`sig/` `use`, witness/list keys, `exclude:`. 0 FP. [formats](notes/20260928-issue-163-output-formats.md)/[rbs-use](notes/20260929-issue-168-rbs-use.md)
- **2026-09-27 #138/#164/#167 ABANDONED** (PRs #184/#177/#183 closed unmerged): full-parity review anti-pattern. [note](notes/20260927-abandoned-infer-pr-stream.md).
- **2026-09-25/28 fold + binder slices, folded** (#121–#166, #148–#283): lookup folds, rebind/binder/splat shapes. 0 FP / 9,337. [rebind](notes/20260925-issue-133-jump-path-rebind.md)
- **2026-09-26 #141 CLOSED** (PR #179): eval-block defs attribute to the receiver. 3,002→827 gaps, 0 FP; →#185–#189/#193. [note](notes/20260926-issue-141-class-eval-defs.md).
- **2026-09-25 #129 CLOSED** (PR #150): `conforms-to` fires only if provable; 39 FP families in review; **0 FP**; → #155–#163. [ADR-0044](adr/0044-conforms-to-directive.md), [note](notes/20260925-conforms-to-audit.md).
- **2026-09-25 re-pin `v0.3.9 → e59b7b89`** (1,030 commits): 0 FP / 9,337. Raw 8+9 FPs (#1021; `fb781023` unported, #141); 10 invisible retractions →#133–#138. [note](notes/20260925-repin-e59b7b89.md)
- **2026-09-21 #128 CLOSED — store values named by their TYPED answer** (PR #131). **0 FP / 9,337**; →#132. [note](notes/20260921-issue-128-store-value-typing.md).
- **2026-09-21 upstream re-pin `v0.3.8 → v0.3.9`** (447 commits): **0 FP / 9,337, gaps 799→892**, 108 fixtures, 0 OVER. Raw: 4 FPs, 3 families. →#128-30. [note](notes/20260921-repin-v039.md).
- **2026-09-09 #123 CLOSED — 26 holes = THREE defects** — `overlay/` LAST, chain LAZY: `::Kernel`→`Gem::Kernel` ×22, `prepend` ×5, SELF-TYPE ×6. 26→0, 0 FP / 9204. [note](notes/20260909-qualified-ancestor-closure-holes.md)
- **2026-09-09 the 799 gaps ADJUDICATED** — by MECHANISM: **395 sit behind decisions**; the 71-row bucket rests on ONE over-narrow rbs sig. `check` exit 0 on unparseable files (PR #125); ADR-0033's premise expired →#123/24. [799](notes/20260909-gap-adjudication-799.md).
- **2026-09-09 #118 CLOSED** (PR #120) — generic dispatch declines to `Dynamic[top]` on join risk + a reference-untyped arg: **13 FPs closed, 0 matched lost**. [note](notes/20260909-generic-dispatch-untyped-arg.md).
- **2026-09-09 re-pin `v0.3.4 → v0.3.8`** — **0 FP / 9204, gaps 820→799**, 105 fixtures. Raw: 7 retractions + 12 sweep FPs, six families ported. Residues: 20 resolvable-`super` rows. [note](notes/20260909-repin-v038.md) / [spec](notes/20260909-repin-v038-port-spec.md).
- **2026-08-26/28 the effects GATE was lying, three times** (PRs #112/#115/#117) — deleted-arm, `methods:{}`, `omit?`/ADR-0043 §2. [s112](notes/20260826-s112-effects-instrument.md)/[s5](notes/20260826-effects-s5-probe.md)/[s116](notes/20260826-s116-snapshot-gate.md).
- **2026-08-26 LSP honours `rootUri` / `workspaceFolders`** (PR #110) — enters the client's root rather than threading one. [note](notes/20260826-s111-lsp-rooturi.md).
- **2026-08-25/26 effects 0–3 + frozen-index folded** — 35 MATCH/11 UNDER/0 OVER ([s3](notes/20260826-effects-s3-impl.md)); file order NORMATIVE, eviction BLOCKED, harvest NO-GO ([s113](notes/20260826-s113-fold-capture-impl.md)).
- **2026-08-23/25 re-pin `v0.3.2→v0.3.4` + survey (HOLD)** — 0 FP / 9204, gaps 841→820; 50 raw-bump FPs, retractions the diff can't see ([note](notes/20260823-repin-v034.md)); plugin drift + 404s ([survey](notes/20260825-upstream-survey-v034-master.md)).
- **2026-08-09 unresolved-const-receiver carrier REJECTED at 0 rows** (PR #89, closed). [note](notes/20260809-unresolved-const-receiver-carrier.md)
- **2026-08-09 re-pin `v0.3.1→v0.3.2` (+rbs 4.1.1), folded** — 0 FP / 9204, gaps 1125→841; shims→overlay; join-wipe; chain-guard meet. [repin](notes/20260809-repin-v032.md)/[join](notes/20260809-join-wipe-retention.md)/[meet](notes/20260809-chain-guard-meet.md)
- **2026-08-08 era, folded (0 FP / 9204)** — narrowing/shape trio #70–#82; `Object` bucket #85; harvest #83/#84.
- **2026-08-07/08 class-narrowing arc CLOSED, measured stop** (#63–#79): `narrow_class_other`: 19 closures + 11 master FPs, 0 FP / 9204. [spec](notes/20260807-class-narrowing-slice-spec.md)/[stage3](notes/20260807-narrowing-stage3-spec.md).
- **2026-08-01/08 instruments + adjudication, folded** — 0-FP gate could pass VACUOUSLY ([note](notes/20260807-fp-audit-port-side-blind-spots.md)); gap CENSUS by MECHANISM ([note](notes/20260807-gap-census.md)); LSP keeps LAST GOOD ([lsp](notes/20260801-lsp-config-reload.md)).
- **2026-08-07 ADR-0042 S5: qualified return-lookup** (PR #64) — namespaced receivers route via the qualified registry; 14 closures (→1179), 0 FP / 9204; fx 82 pins Tier-3. [spec](notes/20260807-adr0042-s5-return-lookup-spec.md).
- **2026-08-07 upstream survey + feedback batch 2, folded** — self-diff moved 2 superseded diagnostics; retired GEM_HOME rbs selection ([note](notes/20260807-upstream-survey-v031-to-master.md)).
- **2026-07-31 era, folded** — sig-gen `Data.define`/`Struct.new`; `BigMath` CLOSED; survey FP 24→0; sweep set CODIFIED ([CORPUS.md](../harness/CORPUS.md)).
- **2026-07-31 pin `v0.3.0→v0.3.1` + rbs `4.0.3→4.1.0`** — 0 FP / 9153, gaps −2: bounded type params resolve; `-> instance` via `SELF_RETURN`. [note](notes/20260731-upstream-pin-v031-rbs41.md)/[survey](notes/20260731-v031-preflight-survey.md).
- **2026-07-17–25 era, folded** — LSP §12 + coverage/MCP/audit (PRs #33–#45), ADR-0042 gate slices, MultiWrite substrate; docs economy; ATM arc + constant-shadow gate, gitlab UM 356→179. [LSP](notes/20260719-adr0042-gate-deliverables.md)/[ATM](notes/20260717-atm-substrate-arc-plan.md).
- **2026-07-16 v0.3.0-RC arc: pin `47ec8625` + 7 slices, ALL MERGED — rules fully ported.** [specs](notes/20260716-v030-upstream-gap-survey.md).
