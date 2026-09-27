# rigor-rules lib.rs module split (#234) — outcome

2026-09-28. `crates/rigor-rules/src/lib.rs` went from **4,497 lines to 95**.
The work was one prep PR (#235), thirteen move-only PRs (#236–#248) and a
doc follow-up (#249), following
`20260928-rules-lib-split-spec.md`. The precedent, the #204 `rigor-infer`
split and the test-module moves from 8 files, is
`20260927-lib-rs-split-outcome.md`. It ran in one session with the
`harness/split` tooling. All 14 split PRs passed review in round 1 (three
batched Opus reviews), and the release sweep of every head matched master:
0 FP, same per-corpus counts.

## End state

`lib.rs` holds the crate docs, which now carry a module map, plus `mod`
declarations, re-exports and the test-module declarations.

| module | lines | holds |
|---|---|---|
| `call_receiver` | 700 | `call.undefined-method`, `call.possible-nil-receiver` |
| `flow` | 682 | the `flow.*` rules except shadowed-rescue |
| `call_arguments` | 526 | `call.wrong-arity`, `call.argument-type-mismatch` |
| `rule_catalog` | 390 | every rule id and `catalog` entry |
| `driver` | 383 | `analyze*`, the single walk |
| `suppression` | 356 | `# rigor:disable`, `disable:`, the rule-token tables |
| `def` | 347 | the `def.*` rules |
| `call_raise` | 331 | `call.raise-non-exception` |
| `suppression_markers` | 253 | the `suppression.*` rules |
| `call_toplevel` | 250 | `call.unresolved-toplevel` |
| `diagnostic` | 136 | `Diagnostic`, `Severity`, `NO_RULE` |
| `scope` | 127 | `ScopedEnv`, span containment, class qualification |
| `void_value_use` | 122 | `static.value-use.void` |

`shadowed_rescue.rs` and `dead_version_guard.rs` did not change.

## The spec predicted every number

This is the first split that ran from a spec written by measuring the file
before any code moved. Every per-step prediction held:
- Moved lines per module: the only differences were P0's added doc-link
  lines, 2 in `call_receiver` and 6 in `rule_catalog`.
- `pub(crate)` added per step: 1, 0, 0, 4, 3, 6, 2, 4, 3, 6, 0, 0, 1.
- Globs: the 4 the spec expected to drop were dropped, and the 9 it expected
  to keep were kept.
- Test-only root imports appeared at steps 8, 11 and 13, as predicted.
- Two hazards were caught up front and fixed in P0: the misplaced
  `unresolved_toplevel_diagnostics` doc, and a mid-file `use` that
  `split_mod` could not move.

The per-step gates were:
- `verify_move` found nothing UNEXPECTED;
- the tests stayed at 270 and the rustdoc warnings at 7;
- the 37 public root pages stayed the same;
- `rigor-cli` kept building;
- clippy 1.88 and `gate.sh` passed.

The reviewers also checked order, which `verify_move` cannot see. For each
step, the moved ranges appear in the new module byte-exact and in order.

## Found along the way

- **#250, a false positive.** `IMPLEMENTED_RULES` left out
  `call.unresolved-toplevel`. As a result, `# rigor:disable call` and the
  config `disable: [call]` never suppressed that rule, while the reference
  does. The spec's §3.11 flagged this, and a probe confirmed it. #251 fixes
  it. #251's review found a second vocabulary gap (`effect`,
  `plugin_trust`), filed as #252 and fixed in #254, plus four smaller
  divergences (#253).
- **P0's reference-style targets on public docs.** They made rustdoc emit
  dead `href="crate::…"` links on docs that point to private items. #249
  turned those links into plain code spans.
- **Tooling.** `harness/split` needed no change for this split. The review
  of the tooling (#232) left one latent gap, #233: the `impl-attr` check
  counts lines, not impls. It is still open, because the crate has no
  attributed impls.
