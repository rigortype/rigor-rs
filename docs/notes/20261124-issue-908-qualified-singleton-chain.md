# #908 — Qualified singleton superclass chain + `open_project_singleton?` (PR #928)

`Resolv::DNS::Resource::IN::A.bogus` fired `call.undefined-method`
(`singleton(Resolv::DNS::Resource::IN::A)`) on the reference and stayed
silent on the port. The mint was never broken — `Type::Singleton` was
produced — the silence came from `qualified_class_has_singleton_method`'s
ADR-0042 Slice-2 measure-first bail: every qualified **class** returned
`true` (assume-present) because the qualified-registry superclass-chain
walk did not exist (the measured hazard that forced it: 36 dependabot
`Gem::Specification` FPs from inherited methods judged absent). The fix
walks the chain with a completeness bit instead of declining classes
wholesale.

## What landed

- **`qualified_singleton_chain(qname)`** — the former
  `qualified_singleton_chain_prefix` walk plus a completeness bit.
  Truncated (incomplete) on: unresolvable `superclass_written`
  (`Foo::Bar < Missing::Parent`), superclass resolving to a module
  (`InheritModuleError`), superclass cycle (`RecursiveAncestorError`),
  hard-unbuildable mid-walk entry, `def nil`/`never` at the walk's own
  start (mirrors `collect_qualified`'s `at_start` rule — probed, no
  observable delta on the return/arity prefix callers).
- **`qualified_class_has_singleton_method`** — `!entry.is_module` bail
  dropped. Presence = own + inherited singleton surface (`def self.x`,
  `attr_* self.x`, `extend`ed module instance methods, singleton aliases
  over chain + base surface). Absence witnessed ONLY when chain-complete
  AND every extend target's instance surface complete AND the five base
  classes (`Class`/`Module`/`Object`/`Kernel`/`BasicObject`) load. A
  module's chain is itself (old single-entry behaviour).
- **`extends_written`** — `extend` members recorded as `(written name,
  member ctx)` beside `extends` (the `includes_written` shape) after the
  review showed the leaf-only store resolved targets the reference can't:
  a flat `class Foo::Bar; extend Util` resolves `Util` at ROOT only
  (`NoMixinFoundError`, not `Foo::Util`), and `extend` to a class is
  `RBS::MixinClassError` — the whole definition fails. The qualified
  surface resolves via `resolve_written_ref` and truncates on class
  targets.
- **`open_project_singleton?` port (upstream #2438)** — a member-less
  project `sig/` declaration + open source declines the singleton
  witness (`call_receiver` Singleton arm). `memberless_project_class`
  (project-sig-introduced + no `MethodDefinition`/`Attribute`/`Alias`/
  `Mixin` member — `sig-gen --write` emits exactly this shape) +
  `SourceIndex::open_source_class` (no class body in `paths:`,
  `OPENING_HOOKS` on either side, singleton-side `extend`, external
  superclass, member-less sig superclass tail, truncated chain).
  Fixtures: `harness/corpus/180_memberless_project_singleton.rb/.sig/`
  (snapshot matches the live reference: 2 diags).
- Removed `qualified_singleton_alias_resolves` (subsumed by the chain
  alias arm).

## Measured outcome

- `fp_audit --gaps --sweep` (8 corpora / ~9200 files, final head):
  **0 FP**; `call.undefined-method` gaps **207 → 205** (−2).
- All 24 `check` parity probes and ~60 review probes byte-identical to
  the oracle; controls (inherited `Gem::Specification.load`, `ERB::Util.h`
  alias, base-surface `name`/`superclass`, `Etc::Group.map` extend) stay
  silent.
- Two review rounds (glm-5.3 then devin/swe-2 — opencode-go quota died
  mid-round-2): round 1 found the memberless family + extend-resolution
  blockers above; round 2 approved with one pre-existing follow-up.

## Follow-ups filed

- **#931** — `call.wrong-arity` never fires on a `Singleton` receiver
  (`class_name_of` is `None` for the carrier): `IN::A.new`,
  `CSV::Row.def_delegator`.
- **#932** — a project `class`/`module` decl shadows the sig `Singleton`
  mint (`constant_shadowed` → `Instance`), so `class Foo::Bar` + memberless
  `sig class Foo::Bar` can't reach the singleton witness at all; the
  deeper `open_source_class` arms (hooks/extends/external-super/tail) are
  dormant until that closes.
- **#933** — `rbs.coverage.definition-build-failed` unimplemented (the
  reference emits a warning row per unbuildable signature class).
- **#939** — the SHORT-key `singleton_lookup`/`singleton_on_chain` still
  treats `extend`-to-class as complete: `class KBar; extend KTarget`
  fires where `MixinClassError` degrades the reference (pre-existing on
  master; the qualified path is the one this PR fixed).
