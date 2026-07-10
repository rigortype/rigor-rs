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
