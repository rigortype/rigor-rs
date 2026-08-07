# Collection-shape receiver survival — slice spec (2026-08-07)

Census mechanism ([gap census](20260807-gap-census.md), post-merge 2026-08-07
baseline, 1168 gaps / 421 `call.undefined-method`): **49 undefined-method gaps
whose receiver is a collection nominal** (`Array[...]` 20, `Hash[...]` 28,
`Set[...]` 1 — after excluding the 29-row `Hash[Symbol, Dynamic[top]]`
rdoc/mail cluster adjudicated separately as a suspected reference mis-typing).
Row dump: `scratchpad/gaps-v2.json`, filter `rule == 'call.undefined-method'`
and `recv` starting `Array[`/`Hash[`/`Set[`. Corpora: gitlab-foss/lib 35,
mastodon 8, rigor-survey/mail 6.

The cluster is NOT one mechanism. Decomposition (§3) splits it into a
mutation/local-binding core this slice builds (stage 1), a chain-root set
(stage 2), and 30 rows that belong to other tracks or are permanent declines
with named reasons.

## 1. Oracle probes (pin `v0.3.1` = `c39e6675`, fresh temp cwd, `--no-cache`, plugin path pinned)

Recipe (fp_audit's, verbatim): `ruby -I reference/rigor/lib -I
reference/rigor/plugins/rigor-rbs-inline/lib reference/rigor/exe/rigor check
--no-cache <file>` from a fresh `mktemp -d` cwd. Receiver renderings below are
the diagnostic's own `for <T>` text; `rigor type-of` cross-checked m01
(`output` at the use = `Array[Dynamic[top]]`).

### Mutation series (locals inside `def` bodies; `compact_blank`* is absent from config-less RBS)

| probe | shape | reference verdict |
|---|---|---|
| m01 | `output = []` ; `output << 'a'` ; `output << 'b' if c` ; use | **fires** `for Array[Dynamic[top]]` |
| m02 | seed `[]`, `<<` inside `xs.each do … end`, use after | **fires** `for Array[Dynamic[top]]` |
| m03 | `project = {}` ; `project[:k] = v` ×2 ; use | **fires** `for Hash[Dynamic[top], Dynamic[top]]` |
| m04 | seed + `<<`, then rebind `output = x`, use | silent — rebind kills |
| m05 | seed + `<<`, then `output = x if cond`, use | silent — branch-rebind join kills |
| m06 | `a = []; b = a; b << 1` ; use both | **fires both**: `a` `for []` (no alias tracking — the Tuple survives), `b` `for Array[Dynamic[top]]` |
| m07 | seed + `<<`, escape `helper(a)` (unresolved callee), use | **fires** — unknown callee does not widen |
| m08 | `def f(a); a << 1; a.use` (Dynamic seed) | silent |
| m09 | ivar: `@h = {}` in `initialize`, `@h['k'] = v` in another method, use in a third | **fires** `for Hash[Dynamic[top], Dynamic[top]]` — cross-method ivar typing |
| m10 | seed, `<<` inside `while`, use after | **fires** `for Array[1]` |
| m11 | control: seed + `<<`, `.frobnicate_zzz` | **fires** — the mechanism is receiver typing, not AS-specific |
| m13 | `output = x` (Dynamic seed), `<<`, use | silent — mutator on a non-shape carrier is a no-op (`widen_for_mutator` → nil) |
| m14 | `a, b = [], []` ; `a << 1` ; use both | **fires both**: `a` `for Array[Dynamic[top]]`, `b` `for []` — multi-write seeds Tuples |
| m15 | seed + `<<`, block rebind `xs.each { output = nil }`, use | silent — block rebind kills |
| m16 | seed, `output += [1]`, use | **fires** `for [1]` — op-write folds Tuple+Tuple, keeps the literal shape |
| m17 | seed, `push` then `concat(xs)`, use | **fires** `for Array[Dynamic[top]]` |
| m18 | seed (unmutated), `<<` inside `case`/`when` arms, use after | **silent** |
| m19 | seed + straight-line `<<`, then `case` arms `<<`, use | **fires** `for Array[Dynamic[top]]` |
| m20 | seed (unmutated), `<<` inside `if cond … end`, use after | **silent** |

The m18/m20 vs m01/m19 split is the load-bearing finding: a branch-contained
mutation on a NOT-yet-widened seed leaves a `Tuple[] | Array[…]` union after
`Scope#join` (`reference/rigor/lib/rigor/scope.rb:680`), and
`receiver_descriptor` has NO `Type::Union` arm
(`reference/rigor/lib/rigor/inference/method_dispatcher/rbs_dispatch.rb:200-223`)
⇒ dispatch declines ⇒ silent. Once the binding is already the widened nominal
BEFORE the construct, both sides of the join agree and the site fires. Block
bodies are different: `widen_after_block` REPLACES the outer binding
unconditionally (m02 fires from an unmutated seed). `while` (m10) also fires,
but its loop-join edges (`break`/`next`) are unprobed — stage 1 declines loops.

### Chain series (expression receivers; archetype rows)

| probe | shape | reference | rigor-rs today |
|---|---|---|---|
| c01 | `files = Dir['*.db']` ; `files.blank?` | **fires** `for Array[String]` | silent (local in def + `Dir.[]` root) |
| c02 | `Dir.glob(…).reject{}.map{}.sort.to_sentence` | **fires** `for Array[Dynamic[top]]` | silent (root: `Dir.glob` declines) |
| c03 | `(ENV.keys.select{} - base_keys).present?` (`base_keys` in-source) | **fires** `for Array[String]` | silent (root: `ENV` untyped) |
| c04 | `CONST_HASH.keys.index_with(0)` | **fires** `for [:high, :low]` | **fires** `for Array` — shape already works |
| c05 | `{ 'A' => 1 }.merge(x).compact_blank!` | **fires** `for Hash["A" \| Dynamic[top], 1 \| Dynamic[top]]` | fires on the literal form; the real row also needs the local binding |
| c06 | `Resolv::DNS.open { \|dns\| dns.getresources(…).to_a.map{}.compact_blank }` | **fires** `for Array[Dynamic[top]]` | silent (RBS block-param typing — out of slice) |
| c07 | bare `select('id, name').from_zzz(…)` in a class | **fires** `for Array[String]` | silent |
| c08 | `s.to_s.split(':', 2).second` | **silent** — Dynamic-rooted `to_s` declines (confirms the narrowing spec's b1 refutation) | silent |
| c08b | `Base64.decode64(x \|\| '').split(':', 2).second` | **fires** `for Array[String]` | silent (root: `Base64.decode64`) |
| c09 | `(1..).each.lazy.map { … }.next` | **fires** `for Array[String]` (runtime-wrong: receiver is `Enumerator::Lazy`) | silent |
| c10 | `missing = KEYS.filter{}` ; `missing.present?` | **fires** `for Array[:a \| :b]` | silent (local in def; the filter fold itself works) |
| c11 | `YAML.safe_load` + `is_a?(Hash)` guard, use | **fires** `for Hash[String, json::value[String]]` | **fires** `for Hash` (class-narrowing slice) |
| c12 | `buf = secret.ljust(64, "\0")` ; `buf[i] = …` in block ; `buf + "x"` | **fires** `for Hash[Integer, Dynamic[top]]` (runtime-wrong: `buf` is a String) | silent |

Micro-matrix (top-level, both engines): rigor-rs ALREADY fires on
`reject{}`/`map{}`/`filter{}`/`select{}`/`transform_values{}` block folds,
`sort`, `Array#-`, and literal `merge` — the only missing chain ROOTS among
the probes are `Dir.glob` (block/no-block overload divergence declines the
singleton return), `Dir.[]`, `ENV`, and `Base64.decode64`.

Row-pinning probes: u1 — rigor-rs fires on a lexical `CONST.keys.index_with`
but is silent on the same constant via a fully-qualified `::A::B::C::PR` path
(the codequality row); u2 — adding a method-level `rescue` clause silences
rigor-rs's c11 narrowing (the database_config/config_generator rows) while the
reference still fires.

## 2. The ActiveSupport caveat — and the build verdict

Most witnessed methods (`compact_blank`, `index_with`, `index_by`, `present?`,
`presence`, `without`, `deep_merge!`, …) are ActiveSupport, absent from the
config-less RBS both engines run in the gate, so these diagnostics are wrong
at runtime for a Rails app while being correct PARITY behaviour. Judgment:
**build**, for three reasons.

- The mechanism is receiver typing, not AS-specific: m11 (`frobnicate_zzz`)
  fires identically, and several rows witness genuinely undefined methods
  (`quote`, `headers`, the E-bucket rows).
- Under the ADR-72 Gemfile.lock overlay, AS sigs appear on BOTH engines
  simultaneously: those diagnostics vanish from the reference and the gap rows
  vanish with them — parity is preserved in both directions, and no work here
  is invalidated. What remains valuable either way is the typing improvement:
  a collection nominal surviving a mutation/chain feeds every current and
  future receiver-typed rule, `type-of`, and sig-gen.
- The FP-safe subset is cleanly delimitable (§5): every emission shape is a
  probed fire, every unprobed edge declines.

The no-go precedent ([tier-bc](20260717-tier-bc-track-closed.md)) does not
apply: closing these rows requires no FP-safety mechanism to be deleted.

## 3. Decomposition of the 49 rows

| bucket | mechanism | rows | disposition |
|---|---|---|---|
| A | local literal seed, kept-nominal mutation (`<<`/`[]=`; straight-line, block-contained, or pre-widened branch) | 8 | **stage 1** |
| A′ | local bound to an already-working collection chain, used later in the same def (`missing = KEYS.filter{…}`) | 1 | **stage 1** (consumption is mutation-agnostic) |
| C | chain-root gaps: `Dir.glob`/`Dir.[]` singleton overloads, `ENV`, `Base64`, literal/const `merge(Dynamic)` chains, `::`-qualified const path | 9 | **stage 2** |
| D | in-source method-return lookup (receiver comes from calling a project method) | 12 | defer to **ADR-0042 S5** ([spec](20260807-adr0042-s5-return-lookup-spec.md)); several also need stage 1/2 pieces — synergy, not duplication |
| B | ivar collection carriers (cross-method `@h` typing, m09) | 5 | decline — ivar flow substrate is its own future slice |
| E | reference quirks, runtime-wrong (see §6) | 11 | decline permanently; upstream-feedback candidates |
| F | class-narrowing follow-up: narrowing killed by a method-level `rescue` (u2) | 2 | belongs to the narrowing slice's backlog |
| G | RBS block-param binding (`Resolv::DNS.open { \|dns\| … }`, c06) | 1 | decline — heavier mechanism, own mini-spec if ever |

Row-level assignment (paths relative to the survey checkouts):

- **Stage 1 (9)**: mastodon `app/helpers/application_helper.rb:180,:186`
  (straight-line `<<`); gitlab
  `lib/gitlab/background_migration/update_jira_tracker_data_deployment_type_based_on_url.rb:39,:43,:47`
  (each-block `<<`);
  `lib/gitlab/database/migration_helpers/require_disable_ddl_transaction_for_multiple_locks.rb:141`
  (each-block `<<`);
  `lib/bulk_imports/projects/transformers/project_attributes_transformer.rb:26`
  (straight-line `[]=`); `lib/gitlab/duo_agent_platform/config.rb:98`
  (straight-line `[]=` at :69 pre-widens; branch `[]=`s then keep — the m19
  shape); `lib/gitlab/ci/reports/security/finding.rb:52` (A′).
- **Stage 2 (9)**: `lib/authz/permission_groups/resource.rb:56` (2a
  `Dir.glob`); `lib/prometheus/cleanup_multiproc_dir_service.rb:13` (2a
  `Dir.[]` + stage-1 local); `lib/backup/targets/database.rb:235,:246` (2b
  `ENV`); `lib/api/helpers/packages_manager_clients_helpers.rb:33` (2c
  `Base64`); `lib/gitlab/workhorse.rb:306` (2d literal-`merge` chain + local);
  `lib/authn/token_field/generator/routable_token.rb:61` (2d const-hash
  `merge`/`transform_values` chain); psych `ext/psych/extconf.rb:29` (2d
  literal with conditional splat — prediction uncertain);
  `lib/gitlab/ci/reports/codequality_reports.rb:50` (2e `::`-qualified
  constant path, u1).
- **D (12)**: `lib/tasks/ci/job_tokens_task.rb:95,:135,:198,:231`;
  `lib/tasks/gitlab/permissions/routes/docs_task.rb:110`;
  `lib/gitlab/github_import/importer/single_endpoint_issue_events_importer.rb:119`;
  `lib/gitlab/database.rb:74,:86`;
  `lib/import/user_mapping/reassignment_csv_validator.rb:63`; mastodon
  `app/lib/translation_service/deepl.rb:27` (×2), `app/models/account.rb:317`.
- **B (5)**: `lib/gitlab/ci/config/external/processor.rb:51,:57`;
  `lib/gitlab/graphql/queries.rb:114`;
  `lib/gitlab/import_export/base/relation_factory.rb:200`; mastodon
  `app/lib/request.rb:192`.
- **E (11)**: §6. **F (2)**: `lib/gitlab/patch/database_config.rb:65`,
  `lib/gitlab/redis/config_generator.rb:79`. **G (1)**: mastodon
  `app/lib/domain_resource.rb:19`.

## 4. Reference semantics to mirror (at the pin)

`reference/rigor/lib/rigor/inference/mutation_widening.rb`:

- `:70 ARRAY_MUTATORS` / `:82 HASH_MUTATORS` — per-shape mutator tables (NOT a
  union: a Hash-only mutator on a Tuple is a no-op); `:93
  PURE_SELF_RETURNERS` never widen.
- `:209 widen_for_mutator` — widening applies ONLY to `Tuple` / `HashShape` /
  empty-witness `Difference` carriers; a non-shape (Dynamic) carrier is a
  no-op (m13's silence), and an already-widened `Nominal` has "no precision to
  lose" (why m01's elements stay `Dynamic[top]` despite String appends — the
  FIRST `<<` on `Tuple[]` fixes the element at untyped).
- `:251 widen_tuple` → `Nominal[Array, [union(elements)]]`; `:265
  widen_hash_shape` → `Nominal[Hash, [K, V]]` — **the nominal is KEPT**, this
  is the whole slice.
- `:112 widen_after_call` (straight-line), `:144 widen_after_block` — block
  mutations of captured locals (`depth >= 1`) REPLACE the outer binding,
  unconditionally ("blindly propagating is sound"); the element-join refinement
  is `:309`-`:427` (ADR-56 slice C) — rigor-rs does not need it for
  undefined-method witnessing (class-only lookup).
- Branch join: `scope.rb:680 Scope#join` unions per name;
  `method_dispatcher/rbs_dispatch.rb:200 receiver_descriptor` has no
  `Type::Union` arm ⇒ a `Tuple[] | Array[…]` receiver silently declines
  (m18/m20). `Tuple`/`HashShape` project to `Array`/`Hash` descriptors
  (`:209`-`:212`) — bare-Tuple receivers fire too (m06, m14).

## 5. rigor-rs design

### Stage 1 — collection-typed-local snapshot pass

**A per-call snapshot pass, not a `TypeEnv` binding** (same shape as ADR-0038
nil and the class-narrowing pass): `collection_shape_snapshots(ast, …) ->
HashMap<NodeId /*call*/, &'static str /*"Array"|"Hash"*/>` in
`crates/rigor-infer/src/lib.rs`, modeled directly on
`class_narrowing_snapshots` (`:2471`) and its `class_flow_stmt` walker
(`:2508`), consumed in `analyze_with_source_and_folder`'s per-call loop
(`crates/rigor-rules/src/lib.rs:558-586`) by a `check_collection_call` that
fires `call.undefined-method` ONLY (pitfall 7 of the narrowing spec: no
wrong-arity/ATM wiring).

The walker (shares the skeleton; a parallel pass or a widened
`class_flow_stmt` — implementer's choice, but the narrowing pass's gates must
stay byte-identical):

1. Thread a `tenv` through def bodies statement-wise, binding
   `LocalVariableWrite`/`MultiWrite` names to `type_of(RHS)` — this already
   types literal seeds (`Tuple`/`HashShape`) AND chain results (`missing =
   KEYS.filter { … }` → `Nominal[Array]` via the existing tier folds).
2. **Keep-nominal mutator widening** (the delta vs today): on a call
   `local.<m>(…)` where `local`'s binding is `Tuple` and `m ∈ ARRAY_MUTATORS`,
   rebind to `Nominal[Array]`; `HashShape` and `m ∈ HASH_MUTATORS` →
   `Nominal[Hash]`. Split `MUTATOR_METHODS` (`crates/rigor-infer/src/lib.rs:3000`,
   currently the union) into the reference's two tables for this pass; the
   flow-passes' use of the union stays as is (over-widening there is safe).
   A mutator on any other carrier: no rebind (m13). Elements are not tracked —
   `args: vec![]` — undefined-method needs only the class.
3. Block-contained mutations (`each` et al.): after typing a block-bearing
   call, walk the block body for mutator calls on outer locals and apply the
   keep-nominal rebind to the outer `tenv` (mirror `widen_after_block`; m02,
   m19). Rebinds inside the block body still kill via the existing
   `collect_flow_writes` machinery (m15).
4. `If`/`Case`-contained mutator writes: after the construct, the local keeps
   its binding IFF the pre-construct binding was already the SAME kept-nominal
   (identical `TypeId` join — m01/m19 fire); otherwise widen to `Dynamic`
   (m18/m20 silences — the load-bearing decline). Never model the union.
5. Unmodeled constructs (`while`/`until`, `begin`/`rescue` at method level
   (u2), logical writes, safe-nav and everything under it, op-writes (m16 —
   coverage loss only)): widen every contained write to `Dynamic` and clear
   facts — the narrowing pass's existing catch-all backstop, unchanged.
6. Snapshot recording: for every call whose receiver is a bare
   `LocalVariableRead` whose `tenv` binding is `Tuple` / `HashShape` /
   `Nominal[Array|Hash]`, record `call_id → "Array"/"Hash"` (Tuple ⇒ "Array",
   HashShape ⇒ "Hash", per `receiver_descriptor:209`). Ivar receivers: never
   (bucket B).
7. Consumption: when `check_call` saw a `Dynamic` receiver (it types under the
   empty def-body env — `ScopedEnv::at`, `crates/rigor-rules/src/lib.rs:2641`)
   and the snapshot has the call, run the SAME core-RBS method-presence lookup
   `check_narrowed_call` uses against the snapshot class; absent ⇒
   `call.undefined-method`. Present ⇒ silent (no other rule sees the type).

`Set` never occurs as a seed in the candidate rows (the one `Set[…]` row is
bucket B) — stage 1 handles `Array`/`Hash` only.

### Stage 2 — chain roots (independent, individually gated micro-slices)

- **2a `Dir.glob` / `Dir.[]`**: the singleton return declines today because
  the block overload (`-> nil`) breaks all-overloads-agree. Add a
  no-block-overload-only return slot to the index (`singleton_method_return`
  twin restricted to block-free overloads, used only from the block-FREE call
  path — the mirror of `method_return_with_block`'s split). Oracle: c01, c02.
- **2b `ENV`**: type the `ENV` object constant (RBS `ENV: ENVClass`) so
  `ENV.keys → Array[String]` resolves; the rest of the c03 chain
  (`select{}`, `-`) already works. Gate on the constant-read arm's existing
  shadow checks (a project `ENV` constant must decline).
- **2c `Base64.decode64`**: diagnose why the stdlib module-function singleton
  return declines (module `self?.` ingestion is the suspect — same family as
  the closed BigMath/nokogiri asymmetries) and fix in the index. Oracle: c08b.
- **2d literal/const-rooted `merge(Dynamic)` chains**: verify
  `HashShape.merge(non-literal)` types `Nominal[Hash]` (not Dynamic) in the
  block-free RBS path; c05's top-level fire suggests mostly-working — the two
  rows (workhorse, routable_token) then ride stage 1's local binding and the
  existing `transform_values{}` fold. extconf's conditional-splat array
  literal is attempted last and may stay open (prediction: uncertain).
- **2e `::`-qualified constant paths** (u1): resolve a fully-qualified
  `::A::B::C::CONST` read to the same constant the lexical read resolves to.
  Oracle: u1; closes codequality:50.

Each 2x lands only with its own oracle probe pair (fire + decline) as unit
tests; any 2x that turns out non-trivial is dropped from this slice without
blocking the others.

## 6. FP-safety argument

Every emission requires ALL of: (a) a receiver binding derived
straight-line from a literal seed or an already-FP-gated tier fold, (b)
surviving the write-invalidation machinery (`collect_flow_writes` +
`indexed_flow_writes` — a strict superset of the reference's invalidations,
since the reference ignores unknown-callee escapes (m07) and does not
alias-track (m06)), (c) a probed-fire shape, and (d) the method absent from
the core RBS class surface — the exact lookup the reference's
`receiver_descriptor` + RBS dispatch performs on its (kept) nominal. The
enumerated declines:

- Dynamic/param seeds (m08, m13): no binding is ever minted.
- Rebinds: straight-line (m04), branch (m05), block-body (m15) — all kill.
- Branch-contained mutation on an unwidened seed (m18, m20): Dynamic — we
  never model the union the reference declines on.
- `while`/`until` (m10 fires in the reference — deliberately given up:
  `break`/`next` join edges are unprobed; no candidate row needs it),
  method-level `rescue` (u2), op-writes (m16), safe-nav + block bodies under
  safe-nav (PR #63 finding), ivars (m09 fires in the reference — bucket B's
  substrate, not this slice), `Struct`/`Data`/`Difference` carriers.
- Consumption is single-rule (`call.undefined-method`) and only replaces a
  `Dynamic`-receiver silence — a site any OTHER rule already witnesses is
  reached first in the existing precedence chain and is unchanged.

Stage-2 roots each widen the set of TYPED expressions, never the witness rule
set; each mirrors a unanimous-overload RBS fact and declines on divergence,
exactly like the existing singleton/tier-3 paths.

Residual risks named: (1) the reference's `while` fire means our loop decline
is coverage loss, not risk; (2) `merge` on `HashShape` with a Dynamic arg must
be verified fold-vs-Dynamic before 2d emits — if it types Dynamic today, those
two rows just stay open; (3) the E-bucket shows the reference WILL fire on
carriers it mis-infers (`[]=`-minted Hash on Dynamic, c12) — we must NOT
mirror that: stage 1 mints bindings only from literal seeds, never from
mutation evidence on Dynamic, which keeps us out of all five of those rows by
construction.

## 7. Verification plan (binding)

- Unit tests in `crates/rigor-infer` reproducing the probe matrix: fires m01,
  m02, m03, m06 (both locals), m07, m14, m17, m19, A′/c10; silences m04, m05,
  m08, m13, m15, m16, m18, m20, u2, ivar, safe-nav. Stage-2: per-root fire +
  decline pairs (c01/c02/c03/c08b + project-`ENV`-shadow, divergent-overload
  decline).
- New fixture `harness/corpus/83_collection_shape.rb` (83 is next free),
  following `81_class_narrowing.rb`'s convention: positives (straight-line
  `<<`, each-block `<<`, straight-line `[]=` hash, pre-widened branch
  mutation, const-chain local) + explicit NEGATIVE CONTROLS (rebind, branch
  rebind, unwidened-seed `if`/`case` mutation, Dynamic seed, block rebind,
  ivar mutation). Oracle-verify every expected/absent line with the recipe
  above BEFORE registering; regenerate `harness/snapshots/` via
  `ruby harness/snapshot.rb`.
- Gates, all green, in order: `cargo build --release --offline && cargo test
  --offline`, `ruby harness/run.rb` (0 FP), `ruby harness/run_snapshot.rb`,
  `python3 harness/fp_audit.py --gaps --sweep` (**0 FP / 9204 files** — build
  RELEASE first or pass `RIGOR_RS_BIN`; the default measures
  `target/release/rigor`), `python3 harness/docs_check.py`, fresh-target
  clippy.
- **Gap-set diff, not grep**: re-run `python3 harness/gap_census.py --sweep
  --dump <new>` against the 2026-08-07 baseline. Prediction — stage 1 closes
  the 9 stage-1 rows of §3; stage 2 closes up to 9 more (extconf and the two
  2d rows are the uncertain tail); **zero new FP rows**; nothing outside the
  49-row set is predicted to close, and any bonus closures (other
  `Dynamic`-receiver locals now typed) must each be oracle-spot-checked.
  A shortfall is a finding, not a failure.

## 7b. Stage 1 — BUILT (2026-08-08)

Shipped as specced: `Typer::collection_shape_snapshots` +
`coll_flow_scope/stmt/expr/if/case` (a parallel walker in
`crates/rigor-infer/src/lib.rs`, deliberately NOT an extension of
`class_flow_*`), consumed by `check_collection_call` in
`crates/rigor-rules/src/lib.rs` for `call.undefined-method` only. Pure
insertions — `MUTATOR_METHODS` and the narrowing pass are untouched; the
reference's two tables were added alongside as `ARRAY_MUTATORS`/`HASH_MUTATORS`.

**One spec correction, oracle-verified.** §5.4's join rule is right for the
METHOD body but does NOT apply inside a block. `widen_after_block`
(`mutation_widening.rb:144`) is a SYNTACTIC walk (`walk_for_outer_mutations`)
of the block body against the OUTER scope — it never joins the block's
evaluated scope, its own doc names `arr.push(x) if cond` as a caught case, and
it recurses into nested blocks. Probed at the pin: a branch-contained `<<`
inside an `each` block FIRES (`m02b`), as does the nested-block form; a block
param SHADOWING the outer local also fires, `for []` (the reference declines to
widen there, but a `Tuple` dispatches as Array anyway — so the `depth` check we
cannot mirror is FP-neutral by construction). The first implementation joined
the block's env instead and left the four each-block survey rows open; mirroring
the syntactic walk closed them.

**Measured** (sweep binary, 8 corpora / 9204 files): `TOTAL FP candidates: 0`.
Gap-set diff against the post-PR-#68 baseline (1167 rows → 1145): **22 closed,
0 opened**. All 9 predicted stage-1 rows closed — 5 before the
`widen_after_block` correction; the remaining 4 (the three jira-tracker rows and
the ddl-lock row) only after it, which is how the correction was found.

The 13 bonus closures are the same mechanism reaching further than predicted.
Nine are literal-seeded locals consumed in a `def` body, which §3 never
enumerated because the census bucketed them by receiver rendering rather than by
seed: gitlab `api/namespaces.rb:61`, `gitlab/auth/identity.rb:67`,
`gitlab/ci/config/extendable/entry.rb:86,:88`, `gitlab/kubernetes.rb:149`,
`gitlab/legacy_github_import/importer.rb:350`, mastodon
`app/lib/activitypub/linked_data_signature.rb:43`, net-ssh
`lib/net/ssh/test/packet.rb:88`. Four more were assigned elsewhere in §3 and
turn out to need only stage 1's local binding: `gitlab/workhorse.rb:306` and
psych `ext/psych/extconf.rb:29` (both filed 2d), `prometheus/
cleanup_multiproc_dir_service.rb:13` (filed 2a), and mastodon
`app/lib/translation_service/deepl.rb:27` ×2 (filed bucket D). Stage 2's
remaining scope shrinks accordingly. Nothing outside the census set opened.

Verification: 23 unit tests in `crates/rigor-infer` covering every probed row
(fires m01, m02, m02b, m03, m06 ×2, m07, m14, m17, m19; silences m04, m05, m08,
m13, m15, m18, m20 plus op-write/ivar/safe-nav/`while`/def-scope), and fixture
`harness/corpus/84_collection_shape.rb` (83 left free for the in-flight
narrowing branch) — 9 oracle diagnostics, rigor-rs matches 7 on
(rule, line, column) with the op-write and ivar rows as documented gaps, 0
extras.

## 7c. Stage 2 — BUILT (2026-08-08)

Three of the five micro-slices shipped, one shipped with a zero-row yield, one
dropped. Verdicts, then the evidence.

| slice | verdict | census rows closed |
|---|---|---|
| 2a `Dir.glob`/`Dir.[]` | **built** | 1 (`authz/permission_groups/resource.rb:56`) |
| 2b `ENV` | **built** | 2 (`backup/targets/database.rb:235,:246`) |
| 2c `Base64.decode64` chain | **built** (the root was NOT Base64 — see below) | 1 (`api/helpers/packages_manager_clients_helpers.rb:33`) |
| 2d const-hash `merge` chain | **dropped**, evidence below | 0 |
| 2e `::`-qualified constant paths | **built**, oracle-verified, **0 rows** | 0 |

Oracle re-verification at the pin (fresh temp cwd, `--no-cache`, plugin path
pinned) reproduced §1 exactly: c01 `blank?` `for Array[String]` @3:9, c02
`to_sentence` `for Array[Dynamic[top]]` @2:70, c03 `present?` `for Array[String]`
@3:60, c08b `second` `for Array[String]` @4:42, u1 firing on all THREE spellings
(lexical, `::A::B::C::PR`, `A::B::C::PR`). rigor-rs now matches c01/c02/c03/c08b
and u1's first two spellings on `(rule, line, column)`; the receiver RENDERING
stays the bare `Array` (the pre-existing element-erasure gap, not this slice).

### 2a + 2c are ONE mechanism, and 2c's root was mis-attributed

§5 filed `Dir.glob` (singleton) and `Base64.decode64` (module function) as
separate diagnoses. Measured, they are neither. `Base64.decode64` **already
typed `String`** in rigor-rs (`annotate` confirms) — the `self?.` ingestion
family closed by the BigMath / nokogiri notes was never the blocker. c08b's
actual root is `String#split`:

```
def split: (?Regexp | string | nil pattern, ?int limit) -> Array[String]
         | (?Regexp | string | nil pattern, ?int limit) { (String substring) -> void } -> self
```

— the same shape as `Dir.glob`'s `… -> Array[String] | … { … } -> nil`. In both,
`method_signature`'s all-overloads-agree collapse drops the return because a
BLOCK overload disagrees, even though a block-free call site can never select
it. Shipped as one slot pair, `ClassEntry::block_free_returns` /
`singleton_block_free_returns`, populated by `block_free_overload_return` ONLY
when the method declares BOTH overload kinds and EVERY block-free overload
agrees on one bare concrete `ClassInstanceType`; read by
`CoreData::method_return_block_free` / `singleton_method_return_block_free`
(strict first-definer-wins) from the two BLOCK-FREE arms of `Typer::type_call`
as an `.or_else` fallback. A block call still reads `block_returns`
(`type_block_call` is a different path entirely), and `Dir.[]` — single
overload — keeps an EMPTY block-free slot, so the arm provably cannot change any
result the flat slot already carried.

### 2b: the nil bit is load-bearing

`ENV` is an RBS **object constant** (`ENV: RBS::Unnamed::ENVClass`), a
declaration kind the index did not ingest at all. Top-level constant
declarations whose type is a bare `ClassInstanceType` are now recorded
(`CoreData::object_constants`, 18 declarations in the vendored set, of which
`CROSS_COMPILING: true?` is the one skipped). The typer types only the CALL's
return, never `ENV` itself — so no new witnessing surface appears for
`ENV.<anything>`; a strict under-emit vs the reference.

Two gates were added AFTER measurement, both from real sweep FPs on the first cut:

1. **NILABLE returns decline.** `ENVClass#[] : (String) -> String?`. The flat
   `method_return` slot drops the nil bit, so `ENV['X'].present?` typed `String`
   and fired where the reference (carrying `String | nil`, on which dispatch
   declines) is silent — **13 of 15 FPs**. The arm now reads
   `method_return_nilable` and refuses a nilable return.
2. **A project `ENV` write declines.** The C1 shadow tables are built from
   class/module DEFINITIONS only and never saw a plain `ENV = …` assignment.
   Probed: with `ENV = Object.new` the reference reports `undefined method
   'keys' for Object`. `SourceIndex::project_writes_constant` (bare name of
   every project constant WRITE, including the ones C5 declines to harvest) now
   gates the arm, scope-independently.

### 2d — DROPPED, with the evidence

The specced question ("does `HashShape.merge(non-literal)` type `Nominal[Hash]`
in the block-free path?") answers **yes** — probed:
`H.merge(other).transform_values { … }.compact_blank` already FIRES in rigor-rs
today, for both a literal and a C5-harvested constant receiver. 2d as written is
a no-op.

The remaining row's blocker is elsewhere:
`lib/authn/token_field/generator/routable_token.rb:15` is
`DEFAULT_ROUTING_PAYLOAD_HASH = { c: ->(_) { Gitlab.config.cell.id } }.freeze` —
a hash literal with a **LAMBDA value**, so C5's fully-literal `const_lit_of`
declines the whole constant and the read is `Dynamic`. Closing it needs
partially-dynamic constant-value harvesting (a `Hash`/`Array` nominal minted
from a literal whose ELEMENTS are not literal), which is a new mechanism with a
project-wide FP surface, not the merge fold. Out of scope; not forced.

### 2e — built, oracle-correct, and it closes nothing

`::A::B::C::CONST` lowers to ONE `ConstantRead` whose `name` is the whole path
(the leading `::` is not preserved), so the bare-name C5 map missed it while the
lexical spelling of the same constant folded. Fixed with a qualified twin map
plus `SourceIndex::qualified_literal_constant`: candidate keys are the path AS
WRITTEN against every initial segment run of the use site's lexical prefix
(ADR-0042 / PR #64 precedent), **ambiguity declines**, and a resolved key must
then pass the SAME lexical-visibility filter the bare path applies.

That last filter was also measured, not theorised: without it, gitlab's
`Gitlab::GitalyClient::DiffBlob::ATTRS` read from a SIBLING class
`…::DiffBlobsStitcher` folded here while the reference stayed silent — 2 of the
15 FPs. The reference's own behaviour is finer than either gate: it resolves an
in-FILE sibling path (fixture 88 case 10 fires there) but not the cross-FILE
one. rigor-rs's constant harvest is project-wide and cannot tell them apart, so
it declines both; losing the in-file case is the price of not shipping the
cross-file FP.

**And the target row does not close.** §3 filed
`lib/gitlab/ci/reports/codequality_reports.rb:50` as a 2e row, but its constant
is `SEVERITY_PRIORITIES = %w[…].map.with_index.to_h.freeze` — a CHAIN, not a
literal, so C5 never harvests it under EITHER spelling. The row is blocked by
exactly the mechanism 2d was dropped for. 2e ships anyway (0 FP over 9204 files,
oracle-verified fire + four declines, and a pure spelling extension of an
already-shipped gate). It is self-contained if a reviewer prefers zero-yield
code not to land: `SourceIndex::{qualified_literal_constants,
qualified_literal_constant}`, its one population line, the second arm of the
typer's `ConstantRead` case, the two `s2e_*` unit tests, and fixture 88 cases 5
(qualified half) / 10 / 11.

### Verification

- **Unit tests**: 4 in `crates/rigor-index` (block-free slot answers where the
  flat slot collapses; absent without a block overload; a synthetic
  divergent-vs-agreeing block-free overload pair on both instance and singleton
  sides declines/answers; RBS object constants ingested) + 10 in
  `crates/rigor-infer` (fire + decline per micro-slice, including the
  project-`ENV`-shadow, the nilable-return, the divergent-overload, the block-form,
  the ambiguous-path and the cross-namespace-path declines).
- **Fixture** `harness/corpus/88_collection_shape_stage2.rb` (87 reserved for
  the in-flight narrowing branch): 9 oracle diagnostics, every line
  oracle-verified before registering; rigor-rs matches 7 on (rule, line, column)
  with 2 documented coverage gaps (the `Dir.glob { }` block form, which the
  reference fires `for nil`; and case 10's in-file sibling path). 0 extras.
- **Gates** (all green): `cargo build --offline && cargo test --offline` (0
  failures); `ruby harness/run.rb` 87 fixtures, 299/319 matched, **0
  unregistered extras**; `ruby harness/run_snapshot.rb` identical;
  `python3 harness/fp_audit.py --gaps --sweep` **TOTAL FP candidates: 0** over 8
  corpora / 9204 files; `python3 harness/docs_check.py` PASS; clippy
  `-D warnings --all-targets` clean in a fresh `CARGO_TARGET_DIR`.
- **Gap-set diff** (`gap_census.py --sweep --dump`, pre- and post-change release
  binaries, keyed on `(path, line, rule, method)`): 1096 → 1092, **4 closed, 0
  opened**. The four are exactly the 2a/2b/2c rows named above. No bonus
  closures; the shortfall vs the "up to 6" prediction is the two rows 2d and 2e
  were filed for, both blocked on non-literal constant-value harvesting.

## 8. Upstream-feedback candidates (bucket E, all runtime-wrong at the pin)

1. **Bare AR-DSL `select('…')` dispatches to `Kernel#select`** — RBS 4.1
   `core/kernel.rbs:1745` declares `self?.select: (::Array[IO], …) ->
   ::Array[String]` (itself a dubious signature) and the reference tolerates
   the argument mismatch, typing every Rails scope's `select(str)` as
   `Array[String]` (probe c07; 4 rows).
2. **`.lazy.map { }` collapses to `Array`** — `(1..).each.lazy.map { }.next`
   fires undefined `next` `for Array[String]`; the runtime receiver is an
   `Enumerator::Lazy` and `next` is valid (probe c09; 2 rows).
3. **`[]=`-driven Hash minting on Dynamic carriers** — `buf = x.ljust(…)`;
   `buf[i] = …` in a block; `buf + y` fires `+` undefined `for Hash[Integer,
   Dynamic[top]]`; `buf` is a String at runtime (probe c12; 5 rows across
   diff-lcs/net-smtp/rufo: index-assignment evidence on an untyped carrier
   mints a Hash).

These 11 rows stay open by design; mirroring any of them would make rigor-rs
emit runtime-wrong diagnostics ("false positives outrank worst-case static
reading" — the reference's own top-tier value).
