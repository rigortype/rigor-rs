# rigor-rs — Current Work

A living map of **what is done** and **what remains to port** from the Ruby
reference (`/Users/megurine/repo/ruby/rigor`) into rigor-rs. Organized as a
port list keyed to the reference's subsystems. **Order is not binding** — pull
whatever is highest-leverage next; this file exists so nothing is lost, not to
fix a sequence.

Last updated: 2026-07-16 (v0.3.0-RC upstream-tracking arc: pin bump + SEVEN slices merged in one
session — see the entry below). Previous: 2026-07-11 (sig-gen arc: THIRTEEN slices merged — erase_to_rbs substrate → --print →
return-union/Node::Return → singletons → --write create → initialize stub → --diff → module_function self?. →
Writer UPDATE/merge + LayoutIndex → generation-time env classification → --overwrite replace path → qualified
source-class naming → Data.define/Struct class shells; sound-superset parity model recorded in **AGENTS.md
"Generative-tool parity"**). Then PIVOTED to productization (MCP `sig_gen` tool). Read `AGENTS.md` "Working
discipline" before continuing.

**▶▶ LANDED (2026-07-17, branch `constant-shadow-gate`, MERGED) — C1+C2+C5: the largest single coverage win of
the port (gitlab-foss lib undefined-method gaps 356 → 200, −156, 0 FP; mastodon matched 397 → 404).** From the
first-ever UM/PN cause-classification on gitlab lib (specs:
[constant-shadow-gate-spec](notes/20260717-constant-shadow-gate-spec.md)). **C1 (−132, measured):** the
ConstantRead suppression matched the BARE written name project-wide, so one nested `module Time` killed all
`Time.*` witnessing batch-wide; now lexically gated (toplevel suppresses everywhere; nested only where visible)
— a strict relaxation, FP-free by construction. **C2 (−21):** parameter default-value expressions are now
lowered and walked (params stay unbound — literal/const receivers only). **C5 (−3):** single-assignment
fully-literal `CONST =` harvest, lexically gated (the first draft's bare-name harvest produced 2 measured FPs
via a Concern constant — caught by the per-part fp_audit gate and fixed before landing; Range consts remain
inert — Range is outside the 9-class witnessing surface, so the estimated ~24 C5 pool was Range-dominated and
mostly stays open). Remaining UM 200 is AS-overlay-dominated + C3a (Module#name→String tier-3 optional-unwrap,
~42, next bounded candidate); PN 169 stays ~75% Tier B/C-blocked (P2 straight-line optional-local slice ~15-20
borderline). harness 63 fixtures / 173 matched; 725 tests.

**▶▶ LANDED (2026-07-16, branch `ivar-write-mismatch`, MERGED `a2098d7`) — `def.ivar-write-mismatch` ported
(the BOUNDED half of the unported-rule pair).** Sonnet investigation verdict
([spec](notes/20260716-ivar-write-mismatch-spec.md)): ivar-write-mismatch BOUNDED (ships on existing
substrate), `call.argument-type-mismatch` SUBSTRATE-BLOCKED (needs per-overload/per-param RBS retention — rbs.rs
keeps only merged arity — an alias/interface recovery layer, and a net-new acceptance/subtyping engine; 3 corpus
gaps total, mastodon fires ONCE in 1236 files ⇒ deferred as a shared-substrate investment). Landed: dedicated
`Node::InstanceVariableWrite` lowering (name preserved; 6 VariableWrite consumers extended), RescueClause
`bound_name` + rescue-body exception-class binding (single-class + bare⇒StandardError; multi-class union silent,
probe-verified), Kernel `Integer()/Float()/String()` NOMINAL fallback on non-constant args (reference types them
unconditionally), and the collector (canonical-first-non-nil logic incl. the whole-group-bails-on-None subtlety,
clear-to-nil idiom, bool folding, op-writes/`self.x=` excluded, block bodies NOT barriers, singleton defs
excluded). **Measured: gitlab-foss lib ivar gaps 2 → 0** (both corpus lines byte-identical), 0 FP everywhere
(mastodon 397 unchanged, rule fires 0 there); harness 60 fixtures / 163 matched; 716 tests; explain 26 rules.

**▶▶ LANDED (2026-07-16, branch `literal-tail-fold`, MERGED `0721943`) — interprocedural literal-tail return
folding: the FIRST measured-closeable coverage lever after four all-substrate-blocked flow surveys.** A
37-gap classification of gitlab-foss `lib`'s non-UM/PN gaps found Cluster A (19/28 always-truthy = project
methods whose return joins to one scalar literal, `Gitlab::Database.read_only? = false` archetype) closeable
— distinct from the exhausted ivar/flow substrate. Spec:
[literal-tail-fold-spec](notes/20260716-literal-tail-fold-spec.md). Landed: SourceIndex Pass-4 —
qualified-owner SINGLETON-method harvest (previously `def self.x` fed only sig_gen), `(method, kind)→owners`
definers index, memoized cycle-guarded recursive literal fold (depth-capped 16; `read_write? = !read_only?`
folds depth-2); `type_call` tier 4c for `Const.method` (dedicated tier — project constants deliberately NOT
typed Singleton, zero blast radius); implicit-self resolution threaded with the enclosing class + ancestry.
**Two audited spec deviations (implementer overrode the spec with oracle proof, both sound):** own-class/
ancestry-scoped resolution instead of name-only (name-only would FP on unrelated same-name owners), and the
reference's related-redefiner override gate instead of the single-definer guard (recovers 2 gaps from
unrelated same-name definers — main-session audit probe confirmed byte-parity on exactly this shape).
Declined (recall-only, documented): branch-tail joins (probe 18), boolean-chain folds. **Measured:
gitlab-foss lib always-truthy gaps 28 → 16 (12 closed), matched 812 → 824, 0 FP on all corpora; mastodon 397
unchanged (its 2 remaining always-truthy gaps are Cluster-B flow-substrate, still blocked).** harness 59
fixtures / 155 matched. Remaining cluster-B/C/D/F gaps + rule-not-ported (argument-type-mismatch,
ivar-write-mismatch): see the classification in the spec note's provenance section.

**▶▶ LANDED THIS SESSION (2026-07-16) — v0.3.0-RC UPSTREAM-TRACKING ARC: pin bump + 7 slices, ALL MERGED.**
Upstream reached the v0.3.0 release-candidate stage; this arc measured the RC gap set and closed it. Full
survey + binding specs: `docs/notes/20260716-v030-*.md` (4 specs) + `20260716-mutation-widening-fp-spec.md`.
Protocol: 5 Sonnet investigations (changelog inventory, three rule-cluster specs, FP root-cause) → binding
specs committed → 6 Opus implementation slices → main-session independent adversarial audit per slice.
Merges (in order): **(1) `pin-v030-rc`** — submodule pin `v0.2.7` → RC commit `47ec8625` (a COMMIT pin, first
time; re-pin when the tag lands); snapshots 0 changed/43 unchanged, RC is behavior-identical on the old corpus.
**(2) `v030-syntactic-rules`** — `flow.duplicate-hash-key`, `flow.return-in-ensure`,
`suppression.unknown-rule`/`suppression.empty` + substrate (HashLit dup-key capture, BeginRescue
`ensure_body`, `Node::Lambda` lowering — closed a general `-> {}` invisibility soundness gap, comment
column). **(3) `mutation-widening-fp`** — ported the reference's MutationWidening as a `collect_flow_writes`
extension, killing 2 MEASURED always-truthy FPs on gitlab-foss `lib` (a long-standing never-ported subsystem
surfaced by the first gitlab-lib sweep, NOT RC-new — v0.2.7 already suppressed; the interprocedural
callee-mutates-arg floor `af3efef3` IS RC-new, 0 live instances, deferred). **(4) `v030-p-pp-identity`** —
the NEW implicit-self dispatch entry (`type_implicit_self_call`; receiver-None calls previously fell to the
Dynamic catch-all untyped) + Kernel `p`/`pp` identity typing (0→nil, 1→identity, N→Tuple; shadow/splat
guards). **(5) `v030-scalar-hashshape`** — Integer/Float/bool/nil hash keys value-pin (ShapeKey Float/Bool/
Nil), duplicate keys last-wins (first position, last value; also fixed the pre-existing all-Symbol dup-key
degradation), hashrocket rendering for non-Sym/Str keys, degraded-erasure union sorted by describe, + the NEW
`fold_hash_shape_projection` tier ([]/fetch/dig/has_key?/slice/except/values_at/invert). **(6)
`v030-kernel-folding`** — `format`/`sprintf` (hand-written Ruby-sprintf interpreter, 4096-byte cap,
decline-on-uncertainty; float directives/positional/named forms decline), `String()`, `Hash()`, `Integer()`
(radix/underscore/base-arg grammar), `Float()`. **(7) `v030-raise-non-exception`** — `call.raise-non-exception`
(error severity) + NEW public `CoreIndex::class_ordering` (Equal/Subclass/Superclass/Disjoint/Unknown) +
`is_module` bit; asymmetric singleton/instance verdict paths, project-class unconditional bail, duck
`#exception`, redefinition gates, `first_arg_nonplain` on Call (kwargs-vs-braced-hash distinction — FP
load-bearing). **(8) `v030-shadowed-rescue`** — `flow.shadowed-rescue-clause` + `RescueClause` per-clause
lowering (flat `body` byte-preserved) + a ROOT FIX in rbs.rs (nested `Psych::Exception` was overwriting
top-level `Exception`'s superclass in the short-name index, manufacturing a cycle and a probe-caught FP).
**Final state:** harness 57 fixtures / 149 matched / 0 FP (live + snapshot); fp_audit 0 FP on mastodon app
(matched 397, unchanged), gitlab-foss lib + app/models, conference-app; 665 workspace tests; explain catalog
25 rules. **The v0.3.0 diagnostic-rule surface is fully ported.** Remaining RC deltas (documented, deferred):
interprocedural mutation floor (P6), `Kernel.format` explicit-receiver folds, float sprintf directives,
literal-string lift carrier on failed folds, plugin-only changes (no plugin engine), new CLI surface
(`--bleeding-edge`, coverage `--workers`, plugins inflection probe — productization candidates). When
upstream tags v0.3.0: re-pin per UPSTREAM.md (expect snapshots unchanged), re-run fp_audit.

**▶▶ LANDED (2026-07-11, branch `mcp-sig-gen`, MERGED `e7ae83e`) — productization: `sig_gen` MCP tool.** The
clean pivot after the sig-gen arc closed: surfaces the sig-gen work through the MCP tool surface an agent calls
(reference `rigor_sig_gen`). Read-only — runs `sig-gen --print --format=json` over FILE `paths` (+ optional
`config`), returns the `{ candidates: [...] }` JSON, panic-isolated. `--params=observed` is NOT exposed
(substrate-blocked). `sig_gen.rs` gained `candidates_json_string` + the `mcp_report_json` seam; `mcp.rs` the tool
declaration + `tool_sig_gen` handler. The MCP surface is now check / type_of / explain / outline / triage /
annotate / sig_gen. **Verified:** stdio smoke (initialize → tools/list has `sig_gen` → call returns candidates)
byte-identical to the CLI `--print --format json`; rigor-cli 246 tests, harness 54/54 0 FP, clippy clean; check
path untouched. The reference's other missing MCP tool, `rigor_coverage`, needs the large mutation-backed
coverage command (unavailable) — deferred.

**▶▶ sig-gen arc RETROSPECTIVE (13 slices, this + prior sessions): byte-mismatch surface CLOSED, `--write` SOUND.**
0 shared-method rbs-mismatch on the full `reference/rigor/lib` sweep; no dangling refs in written RBS. Remaining
sig-gen items are thin coverage-only (attr_* readers — needs an ivar-typing pass, measured ~40 attrs mostly from
computed ivars, byte-risky; merge-path shell injection; non-core-named Data.define/Struct receiver typing — a
pre-existing rigor-infer gap; `--params=observed` — substrate-blocked). **NEXT: the higher-leverage tracks are
now a DIFFERENT direction — the ScopeIndexer substrate (unblocks --params=observed AND the flow/possible-nil
clusters, ADR-0022), more productization (LSP §12 two-tier / watched-files / more MCP tools), or a measured
coverage rule.** The parity-port arc has fully bottomed out; productization + deep-substrate are the frontier.

**▶▶ CONDITIONAL-ASSIGNMENT NILABILITY — BUILT + FP-SAFE but 0 SURVEY GAPS, NOT merged (branch
`flow-cond-assign-nilability`, `7b7fe3d`).** The user-chosen flow slice, done via the full delegation protocol
(2 Sonnet investigations + oracle self-probes → binding spec → Opus impl → main audit). Turned out to be
materially ADR-0038 Slice 2 (the `Node::If` descend + `nenv`/`penv` join + mandatory truthy-`if` narrowing
substrate; the conditional-assignment nil emerges from the join). **Correct + FP-safe** (self-probe matrix
byte-identical to the reference — `x = "s" if c; x.upcase` fires, guards/reassign/safe-nav silent; 0 FP on
mastodon 1236 + gitlab-foss 6513 + conference-app 98, matched count UNCHANGED; harness 54/54; corpus 27/28
non-regression). **But `fp_audit --gaps` closes 0** (mastodon possible-nil 26→26): the 26 gaps are all
`present?`-guarded (accepted under-emit) or PROJECT-METHOD/IVAR nilable returns (Tier B/C, which rigor-rs lacks —
the local is never minted nilable), NONE the unguarded core-typed pattern this closes; that pattern is absent
from the corpora. **The 4th consecutive FP-safe flow slice to close 0 survey gaps** — confirming (again) the
possible-nil frontier is Tier B/C ([ADR-0041](adr/0041-project-method-nilable-return.md), code on
`tier-bc-nilable-return`) + ivar whole-class flow (ADR-58), full stop. **Per AGENTS.md "never ship a speculative
slice", NOT merged**; the If-descent/join is reusable ADR-0038 Slice 2 substrate preserved on its branch (merge
WHEN a measured gap needs it). Detail:
[notes/20260711-conditional-assign-nilability-spec.md](notes/20260711-conditional-assign-nilability-spec.md).
**⇒ The possible-nil sub-tracks are now exhausted at the flow-substrate level; the ONLY remaining lever is Tier
B/C RHS-return inference (a deeper, separate substrate).**

**▶▶ COVERAGE FRONTIER RE-MEASURED (2026-07-11) — bounded wins EXHAUSTED, next is deep-substrate.** Ran
`fp_audit.py --gaps` on mastodon (0 FP throughout): `app/models` 108/115 matched, 7 gaps; `app` 397/459, 62 gaps
(undefined-method 33 tapped-out, possible-nil 26, always-truthy 2, arg-type-mismatch 1). **Characterized the
possible-nil gaps** = conditional-assignment nilability (`local = expr if cond` ⇒ `local` is nil on the false
path; the reference flags every call on it, even the nil-safe `.present?`, and does NOT narrow via `present?`) —
a flow-substrate feature with real FP risk, NOT a bounded cheap win. Full detail + the decision table:
[notes/20260711-coverage-frontier-remeasured.md](notes/20260711-coverage-frontier-remeasured.md). ⇒ No cheap
FP-safe coverage slice remains; the three real next tracks are (1) flow/ScopeIndexer substrate — highest
leverage, multi-session, unblocks possible-nil + always-truthy + --params=observed; (2) more productization (LSP
§12, coverage command); (3) a single FP-risky flow slice (conditional-assignment nilability, closes the 26
possible-nil dominant cause) via the full delegation protocol. **AWAITING DIRECTION on which — all are large or
FP-risky, so none is a speculative slice to start unprompted.**

**▶▶ LANDED THIS SESSION (branch `sig-gen-data-shells`, MERGED `33f9436`) — sig-gen slice 13: Data.define/Struct
class shells on `--write`.** Completes slice 12: qualified naming made `--write` emit `-> Rigor::Triage::Selector`
but never declared `class Selector`, leaving a DANGLING reference in the generated RBS (Steep can't resolve it —
a soundness gap slice 12 introduced). Ports the reference's `@class_shells`: a `Const = Data.define(...)` /
`Struct.new(...)` constant now writes an empty `class Const\nend` shell. `collect_namespace_info` records each
shell FQN (source order) + kind `class`; `cmd_write` routes each shell to its ENCLOSING class's target (rides
beside its class in a consolidated layout) + adds shell-only targets; `render_new_file` appends empty shell class
nodes AFTER the method + real-nested children (reference tree order) via `insert_shell_into_tree`. Shells appear
ONLY in `--write` (never `--print` — no candidate). **Scope: the CREATE path;** merge-path shell injection is
deferred (a `--write` re-run is idempotent since the shell was written on create — only merging into a
USER-authored sig lacking the shell is uncovered, documented). **Verified byte-identical vs the oracle:** mixed
(methods/real-nested/shells order, Data + Struct, top-level + nested shells) + the real triage.rb (5 shells now
declared, dangling refs resolved) + idempotent re-run (`No changes`, matches reference). 567 tests (+4), harness
run.rb + run_snapshot.rb 54/54 0 FP, clippy clean.
**⇒ sig-gen `--write` output is now SOUND (no dangling refs) as well as byte-mismatch-free.** The remaining
sig-gen surface is thin coverage-only: `attr_*` reader generation, merge-path shell injection, non-core-named
Data.define/Struct RECEIVER typing (pre-existing rigor-infer gap), and `--params=observed` (substrate-blocked).
The arc is at a clean stopping point — next is a DIFFERENT track (ScopeIndexer substrate, productization, §12 LSP).

**▶▶ LANDED THIS SESSION (branch `sig-gen-qualified-naming`, MERGED `0f122b6`) — sig-gen slice 12: qualified
source-class naming + source-order class emission.** Closes the LAST known shared-method byte mismatch in the
`reference/rigor/lib` sweep: a source-class instance return (the `Selector = Data.define(...)` constant in
triage.rb, nested `class Inner`) rendered its WRITTEN short name (`-> Selector`) where the reference emits the
fully-qualified `-> Rigor::Triage::Selector`. **rigor-parse:** `Node::ConstantWrite` gains the written constant
`name` (additive — every existing match uses `{ value, .. }`, zero blast radius) so sig-gen can map the file's
`Data.define`/`Struct.new` constant FQNs. **sig-gen:** `collect_source_fqns` builds the file's declared
class/module + class-defining-constant FQN set; `erase_qualified`/`describe_qualified` wrap the base resolver so
a SOURCE class short name is qualified via Ruby constant lookup from the method's enclosing scope
(`qualify_source_name`, longest-enclosing-prefix) while CORE names pass through unqualified — sound because the
sig-gen `SourceIndex` is per-file (every typed source class is defined in the file, FQN always resolvable). The
old `nested_classes` skip is REMOVED (those returns now emit qualified — a coverage gain). **Also fixed a
pre-existing class-group ORDER divergence:** candidates now emit + descend into nested classes in ONE
source-order (span) pass, so a nested class declared before the outer's own methods groups ahead of its parent
(reference walk + `group_by`). **Verified:** sweep 250 shared methods / **0 rbs-mismatch** (Selector CLOSED);
whole-file byte-identical **162 → 166** (net gain, no regression; master-vs-branch measured); 565 tests (+2),
harness run.rb + run_snapshot.rb 54/54 0 FP (arena change inert on the check path), clippy clean. **Remaining
under-emits (documented, sound-superset):** `Data.define`/`Struct.new` empty CLASS SHELLS on `--write` (reference
writes `class Selector\nend`; rigor-rs qualifies RETURNS of it but doesn't generate the shell — a valid subset),
and non-core-named `Data.define`/`Struct` RECEIVER typing (a `Const.new` types to a source class only when
`Const` collides with a core RBS name; else `Dynamic` → skip — a pre-existing inference gap, not a naming defect).
**⇒ The sig-gen arc's byte-mismatch surface is now CLOSED (0 shared-method mismatch on the full sweep). Remaining
sig-gen items are pure COVERAGE under-emits (Data.define shells, Struct/attr generation, --params=observed
substrate-blocked) — no known correctness gap left. The arc is at a clean stopping point; next work is a
different track (deeper ScopeIndexer substrate, productization, or §12 LSP).**

**▶▶ INVESTIGATED THIS SESSION (2026-07-11) — `--params=observed` SUBSTRATE-BLOCKED, deferred (NOT built).**
Investigated as the next slice after `--overwrite` via the full protocol (value probe + two Sonnet
investigations — reference `ObservationCollector` + rigor-rs substrate — + a main-session literal-vs-nominal
measurement). **A faithful byte-safe port is BLOCKED on the ScopeIndexer rigor-rs lacks** (the same per-scope
typing substrate ADR-0022 / the flow frontier need). The reference types observed call-args with the FULL
`scope.type_of` (locals / `let` / self-call returns), and real specs overwhelmingly use those, not inline
literals; rigor-rs can type ONLY literal args (scope-independent) and types block-local reads `Dynamic`. The
parity KILLER: any `Dynamic` member collapses an observed union to bare `untyped`, and the `initialize` stub is
ALWAYS emitted by both tools — so a class whose observe tree has even one scope-dependent caller yields
reference `def initialize: ("hi" | String) -> void` vs rigor-rs `(untyped) -> void`: a **shared-method byte
mismatch**, NOT a safe under-emit. A literal-only partial port is therefore a NET REGRESSION (converts an honest
exit-2 into a mismatching partial impl) and additionally needs an arena keyword-hash discriminator (the matchable
value is ~all keyword args). Kept at exit-2. Full detail + the value numbers:
[notes/20260711-siggen-params-observed-substrate-blocked.md](notes/20260711-siggen-params-observed-substrate-blocked.md).
**⇒ The honestly-portable sig-gen surface is now essentially EXHAUSTED.** The one known SHARED-METHOD byte
mismatch left in the `reference/rigor/lib` sweep is **qualified source-class naming**: a `Const = Data.define(...)`
(or nested `class`) instance return emits its WRITTEN short name (`-> Selector`) where the reference emits the
fully-qualified `-> Rigor::Triage::Selector` (`lib/rigor/triage.rb:28,139`; probe-confirmed current). A real,
bounded, byte-safe-to-close fix — but NON-trivial (needs a SourceIndex investigation: how `Data.define`
constants register + how a nested-scope constant receiver resolves to an FQN-named Nominal), so it wants its own
investigation before implementation. That, or a pivot: the parity-port arc has bottomed out (this is the fifth
"big track, thin/blocked value" finding across the arc) — the deeper frontiers are the ScopeIndexer substrate
(unblocks --params=observed AND the flow/possible-nil clusters) or productization / §12 LSP infra.

**▶▶ LANDED THIS SESSION (branch `sig-gen-overwrite`, MERGED `9e85e07`) — sig-gen slice 11: `--write
--overwrite` replace path.** The payoff slice 10 unlocked — a bounded main-session port (built + oracle-audited
directly on the merge code I'd just line-audited, no delegation round-trip). Ports the reference Writer's
`replace_eligible_conflicts` / `apply_replacement`: with `--overwrite`, a `tighter_return` conflict (the
classifier already proved a strict subtype) has its EXISTING RBS declaration spliced out and replaced by the new
one-liner, moving from `skipped user-authored` → `applied` (`updated (+N, skipped 0)`); without the flag the
same candidate is preserved byte-untouched. A `NEW_METHOD` conflict is replaced ONLY when its RBS has strictly
fewer bare `untyped` tokens than the existing decl (reference `tightens_untyped?`, word-boundary `count_untyped`)
— that path is ported FAITHFULLY but is **dead for BOTH tools until `--params=observed` lands** (an `initialize`
stub stays `(untyped) -> void`), so its absence is parity-safe. Replacements apply **highest-byte-offset-first**
so earlier member spans stay valid; new-method insertion runs first (at `end_start`, after every member) exactly
as the reference orders `insert_into_class` before replace. `--print`/`--diff` ignore the flag (it lives on the
Writer); the exit-2 stub is removed. **Substrate:** `MemberInfo` gains the member's byte span + raw text; a new
`Conflict` struct carries them into `merge_into_existing`; the skip-conflict logic is factored into a helper
shared by both branches. **Verified byte-identical vs the oracle (fresh dirs):** single/multi tighter (offset
ordering), mix (new + tighter + a wider-DROPPED-at-classification `count` — the existing decl stays untouched),
singleton, consolidated multi-class routing (Alpha replaced, Beta untouched), idempotent re-run (`No changes`),
`--print --overwrite` inert, `--write --overwrite` text + JSON. **Gated:** 563 tests (+5), harness run.rb +
run_snapshot.rb 54/54 0 FP, clippy clean. **Remaining sig-gen frontier:** `--params=observed`
(ObservationCollector — the last big machinery, also lights up the overwrite untyped-tightening path), qualified
source-class naming (the `Data.define` nested-constant `-> Selector` vs `-> Rigor::Triage::Selector` gap).

**▶▶ LANDED THIS SESSION (branch `sig-gen-env-classification`, MERGED `a268a6c`) — sig-gen slice 10:
generation-time env classification.** Ports the reference generator's classify-against-existing-RBS
(`new_method` / `tighter_return` / equivalent-drop) so sig-gen on a project WITH a `sig/` now prints
`# [tighter, was: X]` (not `# [new]`), the `--diff` `- def name: () -> X` declared line, and JSON
`classification`/`declared_return_rbs` at GENERATION time — closing the shared-method byte/tag mismatch that was
the hard-guarantee break, and unlocking `--overwrite`. **The design note's original "rigor-rs mapping" was
FALSIFIED by main-session probes before implementation** (amendment appended to the spec, commit `1506c59`):
(1) `CoreIndex` is short-name keyed and MERGES same-short-name classes, so `knows_class("M::Foo")` is always
false — the sketched gate would leave every nested (real-world) class as `new_method`; (2) `ClassEntry` had no
singleton return; (3) the conservative `class_has_method` "assume present" conflates not-declared (emit) with
declared-unresolvable (drop); (4) the gated `ObservedCall#hash` excess never existed (that class is absent from
`reference/rigor/sig`). **Built instead:** a sig-gen-local **FQN-keyed `SigEnv`** (`crates/rigor-cli/src/
sig_gen/sig_env.rs`) over the project `.rbs` + three **additive, precise, sig-gen-ONLY** accessors on
rigor-index (`declared_instance_return`/`declared_singleton_return`/`chain_complete`, three-valued
`Option<Option<&str>>`, never "assume present"); `ClassEntry::singleton_methods` widened to carry the return
(check path inert — diagnostic predicates read only the key set). classify ports the full `classify_def` tail:
equivalent-drop, wider-drop, `narrows_collection_to_shape?` (GENERIC_COLLECTION_CLASSES read verbatim),
`computed_literal_tightening?` on the RAW `sig.body.last()` (no assignment unwrap). **Independently audited
before merge:** 558 tests, harness run.rb + run_snapshot.rb 54/54 0 FP, clippy clean on both touched crates;
main-session fresh-dir oracle re-probe 13 scenarios × 3 modes = 39/39 byte-identical (incl. probe O
superclass-project-sig, and the assign-tail / computed-literal DROP cases); `reference/rigor/lib` sweep 239
shared def lines, 0 tag mismatch. **Under-emits (FP-safe, documented):** incomplete ancestor chain → DROP (not
`# [new]`); non-single-overload/union/optional/untyped/generic-arg declared → DROP; per-file parse isolation
instead of the reference's whole-env collapse (ADR-0016/79). **Remaining sig-gen frontier:** `--overwrite`
(now unlocked — tighter-return replacement in the Writer), `--params=observed` (ObservationCollector),
qualified source-class naming (the pre-existing `Data.define` nested-constant `-> Selector` vs
`-> Rigor::Triage::Selector` naming gap — the sweep's only rbs-line mismatch, unchanged by this slice).

**▶▶ LANDED THIS SESSION (branch `sig-gen-writer-update`, MERGED `c02dcdc`) — sig-gen slice 9: Writer
UPDATE/merge + LayoutIndex.**
The heavy remaining piece, built via the full delegation protocol (Sonnet×2 independent investigations — source
report + 9-scenario byte-exact oracle probes — → binding design note → Opus implementation → main-session
independent audit). `--write` now MERGES into existing `.rbs` files: **parse-for-location, splice-as-text,
reparse after every mutation** (ruby-rbs `end_location()` byte offsets; parse-failure → `noop`, file untouched);
member conflicts by `(name, kind)` (attr_reader/`writer name=`/accessor count, alias doesn't; instance never
blocks singleton); conflicting members skip as `user_authored` with `classification: tighter_return` +
`declared_return_rbs` (write-time return-text extraction); **equivalence-drop** (same return → silent, so re-runs
are idempotent `No changes`); class-not-in-file → compact `class A::B < Super` append; the scenario-4 nested
"indent quirk" reproduces EMERGENTLY from token-start splicing (replicated, not special-cased). **LayoutIndex**:
sorted `**/*.rbs` scan of signature dirs, FQN→file first-found-wins, per-candidate routing (consolidated file
first, 1:1 mirror fallback — one source's candidates can split). **Audited independently:** 545 tests (13 new),
harness 54/54 0 FP, clippy clean, **9/9 fresh-dir E2E scenarios byte-identical (stdout + written trees)**,
idempotence (`No changes` + byte-stable), JSON content-identical incl. skipped-entry fields, reversed-arg result
order. Documented divergences: JSON key order; malformed-scenario STDERR env warning (needs project-sig env at
generation — deferred); sound-superset re-insert of inherited methods (`Object#hash`) the reference's env-based
EQUIVALENT classification drops — the deferred generator slice. Design note:
[siggen-writer-update-design](notes/20260710-siggen-writer-update-design.md). NEXT: `--params=observed`
(ObservationCollector); generation-time env classification (closes the inherited-method excess + malformed
stderr); qualified source-class naming; `--overwrite` tighter-return replacement.

**▶▶ LANDED (branch `sig-gen-module-function`, MERGED `95f490d`) — sig-gen slice 8: `module_function` `self?.`
spelling.** Replaces the conservative whole-body skip with the reference's real semantics, turning
previously-SKIPPED methods into byte-identical emits: shared methods on `reference/lib` **102 → 108** (6 new
`self?.`), **0 mismatch**. Semantics (oracle-probed): a BARE `module_function` (no args) flips a running flag for
every SUBSEQUENT instance def in that body → `def self?.name` (reference `method_def_prefix` /
`@module_function_methods`); **position matters** (a def BEFORE the call stays plain instance); the ARGS form
(`module_function :sym`) does NOT flip the mode nor mark the method; it applies in a CLASS body too
(rule_catalog.rb). Kind stays `instance` — only the rbs prefix changes. Singleton prefix WINS over
module_function. Implemented by tracking `mf_active` in the existing ordered body walk (no new pass); lowering is
module_function-agnostic so post-mf defs stay Public and are not visibility-skipped. **Gated:** 530 tests (3
new), run.rb + run_snapshot.rb 54/54 0 FP, clippy clean; E2E BYTE-IDENTICAL on the full matrix (before/after,
args form, class body, singleton precedence) for `--print` AND `--write`; sweep 140 files: 108 shared / 0
mismatch. NEXT: Writer UPDATE/merge + `LayoutIndex` (existing-sig projects — the heavy remaining piece);
`--params=observed` (ObservationCollector); qualified source-class naming.

**▶▶ LANDED (branch `sig-gen-diff`, MERGED `968c10c`) — sig-gen slice 7: `--diff` mode.** The cheapest remaining
slice — a thin renderer over the SAME candidates `--print` produces, completing the reference's three output
modes (`--print` / `--diff` / `--write`). Per candidate: `--- <path>: <class>#<method>` / `+ <rbs>` / blank line
(reference `render_diff`); rigor-rs emits only NEW methods so never a `- def` declared line (the reference's
`new_method` shape). `--format json` renders the candidate table regardless of print/diff mode. **Gated:** 528
tests (1 new), harness 54/54 0 FP, clippy clean; fresh-dir E2E — `--diff` text BYTE-IDENTICAL, `--diff` json ==
`--print` json. NEXT: Writer UPDATE/merge + `LayoutIndex` (existing-sig projects — the heavy remaining piece);
`--params=observed` (ObservationCollector); qualified source-class naming; `module_function` `self?.` spelling.

**▶▶ LANDED (branch `sig-gen-initialize`, MERGED `25d82eb`) — sig-gen slice 6: `initialize -> void` stub.** A big
convergence win — nearly every class has a constructor; shared-method count on `reference/lib` jumped ~33 → 102
(70 of them `initialize`), **0 mismatch**. **rigor-parse**: a new `ParamShape` (required/optional counts, rest,
keyword `(name, optional)` list, kwrest, block) captured at lowering onto `Node::Definition` via `param_shape_of`
— additive, check path unaffected. **sig-gen**: `initialize` (instance) renders the reference `-> void` stub with
the FULL param shape as `untyped` in `render_initialize_param_list` order (`untyped, ?untyped, *untyped, name:
untyped, ?opt: untyped, **untyped, ?{ (?) -> void }`; posts omitted as the reference does); a trivial all-empty
`initialize` is EXCLUDED; checked BEFORE the `simple_parameter_shape` gate so kwargs/optional/splat constructors
emit; `def self.initialize` stays an ordinary singleton. **Gated:** 527 tests (4 new + matrix), run.rb +
run_snapshot.rb 54/54 0 FP, clippy clean, check-path smoke 0 FP (initialize-heavy), initialize param-matrix
BYTE-IDENTICAL vs the oracle, intersection sweep 140 files: 102 shared / 0 mismatch. NEXT: Writer UPDATE/merge +
`LayoutIndex` (consolidated-sig projects); `--params=observed` (ObservationCollector); qualified source-class
naming; `module_function` `self?.` spelling.

**▶▶ LANDED (branch `sig-gen-write`, MERGED `af4f42f`) — sig-gen slice 5: `--write` (create-only).** The user-facing
payoff — `--print` is advisory, `--write` actually generates the `sig/` tree. Ports the reference `Writer`'s
CREATE path: **PathMapper** (`lib/foo.rb` → `sig/foo.rbs`; strip `config.paths.first` basename, swap ext, place
under `config.signature_paths.first`) + **namespace-tree render** (group candidates by target, split `class_name`
on `::`, render `<keyword> <name><super?>` / body / `end` with `node_keyword` = recorded class/module kind else
leaf-with-methods→class else module, + superclass suffix — a new `NamespaceInfo` from a lightweight walk carries
per-segment kind + plain-constant superclass). **CREATE-ONLY, safe by construction**: writes only when the mirror
target is ABSENT; an existing target is `skipped_exists` (the Writer's merge / user-authored preservation +
`LayoutIndex` consolidated-file re-routing are deferred — **never corrupts or duplicates**). Verified rigor-rs
never over-writes a file the reference doesn't, never touches an existing file; on files both write the shared
method lines are byte-identical (rigor-rs may write a valid RBS SUBSET — `initialize` stub + less-precise
inference — the sound-superset model at FILE level). **Gated:** 525 tests (5 new), run.rb + run_snapshot.rb
54/54 0 FP, clippy clean; fresh-dir E2E vs the oracle — written sig TREE + text + JSON BYTE-IDENTICAL (flat
class+module, nested `module::class < super`, multi-file). NEXT: `initialize -> void` stub (needs full-param
lowering); the Writer's UPDATE/merge path + `LayoutIndex` (unlocks consolidated-sig projects); `--params=observed`
(ObservationCollector); qualified source-class naming; `module_function` `self?.` spelling.

**▶▶ LANDED (branch `sig-gen-singletons`, MERGED `8db1bed`) — sig-gen slice 4: singleton methods + module_function
safety.** Roughly DOUBLES emitted coverage on real code (the reference emits a large share of `def self.` /
`self?.`). **rigor-parse**: `Node::Definition` gains `singleton_name` — `Some(name)` for a `def self.x` SELF
receiver (whose name was otherwise LOST, only receiver-less defs kept a name); additive (all matches use `..`),
the instance harvest still skips singleton defs so check is unaffected. **sig-gen** collects instance + singleton
sigs in ONE source-ordered pass over the class body (`def self.x` via `singleton_name`; `class << self` inner
receiver-less defs), sorted by span so the two interleave in the reference's walk order; singletons render
`def self.NAME`, kind `singleton`, exempt from the visibility + `initialize` skips (both instance-only in the
reference). `module_function` now skips a CLASS body too (rule_catalog.rb uses it in a class → reference emits
`def self?.helper`; rs must not emit `def helper` — probed mismatch, fixed). **Gated:** 522 tests (5 new),
run.rb + run_snapshot.rb 54/54 0 FP, check-path smoke 0 FP over 20 singleton/module_function files, singleton
E2E byte-identical (incl. source-order interleaving), **class-aware intersection sweep ~210 reference/lib files:
0 rbs-mismatch on shared methods**. NEXT (still-skipped coverage): `module_function` `self?.` spelling, qualified
source-class naming, `TypeElaborator` generic fill, `initialize -> void` stub (needs full-param lowering); then
`--diff`/`--write` (Writer) → `--params=observed` (ObservationCollector).

**▶▶ LANDED (branch `sig-gen-return-union`, MERGED `929ff74`) — sig-gen slice 3: `DefReturnTyper` explicit-return
union + `Node::Return`.** The divergence-reducing slice the sound-superset decision named: port the reference's
return inference at the SOURCE. **rigor-parse gains a real `Node::Return` variant** (values fully lowered as
children) replacing the recovered-children fallthrough carrier — the typer's catch-all types it `Dynamic[top]`
exactly like the old carrier so the check path is behavior-preserving BY CONSTRUCTION, with one deliberate
improvement: `flow.dead-assignment` now fires on `return (x = 5)` byte-identically to the reference
(oracle-probed; the write now exists in the arena). **sig-gen unions `(tail, every collectible return)`** per
`DefReturnTyper` (oracle-probed matrix): bare `return`→`nil`; block/nested-def returns BARRIERED (reference
`RETURN_BARRIER_NODES`; span-containment over `Call::block_body` — the lambda barrier holds structurally, a
lambda never lowers a `Node::Return`); multi-value `return a, b` SKIPS the method (the reference silently drops
its type — an unsound emit not adopted); members sort by `describe(:short)` (reference
`Combinator#sort_members` — `("a" | 1 | :sym)` ordering byte-verified). Sweep adjudication added three
sanctioned guards: **module_function modules skip** (reference spells `def self?.name`), **any `untyped` inside
a member skips** (confidence rule — sweep-proven shared-method mismatch source, `Baseline#filter`), **nested
source-class instances skip** (reference emits fully-qualified names; TOP-LEVEL classes byte-match and emit).
**Gated:** 520 tests (7 new), run.rb + run_snapshot.rb 54/54 0 FP, check-path smoke 0 FP over 25 return-heavy
reference/lib files, return-matrix byte-identical, **class-aware intersection sweep: 0 rbs-mismatch on shared
methods**. NEXT sig-gen follow-ups (each unlocks skipped coverage): qualified source-class naming
(`Rigor::Plugin::ProtocolContract`), `module_function` `self?.` spelling, `TypeElaborator` generic fill,
`initialize -> void` stub, singleton methods; then `--diff`/`--write` (Writer) + `--params=observed`.

**▶▶ LANDED (branch `sig-gen-print-slice`, MERGED `7f01322`) — sig-gen slice 2: `rigor sig-gen --print`.**
The `--print` RBS-skeleton path over instance methods in a named `class`/`module` body, atop the landed
`erase_to_rbs` substrate. Walks each file, infers each qualifying method's RETURN via the shared `Typer`, and
prints `def name: (untyped, …) -> <erased>` grouped by file+class (reference `Generator`/`Renderer`). Reuses
`ClassDef`/`ModuleDef` `method_bodies`+`method_visibilities` and `MethodBody.params` (`None` == the reference's
non-simple param shape). **PARITY MODEL DECIDED (2026-07-10) — sound-superset**, chosen on the
minimize-long-term-divergence criterion (now a standing principle in **AGENTS.md "Generative-tool parity"**):
the HARD guarantee is byte-identity on the methods BOTH tools emit (gate: **0 rbs-mismatch across shared methods
on `reference/rigor/lib`**); the emitted SETS may differ by inference precision, and where rigor-rs is MORE
robust (string-interp / `%i[]` / project-class `.new` → its instance / partial-`untyped` shape) it emits a SOUND
signature the reference degrades-and-skips — that excess is COVERAGE we TRACK (the reference converges as it
gains precision), NOT encode with anti-convergence guards. The ONLY guards are the three sanctioned kinds: fix a
rigor-rs UNSOUND emit (`initialize`-as-body → skip), match a reference PERMANENT skip (`dynamic_top?`
whole-`untyped`), or avoid a WRONG emit from an unported rigor-rs LIMITATION (bare-generic → needs
`TypeElaborator`; explicit-`return` union → needs return exprs in the AST). **Gated:** 513 tests (12 new),
run.rb + run_snapshot.rb 54/54 0 FP (check untouched), clippy clean; extras spot-checked SOUND. **NEXT (most
divergence-reducing, per the principle): port `DefReturnTyper`'s explicit-`return` union at the source** (needs
rigor-parse to preserve `return E` exprs) so the SETS converge; then a sig-gen differential audit
(`fp_audit`-style) to keep over-emits visible + adjudicated. Deferred: `--diff`/`--write` (Writer),
`--params=observed` (ObservationCollector), singleton/attr/`Data.define`, tighter-return classification.

**▶▶ LANDED (branch `sig-gen-erase-substrate`, MERGED `ee60d41`) — sig-gen substrate slice 1: `erase_to_rbs`.**
The valid-RBS type-erasure layer `sig-gen` needs, built as a reusable substrate (mirroring how `describe_named`
landed before `annotate` consumed it). `rigor_types::erase_to_rbs_named` ports the reference's
`Type#erase_to_rbs` (`lib/rigor/type/*.rb`): distinct from `describe_named` (human display) — erasure
GENERALIZES the value-pins RBS cannot spell so output is always well-formed RBS: a non-primitive `Constant`→its
class name (`3.5`→`Float`), an `IntegerRange`→`Integer`, an open / non-symbol-keyed `HashShape`→`Hash[K, V]`, a
`Dynamic`→`untyped`; primitive pins are PRESERVED (`3`, `"hi"`, `:sym`, `[1, 2, 3]`, `{ a: 1 }`); unions dedup +
short-circuit to `untyped` with NO `bool`/`T?` collapse (that's display-only). Surfaced today through
**`type-of`'s `erased:` line** (text + json, previously omitted — the smallest gateable consumer) via the shared
`type_display::erase` seam. **Gated:** 502 workspace tests (11 new), run.rb + run_snapshot.rb 54/54 0 FP (check
path untouched), clippy clean on touched files; **fresh-dir E2E vs the oracle — `type:` + `erased:`
BYTE-IDENTICAL** across the scalar/tuple/record/hash-bound/float/dynamic matrix (incl. `Float`→`Float` and
`{ "k" => 2 }`→`Hash[String, 2]`). NEXT sig-gen slices (deferred, ~3000 LOC): the
`ObservationCollector` (per-def observed-call gathering) → `Generator`/`Writer` (RBS emission) — the heavy
machinery `erase_to_rbs` feeds; each a separate slice.

**▶▶ INVESTIGATED THIS SESSION (2026-07-10) — PURE-RBS BUNDLE TRACK CLOSED.** The plugin-engine design slice
recommended pure-RBS bundle expansion as the productization plugin path; a full enumeration of the reference's
31 plugins CLOSES that track: **`activesupport-core-ext` is the ONLY pure-RBS plugin** (all others contribute
code — ADR-16 macros / ADR-13 FactStore producers — the code engine already deferred as thin/interdependent),
and the vendored AS bundle is **byte-identical to the current reference** (`d31d19b0…`, so no refresh either).
The Gemfile.lock auto-overlay map already matches the reference's `GEM_OVERLAY_PLUGIN_IDS` exactly (only
`activesupport`). ⇒ No cheap FP-safe faithful-port plugin work remains. Fourth "big track, thin/absent value"
finding of the session. Detail: [pure-rbs-bundle-track-closed note](notes/20260710-pure-rbs-bundle-track-closed.md).
**Next real frontier is a substantial ADR-backed track (substrate for sig-gen/trace/coverage, the code engine
on a measured Rails gap, or §12 LSP two-tier infra) — none a cheap slice; the parity-port arc has bottomed out.**

**▶▶ LANDED (branch `mcp-triage-annotate`, MERGED `c6c1094`) — (2) MCP tool expansion: `triage` + `annotate`.**
`rigor mcp` gains two read-only tools reusing this session's landed commands: **`triage`** (analyse a source
string → the structured diagnostic triage JSON: distribution / selectors / hotspots / summary / hints, ADR-23
— the aggregate stats an agent uses to prioritise) and **`annotate`** (the `{ line => type }` map, xmpfilter
`#=> <type>` view). Exposed `triage::report_json_for` + `annotate::annotations_json` as the reusable seams;
both MCP handlers run the SAME suppression + disable filter as `check`, panic-isolated (`isError` on a bad
source). The MCP surface is now check / type_of / explain / outline / triage / annotate. **Gated:** 177
rigor-cli tests (3 new), end-to-end stdio smoke (initialize → tools/list → triage + annotate), harness 54/54,
clippy clean; the check path is untouched. NOT yet merged.

**▶▶ LANDED (branch `gemfile-lock-overlay`, MERGED `96d7f47`) — ADR-72 Gemfile.lock-gated auto-overlay (the
productization win the plugin investigation pointed to).** rigor-rs now AUTO-APPLIES the bundled
`activesupport-core-ext` RBS overlay when a project's `Gemfile.lock` locks `activesupport` (which ships no
RBS), so a Rails project "just works" WITHOUT a `plugins:` config entry — closing the systematic AS-method
`undefined-method` FP wall (`3.minutes`, `"x".squish`, `Object#blank?`). `crates/rigor-cli/src/bundler.rs`:
a line-oriented `Gemfile.lock` `GEM/specs:` parser (`locked_gems`, 4-space spec indent) + the
`GEM_OVERLAY_PLUGIN_IDS` map (`activesupport`->`activesupport-core-ext`, reference
`Environment::GEM_OVERLAY_PLUGIN_IDS`); `Config::effective_plugins(root)` unions `plugins:` + auto-detected
overlays (deduped), gated on the new `bundler.auto_detect` (default true). **FP-SAFE by construction** — the
overlay only ADDS signatures for a locked gem, so a real typo (`5.minuets`) still fires. **Verified:** E2E vs
the oracle on a Rails-like dir (activesupport in Gemfile.lock) — undefined-method stream BYTE-IDENTICAL (both
suppress AS methods, both fire the typo); real mastodon root (its actual Gemfile.lock) auto-suppresses the
AS-method FP wall. **Gated:** 490 tests, run.rb + run_snapshot.rb 54/54, corpus 320 files 0 FP (unchanged -
the harness/corpus have no Gemfile.lock, so `effective_plugins == plugins:`). NOT yet merged.

**▶▶ (1) POSSIBLE-NIL / IVAR EXPANSION — INVESTIGATED, CONFIRMED NET-NEGATIVE/ZERO-EV (2026-07-06).** Chased the
possible-nil source-expansion track and found rigor-rs is ALREADY at the FP-safe optimum; the residual is not a
gap to close. Evidence: (1) the existing nilable-local flow substrate is faithful — a live parity test shows
BOTH tools fire on the local nilable form (`x = s.byteslice(r); x.upcase`) and NEITHER on the direct chain, and
BOTH fire **0** possible-nil on the idiomatic node-field traversal (`current = current.next until
current.next.nil?`). (2) The reference's OWN ADR-58 tells the story: its ScopeIndexer ivar index types a node
field `Node | nil`, which MANUFACTURES 109 invariant-guarded FPs across the algorithm corpora, and ADR-58's
DECISION is a FP-SUPPRESSION policy (WD1: declaration-sourced nil is not diagnostic fuel) — WD2 (the precision
half) is recorded "corpus yield ~zero". rigor-rs reaches the identical FP-safe state for FREE by not typing
ivars, so ADDING ivar typing would re-introduce the 109 FPs unless the WD1 suppression is also ported, for ~zero
net coverage. Every other possible-nil source is likewise done-or-measured-zero: string/array slice + nilable-RBS
return LANDED; project-method nilable return is ADR-0041 (0 survey gaps, deferred); `T | nil` params need param
typing (ADR-5 keeps params lenient). ⇒ The possible-nil frontier is thoroughly tapped (confirming both the
flow-frontier note AND ADR-58's own measurement). **The genuine high-value remaining track is the Rails PLUGIN
ENGINE (ADR-0013/0027) — the biggest remaining undefined-method coverage pool.**

**▶▶ (c) REMAINING-COMMANDS ASSESSMENT (2026-07-06, investigated — DEFERRED as substrate-blocked).** The four
unported CLI commands each depend on substrate rigor-rs lacks or diverge structurally, so none is a clean
faithful port right now: **sig-gen** needs `erase_to_rbs` (conservative value-pin->nominal RBS erasure — a
sizeable substrate, distinct from the landed `describe_named`); **trace** needs a FallbackTracer (rigor-rs has
no dynamic-fallback-origin tracking — cf. the `type-of` note that omits `--trace`); **coverage** is ADR-63
protection coverage (large, mutation-engine-backed); **type-scan** reports per-node-class precision over the
AST, but rigor-rs's OWNED lowered AST differs structurally from Prism (different node counts AND names), so its
`visits`/coverage-% and node-class labels cannot byte-match the reference (it could ship as a rigor-rs-NATIVE
precision metric under the same documented divergence `type-of`'s node_kind carries, but not as a parity port).
NEXT high-value tracks instead (deeper inference / coverage, not more thin CLI ports): possible-nil source
expansion + the ivar/flow substrate (ADR-0022/58), or the Rails plugin engine (ADR-0013/0027) — the biggest
remaining undefined-method coverage pool.

**▶▶ LANDED THIS SESSION (branch `triage-hints`) — (b) triage hints Catalogue (portable subset).**
`rigor triage` now emits the reference `hints` section: the COUNT/rule-based recognisers H7 (unresolved-toplevel),
H5 (systemic single-file cluster, >=8 of one (file,rule)), H6 (low-count genuine bugs, rule total 1..=5) - in the
reference precedence order with per-diagnostic claiming (`recognise` in `triage.rs`). Default sections now include
`hints`. The ECOSYSTEM recognisers stay DEFERRED (H1 ActiveSupport, H2/H2K monkey-patch, H3 gem-without-rbs, H4
ActiveRecord relation) - they key on AS/AR method tables / `:info` notices / cross-file provenance / `Array[...]`
receivers rigor-rs doesn't produce, so they never fire on a rigor-rs diagnostic set (nor does the reference on the
same source), keeping the ported subset parity-faithful. **Verified:** fresh-dir E2E vs the oracle - default hints
BYTE-IDENTICAL (H7 claims toplevel calls + H6 the undefined-methods; H5 systemic at threshold). **Gated:** tests
(2 new), run.rb + run_snapshot.rb 54/54, clippy clean; check path untouched. NOT yet merged.

**▶▶ LANDED THIS SESSION (branch `case-union`) — (a) inference precision slice 3: case-expression union (completes if/case).**
A `Node::Case` arm in `Typer::type_of` (reference `type_of_case_simple_union`): a `case`/`when` (or `case`/`in`)
as an expression types to the union of its branch VALUES + the `else` value (or `nil` when no `else` - a
non-exhaustive case returns nil). Each `when`/`in` branch lowers to a `BeginRescue` carrier whose tail is the
branch value (resolved by the recursive `stmt_value_type`). A sound over-approximation of the reference's
`===`-certainty-narrowed variant (which only DROPS statically-impossible branches). Also: `describe_named`'s
union rendering now floats `nil` LAST (`10 | 20 | nil`, the reference's `T | … | nil` convention) in the
non-optional case. Byte-identical to the reference on case/when in `annotate`/`type-of`. **Gated:** 482 tests,
run.rb + run_snapshot.rb 54/54, corpus 400 files 0 FP. **(a) is now COMPLETE**: Tuple projection folds (#15),
if/unless/ternary unions (#16), case unions (this). NOT yet merged.

**▶▶ LANDED THIS SESSION (branch `ifcase-union`) — (a) inference precision slice 2: if/unless/ternary value typing.**
A new `Node::If` arm in `Typer::type_of` (reference `type_of_if`): an `if`/`unless`/ternary AS AN EXPRESSION
types to the union of its branch VALUES (each branch's tail; a missing `else` contributes `nil`) - `if c then 1
else 2 end`->`1 | 2`, `if e then 1 end`->`1?`. A KNOWN-polarity predicate elides the dead branch via
`predicate_polarity` (nil/false->falsey, any Nominal/shape/non-nil-non-false Constant->truthy, else->keep both;
never MORE aggressive than the reference, so it can only cost a witness, never add one): `if "x".upcase then a
end`->`a` (not `a | nil`). `branch_value_type`/`stmt_value_type` resolve a branch's tail through assignment
(->RHS) and the `BeginRescue`/`Statements` wrappers rigor-rs lowers an `else` into. Union receivers never
witness (`class_name_of`->None), so FP-safe. Byte-identical to the reference on if/unless/ternary in
`annotate`/`type-of`. **Gated:** 481 tests, run.rb + run_snapshot.rb 54/54, corpus 560 files 0 FP (matched
unchanged). `case`-as-expression union is the remaining sibling (deferred - needs per-When-branch descent).

**▶▶ LANDED THIS SESSION (branch `tuple-projection`) — (a) inference precision slice 1: Tuple projection folds.**
`Typer::fold_tuple_projection` (new Tier 2 in `type_call`, reference `ShapeDispatch`): a no-arg accessor or
constant-index read on a value-pinned `Tuple` folds to the pinned element/arity — `[1,2,3].first`->`1`,
`.last`->`3`, `.size`/`.length`/`.count`->`3`, `.empty?`->`false`, `[1,2][0]`->`1` (Ruby negative index; OOB->`nil`;
`[].first`->`nil`). Only block-free no-arg / single-constant-index forms fold (an arg-form `first(2)` declines to
the RBS `Array[Elem]` overload; `type_call` never sees block calls). Sharpens `type-of`/`annotate` and chained
witnessing (`[1,2].first.frist` flags on `1`) - byte-identical to the reference on the projection cases.
**Gated:** 480 tests, run.rb + run_snapshot.rb 54/54, corpus 400 files 0 FP (hot-path). Merged as PR (below).

**▶▶ LANDED THIS SESSION (branch `annotate`) — `rigor annotate FILE`.** A port of the reference's
`AnnotateCommand` + `LineTypeCollector`: appends a `#=> <type>` comment to each source line (xmpfilter
convention), rendering the type via the shared `describe_named` layer — the payoff of the type-display arc.
`crates/rigor-cli/src/annotate.rs`: per-line selection ported faithfully — every STATEMENT (a child of a
`Statements`/`Program` body OR a branch/loop/def/class body, which rigor-rs stores as flat `Vec<NodeId>`
fields — collected via `push_statement_children`) sets `by_line[end_line]`, processed in ascending `NodeId`
order so the outermost/last statement closing a line wins (the arena is lowered bottom-up ⇒ NodeId order ==
post-order); an assignment evaluates to its RHS value, a `def` to its `:name` symbol with a header-line
return-type override; a line no statement closes falls back to the widest expression ending there. Text
(column-aligned, idempotent re-annotation via `#=>` strip) + `--format json` (`{ line: type }` map).
**Verified:** fresh-dir E2E vs the oracle — **byte-identical (text AND json) on straight-line top-level code**
(literals, arrays, hashes, `def` return/symbol). On complex code the residual divergences are all rigor-rs
**inference-precision gaps, not annotate defects** (and sound — `Dynamic[top]` over-approximates): an
`if`-expression types `Dynamic` where the reference folds to its branch value, and `[1,2].first` is
`Dynamic` where the reference folds the Tuple projection to `1`. **DEFERRALS (documented):** per-scope local
typing inside method bodies (no `ScopeIndexer` — a def-LOCAL literal binding types `Dynamic` where the
reference's scope pins it; top-level is exact); colour output (`bat`/IRB highlighting — flags accepted,
plain output). **Gated:** 479 tests (5 new), run.rb 54/54, clippy clean; check path untouched (new command).
**NOT yet merged.**

**▶▶ LANDED THIS SESSION (branch `hash-shape`) — value-pinned HASH → HashShape typing (completes the
type-display arc).** The previous slice deferred hashes because rigor-rs's lowering flattens hash assocs to
a `[k, v, k, v]` element list. This adds an `all_assoc: bool` flag to `Node::HashLit` (set in the HashNode
lowering; `false` for a `**`splat element or a bare keyword-hash argument) so the typer can faithfully
re-pair `elements`. `Typer::hash_shape_or_hash` ports the reference `static_hash_shape_for`: every element
an assoc with a static Symbol/String key and no duplicate → a value-pinned `Type::HashShape` (`{ a: 1 }`,
`{}` → empty `HashShape{}`); a splat / dynamic-or-integer / duplicate key degrades to the bare `Hash`
nominal. `class_name_of(HashShape)→Hash` (already added) keeps witnessing intact. **Result:** a hash
receiver now renders `{ a: 1 }` / `{ "k": 2 }` / `{}` — byte-identical to the reference — in the check
message, `type-of`, and `triage` selectors, and value-pinning composes recursively (`{ a: 1, b: [1, 2] }`).
**Gated HARD:** 474 tests, run.rb + run_snapshot.rb 54/54, **0 FP across a 752-real-file corpus sweep**
(matched count unchanged ⇒ no coverage regression), hash + array E2E byte-identical to the reference.
**With arrays + hashes value-pinned + `describe_named`, `annotate` is now unblocked** (it renders each line's
type via `describe_named`). **NOT yet merged.**

**▶▶ LANDED THIS SESSION (PR #12, merged) — reference-faithful type-display layer + value-pinned
array typing.** Builds `rigor_types::describe_named` — a faithful port of the reference's
`Type#describe(:short)` (`lib/rigor/type/*.rb`) with class-name resolution (a `&dyn Fn(ClassId)->Option<String>`
resolver over core RBS + project `sig/`): `Nominal`→name (`Array[Integer]`), `Constant`→Ruby inspect
(`3`/`"hi"`/`:sym`), `Tuple`→value-pinned (`[1, 2, 3]`), optional `Union`→`T?`, `IntegerRange`→`int<…>`/alias,
HashShape→`{ k: v }`. **Routed both `type-of`'s `render_type` AND the check path's `render_receiver` through
it** — one shared reference-faithful vocabulary; this fixes `type-of` leaking `Class<id>` for composite
carriers (union/range/shape) that the low-level `describe` cannot name. **Value-pinned ARRAY typing**
(`crates/rigor-infer`): `[1,2,3]` now types as a `Tuple` (was `Nominal{Array}`; splat/`[]`/unsupported
degrade per the reference `array_type_for`), with a new `class_name_of` fallback (`Tuple`→Array,
`HashShape`→Hash, `IntegerRange`→Integer) that PRESERVES witnessing/dispatch (a typo still flags via the
real Array RBS). **Result:** an array receiver now renders `[1, 2, 3]` — byte-identical to the reference —
in the check message, `type-of`, AND `triage` selectors (the original triage-array divergence CLOSED).
**Gated HARD:** 474 tests, run.rb + run_snapshot.rb 54/54, **corpus 560 real files 0 FP** (hot-path change),
type-of E2E matches the reference on scalars/nominals/nested arrays. (HASH → HashShape typing followed on
branch `hash-shape` — see the entry above.) Merged as PR #12.

**▶▶ LANDED THIS SESSION (branch `triage-command`) — `rigor triage [paths]` statistical core (ADR-23).**
A faithful port of the reference's `Triage`/`TriageRenderer`/`TriageCommand`, SCOPED to the statistical
core: runs the same analysis as `check`, then summarises the stream — rule-id **distribution**, class/method
**selectors** (ADR-61 agent stats), per-file **hotspots**, + a **summary** — text or `--format json`, with
`--top N`/`--include-info`/`--no-hints`/`--selectors-only`/`--hints-only`/`--config`. Read-only, always
exits 0. `crates/rigor-cli/src/triage.rs` reuses `analyze_files` (sound subset) and ports
`normalize_receiver`/`qualified_rule`/the renderer verbatim. **DEFERRED: the `hints` Catalogue** (362-line
ecosystem heuristic — AS core-ext / AR relations / project monkey-patch / `gem-without-rbs` — tuned for a
full Rails run and partly keyed on `:info` plugin-recognition diagnostics rigor-rs doesn't emit); so
rigor-rs's default sections = `[distribution, selectors, hotspots]`, i.e. default output == reference
`triage --no-hints` (the parity gate), `hints` always empty. **Verified:** fresh-dir E2E vs the oracle —
default text **byte-identical** to `triage --no-hints` for all-scalar receivers (incl. `--selectors-only`,
`--top`); the ONE systematic divergence is that rigor-rs types an array/hash literal receiver as its nominal
class (`Array`/`Hash`) where the reference keeps the value-pinned tuple display (`[1, 2, 3]`) — rigor-rs's
tool-wide `receiver_type` spelling, not a triage defect (same convention `type-of` documents). JSON is
content-identical (serde_json alphabetizes keys — documented, diff precedent). 467 workspace tests, run.rb +
run_snapshot.rb 54/54, no new clippy lints. **NOT yet merged.** (Follow-on if pursued: the deferred hints
Catalogue; a `describe`/`erase_to_rbs` type-display layer would let the selector receiver match the
reference's tuple spelling AND unblock `annotate`/`sig-gen`.)

**▶▶ LANDED THIS SESSION (branch `diff-command`) — `rigor diff <baseline.json> [paths...]`.** A faithful
port of the reference's `DiffCommand` (`lib/rigor/cli/diff_command.rb`): compares the current `rigor check`
diagnostics against a saved `check --format=json` baseline and prints the **new** (regressions) / **fixed**
(progress) delta, exit **1** iff any new diagnostic appears (a CI regression gate — legacy errors in the
baseline don't fail, new ones do). A lighter-weight sibling of the ADR-22 baseline system (no
`.rigor-baseline.yml` — just two JSON snapshots). `crates/rigor-cli/src/diff.rs`: identity tuple
`(path, line, column, rule, source_family, message)`; `--format text|json`, `--current PATH` (compare a saved
JSON instead of a live run), `--config PATH`; `load_diagnostics` accepts both a bare array (rigor-rs) and a
`{diagnostics: […]}` object (reference). The current run reuses `analyze_files` in the Ruby-free SOUND SUBSET
(folder=None) — diagnostic-identical to full fidelity per ADR-0037, keeping `diff` dependency-free.
**Verified:** fresh-dir E2E vs the oracle (feeding both tools the same baseline + `--current` JSON to isolate
format from analyzer-coverage differences) — **text output byte-identical**, exit codes identical (1 new / 0
clean / 64 missing-file), JSON **content**-identical (only key ORDER differs — serde_json alphabetizes vs the
reference's insertion order; JSON key order is insignificant, documented). Live end-to-end path confirmed
(generate baseline → edit source → correct new/fixed split). 462 workspace tests, run.rb + run_snapshot.rb
54/54, no new clippy lints. **NOT yet merged.**

**▶▶ LANDED THIS SESSION (merged to master, PR #8) — `warn_unresolved_config` / `ConfigAudit`.**
A faithful port of the reference's config audit (`lib/rigor/config_audit.rb` + `check_command.rb`'s
`warn_unresolved_config`), surfacing configured values that silently resolve to nothing — the class of
mistake whose only symptom is downstream and confusing (a typo'd `signature_paths:` dir manufactures
hundreds of false `undefined-method`s; an inert `disable:` token leaves the rule firing as if unwritten).
`crates/rigor-cli/src/config_audit.rs` ports the applicable subset (rigor-rs's config lacks
`libraries:`/`bundler.*`/`severity_overrides:`): (1) **`signature_paths:`** entries resolving to nothing
(`:missing`/`:not_directory`/`:empty`), audited ONLY when explicitly configured (new `Config::present_keys` +
`explicit_signature_paths()` gate mirrors the reference's nil-when-unset — the implicit `["sig"]` default
never warns); (2) **`disable:`** inert built-in-family tokens via new `rigor_rules::is_inert_builtin_token`
(validated against the reference's FULL 19-id `ALL_CANONICAL_RULES`, not the narrower `IMPLEMENTED_RULES`, so
a recognized-but-unemitted rule is never mis-flagged); (3) explicit **`rbs_collection.lockfile`** that does
not exist. Emitted to STDERR as `rigor: <message>` before analysis (all formats); the JSON-payload
`config_warnings` half is DEFERRED (rigor-rs's JSON is a bare diagnostics *array*, an established shape
divergence). **Verified:** fresh-dir E2E vs the oracle — all message forms byte-identical EXCEPT
`signature_paths:` prints the RELATIVE configured string vs the reference's absolutized path (deliberate,
documented — rigor-rs's house style resolves/prints paths relative to cwd). 0-FP / harness-safe (stderr-only,
harness runs configless): run.rb + run_snapshot.rb 54/54, workspace tests green, clippy adds no new lints.

**▶▶ NEXT SESSION — START HERE: continue PRODUCTIZATION (the measurement-proven high-ROI track).**
Candidates, any of which suits the delegation model (main designs/audits; Sonnet investigates the
reference + probes the oracle; Opus implements on a branch; main byte-audits before merge):
- ✅ **`.rigor.yml` config-audit** (`warn_unresolved_config`) — LANDED (PR #8, merged; see above).
  Follow-ons if pursued: the deferred JSON `config_warnings` payload (needs a JSON top-level shape decision,
  since rigor-rs emits a bare array). NOTE: the reference does NOT warn on unknown keys — it ignores them
  silently, exactly as rigor-rs already does — so there is no "unknown-key" pass to add.
- **reference CLI commands** — `explain`, `type-of`, `diff`, `triage` (statistical core) DONE. The
  remaining ones are BLOCKED ON SUBSTRATE rigor-rs lacks, not quick ports: `annotate` + `sig-gen` need a
  `describe(:short)`/`erase_to_rbs` type-display layer (rigor-rs uses its own `render_type`, deliberately
  divergent — see `type-of`); `trace` needs a FallbackTracer; `type-scan` reports Prism node-class names
  (rigor-rs has owned node variants); `coverage` is ADR-63 protection (large, mutation). The single
  highest-leverage unlock is a **type-display layer** (`describe`/`erase_to_rbs`) — it would make the
  `triage` selector receivers match the reference's tuple spelling AND unblock `annotate`/`sig-gen`.
  Also open: the deferred `triage` **hints Catalogue** (ecosystem heuristics).
- **§12 LSP two-tier / MCP tool expansion** — larger; watched-files invalidation, debounce, worker pool;
  or more MCP tools.
Always: predict value with a valid-mode probe first, port faithfully (read reference + oracle),
gate with fresh-dir E2E parity.

**This session landed (master), newest first — detail in the linked ADRs/notes:**
- **ADR-22 baseline area COMPLETE** (merges `95564d4`, `ac4744f`): `regenerate`/`drift`/`prune` +
  `check --baseline-strict`, faithful ports built by delegated agents from two-way-verified oracle specs,
  each byte-audited before merge (22/22 then 9/9 E2E parity). Reusable `Baseline::{audit,without}` +
  `DriftStatus`/`DriftRow`.
- **`rigor check <dir>` directory support + config `paths:`** ([ADR-0040](adr/0040-directory-path-argument-support.md),
  merges through `9b61513`; audit fixes in `983e6ef`): recursive `**/*.rb` (skip hidden, symlinked FILES
  matched, no `.gitignore`, config `exclude:`), path-error diagnostics + warn-if-any-else-error severity,
  **error-severity-driven exit code** (a warning-only run exits 0; the synthetic `internal-error` still
  fails the run — audit #1), bare `check`/`baseline generate` scan config `paths:` (default `[lib]`).
- **flow substrate (possible-nil)**: [ADR-0038](adr/0038-flow-substrate-incremental-narrowing.md) threaded
  flow-eval (Slice 1a fires treemaps via the block-descent substrate); [ADR-0039](adr/0039-shape-typing-tier.md)
  shape-tier Slice 1a (`Array.new`-provenance) landed, `Type::Tuple` (1b) DEFERRED by measurement;
  [ADR-0041](adr/0041-project-method-nilable-return.md) project-method nilable-return DEFERRED by
  measurement (FP-safe, 0 survey gaps; code on branch `tier-bc-nilable-return`).
- **KEY STRATEGIC FINDING** ([flow-frontier note](notes/20260706-flow-frontier-exhausted.md), [[possible-nil-fold-gated]]):
  three consecutive FP-safe flow slices closed 0 survey gaps → the possible-nil/always-truthy frontier has
  **no cheap FP-safe wins left** (residual = param-dependent return typing, AS RBS, project-class arms, ivar
  whole-class flow, loop narrowing — deep, opt-in, ADR-backed). Productization is the default.
- Audit notes this session: [adr0038-slice1-audit](notes/20260706-adr0038-slice1-audit.md),
  [adr0039-shape-tier-audit](notes/20260706-adr0039-shape-tier-audit.md),
  [slice1-array-fold-blocker](notes/20260706-slice1-array-fold-blocker.md),
  [productization-audit](notes/20260706-productization-audit.md).

(Deep flow clusters — Tier B/C possible-nil, always-truthy on the substrate, Rails/AS plugin — are
enumerated in the flow-frontier note above; each is opt-in and ADR-backed, NOT the default next work.)

Prior: 2026-07-06 — **Coverage-gap track opened (branch `coverage-gaps`).** Added `fp_audit.py --gaps`
(aggregates reference-only diagnostics by rule = the coverage-effort map). Landscape across the survey:
`call.undefined-method` (~109), `call.possible-nil-receiver` (~118), `flow.always-truthy-condition` (~117)
dominate — the top two need the ADR-0022 nil/flow substrate, and `call.argument-type-mismatch` (~30) is an
unimplemented rule; these are feature-scale, not easy. The tractable win taken: **parenthesized receivers**
— `(15).frobnicate` was silent because `(e)` lowered to a Dynamic block wrapper; a single-statement parens
now UNWRAPS to its inner node (`(e)` ≡ `e`), so the receiver types precisely. Closed ~13 undefined-method
gaps, 0 FP, harness 53/53. Commit `b98c658`. **Remaining coverage frontier is substantial** (flow/nil
substrate for the ~235 always-truthy+possible-nil gaps; the argument-type-mismatch rule; the plugin engine).

Prior: 2026-07-06 — **FP-audit sweep completed (branch `fp-audit-sweep`).** Extended the audit to
the full `rigor-survey` library set (redmine, concurrent-ruby, parser, haml/hamlit, ox, pycall, rbnacl,
mangrove, jbuilder, dependabot-core/common, erubi, … + the earlier 12). One new FP class fixed:
**ERB-template `.rb` files** (Rails generator `templates/*.rb` using `<%= … %>`) — Prism error-recovery
over the non-Ruby source yielded a garbage AST that over-fired `unresolved-toplevel`/`undefined-method`
(~58 FPs in jbuilder + redmine templates). `rigor_parse::looks_like_erb_template` (byte-level `%>`,
mirroring the reference's `ErbTemplateDetector` EXACTLY) now skips such files in the check + LSP paths.
**Result: 0 FP across the entire surveyed library corpus** (~4000+ files, 20+ libs). Remaining non-zero
audits were artifacts, not rigor-rs bugs: the reference batch-aborts on some test/ dirs (erubi/test) →
the tool SKIPs (comparison invalid); auditing lib/ dirs is clean. Guarded by a rigor-parse unit test +
corpus fixture 41. harness 53/53 (41 fixtures); cargo test + CI clippy clean. Commit `00c8734`.

Prior: 2026-07-05 — **Real-corpus FP audit (branch `sidecar-perf`).** New tool `harness/fp_audit.py`
diffs rigor-rs vs the reference on real projects (`rigor-survey`), reporting rigor-rs-only diagnostics
(zero-FP-bar violations). Validated **0 FP on mastodon/app/models** (248 files) at the outset, then the
audit across the wider corpus surfaced and fixed **four real FP clusters**: (1) `call.unresolved-toplevel`
inside `class << X` singleton-class bodies (added an `is_singleton_class` AST discriminator; those bodies
are class scopes) — net-ssh/algorithms; (2) `call.unresolved-toplevel` on RubyGems' `Kernel#gem` (a
runtime-injected method the vendored RBS omits; small FP-safe allowlist mirroring the reference's runtime
reflection) — net-ssh; (3) `call.undefined-method` on `Regexp.compile` (a singleton alias whose target
`new` is `Class#new`, a base method the alias resolver didn't consult) — algorithms; (4)
`flow.dead-assignment` on a `def local.m` singleton-def receiver (the receiver was dropped in lowering, so
the local's read was invisible) — textbringer. **Comprehensive audit now: 0 FP across 12 validly-comparable
corpora (~1750 files)** (erubi skipped — the reference aborts its 142-file batch; the tool was hardened to
detect reference failure and skip rather than report false FPs). Each fix guarded by a unit test / corpus
fixture; harness 53/53; cargo test + CI clippy clean. Also on this branch: [ADR-0037](adr/0037-sidecar-perf-slices-retired-by-measurement.md)
(perf slices retired by measurement). Commits `6654cb1`(ADR-0037) `34957a8` `6ad8225` `a543289` `040c5d5`.

Prior: 2026-07-05 — **[ADR-0036](adr/0036-ruby-sidecar-default-reversal.md) — DESIGN ACCEPTED (grill
session), implementation phased.** Reverses ADR-0008's polarity **before any production-ready
announcement** (BC-free window): **full fidelity (Ruby sidecar) is the default and product identity; the
Ruby-free sound subset is an explicit opt-in.** Coverage-posture axis: `--ruby=require|auto|off|<path>`
(+ `--no-ruby`), env `RIGOR_RUBY`/`RIGOR_NO_RUBY`, `.rigor.yml` `rigor_rs: { ruby: }`; precedence
CLI>env>file>default. Default `require` for one-shot commands (hard error, exit **69** EX_UNAVAILABLE
when the sidecar handshake fails), `auto` for `rigor lsp` (never breaks the editor; posture surfaced).
`--ruby` overloads keyword-or-path (path ⇒ require + hard-error-if-unusable; makes the off+path
contradiction unexpressible); same-layer double-spec = usage error (64). rigor-rs-specific config lives
under a new `rigor_rs:` namespace (transparent to the reference, which ignores unknown keys). New
glossary terms in CONTEXT.md: **sound subset / full fidelity / coverage posture**; `Ruby sidecar`
redefined optional→default. **Phasing (a):** ship the flag/env/config surface + an interim "sidecar not
yet implemented — running sound subset" posture notice NOW (converts today's *silent* subset into a
*disclosed* one, freezes the vocabulary); the exit-69 hard-error teeth + real full fidelity land WITH the
sidecar (still unimplemented). **Phase-a surface IMPLEMENTED** (`crates/rigor-cli/src/ruby_mode.rs`):
`--ruby=require|auto|off|<path>` + `--no-ruby` + `RIGOR_RUBY`/`RIGOR_NO_RUBY` + `.rigor.yml` `rigor_rs.ruby`,
layered resolution (CLI>env>file>default) with same-layer mutual-exclusion → exit 64; `check` emits the
one-time interim "sidecar not yet implemented → sound subset" stderr notice (silent under `off`); `doctor`
prints a coverage-posture line (WARN when reduced / PASS when opted out); `rigor lsp` defaults `auto` and
surfaces posture via `window/showMessage`. Diagnostic stdout is byte-identical (notice is stderr-only) —
harness stays 53/53 / 0 FP; `cargo test` + CI clippy clean. **Remaining (phase b, with the sidecar):** the
exit-69 hard error, the handshake probe, real full fidelity, and a `--format json` posture field.
See [ADR-0036](adr/0036-ruby-sidecar-default-reversal.md).

**Phase b / sidecar — Slice 1 LANDED (branch `ruby-sidecar`).** transport + handshake + availability
probe. `crates/rigor-cli/src/sidecar.rb` (embedded via `include_str!`, newline-delimited JSON — ADR-0008
v1 transport, MessagePack deferred to the batching slice) + `sidecar.rs` client: spawn the ruby, read the
`{rigor_sidecar,ruby_version}` handshake, exchange one `ping`, timeout-guarded (5s, worker-thread), child
always killed. `ruby_bin_for(mode)` selects the binary (`ruby` on PATH / `<path>`; project-Ruby/bundler
detection is a later slice). Wired into `rigor doctor` only: reports sidecar reachability + ruby version
(a real ADR-0036 first-class posture check). Tests incl. a real-ruby handshake+ping (skipped when no
ruby). **Sequencing refinement (deliberate):** the exit-69 hard error for `require` is HELD to Slice 2,
not wired now — hard-failing (or spawning ruby on) every default `check` before folding is routed would
be premature blast radius with no fidelity gain yet; the probe is built + tested + surfaced in `doctor`,
and the teeth flip on in Slice 2 when a reachable sidecar actually delivers full fidelity. `check`/`lsp`
hot paths unchanged (still the interim notice); harness stays 53/53 / 0 FP. **Slice 2 LANDED:** persistent worker + fold routing + exit-69 teeth. `sidecar.rb` gains a `fold` op
(tagged-scalar JSON ⇄ real Ruby value, purity-gated + rescue-guarded); `Sidecar` is now a persistent
worker (`fold`/`ping`, dead-pipe short-circuit, `Drop` shutdown) + `SidecarFolder` (`Mutex`+memo, `Sync`)
implementing the new `rigor_infer::RubyFolder` trait. rigor-infer: `folding::sidecar_foldable` allowlist
(parity-confirmed subset — `Integer#to_s(base)`, `String#%`) + `scalar_class`; `Typer` gains an optional
folder; `type_call` tier-1 falls back to it when the Rust core declines. rigor-rules:
`analyze_with_source_and_folder`. CLI: `build_sidecar_folder` (shared by `check` + `baseline`) resolves
the mode, spawns the sidecar, and **`require`/`<path>` unavailable → exit 69** (`auto` degrades+discloses,
`off` silent); `lsp` wires the folder too (never hard-errors — always `auto`-degrades); `doctor` reports
real reachability. The phase-a interim notice is retired (sidecar is real now). **Verified:** E2E vs the
reference — `255.to_s(16).frobnicate` witnesses on the folded `"ff"` identically (full fidelity);
`--no-ruby` stays sound-subset; exit 69 on require+no-ruby; harness **53/53 / 0 FP** with folding active
(default check now spawns a sidecar); a deterministic mock-folder infer test guards the tier-1 wiring;
real-ruby fold round-trip test; `cargo test` + CI clippy clean. **Allowlist expanded** (`53f652d`): +`Integer#gcd`, `Float#round`, `String#center/ljust/rjust/tr/sub/strip`
— each reference-verified (real Ruby both sides ⇒ parity-safe by construction). **Sidecar is now
FUNCTIONALLY COMPLETE** (spawn · fold routing · exit-69 teeth · posture disclosure · growing fidelity),
all gated. **MEASURED on `rigor-survey` (branch `sidecar-perf`, [ADR-0037](adr/0037-sidecar-perf-slices-retired-by-measurement.md)):**
full-vs-`--no-ruby` delta is FLAT ~0.06s across 55→548 files (fixed spawn cost, NOT per-call IPC — folds
fire only on rare pinned literals), and the diagnostic set is IDENTICAL full vs subset on every corpus
(mastodon 109=109, algorithms 1561=1561, kramdown/liquid 0=0). **⇒ Slice 3 (batching+MessagePack) + Slice
4 (on-disk cache) RETIRED** (optimize a non-bottleneck). **Slice 5 (plugin invocation) is NOT a sidecar
slice:** rigor-rs's plugin model is RBS-bundles-only today — the code-contribution surface
(`node_rule`/`dynamic_return`/`type_specifier`, ADR-0013/0027) + sidecar-hosted plugins are UNBUILT, so it
is gated on building rigor-rs's whole plugin ENGINE (a major separate track). **Net: the Ruby sidecar is
at a natural completion** (spawn · fold · exit-69 teeth · posture, all gated); constant folding is
precision-additive on rare literal constructs + the substrate a future plugin engine reuses. Next real
frontier = the plugin engine OR real-scale FP validation vs the reference on the corpora — both larger,
separate tracks. `ruby-sidecar` merged (`2aa5ce6`); perf finding on `sidecar-perf`.

Prior: 2026-07-05 — **[ADR-0034](adr/0034-rbs-collection-ingestion.md) — IMPLEMENTED.** The gem-RBS
leg's Ruby-free half now ships: `rbs collection` discovery (`crates/rigor-cli/src/rbs_collection.rs`) — a
pure filesystem+YAML port of the reference's `RbsCollectionDiscovery` (native `serde_yaml`, no bundler,
no network) resolves `rbs_collection.lock.yaml` (config `rbs_collection.auto_detect` default `true` +
optional `lockfile`), walks `.gem_rbs_collection/<name>/<version>/`, skips `stdlib`-source entries, and
feeds the dirs through the SAME ADR-0033 ingestion so collection gem classes gain project-sig provenance
and are WITNESSED — matching the reference (empirically: it attributes a collection gem to the
signature-path tier and fires `call.undefined-method` on `Mygem.new.typo`). `Config::all_signature_dirs`
concatenates `signature_paths:` + discovered collection dirs; all four index builders use it. **Verified:**
E2E differential vs the reference (collection `.new` typo witnessed identically) + a new corpus fixture
`39_rbs_collection_new` (harness `.collection/` staging, ADR-0033 fixture-env pattern) — live +
reference-free gates now **53/53 / 0 FP**; `cargo test` + CI clippy clean; discovery internals unit-tested.
**Deferred (parity-safe — coverage-gap only, never FP):** the bundler-installed-gem `sig/` leg (needs
gem-path discovery, Ruby-free tension) and inline RBS (ADR-0007's fourth leg).

**[ADR-0035](adr/0035-inline-rbs-deferred.md) — inline RBS DEFERRED (decision recorded).** The fourth
ADR-0007 leg is opt-in in the reference (the `rigor-rbs-inline` plugin, ADR-32 / `--treat-all-as-inline-rbs`),
not the default env — so deferring is parity-safe (the corpus never enables it; coverage-gap only). No
Ruby-free parse path exists: the reference plugin delegates to the `rbs-inline` gem's
`RBS::Inline::Parser`, and `ruby-rbs` parses `.rbs` files, not the rbs-inline comment sub-language — a
faithful port means reimplementing that grammar in Rust (large, standalone), and rigor-rs has no
source-parsing plugin surface (its plugin model is bundled-RBS only). Staged plan in the ADR (WD1
contribution mechanism → WD2 minimal `#:` method-sig slice → WD3 `# @rbs` long tail), demand-gated.
**With this, every ADR-0007 leg is resolved:** embedded stdlib + project `sig/` + `rbs_collection`
IMPLEMENTED; bundler-installed-gem `sig/` (ADR-0034) + inline RBS (ADR-0035) deferred with rationale.

Prior: 2026-07-05 — **Upstream pin bumped `v0.2.6` → `v0.2.7`** (`reference/rigor` @ `47c1c7d3`,
[`UPSTREAM.md`](../UPSTREAM.md)). Re-baselined: live differential 50/50 (0 FP), snapshots byte-identical
(0 written), reference-free gate PASS — no observable parity drift on the current corpus. v0.2.7's
parity-relevant core change is the RBS-loader stability fix (a malformed project `.rbs` no longer
collapses the whole env to `Dynamic[Top]` via `DuplicatedDeclarationError`); the rest of the release is
skills/docs/plugins (out of port scope) plus a dead-method removal in `statement_evaluator`. v0.2.7 now
bundles rbs-4.0.3, matching our vendored pin exactly. **Follow-up investigated → nothing to port:** the
loader's "skip already-declared namespaces when stubbing missing types" guard has no rigor-rs analogue.
The hazard is structurally absent on three independent grounds — (1) rigor-rs never ingests project
`sig/*.rbs` (the index is vendored core+stdlib+plugin RBS only, all well-formed; no config key or code
path reads a project `sig/`), so the malformed-project-.rbs trigger never occurs; (2) there is no
stub-missing-referenced-types / synthesize-missing-namespaces path at all (dispatch is lenient by
construction — unknown class ⇒ Dynamic — so nothing re-declares a name); (3) `Builder::merge`
(`crates/rigor-index/src/rbs.rs`) unions by name key with NO class/module kind concept and raises no
error, and there is no `resolve_type_names`-style global validation that could collapse the env to nil
(per-file parse failures are isolated, ADR-0016). This is exactly the deliberate divergence ADR-79
records. **[ADR-0033](adr/0033-project-sig-ingestion.md) — IMPLEMENTED (2026-07-05).** The ADR-0007
project-`sig/` ingestion leg now ships, Ruby-free (native `ruby-rbs`), in two cohesive slices: (1)
**ingestion** — a `signature_paths:` config key (default `["sig"]`) whose dirs feed the existing
`ingest_rbs_dir` into the same reopen-union `Builder`; all four production callers use
`CoreIndex::for_project`. (2) **witnessing** — project-sig classes carry provenance
(`CoreData::is_project_sig_class`), so `call.undefined-method` witnesses an `X.new` instance typo on a
project-sig class (`Widget.new.spni`) exactly as the reference does, while a bundled stdlib/gem class
(`Pathname.new.typo`) and an in-source-only class stay lenient — the provenance gate keeps the two
apart. Bare-constant/singleton witnessing (`Widget.spni`) fell out of the ingestion slice alone.
**Verified:** E2E differential vs the reference across 7 receiver shapes (core / stdlib / project-sig ×
direct/var/valid/singleton) — full parity; `cargo test` + CI clippy clean; the corpus differential
harness now covers it: a per-fixture `corpus/NN_name.sig/` convention stages a `sig/` copy into each
tool's cwd so the default `signature_paths: ["sig"]` ingests it symmetrically (`harness/lib.rb`
`sig_dir`/`stage_sig_into`). Two fixtures added — `37_project_sig_new` (witnesses the `.new` typo, +2
matched) and `38_project_sig_negatives` (valid calls + `Pathname` leniency, 0 diags). Live + reference-
free gates now **52/52 / 0 FP**; snapshots committed. So project-sig parity is guarded by the automated
corpus gate AND unit tests (`rigor-index` + `rigor-rules`). Not yet ported: gem RBS / `rbs_collection` /
inline RBS (separate ADR-0007 legs).

Prior: 2026-07-01 — **Productization track (lever A): 6 commits pushed (@ `8c3dbee`) + 4
uncommitted polish commits (@ `28592fb`).** (1) §9 **rayon file-level parallelism** (byte-identical
to serial, 0 FP, ~2.4× warm) + `RIGOR_TIMING` observability. (2) §12 **LSP server** — `rigor lsp
--transport=stdio` (sync `lsp-server`/`lsp-types`, no async runtime): live diagnostics +
**node-aware hover** (Call signature / def header / constant) + member-access **completion** +
**documentSymbol** outline. (3) §12 **MCP server** — `rigor mcp` (hand-rolled newline-delimited
JSON-RPC, no new dep): read-only `check` / `type_of` / `explain` / `outline` tools. The
LSP `documentSymbol` + MCP `outline` share one `outline::build` nesting builder. 402 tests +
end-to-end stdio smokes green. Prior: **5 commits pushed (@ `2d0add3`)**: rustfmt policy
(ADR-0032) · `flow.always-truthy-condition` + first ADR-0022 flow substrate · **upstream pinned to
`v0.2.6`** as a `reference/rigor` git submodule + harness re-baselined ([`UPSTREAM.md`](../UPSTREAM.md)) ·
**`call.unresolved-toplevel`** (the highest-frequency unimplemented rule per a v0.2.6 corpus tally, 0 FP) ·
a `flow.dead-assignment` block-pass (`&x`) FP fix. 369 tests, corpus 0 FP. **Net-new-rule coverage is
now exhausted** — see "▶▶ NEXT SESSION — START HERE" for the ranked next levers.
Prior: 2026-06-30 (rustfmt stance recorded — ADR-0032). 2026-06-27 (v0.0.1 release prep; AGPL-3.0 relicense; MSRV→1.88 CI fix).
See "▶ Resume here" for the release-tag steps + the recorded next work (musl/Windows targets; quality management).

> **2026-06-26 correctness finding (this session).** The reference does **not**
> witness `call.undefined-method` on a **project-defined (in-source) class
> instance**, nor on a **non-core `X.new` instance** (`Pathname`/`Set`/`Struct`).
> It gates the rule on `rbs_class_known?(class_name)` (`check_rules.rb:556`) and
> treats a miss there **leniently** (ADR-0023 tier-4: "on a miss, the call stays
> `Dynamic`"). The prior tier-4 implementation **witnessed** those — a systematic
> divergence the narrow corpus never surfaced. A broad 1444-file sweep exposed it
> (2 FPs: `Struct.new(...).new`, `Alba::Resource#to_h`). **Fix:** the rule now
> witnesses **only** receivers whose concrete class is RBS-known in the **core
> surface** (literals, RBS-method returns, core `X.new` like `Array.new`); the
> in-source/registry surface types instances for chaining but is never a
> *witnessing* surface. Result: 0 FP, **matched coverage unchanged** (every real
> match was already a core/RBS receiver). Cross-file in-source *instance*
> witnessing is therefore **not** a coverage lever.
>
> **Coverage work that followed (same session):** a data-driven gap analysis drove
> three zero-FP wins. (1) **Lowering traversal** — `KeywordHashNode` (`f(k: 30.minutes)`)
> and `ParenthesesNode` (`(30.seconds)..(10.minutes)`) weren't lowered, so nested
> calls escaped the walk; +54 matched. (2) **Interpolated strings/heredocs** now
> type as `String` (always sound). (3) **Class-method (singleton) witnessing** —
> `Time.current` → `singleton(Time)`. The reference witnesses class-method typos on
> ALL top-level RBS classes; rigor-rs now matches via a new `Type::Singleton(ClassId)`
> + `CoreIndex::class_has_singleton_method` (extend-aware, singleton-alias-aware,
> conservative). **Singleton needs cross-file:** a bare constant types to `Singleton`
> only when it's a genuine top-level RBS class (`knows_toplevel_class`) AND not
> defined anywhere in the PROJECT (`!source.knows_class`, via a project-wide
> `SourceIndex::build_project` the CLI builds once) — this is what stops a project
> model `Group`/`Report`/`Status` (name-colliding with a stdlib class) from being
> falsely witnessed. Three FP families found+fixed along the way (extend modules,
> namespaced short-name collisions, singleton aliases). Also a pre-existing
> **block-call** FP class fixed: a block-bearing call (`h.select { }`) was first made
> conservative (Dynamic), then (same date) **recovered to its block-overload RBS return**
> — `h.select { } : Hash`, `arr.map { } : Array`, `x.tap { } : x` — so chained witnesses
> fire again with 0 FP (see §4 "RECOVERED"); block-call ARITY is still deferred (silent).

## Legend

- ✅ done (working + tested/parity-checked) · 🟡 partial / stub · ⬜ not started

The hard rule for every increment: **zero false positives**. The differential
harnesses (`harness/run.rb`, `harness/run_corpus.rb`) fail if rigor-rs emits a
diagnostic the reference does not. Coverage grows; it never regresses into guessing.

---

## ▶ Resume here (next session)

> **▶▶ v0.0.1 RELEASE PREP (2026-06-27) — release-ready; awaiting the tag + infra.**
> The first release is **v0.0.1** (version bumped 0.1.0 → **0.0.1** across the single
> source `[workspace.package]` + the gem `version.rb` + the Homebrew formula; `rake
> version:check` green; `rigor --version` → `rigor 0.0.1`). `CHANGELOG.md` records the
> 0.0.1 surface. The release/gem/Homebrew CI is wired and tag-triggered. **To cut the
> release, the maintainer does the infra steps the local toolchain cannot:** (1) publish
> the GitHub repo + set the real `repository` URL (currently the placeholder
> `rigortype/rigor-rs`); (2) tag `v0.0.1` (or `v0.0.1-rc1` first) to run the cross-compile
> matrix + asset upload; (3) for the gem channel, a RubyGems account + `RUBYGEMS_API_KEY`
> secret (+ MFA); (4) for Homebrew, the `rigortype/homebrew-tap` repo + `HOMEBREW_TAP_TOKEN`.
> All push/publish CI steps are gated behind those secrets + a manual `release` environment,
> so they never auto-fire before the infra exists.
>
> **▶▶ NEXT WORK (recorded 2026-06-27, to pick up after v0.0.1) — two tracks the maintainer
> wants tackled incrementally:**
> 1. **Distribution slice 4 — musl + Windows targets** (§13). ✅ **WIRED (2026-06-27), PENDING CI
>    VALIDATION.** `release.yml`'s build matrix now has `x86_64`/`aarch64-unknown-linux-musl` (via
>    **`cargo-zigbuild`** — zig supplies the musl C cross-toolchain, bindgen runs on the host) and
>    `x86_64-pc-windows-msvc` (native windows runner; `.exe` + `.zip` packaging; `LIBCLANG_PATH`
>    for bindgen). The gem `Gem::Platform` map gained `x86_64-linux-musl` + `aarch64-linux-musl`
>    (verified normalizations). The binstall metadata gained a Windows `pkg-fmt = "zip"` override.
>    **DEFERRED-BY-DESIGN:** the Windows gem (`x64-mingw-ucrt` mingw platform — lower-value than the
>    binstall/`.zip` channel) and any Homebrew musl/Windows block (Homebrew Linux is glibc-only, no
>    Windows). **STILL NEEDS A REAL CI TAG RUN** to validate the cc+bindgen musl/MSVC cross/native
>    builds + `.zip` packaging + asset upload (no local cross-toolchain) — same caveat as the
>    existing release targets.
> 2. **Quality management (品質管理)** (§14). (a) ✅ DONE (2026-06-27) — workspace is
>    clippy-clean and `ci.yml`'s clippy step is now BLOCKING (`-D warnings`, `continue-on-error`
>    removed). The ~48 warnings were cleared behavior-preserving: doc-comment formatting (10),
>    `let_and_return`/`question_mark`/`double_comparisons` (3) FIXED inline; `collapsible_match`
>    (1 fn, 3 sites) + `too_many_arguments` (3) + `type_complexity` (1) carry surgical per-item
>    `#[allow]`s with rationale. The 29 `collapsible_if`s were NOT collapsed: their only fix is
>    let-chains (Rust 1.88+), and our own crates stay at older idioms — so a `clippy.toml` holds
>    clippy's suggestion-MSRV at `msrv = "1.85"` (below the build floor), which makes clippy stop
>    proposing them (they vanish, no allow needed). **(Build MSRV note:** the workspace build floor
>    is actually **1.88**, forced by the `ruby-rbs` dependency's own let-chains — Cargo.toml
>    `rust-version` + the CI toolchain pins are 1.88; clippy's suggestion-MSRV stays 1.85 for OUR
>    code. CI failed once on this mismatch — `ruby-rbs` cannot compile on 1.85 — and was fixed by
>    raising the pins to 1.88.)
>    All 352 tests + harness (0 FP) + corpus (0 FP) stay green. (b) ✅ DONE (2026-06-30) —
>    the **rustfmt** stance is now a recorded decision (ADR-0032): the codebase stays
>    **hand-formatted** and `cargo fmt --check` is NOT a CI gate. Adopting `cargo fmt` was
>    rejected (it rewrites 239 hunks across 25 files, erasing the compact hand style for no
>    parity/correctness gain); tuning `rustfmt.toml` to PRESERVE the style was found infeasible
>    (`use_small_heuristics = "Max"` only moved 239 → 222 and introduced opposite-direction
>    diffs — the hand style round-trips through no single stable config; some deviations need
>    unstable nightly options). Artifacts: `docs/adr/0032-source-formatting-policy.md`, a
>    documenting `rustfmt.toml` (loud "do not run cargo fmt" header + `edition`/`max_width` pin),
>    and a strengthened `ci.yml` header comment pointing at the ADR. No source/code change; clippy
>    stays the blocking style gate. (c) ✅ DONE (2026-06-27) —
>    **Snapshot-mode CI parity** (ADR-0002, §14). Shared harness logic extracted to `harness/lib.rb`;
>    `harness/snapshot.rb` regenerates `harness/snapshots/NN_name.json` (36 fixtures) from the live
>    reference — the reference's pinned `(rule,line,column,severity,message)` set, sorted/pretty so
>    regeneration is a no-op. `harness/run_snapshot.rb` is the reference-FREE gate: it loads the
>    snapshots, runs the binary, and applies the IDENTICAL `(rule,line,column)` comparison (FP fail,
>    missing OK, registry honored). A new `parity` job in `ci.yml` (checkout → toolchain → build
>    `rigor-cli` → setup-ruby → `ruby harness/run_snapshot.rb`) enforces zero-FP on every PR without
>    the Ruby reference. Verified snapshot-mode == live-mode (28 matched / 0 FP / 12 missing, identical
>    per-fixture) and reference-independence (passes with `REFERENCE_RIGOR_DIR` pointed at a nonexistent
>    path). The live `harness/run.rb` stays the local source-of-truth that regenerates the snapshots.
>
> Both are independent of each other and of the release; pull either next.
>
> **▶▶ DONE (2026-07-01) — `flow.always-truthy-condition` + the first ADR-0022 flow substrate.**
> The `flow.*` family's inferred-constant rule landed (§5), built on a NEW minimal flow-sensitive
> **local constant-propagation** pass (`Typer::always_truthy_snapshots`, `rigor-infer`): straight-line
> binds + real `if`/`unless` branch JOINS + loop/block/`case`/`begin`/`&&`-`||` widening
> (span-containment, orphan-proof). The join is the zero-FP keystone — `x=5; if c; x=f; end; if x`
> widens `x` and does NOT fire (the flat env's central unsoundness). A strict under-approximation of
> the reference folder (witness ⊆ reference). Verified byte-exact vs the oracle on the positives;
> **0 always-truthy fires across the full ~3800-file corpus** (like `unreachable-branch`, the
> inferred-constant pattern is vanishingly rare in real code — the value is the complete rule + the
> reusable substrate). +11 tests (363 total), live + snapshot harness PASS (34 fixtures), corpus 0 FP.
> This is the **first ADR-0022 increment** — the seam later flow rules build on.
>
> **▶▶ DONE (2026-07-01) — `call.unresolved-toplevel` (ref ADR-34).** A corpus rule-tally against
> the pinned v0.2.6 oracle (mastodon+gitlab: 762 undefined-method, 27 possible-nil, **14
> unresolved-toplevel**, 8 always-truthy, 6 override-visibility) named unresolved-toplevel the
> highest-frequency UNIMPLEMENTED rule with corpus signal — everything else (def.override-return/
> param, ivar-write, argument-type, unreachable-clause) fires **0** on the clean Rails corpus. It
> landed **0-FP** (§5): the presumed Object/Kernel-private-method blocker doesn't exist (core RBS
> declares `puts`/`require`/… `def self?.x`, already recorded as Kernel instance methods), so no
> index change; the real work was PROJECT-WIDE toplevel-def resolution (`SourceIndex::is_toplevel_def`,
> §3) to match the reference's directory-mode cross-file resolution (cleared 19 example-corpus FPs).
> 5 unit tests + 2 fixtures; 5 stale bare-`x` tests updated to `@x` (their `x` now correctly fires).
> The remaining unimplemented rules are effectively 0-on-corpus — **coverage via net-new rules is
> now exhausted** (confirms the [[undefined-method-lever-exhausted]] memory with fresh v0.2.6 data).
>
> Also fixed (2026-07-01) a **`flow.dead-assignment` FP** surfaced during the above: a `while x = …;
> f(&x); end` block-pass read wasn't counted, because the `&expr` block-pass (a `BlockArgumentNode`)
> lowered to nothing so the `x` read never entered the arena. The Call lowering now lowers the
> block-pass expression into `block_body` (also fixes `has_block` for `&block` calls); matched vs the
> v0.2.6 oracle on gitlab-foss `after_commit_queue.rb`.
>
> **▶▶ ALL THIS SESSION'S WORK IS COMMITTED + PUSHED (2026-07-01, origin/master @ `2d0add3`):**
> 5 commits — rustfmt policy (ADR-0032) · `flow.always-truthy-condition` + ADR-0022 flow substrate ·
> upstream v0.2.6 submodule pin · `call.unresolved-toplevel` · dead-assignment block-pass fix.
> 369 tests, run.rb + run_snapshot.rb PASS (36 fixtures, 0 FP), corpus 0 FP (clean v0.2.6 ref run),
> clippy `-D warnings` clean.
>
> **▶▶ DONE (2026-07-01, next session) — §9 rayon file-level parallelism.** The `check` pipeline
> (`analyze_files`) now runs parse+lower and analyze on a rayon pool with `build_project` as the
> serial barrier ("pre-pass tables frozen before workers"); output is byte-identical to serial
> (order-keyed collect + sequential side-effect drain), **0 FP preserved**, **~2.4× warm speedup**
> (12 cores, 7749 real files: 0.91s → 0.37s). See §9 for the full write-up + verification. This is
> the first Productization-track (lever A) increment; `RAYON_NUM_THREADS=1` forces serial.
>
> **▶▶ NEXT SESSION — START HERE.** Net-new-rule coverage is EXHAUSTED (the v0.2.6 rule-tally proved
> every unimplemented rule fires ~0 on the clean Rails corpus). The next levers are a different kind
> of value; ranked by EV:
> - **A. Productization (RECOMMENDED — highest EV, coverage-independent).** ✅ §9 **performance**
>   (rayon file-level parallelism) LANDED 2026-07-01 (see §9). ✅ §12 **LSP server v1 + v2** LANDED
>   2026-07-01 (`rigor lsp --transport=stdio`: live diagnostics + hover + member-access
>   **completion**; see §12) — the three headline LSP features are now all in. **Next LSP slice**
>   = `::` constant/namespace completion + Union-intersection + private-method visibility filter,
>   or the full two-tier `ProjectContext` (watched-files invalidation, debounce, worker pool).
>   ✅ **MCP server** (§12) LANDED 2026-07-01 (`rigor mcp`: read-only `check` + `type_of` tools) —
>   next MCP slice = more tools (`explain`, an outline tool) or resources/prompts. Also open: §11
>   CLI completion (`annotate`/`diff`/`triage`/`coverage --protection`/`sig-gen`); parallelizing
>   stage-2 `build_project` at scale.
> - **B. Plugin phase (§10, ADR-0013/0027)** — the Plugin trait (`dynamic_return`/`narrowing_facts`/
>   `node_rule`) + Rails plugins. The BIGGEST remaining undefined-method coverage pool ("most
>   remaining real-code coverage lives here"), but a large phase.
> - **C. ADR-0022 narrowing extension** — constant-prop → narrowing + negative facts + **ivar typing**,
>   unlocking `possible-nil` source expansion (`T | nil` params, `@ivar = nil` seeds, project-method
>   nilable returns) + `flow.unreachable-clause` (ADR-47). Strategic, but UNCERTAIN corpus payoff
>   (Rails is guard-dominated → live possible-nil ≈ 0).
> - **D. Small closures.** `pre_eval:` support (the one production caveat on `call.unresolved-toplevel`);
>   block-call ARITY recovery (§4 deferred); full config schema; baseline `regenerate`/`drift`/`prune`.
>
> (Independent of all the above: the two pre-v0.0.1 tracks still stand — the musl/Windows release
> targets need a real CI tag run (maintainer infra), and the v0.0.1 tag itself.)

**State:** a working, parity-validated analyzer. `rigor check` runs end to end;
**0 false positives across 3829 real files** (mastodon, gitlab-foss, conference-app,
the reference's own source; matched scales with the sweep — 558 at this size, 100%
precision). 369 tests. The design (ADR 0001–0032) is audited and stable. The
2026-06-26 session (a) aligned the undefined-method rule with the reference's leniency,
(b) closed lowering-traversal + interpolated-string gaps, (c) landed **class-method
(singleton) witnessing** with a cross-file project index, (d) fixed a pre-existing
block-call FP class, then in a follow-on pass: (e) **recovered block-call return
typing** (RBS block-overload derived), (f) added **gitlab/checkstyle/junit/teamcity
formats + CI auto-detection**, and (g) landed **cross-file in-source method RETURN-TYPE
inference** (ADR-0023 tier-4 minimal slice). See the note below.

**Build / test / run (from the repo root):**
```sh
cargo build --offline && cargo test --offline       # 369 tests; ruby-prism + ruby-rbs are cached
cargo run -p rigor-cli -- check <file.rb> --format json
ruby harness/run.rb                                  # fixture differential gate (must PASS, 0 FP)
ruby harness/run_corpus.rb <dir...>                  # scaled real-corpus gate (CORPUS_LIMIT env)
```

**Reference oracle (for the harness / manual checks):** the reference is **PINNED as a
git submodule** at `reference/rigor`, checked out at upstream tag **`v0.2.6`** (see
[`UPSTREAM.md`](../UPSTREAM.md) for the pin + bump procedure). Init once with
`git submodule update --init reference/rigor`.
```sh
ruby -I reference/rigor/lib reference/rigor/exe/rigor check <path> --format json
# JSON on STDOUT; preamble + racc warning on STDERR. Run with cwd = a clean temp dir to
# avoid picking up a project .rigor.yml. It accepts a directory (analyzes all .rb, RBS loaded once).
# The harness defaults REFERENCE_RIGOR_DIR to this submodule; set it to override.
```

**Key facts/paths:**
- RBS source: **vendored + embedded at build time** (ADR-0007). The default load path ingests
  `EMBEDDED_RBS` (generated by `crates/rigor-index/build.rs` from `crates/rigor-index/vendor/rbs/`),
  so the binary is standalone — **no runtime rbs-gem dependency**. The vendored set =
  rbs-4.0.3 whole `core/` (86 `.rbs`) ⊕ the `DEFAULT_LIBRARIES` stdlib transitive closure
  (49 libs, 85 `.rbs`; see `vendor/rbs/PROVENANCE.md`). `RIGOR_RBS_CORE_DIR` is retained as the
  out-of-band override seam (audit-R2): when set, the loader reads that dir at runtime exactly as
  before (whole dir + stdlib closure). Stub fallback only if the embedded set is empty / the
  override dir is unusable. Byte-identical to the old runtime path: same bytes → same `ruby-rbs`
  parser via the shared `ingest_rbs_source` (corpus: 542 matched / 0 FP, embedded == runtime).
- Real corpora under `/Users/megurine/repo/ruby/`: `mastodon/app/{models,services,controllers}`,
  `gitlab-foss/app/{models,services}`, `conference-app`, plus the reference's own `lib/` & `examples/`.
- Spikes (excluded from the workspace): `spike/prism_probe`, `spike/rbs_probe`.

**Highest-leverage next candidates.** **STRATEGIC FINDING (this session, oracle-grounded):
the `call.undefined-method` receiver-typing lever is now largely EXHAUSTED in rigor-rs's
witnessing model.** `undefined-method` is ~96% of the reference's error/warning diagnostics,
but the remaining MISSES are overwhelmingly on receivers rigor-rs *intentionally* does not
witness — **in-source/project-class instances and non-core `.new` instances are lenient**
(the parity invariant at the top of this file). Two investigations confirmed it: (a) the
**ActiveRecord `dynamic_return` plugin** measured **+0 gettable witnesses** over an
ActiveSupport-aware baseline across 581 Mastodon files (its value is on lenient project
models, on an `ActiveRecord::Relation` surface that would need a large plugin-class-registry
+ a new "known-but-non-witnessing" invariant, or in its OWN native rules like `unknown-column`
— none of which is a `dynamic_return` slice); and (b) **tier-4 call-site param binding**
landed sound + zero-FP but **+0 corpus matched** (the pattern is rare in real code). So
further coverage must come from **NET-NEW rule families (the `flow.*` family, §4) — not more
receiver typing.** The pure-RBS `activesupport-core-ext` plugin (core-class reopens) was the
last big receiver-typing win; the gated coverage there is real but only on plugin-enabled runs.
Ranked next levers:
1. 🟡 **Cross-file in-source RETURN-TYPE inference** (ADR-0023 tier-4 body inference) —
   **two slices LANDED** (this session): `SourceIndex` Pass-3 `infer_method_returns`
   types a project method's TAIL expression under an EMPTY `TypeEnv` and, when it yields a
   concrete **core/RBS** class, interns that core nominal so a chained typo witnesses
   (`user.full_name.lenght` where `full_name : String`). Zero-FP by strict
   under-approximation (witness set ⊆ reference): declines on explicit `return`, branch/loop
   tail, param/ivar/self dependence (empty env ⇒ Dynamic), in-source method-call tail, and
   reopen disagreement. **Slice 2 — call-site PARAMETER BINDING (LANDED).** A method whose
   tail is a bare positional-param read (`def full(x); x; end`) or a no-arg core-method CHAIN
   rooted at one (`def up(x); x.upcase; end`) now records a param-bound descriptor
   (`{ param_index, chain }`, Pass-3b `infer_one_param_bound`); the tier-4b call hook
   (`resolve_param_bound`) binds the positional ARGUMENT's type and re-derives the core
   return through the SAME `method_return` table tier 3 uses, so `g.full("hi").lenght`
   witnesses against String. The descriptor is self-contained (param index + no-arg core
   chain — no AST/node-id), so it is fully cross-file safe and never re-enters the build pass
   (no recursion/fixpoint). **Gate (decline ⇒ Dynamic, never an FP):** plain-positional
   params ONLY (lowering returns `params: None` ⇒ decline on splat/post/kwargs/block/optional/
   destructuring); the tail root must be a declared positional param; every chain step must be
   a no-arg, no-block call; arg count must cover `param_index`; the bound arg AND every chain
   step must land on a concrete CORE class; plus the inherited gates (explicit `return`,
   branch/loop tail, reopen disagreement). **Corpus: matched UNCHANGED at 542 (0 new
   real-corpus witnesses), 0 FP** — the pattern (a project pass-through/transform of a
   positional arg, then a typo chained on the result with a literal/core argument) is rare in
   real code; the increment is a correct, zero-FP closure of the param-binding deferral, not a
   coverage lever. **Deferred (next increments):** multi-param / value-unrolling binding (the
   reference binds args more richly — we decline), cross-method-call return inference +
   fixpoint (ref ADR-55/56), branch/explicit-return UNION (needs a union-consuming witness
   site), ivar/self typing (ADR-0022 flow), singleton (`def self.x`) return inference. These
   are the remaining in-source coverage levers.
2. ✅ **Drop-in readiness landed** (this session): inline `# rigor:disable` suppression,
   minimal `.rigor.yml` (disable/exclude), `github` + `sarif` + `gitlab` + `checkstyle` +
   `junit` + `teamcity` output (all four new formats byte-identical to the reference) and
   **CI auto-detection** (ADR-51, full provider table) and **baseline read/write** (ADR-22 —
   byte-compatible `.rigor-baseline.yml`, `check --baseline`, `baseline generate`/`dump`).
   Remaining §7: full config schema; baseline `regenerate`/`drift`/`prune` + `--baseline-strict`.
3. **Plugin phase** (§10, ADR-0013) — the real Rails-coverage unlock (sidecar-hosted Ruby
   plugins). Biggest phase; **the bulk of remaining undefined-method coverage lives here**
   (the gap analysis confirms most misses are Rails receivers needing project-RBS/plugins).
4. **Flow-sensitive scopes + narrowing** (§4, ADR-0022) → the `flow.*` rule family and a
   live `possible-nil-receiver`.
5. ✅ **Vendor + embed RBS at build time** (§3, ADR-0007) — **LANDED.** The runtime RBS path
   is no longer the default: `build.rs` embeds the vendored `vendor/rbs/` set (`EMBEDDED_RBS`),
   `load()` ingests it by default (standalone, no rbs gem). `RIGOR_RBS_CORE_DIR` override seam
   retained (audit-R2). Proven byte-identical: 542 matched / 0 FP, embedded == runtime path.

---

## Status snapshot

- **Design:** ADRs 0001–0032 (`docs/adr/`) + glossary (`CONTEXT.md`), audited
  (`…/ruby/rigor/docs/notes/20260626-rigor-rs-design-audit.md`; verdict positive, R1–R5 done).
- **Build:** Cargo workspace, edition 2024, **MSRV 1.88** (forced by the `ruby-rbs` dep's
  let-chains; CI pins 1.88; clippy's suggestion-MSRV stays 1.85 for our own crates),
  `Cargo.lock` committed. External deps: `ruby-prism` (parser), `ruby-rbs` (RBS parser) — cached.
- **Crates:** `rigor-types` (lattice) · `rigor-parse` (Prism + owned AST) ·
  `rigor-index` (real RBS index) · `rigor-infer` (typer + folding + source index) ·
  `rigor-rules` · `rigor-cli` (`rigor check`).
- **Tests:** 368 (verified `cargo test --offline`; `flow.always-truthy-condition` added +11,
  `call.unresolved-toplevel` added +5 rule tests and updated 5 stale tests whose bare-`x` toplevel
  stand-ins now correctly fire the new rule — switched to `@x` ivar receivers). **Parity:**
  `run.rb` PASS (36 fixtures incl. the plugin-enabled +
  gate-guard pair, the tier-4b param-binding witness/decline pair, the four
  `def.override-visibility-reduced` fixtures — superclass + module-include positives, the
  reopened-class split, and the adversarial negatives bundle — the two
  `call.possible-nil-receiver` fixtures: a byte-exact true positive + a guarded-negatives
  bundle — the two `flow.always-truthy-condition` fixtures: a 4-case witness fixture
  (literal-assigned / nil / inferred-fold / unless-false, all byte-exact vs the oracle) + an
  adversarial-negatives bundle — and the two `call.unresolved-toplevel` fixtures: a witness fixture
  (undefined toplevel calls + a fire inside a toplevel `def` body, byte-exact vs the oracle) + a
  pure-negatives bundle proving ~25 Kernel/Object methods + a toplevel def + in-class calls stay
  silent), 0 FP; `run_corpus.rb`
  validated to **3829 real files, 0 FP, 637/637 matched** (`def.override-visibility-reduced`
  added **+79 matched net**, of which **+44 are override-visibility witnesses on
  mastodon+gitlab, 44/44 reference-equal**; 100% precision; embedded RBS == runtime path,
  byte-identical) — and the default (no-config) corpus is **byte-unchanged with the first
  plugin slice landed**, proving config-gating doesn't regress the default path.
- **Works today:** `rigor check [--format text|json] <file…>` →
  `call.undefined-method` (literals, chained calls, post-fold, **core `X.new`
  instances** like `Array.new`, **interpolated strings/heredocs**, and **class-method
  typos on top-level constants** like `Time.current` → `singleton(Time)`) and
  `call.wrong-arity`; Rust-native constant folding (`1 + 2` → `3`, ASCII
  String/Integer/etc.); JSON field-identical to the reference; never-crash per-file
  isolation; a **cross-file project pass** (`build_project`) so a project model is
  known everywhere. **In-source/project-class *instances* and non-core `.new`
  instances are typed but NOT witnessed** (reference leniency); block-bearing calls
  type to their **block-overload RBS return** (so `arr.map { }.frist` witnesses; declines to
  Dynamic when the block form isn't modeled). Rails models (unknown super) stay silent.

---

## Port backlog by subsystem

Reference paths are under `/Users/megurine/repo/ruby/rigor/`.

### 1. Parsing & AST — `lib/rigor/source/` → `rigor-parse` (ADR-0003/0012)
- ✅ `ruby-prism` binding; `parse()`; offline libprism build.
- ✅ Owned `NodeId`-indexed AST + lowering for a broad node set: program/statements,
  local read/write, str/int/float/sym/nil/true/false literals, call + positional args
  + block body, `if`/`unless`/ternary, `case`/`when`/`in`, `while`/`until`/`for`,
  `begin`/`rescue`/`ensure`, `&&`/`||`, ivar/cvar/gvar read+write, constant read+write,
  array/hash literals, `self`, ranges, interpolation, **`ClassDef`/`ModuleDef`**
  (name + superclass + direct instance-method names).
- ✅ **`Node::If.is_unless`** — the `unless` keyword survives lowering (Prism keeps `IfNode` and
  `UnlessNode` distinct; the lowering collapses both into one `Node::If`, so the keyword would
  otherwise be lost). An additive `bool` field threaded at the two construction sites (`if`/ternary
  ⇒ `false`, `unless` ⇒ `true`); all other consumers match with `..` and are byte-stable. Required
  by `flow.unreachable-branch` (§5), which uses it to pick the correct dead branch under the
  keyword-inversion — a latent AST-correctness fix (the keyword was previously unrecoverable).
- ⬜ Keyword/splat/block-arg precision; string-interpolation typing; `&.`; synthetic-node
  variants (ADR-0012/0013); Tuple/HashShape from array/hash literals; ERB detection.

### 2. Type lattice — `lib/rigor/type/` → `rigor-types` (ADR-0005/0018/0019/0020)
- ✅ Carrier set, `Interner`/`TypeId`, `Scalar`, `describe()`; Dynamic[T] algebra;
  `Certainty` (trinary) + `Evidence`; `subtype`/`consistent` skeleton.
- 🟡 `normalize` (flatten/dedup/order; no `1|Integer` collapse; `true|false` display-only).
  `subtype`/`consistent` return `Maybe` for many cases (need nominal hierarchy, IntegerRange/
  Tuple/HashShape/refinement reasoning).
- ⬜ Dynamic provenance side-channel (ADR-0019/ref ADR-75); `DataClass`/`DataInstance`
  (ref ADR-48) + `StructClass`/`StructInstance` (ref ADR-68); HKT `App[uri,args]` (ref ADR-20);
  refinement carriers catalogue (kebab-case built-ins).

### 3. Index layer — `lib/rigor/environment*.rb`, `scope_indexer.rb` → `rigor-index` (ADR-0004/0007)
- ✅ **Real RBS-backed `CoreIndex`** (`rbs.rs`): parses the WHOLE `core/` + the reference's
  `DEFAULT_LIBRARIES` stdlib set (json/yaml/date/uri/csv/pathname/…, transitively closed over
  each lib's `manifest.yaml` deps) via the `ruby-rbs` parser. Builds per class: instance-method
  set, **singleton (class) methods** + extends + singleton aliases, return type, arity (min..max
  over overloads), super + includes; flattens the ancestor chain. Handles RBS `alias` (instance
  AND singleton, resolved through the chain) and **nested class/module decls** (registered by
  simple name; a `nested` flag keeps lexically-nested decls OUT of the top-level set so
  `knows_toplevel_class` is sound). **Conservative gate (zero-FP keystone):** absence is witnessed
  only when the entire chain is loaded; an incomplete/unknown chain ⇒ assume present ⇒ silent.
  Exposes `class_has_method` (instance), `class_has_singleton_method` (class-method,
  extend/alias-aware), `knows_toplevel_class`.
- ✅ **In-source class index** (`rigor-infer/source_index.rs`): a map of project class names ↔
  registry `ClassId`s. **`build_project(asts, core)`** builds it PROJECT-WIDE (the CLI's pass);
  `build(ast, core)` is the single-file path. Used to (a) **type `X.new`** instances (for chained
  RETURN inference), and (b) gate **singleton typing**: a bare constant types to `Singleton` only
  when `!source.knows_class(name)` — so a project model defined in ANY file is never class-method-
  witnessed (the cross-file zero-FP keystone). Project-class *instances* are still NOT a
  witnessing surface for the undefined-method rule (reference leniency).
- ✅ RBS source is **vendored + embedded at build time** (ADR-0007): `build.rs` walks
  `crates/rigor-index/vendor/rbs/` (whole `core/` ⊕ `DEFAULT_LIBRARIES` transitive closure, the
  exact set the runtime path loaded — 86 core + 85 stdlib `.rbs`, 49 libs) and emits
  `$OUT_DIR/embedded_rbs.rs` (`EMBEDDED_RBS: &[(&str,&str)]`, sorted for determinism; std-only, no
  new deps, offline). `load()` ingests the embedded set by default via the shared
  `ingest_rbs_source` (same bytes → same `ruby-rbs` parser as the filesystem path ⇒ byte-identical).
  `RIGOR_RBS_CORE_DIR` retained as the runtime override; hardcoded-stub only on the degenerate path.
- ✅ **Cross-file** project class index (`build_project`) for the singleton FP gate + the
  PROJECT-WIDE **toplevel-def set** (`SourceIndex::is_toplevel_def`, ADR-34): every `def` outside a
  class/module (across all files) + in-source Object/Kernel/BasicObject reopens, so
  `call.unresolved-toplevel` resolves a call against a `def` in a `require`d file (matching the
  reference's project-mode resolution — the cross-file zero-FP keystone). ⬜ cross-file CONSTANT
  index + cross-file in-source method RETURN inference (the next real coverage lever).
- ⬜ Project `sig/` + gem RBS (bundler / rbs_collection) + `target_ruby` overlays (ADR-0007).
- ⬜ Method visibility, `prepend` order, generics/refinement resolution.
- ⬜ Constant resolution (in-source > RBS precedence, `# TYPE:`); `pre_eval` monkey-patch pass
  (ref ADR-17); Gemfile.lock-gated RBS overlays (ref ADR-72); Rubydex accelerator (ADR-0004 spike).

### 4. Inference engine — `lib/rigor/inference/` → `rigor-infer` (ADR-0022/0023/0024)
- ✅ `Typer`: pure `type_of` by node variant; literals; local env; **chained-call typing**
  (resolve receiver class → method return → nominal); **`X.new` → instance typing**;
  array/hash literal → nominal Array/Hash; **interpolated string → String**; **bare top-level
  constant → `Singleton(class)`** (class-object, for class-method witnessing); **block-bearing
  call → block-overload RBS return** (`Hash#select { } -> Hash`, `arr.map { } -> Array`, `x.tap
  { } -> x`; declines to Dynamic when the block form isn't modeled — zero-FP).
- ✅ Rust-native constant folding (`folding.rs`) — deterministic Integer/Float/Bool/Nil/Symbol/
  ASCII-String; declines (→ None) on any doubt; arg-dependent folds (`1 + 2 → 3`).
- 🟡 The general typer environment is flat / top-level (the call/chaining/arity rules consume it);
  params/ivars/non-class-constants → Dynamic. **A first flow-sensitive substrate landed** alongside
  it (ADR-0022, used ONLY by `flow.always-truthy-condition`, §5): `Typer::always_truthy_snapshots`
  runs a SEPARATE local **constant-propagation** pass with real `if`/`unless` branch JOINS +
  loop/block/`case`/`begin`/`&&`-`||` widening, so a predicate's constant-ness is sound across
  conditional reassignment. It is scoped to that rule (does not perturb the flat env the other
  rules use) and is a strict under-approximation (widen on any doubt). Full narrowing / negative
  facts / 5-edge scopes / fact buckets remain deferred.
- ✅ **RECOVERED (2026-06-26): block-call result typing.** A block-bearing call now types to its
  **block-overload RBS return**, not Dynamic — exactly the reference's `block_required: true`
  overload selection (`method_dispatcher/rbs_dispatch.rb` → `overload_selector.rb`). It is
  **RBS-derived, not a hardcoded table:** the index records, per method, the return of the overload
  that declares a `block:` clause, resolving a concrete `ClassInstanceType` (`Hash#filter { } ->
  ::Hash`, `Enumerable#map/flat_map { } -> ::Array`) or a `self` return (`Array#each { } -> self`,
  `Kernel#tap { } -> self`) to the receiver's own class. So `h.select { } : Hash` (alias of
  `filter`), `h.reject { } : Hash`, `arr.map { } : Array`, `x.tap { } : x`, `arr.each { } : arr` —
  and `arr.map { }.frist`-style chains witness again (verified byte-identical to the reference on
  the §4 target cases + 0 FP across 831 corpus files). Zero-FP discipline preserved: when the
  block-form return isn't precisely modeled (no block overload, or a generic/union/void/unknown
  return — `method_return_with_block ⇒ None`), or the receiver isn't a concrete modeled class, the
  call DECLINES to Dynamic (silent), exactly as the placeholder did; the `select{}.keys` FP-guard
  case still types to `Hash` and stays silent. Touch points: `rigor-index/rbs.rs`
  (`block_overload_return` + per-class `block_returns` map + `method_return_with_block`),
  `rigor-index/lib.rs` (free `method_return_with_block`), `rigor-infer/lib.rs`
  (`Typer::type_block_call`, replacing the `!block_body.is_empty()` short-circuit). **Block-call
  ARITY is still deferred** (kept the `check_wrong_arity` `has_block` early-return): the reference
  DOES witness block-form arity (the block overload takes 0 positional args), but we store only a
  single arity envelope collapsed over all overloads and cannot isolate the block overload's
  count — staying silent there is a missed witness, never an FP. Per-block-overload arity is the
  follow-up to recover those.
- 🟡 **Flow-sensitive scopes** (ADR-0022) — a FIRST slice landed: `Typer::always_truthy_snapshots`
  is a flow-sensitive local **constant-propagation** pass with real `if`/`unless` branch JOINS +
  loop/block/`case`/`begin`/`&&`-`||` widening (used by `flow.always-truthy-condition`, §5). Still
  ⬜: the full 5 edges + fact buckets + invalidation, and narrowing (guards, `is_a?`, truthy/falsey,
  equality trust, negative facts domain-relative) — the substrate the `possible-nil` source
  expansion + `flow.unreachable-clause` need next.
- ⬜ Full dispatch tier cascade (tier-2 shape, tier-4 in-source bodies); cross-file implicit-self
  (ref ADR-24/57); inference budgets (wired guards + table, ADR-0024); block/loop fixpoint +
  break-sink (ref ADR-56); recursive-return precision (ref ADR-55); reflexive-send fold guard
  (ref ADR-78); parameter type inference (ref ADR-67); purity/mutation summaries.
- ⬜ Ruby **sidecar** for the folding long tail + plugin calls (ADR-0008): worker, MessagePack
  IPC, two-level persistent cache, graceful degradation.

### 5. Diagnostic rules — `lib/rigor/analysis/check_rules.rb` → `rigor-rules` (ADR-0030)
Converged single walk (ADR-0005). Reference has ~19 built-ins.
- ✅ `call.undefined-method` (witnesses **core/RBS receivers only** — literals, RBS-method
  returns, core `X.new`; in-source/non-core `.new` instances are lenient, matching
  `check_rules.rb:556` `rbs_class_known?`) · ✅ `call.wrong-arity` · 🟡 `call.possible-nil-receiver`
  (**partial — the nilable-RBS-return slice**, ref `check_rules.rb:1069` `nil_receiver_diagnostic`).
  Fires `error` (balanced) when a method-local `x = recv.m(..)` has a CERTAIN nilable core RBS
  return (`String#byteslice -> String?`) on a **non-constant Nominal** core receiver — minting
  `C | nil` — and the called method is present on `C` but absent on NilClass, with **no guard**.
  The keystone is the nil-source restriction: nil is minted ONLY from a certain nilable RBS return
  on a known core class — NEVER from Dynamic / unknown / project receivers, a non-nilable return,
  or a **Constant** RHS receiver (the reference CONSTANT-FOLDS a literal-receiver core call to a
  concrete non-nil value, so it stays silent there — minting on a Constant would be a guaranteed
  FP). Replaces the reference's full flow-narrowing with a conservative whole-method-body
  **DECLINE scan** (same span-scan as `dead-assignment`): declines silently if anything touches
  `x` — `.nil?`, an `if`/`unless`/`while`/`until`/ternary predicate, a `&&`/`||` operand, safe-nav,
  any op-write (`||=`), or `present?`/`blank?`/`presence` (the reference does NOT narrow on the
  last three, so declining only loses recall — never an FP). A scoped per-method-body local env
  (`Typer::build_method_body_env`, used ONLY by this rule) types the nil-source RHS receiver
  without perturbing the top-level-only typing of the other rules. Substrate added: RBS `Optional`
  return preserved as `(class, nilable)` (`method_return_nilable`, was discarded → Dynamic) +
  `Node::Call.safe_nav`. **+0 net corpus matched** (637 → 637, 0 FP) — accepted: the corpus
  nil-sources are params / `@ivar = nil` seeds / project-method returns, all DEFERRED here; the
  value is the reusable nilable substrate + converting the inert stub to a real, byte-exact rule.
  **Deferred** (needs ADR-0022 flow scopes for full narrowing): `T | nil` param nil-sources,
  class-ivar `@x = nil` seeds (ref ADR-58 WD1), project-method nilable returns, chained receivers.
- ✅ **Metaclass-constructor guard** (`CLASS_RETURNING_NEW` in `rigor-infer`): `Struct.new(...)`,
  `Data.define(...)`, `Class.new` return a CLASS, not an instance — never typed as an instance
  of the receiver (was a chained-`.new` FP).
- ✅ `call.unresolved-toplevel` (ref ADR-34) — an implicit-self call (`receiver: None`) at
  TOPLEVEL scope (outside any class/module body — a toplevel `def`'s BODY IS still toplevel; only a
  method inside a class/module is not) whose name resolves against NEITHER the `Object`/`Kernel`
  instance surface NOR a toplevel `def`. Fires `warning` (evidence `low`; the reference message +
  `pre_eval:` routing, verbatim), anchored on the method-name token. **The Object/Kernel surface was
  the presumed blocker (private methods) — but it does NOT exist:** `puts`/`require`/`raise`/`loop`/
  `format`/… are declared `def self?.x` in core RBS, so rigor-rs already records them as instance
  methods on Kernel (which Object includes), and `class_has_method("Object", …)` resolves them
  (verified `"x".puts`/`.require`/`.loop` all silent). Zero-FP gate: suppress on the Object surface
  (witnessed-absent only when Object's whole core chain is loaded ⇒ a miss stays silent), on
  PROJECT-WIDE toplevel `def` names (`SourceIndex::is_toplevel_def`, §3 — cross-file so a `def` in a
  `require`d file resolves the call, matching the reference's project-mode resolution; this is the
  zero-FP keystone that cleared 19 example-corpus FPs where `route_helpers.rb` defines the toplevel
  defs `demo.rb` calls), and on in-source `Object`/`Kernel`/`BasicObject` reopens. Toplevel detection
  is span-containment against class/module spans (orphan-proof). `pre_eval:` monkey-patches are not
  modeled (rigor-rs has no `pre_eval`), a documented limitation — on the config-less corpus/harness
  the tools agree exactly. **Corpus (pinned v0.2.6): 0 FP** across mastodon+gitlab+conference (the
  one residual corpus FP is a PRE-EXISTING `flow.dead-assignment` bug on `while x = …; f(&x)` — the
  `&x` block-pass read isn't counted — unrelated to this rule; see the spawned task).
- ⬜ `call.self-undefined-method` (ships `:off`; needs subclass-aware gate) ·
  `call.argument-type-mismatch` (ref ADR-64).
- ✅ `flow.dead-assignment` — **the first `flow.* rule`**. A pure AST/structural check (no
  flow-sensitive scopes, no typer/folding): a local assigned in a NAMED method body but never
  read in that body fires `warning` (`local \`x' assigned in \`m' but never read`), anchored on
  the name token. Faithful port of `DeadAssignmentCollector` — op-write/and/or-write targets
  count as READS (so `total += 1` suppresses), trailing-write (implicit return) / `_`-prefix /
  multi-write are skipped, nested defs are their own unit. Reads/writes are gathered by
  **span-containment over the def span** (orphan-proof: several Prism wrappers — `return`,
  `super`, `*splat` — lower lossily; a structural child-walk would miss reads underneath and
  FALSE-flag). Closing that gap required a lowering fix: a new `Node::LocalVariableOpWrite`
  variant (op/and/or-writes) + recovering reads/calls buried under unhandled wrapper nodes
  (the catch-all now lowers descendant reads/calls instead of dropping the subtree) + **lowering
  the `&expr` block-pass argument** (a `BlockArgumentNode`, previously dropped — so `while x = q.pop;
  f(&x); end` orphaned the `x` read and FALSE-flagged the loop-condition write; the passed
  expression now lowers into `block_body`, which also makes `has_block` correct for `&block` calls;
  fixed 2026-07-01, matched vs the v0.2.6 oracle on gitlab-foss `after_commit_queue.rb`).
  **+0 net corpus fires** in this unusually-clean corpus (accepted — the value is the net-new
  `flow.*` family + the adversarial-fixture FP guarantee); 0 FP across 3829 corpus files.
- ✅ `flow.always-raises` — a provable Integer `ZeroDivisionError`. Fires `error`
  (`always raises ZeroDivisionError: \`<op>' by zero on Integer receiver`, anchored on the
  operator/method token) iff ALL hold: the method ∈ the reference's `INTEGER_RAISING_OPERATORS`
  (`/ % div modulo divmod` — verbatim, op set closed), the receiver is provably **Integer-rooted**
  (`Constant[Integer]` | `IntegerRange` | `Nominal[Integer]` with no type args — the reference's
  `integer_rooted_for_diagnostic?`), exactly ONE positional arg, and that arg types to a constant
  **Integer zero** (`Constant[Int(0)]`). **Float is declined on BOTH sides** (verified against the
  oracle): a Float receiver (`5.0 / 0` → Float division is `Infinity`, not an error) and a Float
  divisor (`5 / 0.0`) are silent; a non-constant divisor (`x / y`), a Dynamic receiver (`x / 0`,
  `x` unbound), a non-zero divisor (`5 / 2`), and any block-bearing call all decline. Implemented
  in the existing call-rule `.or_else` chain (`check_always_raises`) — undefined-method /
  wrong-arity never fire on these (the ops are defined with correct arity), so no double-emit.
  Error severity ⇒ the gate declines on any uncertainty (zero-FP keystone: an FP here is an ERROR
  on correct code). **+0 net corpus fires** (real production code never divides by a literal `0`;
  accepted — a complete, correct rule for general code, fully exercised by the harness fixtures);
  0 FP across 3829 corpus files, grand matched UNCHANGED at **637**.
- ✅ `flow.unreachable-branch` — a purely **SYNTACTIC**/AST check (no typer, no folding): an
  `if`/`unless`/ternary (Prism parses a ternary as an `IfNode` too) whose predicate is a
  **literal node** that is always truthy or always falsey, making one branch dead, fires `warning`
  (`unreachable branch: literal predicate is always <truthy|falsey>`, evidence `high`) anchored on
  the DEAD branch. The literal set mirrors the reference's `TRUTHY_LITERAL_NODES`/`FALSEY_LITERAL_NODES`
  exactly: `true`/Integer/Float/String/Symbol ⇒ truthy, `false`/`nil` ⇒ falsey; a **constant or
  variable predicate that would fold to a literal must NOT flag** (the reference uses syntactic
  literal detection, not the folder), and an interpolated string (`"a#{x}"`) is declined (the
  reference matches `StringNode`, not `InterpolatedStringNode`). The **keyword-inversion** is the
  parity keystone: for `if`, truthy ⇒ ELSE dead / falsey ⇒ THEN dead; for `unless` the two INVERT
  — so the dead-branch selection reads the new `Node::If.is_unless` flag (see §1). The dead branch
  must be PRESENT (its node exists) — a then-dead with an empty/absent then declines, but an
  empty-but-present `else` clause still fires (verified against the oracle). Anchor: a dead THEN
  on its first statement, a dead ELSE on the `else` keyword. **Fires ~0 times on the real corpus**
  (literal-predicate conditionals are vanishingly rare in production) — accepted; the value is a
  complete, correct rule plus the `is_unless` AST-correctness fix. 0 FP across 3829 corpus files,
  grand matched UNCHANGED at **637**.
- ✅ `flow.always-truthy-condition` (ADR-0022 first flow slice) — the **inferred-constant**
  counterpart to the syntactic-literal `unreachable-branch`. Fires `warning` (`condition is always
  <truthy|falsey> (the surrounding flow proves it folds to a constant)`, anchored on the predicate
  node) when an `if`/`unless`/ternary predicate folds to a `Type::Constant` under the dominating
  flow scope. Polarity mirrors the reference exactly: a `nil`/`false` constant ⇒ `falsey`, every
  other constant ⇒ `truthy`. Skip envelope ported verbatim from the reference's
  `AlwaysTruthyConditionCollector`: a SYNTACTIC literal predicate (owned by `unreachable-branch`,
  so no double-fire), a defensive predicate call (`nil?`/`empty?`/`zero?`/`any?`/`none?`/`all?`/
  `respond_to?`), and a predicate lexically inside a loop/block are all declined.
  **The zero-FP keystone is a NEW minimal flow substrate** — `Typer::always_truthy_snapshots`
  (`rigor-infer`): ONE flow-sensitive local **constant-propagation** pass that threads a per-scope
  env, **forks `if`/`unless` branches and JOINS them** (a binding survives only when both branches
  agree on the identical `TypeId`, else widens), and widens every local written under a loop /
  block / `case` / `begin` / `&&`-`||` / any other node (span-containment, orphan-proof). This is
  what makes a surviving constant SOUND: `x = 5; if c; x = f; end; if x` widens `x` and does NOT
  fire (the flat env's central unsoundness — it would falsely retain `x = 5`). `def`/`class`/
  `module` bodies are independent scopes (fresh env, inherited loop/block suppression). A strict
  UNDER-approximation of the reference folder (witness ⊆ reference): it never folds ivars,
  method-call returns, or params to constants, so the dangerous FP families (ivar/overridable-method
  folding) simply never arise. Verified byte-exact against the oracle on the positive cases
  (`x=5;if x` ⇒ 2:4 truthy; `y=nil` ⇒ falsey; `1+1` inferred fold; `unless false`). Like
  `unreachable-branch`, fires ~0 times on the real corpus (inferred-constant predicates are
  vanishingly rare in production) — ACCEPTED; the value is a complete `flow.*` rule plus the
  reusable flow-constant substrate (the first ADR-0022 increment, the seam later flow rules build
  on). **Deferred:** full narrowing / negative facts / 5-edge scopes / fact buckets (the rest of
  ADR-0022); predicates nested in non-loop `case`/`begin`/`&&` are conservatively declined here
  (the reference records them).
- ⬜ `flow.unreachable-clause` (ref ADR-47).
- ✅ `def.override-visibility-reduced` (ref ADR-35 slice 1) — a purely **STRUCTURAL** def-family
  check (no typer, no flow scopes, no unions): an instance-method override whose visibility is
  STRICTLY MORE RESTRICTIVE than the nearest **project-source** ancestor method it overrides
  (public→protected/private, protected→private) fires `warning` (`visibility of \`m' reduced from
  <parent> to <override> (overrides Parent#m); breaks substitutability`), anchored on the
  overriding def's name token. The override visibility is read from a source-discovered table
  (bare-modifier flip / `private :sym` back-patch; `def self.x` excluded; `private def foo` records
  at the running default and is therefore untracked — both deferrals match the reference gap).
  Ancestors are walked MRO-ordered (includes/prepends FIRST, then superclass) over a **lexically-
  qualified** override index — `module Params` nested in `IssuableFinder` keys `IssuableFinder::Params`,
  never merging with `Groups::Params` (last-component collapse was the gitlab-foss FP cluster).
  **Two zero-FP keystones**: (1) RBS / third-party ancestors are NOT walked (project-source ancestors
  only); (2) the rule NEVER synthesizes `Public` from a missing ancestor-visibility entry — absent
  visibility ⇒ silent. **Corpus: +44 override witnesses on mastodon+gitlab (44/44 = reference-
  equal), 0 FP**; grand corpus **558 → 637 matched / 0 FP** across 3829 files. RBS-ancestor
  comparison, the singleton/`private def` forms, and `def.override-return-widened` are deferred.
- ⬜ `def.return-type-mismatch` · `def.method-visibility-mismatch` · `def.override-return-widened` (ref ADR-35) ·
  `def.ivar-write-mismatch` (ref ADR-58).
- ⬜ `dump.type` / `assert.type-mismatch`; discriminated-union narrowing (ref ADR-66);
  `rbs.coverage.missing-gem` + config/coverage diagnostics.
- 🟡 Suppression order (inline → config `disable:` → baseline LAST) is wired in
  `main.rs`/`baseline.rs` (ADR-22 WD6). ⬜ Severity resolution precedence + per-rule canonical
  severities + token expansion (ADR-0030); diagnostic enrichment remainder
  (`project_definition_site`, full `source_family`).

### 6. Output & reporters — `lib/rigor/cli/diagnostic_formats.rb` → `rigor-cli` (ADR-0014/0030)
- ✅ text + JSON (hand-rolled; field-identical to the reference for the call rules — the
  harness depends on this, keep byte-stable). ✅ **`github`** (Actions annotations) + **`sarif`**
  (SARIF 2.1.0, serde_json) — additive, CI-consumable, NOT harness-gated.
- ✅ **`gitlab`** (GitLab Code Quality JSON; serde-derived structs for exact key order; SHA-256
  `fingerprint` over `[path, qualified_rule, line, column, message].join("\0")` — the NUL
  separator is load-bearing, dependency-free SHA-256 in `diagnostic_formats.rs`) ·
  ✅ **`checkstyle`** (hand-rolled XML, 5-entity escaping, grouped by file in first-appearance
  order) · ✅ **`junit`** (hand-rolled XML; one `testcase`/diagnostic, clean run = one passing
  case) · ✅ **`teamcity`** (`##teamcity[…]` service messages, `|`-escaping; empty on a clean
  run). All four are **byte-identical to the reference** (parity-checked with + without
  diagnostics, single + multi-file). Additive, NOT harness-gated.
- ✅ **CI auto-detection** (ref ADR-51 WD7, `ci_detector.rs`): the reference's full 14-row
  `PROVIDERS` table (most-specific first, `CI` catch-all last), tiers
  `NativeStdout`/`NativeArtifact`/`Reviewdog`, `RIGOR_CI_DETECT=0|false|no|off` disable seam.
  Triggered ONLY for `--format text` (an explicit format means the caller is in control):
  GitHub Actions/TeamCity auto-emit their native format on stdout on top of the human output;
  GitLab/reviewdog-routed CIs print a one-line hint to stderr when there are diagnostics. The
  harness (no CI env) is never augmented.

### 7. Config & baseline — `configuration.rb`, `analysis/baseline.rb` → (ADR-0009/0031)
- ✅ **In-source suppression** (`# rigor:disable <rules>` line, `# rigor:disable-file <rules>`/`all`)
  — `rigor_parse::comment_lines` + `rigor_rules::filter_suppressed` with reference-exact token
  expansion (legacy aliases, `call` family, canonical ids, `all`; `internal-error` never
  suppressed). Honored with no config, matching the reference (fixtures 13/14).
- ✅ **`.rigor.yml` loader (minimal):** `disable:` (rule tokens, reuses the suppression
  `SuppressSet`) + `exclude:` (path globs, `glob` crate). Discovery: `--config <path>` else
  `.rigor.yml` in **cwd only** (reference-matching + harness-safe — the repo has none, so the
  differential gate sees no config). Malformed ⇒ default+warn; unknown keys ignored.
- ⬜ Full key schema (target_ruby/paths/plugins/libraries/signature_paths/severity_profile/
  auto_detect/budget_overrun_strategy/bleeding_edge/plugins_isolation); `.rigor.dist.yml`,
  winner-takes-all `includes:` stack, relative-to-config paths, config-validation warnings.
- ✅ **Baseline read/write** (ref ADR-22) — `crates/rigor-cli/src/baseline.rs`. Byte-compatible
  `.rigor-baseline.yml` (`version: 1`; `ignored:` rows `file`/`rule`/`message?`/`count`;
  `ignored: []` when empty). Hand-rolled writer/reader (the `.rigor.yml`-loader precedent) plus a
  faithful Ruby-`Regexp.escape` port. **`--match-mode rule` (default) baselines are byte-identical
  to the reference's, verified both directions** (the file/rule/count rows match exactly, and a
  reference-generated rule baseline suppresses rigor-rs diagnostics and vice-versa). `message`-mode
  baselines are byte-identical **only where the underlying diagnostic message matches** — they embed
  the rendered `message:`, so a literal receiver (`[1, 2].firts`) diverges (`for \[1,\ 2\]` in the
  reference vs `for Array` in rigor-rs) because of the **pre-existing literal-vs-nominal receiver
  render gap** (rigor-rs types literals to a bare `Array`/`Hash` nominal; not a baseline-format bug).
  So rule-mode is the fully-interchangeable mode; message-mode interchange is exact only for
  core/RBS receivers. WD4 bucket semantics
  (`actual <= count` → all silenced; `> count` → whole bucket surfaces) and WD6 ordering
  (baseline applied LAST, after inline + config suppression) match; message-pattern rows take
  precedence over rule-ID rows (`regex` crate, already in Cargo.lock). `check` gains `--baseline
  <path>` / `--no-baseline` plus the `.rigor.yml` `baseline:` key (string activates, `false`/absent
  = off); paths keyed project-root-relative like `Dir.pwd`. With no baseline the `check` path is a
  no-op (harness-gated, byte-identical). 🟡 **Deferred:** `baseline regenerate`/`drift`/`prune` and
  `check --baseline-strict` (they depend on `configuration.paths`, which rigor-rs's CLI does not yet
  model) — recognized with a clear message + exit 2.

### 8. Caching & incremental — `lib/rigor/cache/` → (ADR-0017/0028)
- ⬜ Content-addressed persistent analysis cache (`.rigor/cache`), LRU; six-slot descriptor +
  two store paths; incremental cross-file dep graph + `--verify-incremental` (ref ADR-46).

### 9. Concurrency — `worker-session`, ractor → (ADR-0006/0028)
- ✅ **rayon file-level parallelism landed (2026-07-01).** `analyze_files`
  (`rigor-cli/main.rs`, the shared `check`/`baseline generate` pipeline) now runs its two
  file-INDEPENDENT stages on a rayon work-stealing pool: **stage 1** (read + parse + lower each
  file) and **stage 3** (analyze each file against the shared index). **Stage 2** — the
  project-wide `SourceIndex::build_project` — stays the **serial barrier** between them (this IS
  the "pre-pass tables frozen before workers": `index` + `project_source` are immutable/`Sync`
  and shared read-only across the stage-3 pool; each worker mints a FRESH per-file `Interner`).
  **Byte-identical output is the parity keystone:** each parallel stage `par_iter().map().collect()`s
  its outcomes IN INPUT ORDER, and all side effects — the stderr lines AND the findings pushes —
  are replayed by a SEQUENTIAL drain of that ordered Vec, then the existing `sort_by_key(order)`
  restores global input order. So stdout, stderr, and exit code are byte-for-byte the serial
  result; the pool is invisible. Per-file panic isolation (ADR-0016) is preserved — each closure
  `catch_unwind`s its own file; a panic's stderr line is DEFERRED to the ordered drain.
  **Verified:** 8-thread ≡ 1-thread (`RAYON_NUM_THREADS`) byte-identical stdout+stderr+exit on
  the 36 corpus fixtures (52 real diagnostics) AND on 400 real corpus files; 10 repeated parallel
  runs → one identical md5; 369 tests + `run.rb` (36 fixtures, 0 FP) + `run_snapshot.rb` +
  `run_corpus.rb` (1200 real files, 0 FP) all green; clippy bin-clean. **Speedup: ~2.4× warm**
  (12 cores, 7749 mastodon+gitlab `.rb`: serial ~0.91s → parallel ~0.37s; the ~0.02s RBS-load
  floor is negligible, so this is ~2.5× on the parallelizable work). Sublinear vs core count
  because stage 2 + output collection stay serial (by design — §9's "pre-pass frozen" model).
  rayon 1.12 + crossbeam/either added to `Cargo.lock` (offline-cached); `RAYON_NUM_THREADS=1`
  forces serial.
- ✅ **`RIGOR_TIMING` stage-breakdown observability (2026-07-01).** `analyze_files` emits a
  one-line per-stage breakdown to stderr under the `RIGOR_TIMING` env gate (invisible by default —
  the harness never sets it, so byte-exact output + 0-FP are unaffected): `index-load` /
  `stage1(parse+lower)` / `stage2(build_project)` / `stage3(analyze)` / `sort` / `total` / file +
  thread count. Fits the "performance prototype" positioning (benchmarkable). **Profiling finding
  (7749 mastodon+gitlab `.rb`, 12 cores, warm, ~296ms total):** stage1 ~152ms/51% (parallel, 3.3×
  — I/O + libprism-FFI bound, the scaling ceiling), **stage2 ~77ms/26% (SERIAL — the next
  bottleneck)**, stage3 ~46ms/16% (parallel, 5.3× — pure-Rust analysis scales best), index ~17ms,
  sort ~2µs.
- **Stage-2 parallelization assessed + DEFERRED (low EV / high risk).** `build_project`'s heavy
  cost is NOT the one parallelizable pass: Pass 3 (`infer_method_returns`, the only Typer-running
  pass, and order-INDEPENDENT in outcome so it's safely map-reducible) measures only **~20ms of
  the ~77ms** (~7% of total) — parallelizing it buys ≤1.1× for real risk. The remaining ~55ms is
  Passes 1/1b/1c/2 (4 structural AST walks) which ASSIGN `ClassId`s by `names`-Vec insertion order
  (`add_source`/`register`) — order-SENSITIVE, so parallelizing them would need a deterministic
  serial ID-assignment merge to stay byte-identical, a large risk to the zero-FP cross-file
  keystone for a ~1.2× ceiling. **Verdict: the headline file-level parallelism (2.4×) is the
  high-value win; stage-2 is deferred.** **Deferred** (not needed for this slice): per-worker
  incremental merge, severity re-stamp post-pool, `workers:` config precedence, stage-2
  parallelization. (Salsa deferred — empirical trigger only.)

### 10. Plugins — `lib/rigor/plugin/` + `plugins/` (31) → (ADR-0013/0027)
- ✅ **First plugin slice landed — `rigor-activesupport-core-ext` (PURE-RBS via
  `signature_paths:` ingest, config-gated; ADR-25).** The highest-leverage Rails plugin
  ships NO analyzer code: its whole contribution is a bundled `core_ext.rbs` that reopens
  core classes (Object/String/Integer/Float/Time/Date/DateTime/Array/Hash/Enumerable/Nil/
  True/FalseClass) with ~40 of the most-flagged ActiveSupport selectors (`blank?`/`squish`/
  `underscore`/`pluralize`/`minutes`/`days`/`current`/`symbolize_keys`/`second`/…). The
  reference's RBS is **vendored byte-for-byte** (`crates/rigor-index/vendor/plugins/`, see
  its `PROVENANCE.md`), embedded via `include_str!` (`rigor-index/src/plugins.rs`), and
  ingested on top of the embedded core via the SAME `ruby-rbs` parser + `Builder::merge`
  reopen-union seam (`CoreData::load_with_plugins`). **Config-gated end-to-end:**
  `.rigor.yml`'s `plugins:` → `Config::plugins` → `CoreIndex::with_plugins(&cfg.plugins)`
  (only at `main.rs`'s `check` index build). Gem-name ↔ manifest-id normalised in
  `bundled_plugin()` (`rigor-activesupport-core-ext` and `activesupport-core-ext` both
  resolve); unknown ids are silently ignored. The instance `CoreIndex::method_return /
  _with_block / method_arity` (routed through `self.index` in `rigor-infer`/`rigor-rules`,
  replacing the plugin-unaware process-global free fns) carry the plugin returns into
  chained typing, so `"x".squish.foo` witnesses `foo' for String` — byte-identical to the
  reference with the plugin loaded. **Zero-FP & gating proven:** the default (no-config)
  corpus stays **3829 files / 542 matched / 0 FP** (byte-unchanged), and the 16 existing
  fixtures are untouched; the win shows only on the plugin-enabled fixture pair (A: chained
  witness with config; B: gate guard — the 3 direct calls still flag with no config). The
  harness gained a minimal sidecar mechanism: a fixture `NN.rb` with a sibling `NN.rigor.yml`
  passes `--config` to BOTH tools (reference also gets `-I <plugin lib>`; sidecar uses the
  **gem-name** spelling, the only form the reference can `require`).
- ⬜ **Deferred** (this slice needed NONE of it): the Plugin trait
  (`node_rule`/`dynamic_return`/`type_specifier` + NodeContext + FactStore topo-sort +
  `open_receivers` + manifest fields beyond `signature_paths:`); the sidecar-hosted Ruby
  plugin runner (strangler default) + IoBoundary/TrustPolicy; the other ~30 plugins;
  native-Rust analyzer ports, hottest-first (Rails family). **This is where most remaining
  real-code coverage lives.** Next pure-RBS candidates by survey frequency: the rest of the
  Rails family (`rigor-rails-*`), then the analyzer-bearing plugins once the trait lands.

### 11. CLI commands — `lib/rigor/cli.rb` → `rigor-cli` (ADR-0015)
- ✅ Full surface presented; unimplemented commands report clearly. ✅ `check`
  (`--format text|json|github|sarif|gitlab|checkstyle|junit|teamcity`, `--config <path>`,
  project two-phase pass, inline + config suppression, CI auto-detection on `--format text`).
- ✅ `baseline` — `generate [--match-mode rule|message] [--output PATH] [--force] [--config PATH]
  <file...>` (byte-compatible `.rigor-baseline.yml`) · `dump [--baseline PATH]`. `regenerate`/
  `drift`/`prune` recognized but deferred (need `configuration.paths`).
- ✅ `type-of` — `[--format text|json] FILE:LINE:COL` (or `FILE LINE COL`). Reuses
  `check`'s parse + `Typer` + top-level env; a span-contains node-at-position lookup
  (deepest covering node) over the owned arena locates the expression, then
  `Typer::type_of` types it. Renders `file:line:col` / `node:` / `type:` (text) or the
  same keys (json). Parity: the `file:line:col` line, error messages, and exit codes
  (1 missing-file / no-expr, 64 out-of-range / usage) are byte-identical; the `type:`
  line uses the SAME spelling `check`'s `receiver_type` uses (a Constant renders its
  value `"hello"`, matching the reference's `"hello"`/`"HELLO"`). Intentional diffs vs
  the reference: `node:` names the rigor-rs owned `Node` variant (`StringLit`) not the
  Prism class (`Prism::StringNode`); an array literal types to the `Array` nominal not
  the value-pinned `[1, 2, 3]`; json keys are serde-ordered and the reference's
  `erased`/`fallbacks`/`--trace` fields (no `erase_to_rbs` / FallbackTracer in this
  port) are omitted.
- ✅ `explain` — `[--format text|json] [<rule>]`. Static catalogue mirroring the
  reference's `RuleCatalog::ENTRIES` content verbatim (all 19 rules + legacy aliases +
  `call`/`flow`/`assert`/`dump`/`def` family wildcards). Text AND json are
  **byte-identical** to the reference for every canonical id, alias, family, and the
  no-arg index; unknown rule → the reference's two-line stderr + exit 64. (json key
  order is hand-built to match `JSON.pretty_generate`, which serde would alphabetize.)
- ✅ `init` — writes `.rigor.dist.yml` (default; `--path PATH` retargets, `--force`
  overwrites, refuses an existing file without `--force` → exit 1, matching the
  reference's surface + "already exists; use --force to overwrite it" message + the
  "Created … / Next steps:" stdout shape). **Intentional difference:** the reference
  serialises its full `Configuration::DEFAULTS` (~30 keys, mostly preview surface);
  rigor-rs's template documents ONLY the four keys its loader honors (`disable:` /
  `exclude:` / `plugins:` / `baseline:`) so it never advertises keys rigor-rs silently
  drops — truthful to the standalone sound subset. The file round-trips through
  `Config::load`.
- 🟡 `doctor` — environment/setup diagnostic. Reports: config discovery (found+parsed /
  malformed→WARN / absent), the **active RBS source** (embedded vendored set vs
  `RIGOR_RBS_CORE_DIR` override vs stub→FAIL) **+ class count** (audit-R1), the bundled
  plugins + which the discovered config enables (config-gated), and the implemented
  (sound-subset) rule set. `[PASS]`/`[WARN]`/`[FAIL]` line shape + exit 0/1 borrowed
  from the reference (ADR-77). **Deferred** (no `configuration.paths` model in rigor-rs's
  CLI yet): the reference's scoped-`check` baseline-drift + Rails-unconfigured checks, and
  a `--format json` (the reference has one; human format first). Intentionally divergent:
  the reference's doctor is a findings classifier over a real analysis pass; rigor-rs's
  surfaces the standalone/embedded setup state instead.
- ✅ `plugins` — `[list] [--config PATH]`. Lists the bundled plugins rigor-rs ships
  (`activesupport-core-ext`) and, per plugin, whether the discovered `.rigor.yml`'s
  `plugins:` enables it (config-gated; reuses `rigor_index::plugins`, the same source
  `doctor` uses). Borrows the reference's `[OK]`/loaded-vs-available framing + exit-0
  (non-`--strict` advisory) semantics; surfaces the vendored RBS bundle's `.rbs` count
  as the `signature_paths:` analog. **Intentional difference:** rigor-rs ships only
  native PURE-RBS bundled plugins (no gem loader, no gem-installed plugins), so the
  listing differs from the reference's gem-based activation report. **Deferred:**
  gem-load status, signature-path filesystem inspection, the ADR-37 `--capabilities`
  catalogue, `--format json`, `--strict` (no gem loader / manifest in the standalone
  build).
- 🟡 `docs` — `[<rule-id>]`. The reference's `docs` (ADR-74) is a bundled-MANUAL
  renderer (gem-shipped `docs/install.md` + `docs/manual/*.md` + `docs/handbook/*.md`
  + `llms.txt`, with `--list`/`--path`). The standalone build bundles none of that
  prose, so this implements the tractable CORE over the documented content rigor-rs
  *does* ship — the rule catalogue (the `explain` `RuleCatalog` port): `rigor docs`
  lists the documented rules (id + summary); `rigor docs <rule-id>` prints that rule's
  documentation (the same per-rule reference `explain <rule-id>` renders — canonical
  id, legacy alias, family token all resolve); unknown id → stderr error + exit 64
  (reuses `explain`'s contract). **Deferred** (no bundled prose corpus): the manual /
  handbook / install pages, the `llms.txt` index, and the `--list`/`--path` flags that
  address them; `docs` prints a note pointing at the web manual instead (no fabricated
  content).
- ✅ `lsp` — `rigor lsp [--transport=stdio] [--log=PATH]` (see §12).
- ✅ `mcp` — `rigor mcp` read-only MCP server over stdio (`check` + `type_of` tools; see §12).
- ⬜ `annotate` · `diff` · `triage` ·
  `coverage` (incl. `--protection`, ref ADR-63/70) · `plugin` ·
  `sig-gen` (ref ADR-14) · `skill`/`describe` ·
  `trace` · `type-scan`.

### 12. Editor / agent servers (ADR-0029)
- ✅ **LSP server v1 landed (2026-07-01) — `rigor lsp --transport=stdio`.** An in-process
  Language Server built on the sync **`lsp-server`** scaffold (stdio JSON-RPC framing + message
  loop; NO async runtime / tokio — chosen precisely to keep the single self-contained binary
  runtime-free) + **`lsp-types`** 0.97 protocol structs (`crates/rigor-cli/src/lsp.rs`, wired at
  `main.rs`'s `Some("lsp")`). **Capabilities advertised:** `textDocumentSync = FULL` +
  `hoverProvider`. **Features:** (1) live **diagnostics** — `didOpen`/`didChange` run the EXACT
  `check` single-file path (parse → lower → single-file `SourceIndex` → `analyze_with_source`) +
  inline `# rigor:disable` + config `disable:` suppression, mapped to LSP `Diagnostic`s
  (`source="rigor"`, `code=<rule id>`, severity error→Error/warning→Warning/info→Information per
  ADR-0029); `didClose` publishes an empty set to clear markers. (2) **hover** — NODE-AWARE
  markdown cards (enriched 2026-07-01): a `Call` shows `receiver#method → return` + the RBS arity
  envelope, a `class`/`module`/`def` name shows its header/signature (`class Foo < Bar` /
  `def name(params)`), a constant shows `Name : type`, else the inferred type + node kind + hover
  range. Reuses the `type-of` node-locator + `Typer` + `CoreIndex` (`class_name_of`/`method_arity`).
  (The def-hover work also fixed a latent `locate_node` wrapper tie-break: a `Program`/`Statements`
  container sharing its span with a sole child no longer wins — improves `type-of` too.) **Two-tier essence:** the RBS
  environment (`CoreIndex::with_plugins`) + config-derived suppression set are built ONCE at
  startup and reused across every request, so the per-keystroke cost is a single-file
  parse+lower+analyze, never the RBS-load floor. Panic-isolated (ADR-0016): a malformed buffer
  yields no diagnostics/hover, never a crash. LSP is a NEW surface (no Ruby-reference byte-parity
  harness) — correctness comes from reusing the `check`/`type-of` path verbatim. **Verified:** +7
  unit tests (UTF-16 position round-trip incl. multibyte `é`/`𐐷`, diagnostics + inline
  suppression + severity/source/code mapping, hover type report, unknown-buffer null); an
  end-to-end stdio smoke session (initialize handshake → didOpen diagnostics → hover → clean
  shutdown/exit 0) and a didChange/didClose lifecycle (open-clean→0, change-typo→1, close→0).
  376 tests total, run.rb + run_snapshot.rb PASS (0 FP), clippy bin-clean. Deps fetched into the
  offline cache (`lsp-server` 0.8, `lsp-types` 0.97 + crossbeam-channel/fluent-uri/serde_repr).
- ✅ **LSP v2 — `textDocument/completion` landed (2026-07-01).** Member-access method completion,
  triggered on `.` and `:` (advertised `completionProvider`). **New index enumeration API**
  (`rigor-index`): `CoreIndex::instance_method_names` (own + inherited over the flattened ancestor
  chain + `alias` names) and `singleton_method_names` (own/inherited `def self.x` + extended-module
  instance methods + singleton aliases + the `Class`/`Module`/`Object`/`Kernel`/`BasicObject`
  instance surface); sorted/deduped, advisory (no completeness gate — completion isn't a witness).
  **Receiver resolution is robust to incomplete input via placeholder injection:** a stub method
  name is spliced in at the cursor (dropping any half-typed prefix — the client filters the full
  set), so the parser yields a `Call { receiver, method: <stub> }` regardless of what's typed; the
  receiver node is typed with the SAME `Typer` hover/check use, and its class drives instance-
  (`class_name_of`) vs singleton- (`Type::Singleton` → `class_name_for_id`) enumeration. A
  `Dynamic`/project/unknown receiver ⇒ empty (no guess). **Verified:** +6 LSP completion tests
  (String/Integer instance methods, half-typed-prefix, `Time.` singleton `now`/`new`, non-member
  and Dynamic-receiver empties) + 2 index enumeration tests; an e2e stdio completion session (269
  String methods incl. `upcase`/`length`). **The v2 index+completion code is DEAD CODE for the
  diagnostic path — proven byte-identical `check` output on 1236 real mastodon files vs committed
  v1 (both 397 diags).** 384 tests, run.rb + run_snapshot.rb PASS (0 FP), clippy index-lib +
  cli-bin clean.
- ✅ **LSP v3 — `textDocument/documentSymbol` landed (2026-07-01).** A nested outline
  (classes/modules/methods) built from the lowered AST: every `ClassDef`/`ModuleDef`/named
  `Definition` becomes a `DocumentSymbol` (`Class`/`Module`/`Method` kind), nested by BYTE-SPAN
  CONTAINMENT (a method nests under its class; nested classes nest too) — the same span-containment
  approach the toplevel-def/override rules use, since the arena is flat. `range` = the whole def
  span, `selectionRange` = the name token (`name_span` for methods). Advertised
  `documentSymbolProvider`. +2 tests (nested class→methods + module; empty for a script-ish file)
  and an e2e stdio session. 386 tests, harnesses PASS (0 FP), clippy-clean.
- ⬜ **Deferred (LSP v4+):** `::` constant/namespace completion (currently `::` yields singleton
  methods, not nested constants); Union-receiver method intersection + private-method visibility
  filter; the full two-tier `ProjectContext` (generation counter,
  `didChangeWatchedFiles`/`didChangeConfiguration` invalidation), cross-file project context for
  open buffers, a pre-warmed worker pool, 200ms `didChange` debounce, temp-file `BufferBinding`,
  incremental UTF-16 `didChange` sync, `--log` wiring, and TCP/socket transport.
- ✅ **MCP server landed (2026-07-01) — `rigor mcp`.** A read-only Model Context Protocol server
  over stdio so an AI agent can analyse Ruby with rigor as a tool. **Transport hand-rolled on
  `serde_json`** (MCP stdio = newline-delimited JSON-RPC 2.0, one message per line — simpler than
  LSP's `Content-Length`) — no async runtime, no new dependency, offline-safe. **Tools (read-only,
  operate on source passed in the call — the server never touches the filesystem):** `check`
  (analyse Ruby source → diagnostics JSON, the exact `check` path incl. inline `# rigor:disable` +
  config suppression) and `type_of` (inferred type at a 1-based line/column, reusing the `type-of`
  probe). Protocol: `initialize` (echoes the client's `protocolVersion`, advertises `tools`,
  identifies `rigor-rs`), `notifications/initialized`, `ping`, `tools/list`, `tools/call`; unknown
  method → JSON-RPC `-32601`, a tool-level failure → an `isError` result (visible to the model, MCP
  convention). Same two-tier essence as the LSP server (RBS index + config built once, reused per
  call) and panic isolation. **Verified:** +9 unit tests (initialize echo/default, tools/list
  schema, check-typo + inline-suppression, type_of, unknown-tool/missing-arg isError, unknown-method
  JSON-RPC error) + an e2e stdio session (initialize → tools/list → `check` 1 diagnostic → `type_of`
  `"HI"` → unknown-tool error). MCP is a purely additive subcommand (no `check` impact).
  **Tools added (2026-07-01):** `explain` (rule-catalogue lookup — no arg → the 19-rule index, or a
  rule/alias/family token → full metadata; reuses `explain`'s `ENTRIES` via `explain::explain_json`)
  and `outline` (nested class/module/method structure with 1-based line ranges; reuses the shared
  `outline::build` — the SAME nesting builder the LSP `documentSymbol` handler now uses, so the
  span-containment logic lives in one place: `crates/rigor-cli/src/outline.rs`). 400 tests, run.rb +
  run_snapshot.rb PASS (0 FP), clippy-clean. Deferred: resources/prompts capabilities.
- **NOTE (reference-harness flakiness, observed 2026-07-01):** `run_corpus.rb` (the LIVE
  differential harness) gave swinging FP counts (70/0/2/0) on a DETERMINISTIC file set
  (`Dir[...].sort.first(limit)`) with a provably-deterministic rigor-rs binary — i.e. the Ruby
  v0.2.6 reference oracle is itself nondeterministic across runs (transient per-file
  under-emission). The reference-free **`run_snapshot.rb`** (pinned snapshots) is the reliable
  0-FP gate and stays green; treat live-corpus FP counts as advisory, and confirm any apparent
  regression by diffing rigor-rs's OWN output across builds (as done for v2 above).

### 13. Distribution (ADR-0010)
> **Version is now `0.0.1`** — the v0.0.1 first-release target (see "▶▶ v0.0.1 RELEASE PREP"
> at the top). The distribution scaffolding below was authored at `0.1.0` and lowered to
> `0.0.1` for the first release; the single-source `[workspace.package] version`, the gem
> `version.rb`, and the Homebrew formula are all `0.0.1` (`rake version:check` green). Some
> dated proof-run artifact names below still read `0.1.0`; re-running them now yields `0.0.1`.
- ✅ **Release-pipeline foundation landed (purely additive — no dev-loop/analysis change).**
  - Version set to **0.0.1** (single source: `[workspace.package] version`, inherited by all
    crates; the first release is `v0.0.1`). `repository`/`license` (**AGPL-3.0** — note this DIFFERS from the reference gemspec's MPL-2.0; LICENSE is the verbatim GNU AGPL v3) added to
    `[workspace.package]`; `description`/`homepage` + the `[package.metadata.binstall]` block on
    `rigor-cli`. **NOTE:** `repository`/`homepage` URL `https://github.com/rigortype/rigor-rs` is a
    PLACEHOLDER (no git remote configured yet) — confirm when the repo is published.
  - `rigor version` / `--version` / `-v` / `-V` command — prints `rigor <version>` (mirrors the
    reference `lib/rigor/cli.rb`), exit 0; sourced from `env!("CARGO_PKG_VERSION")`. `doctor` now
    shows `v0.1.0` automatically.
  - cargo-binstall metadata: `pkg-url = "{ repo }/releases/download/v{ version }/rigor-{ version }-{ target }{ archive-suffix }"`,
    `pkg-fmt = "tgz"`, `bin-dir = "rigor{ binary-ext }"`.
  - `.github/workflows/release.yml` — tag-triggered (`v*.*.*`) 4-target cross-compile matrix
    (aarch64/x86_64 macOS native, x86_64 Linux native, aarch64 Linux via `cross`); builds
    `--release --locked`, smoke-tests `rigor doctor` on native targets, packages
    `rigor-<version>-<target>.tar.gz` (bare binary + LICENSE at root) + `.sha256` sidecar, uploads
    via `softprops/action-gh-release@v2`. Action versions pinned. End-to-end CI validation (the
    actual cross-builds + asset upload) requires a real tag/CI run — out of local scope.
  - **Static libprism link is ALREADY DONE:** `ruby-prism`/`ruby-rbs` are `-sys` crates that
    statically compile vendored C via `cc` + `bindgen`, and the core RBS is embedded (ADR-0007).
    `otool -L target/release/rigor` shows only `libSystem` — the binary is self-contained.
- ✅ **Precompiled-binary gem scaffold landed (ADR-0010 PRIMARY channel — purely additive,
    everything under `gem/` + a downstream `gem`/`gem-fallback` job appended to `release.yml`;
    the existing `build` job is byte-unchanged, no `crates/`/`Cargo.toml`/dev-loop change).**
  - **Mechanism:** platform-specific precompiled gems (4 variants + a `ruby` fallback). ONE
    gemspec (`gem/rigortype-rs.gemspec`, platform-neutral); the Rakefile sets `spec.platform` per
    build. Each platform gem bundles the matching native binary at `libexec/rigor`; the fallback
    bundles none. Module name **`RigortypeRs`** (consistent across `lib/`, gemspec, sig, tests).
  - **Name `rigortype-rs`** (NOT `rigortype` — a 0.1.0 over the reference's 0.2.x (pinned v0.2.6)
    would be a
    downgrade; and per ADR-0001 rigor-rs COEXISTS with the Ruby mainstream — there is NO planned
    `rigortype` name takeover, so the distinct name is permanent). Both gems install a `rigor`
    exe → README warns not to install both in one env.
  - **Version lockstep:** `version.rb` `VERSION="0.1.0"`, enforced by `rake version:check` reading
    `[workspace.package] version` from `../Cargo.toml` (single source of truth). Green.
  - The shim (`exe/rigor`) `exec`s the bundled native binary with ARGV passthrough (process-
    replacing, no Ruby require path). `RigortypeRs::Binary.path` resolves `libexec/rigor`, raises
    `NotFound` with guidance (supported platforms + `cargo binstall`/`brew`) when absent, defensive
    chmod. The native binary is NOT committed — only `libexec/.keep` (staged at build/test time).
  - **Gem::Platform map (versionless for CI/published): arm64-darwin / x86_64-darwin /
    x86_64-linux / aarch64-linux** — note macOS arm64 is `arm64` in Gem::Platform but `aarch64` in
    the Rust triple. The local proof builds a HOST-exact gem (`arm64-darwin-23`) so `gem install`
    selects it on this machine.
  - **Local end-to-end PROOF (ran, all green):** staged `target/release/rigor` → `rake build:local`
    built `rigortype-rs-0.1.0-arm64-darwin-23.gem` (zero warnings); `gem specification` shows
    name/version/platform/executables=[rigor]/files incl `libexec/rigor`; `gem install --local`
    into a temp GEM_HOME → `rigor --version` prints `rigor 0.1.0`; the KEY GATE
    `diff <(gem-shim check) <(bare-binary check)` is EMPTY (shim === bare binary); the NotFound
    negative test (binary removed) emits the guidance message. Unit test
    `spec/binary_resolution_spec.rb` (minitest, 4 runs/23 assertions): path resolves when present,
    `NotFound`+guidance when absent, ARGV passthrough via a stub binary. Temp GEM_HOME + staged
    binary cleaned up; only `libexec/.keep` committed.
  - **CI gem job (`release.yml`, `needs: build`):** matrix over the 4 targets × versionless
    Gem::Platform; downloads the matching `rigor-<v>-<target>.tar.gz`, stages → `gem/libexec/rigor`,
    `rake version:check`, `rake build:platform[<gem-platform>]`, smoke-installs + runs
    `rigor --version` on arch-matched rows (macOS + x86_64-linux; aarch64-linux smoke skipped). A
    `gem-fallback` job builds the `ruby` gem. `gem push` is GATED behind a `RUBYGEMS_API_KEY`
    secret + a manual `release` environment — never auto-pushes.
  - **DEFERRED:** RubyGems account + API key + MFA setup; the first real tag to validate the
    multi-platform CI build/push end-to-end; musl + Windows targets; sidecar
    Ruby auto-detection. (The `rigortype` name takeover is NOT deferred but NOT planned — rigor-rs
    coexists with the Ruby mainstream per ADR-0001.)
- ✅ **Homebrew formula scaffold landed (ADR-0010 co-equal channel — purely additive: a new
    `HomebrewFormula/` dir + a downstream `homebrew-formula` job appended to `release.yml`; the
    existing `build`/`gem`/`gem-fallback` jobs are BYTE-UNCHANGED, no `crates/`/`Cargo.toml`/
    dev-loop/`gem/` change).**
  - **Template:** `HomebrewFormula/rigor.rb` — `class Rigor < Formula`, `desc`/`homepage` (the
    repository URL)/`license "MPL-2.0"`/`version "0.1.0"`; per-OS/per-arch blocks
    (`on_macos`+`on_arm`/`on_intel`, `on_linux`+`on_arm`/`on_intel`) each with the matching
    `url ".../releases/download/v#{version}/rigor-#{version}-<target>.tar.gz"` + `sha256`.
    Arch→target map: macOS arm → `aarch64-apple-darwin`, macOS intel → `x86_64-apple-darwin`,
    linux arm → `aarch64-unknown-linux-gnu`, linux intel → `x86_64-unknown-linux-gnu` (consistent
    with the release asset naming + the cargo-binstall `pkg-url`). `def install; bin.install
    "rigor"; end` (bare binary at archive root); `test do` asserts `rigor #{version}` from
    `--version` + a trivial `rigor check`.
  - **Placeholder sha256s** (`0`×64, obvious + prominently commented) — NOT shipped as-is; the CI
    job regenerates them. `HomebrewFormula/README.md` documents the template/CI-fill/deferred-tap
    story.
  - **CI `homebrew-formula` job (`release.yml`, `needs: build`):** downloads the four
    `rigor-<v>-<target>.tar.gz.sha256` sidecars, rewrites `HomebrewFormula/rigor.rb` in place with
    the real version (`${GITHUB_REF_NAME#v}`) + the four real per-target sha256s (a Ruby rewriter
    that matches each placeholder by its target comment/URL; aborts if any `0`×64 survives),
    re-validates with `ruby -c`, and uploads the filled formula as a workflow artifact + attaches
    it to the Release. The **tap push** (`rigortype/homebrew-tap`,
    `brew install rigortype/tap/rigor`) is **GATED/DEFERRED** behind a `HOMEBREW_TAP_TOKEN` secret
    + the manual `release` environment (mirrors the gem `gem push` gate) — never auto-runs.
  - **Local verification (ran):** `ruby -c HomebrewFormula/rigor.rb` → Syntax OK; `brew style`
    (in a throwaway tap, since brew refuses out-of-tap formulae) → no offenses; `brew audit --new`
    → only the expected placeholder/no-repo findings (URLs 404 — no release/repo yet; `version`
    redundant-with-URL is a style preference, kept deliberately for DRY interpolation). The CI
    rewriter was exercised end-to-end with fake sidecars: each target's sha lands in the correct
    arch block, version substituted, placeholder-survival guard fires on a missing sidecar.
  - **DEFERRED (Homebrew):** the `rigortype/homebrew-tap` repo + a `HOMEBREW_TAP_TOKEN`; the first
    real tag to produce real sha256s; sidecar auto-detection. **musl/Windows are NOT added to the
    formula by design** — Homebrew on Linux uses glibc (not musl) and has no Windows support, so
    `HomebrewFormula/rigor.rb` stays macOS + linux-gnu (left BYTE-UNCHANGED in slice 4).
- ✅ **Distribution slice 4 — musl + Windows binary targets WIRED (2026-06-27; purely additive
    CI/packaging config — no `crates/` source change; the existing 4 `build` rows + the
    `gem`/`gem-fallback`/`homebrew-formula` jobs are byte-unchanged; the only `Cargo.toml` change
    is a binstall packaging-metadata override).**
  - **Binary matrix (`release.yml` `build` job): +3 rows.** `x86_64-unknown-linux-musl` +
    `aarch64-unknown-linux-musl` build via **`cargo-zigbuild`** (zig supplies the musl C
    cross-toolchain the `-sys` crates' `cc` needs; bindgen runs on the ubuntu host against the
    apt-installed libclang) — gated by a new `use_zigbuild: true` matrix flag (mirrors the
    `cross` flag pattern): an "Install zig + cargo-zigbuild" step (`pip3 install ziglang` +
    `cargo install cargo-zigbuild --locked`) and a `cargo zigbuild --release --locked --target`
    build step, both `if: matrix.use_zigbuild == true`. musl Linux is **fully static** (an
    ADR-0010 goal). Packaged as `.tar.gz` (bare `rigor`) like the others; smoke SKIPPED (uniform
    with the cross/musl skips). `x86_64-pc-windows-msvc` runs **natively** on `windows-latest`
    (rustup default MSVC toolchain; `LIBCLANG_PATH=C:\Program Files\LLVM\bin` set for bindgen),
    `cargo build --release --locked --target` (the existing Build-cargo step, now gated
    `&& matrix.use_zigbuild != true`, also covers Windows), packaged as a **`.zip`**
    (`rigor-<v>-x86_64-pc-windows-msvc.zip`, `rigor.exe` + LICENSE) via PowerShell
    `Compress-Archive` + a `Get-FileHash` `.sha256` sidecar; smoke `rigor.exe --version` runs
    natively. The shared smoke/Package steps were tightened with `if:` guards
    (`runner.os != 'Windows'`, `matrix.use_zigbuild != true`) so the original 4 rows' behavior is
    unchanged; the `action-gh-release` upload glob gained the two Windows `.zip`/`.zip.sha256`
    entries (empty on non-Windows rows, so the tar.gz upload is unaffected).
  - **binstall consistency:** added `[package.metadata.binstall.overrides."x86_64-pc-windows-msvc"]`
    with `pkg-fmt = "zip"` in `crates/rigor-cli/Cargo.toml` (unix targets keep the default `tgz`);
    `{ archive-suffix }` in the inherited `pkg-url` then resolves to `.zip` for Windows. Confirmed
    `cargo build/test --offline` (352) + clippy (`-D warnings`) stay green after the metadata add.
  - **Gem matrix (`gem` job): +2 musl rows.** `x86_64-unknown-linux-musl` → `x86_64-linux-musl`,
    `aarch64-unknown-linux-musl` → `aarch64-linux-musl` (VERIFIED:
    `ruby -e 'Gem::Platform.new("x86_64-linux-musl")'` → `x86_64-linux-musl`, aarch64 likewise —
    musl Ruby hosts e.g. Alpine report `*-linux-musl`). Both `smoke: false` (musl binary can't run
    on the glibc x86_64 runner). The **Windows gem is DEFERRED** (commented in-job): needs a mingw
    `Gem::Platform` (`x64-mingw-ucrt`) + packaging an MSVC `.exe` into it is finicky and lower-value
    than the binstall/`.zip` channel that already serves Windows.
  - **Homebrew: NO change (by design)** — see the DEFERRED note above; `HomebrewFormula/rigor.rb`
    left byte-unchanged (glibc-only Linux, no Windows).
  - **Local gates (all green):** `release.yml` YAML parses (`yaml.safe_load`); the original 4
    build rows + gem/gem-fallback/homebrew jobs verified byte-unchanged (diff vs a pre-edit
    backup shows only the 4 sanctioned `if:`/comment edits + additive hunks); `cargo build/test
    --offline` (352), clippy `-D warnings`, `ruby harness/run.rb` (PASS, 0 FP) all green.
  - **REQUIRES A REAL CI TAG RUN TO VALIDATE (the documented caveat, same as the existing
    targets):** the actual `cargo-zigbuild` musl cross-builds + Windows MSVC native build, the
    bindgen-on-host success for both, the `.zip` packaging + `.sha256` sidecar, and the
    asset upload (incl. the broadened glob). None of these are locally runnable (no
    zig/cross/cargo-zigbuild + no Linux/Windows cross-toolchain on this host).

### 14. Parity harness & QA (ADR-0002/0011)
- ✅ `harness/run.rb` (fixture gate, 36 fixtures incl. alias regression, the
  `call.possible-nil-receiver` TP + guarded-negatives pair, the ADR-25
  plugin-enabled / gate-guard pair via sibling-`.rigor.yml` sidecars, the tier-4b
  param-binding witness/decline pair, the `flow.always-truthy-condition`
  witness/adversarial-negatives pair, and the `call.unresolved-toplevel`
  witness/pure-negatives pair) + divergence-registry.
- ✅ `harness/run_corpus.rb` (scaled, real-corpus gate; 2458 files validated 0 FP; `harness/CORPUS.md`).
- ✅ **CI workflow** (`.github/workflows/ci.yml`): `cargo build` + `cargo test` (the
  Ruby-free gates) on push/PR over ubuntu+macos, toolchain pinned to the **1.88** build MSRV
  (forced by the `ruby-rbs` dep's let-chains), `--locked`, libclang for
  bindgen, rust-cache; clippy BLOCKING (`-D warnings`; workspace is clippy-clean, `clippy.toml`
  holds the suggestion-`msrv = "1.85"` for OUR code, below the 1.88 build floor); rustfmt NOT
  enforced (hand-formatted codebase — a recorded decision, **ADR-0032**, with a documenting
  `rustfmt.toml`; `cargo fmt` rejected as a 239-hunk/25-file reformat, and no stable config
  round-trips the hand style). The differential harnesses stay a LOCAL gate (they need the
  reference checkout + real corpora).
- ✅ **Snapshot-mode CI parity** (ADR-0002, §14 track c): shared harness logic in `harness/lib.rb`;
  `harness/snapshot.rb` regenerates `harness/snapshots/NN_name.json` (36 fixtures) from the live
  reference (sorted/pretty → deterministic, `--check` flags drift); `harness/run_snapshot.rb` is the
  reference-FREE gate (loads snapshots + runs the binary + IDENTICAL `(rule,line,column)` comparison);
  a separate `parity` job in `ci.yml` runs it on every PR (setup-ruby, no reference checkout). Snapshot
  mode == live mode (28 matched / 0 FP / 12 missing, identical per-fixture). The live `harness/run.rb`
  regenerates the snapshots and remains the local source-of-truth gate.
- ⬜ Continuous corpus growth (new fixtures per rule/feature).

---

## Cross-cutting status

- ✅ `internal-error` rule id → `:info` (audit R5), excluded from the parity gate (ADR-0016).
- 🟡 Hand-rolled JSON (no serde) — swap to serde + add SARIF/CI formats (§6); serde is available.
- ✅ Real RBS index landed (§3); RBS `alias` + nested-decl + the `Hash#to_json` stdlib FP all fixed.

## External audit (2026-06-26) — all addressed

`…/ruby/rigor/docs/notes/20260626-rigor-rs-design-audit.md` (verdict: structurally avoids the
Pzoom/artichoke/pylyzer traps).
- ✅ **R1** ADR-0008: positioning (standalone = sound subset; full parity needs the sidecar).
  `rigor doctor` now surfaces the standalone/embedded coverage state: the active RBS source
  (embedded vendored set vs `RIGOR_RBS_CORE_DIR` override vs stub) + class count, and the
  implemented rule set as an explicit "sound subset of the reference" line. (The
  "sidecar absent ⇒ reduced coverage" framing is the rule-set line; the deferred sidecar
  itself is still out of scope.)
- ✅ **R2** ADR-0007: RBS now **vendored + embedded at build time** (standalone binary, no runtime
  rbs gem); `RIGOR_RBS_CORE_DIR` retained as the out-of-band stdlib-RBS refresh/override seam.
- ✅ **R3** ADR-0001: positioning stated — rigor-rs is a performance prototype that COEXISTS
  with the Ruby mainstream (Ruby leads; no planned retirement / single-implementation; full
  parity + eventual sync are possibilities, not commitments).
- ✅ **R4** graded at scale — 0 false positives across 2458 real files; the corpus harness stays
  for ongoing regression as rules/inference grow.
- ✅ **R5** internal-error → `:info`.
