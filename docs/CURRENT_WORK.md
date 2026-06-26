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
the reference's own source; matched scales with the sweep — 558 at this size, 100%
precision). 352 tests. The design (ADR 0001–0031) is audited and stable. The
2026-06-26 session (a) aligned the undefined-method rule with the reference's leniency,
(b) closed lowering-traversal + interpolated-string gaps, (c) landed **class-method
(singleton) witnessing** with a cross-file project index, (d) fixed a pre-existing
block-call FP class, then in a follow-on pass: (e) **recovered block-call return
typing** (RBS block-overload derived), (f) added **gitlab/checkstyle/junit/teamcity
formats + CI auto-detection**, and (g) landed **cross-file in-source method RETURN-TYPE
inference** (ADR-0023 tier-4 minimal slice). See the note below.

**Build / test / run (from the repo root):**
```sh
cargo build --offline && cargo test --offline       # 352 tests; ruby-prism + ruby-rbs are cached
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

- **Design:** ADRs 0001–0031 (`docs/adr/`) + glossary (`CONTEXT.md`), audited
  (`…/ruby/rigor/docs/notes/20260626-rigor-rs-design-audit.md`; verdict positive, R1–R5 done).
- **Build:** Cargo workspace, edition 2024, MSRV 1.85, `Cargo.lock` committed.
  External deps: `ruby-prism` (parser), `ruby-rbs` (RBS parser) — both cached.
- **Crates:** `rigor-types` (lattice) · `rigor-parse` (Prism + owned AST) ·
  `rigor-index` (real RBS index) · `rigor-infer` (typer + folding + source index) ·
  `rigor-rules` · `rigor-cli` (`rigor check`).
- **Tests:** 352 (verified `cargo test --offline`; this distribution slice added no new Rust
  tests — version command is exercised via the CLI binary). **Parity:** `run.rb` PASS (28 fixtures incl. the plugin-enabled +
  gate-guard pair, the tier-4b param-binding witness/decline pair, the four
  `def.override-visibility-reduced` fixtures — superclass + module-include positives, the
  reopened-class split, and the adversarial negatives bundle — and the two
  `call.possible-nil-receiver` fixtures: a byte-exact true positive + a guarded-negatives
  bundle), 0 FP; `run_corpus.rb`
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
- ⬜ `call.self-undefined-method` (ships `:off`; needs subclass-aware gate) · `call.unresolved-toplevel`
  (ref ADR-34) · `call.argument-type-mismatch` (ref ADR-64).
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
  (the catch-all now lowers descendant reads/calls instead of dropping the subtree).
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
- ⬜ `flow.unreachable-clause` (ref ADR-47) · `flow.always-truthy-condition` (deferred — needs the
  ADR-0022 flow-scope substrate, i.e. flow-sensitive scopes + narrowing in §4, which
  `dead-assignment` and `unreachable-branch` deliberately do NOT use).
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
- ⬜ rayon file-level parallelism; pre-pass tables frozen before workers; per-worker merge;
  severity re-stamp post-pool; workers precedence. (Salsa deferred — empirical trigger only.)

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
- ⬜ `annotate` · `diff` · `triage` ·
  `coverage` (incl. `--protection`, ref ADR-63/70) · `plugin` ·
  `sig-gen` (ref ADR-14) · `skill`/`describe` · `lsp` · `mcp` ·
  `trace` · `type-scan`.

### 12. Editor / agent servers (ADR-0029)
- ⬜ LSP (`rigor lsp --transport=stdio`, two-tier ProjectContext, BufferBinding, hover/completion);
  MCP server (read-only tools over stdio).

### 13. Distribution (ADR-0010)
- ✅ **Release-pipeline foundation landed (purely additive — no dev-loop/analysis change).**
  - Version bumped to **0.1.0** (single source: `[workspace.package] version`, inherited by all
    crates). `repository`/`license` (**AGPL-3.0** — note this DIFFERS from the reference gemspec's MPL-2.0; LICENSE is the verbatim GNU AGPL v3) added to
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
  - **Name `rigortype-rs`** (NOT `rigortype` — a 0.1.0 over the reference's 0.2.5 would be a
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
    multi-platform CI build/push end-to-end; Homebrew formula; musl + Windows targets; sidecar
    Ruby auto-detection. (The `rigortype` name takeover is NOT deferred but NOT planned — rigor-rs
    coexists with the Ruby mainstream per ADR-0001.)

### 14. Parity harness & QA (ADR-0002/0011)
- ✅ `harness/run.rb` (fixture gate, 28 fixtures incl. alias regression, the
  `call.possible-nil-receiver` TP + guarded-negatives pair, the ADR-25
  plugin-enabled / gate-guard pair via sibling-`.rigor.yml` sidecars, and the tier-4b
  param-binding witness/decline pair) + divergence-registry.
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
