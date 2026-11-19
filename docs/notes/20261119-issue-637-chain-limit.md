# #637: CHAIN_LIMIT off-by-one on Object-mixin union walks

PR #664 → merged 2026-11-19 (`e95c7e8`). Closes #637. Opus 5.5 +
Grok 4.6 **Approved** on `1279a16`; rebase onto master after #659/#660
was a no-op on the PR diff (tests aside).

## What fired

A top-level `include W0` chain of **exactly 100** project modules was
silent on the reference and fired `call.undefined-method` +
`call.unresolved-toplevel` on the port (pre-existing on master). 99
and 101 matched.

The reference's `ResolutionChain` cuts once it holds more than
`LIMIT` (100) **project entries, root included** (`Builder#cut_position`).
`Inference::ObjectMixins` builds one chain per `EDGE_OWNERS`
(`<toplevel-include>`, then `Object`). 100 modules under a top-level
include is already truncated → `[:truncated]` → `may_define?` → silent.

The port's union walks counted only the modules they reached:

- `collect_mixin_edge` gave **each seed** its own `seen`
- `mixin_closure_may_answer` / `singleton_truncated` started `seen`
  empty

They cut one module late.

## Port shape

- `collect_object_mixins` groups seeds per owner (one budget per
  `EDGE_OWNERS` owner). Each walk's `seen` starts holding the root
  (`""` for the toplevel pseudo-owner).
- `mixin_closure_may_answer` seeds `seen` with the RBS ancestor.
- `singleton_truncated` seeds `seen` with the owner `q` (name-keyed;
  residual #667).
- 4 unit tests: 99 vs 100 on each walk, plus two 60-chains sharing
  one toplevel budget.

## Measured

`harness/probe.py` (full tuples): 99 fires both rules; 100/101 silent;
A60+B60 two top-level includes silent; A50+B49 still fires;
`class Object; include W0` and `Integer.include(W0)` 99/100 match;
`Integer.extend(W0)` 99 fires / 100 silent. Opus re-probed 63 rows.

`fp_audit.py --gaps --sweep`: **0 FP** (mastodon, gitlab-foss/lib,
dependabot-core, net-ssh). Snapshot unchanged. Timing: no per-node
hook.

## Residuals (filed, not bounced)

- #665 — empty project module / call-form `Object.include` drops
  `unresolved-toplevel` (pre-existing, safe side).
- #666 — class-body `extend W` on an RBS class silences `Integer.zzz`
  (pre-existing, safe side; call form matches).
- #667 — self-extend + 99-module include never truncates after the
  name-keyed singleton seed (introduced here, pathological only).
