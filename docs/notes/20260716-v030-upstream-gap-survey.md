# v0.3.0-RC upstream gap survey (2026-07-16)

Upstream (`rigortype/rigor`) is at the **v0.3.0 release-candidate** stage
(commit `47ec8625`, 239 commits / ~43k lines past the old `v0.2.7` pin). This
note records the measured behavior gap between rigor-rs and the RC, and indexes
the binding specs for the port slices. The submodule pin was bumped to the RC
commit on branch `pin-v030-rc` (snapshots regenerated: **0 changed, 43
unchanged**; `run.rb` + `run_snapshot.rb` both PASS 54/54, 0 FP — the RC does
not change reference behavior on the existing corpus).

## Measured state (all against the RC reference)

- `harness/run.rb` live vs RC: 54/54 matched, 0 FP, 0 gaps — no regression on
  the fixture corpus.
- `fp_audit.py --gaps` mastodon `app` (1236 files): reference 459 / rigor-rs
  397 / **0 FP / 62 gaps** — byte-for-byte the same breakdown as the 2026-07-11
  measurement against v0.2.7 (undefined-method 33, possible-nil 26,
  always-truthy 2, arg-type-mismatch 1). The RC changes nothing on mastodon.
- gitlab-foss `app` (first 3000 files), RC reference rule frequencies: none of
  the new v0.3.0 rules fire on this corpus either.
- Ported reference constants re-measured at HEAD — **all unchanged**
  (`ARRAY_NEW_TUPLE_LIMIT=16`, `STRING_FOLD_BYTE_LIMIT=4096`,
  `UNION_FOLD_INPUT_LIMIT=32`, `UNION_FOLD_OUTPUT_LIMIT=8`,
  `RANGE_TO_A_LIMIT=16`, `STRING_ARRAY_LIFT_LIMIT=32`,
  `TUPLE_JOIN_BYTE_LIMIT=4096`).

## The actual v0.3.0 gap set

### New diagnostic rules (all probe-confirmed firing at the RC; rigor-rs emits none)

| rule | severity | kind | spec |
|---|---|---|---|
| `flow.duplicate-hash-key` | warning | syntactic | [syntactic-rules spec](20260716-v030-syntactic-rules-spec.md) |
| `flow.return-in-ensure` | warning | syntactic | same |
| `suppression.unknown-rule` / `suppression.empty` | warning | lexical | same |
| `call.raise-non-exception` | error | typed | [typed-rules spec](20260716-v030-typed-rules-spec.md) |
| `flow.shadowed-rescue-clause` | warning | ancestry | same |

### Inference/typing changes ([inference-cluster spec](20260716-v030-inference-cluster-spec.md))

- **`Kernel#p`/`#pp` identity typing** — closes real diagnostic-set gaps
  (probe: `x = p 42; x.frobnicate` fires in the reference, silent in rigor-rs).
  Blocked on a structural gap: rigor-rs has **no implicit-self call dispatch**
  at all (`receiver: None` calls fall to the `Dynamic[top]` catch-all).
- **Scalar-key HashShape** (Integer/Float/true/false/nil keys value-pin;
  duplicate keys last-wins; hashrocket rendering) — message parity on
  undefined-method receivers + real gaps via the (missing) HashShape
  projection tier.
- **Kernel constant-folding** (`format`/`sprintf`, `String()`, `Hash()`, plus
  the pre-existing `Integer()`/`Float()` hole) — all real gaps, gated on the
  same implicit-self dispatch prerequisite.
- `present?`/`blank?` possible-nil narrowing (reference REMOVES diagnostics) —
  rigor-rs is already lenient there, 0 FP measured ⇒ nothing to do.

### Out of scope for parity

- Plugin-only changes (rigor-actionpack/activerecord/rails-routes fixes) — the
  rigor-rs plugin engine doesn't exist (deferred per the plugin-engine note).
- Removed CLI verb subcommands (`docs list` etc.) and the `type_specifier`
  plugin-hook removal — rigor-rs never had them.
- Perf work (YJIT arming, cache validation modes) — explicitly
  diagnostic-byte-identical upstream.
- New CLI surface (`--bleeding-edge`, `plugins` inflection probe, coverage
  `--workers`) — productization candidates, not diagnostic-set parity.

## Slice order (dependency-driven)

1. **Syntactic trio** (duplicate-hash-key, return-in-ensure, suppression.*) —
   needs: HashLit assoc/splat element parity, BeginRescue `ensure_body`,
   LambdaNode lowering, comment column tracking. Branch: `v030-syntactic-rules`.
2. **p/pp identity** + the new implicit-self dispatch entry point.
3. **Scalar-key HashShape** (independent of 2).
4. **Kernel folding** (format/sprintf interpreter; rides on 2's entry point).
5. **raise-non-exception** — needs a public class-ordering (ancestry) API on
   rigor-index + class/module bit.
6. **shadowed-rescue-clause** — needs full `RescueClause` lowering (supersedes
   1's `ensure_body` shape if sequenced together; keep compatible).
