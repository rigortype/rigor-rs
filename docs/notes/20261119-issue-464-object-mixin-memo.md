# #464: `Object`'s mixin closure is collected once per run, not per diagnostic

PR #635 → merged 2026-11-19 (`0370baa`). Closes #464; residuals filed as
#636 (`to_rendered`/`line_col` superlinearity) and #637 (pre-existing
`CHAIN_LIMIT` off-by-one FP on a 100-module chain, found by the review).

## What was slow

`SourceIndex::object_mixin_may_define` re-walked the whole top-level-include
closure — a `resolve_override_ancestor` string alloc per edge — for **every**
`call.undefined-method` / `call.unresolved-toplevel` diagnostic. On the
issue's synthetic (`harness/gen_object_mixin_perf.py 90 30 5000 --include`,
committed): master 4.67–4.72 s vs 0.87 s without the include.

## Port shape

The closure is method-independent and the index is frozen post-merge, so it
is collected once into an `ObjectMixins` summary — `OnceLock` on
`SourceIndex`, `Sync` under the stage-3 file-parallel `&SourceIndex` — and
each diagnostic asks only the per-method questions of the collected sets.
The union is the removed `mixin_edge_may_define`'s answer, reorganised:

- `cut` — a seed's `seen` passed `CHAIN_LIMIT` → every method (unconditional).
  Each seed still walks with its own `seen`, so the limit still counts
  modules reachable from ONE edge.
- `unnamed` / `tainted` — marker edge / wildcard-or-dirty module → bare
  calls only (`!explicit`), as the early returns did.
- `object_records` — `known_user("Object")` → `records_name("Object", m)`
  per call, under the same `override_classes.get("Object")` guard.
- `modules` — reachable `known_user` modules → `records_name ‖ macro_named`.
- `opaque` — edges resolving to nothing or to a non-`known_user` module,
  kept as `(from, raw, declared)` → `macro_named(declared) ‖
  external_may_define(from, raw)` per call.

Over-collection past an early return is safe: every early `return true` was
already the answer. The summary is a runtime cache, not index state — it is
deliberately outside the s92 field fingerprint.

## Measured outcome

| build | `--include` | `--no-include` |
|---|---|---|
| master (`b94ccbd`) | 4.67–4.72 s | — |
| PR #635 | 0.66–0.67 s | 0.77–0.80 s |

Ratio ≈ **0.86×** (the brief's bar was 1.2×). Byte-identical output on the
synthetic (5,000 diagnostics), on mastodon/gitlab-foss/dependabot-core/
net-ssh, and on a 90-gem / 1,354-file installed tree (4.2 s vs 4.1 s).

The issue's unverified 300-module / 60,000-diagnostic no-include case is
**real and is a different function**: ~160 s on both binaries, `sample` puts
~100% of the busy thread in `diagnostic_formats::to_rendered` → `line_col`
(O(diagnostics × file bytes) re-scan, serial after stage 3). Filed as #636
with the profile.

Review: Opus `Needs fix` (PR-body numbers only — code/parity/gates clean) →
body revised → fix-confirmation `Approved`. Grok 4.6 advisory ran alongside.
