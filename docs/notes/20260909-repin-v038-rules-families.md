# Re-pin `v0.3.4 → v0.3.8` — families F-C and F-D, ported (2026-09-09)

Implementation record for §§ 3–4 of
[the port spec](20260909-repin-v038-port-spec.md). Both families are upstream
RETRACTIONS: the reference stopped emitting a diagnostic and the port did not, so
the entire fix is a DECLINE. Neither change can create a false positive; the whole
risk is over-declining, which is what every control below is for.

Every row was measured 2026-09-09 against the PINNED reference at `ffb456b0`
(rigor 0.3.8) and, for attribution, against the PREVIOUS pin `b10bd5df`
(rigor 0.3.4) — one fresh temp cwd per run, `--no-cache`, both `-I` libs pinned
(`UPSTREAM.md` hazards 1–3; the reference was populated by an offline
`--shared` clone of the superproject's submodule, never from the network and
never via `REFERENCE_RIGOR_DIR`).

Caveat on the `v0.3.4` column: the host resolves rbs 4.2.0 for both checkouts,
where the older pin bundled 4.1.1. It is used for ATTRIBUTION only (did this row
fire before?), never as a parity baseline.

## F-C — a mixin-module receiver, and a `Class`/`Module` value, are not enumerable

Upstream #739 / PR #741 (`3636649f`) and #742 / PR #743 (`23341a87`).

### The decline predicate, and where it is applied

`crates/rigor-rules/src/lib.rs`:

```rust
const METACLASS_ARMS: &[&str] = &["Class", "Module"];

fn unenumerable_metaclass_receiver(class_name: &str) -> bool {
    METACLASS_ARMS.contains(&class_name.strip_prefix("::").unwrap_or(class_name))
}

fn unenumerable_instance_receiver(index: &CoreIndex, class_name: &str) -> bool {
    unenumerable_metaclass_receiver(class_name) || index.is_qualified_module(class_name)
}
```

Applied at four seams, all of them `call.undefined-method` and nothing else:

| seam | predicate | why |
|---|---|---|
| `check_call`, `Type::Singleton` branch (after `class_name_for_id`) | `unenumerable_metaclass_receiver` | the reference's `unenumerable_receiver?` sits ABOVE the instance/singleton split, so `Class.typo` / `Module.typo` decline; the MODULE half deliberately does not reach here |
| `check_call`, source-registry path (after `class_name_for_id_of`) | `unenumerable_instance_receiver` | the project-`sig/` shape upstream measured on redmine |
| `check_call`, core-id path (after `class_name_of`) | `unenumerable_instance_receiver` | defence in depth — `CORE_CLASSES` holds no module today |
| `check_narrowed_call` (after the snapshot's `class_name`) | `unenumerable_instance_receiver` | **the path the whole family actually fires from** |

`check_collection_call` is untouched: its snapshot only ever carries `"Array"` /
`"Hash"`.

**The spec named `check_call` as the port site; that is not where the FPs were.**
Every c/x row below is a `return unless v.is_a?(…)` guard on a `def` parameter,
which types `v` through `Typer::class_narrowing_snapshots` — a `Dynamic` carrier
that `check_call` declines on (`class_name_of` and `class_name_for_id_of` both
answer `None`). Fixture 91's retracted row 42 fires from `check_narrowed_call`.

**`index.is_module` cannot answer the question.** The spec asked for it to be
verified, and it fails: the short-key map files a nested declaration under its
LEAF, so `is_module("Digest::Instance")` is `false` (and bare `"Instance"` is a
defect-2 merge of every nested `Instance`). Added
`CoreIndex::is_qualified_module`, which reads the ISOLATED qualified registry —
the faithful shape of the reference's `Environment#rbs_module?`, which looks the
parsed name up exactly in `env.class_decls`. A top-level module's qualified key IS
its bare name, so `Kernel` / `Enumerable` / `Comparable` answer there too. Pinned
by `qualified_module_predicate_answers_for_nested_and_toplevel_names`
(`crates/rigor-index/src/lib.rs`), which asserts the short-key hole explicitly.

### Rows

`c*` are the spec's; `x*` are new here. "0.3.4" is the previous pin's oracle.

| row | shape | 0.3.4 | 0.3.8 | port before | port after |
|---|---|---|---|---|---|
| c1 | `is_a?(Digest::Instance)` → `v.typo` | fires | silent | fires | **silent** |
| c2 | `is_a?(Enumerable)` → `v.typo` | fires | silent | fires | **silent** |
| c3 | `is_a?(Comparable)` → `v.typo` | fires | silent | fires | **silent** |
| c4 | `Digest::Instance.typo` | fires | fires | fires | fires — BOTH |
| c5 | `Comparable.typo` | fires | fires | fires | fires — BOTH |
| c6 | `is_a?(Digest::Instance)` → `v.class.typo` | fires `singleton(Digest::Instance)` | silent | silent | silent |
| c7 | `is_a?(Class)` → `v.typo` | fires | silent | fires | **silent** |
| c8 | `is_a?(Module)` → `v.typo` | fires | silent | fires | **silent** |
| c9 | `is_a?(String)` → `v.typo` | fires | fires | fires | fires — BOTH |
| c10 | `is_a?(Kernel)` → `v.typo` | fires | silent | fires | **silent** |
| c11 | `String.typo` | fires | fires | fires | fires — BOTH |
| c12 | `is_a?(Digest::Instance)` → `v.hexdigest.typo` | fires `String` | fires `String` | silent | silent — REF gap |
| c13 | project `module ProjMix` → `v.typo` | silent | silent | silent | silent |
| **x1** | `is_a?(Digest::Instance)` → `v.hexdigest(1,2,3)` | **`call.wrong-arity` fires** | **`call.wrong-arity` fires** | silent | silent — REF gap |
| **x2** | `is_a?(String)` → `v.upcase(1,2,3)` | `call.wrong-arity` fires | `call.wrong-arity` fires | silent | silent — REF gap |
| **x3** | `Class.typo` (SINGLETON read) | fires `singleton(Class)` | silent | fires | **silent** |
| **x4** | `Module.typo` (SINGLETON read) | fires `singleton(Module)` | silent | fires | **silent** |
| **x5** | `is_a?(Class)` → `v.class.typo` | fires `singleton(Class)` | silent | silent | silent |
| **x6** | `is_a?(Digest::Instance)` → `v.inspect` | silent | silent | silent | silent |
| **x7** | `is_a?(Enumerable)` → `v.each_slice(2)` | silent | silent | silent | silent |
| **x8** | `case v when Comparable` → `v.typo` | fires | silent | fires | **silent** |
| **x9** | `case v when Class` → `v.typo` | fires | silent | fires | **silent** |
| **x10** | `kind_of?(Kernel)` → `v.typo` | fires | silent | fires | **silent** |
| **x11** | `h = Array.new; h.typo if h.is_a?(Enumerable)` | fires `Array` | fires `Array` | fires | fires — BOTH |
| **x12** | `instance_of?(Comparable)` → `v.typo` | fires | silent | fires | **silent** |
| **x13** | `is_a?(Module)` → `v.ancestors` | silent | silent | silent | silent |

### What the new rows settle

- **The arity probe the spec asked for: `call.wrong-arity` did NOT move.** x1
  fires on the oracle at BOTH pins. Upstream's `unenumerable_receiver?` has one
  caller — the undefined-method diagnostic — and the arity rule reads the
  narrower `unbounded_receiver_surface?`, which `Class`/`Module` and modules were
  never in. So the decline is scoped to `call.undefined-method`, and the port's
  arity path (`check_wrong_arity`, its own `class_name_of` at line ≈1808) is left
  alone. rigor-rs is silent on x1 AND on its `String` control x2, so this is the
  pre-existing "no arity on a narrowed receiver" gap, not evidence about modules.
- **The singleton side is touched in exactly one place.** x3/x4 fired at `v0.3.4`
  and are silent at `v0.3.8` — `unenumerable_receiver?` runs before the
  instance/singleton split, so a `Singleton[Class]` carrier declines too. These
  were live rigor-rs false positives the spec's table did not have, and no fixture
  exercised them. c4/c5/c11 are the countervailing controls: a namespace module's
  `module_function` / `def self.` surface is real and enumerable, and an
  over-broad "decline every `Singleton`" would silence all three.
- **x11 is the control an "any module named in the guard" test would fail.**
  `Array < Enumerable` is a SUBCLASS ordering, so the guard keeps the `Array`
  bound and the narrowed `class_name` is `"Array"`, not `"Enumerable"`. Both
  engines still fire. The port reads the RESOLVED carrier, so this is free — but
  it is exactly the shape a name-based test breaks on. (Fixture 91 already
  carried this row as `shape_supertype`.)
- **`mixin_self_class_receiver?` (#739's syntax-keyed half) needs no port.** c6
  and x5 both fired at `v0.3.4` and are silent at `v0.3.8`; rigor-rs is silent at
  both, because `.class` on a Dynamic receiver yields no witnessable carrier. Both
  are pinned in fixture 101 so a future `.class` typing slice cannot reopen them
  silently.
- **x12 reaches parity by a different route on each side.** The reference's
  `instance_of?` guard against a module is `Bot` (nothing is exactly an instance
  of a module), so it never had a receiver to enumerate; rigor-rs narrows to
  `Comparable` and now declines on moduleness. Same answer, different reason —
  recorded so nobody reads it as evidence for the port's narrowing being right.

## F-D — a constant-write meta-new block is a class body

Upstream #590 / PR #619 (`b3d688f7`).

`meta_new_block_body_spans` (`crates/rigor-rules/src/lib.rs`) lost its
`constant_write_values` carve-out and the doc paragraph that justified it. The
carve-out was faithful when written: upstream's `StatementEvaluator` had no
`ConstantWriteNode` handler at all, so a constant rvalue fell to the
pure-expression default, its block was never walked, and `ScopeIndexer.propagate`
handed every node inside the ENCLOSING scope — a nil `self_type` at file top
level, which is what `Scope#toplevel?` keys on. `b3d688f7` adds the handler and
enters the block through the same `enter_meta_class_body` the #319 arm uses.

Test `unresolved_toplevel_fires_in_constant_assigned_meta_class_body` became
`unresolved_toplevel_silent_in_constant_assigned_meta_class_body` (and gained the
`Data.define` spelling). New sibling
`unresolved_toplevel_fires_outside_a_constant_assigned_meta_class_body` holds the
two controls an over-broad port would silence.

| row | shape | oracle `ffb456b0` | port before | port after |
|---|---|---|---|---|
| d1 | `Registry = Class.new do attr_reader :entries end` | silent | fires | **silent** |
| d2 | `Coercible = Module.new do attr_reader :raw end` | silent | fires | **silent** |
| d3 | `Line = Struct.new(:text) do def shout; text.upcase; end end` | silent | fires (`text`) | **silent** |
| d4 | `Point = Data.define(:x) do def dbl; x * 2; end end` | silent | fires (`x`) | **silent** |
| d5 | `Outer::Inner = Class.new do … end` | silent | silent | silent |
| d6 | `Frozen = Class.new do … end.freeze` | fires | silent | silent — REF gap |
| d7 | `Lazy \|\|= Class.new do … end` | fires | silent | silent — REF gap |
| d8 | `Base = Class.new(StandardError) do … end` | silent | fires | **silent** |
| d9 | `module Wrap; Inner2 = Class.new do … end; end` | silent | silent | silent |
| d10 | `Plain = Class.new(parent_of(1)) do … end` — the ARGUMENT | fires | fires | fires — BOTH |
| d10' | … its body `attr_reader :p` | silent | fires | **silent** |
| d11 | `some_dsl_call do attr_reader :d end` (#316) | fires ×2 | fires ×2 | fires ×2 — BOTH |

d10 and d11 are the controls: widening the suppression from "the block body" to
"the whole constant write" takes d10 with it, and widening it from "a meta-new
selector's block" to "any block body" takes d11's two firings with it. Both are
asserted in the unit tests and pinned in fixture 102.

## Residues left, deliberately

- **d6 / d7 — the oracle fires, rigor-rs is silent.** In both the constant's
  rvalue is not the meta-new call (a `.freeze` send; an `||=` operator write), so
  upstream's `meta_new_constant_body_context` declines and still enters the body
  as toplevel. rigor-rs's span scan sees the inner `Class.new do … end` block
  regardless. Closing it needs the rvalue SHAPE test upstream has — a separate
  slice, and silence is never a false positive. Pinned as gaps in fixture 102.
- **c12 — the oracle types the module method's `String` return and witnesses on
  it.** rigor-rs threads no return type off a narrowed module receiver. Pinned as
  a gap in fixture 101.
- **x1 / x2 — no arity check on a narrowed receiver at all.** Pre-existing and
  orthogonal; pinned in fixture 101 so that if the narrowed-arity gap is ever
  closed, the fixture immediately shows whether the module decline wrongly
  extended to arity (it must not).
- **`check_collection_call` left alone.** Its snapshot carries only `"Array"` /
  `"Hash"`; adding the predicate there would be unreachable code.
- **`is_module` (short-key) left in place** for `call.raise-non-exception`, its
  only caller. Its rustdoc now says what it cannot answer.

## Gates

`cargo test --workspace --offline` — 449 + 253 + … all green.
`CARGO_TARGET_DIR=/tmp/rigor-clippy-target-rules cargo clippy --workspace
--offline -- -D warnings` — clean (fresh target dir).
`ruby harness/snapshot.rb` → 4 written (91, 96 line shifts from the rewritten
comments; 101, 102 new), 96 unchanged. `ruby harness/run.rb` and
`ruby harness/run_snapshot.rb` — 100 fixtures, **4** unregistered extras, all of
them the OTHER families' (60:59, 67:36, 67:59 = F-A; 86:134 = F-B). Fixtures 91,
96, 101 and 102 show **0**.
