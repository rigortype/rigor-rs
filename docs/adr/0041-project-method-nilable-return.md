# Tier B/C flow, piece A: project-method nilable-return inference

Status: accepted (design) — IMPLEMENTED then DEFERRED by measurement (0 survey
gaps); the code is not shipped, see "Outcome" below.

Opens the "Tier B/C" possible-nil / always-truthy track — the frontier the
valid-mode gap measurement (fp_audit, explicit file lists) identifies as the real
remaining flow cluster, after directory support ([ADR-0040](0040-directory-path-argument-support.md))
corrected the dir-mode measurement artifacts. Classifying the redmine possible-nil
(6) + always-truthy (3) gap sites:

- **ivar value-flow** (`@pre_list.<<`, `if @added`, `if @custom_field_values`) —
  3 gaps, BOTH rules, but the deepest (an ivar's nilability needs whole-class
  writer analysis — any method can rewrite it), so most FP-delicate.
- **project-method nilable-return** (`scm_iconv`, `render_unattached_children_menu`)
  — possible-nil; extends the existing tier-4 in-source return inference.
- **conditional-assignment / local-flow** (`x = v if cond`), **loop / case /
  param** — the remainder.

Order chosen (recommended, maintainer-approved): **A (project-method
nilable-return) → B (ivar value-flow) → …**. A first because it is the most
tractable and FP-safe: it reuses TWO landed systems (the tier-4 return inference
and the [ADR-0038](0038-flow-substrate-incremental-narrowing.md) possible-nil
substrate) and its FP-safety is contained to a SINGLE method body, not a
whole-class ivar analysis.

## Context — what the reference does, verified

The reference fires possible-nil on `x = obj.m(...); x.core_method` when the
project method `m` returns `C | nil`. It infers nilability from the method body —
e.g. `def scm_iconv(...); return if str.nil?; …; nil; end` and
`def render_...; return nil unless …; "".html_safe.tap{…}; end` both have explicit
`nil` returns. Probed clean-vs-muddy (a class method, both implicit-self and
receiver'd call):

- `def m(c); return nil if c; "hello"; end` then `x = m(true); x.upcase` — the
  reference FIRES (`m : String | nil`, `upcase` on the String arm); rigor-rs is
  silent. **The gap A closes.**
- `def conv(s); return if s.nil?; s.encode("utf-8"); end` then `r = conv("a"); r.b`
  — the non-nil arm is param-dependent (`s`), so the arm is not a single concrete
  class; the reference is **also silent** here. So declining a muddy arm MATCHES
  the reference — the conservative scope below is faithful, not merely safe.

## The decision — a conservative, single-core-arm nilable-return

### 1. Inference (a new pass, alongside `infer_method_returns`)

For each harvested class/module instance method, record a **nilable return**
`(class, method) → C` when ALL hold (any failure ⇒ no entry — the decline
backstop):

- **Nil signal**: the body has at least one return point that is `nil` — an
  explicit `return nil`, a bare `return` (Ruby `return` ≡ `return nil`), or a
  `nil`-literal tail.
- **Clean non-nil arm**: EVERY non-nil return point (the other explicit returns +
  the tail, if non-nil) types — under the EMPTY env, exactly as tier-4 — to the
  SAME concrete core class `C` with `knows_class(C)`. A param / ivar / `self` /
  in-source-call / branch-carrier return is `Dynamic` under the empty env ⇒
  decline (this is why `scm_iconv` declines — its `return str` arm is a param).
- **Reopen agreement**: the same `(class, method)` inferred twice with a different
  `C` (or a nilable-vs-non-nilable disagreement) ⇒ remove + blacklist, exactly
  like the tier-4 map. A method is in AT MOST one of the return maps.

This is kept SEPARATE from the non-nilable `method_returns` map: that map is the
`C` (non-nil) return; this one is the `C | nil` return.

### 2. Integration with the ADR-0038 possible-nil source

`nilable_source_class` (rigor-infer) gains a path: `x = <Call to m>` mints
`nenv[x] = C` when `source.nilable_return(K, m) == Some(C)`, where `K` is:

- the **enclosing class** for an implicit-self call (`m(...)` with no receiver) —
  the nil-flow walk threads the enclosing class name (set on `ClassDef`/`ModuleDef`
  descent); OR
- the **project instance class** for a receiver'd call (`obj.m(...)`) — resolved
  from the source registry (`class_name_for_id_of`).

Everything downstream is the landed ADR-0038 machinery: the fact is threaded /
block-descended / decline-widened identically, and `check_nil_receiver` still
gates on `method` ABSENT on `NilClass` and PRESENT on `C`. So a method absent on
the arm (`x.frobnicate`) does NOT fire (it is undefined-method's job / silent on a
union) — matching the reference.

### 3. Gate (ADR-0038 §5 discipline)

`fp_audit.py` 0-FP across the survey (a wrong nilable-return inference surfaces as
an FP here — the critical gate), harness green, matched non-regression, and a
MEASURED possible-nil gap reduction. If the clean pattern proves too rare to close
gaps (the Slice-1b lesson), A stays minimal and B (ivar) is reconsidered — the
measurement decides, not the plan.

## Outcome (2026-07-06) — implemented, FP-safe, but 0 survey gaps ⇒ not shipped

Piece A was fully implemented (parse: `MethodBody.returns_nil` /
`has_nonnil_explicit_return` via a Prism `return`-classifier; infer:
`nilable_returns` map + `infer_one_nilable_return`; the possible-nil source path
threading the enclosing class for implicit-self resolution). It is **FP-safe and
correct** — it fires the confirmed clean synthetic gap (a class method
`def m(c); return nil if c; "hello"; end` used implicit-self OR receiver'd), 437
tests pass, harness 54/54, **0 FP across the survey**. But it closes **0 measured
survey possible-nil gaps** (redmine 6→6, algorithms 49→49, parser 25→25, mastodon
services 4→4, … all unchanged). Classifying WHY the real gaps are untouched:

- redmine `scm_iconv` — the non-nil arm is a **param-dependent** `return str`
  (Dynamic under the empty env) ⇒ the reference's `return_type_heuristic`
  territory, not a clean core arm.
- redmine `render_...` — the arm is an ActiveSupport `SafeBuffer` and the method
  is `.blank?` ⇒ **AS RBS** (plugin) territory.
- parser (25) — `x.adjust` / `x.resize` / `x.begin_pos`: the arm is a **PROJECT
  class** (`SourceRange`/`Node`) and the method is a project method ⇒ needs
  **project-class-arm** possible-nil, a different extension.

So the clean core-arm nilable-return pattern, while real, is **rare** in the
surveyed corpora — the remaining possible-nil gaps are all the deeper clusters.
Per §3's gate ("if the clean pattern proves too rare to close gaps — the Slice-1b
lesson — A stays minimal; the measurement decides"), **piece A is NOT shipped**
(code reverted from master; kept on branch `tier-bc-nilable-return` as a record /
reusable scaffold if the param-dependent extension is later pursued).

**Meta-finding (three consecutive FP-safe-but-0-gap flow slices: Slice 1b, and
now piece A):** the remaining possible-nil / always-truthy gaps have **no cheap
FP-safe wins left** — each residual cluster (param-dependent return typing, AS
RBS, project-class arms, ivar whole-class flow, loop narrowing) is a deep,
per-cluster effort for a handful of gaps. Recorded in
[the flow-frontier note](../notes/20260706-flow-frontier-exhausted.md).

## Considered / deferred

- **ivar value-flow (B)** — higher EV (both rules) but whole-class and most
  FP-delicate; its own later slice, likely its own ADR (ADR-58-class).
- **Param-dependent / heuristic non-nil arms** (the reference's
  `return_type_heuristic`) — deferred; the conservative single-core-arm is a
  strict subset that the reference also declines on, so this loses only recall.
- **AS-method-on-the-arm** (`render_...`'s `.blank?`) — needs ActiveSupport RBS
  (the plugin phase), orthogonal to nilable-return inference.
