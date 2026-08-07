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

#### BUILT (PR — `crates/rigor-infer/src/lib.rs`, fixture
`harness/corpus/83_narrowing_unmodeled_statements.rb`)

Shipped as specced except for **d4-d7, which moved into the decline set** (see
"FP found mid-slice" below). Three build-measured corrections:

1. **The no-op arm is an INERT-LEAF arm, not a `LocalVariableRead` arm.** The
   flat `BeginRescue.body` interleaves the protected statements with each
   clause's lowered exception `ConstantRead`s (`ast.rs:1516`), so f7 dies on a
   constant read and d19's `nil` protected body dies on a `NilLit` unless the
   whole leaf family is inert. The arm covers `LocalVariableRead`,
   `ConstantRead`, `VariableRead`, `SelfExpr` and the six literal variants —
   each provably no-op on BOTH halves of the decline it replaces (a leaf binds
   nothing and, being a leaf, can contain no write to widen). `Range` and
   `Lambda` are deliberately NOT in the set.
2. **`RescueClause.bound_name` must be killed** — newly measured, not in the
   spec's decline set. `rescue StandardError => v` rebinds the narrowed local
   with no `LocalVariableWrite` node, so `collect_flow_writes` cannot see it;
   the reference narrows the bound name to the EXCEPTION class and says
   `for StandardError`. Keeping the `String` fact would have been a live FP.
   Fixture control (11).
3. **The `for` decline is narrower than the spec read it.** f10 is ref=0 only
   when the index REBINDS the narrowed local (`for v in list`); with a distinct
   index (`for i in list`) the reference FIRES. The decline still stands —
   `Node::Loop` cannot tell the two apart — but it costs the whole loop-body
   bucket, not just `for`. Conversely the loop PREDICATE is safe in every case,
   including `for v in v.use`: the collection is evaluated before the rebind and
   the reference fires (probes g1b/g1c/g1d).

#### FP found mid-slice: d4-d7 declined, and a PRE-EXISTING carrier gap

The first full sweep of the slice reported **2 FP candidates**, both on the
ivar/gvar/cvar/constant-write VALUE descent (spec item 2):

- gitlab-foss `lib/ci/inputs/base_input.rb:30` — `spec_hash = spec || {}` then
  `@spec = spec_hash.is_a?(Hash) ? spec_hash.with_indifferent_access : …`;
- gitlab-foss `lib/gitlab/encrypted_configuration.rb:70` — `contents =
  deserialize(read)` (whose body ends `… .presence || {}`) then
  `raise … unless contents.is_a?(Hash)` / `@config =
  contents.deep_symbolize_keys`.

Reduced, both are ONE root cause and it is **not in this slice**:
`narrow_class_other` narrows `Dynamic`/`Top` carriers only, and rigor-rs types
`Node::Logical` as `Dynamic[top]` (`ast.rs:642`) where the reference's
`analyse_or` produces a UNION. So for any local bound from `a || b`, from
`x ||= b`, or from a project method whose inferred return ends in one, the
reference's gate DECLINES and rigor-rs's passes. Deleting the `|| {}` from the
`deserialize` body makes the reference fire (probe `enc_v1`), which pins the
mechanism exactly.

**Master (`a61992f`) emits the same three FPs** on the reduced repro
(`c = x || {}` / `c ||= {}` / a project method returning `unknown || {}`,
each with a bare-statement use) — the standing sweep simply contained no
instance until 3b-1's descent reached two of them at ivar-write RHSs. Per the
spec's own rule ("any FP found mid-slice moves the offending shape into the
decline set rather than patching around it") the VALUE descent was dropped; the
half that ships is fact SURVIVAL past these writes (e1/e6/e7/e8), with a
`kill_cenv_writes` over the statement span so a write nested in the value still
invalidates. Unit rows `fp1`/`fp2` pin the decline so re-enabling d4-d7 without
closing the carrier gap fails loudly.

**Standing finding for the orchestrator**: the carrier-fidelity gap is a live
hazard for every later narrowing slice (3a-1's `&&`/`||` predicates walk the
same ground). Closing it means either typing `Node::Logical` as a union or
excluding `Logical`-derived carriers from the narrowing gate — a stage-1/2
change, not a 3b change. d4-d7 is worth ~1 census row today.

Other declines carried, each measured ref=1 (coverage gaps, never FPs): survival
past a `begin`/`rescue` and past a loop (post1/post2); a BLOCK inside a literal
container or a loop predicate (blk4/blk6 — container elements and a loop
predicate carry EXPRESSION position); a bare literal container in STATEMENT
position (the container arms live in `class_flow_expr` only); a use preceding a
`rescue => e` clause whose bound name shadows the local (bound names are killed
before the descent, not at the clause).

**Gates** (all green): `cargo test --offline` (the new
`class_narrowing_stage3b1_statement_form_matrix` carries 59 rows — every 3b
decision-table row, the e-family survival rows, the nine rows the spec
corrected to ALREADY-CLOSED, and the declines
d4-d7/fp1/fp2/f10a/f10b/d21/g3/post1/post2/rescuebind/c5/c2a/c2c/blk4/blk6 plus
four invalidation rows); `ruby harness/run.rb` 83 fixtures / 0 unregistered
extras; `run_snapshot.rb`; fresh-target clippy; `docs_check.py`; the full
`fp_audit.py --gaps --sweep` **TOTAL FP candidates: 0**, 8 corpora / 9204 files.

**Gap diff** (`gap_census.py --sweep`, 1167 → 1161): **6 rows closed, 0
opened**.

| corpus | row | closed by |
|---|---|---|
| app | mastodon `app/lib/activitypub/case_transform.rb:18` `String#underscore` | inert-leaf arm (NAMED archetype) |
| app | mastodon `app/lib/seo/case_transform.rb:28` `String#underscore` | inert-leaf arm (NAMED archetype) |
| lib | gitlab-foss `lib/gitlab/auth/o_auth/auth_hash.rb:44` `Hash#locality` | `ArrayLit` descent |
| lib | gitlab-foss `lib/gitlab/auth/o_auth/auth_hash.rb:44` `Hash#country` | `ArrayLit` descent |
| lib | gitlab-foss `lib/gitlab/patch/database_config.rb:65` `Hash[…]#deep_stringify_keys` | `BeginRescue` descent |
| lib | gitlab-foss `lib/gitlab/redis/config_generator.rb:79` `Hash[…]#deep_symbolize_keys` | `BeginRescue` descent |

Both spec-named closures landed. A seventh —
`lib/gitlab/sidekiq_middleware/concurrency_limit/middleware.rb:14`
`String#safe_constantize` — closed on the d4-d7 build and reopened with the
decline; it is the measured price of the carrier gap.

The shortfall against the ≤27 bound is a FINDING, not a failure: 27 was an upper
bound over OVERLAPPING census tags, and the residue sits in buckets 3b-1 does
not touch — loop BODIES (3b-2, now known to include `while` as well as `for`),
`Logical` MINTING (3a-2), and rows whose fact is never minted at all without the
3a-1 guard shapes.

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

#### BUILT 2026-08-08 (PR — `crates/rigor-{parse,infer,rules}/src/lib.rs`,
#### fixture `harness/corpus/87_narrowing_3a1_compound_predicates.rb`)

Shipped, with **three spec corrections and one arena change**, all forced by
probes. The arena change is additive and lands in the same commit:
`Node::Logical` gained `is_and: bool` — the lowering collapsed prism's `AndNode`
and `OrNode` into one variant, and `&&` and `||` swap which edge concatenates
and which joins, so the operator is not recoverable otherwise. Every existing
consumer matched with `..`; only the two construction sites changed.

##### Correction 1 — "b wins a same-local collision" is a live FP

`analyse_and` evaluates the right conjunct's truthy scope UNDER the left's, so
`v.is_a?(String) && v.is_a?(Hash)` re-narrows `Nominal[String]` to Hash and
reaches `Bot`: probe `a_same_local_disjoint_then` measures the reference
**silent**, where the spec's rule would have witnessed `for Hash`. The build
applies each edge's guards SEQUENTIALLY against the working fact env, so the
existing review-R3 conflict rule resolves the collision by DROPPING the local.
Same evidence retires the subclass case as a decline: `v.is_a?(Numeric) &&
v.is_a?(Integer)` narrows to the more specific class on the reference (`for
Integer`, either order) and to nothing here.

##### Correction 2 — the `&&` falsey join DOES keep a same-class fact

Probe (b) as written in the task brief (`v.is_a?(String) && v.is_a?(String)`
… `else USE`) is reference-silent, but it does not test the rule: an atomic
class guard contributes an EMPTY falsey map, so that join is empty for the
trivial reason. The rule's real form is `!v.is_a?(String) && !v.is_a?(String)`
… `else USE`, and there the reference **fires** `for String`
(`b2_and_bang_same`). Keep-when-same-class stands. Different classes join to a
real union (`Hash | String`, `b2_and_bang_diff`) — declined until 3a-4.

##### Correction 3 — a compound predicate's OTHER conjuncts can pin a class

The one genuinely new FP surface, and the spec did not have it. `analyse_and`'s
sequencing means any conjunct the reference recognises and we do not can leave
the local at a concrete type, against which our guard class is disjoint. A
54-row battery (`X && guard` and `guard && X` over 27 shapes for `X`, then a
second 40-row pass over the rest of `dispatch_call_simple`) found the exposure
is **exactly three mechanisms**, not the open-ended set the shape of the risk
suggests:

| mechanism | probes | fix |
|---|---|---|
| `local.nil?` on the same local — `analyse_nil_predicate` pins `NilClass` | `L_nilq`, `R_nilq`, `mid_nilq` (all ref=0) | recognise `nil?` as a NON-mintable `NilClass` fact so R3 drops the collision |
| `local == nil` after a guard — `analyse_equality_predicate` meets with `Constant[nil]` | `R_eq_nil` (ref=0); `L_eq_nil` fires, so declining it costs coverage | same fact, on the truthy edge (`!=` swaps) |
| `C === local` beside a different `is_a?(D)` | `L_caseeq`, `R_caseeq` (ref=0) | `===` was already recognised non-mintably; the collision now needs a WITHIN-MAP assertion tracker, because a non-mintable fact writes nothing to the env |

Everything else is inert and measured firing on BOTH engines, and is pinned as
a POSITIVE row so a future over-broad interference rule cannot delete it:
`v.length > 2`, `v.frozen?`, a bare `v`/`w`, `v.respond_to?`, `v.empty?`,
`v.any?`, `v.none?`, `v.key?`, `v >= 2`, `v > 2`, `v.between?`,
`v.start_with?`, `v.match?`, `v =~ /a/`, `v.zero?`, `v.present?`, `v.blank?`,
`v&.foo`, `v[0]`, `v.foo.bar`, `w.include?(v)`, `v == 1`, `v != nil`,
`!v.nil?`, `w.nil?`, `(v = w)`, `String === v` with the SAME class. Note
`!v.nil? && v.is_a?(String)` keeps narrowing: the `!` swap puts the `NilClass`
fact on the edge the `&&` does not concatenate, which is why modelling `nil?`
beat poisoning every local an unrecognised conjunct mentions.

A fourth, arena-level hazard: `/(?<v>a)/ =~ s` BINDS `v` to `String` in the
reference and to nothing here (prism's `MatchWriteNode` has no lowering, so `v`
reads as an unbound untyped local). Probe `matchwrite` is a measured FP. The
binding is arena-invisible, so a compound predicate containing a `=~` whose
RECEIVER is not a bare variable read declines entirely; `v =~ /a/` binds nothing
and still narrows (`matchop_keep`, fires on both).

##### The task brief's five mandatory probes

| # | shape | ref | rs (built) | outcome |
|---|---|:--:|:--:|---|
| a1 | `if v.is_a?(String) && v.is_a?(Hash) … else USE end` | 0 | 0 | falsey join drops on mismatch — as specced |
| a2 | `if v.is_a?(String) && w.is_a?(Hash) … else USE end` | 0 | 0 | atomic guards have empty falsey maps; join over disjoint locals is empty |
| a3 | same-local disjoint `&&`, **truthy** side | **0** | 0 | **correction 1** — the spec rule would have fired |
| b1 | `if v.is_a?(String) && v.is_a?(String) … else USE end` | 0 | 0 | not a test of the rule (empty ∧ empty) |
| b2 | `if !v.is_a?(String) && !v.is_a?(String) … else USE end` | **1** | 1 | **keep-when-same-class CONFIRMED** (correction 2) |
| c1 | `v = [1,2]`; `if !v.is_a?(Hash); v.zzz; end` | 1 | 1 | truthy edge of `!guard` carries nothing — must-still-fire, held |
| c2 | `v = [1,2]`; `return if !v.is_a?(Hash)`; `v.zzz` | 0 | 0 (was **1**) | the falsey map + termination reaches the collapse — a live FP on master, now closed |
| d1 | `v = a \|\| b`; `if !v.is_a?(String) … else USE end` | 1 | 0 | carrier ALLOW-list declines PER LOCAL — coverage cost, as required |
| d2 | same on a PARAMETER | 1 | 1 | the control: the gate is per-local, not per-shape |
| e/c1a | `USE if v.is_a?(String) && v.length > 2` | 1 | 1 | harness reproduces the matrix |
| e/c4d | `if !guard … else USE end` | 1 | 1 | reproduced |
| e/f12 | `if guard \|\| guard \|\| cond` | 0 | 0 | reproduced |

##### Pre-existing FPs this slice closes (each measured on master)

`c_bang_return`, `e3_case_eq_bot`, `p_bang_and_precise`, `k_bot_and_cond`,
`k_bot_or_bot`, `k_bot_or_same`, `p_and_collide_precise` (+ reversed),
`nilq_bot_then`, `nilq_bot_return`, `eqnil_bot_then`, `nilq_bot_and` — eleven
shapes where rigor-rs witnessed a precise carrier the reference had already
collapsed to `Bot`. PR #73 built the collapse; 3a-1 is what reaches it through
`!`, `&&`, `||`, `nil?` and `===`. Their must-still-fire twins
(`must_fire_or_cond`, `must_fire_bang_then`, `must_fire_else_of_and`,
`must_fire_bang_and_else`, `must_fire_nilq_else`) are pinned in the same matrix.

##### Declines added to the set

- `C === local` never MINTS (the reference narrows through it — `e3_case_eq_bang`
  fires — but that is new coverage 3a-1 does not claim).
- `local == nil` on the LEFT of a guard (`L_eq_nil`, ref fires).
- A same-local `&&` collision in a SUBCLASS relation (`s_num_then_int` /
  `s_int_then_num`, ref narrows to the more specific class).
- An `||` of DIFFERENT classes, on either edge (`x_or_diff_class`,
  `b2_and_bang_diff` — a real union, stage 3a-4).
- A compound predicate containing a regex-binding `=~` (`matchwrite_str`, ref
  fires).
- BOTH branches terminating: no propagation at all. `t_both_terminate` measures
  the reference silent, so propagating either map would be a live FP.
- A write to the local inside the conditional's span still declines the
  propagation (`t_write_in_span`, ref fires — carried from stage 1-2).

`class_flow_case` is untouched, as specced.

##### Gates

`cargo build --offline && cargo test --offline` (14 suites green; the 3a-1
matrix carries 78 rows and the Bot-composition matrix 17); `ruby harness/run.rb`
**87 fixtures, 0 unregistered extras** (312/333 matched, 20 gaps, 1 registered);
`ruby harness/run_snapshot.rb`; `python3 harness/docs_check.py`; clippy
`-D warnings` in a fresh `CARGO_TARGET_DIR`; `python3 harness/fp_audit.py --gaps
--sweep` on a freshly built release binary — **TOTAL FP candidates: 0**, 8
corpora / 9204 files.

##### Gap diff — 1 row closed, 0 opened (a SHORTFALL, and why)

`gap_census.py --sweep`, release binary both sides: **1142 → 1141**.

| corpus | row | closed by |
|---|---|---|
| app | mastodon `app/models/content_retention_policy.rb:23` `positive-int#days` | c1a — `value.days if value.is_a?(Integer) && value.positive?` |

Zero rows opened, which is the load-bearing half: the slice adds narrowing AND
suppression, and neither regressed a matched diagnostic.

One row against a ≤22 window is a large shortfall and worth naming precisely.
Re-scanning the 1141 remaining rows for a compound `is_a?`/`kind_of?` within the
same 8-line window that produced the 12+10 estimate leaves **27 candidates**,
and reading them shows the window was measuring the wrong thing — the guard is
present but the gap is a different mechanism:

- Rails/ActiveSupport methods the bundled RBS does not carry (`present?`,
  `presence`, `blank?`, `stringify_keys`, `starts_with?`) — 9 rows, an RBS
  ingestion gap, not a narrowing one;
- `call.possible-nil-receiver` rows — 5, a different rule entirely;
- project monkey-patches the reference applies cross-file and we do not (rdoc,
  Bundler) — 6;
- the carrier ALLOW-list decline from PR #72 (gitlab-foss `auth_hash.rb:44`
  ×2, `normalizer.rb:29`) — 3, already recorded as its measured price;
- the remainder are shapes 3a-1 explicitly declines (a `||` union, a chain
  receiver — 3a-3/3a-4).

Two rows have a concrete, cheap follow-up: gitlab-foss
`sidekiq_config/cron_jobs.rb:58` (`next unless job && overrides.is_a?(Hash)`)
and `api/internal/base.rb:266` both terminate the guard branch with **`next`**,
which `branch_terminates` does not recognise — it accepts only `return` and
`raise`. Extending it to `next`/`break` inside a block body is a small, separate
slice worth ~2 rows; it is NOT folded in here because the block-boundary
semantics of `next` are unprobed.

The honest read: the c1/c4 buckets were an upper bound over an 8-line window
that cannot distinguish "the reference narrowed through a compound guard" from
"a compound guard happens to be nearby", and the real narrowing-limited residue
in these corpora is close to exhausted. The slice's measured value is
overwhelmingly on the FP side — eleven reference-silent shapes rigor-rs was
witnessing — not on the coverage side.

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
- **ADDED BY THE 3b-1 BUILD** — the ivar/gvar/cvar/constant-write VALUE descent
  (d4–d7): the one arm measured to surface the pre-existing `Dynamic`-carrier
  fidelity gap (2 live sweep FPs). Fact SURVIVAL past those writes still ships.
- **ADDED BY THE 3b-1 BUILD** — a `rescue => e` clause's `bound_name` must be
  killed before the `begin` body is descended (the reference narrows the bound
  name to the exception class; carrying the old fact in is a live FP).
- `for` bodies — and, as collateral, `while`/`until` bodies: f10 is ref=0 only
  when the `for` index rebinds the narrowed local, and `Node::Loop` cannot tell
  a `for` from a `while` (arena-invisible rebind).
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

## `next` / `break` termination — BUILT 2026-08-08 (PR — `crates/rigor-{parse,infer}/src/lib.rs` + `ast.rs`, fixture `harness/corpus/89_next_break_termination.rb`)

The 3a-1 build's shortfall analysis named this as a cheap follow-up worth ~2
rows and would not fold it in because "the block-boundary semantics of `next`
are unprobed". They are probed now. **Verdict: GO** — the reference's
`branch_unconditionally_exits?` (`statement_evaluator.rb:2836`) accepts
`Prism::NextNode`/`BreakNode` beside `ReturnNode`, **unconditionally**: no
in-block gate, no loop-body special case, and no re-entry analysis. Every FP
hazard the brief listed is either measured absent on the reference or already
killed by machinery this pass has.

### Probe matrix (pin `v0.3.1`, fresh temp cwd, `--no-cache`, plugin path pinned)

`ref`/`rs` are diagnostic counts on `v.frobnicate_zzz` under the guard;
`rs` is AFTER the build (all rows were `rs=0` on master except the `h*`
harness controls, `p4`, and the reference-silent controls).

| # | shape | ref | rs | outcome |
|---|---|:--:|:--:|---|
| h0 | `return unless G` inside a block (harness control) | 1 | 1 | the matrix reproduces the known-good case |
| h1 | `return unless G` at def top level | 1 | 1 | ditto |
| **p1** | `xs.each { next unless G; USE }` | **1** | **1** | **the archetype — reference narrows past a block `next`** |
| **p2** | same with `break` | **1** | **1** | identical treatment |
| p3 / p3b | guard on the BLOCK PARAMETER, `next` / `break` | 1 | 1 | both carriers narrow |
| p11 | `next if !G` | 1 | 1 | the 3a-1 `!` swap reaches the propagation |
| p17 | `next unless w && G` | 1 | 1 | the real census predicate shape |
| q13 | `next if !G \|\| v.empty?` | 1 | 1 | `\|\|` falsey concatenation |
| r9 / r10 | `kind_of?` / `instance_of?` | 1 | 1 | guard family unchanged |
| q6 | the jump is the branch's LAST statement, after a log call | 1 | 1 | `.last` is the right test |
| q15 / q16 / r2 / q18 | `lambda` / `define_method` / `loop` / `3.times` | 1 | 1 | recognition is syntactic — the block need not iterate |
| r1 | carrier bound from an `@ivar` | 1 | 1 | allow-list member |
| r3 | the guard inside a `begin`/`rescue` in the block | 1 | 1 | — |
| r5 / p7a | the reduced gitlab-foss `cron_jobs.rb:58` row | 1 | 1 | **the census row** |
| q2 | brace-block one-liner | 1 | 1 | — |
| q9 / q22 | use inside the same inner `if` / inner block | 1 | 1 | — |
| **p6 / r11** | **loop-carried rebind AFTER the use** (`next unless G; USE; v = w`) | **1** | **1** | **the 3b-1 hazard class — the reference fires anyway, in a block AND in a `while`** |
| p4 | CONTROL: use BEFORE the guard | 0 | 0 | — |
| p10 | CONTROL: `next if G` (truthy edge terminates) | 0 | 0 | an atomic guard's falsey map is empty |
| q3 | CONTROL: rebind inside the conditional's span | 0 | 0 | the `rewritten` filter |
| q17 | CONTROL: rebind between the guard and the use | 0 | 0 | source-order walk kills it |
| **p9 / p9b / p13 / q10 / r13** | **CONTROL: use AFTER the block / nested block / inner `if` / inner `while`** | **0** | **0** | **`join_cenv` keeps only `Bot`, so a minted fact cannot escape a block — the leak hazard is already closed** |
| r7 | CONTROL: the block is in ARGUMENT position | 0 | 0 | the block position gate |
| q7 | CONTROL: `next` followed by dead code in the branch | 0 | 0 | `.last` is not the jump; ref agrees |
| q1 / q1b | CONTROL: `next`/`break` at def top level | syntax error | 0 | not valid Ruby — no hazard |
| q5 | CONTROL: `v = 1` then `next unless G` (Bot collapse) | 0 | 0 | — |
| r0 / r0b / r0c / q8 | CONTROL: the guard is on an `@ivar`/`$gvar` | 0 | 0 | the reference narrows no ivar at all, anywhere |
| p5 / p5b / p5c / p6b / r12 | DECLINE: `next`/`break` in a `while`/`until` BODY | 1 | 0 | `Node::Loop` bodies are never descended (3b-2) |
| p16 / p16b / q21 | DECLINE: `next 0` / `break 0` (jump WITH a value) | 1 | 0 | keeps the recovered-children carrier; not tagged |
| p15 / p15b / p15c / p15d | DECLINE: `throw` / `fail` / `exit` / `abort` | 1 | 0 | the reference's `EXIT_CALL_NAMES`; only `raise` is ported |
| p8 | DECLINE: `redo` | 1 | 0 | not in the exit set — the reference reaches it via the `Bot`-branch arm of `branch_terminates?` |
| p8b | DECLINE: `retry` | n/a | 0 | the reference emits an unrelated non-rule diagnostic; not trivial, declined per the brief |
| p12 / q19 / q20 | DECLINE: BOTH branches jump | 1 | 0 | `eval_if:495` needs only a PRESENT then-branch, so the reference propagates the truthy map; our `truthy_terminates != falsey_terminates` is the subset rule |
| q11 | DECLINE: a `case`/`when` clause ending in `next` | 1 | 0 | `class_flow_case` has no termination propagation (3a-4) |
| q4 | DECLINE: coarse carrier (`v = a \|\| b`) | 1 | 0 | the PR #72 allow-list, per local |
| r6 | DECLINE: a MUTATOR call between the guard and the use | 1 | 0 | `kill_cenv_narrowed` (carried from stage 1-2) |
| q5b / r4 | DECLINE: `1.frobnicate_zzz` (unrelated carrier gap) | 1 | 0 | pre-existing, orthogonal |

### The arena change (additive, one bit)

`next`/`break` have no owned variant: prism's `NextNode`/`BreakNode` fall
through to the recovery path, and an argument-less one recovers nothing, so it
lands as `Node::Other` — indistinguishable from every other unmodeled leaf.
`Node::Other` gained `jump: Option<JumpKind>`, set ONLY at the two new
interception sites. That is the entire arena diff: `Other` carries no children,
so no child walk, typer arm or rule changes at all (a real owned `Jump` variant
would have to be wired into `child_ids`, the coverage walk and `type_of` for no
measured gain, and dropping its children would be a `flow.dead-assignment` FP
source).

A jump **with an argument** deliberately keeps the recovered-children
`Statements` carrier and stays `jump: None` — its value must remain reachable to
the rule walk. That is the `p16`/`p16b`/`q21` decline.

### The two named census rows — one closes, the other was misattributed

`gap_census.py --sweep --dump`, release binary both sides: **1137 → 1136**.

| corpus | row | outcome |
|---|---|---|
| gitlab-foss | `lib/gitlab/sidekiq_config/cron_jobs.rb:58` `Hash#stringify_keys` | **CLOSED** — `next unless job && overrides.is_a?(Hash)` |
| gitlab-foss | `lib/api/internal/base.rb:266` | NOT this mechanism — it is `call.possible-nil-receiver`, produced by the reference's NIL-flow narrowing, not by class narrowing. `branch_terminates` has exactly one caller (`class_flow_if`), so no `next`/`break` change can reach it. The 3a-1 note's "both terminate the guard branch with `next`" was right about the syntax and wrong about the pass. |

**Zero rows opened**, which is the load-bearing half.

### A PRE-EXISTING FP the slice REACHES but did not create

`return unless v.is_a?(String)` followed by `return unless v.is_a?(Hash)` (two
SEPARATE statements, disjoint classes) is reference-SILENT — its scope carries
`String` into the second guard and collapses to `Bot` — and rigor-rs witnesses
`for Hash`. Measured on the MASTER binary (`s1_two_returns_sequential`,
`s2_two_raise_sequential`), so it predates this slice; the `next` spelling
(`r8_two_guards_sequential`) is a third way in.

Root cause: `class_flow_if` runs `join_cenv` (which retains only `Bot`) BEFORE
the termination propagation, so `apply_guards` sees an EMPTY `cenv` and the
review-R3 conflict rule has no incoming fact to conflict with. The obvious fix —
apply the carried map against the PRE-JOIN snapshot — also newly declines
`s7_two_returns_subclass` (`Numeric` then `Integer`), which the reference FIRES
and rigor-rs currently MATCHES, so a correct fix needs the disjoint-vs-refinement
distinction rather than R3's blanket drop. Out of scope here, and not urgent:
the shape is effectively dead code (the second guard can never pass) and the
standing sweep is **0 FP over 9204 files** with the `next` spelling live.

### Gates

`cargo build --offline && cargo test --offline` (14 suites green; the new
`class_narrowing_next_break_termination_matrix` carries 30 rows);
`ruby harness/run.rb` **89 fixtures, 0 unregistered extras** (326/354 matched,
27 gaps, 1 registered); `ruby harness/snapshot.rb` + `ruby harness/run_snapshot.rb`;
`python3 harness/docs_check.py`; clippy `-D warnings` in a fresh
`CARGO_TARGET_DIR`; `python3 harness/fp_audit.py --gaps --sweep` on a freshly
built release binary — **TOTAL FP candidates: 0**, 8 corpora / 9204 files.

Fixture 89 carries 7 firing rows (each oracle-verified per line) and 5 measured
declines; its controls emit nothing on either engine.
