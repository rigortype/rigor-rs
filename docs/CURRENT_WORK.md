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
> *witnessing* surface. Result: 0 FP across 1444 real files, **matched coverage
> unchanged** (every real match was already a core/RBS receiver). Cross-file
> in-source witnessing is therefore **not** a coverage lever — see the reframed
> §3/§4 note below.

## Legend

- ✅ done (working + tested/parity-checked) · 🟡 partial / stub · ⬜ not started

The hard rule for every increment: **zero false positives**. The differential
harnesses (`harness/run.rb`, `harness/run_corpus.rb`) fail if rigor-rs emits a
diagnostic the reference does not. Coverage grows; it never regresses into guessing.

---

## ▶ Resume here (next session)

**State:** a working, parity-validated analyzer. `rigor check` runs end to end;
**0 false positives across 1444 real files** (mastodon, gitlab-foss, conference-app,
the reference's own source), **109/109 matched** (100% precision). 113 tests. The
design (ADR 0001–0031) is audited and stable. A 2026-06-26 correctness fix aligned
the undefined-method rule with the reference's leniency (see the note below).

**Build / test / run (from the repo root):**
```sh
cargo build --offline && cargo test --offline       # 113 tests; ruby-prism + ruby-rbs are cached
cargo run -p rigor-cli -- check <file.rb> --format json
ruby harness/run.rb                                  # fixture differential gate (must be 8/8, 0 FP)
ruby harness/run_corpus.rb <dir...>                  # scaled real-corpus gate (CORPUS_LIMIT env)
```

**Reference oracle (for the harness / manual checks):**
```sh
ruby -I/Users/megurine/repo/ruby/rigor/lib /Users/megurine/repo/ruby/rigor/exe/rigor check <path> --format json
# JSON on STDOUT; preamble + racc warning on STDERR. Run with cwd = a clean temp dir to
# avoid picking up a project .rigor.yml. It accepts a directory (analyzes all .rb, RBS loaded once).
```

**Key facts/paths:**
- RBS source: `RIGOR_RBS_CORE_DIR` env, else `…/mise/installs/ruby/4.0.5/…/gems/rbs-4.0.3/core`
  (62 core `.rbs`; stdlib at `…/rbs-4.0.3/stdlib/<lib>/0/*.rbs`). The index loads the whole
  `core/` + the reference's `DEFAULT_LIBRARIES` stdlib set. Stub fallback if the dir is absent.
- Real corpora under `/Users/megurine/repo/ruby/`: `mastodon/app/{models,services,controllers}`,
  `gitlab-foss/app/{models,services}`, `conference-app`, plus the reference's own `lib/` & `examples/`.
- Spikes (excluded from the workspace): `spike/prism_probe`, `spike/rbs_probe`.

**Highest-leverage next candidates** (data-driven: on real code `call.undefined-method`
is **96%** of error/warning diagnostics — so coverage comes from *typing more receivers*
precisely, not new rules. The remaining gap is mostly **Rails** receivers needing
project-RBS / plugins):
1. ~~**Cross-file project class index** to raise in-source witnessing coverage~~ —
   **retired by the 2026-06-26 finding.** The reference never witnesses on in-source
   instances, so completing cross-file chains adds FP risk, not matched coverage. The
   real coverage lever is **cross-file in-source RETURN-TYPE inference** (ADR-0023
   tier-4 body inference, currently ⬜): infer a project method's return type so a
   chained call lands on a *core/RBS* receiver that DOES witness (e.g.
   `user.full_name.lenght` where `full_name : String`). A project-wide `SourceIndex`
   pass is the right substrate for THAT — build it when body inference lands, not for
   witnessing.
2. **Config loader `.rigor.yml`** (§7, serde_yaml now available) + **serde output + SARIF /
   GitHub / CI formats** (§6) — drop-in readiness; both unblocked by network.
3. **Plugin phase** (§10, ADR-0013) — the real Rails-coverage unlock (sidecar-hosted Ruby
   plugins). Biggest phase; the bulk of remaining undefined-method coverage lives here.
4. **Flow-sensitive scopes + narrowing** (§4, ADR-0022) → the `flow.*` rule family and a
   live `possible-nil-receiver`.
5. **Vendor + embed RBS at build time** (§3, ADR-0007) → remove the runtime RBS path /
   Ruby dependency so the core binary is truly standalone + instant startup.

---

## Status snapshot

- **Design:** ADRs 0001–0031 (`docs/adr/`) + glossary (`CONTEXT.md`), audited
  (`…/ruby/rigor/docs/notes/20260626-rigor-rs-design-audit.md`; verdict positive, R1–R5 done).
- **Build:** Cargo workspace, edition 2024, MSRV 1.85, `Cargo.lock` committed.
  External deps: `ruby-prism` (parser), `ruby-rbs` (RBS parser) — both cached.
- **Crates:** `rigor-types` (lattice) · `rigor-parse` (Prism + owned AST) ·
  `rigor-index` (real RBS index) · `rigor-infer` (typer + folding + source index) ·
  `rigor-rules` · `rigor-cli` (`rigor check`).
- **Tests:** 113. **Parity:** `run.rb` 8/8, 0 FP; `run_corpus.rb` validated to **1444 real
  files, 0 FP, 109/109 matched** (100% precision) after the 2026-06-26 leniency fix.
- **Works today:** `rigor check [--format text|json] <file…>` →
  `call.undefined-method` (literals, chained calls, post-fold, **core `X.new`
  instances** like `Array.new`) and `call.wrong-arity`; Rust-native constant folding
  (`1 + 2` → `3`, ASCII String/Integer/etc.); JSON field-identical to the reference;
  never-crash per-file isolation. **In-source/project classes and non-core `.new`
  instances are typed (for chaining) but NOT witnessed** — matching the reference's
  leniency (see the 2026-06-26 finding above). Rails models (unknown super) stay silent.

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
  set, return type, arity (min..max over overloads), super + includes; flattens the ancestor
  chain. Handles RBS `alias` (resolved through the chain) and **nested class/module decls**
  (registered by simple name). **Conservative gate (zero-FP keystone):** absence is witnessed
  only when the entire chain is loaded; an incomplete/unknown chain ⇒ assume present ⇒ silent.
- ✅ **In-source class index** (`rigor-infer/source_index.rs`): a per-run map of a file's own
  `class name → {own methods, superclass}`, used to **type `X.new`** (instance identity for
  chained-call RETURN inference). **NOT a witnessing surface** (2026-06-26 finding): the
  undefined-method rule never fires on a source/registry instance — the reference is lenient
  there. `class_has_method` exists as a SourceIndex capability but is no longer consulted by
  the rule. Per-file only (a project pass was prototyped this session, then reverted as it
  added no matched coverage given the leniency).
- 🟡 RBS source is a **runtime path** (`RIGOR_RBS_CORE_DIR`/local rbs gem) + hardcoded-stub fallback.
- ⬜ **Vendor + embed RBS at build time** → remove runtime path / Ruby dep (ADR-0007).
- ⬜ **Cross-file** project class/constant index (current SourceIndex is per-file).
- ⬜ Project `sig/` + gem RBS (bundler / rbs_collection) + `target_ruby` overlays (ADR-0007).
- ⬜ Method visibility, `prepend` order, generics/refinement resolution.
- ⬜ Constant resolution (in-source > RBS precedence, `# TYPE:`); `pre_eval` monkey-patch pass
  (ref ADR-17); Gemfile.lock-gated RBS overlays (ref ADR-72); Rubydex accelerator (ADR-0004 spike).

### 4. Inference engine — `lib/rigor/inference/` → `rigor-infer` (ADR-0022/0023/0024)
- ✅ `Typer`: pure `type_of` by node variant; literals; local env; **chained-call typing**
  (resolve receiver class → method return → nominal); **`X.new` → instance typing**;
  array/hash literal → nominal Array/Hash.
- ✅ Rust-native constant folding (`folding.rs`) — deterministic Integer/Float/Bool/Nil/Symbol/
  ASCII-String; declines (→ None) on any doubt; arg-dependent folds (`1 + 2 → 3`).
- 🟡 Environment is flat / top-level (no flow sensitivity yet); params/ivars/constants → Dynamic.
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
- ⬜ Severity resolution precedence + suppression order (baseline last) + per-rule canonical
  severities + token expansion (ADR-0030); diagnostic enrichment remainder
  (`project_definition_site`, full `source_family`).

### 6. Output & reporters — `lib/rigor/cli/diagnostic_formats.rb` → `rigor-cli` (ADR-0014/0030)
- ✅ text + JSON (hand-rolled; field-identical to the reference for the call rules).
- 🟡 Swap hand-rolled JSON for serde (now available); ⬜ SARIF · GitHub annotations · GitLab ·
  Checkstyle · JUnit · TeamCity (ref ADR-51); CI auto-detection (ref ADR-51).

### 7. Config & baseline — `configuration.rb`, `analysis/baseline.rb` → (ADR-0009/0031)
- ⬜ `.rigor.yml` / `.rigor.dist.yml` loader: winner-takes-all (no merge), `includes:` stack,
  relative-to-config paths, hard-coded exclusions, config-validation warnings (plugin-family
  exempt). serde_yaml available now. Full key schema (target_ruby/paths/exclude/plugins/disable/
  libraries/signature_paths/severity_profile/auto_detect/budget_overrun_strategy/bleeding_edge/
  plugins_isolation).
- ⬜ Baseline read/write (same format; `message:` field; `--match-mode`; drift) — ref ADR-22.

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
- ✅ Full surface presented; unimplemented commands report clearly. ✅ `check`.
- ⬜ `annotate` · `type-of` · `explain` · `init` · `diff` · `baseline` · `triage` ·
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
- ✅ `harness/run_corpus.rb` (scaled, real-corpus gate; 1444 files validated 0 FP; `harness/CORPUS.md`).
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
- ✅ **R2** ADR-0007: `RIGOR_RBS_CORE_DIR` as the out-of-band stdlib-RBS refresh seam.
- ✅ **R3** ADR-0001: positioning stated — rigor-rs is a performance prototype that COEXISTS
  with the Ruby mainstream (Ruby leads; no planned retirement / single-implementation; full
  parity + eventual sync are possibilities, not commitments).
- ✅ **R4** graded at scale — 0 false positives across 1444 real files; the corpus harness stays
  for ongoing regression as rules/inference grow.
- ✅ **R5** internal-error → `:info`.
