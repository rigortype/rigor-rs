# rigor-rs — Current Work

A living map of **what is done** and **what remains to port** from the Ruby
reference (`/Users/megurine/repo/ruby/rigor`) into rigor-rs. Organized as a
port list keyed to the reference's subsystems. **Order is not binding** — pull
whatever is highest-leverage next; this file exists so nothing is lost, not to
fix a sequence.

Last updated: 2026-06-26.

## Legend

- ✅ done (working + tested/parity-checked)
- 🟡 partial / stub (works for a subset; a `// TODO(spec):` marks the gap)
- ⬜ not started
- 🔒 network-gated (needs crates.io / a crate we can't fetch offline yet)

The hard rule for every increment: **zero false positives** — the differential
harness (`harness/run.rb`) fails the build if rigor-rs emits a diagnostic the
reference does not. Coverage grows; it never regresses into guessing.

---

## Status snapshot

- **Design:** ADRs 0001–0031 (`docs/adr/`) + glossary (`CONTEXT.md`). The
  normative type-system + contracts are captured; the rule is "faithfully port
  the reference spec, the spec file is the authority."
- **Build:** Cargo workspace, edition 2024, MSRV 1.85, `Cargo.lock` committed.
  Builds and tests fully offline (only cached `ruby-prism` is external).
- **Crates:** `rigor-types` (lattice) · `rigor-parse` (Prism + owned AST) ·
  `rigor-index` (stub) · `rigor-infer` (typer + folding) · `rigor-rules` ·
  `rigor-cli` (`rigor check`).
- **Tests:** 74 unit/integration tests passing.
- **Parity:** `harness/run.rb` runs the live reference vs rigor-rs over a small
  corpus — **100% (7/7) matched, 0 false positives**. Coverage grows as rules land.
- **Works today:** `rigor check [--format text|json] <file…>` detecting
  `call.undefined-method` (literals, chained calls, post-fold), `call.wrong-arity`;
  Rust-native constant folding (`1 + 2` → `3`, ASCII String/Integer/etc.); JSON
  output field-identical to the reference; never-crash per-file isolation.

---

## Port backlog by subsystem

Reference paths are under `/Users/megurine/repo/ruby/rigor/`.

### 1. Parsing & AST — `lib/rigor/source/` → `rigor-parse` (ADR-0003/0012)
- ✅ `ruby-prism` binding wired; `parse()`; offline build of libprism.
- 🟡 Owned `NodeId`-indexed AST lowering — minimal node subset (program,
  statements, local read/write, str/int/float/sym/nil/true/false lits, call +
  positional args).
- ⬜ Lower the rest: `def`/`class`/`module`, `if`/`unless`/`elsif`/`case`/`when`/`in`,
  blocks, `begin`/`rescue`/`ensure`, ivars/cvars/gvars/constants, array/hash
  literals (→ Tuple/HashShape), keyword/splat/block args, string interpolation,
  ranges, `&.`, operators-as-calls.
- ⬜ Synthetic-node variants for plugin/macro-generated definitions (ADR-0012/0013).
- ⬜ ERB template detection (`analysis/erb_template_detector.rb`).

### 2. Type lattice — `lib/rigor/type/` → `rigor-types` (ADR-0005/0018/0019/0020)
- ✅ Carrier set, `Interner`/`TypeId`, `Scalar`, `describe()`.
- ✅ Dynamic[T] algebra in join/meet/difference; `untyped == Dynamic[top]`.
- ✅ `Certainty` (trinary) + `Evidence`; `subtype` vs `consistent` skeleton.
- 🟡 `normalize` — flatten/dedup/order, no `1|Integer` collapse, `true|false`
  display-only. `// TODO(spec):` finite-domain difference/complement, `T?`→`T|nil`,
  literal-precision widening cap, `void|bot`→`void`.
- 🟡 `subtype`/`consistent` — many cases return `Maybe`; needs the nominal
  hierarchy, `Constant <: base`, IntegerRange/Tuple/HashShape/refinement reasoning.
- ⬜ Dynamic **provenance** side-channel (5-cause, scope-side map; ADR-0019/ADR-75).
- ⬜ `DataClass`/`DataInstance` folding (ADR-0019 / ref ADR-48); `StructClass`/`StructInstance` (ref ADR-68).
- ⬜ Lightweight HKT `App[uri,args]` + URI registry (ref ADR-20).
- ⬜ Refinement carriers catalogue (60 built-ins, kebab-case; ref imported-built-in-types).

### 3. Index layer — `lib/rigor/environment*.rb`, `scope_indexer.rb` → `rigor-index` (ADR-0004/0007)
- ✅ **Real RBS-backed `CoreIndex`** (`rbs.rs`): parses 15 core `.rbs` via the
  `ruby-rbs` crate, flattens ancestor chains (class → includes → super), and
  resolves method existence / return / arity (min..max across overloads) over
  the full chain. **Conservative gate:** absence is only witnessed when the whole
  chain is loaded — incomplete chain ⇒ assume present (zero FP). Verified:
  inherited methods (`frozen?`/`tap`/`class`) silent; real-RBS-only methods
  (`bytes`/`scan`) silent; typos flagged; harness still 100%, 0 FP.
- 🟡 RBS source is a **runtime path** (`RIGOR_RBS_CORE_DIR` or the local rbs gem),
  with a hardcoded-stub fallback when absent.
- ⬜ Vendor + embed RBS at build time → remove the runtime path / Ruby dependency (ADR-0007).
- ⬜ Project `sig/` + gem RBS (bundler / rbs_collection) + `target_ruby` overlays (ADR-0007).
- ⬜ Method visibility, `prepend` order, generics/refinement resolution, in-source class defs.
- ⬜ Constant resolution; `pre_eval` monkey-patch pass (ref ADR-17); Gemfile.lock overlays (ref ADR-72).
- ⬜ RBS stdlib shipping: vendor + build-time pre-parse + embed; merge project
  `sig/` ⊕ gem RBS (bundler / rbs_collection auto-detect) ⊕ inline; `target_ruby`
  version overlays (ADR-0007).
- ⬜ Ancestor linearization with visibility; method resolution (prepend>class>include>super).
- ⬜ Constant resolution (in-source > RBS precedence, `# TYPE:` override).
- ⬜ `pre_eval` monkey-patch pre-evaluation pass (ref ADR-17).
- ⬜ Gemfile.lock-gated RBS overlays (ref ADR-72).
- ⬜ Rubydex as optional accelerator — re-evaluate per ADR-0004 spike.

### 4. Inference engine — `lib/rigor/inference/` → `rigor-infer` (ADR-0022/0023/0024)
- 🟡 `Typer`: pure `type_of` by node variant; literals; local env; Call dispatch
  (fold → nominal return → Dynamic). Flat top-level env only.
- ✅ Rust-native constant folding core (`folding.rs`) — deterministic Integer/
  Float/Bool/Nil/Symbol/ASCII-String; declines (→ None) on any doubt.
- ⬜ Flow-sensitive scopes + the 5 edges + fact buckets + per-bucket invalidation
  (ADR-0022); narrowing (guards, `is_a?`, truthy/falsey, equality trust levels;
  Float literal narrowing refused; negative facts domain-relative).
- ⬜ Full dispatch tier cascade: tier-2 shape dispatch, tier-4 in-source bodies;
  cross-file implicit-self resolution (ref ADR-24/57).
- ⬜ Inference budgets — wire the hard guards (recursion re-entry, ancestor 100,
  HKT fuel 64, dep budget 5000) + the configurable precision table (ADR-0024).
- ⬜ Block-captured local mutation / loop fixpoint, break-sink propagation (ref ADR-56).
- ⬜ Recursive-return precision (ref ADR-55); reflexive-send fold guard (ref ADR-78).
- ⬜ Parameter type inference (precision-additive only; ref ADR-67).
- ⬜ Purity/mutation summaries for the fixed v1 class set (ADR-0022).
- ⬜ Ruby **sidecar** for the folding long tail + plugin calls (ADR-0008): worker,
  MessagePack IPC, two-level persistent cache, graceful degradation.

### 5. Diagnostic rules — `lib/rigor/analysis/check_rules.rb` → `rigor-rules` (ADR-0030)
Converged single walk (ADR-0005). Reference has ~19 built-ins:
- ✅ `call.undefined-method` · ✅ `call.wrong-arity`
- 🟡 `call.possible-nil-receiver` (registered, inert until union/flow types exist)
- ⬜ `call.self-undefined-method` (ships `:off`; needs subclass-aware gate — ref ADR-24/notes)
- ⬜ `call.unresolved-toplevel` (ref ADR-34) · ⬜ `call.argument-type-mismatch` (ref ADR-64, COERCE_DISPATCH exclusion)
- ⬜ `flow.always-raises` · `flow.unreachable-branch` · `flow.unreachable-clause` (ref ADR-47) · `flow.dead-assignment` · `flow.always-truthy-condition`
- ⬜ `def.return-type-mismatch` · `def.method-visibility-mismatch` · `def.override-*` (ref ADR-35) · `def.ivar-write-mismatch` (ref ADR-58)
- ⬜ `dump.type` · `assert.type-mismatch` (annotation-driven)
- ⬜ Discriminated-union member narrowing (ref ADR-66)
- ⬜ `rbs.coverage.missing-gem` and other config/coverage diagnostics
- ⬜ Severity resolution precedence + suppression order (baseline last) + per-rule
  canonical severities + token expansion (ADR-0030)
- ⬜ Diagnostic enrichment remainder: `project_definition_site`, full `source_family` set

### 6. Output & reporters — `lib/rigor/cli/diagnostic_formats.rb` → `rigor-cli` (ADR-0014/0030)
- ✅ text + JSON (field-identical to the reference for the call rules).
- ⬜ SARIF · GitHub annotations · GitLab Code Quality · Checkstyle · JUnit · TeamCity (ref ADR-51).
- ⬜ CI auto-detection (GitHub/TeamCity native annotations on top of text; ref ADR-51).
- ⬜ Replace hand-rolled JSON with serde once crates are fetchable (🔒).

### 7. Config & baseline — `configuration.rb`, `analysis/baseline.rb` → (ADR-0009/0031)
- ⬜ `.rigor.yml` / `.rigor.dist.yml` loader: winner-takes-all (no merge),
  `includes:` stack, relative-to-config-file paths, hard-coded exclusions,
  config-validation warnings (plugin-family exempt). Needs a YAML reader (🔒 serde_yaml/yaml-rust).
- ⬜ Full key schema (target_ruby, paths, exclude, plugins, disable, libraries,
  signature_paths, severity_profile, bundler/rbs_collection auto_detect,
  budget_overrun_strategy, bleeding_edge grammar, plugins_isolation).
- ⬜ Baseline read/write (same format; `message:` field; `--match-mode`; drift) — ref ADR-22.

### 8. Caching & incremental — `lib/rigor/cache/` → (ADR-0017/0028)
- ⬜ Content-addressed persistent analysis cache (`.rigor/cache`, `cache.path`), LRU.
- ⬜ Six-slot descriptor + two store paths (`fetch_or_compute` / `fetch_or_validate`).
- ⬜ Incremental cross-file dependency graph + `--verify-incremental` (ref ADR-46).

### 9. Concurrency — `worker-session`, ractor → (ADR-0006/0028)
- ⬜ rayon file-level parallelism; pre-pass discovery tables frozen before workers;
  per-worker state merged post-pool; severity re-stamp post-pool; workers precedence.
- ⬜ (Salsa stays deferred; adopt only on an empirical profiling trigger — ADR-0006.)

### 10. Plugins — `lib/rigor/plugin/` + `plugins/` (31) → (ADR-0013/0027)
- ⬜ Plugin trait (`node_rule`/`dynamic_return`/`type_specifier` + NodeContext + FactStore topo-sort + manifest fields).
- ⬜ Sidecar-hosted Ruby plugin runner (strangler default) + IoBoundary/TrustPolicy.
- ⬜ Native-Rust ports, hottest-first (Rails family: rails-routes, activerecord, actionpack…).

### 11. CLI commands — `lib/rigor/cli.rb` → `rigor-cli` (ADR-0015)
- ✅ Full surface presented; unimplemented commands report clearly.
- ✅ `check`. ⬜ `annotate` · `type-of` · `explain` · `init` · `diff` · `baseline`
  · `triage` · `coverage` (incl. `--protection`, ref ADR-63/70) · `plugins`/`plugin`
  · `docs` · `sig-gen` (ref ADR-14) · `skill`/`describe` · `doctor` (ref ADR-77)
  · `lsp` · `mcp` · `trace` · `type-scan`.

### 12. Editor / agent servers (ADR-0029)
- ⬜ LSP: `rigor lsp --transport=stdio`, in-process, two-tier ProjectContext
  invalidation, BufferBinding temp-file path, hover/completion.
- ⬜ MCP server (read-only tools over stdio).

### 13. Distribution (ADR-0010)
- ⬜ Static libprism link; cross-compile matrix (linux gnu+musl, macOS, Windows).
- ⬜ Channels: precompiled-binary gem (primary) + GitHub Releases + cargo-binstall + Homebrew.
- ⬜ Sidecar Ruby auto-detection (`.ruby-version`/`.tool-versions`/bundle/PATH).

### 14. Parity harness & QA (ADR-0002/0011)
- ✅ `harness/run.rb` (live reference vs rigor-rs, one-sided gate), corpus, divergence-registry.
- ⬜ Grow the corpus continuously (each new rule/feature gets fixtures; OSS corpora
  like Redmine/Mastodon as the reference uses).
- ⬜ Snapshot mode (pin reference, commit expected JSON) for CI without a Ruby runtime (ADR-0002).

---

## Cross-cutting known issues / decisions to revisit

- ✅ `internal-error` rule id (audit R5) — resolved: emitted at `:info` severity so
  the harness's error/warning gate never treats it as a parity FP; recorded in ADR-0016.
- 🟡 Hand-rolled JSON (no serde) — network is back, so swap to serde + add SARIF/CI
  formats (§6).
- ✅ `CoreIndex` stub → **real RBS index landed** (§3); RBS `alias` resolution fixed
  (was a latent `s.size` false positive surfaced by the corpus, now harness-guarded).

## External audit (2026-06-26) — action items

From `…/ruby/rigor/docs/notes/20260626-rigor-rs-design-audit.md` (verdict: design
structurally avoids the Pzoom/artichoke/pylyzer traps). Tracked actions:

- ✅ **R5** internal-error → `:info`, documented (above).
- ✅ **R1** positioning recorded in ADR-0008 (standalone = sound subset; full parity
  needs the sidecar). ⬜ remaining: surface "sidecar absent ⇒ reduced coverage" in
  `rigor doctor` (ADR-0031, when doctor lands).
- ✅ **R2** ADR-0007: `RIGOR_RBS_CORE_DIR` formalized as the out-of-band stdlib-RBS
  refresh seam.
- ✅ **R3** ADR-0001: positioning stated — rigor-rs is a performance prototype that COEXISTS
  with the Ruby mainstream (Ruby leads; no planned retirement / single-implementation; full
  parity + eventual sync are possibilities, not commitments).
- 🟡 **R4 (urgent, in progress)** scaled corpus harness landed (`harness/run_corpus.rb`):
  ran 115 real files (rigor examples + lib/rigor/type + mastodon/app/models), coverage
  67%, and surfaced exactly **1 false positive** — `Hash#to_json` (index loaded core
  RBS only, not the `json` stdlib that reopens Hash). Fix in flight: load core +
  the reference's `DEFAULT_LIBRARIES` stdlib set. Next: rescan + bigger corpora,
  iterate scan→fix→rescan.

## Network — RESTORED (2026-06-26)

crates.io is reachable again: the sparse index, `.crate` downloads, and
`cargo build` of fresh external crates all work; GitHub is reachable (submodules
OK). Verified fetchable: `ruby-rbs` 0.3.0 (+ `ruby-rbs-sys`), `serde` 1.0.228,
`serde_json`, `rayon` 1.12, `rmp-serde` 1.3. The previously 🔒 items are now
actionable:

- `ruby-rbs` → real index layer (§3) — **the biggest single accuracy jump**;
  start by confirming its public API surfaces typed method definitions
  (ADR-0004 spike), then replace the `CoreIndex` stub.
- serde / serde_json (/ a YAML crate) → real JSON/SARIF (§6) + config loader (§7).
- rayon → parallelism (§9). rmp-serde → sidecar IPC (§4).
- Rubydex evaluation as optional accelerator (§3).

(`ruby-rbs-sys` ships a native component like `ruby-prism-sys`; clang is present,
so it should build — confirm on first integration.)

## Immediate next candidates (highest leverage; pick dynamically)

1. Broaden AST lowering (if/case/def/ivars/array+hash) → unblocks flow rules + shapes (§1).
2. Flow-sensitive scopes + narrowing (ADR-0022) → unblocks the `flow.*` rule family and `possible-nil-receiver` (§4, §5).
3. Grow the harness corpus to expose the next coverage gaps and drive 1–2 (§14).
4. When network returns: wire `ruby-rbs` real index (§3) — the biggest single accuracy jump.
