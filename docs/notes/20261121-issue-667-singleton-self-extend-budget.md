# #667: singleton_truncated's name-keyed seed — a self-extend never truncates

PR #742 → merged 2026-11-20 (`062f33d1`). Closes #667. Review **Approved**
on `1934f8e9` (adversarial: ~90 fresh-dir parity probes, first-hand master
arm at `c7556e6e`, first-hand sweep); the only later commit is the merge of
master `2b010fe3` (#741's fold), CI 5/5 on it.

## What fired (the live FP, master `c7556e6e`)

`Integer.extend(Integer)` + `Integer.include(W0<sub>chain N</sub>)` +
`Integer.zzz`: the reference is silent at N ≥ 99 — the singleton chain
holds `project_entry(Integer, :singleton)` + `instance_lin(Integer)` =
`Integer(:instance)` + the include chain, 2 + N project entries against
`cut_position`'s LIMIT. The port fired at every N ≥ 99: the walk could not
truncate at all. Two stacked causes:

- PR #664's root-entry fix seeded `seen` with `q` **by name**; the
  self-extend edge resolves to `q`, hit the dedup, and the walk never
  descended `q`'s instance closure.
- The walk resolved edges through `override_classes` only, so the
  receiver-form self-extend (no project declaration of `Integer`)
  resolved to `None` and the edge was dropped before the dedup.

## Port shape

`singleton_truncated` (`crates/rigor-infer/src/source_index/resolution_chain.rs`):
`seen` starts **empty** and the budget tests `seen.len() + 1` — the
singleton root's own entry, arithmetically identical to the seeded walk
for every non-self-extend row. An unresolved raw falls back to the
RBS-known name (`knows_qualified_class` / `knows_toplevel_class`), the
`each_mixin_chain` → `instance_lin(name)` analogue for a mixin the
reference resolves.

## The brief's declared spelling is C1-blocked (pre-existing, disclosed)

`module Comparable; include W0; extend Comparable; end; Comparable.zzz`
is unreachable at file level on the port: the C1 lexical-shadow constant
gate (`constant_shadowed` → toplevel declaration → `Dynamic`, spec
`notes/20260717-constant-shadow-gate-spec.md`) types the receiver before
the singleton consult. The port is silent at every N — at N ≤ 98 the
reference fires, a pre-existing coverage loss (safe side), unchanged by
the PR. The budget is pinned at the unit surface instead
(`core_singleton_mixin_may_answer`: 98 answers / 99 truncates, plus a
no-closure sanity row and a `Float.extend(E)` control).

## Measured

`harness/probe.py` (full tuples, both engines, oracle v0.4.8, mise ruby
4.0.5 first on PATH):

- `Integer.extend(Integer)` rows: N = 3/93/98 both fire; N = 99/100/105
  both silent (master: FP).
- Controls unchanged: `Integer.extend(W0)` 99 fires / 100 silent;
  `Float.extend(E)` with `E include W0` 98 fires / 99+ silent;
  `Integer.extend(Comparable)` + `Comparable.include(W0)` 98 both fire /
  99 both silent; rooted operands, other receivers, multi-file splits and
  shared closures (review round) all match.
- `fp_audit.py --gaps --sweep`: **0 FP**, gap totals identical head vs
  master arm (212 `call.undefined-method` / 138 `possible-nil-receiver` /
  59 `argument-type-mismatch` / 18 `always-truthy` / 8 `wrong-arity` /
  2 `dead-assignment` = 437).
- CI 5/5 on `1934f8e9` and on the `54746149` master-merge head (the
  macOS job was once cancelled at the runner — infra, rerun green).

## Residuals (filed, not bounced)

- #748 — the walk has no superclass arm: `Integer.extend(Deep)` over 99
  project superclasses truncates on the reference (`compute_instance`
  concatenates `superclass_lin`) and fires on the port; pre-existing.
- #749 — the fallback resolves on RBS knowledge alone where the reference
  resolves only a `known_user_class` project fact: boundary rows
  (`chain(98)` + an extra `extend(::Math)`) move by exactly one entry,
  port-declines direction; plus the pre-existing reverse decline for a
  receiver-form-include-only operand.
- #750 — the vendored core-RBS snapshot lacks top-level
  `Mutex`/`ConditionVariable`: `Integer.extend(::Mutex)` is
  ref-fires/port-silent.
