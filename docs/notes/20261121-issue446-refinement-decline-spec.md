# Issue #446 — refinement decline extensions (investigation + spec)

Re-triaged rows (all probe-verified FPs on master `b2d800d3`):

| row | shape | ref | port |
|---|---|---|---|
| T4/T6 | `refine(String|Comparable){ def m(a,b,c) }` + `using` → `recv.m(1,2,3)` | silent | `call.wrong-arity` |
| T5 | refine members via `alias`/`alias_method`/`attr_*`/`define_method` | silent | `call.undefined-method` |
| T8 | `refine` inside `def self.x` (own method of module) | silent | `call.undefined-method` |
| T9 | `proc{}.refined(M)` / `->{}.refined(M)` / `Proc.new{}.refined(M)` literal body | silent | `call.undefined-method` |
| T11 | `using B` where `B include`s the refining module (transitive) | silent | `call.undefined-method` |
| T12 | `refine(Object){ def anything }` → bare `anything` | silent | `call.unresolved-toplevel` |

Controls (probe-verified): `"a".zq` before `using` fires; `"b".nope` under
`using` fires; `[1].zq` under `using` of a String-refining module FIRES on the
reference (the port's any-target superset is a pre-existing over-decline the
fix must tighten); `x = proc{..}; x.refined(M)` fires (a `.refined` on a
non-literal receiver creates no region); class-body `using`, outside call
fires; `refine(String){def clamp}`/`refine(Comparable){def center}` on
`1.clamp`/`"x".center` FIRE (own definer answers first — `OwnMethod`).

Extra FPs the same machinery covers (probe-verified):

- `refine(ModuleOffLevels){def m}` + `using` → `1.m(1,2,3)` — ref silent
  (a raw module target off the receiver's levels may extend in ahead —
  `refined_off_levels?`), port fires `wrong-arity`.
- `refine` inside an instance `def` / `define_method` / `def M.x` /
  `def <other>.x` / lambda / non-keeps-self block → a targets-wildcard row.
  Under the `*` owner (instance def etc.) it is `{*:{*:[*]}}` which makes
  EVERY module opaque (`opaque_module?` reads `dig(*,*)`) — so EVERY `using`
  region in the project then declines ALL declined rules. Probed: `using RB`
  (empty module) declines `"a".qqq`. Under owner `M` (`def M.x`,
  `define_singleton_method`) it makes `using M` opaque.
- Inside a wildcard-owned `refine` block (`def x { refine(T){…} }`) the block
  region declines EVERYTHING (the `*` name is opaque). Inside a named-owner
  block (`def self.x`, module body) the arg rules decline only via the
  `!OwnMethod.answers?` walk, and the name rules decline iff
  `refining_modules` is non-nil (refine-block activation ⇒ `refinement_active?`
  true).
- Non-constant `using` arg: UNKNOWN in the in-effect list FILE-WIDE —
  declines wrong-arity/argument-type-mismatch file-wide, and the name rules
  for any name `refining_modules` answers. Probed FP: `"x".center(1,2,3)`
  BEFORE `using Module.new {}` is silent on the reference; the port's
  span-tail opaque region misses it.
- `refine` inside a refine block (raw={String:[M]}) ⇒ `"x".center(1,2,3)`
  inside the block declines on ref, fires on port (pre-existing FP).

## Reference machinery (pin `f7595e2`)

- `refined_method_in_effect?(class, call, scope, kind, lexical_sites)` —
  readers: undefined-method scalar + per-union-arm (`refined_on_some_arm?`),
  `unresolved-toplevel` (class `"Object"`), `possible-nil-receiver`
  (`"NilClass"`), `always-raises` (`"Integer"`), `visibility-mismatch`
  (not ported). Decline iff `opaque_refinement_in_effect?` OR
  `refining_modules(class,name,kind)` non-nil AND `refinement_active?`
  (in-effect list holds a module of the answer, or UNKNOWN, or a
  refine-block activation covers the offset, or `@unresolved_using`).
- `refined_redefinition_in_effect?` — readers: wrong-arity,
  argument-type-mismatch. Decline iff in-effect list nonempty AND
  (UNKNOWN or opaque in it OR `!RefinedDispatch::OwnMethod.answers?`).
  `OwnMethod.answers?`: raw in-effect rows for `name` (nil ⇒ own answers);
  wildcard/unreadable key ⇒ no; levels walk — first level that is refined
  or defines the name must define it (proven); `refined_off_levels?` — a
  raw module target off the receiver's levels ⇒ no (extend may put it
  ahead); a raw class off the levels ⇒ `placed_away?` decides;
  `subclass_refined?` — a project subclass whose own level is refined or
  unreadable-with-module-raw ⇒ no.
- `refining_modules(scope, class, method, kind)`: every module whose
  refinement table has `method` (or a names-wildcard row) for a class the
  receiver reaches — instance: target == class or an ancestor
  (`class_ordering == :subclass`), wildcard-class key ⇒ reaches; singleton:
  `ClassObject.reach` (port already models: Object/Module/Kernel/BasicObject,
  `Class` for class objects, a module it may extend).
- `activated_modules(scope, name)`: the modules `using name` puts in effect —
  `name`'s instance-chain modules (include/prepend closure), project
  entries; an unreadable chain ⇒ nil (UNKNOWN); a core/stdlib mixin is
  dropped; any other unnameable mixin enters the list and is OPAQUE.
- `RefineCensus.read_body` — refine-block name harvest: `def` (no receiver),
  `alias`/`alias_method` (literal pair ⇒ NEW name), `attr_reader`/`attr` ⇒
  name, `attr_writer`/`attr_accessor` ⇒ +`name=`, `define_method` literal,
  `undef` literal ⇒ name; visibility calls and `target` define nothing;
  ANY other self-call ⇒ names incomplete (names-wildcard row per target).
  Calls inside `def`/`define_method` bodies and on other receivers do not
  classify.
- Refine placement (`RefineSelf` contexts):
  - module body / `def self.x` / `def` inside `class << self` of the module
    / keeps-self blocks (CORE_ITERATORS each/map/times/… on a literal or
    literal-bound named receiver) ⇒ OWN-METHOD, real rows `{T → names→[M]}`.
  - `def M.x` (const receiver = module last segment) or a
    `define_singleton_method` block ⇒ `{*:{*:[M]}}` (`using M` ⇒ opaque).
  - instance `def`, `define_method` block, `def <other>.x`, lambda, a
    non-keeps-self block, `Module.new {}`, eval blocks ⇒ `{*:{*:[*]}}`
    (GLOBAL: every module opaque ⇒ every `using` region declines all).
  - class bodies (`class <<` included), `Class.new`/`Struct.new`/
    `Data.define` blocks ⇒ DSL, nothing. `refine` at top level ⇒ rows
    under `""` (matches nothing). `refine` with no block / non-self
    receiver ⇒ raises, nothing.
  - `using` inside a `def` raises ⇒ not recorded at all (skip via
    `method_body_spans`).

## Port design

`source_index`:

- `Refinement` gains `names_wildcard: bool`. `target: None` already models
  wildcard/unreadable targets (reaches every receiver).
- File-wide `scan_refinements(ast)` walk (shared by harvest + driver):
  per-module `refines` + `global_wildcard` flag + refine-block regions
  `(span, owner: Option<module>)`.
- `refining_modules(core, class, method, singleton) -> Vec<String>`.
- `own_method_answers(core, class, method, raw_targets) -> bool` — the
  port's decline-side `OwnMethod`: first `level_names` entry that is a raw
  target or the settled first definer; off-levels module target ⇒ false;
  off-levels class target ⇒ false unless its complete ancestor list is
  readable and excludes `class`; project subclass with refined/unreadable
  own level ⇒ false. Singleton receivers ⇒ caller declines outright.
- `expand_using(module)` → {modules, Unknown, Opaque} include closure.

`rigor-rules` driver post-pass (replacing the two existing retains):

- regions: opaque (undeclared-const `using`, `.refined` undeclared arg,
  expansion-opaque) ⇒ drop all `REFINEMENT_DECLINED_RULES` in span;
  unknown (non-const `using` ⇒ whole file; `.refined` non-const arg ⇒
  literal span; expansion-unreadable) ⇒ arg rules drop outright, name
  rules drop iff `refining_modules` non-nil; using-sets (`using` /
  `.refined` of a declared module, include-closure expanded) — a global or
  module wildcard row upgrades a region to opaque; refine-blocks —
  `refined_method_in_effect?` drops iff `refining_modules` non-nil
  (block covered ⇒ active), wildcard-owner block ⇒ opaque.
- `Diagnostic.receiver_classes: Option<Vec<String>>` — dispatch class per
  arm (`singleton(X)` spelled for class objects) so `[1].zq` fires.
- `undef` inside a refine body: not lowered — residual (noted in PR).

Tests: wrong-arity under `using` (T4), `attr_*`/`define_method`/`alias`
names (T5), `.refined` region (T9) + transitive `using` (T11), wildcard
(`def`-nested refine) opaquing an unrelated `using`.
