# rigor-rs — Current Work

A living map of **what is done** and **what remains to port** from the Ruby
reference (`/Users/megurine/repo/ruby/rigor`) into rigor-rs. Organized as a
port list keyed to the reference's subsystems. **Order is not binding** — pull
whatever is highest-leverage next; this file exists so nothing is lost, not to
fix a sequence.

Last updated: 2026-06-26. HEAD at handoff: `82e9eb1`.

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

**State:** a working, parity-validated analyzer. `rigor check` runs end to end;
**0 false positives across 3829 real files** (mastodon, gitlab-foss, conference-app,
the reference's own source; matched scales with the sweep — 542 at this size, 100%
precision). 226 tests. The design (ADR 0001–0031) is audited and stable. The
2026-06-26 session (a) aligned the undefined-method rule with the reference's leniency,
(b) closed lowering-traversal + interpolated-string gaps, (c) landed **class-method
(singleton) witnessing** with a cross-file project index, (d) fixed a pre-existing
block-call FP class, then in a follow-on pass: (e) **recovered block-call return
typing** (RBS block-overload derived), (f) added **gitlab/checkstyle/junit/teamcity
formats + CI auto-detection**, and (g) landed **cross-file in-source method RETURN-TYPE
inference** (ADR-0023 tier-4 minimal slice). See the note below.

**Build / test / run (from the repo root):**
```sh
cargo build --offline && cargo test --offline       # 226 tests; ruby-prism + ruby-rbs are cached
cargo run -p rigor-cli -- check <file.rb> --format json
ruby harness/run.rb                                  # fixture differential gate (must PASS, 0 FP)
ruby harness/run_corpus.rb <dir...>                  # scaled real-corpus gate (CORPUS_LIMIT env)
```

**Reference oracle (for the harness / manual checks):**
```sh
ruby -I/Users/megurine/repo/ruby/rigor/lib /Users/megurine/repo/ruby/rigor/exe/rigor check <path> --format json
# JSON on STDOUT; preamble + racc warning on STDERR. Run with cwd = a clean temp dir to
# avoid picking up a project .rigor.yml. It accepts a directory (analyzes all .rb, RBS loaded once).
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

**Highest-leverage next candidates** (data-driven: on real code `call.undefined-method`
is **96%** of error/warning diagnostics — so coverage comes from *typing more receivers*
precisely, not new rules. The remaining gap is mostly **Rails** receivers needing
project-RBS / plugins):
1. 🟡 **Cross-file in-source RETURN-TYPE inference** (ADR-0023 tier-4 body inference) —
   **minimal slice LANDED** (this session): `SourceIndex` Pass-3 `infer_method_returns`
   types a project method's TAIL expression under an EMPTY `TypeEnv` and, when it yields a
   concrete **core/RBS** class, interns that core nominal so a chained typo witnesses
   (`user.full_name.lenght` where `full_name : String`). Zero-FP by strict
   under-approximation (witness set ⊆ reference): declines on explicit `return`, branch/loop
   tail, param/ivar/self dependence (empty env ⇒ Dynamic), in-source method-call tail, and
   reopen disagreement. **Deferred (next increments):** call-site param binding (`def n(x); x; end`),
   cross-method-call return inference + fixpoint (ref ADR-55/56), branch/explicit-return
   UNION (needs a union-consuming witness site), ivar/self typing (ADR-0022 flow), singleton
   (`def self.x`) return inference. These are the remaining in-source coverage levers.
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

- **Design:** ADRs 0001–0031 (`docs/adr/`) + glossary (`CONTEXT.md`), audited
  (`…/ruby/rigor/docs/notes/20260626-rigor-rs-design-audit.md`; verdict positive, R1–R5 done).
- **Build:** Cargo workspace, edition 2024, MSRV 1.85, `Cargo.lock` committed.
  External deps: `ruby-prism` (parser), `ruby-rbs` (RBS parser) — both cached.
- **Crates:** `rigor-types` (lattice) · `rigor-parse` (Prism + owned AST) ·
  `rigor-index` (real RBS index) · `rigor-infer` (typer + folding + source index) ·
  `rigor-rules` · `rigor-cli` (`rigor check`).
- **Tests:** 211. **Parity:** `run.rb` PASS (16 fixtures), 0 FP; `run_corpus.rb` validated to **3829 real
  files, 0 FP, 542/542 matched** (100% precision; embedded RBS == runtime path, byte-identical).
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
- ✅ **Cross-file** project class index (`build_project`) for the singleton FP gate; ⬜ cross-file
  CONSTANT index + cross-file in-source method RETURN inference (the next real coverage lever).
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
- 🟡 Environment is flat / top-level (no flow sensitivity yet); params/ivars/non-class-constants → Dynamic.
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
- ⬜ **Flow-sensitive scopes** + 5 edges + fact buckets + invalidation (ADR-0022); narrowing
  (guards, `is_a?`, truthy/falsey, equality trust, negative facts domain-relative).
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
  (inert until union/flow types exist).
- ✅ **Metaclass-constructor guard** (`CLASS_RETURNING_NEW` in `rigor-infer`): `Struct.new(...)`,
  `Data.define(...)`, `Class.new` return a CLASS, not an instance — never typed as an instance
  of the receiver (was a chained-`.new` FP).
- ⬜ `call.self-undefined-method` (ships `:off`; needs subclass-aware gate) · `call.unresolved-toplevel`
  (ref ADR-34) · `call.argument-type-mismatch` (ref ADR-64).
- ⬜ `flow.always-raises` · `flow.unreachable-branch` · `flow.unreachable-clause` (ref ADR-47) ·
  `flow.dead-assignment` · `flow.always-truthy-condition`.
- ⬜ `def.return-type-mismatch` · `def.method-visibility-mismatch` · `def.override-*` (ref ADR-35) ·
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
- ⬜ rayon file-level parallelism; pre-pass tables frozen before workers; per-worker merge;
  severity re-stamp post-pool; workers precedence. (Salsa deferred — empirical trigger only.)

### 10. Plugins — `lib/rigor/plugin/` + `plugins/` (31) → (ADR-0013/0027)
- ⬜ Plugin trait (`node_rule`/`dynamic_return`/`type_specifier` + NodeContext + FactStore topo-sort
  + manifest fields); sidecar-hosted Ruby plugin runner (strangler default) + IoBoundary/TrustPolicy;
  native-Rust ports, hottest-first (Rails family). **This is where most real-code coverage lives.**

### 11. CLI commands — `lib/rigor/cli.rb` → `rigor-cli` (ADR-0015)
- ✅ Full surface presented; unimplemented commands report clearly. ✅ `check`
  (`--format text|json|github|sarif|gitlab|checkstyle|junit|teamcity`, `--config <path>`,
  project two-phase pass, inline + config suppression, CI auto-detection on `--format text`).
- ✅ `baseline` — `generate [--match-mode rule|message] [--output PATH] [--force] [--config PATH]
  <file...>` (byte-compatible `.rigor-baseline.yml`) · `dump [--baseline PATH]`. `regenerate`/
  `drift`/`prune` recognized but deferred (need `configuration.paths`).
- ⬜ `annotate` · `type-of` · `explain` · `init` · `diff` · `triage` ·
  `coverage` (incl. `--protection`, ref ADR-63/70) · `plugins`/`plugin` · `docs` ·
  `sig-gen` (ref ADR-14) · `skill`/`describe` · `doctor` (ref ADR-77) · `lsp` · `mcp` ·
  `trace` · `type-scan`.

### 12. Editor / agent servers (ADR-0029)
- ⬜ LSP (`rigor lsp --transport=stdio`, two-tier ProjectContext, BufferBinding, hover/completion);
  MCP server (read-only tools over stdio).

### 13. Distribution (ADR-0010)
- ⬜ Static libprism link; cross-compile matrix (linux gnu+musl, macOS, Windows); channels
  (precompiled-binary gem primary + GitHub Releases + cargo-binstall + Homebrew); sidecar Ruby
  auto-detection.

### 14. Parity harness & QA (ADR-0002/0011)
- ✅ `harness/run.rb` (fixture gate, 12 fixtures incl. alias regression) + divergence-registry.
- ✅ `harness/run_corpus.rb` (scaled, real-corpus gate; 2458 files validated 0 FP; `harness/CORPUS.md`).
- ⬜ Continuous corpus growth (new fixtures per rule/feature); snapshot mode (pin reference,
  commit expected JSON) for CI without a Ruby runtime (ADR-0002).

---

## Cross-cutting status

- ✅ `internal-error` rule id → `:info` (audit R5), excluded from the parity gate (ADR-0016).
- 🟡 Hand-rolled JSON (no serde) — swap to serde + add SARIF/CI formats (§6); serde is available.
- ✅ Real RBS index landed (§3); RBS `alias` + nested-decl + the `Hash#to_json` stdlib FP all fixed.

## External audit (2026-06-26) — all addressed

`…/ruby/rigor/docs/notes/20260626-rigor-rs-design-audit.md` (verdict: structurally avoids the
Pzoom/artichoke/pylyzer traps).
- ✅ **R1** ADR-0008: positioning (standalone = sound subset; full parity needs the sidecar);
  ⬜ remaining: surface "sidecar absent ⇒ reduced coverage" in `rigor doctor` when it lands.
- ✅ **R2** ADR-0007: RBS now **vendored + embedded at build time** (standalone binary, no runtime
  rbs gem); `RIGOR_RBS_CORE_DIR` retained as the out-of-band stdlib-RBS refresh/override seam.
- ✅ **R3** ADR-0001: positioning stated — rigor-rs is a performance prototype that COEXISTS
  with the Ruby mainstream (Ruby leads; no planned retirement / single-implementation; full
  parity + eventual sync are possibilities, not commitments).
- ✅ **R4** graded at scale — 0 false positives across 2458 real files; the corpus harness stays
  for ongoing regression as rules/inference grow.
- ✅ **R5** internal-error → `:info`.
