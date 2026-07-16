# Binding spec — interprocedural literal-tail return folding (always-truthy coverage slice)

Oracle: reference `47ec8625`. Sonnet investigations 2026-07-16 (gap classification
over gitlab-foss/lib 37 non-UM/PN gaps + reference-semantics probe matrix).
Measured target: **Cluster A = 19/28** always-truthy gaps on gitlab-foss `lib`
(cross-file/same-class project-method calls whose return is a literal —
`Gitlab::Database.read_only?` = `false` archetype). Predicted close ≈ 12–14
after confirmed exclusions. mastodon close: 0 (its 2 always-truthy gaps are
Cluster B flow-substrate, the exhausted track).

## Reference mechanism (probe-verified)

- Instance implicit-self: `try_user_method_inference` — ancestor-resolved
  (ADR-24), adoption unconditional post-ADR-57 subject to
  `degrade_if_overridable` (subclass/includer override ⇒ DECLINE, even if the
  values match — probe 16; SAME-owner reopen is NOT an override, last def wins
  — probe 3).
- Cross-file `Module.method`: `try_singleton_method_inference` — requires the
  receiver to type `Type::Singleton` (ADR-57 WD3 seeds project MODULES too);
  resolution is OWN-CLASS ONLY (inherited singleton via subclass constant
  DECLINES — probe 9).
- Fold eligibility is about the method's **whole computed return type joining
  to ONE scalar Constant**, not the tail AST node: raise-guarded tail folds
  (raise = Bot, probe 5); if/else with IDENTICAL literal on every leaf folds
  (probe 18); any disagreeing/Dynamic leaf declines (4/12/18b). Params/arity/
  side effects/caller ivars irrelevant (10/11/14). Endless == regular def (6).
  Singleton vs instance kinds are independent tables (8).
- Boolean combinators fold only when EVERY operand is Constant (13); one
  Dynamic operand kills the whole predicate — the gitlab `can_migrate?`
  &&-chain is NOT closeable (17).
- Collector skip envelope (unchanged, structural): predicate inside
  loop/block; bare-literal predicate; call NAMED `nil? empty? zero? any?
  none? all? respond_to?` (so the `.any?` gap item isn't closeable by typing —
  re-verify that corpus line).
- NOT closeable here (reference declines too or needs other levers):
  `||=`-memoized ivar (20), HashShape return (19 — reference folds via shape
  threading, out of scope), Dynamic-operand chains (17).
- Latent reference gap (probes 22/23): the singleton override gate silently
  fails for classes with only singleton defs — the port's stricter guard below
  is SAFER and never over-fires.

## Port fold conditions (ALL must hold)

1. Call is implicit-self (`receiver: None`) or Const-receiver resolving to a
   project class/module; resolves by `(method_name, kind)` — kind ∈
   {instance, singleton}, separate tables — to **exactly ONE project
   definition across the whole run's SourceIndex** (a NEW inverted
   `name → [owner]` index; strictly more conservative than the reference's
   ancestry-based override gate — sound subset, accepted recall loss on
   unrelated same-name methods, probe 15).
2. The definition's return type JOINS to a single scalar Constant
   (true/false/nil/Int/Float/String/Symbol): tail literal; raise branches
   contribute nothing; if/unless/case with every reachable leaf the IDENTICAL
   literal; anything else (unions, non-literal leaf, shape returns) declines.
3. Depth-1: body contains NO call to another project method.
4. Existing collector skip envelope reused unchanged.
5. Boolean chains: fold only when every operand independently folds.

## rigor-rs attachment points (verified)

- `SourceIndex` IS run-wide (one `build_project` over all files per
  invocation, `analyze_files` in rigor-cli main.rs ~:706; fp_audit passes one
  file list) — NO CLI plumbing needed.
- **Gap: no singleton-method harvest.** `harvest_method_bodies`
  (rigor-parse ast.rs:1588-1607) only collects instance defs; `def self.x`
  (`singleton_name` field) feeds only sig_gen — the cross-file half needs a
  new singleton harvest into SourceIndex.
- **Gap: no `Type::Singleton` for project constants** (lib.rs:263-273 gates on
  `!source.knows_class`), and no type_call tier inspects Singleton. Blast
  radius caution: making project constants type Singleton may affect OTHER
  rules — either gate narrowly (only when the fold applies) or prove
  behavior-neutral via full gates.
- The fold must preserve `Type::Constant` end-to-end: the existing
  `method_returns` tier widens to Nominal, which `constant_polarity` (rules
  lib.rs ~:790) never fires on — do NOT reuse it unmodified.
- Consumers: `always_truthy_snapshots` (infer lib.rs:1285-1298) +
  `check_always_truthy` (rules lib.rs:745-780) — unchanged if the call types
  Constant.
- `MethodBody { name, body, has_explicit_return, params }` — tail =
  body.last(); endless normalized at lowering.

## Gates

Standard: cargo test (+matrix unit tests incl. every DECLINE row above),
clippy, oracle E2E byte-parity probes (incl. probes 1–23 shapes), new harness
fixture + snapshot, run.rb/run_snapshot.rb PASS 0 FP, fp_audit 0 FP on
mastodon app (matched 397 floor) AND gitlab-foss lib AND gitlab-foss
app/models, with the always-truthy gap count on gitlab-foss lib measured
before/after (expect 28 → ~15±2).
