# sig-gen generation-time env classification — design (2026-07-10)

Design for porting the reference generator's classification-against-existing-RBS
(`new_method` / `tighter_return` / equivalent-drop) into rigor-rs sig-gen.
Investigated via the AGENTS.md protocol: two independent Sonnet investigations
(full source trace of `generator.rb`'s classify chain; 13-scenario byte-exact
oracle probes) + three decisive main-session probes (N/O/P below) that settled a
disagreement BETWEEN the two investigations. **Implementation has NOT started —
this note is the binding spec for the next session's Opus delegation.**

## What this slice closes (all currently-documented deferrals)

- The `ObservedCall#hash` sound-superset excess from the Writer slice (an
  inherited method the reference drops as EQUIVALENT when the class is RBS-known).
- Correct `--print` on projects WITH a `sig/`: today rigor-rs tags an
  already-declared-tighter method `# [new]` where the reference prints
  `# [tighter, was: X]` — a shared-method BYTE MISMATCH (hard-guarantee break).
- The `--diff` `- def name: () -> X` declared line (today never printed).
- JSON `declared_return_rbs` + `classification: tighter_return` at GENERATION
  time (the Writer currently synthesizes them at write time only — once this
  lands, the writer's write-time extraction becomes the fallback for
  consolidated files whose class the generation env didn't see... verify overlap
  at implementation time; likely the write-time path can stay as-is since its
  probes pass).
- Unlocks `--overwrite` (tighter-return replacement needs the classification).

## The decisive rule (both investigations partially WRONG — probes N/O/P bind)

**Ancestor resolution IS performed, but gated on the RECEIVER CLASS ITSELF
being declared in the loaded RBS environment** (`env.class_decls.key?` in
`build_instance_definition`, rbs_loader.rb:1156-1167):

- Probe N: `sig/foo.rbs` = `class Foo\nend` (EMPTY decl) + `def hash; 1; end`
  → `# [tighter, was: Integer]` — Object#hash resolved through ancestors.
- Probe O: sig declares `class Base` (with `greeting: () -> String`) and
  `class Foo < Base` → Foo#greeting → `# [tighter, was: String]` — superclass
  project-sig declaration resolved.
- Probe P: same as N but `def hash; [1].size; end` (inferred `Integer`) →
  `No candidates` — EQUIVALENT drop against the inherited `Integer`.
- Probe A (agent): NO sig at all → `def hash; 1; end` → `# [new]` — the class
  is not in the env, ancestor lookup never runs, everything is `new_method`.

Methodology note (why the extra probes were needed): the source-reading agent
speculated "literal 1 erases to Integer → equivalent" (WRONG — N shows
`-> 1` emitted with `was: Integer`); the probing agent concluded "no ancestry
at all" (WRONG — its scenarios A/J never declared the class itself in sig, so
the gate short-circuited before ancestry). Never design from a single agent's
interpretation; cross-validate and probe the disagreement.

## Full classification algorithm (reference `classify_def`, oracle-confirmed)

Order (generator.rb:484-504): visibility → initialize (BYPASSES env lookup
entirely — probe K: an identical declared `initialize` is still emitted
`new_method`; keep rigor-rs's current stub behavior unchanged) →
`simple_parameter_shape` → return inference → `dynamic_top?` skip → THEN:

1. `lookup_existing_method(class, name, kind, env)`:
   - class not declared in env → `nil` → **NEW_METHOD**.
   - class declared → full ancestor-inclusive definition
     (`RBS::DefinitionBuilder#build_instance` / `build_singleton`); member by
     `(name, kind)` — instance decl does NOT shadow a singleton candidate
     (probe H).
2. Found → `compare_against_declared`:
   - declared return = union over ALL overloads' translated returns; any
     overload failing translation is dropped; ALL failing → treated as
     equivalent (drop).
   - `declared_rbs == inferred_rbs` (erased strings) → drop (probe C, P).
   - NOT (`tighter?(declared, inferred)`) → drop (probes E untyped, F wider,
     G multi-overload — all collapse to silence; no "widened" class exists).
   - `computed_literal_tightening?` (a Constant whose def tail is NOT a
     directly-authored literal node) → drop.
   - else → **TIGHTER_RETURN** with `declared_return_rbs` = declared erased
     string.
3. `tighter?` = `declared.accepts(inferred).yes? && !inferred.accepts(declared).yes?`
   (gradual acceptance) AND none of the three lenience-loss guards:
   union-member loss / `narrows_collection_to_shape?` (declared collection
   nominal vs inferred Tuple/HashShape) / `replaces_untyped_type_arg?`.
4. `attr_reader name: T` classifies exactly like `def name: () -> T` (probe I).
5. Malformed project sig → whole env build fails → stderr warning (once) →
   EVERY candidate degrades to `new_method`, exit 0 (probe L). rigor-rs
   equivalent: `for_project` isolates per-file parse failures (ADR-0016), so
   rigor-rs degrades per-FILE not whole-env — deliberate divergence per ADR-79
   (record it; do NOT replicate whole-env collapse).
6. `sig/` is auto-scanned by default with no `.rigor.yml` (probe M) —
   rigor-rs's `Config` default `signature_paths: ["sig"]` already matches.

## Output surfaces (byte-exact, probe D)

- `--print` tag: `# [tighter, was: <declared_return_rbs>]`
- `--diff`: `- def <name>: () -> <declared_return_rbs>` line before the `+`
  line (note: HARDCODED `()` param list; header always `Class#method` even for
  singletons — probe H).
- JSON: `declared_return_rbs` present ONLY on tighter_return candidates.

## rigor-rs mapping (substrate verified in-session)

Env: build the sig-gen index with `CoreIndex::for_project(&cfg.effective_plugins(root),
&cfg.all_signature_dirs(root))` instead of `CoreIndex::new()` (matches the
reference loading core+stdlib+project sig; plugins ≈ vendored overlays).
CAUTION: switching the index changes `type_display::erase`'s resolver input —
should be inert (more names resolvable, never fewer) but gate with the full
sweep.

Lookup: the KEY ALIGNMENT — rigor-rs's conservative `CoreIndex::method_return`
(ancestor-resolved; returns `Some(class_name)` ONLY for a single concrete
`ClassInstanceType`/Optional across ALL overloads; literal/union/untyped/
generic-args/overload-disagreement → `None`) maps onto the reference's drop
set almost exactly:

| reference outcome                       | rigor-rs decision                     |
|-----------------------------------------|---------------------------------------|
| declared literal (C) → equivalent drop  | method_return None → drop             |
| declared untyped (E) → drop             | None → drop                           |
| inferred wider (F) → drop               | class-name mismatch → drop            |
| multi-overload (G) → drop               | disagreement → None → drop            |
| tighter (D/I/N/O)                       | declared name == nominal-of(inferred) |
| equivalent (P)                          | declared name == inferred erased      |

Sketch (per candidate, after the existing skips, instance and singleton
symmetrical via `class_has_method`/`class_has_singleton_method` +
`method_return`/singleton return lookup — check what the index offers for
singleton returns; may need a small addition):

```
if !(index.knows_class(class) && index.knows_toplevel_class(class)) => NEW  // gate
if !index.class_has_method(class, name)                            => NEW
// declared present:
match index.method_return(class, name) {
  None => DROP  // untyped/literal/union/multi-overload/unknown — reference drops all
  Some(declared_name) => {
    let inferred_erased = erase(inferred);
    if inferred_erased == declared_name => DROP                     // equivalent (P)
    if class_name_of(inferred) == Some(declared_name)
       && !narrows_collection_to_shape(declared_name, inferred)     // Tuple/HashShape vs Array/Hash
       && is_directly_authored_literal_or_composite(tail_node)      // computed_literal_tightening
       => TIGHTER(declared_return_rbs = declared_name)
    else => DROP                                                    // wider/unrelated (F)
  }
}
```

Known deviations to DOCUMENT (all FP-safe under-emit or established policy):
- `class_has_method`'s "incomplete ancestor chain ⇒ true" causes over-DROP
  under an unknown superclass (e.g. `< ApplicationRecord` without Rails sig) —
  under-emit, never a wrong byte.
- `method_return` None where the reference computes a translatable non-simple
  declared type that IS strictly wider (rare: e.g. declared `Integer | String`
  vs inferred `"x"` — reference emits tighter with `was: Integer | String`,
  rigor-rs drops) — under-emit.
- Whole-env-collapse on malformed sig NOT replicated (ADR-79 divergence).
- The short-name collision gate (`knows_toplevel_class`) may drop a candidate
  for a project class named like a nested stdlib class — under-emit; note in
  code.

## Gates for the implementation slice

1. Unit tests: gate logic (no-env NEW, empty-decl tighter, inherited
   equivalent-drop, singleton kind separation, attr_reader, literal-vs-computed
   constant, collection→shape guard).
2. Oracle E2E fresh-dir: scenarios A–P (agent's 13 + N/O/P) — `--print` +
   `--diff` + JSON byte/content-identical on every one EXCEPT the documented
   deviations above (L's whole-env collapse; enumerate expected diffs
   explicitly in the test script).
3. The full intersection sweep over `reference/rigor/lib` (the reference repo
   HAS a sig/ → this now exercises classification on real code): shared-method
   rbs-mismatch MUST be 0, and the previously-observed `def hash` excess on
   `observed_call.rb` MUST disappear.
4. Writer E2E re-run (all 9 update scenarios must stay byte-identical — the
   generation-time classification feeds the writer's skipped-entry fields;
   reconcile with the write-time extraction so fields don't double-diverge).
5. Full workspace tests + clippy + harness run.rb/run_snapshot.rb 54/54 0 FP
   (the sig-gen index switch to `for_project` must not touch the check path —
   it's a sig-gen-local index build).

## Delegation plan (next session)

Opus implementer on branch `sig-gen-env-classification`, prompt = this note +
pitfalls: (a) the env gate is the CLASS's presence, not the method's; (b)
initialize bypasses everything; (c) `--diff`'s hardcoded `()`; (d) singleton
returns may need an index addition — investigate `CoreIndex` singleton return
surface first; (e) zsh/word-split + `.rigor/cache` probe traps; (f) do NOT
replicate the whole-env collapse. Main session audits with independent probes
(N/O/P + sweep) before merge.

---

# AMENDMENT (2026-07-11, main session) — the substrate mapping above is WRONG

Before dispatching the implementer, the main session re-probed the oracle AND
read `crates/rigor-index/src/rbs.rs`. **Four load-bearing claims in the
"rigor-rs mapping" section above do not survive contact with the substrate.**
The classification RULE (probes A/N/O/P, `classify_def` order, output surfaces)
is re-confirmed byte-for-byte and stands unchanged. What follows REPLACES the
"rigor-rs mapping" section and gate 3.

## Re-probed and CONFIRMED (unchanged)

`A` (no sig → `# [new]`), `N` (empty `class Foo` in sig + `def hash; 1; end` →
`# [tighter, was: Integer]`), `P` (`def hash; [1].size; end` → `No candidates`),
`K` (an identical declared `initialize` still prints `# [new]`), `I`
(`attr_reader name: String` → `def name` classifies `[tighter, was: String]`),
plus the byte-exact `--diff` / JSON surfaces.

New probes that pin the DROP set — every one prints `No candidates`:

| declared in sig            | inferred | outcome |
|----------------------------|----------|---------|
| `() -> String?`            | `"hi"`   | DROP (declared-union member loss) |
| `() -> String \| Integer`  | `"hi"`   | DROP (same) |
| `() -> String` \| `(Integer) -> Integer` (2 overloads) | `"hi"` | DROP |
| `() -> untyped`            | `"hi"`   | DROP |
| `() -> Integer` (wider inferred `"hi"`) | `"hi"` | DROP |
| `() -> Array[Integer]`     | `[1, 2]` | DROP (`narrows_collection_to_shape?`) |

⇒ **The conservative alignment is even cleaner than the table above claimed:
a declared return resolves ONLY when it is a single overload returning a bare
concrete `ClassInstanceType` with no type args. Everything else DROPS.**

The table's row `| equivalent (P) | declared name == inferred erased |` is
MISDIAGNOSED: in probe P rigor-rs infers `1` (tuple-projection fold), not
`Integer`, so a string compare would NOT drop it. The real driver is
`computed_literal_tightening?` — inferred is a `Constant` and the def's tail is
not a directly-authored literal node. **Both mechanisms must be implemented.**

## FALSIFIED CLAIM 1 — `CoreIndex` cannot answer the env gate (FQN)

`rigor-index` keys every class by its **short name** (`rbs.rs` `Builder::merge`),
and `merge` FOLDS distinct classes that share a short name into ONE entry. So:

- `index.knows_class("Rigor::SigGen::ObservedCall")` is **always false**.
- `M::Foo` and `N::Foo` share one `ClassEntry` — their returns are not separable.

Probe `Q1_nested` (sig `module M; class Foo; end; end`, source `M::Foo#hash`):
oracle → `# [tighter, was: Integer]`, rigor-rs today → `# [new]`. The reference
gates on `env.class_decls.key?(TypeName)` — a **fully-qualified** key.

**The sketch's `index.knows_class(class) && index.knows_toplevel_class(class)`
gate would classify EVERY nested class as `new_method`** — i.e. it would leave
the exact hard-guarantee break this slice exists to close, on every real project
(where classes are namespaced).

## FALSIFIED CLAIM 2 — `CoreIndex` has no singleton return

`ClassEntry::singleton_methods` is `name -> arity`; no return type is stored.
Probe `Q2_singleton` (sig `def self.build: () -> Integer`) → oracle
`# [tighter, was: Integer]`. Probe `Q3_sing_inherit` (empty `class Foo` in sig,
source `def self.hash; 1; end`) → oracle `# [tighter, was: Integer]` — the class
OBJECT inherits `Object#hash` through `Class`/`Module`/`Object`. Both need a
singleton return surface that does not exist.

## FALSIFIED CLAIM 3 — the conservative predicates answer the wrong question

`class_has_method` / `class_has_singleton_method` return `true` when the ancestor
chain is INCOMPLETE ("assume present ⇒ stay silent"), which is correct for a
diagnostic rule and wrong here: it conflates *not declared* (→ `new_method`,
EMIT) with *declared, return unresolvable* (→ DROP, silent). Classification needs
a **precise three-valued** lookup:

```
NotDeclared            => NEW_METHOD          (emit `# [new]`)
Declared(None)         => DROP                (untyped/union/optional/multi-overload)
Declared(Some(class))  => compare             (tighter / equivalent)
```

## FALSIFIED CLAIM 4 — gate 3's `def hash` excess does not exist

Verified directly: from `reference/rigor`, `sig-gen --print
lib/rigor/sig_gen/observed_call.rb` is **already byte-identical** between the two
tools (both `# [new] def hash: () -> Integer`) — because `Rigor::SigGen::ObservedCall`
is not declared in `reference/rigor/sig/` at all, so the reference also takes the
`new_method` path. The `Object#hash` excess recorded in the Writer slice was a
WRITER-scenario fixture artifact, not a `reference/lib` sweep finding. **Do not
gate on it.** (It should still disappear in the writer scenarios, where the
fixture sig DOES declare the class.)

## CORRECTED rigor-rs mapping — a sig-gen-local `SigEnv`

Do **not** switch the sig-gen index to `CoreIndex::for_project` for the gate.
Keep `CoreIndex` for typing/erasure and build a separate, sig-gen-local,
**FQN-keyed** declaration env. The check path stays untouched by construction.

`SigEnv` (new module, e.g. `crates/rigor-cli/src/sig_gen/sig_env.rs`; parse the
`**/*.rbs` under `cfg.all_signature_dirs(root)` with `ruby_rbs` — the same crate
`LayoutIndex` already uses, and `decl_full_name` already computes qualified
names):

```
decls: HashMap<String /*FQN*/, Decl>
Decl {
  superclass: Option<String>,        // as written in the RBS
  includes: Vec<String>,
  extends: Vec<String>,
  instance:  HashMap<String, Option<String>>,  // method -> resolved return class
  singleton: HashMap<String, Option<String>>,
}
```

- A member's return resolves to `Some(name)` **only** for a single overload whose
  return is a bare `ClassInstanceType` with no type args; otherwise `None`
  (probed: optional / union / untyped / multi-overload / generic all DROP).
- `attr_reader x: T` contributes `instance["x"] = resolve(T)`; `attr_writer` /
  `attr_accessor` follow the reference's member expansion.
- `def self.x` → `singleton`; `def self?.x` → BOTH.

`lookup(class_fqn, method, kind) -> Lookup`:

1. `decls.get(class_fqn)` — miss ⇒ if `core.knows_toplevel_class(class_fqn)`
   (a project class shadowing a core/stdlib name — the reference resolves it the
   same way) delegate to the core lookup below; else **`NotDeclared`**.
2. Project chain (first defining ancestor wins): own members → `includes` →
   `superclass` (recurse). For `kind == singleton`: own singleton members →
   `extends`' *instance* members → superclass's singleton chain → finally the
   INSTANCE surface of `Class`/`Module`/`Object`/`Kernel`/`BasicObject` via the
   core lookup (this is what makes probe Q3 resolve).
3. A chain link that is neither in `decls` nor a known core toplevel class ⇒ the
   chain is INCOMPLETE.
4. Core lookup: the ancestor chain of a core class is fully loaded, so a precise
   three-valued answer is available — **add it to `rigor-index`** (see below).
5. Not found, chain COMPLETE ⇒ `NotDeclared` (emit `# [new]`).
   Not found, chain INCOMPLETE ⇒ **`Declared(None)` ⇒ DROP.** (Deliberately
   conservative: the reference, whose env is complete, might find a declaration;
   emitting `# [new]` there would be a shared-method TAG MISMATCH — the exact
   hard-guarantee break. Dropping is a silent under-emit. Note the reference
   itself, when `build_instance` raises on an unresolvable superclass, rescues to
   `nil` ⇒ `new_method`; that divergence is under-emit and acceptable.)

### The `rigor-index` addition (small, additive, check-path-inert)

`ClassEntry::singleton_methods` becomes `name -> (Option<&'static str> /*ret*/, usize /*arity*/)`
(three call sites use only the key set). Then two new precise, non-conservative
accessors, documented as **sig-gen-only — NOT diagnostic predicates**:

```rust
/// `None` = not declared anywhere on the (fully loaded) chain;
/// `Some(None)` = declared, return is not a single bare concrete class;
/// `Some(Some(c))` = declared, returns `c`.
pub fn declared_instance_return(&self, class: &str, method: &str) -> Option<Option<&'static str>>;
pub fn declared_singleton_return(&self, class: &str, method: &str) -> Option<Option<&'static str>>;
/// Whether the flattened ancestor chain of `class` is fully loaded.
pub fn chain_complete(&self, class: &str) -> bool;
```

`declared_singleton_return` mirrors `class_has_singleton_method`'s surface
(own `def self.` up the superclass chain, `extend`ed modules' instance methods,
then the five base classes) but returns the resolved return instead of a bool,
and never "assumes present".

### classify (per candidate, AFTER every existing skip, BEFORE rendering)

```
if sig.name == "initialize" && !sig.singleton  => unchanged stub, NEW_METHOD   // bypasses env
match sig_env.lookup(class_name, name, kind) {
  NotDeclared            => NEW_METHOD,
  Declared(None)         => DROP,
  Declared(Some(decl)) => {
    if erase(inferred) == decl                          => DROP   // equivalent
    if class_name_of(inferred) != Some(decl)            => DROP   // wider / unrelated
    if narrows_collection_to_shape(decl, inferred)      => DROP   // Array/Hash/Set/Range vs Tuple/HashShape
    if computed_literal_tightening(inferred, tail_node) => DROP   // Constant + tail is not a direct literal
    TIGHTER_RETURN { declared_return_rbs: decl }
  }
}
```

- `computed_literal_tightening`: `inferred` is a `Type::Constant` AND
  `ast.get(*sig.body.last())` is not one of `IntegerLit | FloatLit | StringLit |
  SymbolLit | TrueLit | FalseLit | NilLit`. **No assignment unwrap** — the
  reference's `body_last_expression` does not unwrap, so `def m; x = 1; end`
  DROPS (`tail_ty` still unwraps for TYPING; only this guard sees the raw tail).
- `narrows_collection_to_shape`: reference `GENERIC_COLLECTION_CLASSES` — read
  the constant, do not guess the member list.
- `class_name_of(inferred)` is the existing nominal-of helper (`1` → `Integer`,
  `"hi"` → `String`, `[1,2]` → `Array`). The collection guard fires exactly where
  that helper would otherwise green-light a Tuple/HashShape.

### Rendering (all three surfaces)

- `Candidate` gains `classification: &'static str` + `declared_return_rbs: Option<String>`.
- `render_text`: `# [tighter, was: {declared}]` instead of `# [new]`.
- `diff_string`: emit `- def {method}: () -> {declared}\n` before the `+` line
  when `declared_return_rbs.is_some()`. **HARDCODED `()`; the `-` line uses the
  BARE method name even for a singleton** (probed: `- def build: () -> Integer`
  under a `+ def self.build:` line), and the header stays `Class#method`.
- `render_json`: real `classification`; `declared_return_rbs` present ONLY on
  tighter candidates (the reference `.compact`s the nil).
- EQUIVALENT candidates are simply never constructed (the reference builds them
  and the renderer's `EMITTABLE` filter drops them — same observable output).

### `--write` reconciliation

The Writer's write-time `extract_method_return_text` fallback stays; a candidate
now arriving as `tighter_return` from GENERATION must not have its fields
overwritten by the write-time extraction. All 9 Writer E2E scenarios must remain
byte-identical (gate 4 below is unchanged and is the real test).

## CORRECTED gates

1. Unit tests: FQN gate (nested class), no-env → NEW, empty-decl → tighter,
   inherited equivalent-drop, **singleton own + inherited-through-`Class`**,
   attr_reader, literal-vs-computed constant, collection→shape guard, optional /
   union / untyped / multi-overload declared → DROP.
2. Oracle E2E fresh-dir: A, N, O, P, K, I, Q1_nested, Q2_singleton,
   Q3_sing_inherit, Q4, plus the six DROP rows above — `--print` + `--diff` +
   JSON byte/content-identical. (Scenario L's whole-env collapse is NOT
   replicated — ADR-79 divergence; assert rigor-rs's per-file degradation
   explicitly instead.)
3. Intersection sweep over `reference/rigor/lib` **run from `reference/rigor`
   (its `sig/` present)**: shared-method rbs-mismatch MUST be 0 **and the
   `# [tighter, was: …]` TAG must match on every shared method** (the sweep must
   be extended to compare the tag, not just the rbs line — today it would not
   have caught Q1). Do NOT gate on a `def hash` excess; there is none.
4. Writer E2E: all 9 update scenarios byte-identical.
5. Full workspace tests + clippy + harness `run.rb`/`run_snapshot.rb` 54/54 0 FP.
   The check path must be untouched: `SigEnv` is sig-gen-local, and the
   `rigor-index` addition is additive (no existing predicate changes behavior).
