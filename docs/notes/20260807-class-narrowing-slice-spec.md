# `is_a?` / `case-when` class narrowing — slice spec (2026-08-07)

Census mechanism 1 ([gap census](20260807-gap-census.md)): **57 candidate
`call.undefined-method` gaps** (8-line-window upper bound; gitlab lib 30,
mail 12, mastodon 6) where the reference narrows a tested local inside the
guarded branch and rigor-rs leaves it `Dynamic`. Candidate rows:
`scratchpad/slice1-candidates.json` (from the 2026-08-07 baseline dump; the
baseline is `gaps-baseline.json` in the same scratch dir — re-derivable via
`python3 harness/gap_census.py --sweep --dump`).

## Oracle probes (pin `v0.3.1`, fresh cwd, `--no-cache`, plugin path pinned)

| probe | shape | reference verdict |
|---|---|---|
| a1 | `if value.is_a?(Hash)` → call in branch | fires `for Hash`; use AFTER the `if` stays silent |
| a2 | ternary `rule.is_a?(Hash) ? rule.frobnicate_zzz : rule` | fires `for Hash` |
| a3 | `case value / when Hash / when String` | fires per clause, `for Hash` / `for String` |
| a4 | rebind (`value = …`) inside the branch, then call | **silent** — rebinding invalidates the narrowing |
| a5 | `unless value.is_a?(Hash); return; end` then call | fires `for Hash` — a TERMINATING branch propagates the opposite edge past the guard (`eval_if` early-return narrowing) |
| a6 | `kind_of?`; `when Hash, String` | fires; multi-condition narrows to the **union** `Hash \| String` |

### Slice 2 (`X.to_s` → String) is REFUTED — do not build

Probe b1: `def f(a); a.to_s.frobnicate_zzz; end` — the reference is **SILENT**
(same with `to_s(16)`; control `"x".frobnicate_zzz` fires). `rbs_dispatch.rb`'s
`receiver_descriptor` declines a `Dynamic[Top]` receiver, so there is no
universal `Object#to_s` fold to mirror; an unconditional fold in rigor-rs would
emit where the oracle is silent — FP by construction. The census's 4 `to_s`
chain gaps (e.g. gitlab `award_emoji.rb:14` `awardable_params[:resource].to_s
.singularize`) reach `String` because the reference types the **block param**
from the cross-file literal array `Helpers::AwardEmoji.awardables` — i.e. they
are receiver-typing gaps of a different, much heavier mechanism (block-param
element typing), not a `to_s` fold. Known-receiver `to_s` already resolves via
the normal Tier-3 RBS path in rigor-rs. Nothing to build; this section is the
record so it is not re-proposed.

## Semantics to mirror (reference, at the pin)

`reference/rigor/lib/rigor/inference/narrowing.rb`:

- `:344 predicate_scopes` → `[truthy, falsey]`; `:979` routes
  `is_a?`/`kind_of?` (`exact: false`) and `instance_of?` (`exact: true`).
- `:1761 analyse_class_predicate`: exactly 1 argument, a **static constant**,
  resolved lexically (`:1845`, `Module.nesting` approximation); receiver must be
  a `LocalVariableReadNode` (`:1784`) or a stable single-hop chain (`:1805` —
  out of slice).
- **`:2425 narrow_class_other`: `Dynamic`/`Top` narrows to `Nominal[C]` on the
  truthy edge and is UNCHANGED on the falsey edge.** This one rule is the whole
  slice: we narrow ONLY locals that are currently `Dynamic`/`Top`, so every
  other carrier (`Nominal`, unions, scalars — `:2311`/`:2374`) is out of scope
  and left untouched, keeping us a strict subset of the reference.
- `:374 case_when_scopes`: subject must be a `LocalVariableReadNode`; each
  clause's body runs under the union of its conditions' truthy edges; an
  unrecognised condition sets `fully_narrowable = false` (`:2158`).
- `statement_evaluator.rb:458 eval_if` / `:506 eval_unless`: branch-scoped
  narrowing + **early-return propagation** (`:481-493`): when a branch
  terminates, the opposite edge applies to the statements after the
  conditional. `scope_indexer.rb:2742 propagate_if_branches` gives the ternary
  (expression-position `if`) the same treatment.
- `scope.rb:194/:198`: rebinding a local invalidates its narrowing.

## Design (rigor-rs)

**A new snapshot pass, not a `TypeEnv` binding.** `ScopedEnv::at` hands an
empty env inside `def` bodies (`crates/rigor-rules/src/lib.rs:2554`), which is
exactly where the archetype lives, so narrowing must ride a per-call-node map
like the ADR-0038 nil pass: `class_narrowing_snapshots(ast, …) ->
HashMap<NodeId /*call*/, ClassKey>` in `crates/rigor-infer/src/lib.rs`,
modelled on `nilable_receiver_snapshots` (`:2048`), consumed in
`analyze_with_source_and_folder`'s per-call loop (`crates/rigor-rules/src/lib.rs:545-570`).

Stage 1 — `Node::If` (covers `if`/`elsif`/`unless`/ternary; one arena variant):

1. Predicate shapes accepted: the WHOLE predicate is
   `local.is_a?(C)` / `local.kind_of?(C)` / `local.instance_of?(C)` — receiver
   `LocalVariableRead`, no safe-nav, no block, exactly one arg which is a
   `ConstantRead`/`ConstantPath` with a statically known name. Anything else
   (`&&`/`||`, negation, chains, ivars) → no narrowing (`Logical` stays
   unmodeled, per ADR-0038 "unmodeled ⇒ decline").
2. Resolve `C` lexically via `Typer::enclosing_prefix` +
   `SourceIndex::constant_shadowed`; if the name is shadowed by a project
   declaration → **decline entirely** (gap, FP-safe — do not narrow to the
   project nominal in this slice).
3. Narrow ONLY when the local's type at the predicate is `Dynamic`/`Top`
   (query the same flow env the pass threads; `narrow_class_other` semantics).
4. Truthy edge only: the then-branch for `if`/ternary, the else-branch (and
   fall-through) for `unless`. The falsey edge is never narrowed.
5. Early-return propagation (mirrors `eval_if:481`): if the branch bound to
   the OPPOSITE edge terminates (its final statement is `return`/`raise` —
   a conservative approximation; missing a termination just loses narrowing),
   the truthy edge applies to the statements following the conditional. This
   is the a5 guard idiom and it occurs in the candidate set.
6. Invalidation inside the narrowed region: any write to the local
   (`LocalVariableWrite`/`OpWrite`/`MultiWrite` target) or a
   `MUTATOR_METHODS` call on it kills the fact for all subsequent uses
   (reuse `collect_flow_writes`/`indexed_flow_writes` span machinery,
   `crates/rigor-infer/src/lib.rs:2449/:2492`). Killing the whole branch on
   any write is an acceptable conservative simplification.
7. Block bodies: facts do NOT enter `block_body` (fresh env, ADR-0038 §3).
   The archetype's `value.deep_transform_keys! { … }` receiver sits OUTSIDE
   the block and must witness.
8. Consumption: for a call whose receiver is the narrowed local, when
   `typer.type_of` returns `Dynamic`/`Top` AND the snapshot has this call
   node, use `Nominal[C]` for `check_call` (undefined-method). Wiring
   wrong-arity/ATM on narrowed receivers is a follow-up, not this slice.

Stage 2 — `case`/`when` (separate commit; needs an arena change):

9. `when` conditions are currently prepended into the clause body with no
   marker (`crates/rigor-parse/src/ast.rs:1424`). Introduce a dedicated
   `Node::When { conditions, body, span }` (or an equivalent explicit split)
   and update EVERY `Node::Case` consumer (`type_of_case_simple_union`,
   `branch_value_type`, `node_children`, the unreachable-branch walk, lowering
   tests). All gates must stay byte-identical before narrowing is added on
   top — land the arena change as its own commit and re-run the gates on it.
10. Narrow when: subject is `LocalVariableRead`, clause has EXACTLY ONE
    condition and it is a static constant (multi-condition unions — a6 — are
    a follow-up; declining them is FP-safe), same Dynamic-only + lexical rules
    as stage 1. Clause bodies only; no falsey threading between clauses
    (we never narrow negative edges).

## FP-safety argument

Every precondition is a strict subset of the reference's: we narrow fewer
shapes (no chains, no ivars, no logic operators, no multi-condition unions, no
non-Dynamic carriers), only on truthy edges, with at-least-as-aggressive
invalidation. Therefore every diagnostic this slice adds is one the reference
already emits at the same site — modulo implementation bugs, which the gates
catch. NOTE (pitfall 7 of the code map): a narrowed `Nominal[Hash]` receiver
makes the site witnessable by MORE rules than undefined-method if wired
broadly; stage 1 deliberately wires `check_call` only.

## Verification (binding)

- Unit tests in `crates/rigor-infer` reproducing the probe matrix a1–a6
  (a6 asserts NO narrowing for multi-condition; a4 asserts invalidation), plus
  block-body decline, `&&` decline, shadowed-constant decline, non-Dynamic
  local decline.
- New fixture `harness/corpus/81_class_narrowing.rb`: positives (if / ternary
  / unless-guard / case-when) + negative controls (rebind-in-branch — rebind
  from a param, NOT an unresolved call, to keep the fixture single-diagnostic;
  use-after-if without termination; block-body use; `when Hash, String`).
  Verify expected lines against the oracle, regenerate
  `harness/snapshots/` via `ruby harness/snapshot.rb`.
- Gates, all green: `cargo build --offline && cargo test --offline`,
  `ruby harness/run.rb` (0 FP), `ruby harness/run_snapshot.rb`,
  `python3 harness/fp_audit.py --gaps --sweep` (**0 FP / 9204 files**),
  `python3 harness/docs_check.py`, fresh-target clippy.
- **Gap-set diff, not grep**: re-run
  `python3 harness/gap_census.py --sweep --dump <new>` and diff against the
  baseline dump. Expected direction: rows leave the gap set (become matched);
  ZERO new FP rows. Report the exact closed-row count vs the 57-row
  candidate list (the window heuristic overcounts; a shortfall is a finding,
  not a failure).
