# Conditional-assignment nilability — binding spec (2026-07-11)

> **OUTCOME (2026-07-11): BUILT, CORRECT, FP-SAFE, but closes 0 SURVEY GAPS —
> NOT merged, preserved on branch `flow-cond-assign-nilability` (commit
> `7b7fe3d`).** Opus implemented the full ADR-0038 Slice 2 substrate (`Node::If`
> descend + `join_nil_envs` + conservative predicate-mention narrowing +
> constant-fold parity guard). Main-session audit CONFIRMED: the mechanism is
> byte-identical to the reference on the whole self-probe matrix (`x = "s" if c;
> x.upcase` FIRES at the same location, `unless`/int-RHS/double-conditional fire,
> truthy-`if`/return-guard/safe-nav/NilClass-`to_s` all silent, 7/7 + the agent's
> 13/13); FP-safe on mastodon/app (1236) + gitlab-foss/app (6513) + conference-app
> (98) — **0 FP candidates, matched count UNCHANGED (397/459 mastodon)**; harness
> 54/54; corpus 27 fires / 28 silent. But `fp_audit --gaps`: mastodon possible-nil
> gap 26→26, gitlab 95→95 — **0 closed**. Every one of the 26 mastodon gaps is
> either (a) a `present?`/`blank?` guard-method call (the accepted conservative
> under-emit) or (b) sourced from a PROJECT-METHOD / IVAR nilable return
> (`scope = scope_for(...)`, `@signature.created_time`) — **Tier B/C inference
> rigor-rs lacks, so the local is never minted nilable regardless of the flow
> substrate**. NONE is the unguarded core-typed `x = expr if c; x.foo` this slice
> closes; that pattern does not occur in the surveyed corpora.
>
> **This is the 4th consecutive FP-safe flow slice to close 0 survey gaps**
> ([[possible-nil-fold-gated]] + [flow-frontier note](20260706-flow-frontier-exhausted.md)):
> the possible-nil frontier is Tier B/C (project-method nilable return, ADR-0041 —
> code on branch `tier-bc-nilable-return`; ivar whole-class flow, ADR-58), full
> stop. The If-descent/join substrate here is a genuine, reusable ADR-0038 Slice 2
> prerequisite (future Tier B/C uses in `if`-branches need it), preserved on its
> branch per the ADR-0041 precedent — merge it WHEN a measured gap needs it, not
> speculatively. **Per AGENTS.md "never ship a speculative slice", the discipline
> call is: do not merge a 0-gap slice.** The durable value is this finding + the
> preserved branch.

---

Port `call.possible-nil-receiver` for the **conditional-assignment nilable
local** (`x = expr if cond` ⇒ `x : T | nil`). Investigated via the full protocol:
two independent Sonnet investigations (reference semantics; rigor-rs substrate) +
main-session oracle self-probes. **This is materially ADR-0038 Slice 2** (the
`Node::If` descend + join + truthy-narrowing substrate) with the
conditional-assignment source emerging FROM the join — NOT an isolated "new
source" hook. It carries real FP risk and MUST pass the full ADR-0038 §5 gate.

## The mechanism (both investigations agree)

The nilability is not a "source" on the RHS — it emerges from a **branch-merge
nil-injection** (reference `join_with_nil_injection`, `statement_evaluator.rb`):
at an `if`/`unless`/`case`/loop merge, a local bound in only ONE branch gets
`nil` injected on the OTHER branch's path, then `Scope#join` unions the arms
⇒ `x : T | nil`. So `x = bar if c` ⇒ `x` is `typeof(bar) | nil` (RHS type is
irrelevant to the nil — `x = 5 if c` is still nilable; probe-confirmed).

## The rigor-rs substrate blocker (why this is Slice 2, not a hook)

`nil_flow_stmt` (`crates/rigor-infer/src/lib.rs:~1065-1140`) does NOT descend into
`Node::If` today — `If` falls into the `other` catch-all which just widens
`tenv`/`penv` and **unconditionally `nenv.clear()`s**. So:
- The `LocalVariableWrite` for `x` INSIDE an `If.then_body` is never visited ⇒
  the source can't be minted there by editing `nilable_source_class` alone.
- All calls inside `If` branches are invisible to the possible-nil pass today
  (the accidental blanket-decline safety net).

`x = expr if cond` lowers to `Node::If { predicate: cond, then_body: [assign],
else_body: [] }` (probe-confirmed; the block form and `unless`/`while`/`case`
carry the same shape, `else_body:[]` = "no assignment on this path"). The arena
fully supports detection — but ONLY if the pass descends into `If`.

⇒ The slice REQUIRES a new `Node::If` arm in `nil_flow_stmt` that (1) clones
`(tenv, nenv, penv)` into then/else, (2) recurses `nil_flow_scope` into each
branch, (3) **joins** the two states. There is NO `nenv` join today (only the
tenv-only `join_flow_envs` at `lib.rs:1380`, used by the SEPARATE
`always_truthy` pass whose own `Node::If` arm at `lib.rs:920-941` is the working
descend+join pattern to port over). **Descending into `If` removes the accidental
safety net**, so real truthy-`if x` narrowing becomes MANDATORY in the same slice
(else `x = foo if c; if x then x.upcase end` self-FPs — regresses
`harness/corpus/28_nil_receiver_negatives.rb`).

## FP-safety strategy — narrow AT LEAST as much as the reference (over-narrow)

Zero-FP requires rigor-rs to FIRE ⊆ what the reference fires. The reference
narrows via a specific set; rigor-rs must narrow at least that set. **Design rule:
CONSERVATIVE over-narrowing — whenever `x` appears in ANY condition, guard,
safe-nav, `&&`/`||` operand, or reassignment, CLEAR `x`'s nil fact.** Firing only
on a bare unguarded `x.foo` (method absent on NilClass) is then a guaranteed
subset. This deliberately UNDER-emits the reference's "guard that doesn't actually
narrow" cases (`if x.present?` — the reference still fires inside; rigor-rs will
not) — an accepted coverage gap, FP-safe. (rigor-rs's existing `is_guard` list
already over-narrows on `present?`/`blank?` — keep it; it only costs gaps.)

### What the reference narrows (from the reference investigation + self-probes)
truthy `if x` · `!x.nil?` / `unless x.nil?` · `is_a?`/`kind_of?`/`instance_of?` ·
`case x when C` · `respond_to?(:m)` (only if `m` absent on NilClass) · early
return/raise guard (`return unless x`, `return if x.nil?`, `raise unless x`) ·
`x || return` · `x && x.foo` · `x.nil? || x.foo` · reassign guard
(`x = d if x.nil?`) · `if x&.foo` predicate · elsif chains. rigor-rs must clear
on all of these; the conservative "any mention of x in a guard/condition clears"
covers them by construction.

### What does NOT narrow (rigor-rs may under-emit these — gap, not FP)
`if x.present?` and any unrecognized/project predicate. The mastodon
`linked_account` case IS guarded by `if linked_account.present?`, so the
conservative model will UNDER-emit its inner uses (166-167) and the present? call
itself — an accepted first-slice gap. The gap this slice DOES close: unguarded
conditional-assignment uses (`x = foo if c; x.bar`), the FP-safe core.

## Load-bearing semantics (reproduce for parity / avoid FP)

1. **NilClass-defines-method decline** — the rule does NOT fire when NilClass
   itself defines the method. `.nil?`/`.is_a?`/`.to_s`/`.inspect` never fire even
   unguarded. **rigor-rs's `check_nil_receiver` already gates on "method absent on
   NilClass"** (`crates/rigor-rules/src/lib.rs:~1237`) — this is already correct;
   do NOT re-implement, just confirm it composes with the new source.
2. **Bare LOCAL receiver only** — ivars (`@x`) and chains (`b.inner.foo`) never
   fire this rule. rigor-rs already restricts to `LocalVariableRead`.
3. **Unconditional reassignment CLEARS; a second CONDITIONAL reassignment does
   NOT** — `x = bar if c; x = baz; x.foo` → silent; `x = bar if c; x = baz if d`
   → STILL fires (the `d`-false path carries the stale `T|nil` forward, re-unioned
   at the join). The join must union the pre-branch binding on the untouched path.
4. **`||=` clears nil; `&&=` keeps nilable; `+=`/`-=` degrade the whole binding to
   Dynamic (silent)** — match via the existing `LocalVariableOpWrite` arm
   (verify; the conservative model can also just clear on any op-write, FP-safe).
5. **Ternary** `c ? a : b` (both non-nil) → NOT nilable; `c ? a : nil` → nilable
   (via the nil arm, ordinary union — no special-case needed).
6. **`case`/`when` no-else → nilable; `case`/`in` no-else → nilable too** (a
   reference soundness quirk — real Ruby raises; reproduce for parity, do not
   "fix"). With the conservative model these emerge from the join if
   implemented; a `case` arm can be deferred (only `if`/`unless` in slice 1 —
   see scope).
7. **Top-level constant-fold suppression** — a literal-boolean condition
   (`c = true`) folds the branch "provably live" and suppresses the nilable fact.
   Probe with genuinely-unknown-typed conditions.
8. **Narrowed-to-pure-nil flips rule** — the else-branch of `unless x` types `x`
   as pure `NilClass` (not a Union), which the reference reports as
   `call.undefined-method "for nil"`, a DIFFERENT rule. To stay FP-safe rigor-rs
   should NOT emit possible-nil there; simplest is to clear/skip the else-of-`if x`
   branch conservatively (don't fire in it). Do not attempt the rule-flip in this
   slice.

## Scope decision (slice 1 = `if`/`unless` only, conservative)

- Implement the `Node::If` (both `is_unless` polarities) descend + `nenv`/`penv`
  join in `nil_flow_stmt`. `case`/loop `If`-shaped merges: DEFER (leave in the
  `other` clear-arm — FP-safe under-emit) unless trivially free from the same
  join; state the deferral.
- Mint the nilable fact from the join: a local bound-and-typed to a `knows_class`
  nominal in exactly one branch (absent/nil on the other) → `nenv[name] = class`
  in the continuation; a local nilable in either branch stays nilable; a local
  bound consistently in both → its joined type (non-nil).
- Conservative narrowing: clear `x`'s fact whenever `x` appears in the `If`
  predicate or any guard/safe-nav/op-write/`&&`/`||`; keep the existing
  `nil_flow_expr` narrowing (safe-nav, `is_guard` list, `Logical` clear,
  reassignment).
- Fire only on a bare statement-level `x.foo` with the fact live (the existing
  `check_nil_receiver` NilClass gate does the rest).

## Gate (ADR-0038 §5 — ALL mandatory, this is FP-risky substrate)

1. `cargo test --workspace` green; `cargo clippy` clean on touched crates.
2. **`harness/run.rb` + `run_snapshot.rb` 54/54, 0 unregistered FP.**
3. **`harness/fp_audit.py` 0 FP candidates across the FULL survey corpora**
   (mastodon app + the algorithm/library set — the reference-only-is-fine but
   rigor-rs-only-is-a-FP bar). This is the load-bearing FP gate — the If-descent
   blast radius (all in-branch calls now visible) is exactly where a regression
   hides.
4. **Non-regression on `harness/corpus/27_nil_receiver_fires.rb` (still fires) +
   `28_nil_receiver_negatives.rb` (still silent — the `guard_if_predicate`
   fixture is the self-FP tripwire).**
5. **Measured gap reduction**: `fp_audit.py --gaps` on mastodon/app — the
   `call.possible-nil-receiver` gap count MUST drop (from 26) by the unguarded
   conditional-assignment cases, with 0 new FP. Report the before/after.
6. Oracle E2E fresh-dir on the self-probe matrix (cond_assign fires, uncond
   silent, narrow_if/return/nil?/reassign/safenav silent, unless_form fires,
   int_rhs fires) — byte-identical rule set to the reference.

## Delegation

Opus implementer on branch `flow-cond-assign-nilability`, prompt = this note +
the two investigation reports' key facts. Pitfalls to name: (a) the `nenv` join
does not exist — port the `flow_eval_stmt` If arm's tenv-join pattern to nenv;
(b) truthy-`if x` narrowing is MANDATORY in the same slice (self-FP tripwire);
(c) the second-conditional-reassignment stale-nil re-union; (d) NilClass-decline
already exists — don't duplicate; (e) `.present?` does NOT narrow in the
reference but rigor-rs conservatively clears on it (accepted gap); (f) cache
pollution + top-level constant-fold suppression when probing. Main session
audits with independent fp_audit + the self-probe matrix before merge.
