# Eval-block defs attribute to their receiver (issue #141)

Closes [#141](https://github.com/rigortype/rigor-rs/issues/141), merged as PR
[#179](https://github.com/rigortype/rigor-rs/pull/179) (`a7b6120`, merge
`4d3b43b`). The unported `fb781023` eval-block carve-out: `def`s inside
`class_eval`/`module_eval`/`instance_eval`/`class_exec`/etc. were filed at
toplevel, so `"s".str_m` fired `undefined-method` FPs and bare `helper` calls
fired `unresolved-toplevel` where the reference is silent. Sized at
**3,002 of 3,829** sweep gaps — closed them all: mail corpus **9,649 matched /
342 gaps**, 0 FP across all 8 corpora.

## The mechanism

- `Node::ClassDef`/`Node::ModuleDef` carry `rooted` and `self_anchored`; the
  declaration prefix honours the reference's `Source::ConstantPath
  .declaration_prefix` — `class ::X` re-anchors at toplevel, `self::X`
  preserves the rebound self, and a rendered multi-segment name is ONE prefix
  element (`class A::B` inside `module M` yields rungs `M::A::B`, `M` — never
  `M::A`).
- `walk_defs`/`file_orphan_defs`/`collect_declared_names_at` share
  `decl_body_cx` so span-recovered orphan defs (range endpoints, default
  args, dynamic const paths) file identically to walked ones.
- File-locality is two-layer like the reference: eval-defs land in a per-file
  `Object` slice (`file_toplevel`), so `Object.class_eval { def om }` in
  `a.rb` resolves in-file but stays `unresolved-toplevel` from `b.rb`, while
  project toplevel defs are visible cross-file.
- S92 fingerprint gained an 18th field (`file_defs`); legacy-vs-new merge
  tests still pass over permutations.

## What review cost (4 rounds — the deep rounds found real FPs)

- Orphan arena defs and `::`-rooted receivers resolving lexically (r1/r2).
- `discovered_methods` singleton/instance mixing (r2).
- Multi-segment rung split — `class ::M::N` leaked a bare `M` rung that
  resolved `String`→`M::String` (Grok r3).
- Follow-ups filed, all verified pre-existing or bounded: #185 (ADR-17
  def-site annotation in the message), #186 (singleton-side table),
  #187 (`alias` discovery), #188 (remaining orphan shapes), #189
  (`K ||= Class.new` gap), #193 (rooted-header singleton registry).
