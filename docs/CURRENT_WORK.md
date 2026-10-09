# rigor-rs — Current Work

The session-to-session baton: **what is in flight, what to pull next, and a
one-line ledger of what landed**. Port map: [PORT_BACKLOG.md](PORT_BACKLOG.md);
outcomes: `docs/notes/` + `docs/adr/`; history: `git log`.

**Contract (gated by `harness/docs_check.py`):** a landed/closed arc gets ONE
ledger line here — verdict + numbers + link — and its detail goes to a dated
note or ADR *first*. No status essays — hard byte budget.


## Now / Next

▶ **NEXT: pin `e59b7b89`, 0 FP / 9,337 / 815 gaps.**
CLI/config: #155–#157, #159, #169–#171, #130, #132; #160 blocked.
- **CLOSED arcs** (do not re-open): ADR-0042 migration + compat
  ([plan](notes/20260718-compat-next-stage-plan.md)).
- **CLI surface (v0.3.0 RC)** — `--bleeding-edge`, severity, `coverage` done;
  `--protection`/`--mutation` + `type-scan` deferred
  ([scoping call](notes/20260719-coverage-command-scoping.md)).
- **Pin is master `e59b7b89`** (rbs 4.2.0 vendored). Both
  standing exception tables EMPTY; new entries are real findings
  (`UPSTREAM.md` hazards + overlay/`sig/shims` trap).

State (verified 2026-10-10, at `e59b7b89`): harness **118 fixtures / 0
unregistered / 0 divergent**, coverage 651/708; standing sweep **0 FP /
9,337 files / 815 gaps**, 8 corpora; effects gate 0 OVER. `--sweep` ~3 min.
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
- **The receiver-typing lever is NOT exhausted** (2026-07 conclusion RETIRED): the
  08-07 census buckets gaps by MECHANISM; four slices closed **47 rows**. Re-run
  `gap_census.py --sweep` after each.
- **The effects TRANSITIVE LABEL lane is DECLINED — not portable at parity** (s4): four typer-free rules measured, best still 5 OVER; matching the reference outranks the ~2,000-method prize — labels stay UNDER. [probe](notes/20260826-effects-s4-probe.md).
- **sig-gen closed** — byte surface 0, `--write` sound.
- **Plugin work:** pure-RBS bundle track closed ([note](notes/20260710-pure-rbs-bundle-track-closed.md));
  sidecar + perf retired ([ADR-0037](adr/0037-sidecar-perf-slices-retired-by-measurement.md)).

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

- **2026-09-30 #309 CLOSED** (PR #333): rescue-carrier post-scopes join in coll snapshots + literal-arm member evidence (union collapse FP fix). 0 FP; → #337–#339. [note](notes/20260930-issue-309-rescue-snapshot.md)
- **2026-09-30 #325 CLOSED** (PR #336): `IndexedFlow` stored-slot narrowing (`h[k] ||= v` records reach `h[k]` reads). 0 FP; → #342–#344. [note](notes/20260930-issue-325-slot-narrowing.md)
- **2026-09-30 #158 CLOSED** (PR #335): config parity — Psych dup/`<<`-merge/multi-doc/UTF-8 + `~`/`..`/dist, exit-64 surface. 0 FP; → #345–#349. [note](notes/20260930-issue-158-config.md)
- **2026-09-30 #342 CLOSED** (PR #350): index-target `[]=` stores drop the IndexedFlow slot record (`drop_key` on MultiWrite/Loop/BeginRescue). 0 FP; → #352–#354. [note](notes/20260930-issue-342-index-target.md)
- **2026-09-30 #341 CLOSED** (PR #351): `when`/`in`-guard writes out of reach scans. 0 FP; → #355–#357. [note](notes/20260930-i341-when.md)
- **2026-09-30 #155 CLOSED** (PR #359): OptionParser parity for all subcommands — unknown flags 64, abbrev/`=`/`--`/POSIXLY_CORRECT, per-cmd tables. → #360. [note](notes/20260930-issue-155-optparse.md)
- **2026-09-30 #343 CLOSED** (PR #358): `h.attr op=` own-lowered; evaluated-position attr writes on mutator names drop IndexedFlow records. 0 FP; → #361–#366. [note](notes/20260930-issue-343-attr-writes.md)
- **2026-09-30 #357 CLOSED** (PR #367): blocked-extent writes stay out of reach/rebinds/binding — modifier `rescue` leak + dead-arm condition fix. 0 FP; → #368. [note](notes/20260930-i357-rescue-mod.md)
- **2026-09-30 #361 CLOSED** (PR #369): `OperandEffects.any?` ported into the `evaluated` decision (Dead/Gate/Eval modes). 0 FP; → #372/#373. [note](notes/20261001-i361.md)
- **2026-09-30 #157 CLOSED** (PR #371): config value validation — `Integer()` grammar, `enabled:false`-only, `target_ruby` split, plugin load-error rows. → #360. [note](notes/20260930-i157-config-values.md)
- **2026-10-01 #352 CLOSED** (PR #370): `String#[]` post-`[]=` reads non-firing + `T|nil` retry + rebind-gated index-target widening; `UnmodeledWrite` carries pattern-bound names. 0 FP; → #374. [note](notes/20261001-i352-string-index.md)
- **2026-10-01 #360 CLOSED** (PR #375): deferred `plugin path|print`/`skill --*` argument-boundary grammar — missing name → 64+usage, unknown → 1+`Unknown X`; bundled-name tables. → #376 (pin-drift). [note](notes/20261001-i360-deferred-grammar.md)
- **2026-10-01 #366 CLOSED** (PR #377): in-block attr writes drop indexed records — `closure_evaluated` + `closure_mutations`/`closure_descend` replay. 0 FP; → #379–#381. [note](notes/20261001-i366-block-attr-drop.md)
- **2026-10-01 #304 CLOSED** (PR #378): `RetainedParamType` keeps generic args — `Range[::int]` byte-identical both channels; `stub_typed_param` gate (kills a master FP). → #382–#384. [note](notes/20261001-i304-range-render.md)
- **2026-10-08 #368 CLOSED** (PR #385): `dead.rs` dead-position exclusion — folded if-arms, dead rescue clauses, modifier-under blocks; `RescueArm.span` clip; live-only ensure join; `!` fold Nil|Bool only. → #386. [note](notes/20261008-i368-dead-positions.md)
- **2026-10-10 #379 CLOSED** (PR #387): nested-body Barrier siblings apply under `owner=id` (re-key one level late); shared `owns_deferred_body`. 0 FP / 815; → #388, #381. [note](notes/20261010-i379.md)
- **2026-09-30 #332 CLOSED** (PR #334): pin-value threading (`local_reach` → `(Reach, Option<Scalar>)`); nominal multi-arg + `when`-pattern FPs. 0 FP / 818; → #340/#341. [note](notes/20260930-issue-332-multi-arg.md)
- **2026-09-30 #312 CLOSED** (PR #324): recovery collector models `joined`/`blocked`/loop-writeback marks — compound index writes widen exactly where the reference joins. 0 FP; → #325. [note](notes/20260930-issue-312-recovery.md)
- **2026-09-30 #146 CLOSED** (PR #318): `Reach::multi` + tier-3 decline for multi-valued args; unentered closures floor to untyped. 0 FP / 818; → #330–#332. [note](notes/20260930-issue-146-untyped-args.md)
- **2026-09-30 #162 CLOSED** (PR #323): baseline parity — (file,rule) regroup, Psych scalar reader/writer, `../` keys. 0 FP; → #326–#329. [note](notes/20260930-issue-162-baseline.md)
- **2026-09-30 #306 CLOSED** (PR #320): `Loop`/`rescue` index targets decline + `Range` bounds link Uncond in `flow_children` — kills the unlinked-span FP fallback. 0 FP / 818; → #321/#322. [note](notes/20260930-issue-306-effect-spans.md)
- **2026-09-29 #137 CLOSED** (PR #305): closure-bound locals shadow outer to `Dynamic[top]` in unentered blocks (entry-scope + shadow). 0 FP / 818; → #315–#317. [note](notes/20260929-issue-137-closure-shadow.md)
- **2026-09-29 #135 CLOSED** (PR #297): `Node::IndexWrite` for `h[k] op=/||=/&&=` routes into mutator widening; resolves #298. 0 FP; → #312–#314. [note](notes/20260929-issue-135-index-write.md)
- **2026-09-29 #136 CLOSED** (PR #296): per-site operand env replay (`OperandWalk` port) — later operands type from the scope earlier ones left. 0 FP; → #306–#311. [note](notes/20260929-issue-136-operand-scope.md)
- **2026-09-29 #134 CLOSED** (PR #295): index-target stores (multi-assign/`for`/`rescue`) widen receivers via `MultiTarget::Index`. 0 FP; → #298–#304. [note](notes/20260929-issue-134-index-widening.md)
- **2026-09-29 #194 CLOSED** (PR #291): symbol/BigInt/float witness spelling; literal-tuple block fold + write gate. 0 FP / 818; → #292–#294. [note](notes/20260929-issue-194-witness-rendering.md)
- **2026-09-29 #168 CLOSED** (PR #289): project-`sig/` `use` + `resolve-type-names` + missing-name stubs; alias-aware head-first resolver. 0 FP / 818; → #286–#290. [note](notes/20260929-issue-168-rbs-use.md)
- **2026-09-28 #201 CLOSED** (PR #285): `exclude:` + `BUILTIN_EXCLUDES` moved inside directory expansion (explicit `.rb` roots verbatim); exact `dir.c` `fnmatch` port; LSP exclusion-immunity for verbatim roots. 0 FP / sweep. [note](notes/20260928-issue-201-excludes.md)
- **2026-09-28 #139 CLOSED** (PR #284): `[]=` splice writes + top-level mutation widening (unconditional → `Nominal`, conditional → `Dynamic`, coll pass joins); `BeginRescue` flow arm; coll rule reads the widened env. 0 FP / sweep. [note](notes/20260928-issue-139-splice-write.md)
- **2026-09-28 #163 CLOSED** (PR #282): `text`/`github`/`sarif` byte-matched (`[rule]` suffix, `N error(s)` summary, `title=`, serde key-order); SARIF `driver.version` is the port's own. 0 FP; `output_formats.rs` pin. [note](notes/20260928-issue-163-output-formats.md)
- **2026-09-28 #164 CLOSED** (PR #283): `getbyte`/`rindex`/`byteindex`/`byterindex`/`Float#<=>` literal folds incl. nil; Dynamic decline via `declines_unfolded`, stale guard widened. 0 FP; fx 119. [note](notes/20260928-issue-164-nilable-fold.md)
- **2026-09-28 #199 CLOSED** (PR #281): list config keys read with `Array().map(&:to_s)` semantics — a scalar no longer drops the file to `Config::default()`; `signature_paths: ~` keeps reference nil→default. 0 FP; satisfies #157's scalar-`signature_paths` item. [note](notes/20260928-issue-199-scalar-list-keys.md)
- **2026-09-27/28 splits** #204/#234/#258/#260: lib.rs infer 14,084→144, rules 4,497→95; ast.rs 3,756→80; source_index.rs 4,650→668. [note](notes/20260928-ast-index-split-outcome.md)
- **2026-09-27 #138/#164/#167 ABANDONED** (PRs #184/#177/#183 closed unmerged): full-parity review met the deepest infer file; branches keep the work (`6e86120`/`9502a3a`/`eb1edd3`). [note](notes/20260927-abandoned-infer-pr-stream.md).
- **2026-09-27 #140 CLOSED** (PR #180): `tap`/`then`/`yield_self` call the block once — nominal self slot, `arm_of`/`join` auto-splat, reopen-aware union answering (`Node::Alias` + ancestor walk), `paths:` widen = `expand(paths|argv)>files` + excludes + undecidable-decline. 0 FP; fx 117; → #190/#195/#198–#203. [note](notes/20260927-issue-140-tap-exactly-once.md).
- **2026-09-26 #141 CLOSED** (PR #179): eval-block defs attribute to the receiver — `declaration_prefix` re-anchors rooted/self::, multi-segment names = ONE rung, per-file `Object` slice keeps `unresolved-toplevel`. **3,002 gaps (→827), 0 FP**; fx 118; → #185–#189/#193. [note](notes/20260926-issue-141-class-eval-defs.md).
- **2026-09-26 #166 CLOSED** (PR #174): block/lambda params shadow toplevel locals — bound set = Prism `locals`; membership structural not span. 0 FP; fx 116b. [note](notes/20260926-issue-166-block-param-shadow.md).
- **2026-09-26 #165 CLOSED** (PR #175): `wrong-arity` declines on splat/kwarg/`...`; `&b`→#176. 0 FP; fx 116. [note](notes/20260926-issue-165-splat-arity.md).
- **2026-09-25 #129 CLOSED** (PR #150): `conforms-to` fires only if provable; 4 review rounds found 39 FP families; **0 FP / 9,337**; → #155–#163. [ADR-0044](adr/0044-conforms-to-directive.md), [note](notes/20260925-conforms-to-audit.md).
- **2026-09-25 #151 CLOSED + #153 rows 1–3** (PR #154): `Statements` carriers gain a kind (`Inert` = `defined?`/`END`/`BEGIN`/`super`/`yield`, writes dropped; `Recovered` widens); `for` index is a rebind. **0 FP / 9,337, = master**; fixture 114 exact. Review: 0 new keys. [note](notes/20260925-issues-151-153-binder-writes.md).
- **2026-09-25 #121 CLOSED** (PR #149): String `[]`/`slice`/`byteslice`/`index` fold on literals; a class-guarded param drops the nilable slot (10 FPs). Review caught a stale-top-local nil fold + `i32`→`0` lowering + `nil&.m`, all fixed. **0 FP / 9,337, gaps = master**; fixture 113. [note](notes/20260925-string-lookup-fold-guarded-arg.md).
- **2026-09-25 #133 CLOSED by a decline** (PR #148): the flat top-level binder never saw a nested rebind; rules now widen such locals, Dynamic-only gates keep the old env. **0 FP / 9,337, = master**; fixture 112 7→0 FPs. → #152. [note](notes/20260925-issue-133-jump-path-rebind.md).
- **2026-09-25 re-pin `v0.3.9 → e59b7b89` (master, 1,030 commits)**: **0 FP / 9,337, 111 fixtures**, effects 0 OVER. The raw bump had 8 fixture FPs and 9 sweep FPs in two families: #1021 `imprecise_arg?` (now a reach analysis) and #1135's eval-block carve-out (its companion `fb781023` is unported = +3,002 gaps, #141). Re-synced `core_overlay/` (`hash_rbs3` excluded), the plugin sig (23 FPs) and the mutator sets (hash **20**). The fragment probe found 10 invisible retractions (#133–#138). [note](notes/20260925-repin-e59b7b89.md).
- **2026-09-21 #128 CLOSED — store values named by their TYPED answer** (PR #131) — member is the erased class of the value's `stmt_value_type`; union-carrier stores widen per-arm. **0 FP / 9,337**; #132. [note](notes/20260921-issue-128-store-value-typing.md).
- **2026-09-21 upstream re-pin `v0.3.8 → v0.3.9`** (447 commits): **0 FP / 9,337, gaps 799→892**, 108 fixtures, 0 OVER. Raw bump: 4 FPs in three families (`declines_bot?` shaped carriers, rooted `::RUBY_VERSION`, `MutationRejoin`). Residues: #128–#130. [note](notes/20260921-repin-v039.md).
- **2026-09-09 #123 CLOSED — 26 holes = THREE defects** — `overlay/` loads LAST, chain LAZY: **22** `module ::Kernel` keyed `Gem::Kernel`; **5** `prepend` uningested; **6** SELF-TYPE unrecorded. **26→0**, 0 FP / 9204 — DORMANT until `check_call`'s declaration-only conjunct drops. [note](notes/20260909-qualified-ancestor-closure-holes.md).
- **2026-09-09 the 799 gaps ADJUDICATED** — partitioned by MECHANISM: **395 sit behind decisions**; the 71-row bucket rests on ONE over-narrow rbs signature. `check` exited 0 on unparseable files (PR #125); ADR-0033's leniency premise expired → #123/#124. [799](notes/20260909-gap-adjudication-799.md) / [gems](notes/20260909-declared-unwitnessed-gem-classes.md).
- **2026-09-09 #118 CLOSED** (PR #120) — generic dispatch declines to `Dynamic[top]` when the join is at risk AND an argument is reference-untyped: **13 FPs closed, 0 matched lost**. Blanket "nilable declines" cost TEN folded rows — untypedness is what stops folding. [note](notes/20260909-generic-dispatch-untyped-arg.md).
- **2026-09-09 upstream re-pin `v0.3.4 → v0.3.8`** (924 commits / 4 releases) — **0 FP / 9204, gaps 820→799**, harness 98→**105** fixtures. Raw bump = 7 fixture retractions + 12 sweep FPs = **SIX families**, each bisected+ported. **Three spec claims were wrong; must-fire controls caught them.** Residues: 20 resolvable-`super` rows. [note](notes/20260909-repin-v038.md) / [spec](notes/20260909-repin-v038-port-spec.md) / [feedback 4](notes/20260909-upstream-feedback-batch4.md).
- **2026-08-26/28 the effects GATE was lying, three times** (PRs #112/#115/#117) — deleted-arm byte-identity, `methods:{}` covering 2 of 4 producers, `omit?` inverting ADR-0043 §2. [s112](notes/20260826-s112-effects-instrument.md) / [s5](notes/20260826-effects-s5-probe.md) / [s116](notes/20260826-s116-snapshot-gate.md).


- **2026-08-26 LSP honours `rootUri` / `workspaceFolders`** (PR #110) — enters the client's root rather than threading one (the root IS a cwd in all consumers; threading missed `sig/` + regressed `exclude:`). [note](notes/20260826-s111-lsp-rooturi.md).

- **2026-08-25/26 two arcs CLOSED, folded** — effects slices 0–3 (35 MATCH / 11 UNDER / 0 OVER — [s3](notes/20260826-effects-s3-impl.md)); frozen-index arc (file order NORMATIVE, eviction BLOCKED, harvest NO-GO — [s113](notes/20260826-s113-fold-capture-impl.md)).
- **2026-08-23/25 re-pin `v0.3.2 → v0.3.4` + survey (HOLD)** — 0 FP / 9204, gaps 841→820; raw bump opened **50 FPs**, all upstream RETRACTIONS invisible to the snapshot diff ([note](notes/20260823-repin-v034.md)). Survey found the **vendored plugin RBS had drifted = 10 FPs** (fixture 98 covers it now) and every `documentation_url` 404s ([survey](notes/20260825-upstream-survey-v034-master.md)).
- **2026-08-09 unresolved-const-receiver carrier REJECTED at 0 rows** (PR #89, closed). [note](notes/20260809-unresolved-const-receiver-carrier.md)
- **2026-08-09 era (3 slices, folded)** — re-pin `v0.3.1 → v0.3.2` (+rbs 4.1.1): 0 FP / 9204, gaps 1125→841; **trap: bundler/rubygems sigs depend on rbs's `sig/shims/` — 2 FPs the sweep CANNOT SEE**, closed by `overlay/rbs_shims/` ([note](notes/20260809-repin-v032.md)); join-wipe retention (1 FP closed — [note](notes/20260809-join-wipe-retention.md)); chain-guard meet (2 FPs closed — [note](notes/20260809-chain-guard-meet.md)).
- **2026-08-08 era, folded (0 FP / 9204)** — narrowing/shape trio (PRs #70/#75/#78/#80–#82, **26 rows closed** — [meet](notes/20260808-sequential-guard-meet.md) / [witnessing](notes/20260808-qualified-witnessing-mini-spec.md) / [shape](notes/20260807-collection-shape-slice-spec.md)); `Object` bucket adjudicated (PR #85 — [adjudication](notes/20260808-object-bucket-adjudication.md)); constant-value harvest + partial containers (PRs #83/#84 — [mini-spec](notes/20260808-partial-constant-harvest-mini-spec.md)).
- **2026-08-07/08 the class-narrowing ARC, CLOSED at a measured stop** (PRs #63–#79) — `narrow_class_other` end-to-end: **19 gap closures + 11 master FP shapes**, 0 FP / 9204 throughout. Lessons: FP-safety argument WRONG 3× (position axis; carrier allow-list; disjoint→`Bot`). [spec](notes/20260807-class-narrowing-slice-spec.md) / [stage3](notes/20260807-narrowing-stage3-spec.md).
- **2026-08-01/08 instruments + adjudication, folded** — the 0-FP gate could pass VACUOUSLY (PR #65: corpus tools measured `target/release` while `cargo build` writes debug, and `run_rs` swallowed failures into `[]` — [note](notes/20260807-fp-audit-port-side-blind-spots.md)); the coverage-gap CENSUS buckets gaps by MECHANISM, not rule, and half sit behind decisions already made ([note](notes/20260807-gap-census.md)); `arity_eligible?` was never ported = a `call.wrong-arity` FP (fixture 80); LSP config reload keeps LAST GOOD ([lsp](notes/20260801-lsp-config-reload.md)).
- **2026-08-07 ADR-0042 S5: qualified return-lookup routing** (PR #64, MERGED) — the 8-member return family routes namespaced receivers via the qualified registry (refs AS WRITTEN + lexical ctx; ambiguity DECLINES); **14 closures (→1179), 0 FP / 9204**; fixture 82 pins the Tier-3 instance boundary (gaps 3→4 on merge). [spec+outcome](notes/20260807-adr0042-s5-return-lookup-spec.md).
- **2026-08-07 upstream survey + feedback batch 2, folded** — the `v0.3.1`→`80aaf9bc` 2×2 self-diff moved 2 diagnostics on 9204 (superseded by the `v0.3.2` re-pin) and RETIRED GEM_HOME rbs selection, which had silently dropped 1650 files ([note](notes/20260807-upstream-survey-v031-to-master.md)); batch 2 filed 3 reference-side defects with paste-ready repros — the `c7f28da1` master FP, the `Dynamic|nil` possible-nil FP class, and the fail-soft definition build blinding 12 classes ([note](notes/20260807-upstream-feedback-batch2.md)).
- **2026-07-31 era (7 slices, folded)** — sig-gen `Data.define`/`Struct.new` members; `BigMath` blinded-oracle asymmetry CLOSED; LSP v4 const completion; survey FP triage 24→0 ([note](notes/20260731-survey-fp-triage-24.md)); project-`sig/` blind-spot → fixture 79; `-> self` on instance methods; standing sweep set CODIFIED ([CORPUS.md](../harness/CORPUS.md)).
- **2026-07-31 upstream pin `v0.3.0 → v0.3.1` + vendored rbs `4.0.3 → 4.1.0`** — **0 FP / 9153 files, gaps net −2**. 4.1.0's rewritten signatures broke two things, both FIXED rather than accepted: bounded method type params now resolve to their bound, and `-> instance` on an INSTANCE method resolves via the `SELF_RETURN` call-site sentinel. New `harness/vendor_rbs.py` makes the vendoring recipe executable (proven by reproducing the 4.0.3 tree byte-for-byte first). Upstream logic delta: ZERO. [note](notes/20260731-upstream-pin-v031-rbs41.md) / [survey](notes/20260731-v031-preflight-survey.md).
- **2026-07-25 era (3 slices, folded)** — MultiWrite substrate s1+s2 (PRs #46/#47: arena lowering + `MultiTargetBinder`, RBS tuple returns; 14 corpora / 35,706 files bit-identical — [spec](notes/20260725-multiwrite-substrate-spec.md)); LSP `exclude:` parity (PR #45, matrix 24→144); LSP stage-3 parity tail (PR #44, 4 E2E tests vs real `check`, each proven non-vacuous).

- **2026-07-18/19 era (5 arcs + the RC bump, folded)** — LSP §12 two-tier S1–S4b (PRs #35–#38/#42/#43); upstream tracking `b70adcb5..ff6b6158` (0 added / 0 dropped, nothing to port); `coverage` precision mode + MCP tool + node-granularity audit (PRs #33/#40); ADR-0042 gate + Slices 1–4 ([deliverables](notes/20260719-adr0042-gate-deliverables.md)); the compat arc Phases 0/1/3 + M2 receiver typing + severity machinery + `--bleeding-edge` ([findings](notes/20260718-phase0-m1-m2-findings.md)); and the RC bump `47ec8625→7a69f142` (80 commits, 2 parity divergences closed at 0 FP — [note](notes/20260718-upstream-rc-bump-47ec8625-7a69f142.md)).
- **2026-07-17 era (folded)**: docs economy (baton + [PORT_BACKLOG.md](PORT_BACKLOG.md), `harness/docs_check.py`, #21); Tier B/C CLOSED ([note](notes/20260717-tier-bc-track-closed.md)); ATM arc + constant-shadow gate + C3a String-tail, gitlab UM 356→179 ([spec](notes/20260717-atm-substrate-arc-plan.md)).
- **2026-07-16 two MERGED inference slices** — `def.ivar-write-mismatch` (`a2098d7`; gitlab ivar gaps 2→0) and literal-tail return folding (`0721943`; gitlab always-truthy 28→16). 0 FP both. [ivar](notes/20260716-ivar-write-mismatch-spec.md) / [fold](notes/20260716-literal-tail-fold-spec.md).
- **2026-07-16 v0.3.0-RC arc: pin `47ec8625` + 7 slices, ALL MERGED** — syntactic rules (dup-hash-key, return-in-ensure, suppression.*, `Node::Lambda`), MutationWidening (killed 2 measured FPs), implicit-self dispatch + `p`/`pp`, scalar HashShape keys + projection folds, Kernel `format`/casts folding, `raise-non-exception` + `class_ordering`, `shadowed-rescue-clause` + rbs.rs nesting root-fix. **v0.3.0 rule surface fully ported.** [specs](notes/20260716-v030-upstream-gap-survey.md).
- **2026-07-11 sig-gen arc CLOSED (13 slices) + periphery (4 items)** — `erase_to_rbs` → `--print` → return-union → singletons → `--write` → initialize stub → `--diff` → module_function → Writer merge+LayoutIndex → env classification → `--overwrite` → qualified naming → Data/Struct shells; 0 shared-method mismatch on the full sweep, `--write` sound. Periphery: MCP `sig_gen` tool; `--params=observed` SUBSTRATE-BLOCKED ([note](notes/20260711-siggen-params-observed-substrate-blocked.md)); coverage frontier re-measured — bounded wins exhausted ([note](notes/20260711-coverage-frontier-remeasured.md)).
