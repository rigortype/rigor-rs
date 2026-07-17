# rigor-rs — Port backlog by subsystem

The complete per-subsystem map of **what is done** and **what remains to port**
from the Ruby reference (`/Users/megurine/repo/ruby/rigor`) into rigor-rs.
Read on demand; the session baton is [CURRENT_WORK.md](CURRENT_WORK.md).
Update a checklist line when a slice lands — status marks and short deltas only;
measured outcomes and narratives live in `docs/notes/` + `docs/adr/`.

## Legend

- ✅ done (working + tested/parity-checked) · 🟡 partial / stub · ⬜ not started

The hard rule for every increment: **zero false positives**. The differential
harnesses (`harness/run.rb`, `harness/run_corpus.rb`) fail if rigor-rs emits a
diagnostic the reference does not. Coverage grows; it never regresses into guessing.

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
