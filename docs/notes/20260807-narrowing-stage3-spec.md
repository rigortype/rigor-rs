# Class narrowing, stage 3 — slice spec (2026-08-07)

Sequel to the [stage 1–2 spec](20260807-class-narrowing-slice-spec.md) (PR #63,
11 closures) built on the measured frontier in the
[stage-3 evidence note](20260807-narrowing-stage3-probe-evidence.md). Designed
ON TOP of the in-flight position-rule fix
([probe matrix](20260807-block-narrowing-position-rule.md)): block/`case`
narrowing gated to statement position + local-write RHS is assumed landed; the
position flag it threads is reused here.

Two independently-buildable halves: **3b** records narrowed USES inside
statement forms the flow pass currently discards facts on (grants no new
facts), **3a** adds guard SHAPES (`&&`/`||` conjunctions, `!` negation,
`or raise`, single-hop chains, multi-`when` unions).

## Probe matrix (additions beyond the evidence note)

Pin `v0.3.1`, fresh cwd, `--no-cache`, plugin path pinned; rigor-rs
`target/release/rigor` at master `c146425`. Guard is `return unless
v.is_a?(String)` unless shown; the witnessed call is `v.frobnicate_zzz`.
Probes live in the session scratchpad (`s3probe/`), trivially recreatable.

### 3a — guard shapes

| # | shape | ref | rs | verdict |
|---|---|:--:|:--:|---|
| c1a | `use if v.is_a?(String) && v.length > 2` | 1 | 0 | build (left conjunct) |
| c1b | `use if cond && v.is_a?(String)` | 1 | 0 | build (right conjunct) |
| c1c | `use if a && v.is_a?(String) && b` | 1 | 0 | build (middle conjunct) |
| c1d | `return unless guard && cond` then use | 1 | 0 | build (early return through `&&`) |
| c1g | `if guard && cond … else USE end` | 0 | 0 | control: falsey edge of `&&` stays unnarrowed |
| c2a | statement `v.is_a?(String) && use` | 1 | 0 | build |
| c2b | `x = v.is_a?(String) && use` | 1 | 0 | build (LV-write RHS) |
| c2c | `g(v.is_a?(String) && use)` | **0** | 0 | **decline: the POSITION RULE covers `Logical` minting** |
| c2d | `guard && v.length > 2 && use` | 1 | 0 | build (recursive) |
| c2e | statement `!guard \|\| use` | 1 | 0 | build (`\|\|` RHS runs on LHS falsey edge) |
| c2f | `guard or raise …` then use | 1 | 0 | build (`eval_and_or` termination) |
| f5 | `return(guard && use)` | **0** | 0 | decline: same position rule |
| c4a | `return if !guard` then use | 1 | 0 | build |
| c4b | non-modifier `if !guard; return; end` then use | 1 | 0 | build |
| c4d | `if !guard … else USE end` | 1 | 0 | build (`!` narrows the falsey edge) |
| c4f | `unless !guard; USE; end` | 1 | 0 | build (falls out of edge-swap) |
| f22 | `raise … if !guard` then use | 1 | 0 | build |
| c6a | `when Hash, String then use` | 1 | 0 | build last — msg `for Hash \| String` |
| c6b | `if v.is_a?(Hash) \|\| v.is_a?(String)` | 1 | 0 | same union, via `\|\|` join |
| c6c | `when Hash, String then v.fetch(:a)` | 0 | 0 | control: method on ANY arm ⇒ silent |
| f13 | `when Hash, String then v.upcase` | 0 | 0 | control: other direction of c6c |
| f12 | `if guard \|\| guard \|\| cond` | 0 | 0 | control: unrecognized `\|\|` disjunct kills the join |
| g7 | `when Hash, cond then use` | 1 (`for Hash`) | 0 | DECLINE anyway (mixed conditions; ref narrows to the recognized subset — probed, but out of the all-static envelope) |
| c7a | `h.last.use if h.last.is_a?(String)` | 1 (`for String`) | 0 | build (chain, local root) |
| c7b | ivar root `@h.last.…` | 1 | 0 | decline: arena `VariableRead` is NAMELESS (lowering change needed; follow-up) |
| c7c | `g(h)` between guard and chain use | 1 | 0 | decline (we kill on any root mention; ref kills only on root-RECEIVER calls) |
| f23 | `other.push(h)` between | 1 | 0 | decline (same conservatism) |
| c7d | `h.pop` between | 0 | 0 | control: root-receiver call invalidates |
| c7e | `h.fetch(0).is_a?(String)` guard | 0 | 0 | control: args on the chain hop ⇒ no address |
| c7g | `h = []` rebind between | 1 (`for nil`!) | 0 | control: ref fires via the REBOUND `[].last` fold, NOT the chain — different diagnostic; we must stay silent |
| c7h | inert `x = 1` between | 1 | 0 | build |
| f11 | two chain uses in one branch | 2 | 0 | build: fact survives its own re-read |
| h1 | `return unless h.last.is_a?(String)` then chain use | 1 | 0 | build (chain early-return propagation) |

### 3b — node-kind decision table (statement forms containing a use)

Lowering fact that reframes the evidence note: `cache[v] ||= v.zzz` does NOT
die in `class_flow_stmt`'s `other` arm. `IndexOrWriteNode` (and every
op-assign wrapper without an owned variant) lowers through
`collect_recoverable_children` into a `Statements` carrier
(`crates/rigor-parse/src/ast.rs:2405`, carrier at `:1861`), which
`class_flow_stmt` DOES descend — but the recovered bare
`LocalVariableRead` children (`cache`, `v`) are statements with no arm, hit
`other`, and **clear all facts before the call child is reached**. That is why
`@x ||= use` / `yield use` / `defined?(use)` (no leading local read) already
fire on master while `cache[v] ||= use` does not.

| # | form | arena node | ref | rs | fix |
|---|---|---|:--:|:--:|---|
| d4 | `@x = use` | `InstanceVariableWrite` | 1 | 0 | new arm: descend value, KEEP facts |
| d5 | `$gx = use` | `VariableWrite` | 1 | 0 | same arm |
| d6 | `@@cx = use` | `VariableWrite` | 1 | 0 | same arm |
| d7 | `X = use` (top level) | `ConstantWrite` | 1 | 0 | same arm |
| d8 | `cache[v] = use` | `Call` (`[]=`) | 1 | **1** | none (already fires) |
| d9 | `obj.attr = use` | `Call` (`attr=`) | 1 | **1** | none |
| d1/d10c | `cache[v] \|\|= use` | recovered carrier | 1 | 0 | `LocalVariableRead` no-op arm |
| d10a | `cache[v] += use` | recovered carrier | 1 | 0 | same |
| d10b | `cache[v] &&= use` | recovered carrier | 1 | 0 | same |
| d2 | d1 with nested `if` RHS (mastodon archetype) | recovered carrier | 1 | 0 | same |
| d11/f6 | `obj.attr \|\|= use`, `&&=` | recovered carrier | 1 | 0 | same |
| d12a/b | `@x \|\|= use`, `@x += use` | recovered carrier | 1 | **1** | none (no leading local read) |
| d13 | `$gx \|\|= use` | recovered carrier | 1 | **1** | none |
| d23 | `yield use` | recovered carrier | 1 | **1** | none |
| d24 | `defined?(use)` | recovered carrier | 1 | **1** | none (ref fires inside `defined?` too — probed) |
| g2 | `super(use)` | recovered carrier | 1 | **1** | none |
| g5 | `use rescue nil` | recovered carrier | 1 | **1** | none |
| d25 | `x = *use` | carrier as LVW RHS | 1 | 0 | `class_flow_expr` `Statements` arm |
| g6 | `x = (use rescue nil)` | carrier as LVW RHS | 1 | 0 | same |
| d14 | `a, b = use, 1` | `MultiWrite` value `ArrayLit` | 1 | 0 | `class_flow_expr` `ArrayLit` arm |
| d15 | `x = [use]` | `ArrayLit` | 1 | 0 | same |
| d16 | `x = { k: use }` | `HashLit` | 1 | 0 | `HashLit` arm |
| d17 | `x = "#{use}"` | `InterpolatedString` | 1 | 0 | `InterpolatedString`/`InterpolatedSymbol` arm |
| d19 | `begin USE rescue …` | `BeginRescue` | 1 | 0 | new arm: bound-name kill, descend `body`, clear after |
| d20 | `x = begin USE rescue …` | `BeginRescue` (expr) | 1 | 0 | same arm from `class_flow_expr` |
| f7 | use in RESCUE clause body | `BeginRescue` | 1 | 0 | covered (clause bodies are flattened into `body`) |
| f8 | use in ENSURE body | `BeginRescue` | 1 | 0 | covered (same) |
| g1 | `while use…` (predicate) | `Loop` | 1 | 0 | new arm: descend PREDICATE only |
| d21 | use in `while` BODY | `Loop` | 1 | 0 | stage 3b-2 (needs a `for` discriminator — see below) |
| f9 | `while` body use + rebind after it | 1 | 0 | ref threads lexically, no loop fixpoint — subset-safe to mirror in 3b-2 |
| f10 | use in `for v in […]` body | **0** | 0 | **DECLINE — the `for` index rebind is INVISIBLE in the arena** (`ast.rs:1501` drops the index target) and ref is silent |
| g3 | `break use` in `while` body | 1 | 0 | falls out of 3b-2 |

### Fact survival PAST the new statement arms (all probed)

| # | intervening statement | ref | rs | design |
|---|---|:--:|:--:|---|
| e1 | `@x = 1` | 1 | 0 | keep facts |
| e6 | `$gx = 1` | 1 | 0 | keep |
| e7 | `@@cx = 1` | 1 | 0 | keep |
| e8 | `SOME_CONST = 1` | 1 | 0 | keep |
| e3 | `cache[:k] \|\|= 1` | 1 | 0 | keep (carrier no longer clears) |
| e2/e5 | `cache[v] = 1` / `cache.push(v)` | 1 | **1** | already kept (plain `Call`) |

Survival past `BeginRescue`/`Loop`/`case` stays CLEARED (unprobed → decline).

## Reference semantics (pin `v0.3.1`, `reference/rigor/lib/rigor/inference/`)

- `narrowing.rb:2631 analyse_and`: truthy = truthy(b) evaluated UNDER
  truthy(a) — an unrecognized conjunct falls back to the other's truthy scope,
  which is why any single recognized conjunct narrows (c1a–c1c); falsey =
  `falsey_a.join(falsey_b)` (join = per-local type union of scopes), which for
  class guards is the unchanged entry scope (c1g).
- `narrowing.rb:2640 analyse_or`: truthy = join (c6b union; one unrecognized
  disjunct joins in the unnarrowed scope ⇒ nothing, f12); falsey = falsey(b)
  under falsey(a) — so `!guard || use` evaluates the RHS narrowed (c2e).
- `narrowing.rb:1555 dispatch_unary_predicate`: `:!` = `analyse(receiver)
  &.reverse` — pure edge swap (c4 family).
- `statement_evaluator.rb:458 eval_if` / `:506 eval_unless`: termination
  propagation runs in BOTH directions — a terminating then-branch applies the
  FALSEY scope to the statements after (`:486`, the c4a idiom), a terminating
  else applies the truthy (`:495`, already ported).
- `statement_evaluator.rb:1232 eval_and_or`: when the RHS of a
  statement-position `and`/`or` terminates, the post-scope is the
  LHS-short-circuit edge alone — `guard or raise` continues on truthy(guard)
  (c2f).
- `narrowing.rb:374 case_when_scopes` (subject must be a local read, `:381`) +
  `:2158 accumulate_case_when_scopes`: the clause body edge is the UNION of
  the recognized conditions' truthy types — even when other conditions are
  unrecognized (g7 narrows to `Hash`); `fully_narrowable` only gates the
  falsey edge, which we never narrow.
- `narrowing.rb:1805 analyse_class_predicate_on_chain` + `:1826
  stable_chain_address`: chain guard = `<local|ivar>.<m>` with NO args, NO
  block; consumption `expression_typer.rb:1062 method_chain_narrowing_for`
  (same shape gate); invalidation `statement_evaluator.rb:1326 eval_call` →
  `indexed_narrowing.rb:151 invalidate_chain_after_call` — ONLY a call whose
  receiver is the root variable invalidates (c7c/f23 fire in ref); rebind
  invalidates via `Scope#with_local`.
- `narrow_class_other` (`narrowing.rb:2425`) is unchanged: Dynamic/Top
  carriers only, everything else out of envelope.

## Design (rigor-rs)

All in `crates/rigor-infer/src/lib.rs` unless noted. `check_narrowed_call`
(`crates/rigor-rules/src/lib.rs:1398`) is untouched until the union commit.

### 3b-1 — descend the unmodeled statement forms (no new facts)

`class_flow_stmt` gains arms:

1. `LocalVariableRead` → no-op (today it clears every fact via `other`). This
   single arm closes the whole recovered-carrier op-assign family
   (d1/d2/d10/d11/f6/e3).
2. `InstanceVariableWrite`/`VariableWrite`/`ConstantWrite` → `class_flow_expr`
   on `value`, KEEP facts (d4–d7, e1/e6/e7/e8). No local can be rebound by
   these writes; a write nested in the value threads through the existing
   expression-position write arms.
3. `BeginRescue` → kill facts for every clause `bound_name`
   (`RescueClause.bound_name` — the `rescue => e` rebind is otherwise
   invisible), then descend `body` statement-by-statement (rescue/else/ensure
   statements are flattened into `body` in source order — d19/d20/f7/f8), then
   clear all facts (post-begin survival unprobed). Use-recording under
   exception paths is safe: facts are only KILLED inside, never minted, and a
   runtime path that skips a rebind only makes a recorded fact more true.
4. `Loop` → descend `predicate` via `class_flow_expr` (g1), do NOT descend
   `body` (the `for` index rebind is arena-invisible, f10 is a measured ref=0
   — descending would be a live FP), clear all facts after (unchanged).
5. `Logical` in statement position → route to the 3a-2 logic below; under
   3b-1 alone: descend left then right recording uses (f1/f2 — an OUTER fact
   is valid on both edges), then `kill_cenv_writes(span)` (a conditionally
   executed rebind must kill).

`class_flow_expr` gains pure-descent arms — `ArrayLit`/`HashLit` elements,
`InterpolatedString`/`InterpolatedSymbol` parts, `Statements` (recovered
carrier in expression position, d25/g6: descend via `class_flow_stmt`),
`BeginRescue` (d20, same treatment as the statement arm) — and its `Logical`
arm STOPS clearing facts up front (f1–f4: ref records uses in every position;
minting stays position-gated per 3a-2).

### 3a-1 — compound predicate analysis (`&&`/`||`/`!`)

Replace `class_check_predicate`'s single-shape return with a recursive
`analyse_predicate(pred) -> Option<(FactMap, FactMap)>` (truthy, falsey), a
direct port of the reference dispatch:

- class guard on a local → `({local: C}, ∅)`;
- `!x` (a `Call`, method `"!"`, no args) → swap;
- `&&`: truthy = truthy_a then truthy_b (b's fact wins a same-local
  collision); falsey = join = per-local: kept only when both sides carry the
  SAME class (unions arrive with 3a-4; until then a mismatch drops the local);
- `||`: truthy = join; falsey = falsey_a then falsey_b;
- anything else → `None` (whole-predicate decline, exactly as today).

`class_flow_if` consumes the maps: Dynamic/Top gate and the R3 conflict rule
apply per-local; the falsey edge is now applied too (it is no longer
categorically empty — c4d/c4f). Termination propagation generalizes to both
directions (`eval_if:486`/`:495`, probes c4a/c4b/f22/f16): a terminating
then-branch propagates the falsey map, a terminating else the truthy map —
still statement-position-only, still declined when any write to the local
lands inside the conditional span.

`class_flow_case` keeps its single-static-constant clause rule until 3a-4.

### 3a-2 — `Logical` minting in statement / LV-write-RHS position

For a `Node::Logical` that is a statement or the direct RHS of a
`LocalVariableWrite` (the SAME position gate the in-flight branch introduced —
c2c and f5 measure ref silent in argument/return position):

- `&&`: descend left; descend right under `cenv + truthy(left)` (c2a/c2d —
  recursion handles chained conjuncts);
- `||`: descend left; descend right under `cenv + falsey(left)` (c2e);
- afterwards: if in statement position and the RHS terminates
  (`branch_terminates` on the RHS), continue with `cenv + truthy(left)` for
  `or` / `+ falsey(left)` for `and` (`eval_and_or:1232`, probe c2f);
  otherwise `kill_cenv_writes(span)` and no minted fact survives.

### 3a-3 — single-hop chain guards (local roots only)

New fact family threaded alongside `cenv`: `chain_env: HashMap<(String, String),
String>` keyed `(root local, method)`.

- **Mint**: predicate receiver is `root.m` — `root` a `LocalVariableRead`,
  `m` no-arg/no-block/no-safe-nav — wrapped in the same
  `is_a?`/`kind_of?`/`instance_of?`+static-constant+shadow rules; gate on the
  chain call's `type_of` being Dynamic/Top (the `narrow_class_other`
  envelope). Ivar roots are DECLINED: the arena's `VariableRead` carries no
  name (c7b is a recorded gap; a named ivar-read variant is a follow-up
  lowering change).
- **Record**: a call whose receiver is a `Call` matching the address (same
  root, same method, no args/block/safe-nav) with a live chain fact →
  `out.insert(outer_call, C)`. The fact SURVIVES its own re-read (f11: ref
  fires twice).
- **Invalidate** (strict superset of the reference's rule): any write to the
  root (existing `writes` machinery), any call whose receiver reads the root
  other than the pure address read (c7d `h.pop`), any recorded
  mutation/mutated-argument span mentioning the root, every point where
  `cenv` is cleared (branch joins, blocks, unmodeled forms). This declines
  c7c/f23 (ref keeps through argument-position mentions) — pure coverage
  loss.
- **Propagation**: chain facts ride the same early-return machinery (h1
  probed) and the same truthy-edge-only branch scoping.
- **Consumption**: entries land in the same snapshot map `check_narrowed_call`
  reads; the receiver of the recorded call is the chain call, which types
  Dynamic (gate 3 passes), and the witness/message path is unchanged
  (`for String`, c7a).

The rebind control c7g is load-bearing: ref's hit there is a DIFFERENT
diagnostic (`for nil`, from folding the rebound `[].last`) — the write-kill
must fire so we stay silent.

### 3a-4 — multi-condition `when` unions (build last, or never)

Value-type change: the snapshot map and `cenv` values become `Vec<String>`
(ordered, deduped). Minting: a `when` clause narrows iff EVERY condition is a
static resolved constant (mixed conditions — g7 — DECLINED even though ref is
probed to narrow to the recognized subset; the decline is a strict subset);
`||` chains union through the 3a-1 join. `check_narrowed_call` witnesses the
INTERSECTION: fire only when every arm is `knows_toplevel_class` AND
`class_has_method` is false on every arm AND `project_declares_method` is
false on every arm (c6c/f13: presence on ANY arm ⇒ silent); message renders
the arms joined with `" | "` in source order (`for Hash | String`, c6a/c6b).
Worth 1 measured row — recommend building only if the value-type refactor
stays under ~1 day, else record as a standing decline.

### 3b-2 — `while`/`until` body descent (optional follow-up)

Requires an arena discriminator first: `Loop` cannot distinguish `for` (whose
index rebind is invisible — f10 is ref=0, descending is a guaranteed FP) from
`while`/`until` (d21/f9/g3 all ref=1; ref threads the body lexically with no
fixpoint, so straight-line descent is subset-safe). Land an additive
`Loop.is_for: bool` (or `for_binds: Vec<String>`) arena-only and
gate-verified byte-identical first — the `Node::When` split precedent — then
descend `while`/`until` bodies only. Worth doing only if the gap diff after
3b-1 shows loop-body rows.

## Decline set (enumerated, each load-bearing)

- Argument/receiver/return-operand `Logical` minting (c2c, f5 — ref silent).
- `for` bodies (f10 — ref silent; arena-invisible rebind).
- `while` predicate minting (c5, evidence note — ref silent).
- Mixed `when` conditions (g7 — ref fires narrowed-to-subset; declined as
  out of the all-static envelope).
- Ivar-rooted chains (c7b — nameless arena node; gap recorded).
- Chain facts across ANY root mention (c7c/f23 — ref keeps; we kill).
- Post-`BeginRescue`/`Loop`/`case` fact survival (unprobed → cleared).
- `case`/`in` pattern branches, block-boundary crossing, safe-nav dispatch,
  non-Dynamic carriers, shadowed constants: unchanged from stages 1–2.

## FP-safety argument

3b-1 mints NOTHING: every arm either descends to record uses under facts the
stage-1/2 machinery already justified (and the reference is measured to fire
on every recorded form — d/e/f/g tables) or kills facts. Killing is always
safe; recording is safe because a use records only where ref narrows the same
scope (f1–f4 confirm use-recording is position-independent even where minting
is not). 3a-1/2/3 mint strictly fewer facts than the reference's analysers
they port (every rule above is a measured subset; every unprobed axis is in
the decline set), on the same edges, with at-least-as-aggressive
invalidation. 3a-4 fires only on an intersection witness over arms the
reference's union type also carries. Per ADR-0038, any construct outside the
enumerated arms still lands in `other`/`_` and declines.

## Verification (binding)

- Unit tests in `crates/rigor-infer` reproducing the FULL probe matrix
  including every control row (c1g, c2c, f5, f10, f12, c6c/f13, c7d/e/g, g7 as
  declines; e-family survival; d-family recording).
- New fixture `harness/corpus/8N_narrowing_stage3.rb` — N = next free number
  at build time (81/82 taken; the in-flight position-rule branch may take 83).
  Positives: one per built shape family (c1, c2 statement + `or raise`, c4,
  c7, d-op-assign, d-ivar-write, begin-rescue). Negative controls: c2c
  argument-position `&&`, `for`-body use, chain-after-`h.pop`, mixed `when`.
  Oracle-verify every expected line before registering; regenerate
  `harness/snapshots/` via `ruby harness/snapshot.rb`.
- Gates, all green, per slice commit: `cargo build --offline && cargo test
  --offline`, `ruby harness/run.rb` (0 FP), `ruby harness/run_snapshot.rb`,
  `python3 harness/fp_audit.py --gaps --sweep` (**0 FP / 9204 files** —
  release build first), `python3 harness/docs_check.py`, fresh-target clippy.
- **Gap-set diff, not grep** (chain-gap-prediction rule): re-run
  `python3 harness/gap_census.py --sweep --dump <new>` and diff against the
  verified 1168-row baseline (`gaps-v2.json`). Per-shape upper bounds from the
  census (tags overlap; 49 distinct window-candidates):

| slice | bucket bound | named expected closures |
|---|---:|---|
| 3b-1 | ≤ 27 | mastodon `activitypub/case_transform.rb:18` AND `seo/case_transform.rb:28` (`String#underscore`, the archetype) |
| 3a-1 (c1/c4) | ≤ 12 + 10 | — |
| 3a-2 (c2) | (inside the c1/c2 12) | — |
| 3a-3 (c7) | ≤ 6 (1 pure) | — |
| 3a-4 (c6) | ≤ 1 | — |

  Expected direction only: rows leave, ZERO new FP rows, matched
  non-regression. A shortfall vs the window bound is a finding, not a failure.

## Recommended build order

1. **3b-1** — biggest bucket (27), grants no facts, smallest diff (one no-op
   arm + descent arms), each arm independently gateable. Ship first.
2. **3a-1** (compound predicates + both-direction termination) — pure
   predicate-side change, position-independent, covers the c1 (12) and c4
   (10) buckets.
3. **3a-2** (`Logical` statement/LV-RHS minting incl. `or raise`) — small,
   but depends on the in-flight position flag being merged.
4. **3a-3** (chains, local roots) — new fact family, bounded by aggressive
   invalidation; 6-row bound.
5. **3a-4** (unions) — value-type refactor for 1 measured row; build only if
   cheap after 1–4, else record the standing decline.
6. **3b-2** (`while` bodies) — only if the post-3b-1 gap diff shows loop-body
   rows; arena change lands separately, byte-identical, first.

Each step keeps the previous ones' gates green; any FP found mid-slice moves
the offending shape into the decline set rather than patching around it.

## Where the evidence note's reading was wrong (recorded)

- **Mechanism of d1/d2**: not the `other` arm's "no descent" — the recovered
  `Statements` carrier IS descended; the fact dies when the recovered bare
  local READS hit the `other` arm. Consequence: the fix is a no-op
  `LocalVariableRead` arm, not an op-assign model, and `@x ||= use`, `yield
  use`, `super(use)`, `defined?(use)`, `use rescue nil` were ALREADY closed
  on master (d12/d13/d23/d24/g2/g5 measure rs=1).
- **d4's bucket is three node kinds wide**: `VariableWrite` (gvar/cvar) and
  `ConstantWrite` behave identically to `InstanceVariableWrite` (d5/d6/d7).
- **The falsey edge is not categorically unnarrowed** once `!` lands (c4d) —
  the stage-1/2 doc comment's "truthy edge only" invariant must be reworded
  when 3a-1 ships.
