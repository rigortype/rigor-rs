# Effects slice 4 — the transitive label lane and the `resolved` bit: a probe

2026-08-26. Investigation only, no production code. Everything measured against
the PINNED submodule at `v0.3.4` (`b10bd5df`), populated into this worktree from
the parent checkout's tree (never the network, never `REFERENCE_RIGOR_DIR`),
invoked as `ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib …`
from the project directory each measurement names, with `.rigor/cache` cleared
either side of every oracle run.

Subject: [ADR-0043](../adr/0043-effect-system-port-parity-model.md) slice 4 —
"transitive propagation over the project call graph, overrides joined", gated on
**0 OVER**. Builds on [the slice-3 probe](20260826-effects-s3-probe.md) and
[its impl note](20260826-effects-s3-impl.md), whose selector-set edge taint this
slice was to replace with the real closure.

**Headline: slice 4 as scoped cannot pass its own gate, and the reason is
upstream's, not the port's.**

- **Upstream's edge set is not "the calls in the body". It is the calls THE
  REFERENCE'S TYPER VISITED.** `push_edge` bails on `record.nil? ||
  record.receiver_class.nil?` (`unit_scan.rb:515`), and `record` exists only
  where `ExpressionTyper#call_type_for` ran (`expression_typer.rb:1141`). A
  syntax-only walk sees strictly more call nodes, so the port's resolved-target
  set is **incomparable** with upstream's — 48 methods with EXTRA resolved edges
  and 534 MISSING ones on mastodon/app. The s3 stand-in was sound because a
  superset only ever adds TAINT; a label join along the same superset is an
  **over-claim** (§ 3).
- **`resolved` is not "the project defines the method".** It is "some dispatch
  tier typed the call", and for a project method that means the callee has
  **requireds-only parameters** and the call's positional arity matches exactly
  (`expression_typer.rb:2388-2392`, `:2427`). A kwarg or splat callee is
  `unresolved-self-call` at every call site **even though the edge resolves and
  the labels propagate** (§ 2, probe `p_arity`).
- **Measured OVERs, four sound-looking rules, three corpora.** Every rule that
  is 0 OVER on the graded corpus and on mastodon/app leaks on gitlab-foss/lib:
  labels + the best blind-spot mirror scores **111 OVER**; bit-only scores
  **29**; bit-only with the strictest resolution rule still scores **5** (§ 5).
- **The whole existing corpus is BLIND to this slice.** An arm that emits **no
  `unresolved-self-call` taint at all** scores *identically* to the tuned arm on
  all seven corpus projects — `MATCH=327 UNDER=237 OVER=0` either way. Slice 4
  cannot be gated by `harness/effects-corpus/` as it stands (§ 6). This is the
  fixture-corpus blind spot, in its purest form yet.
- **rigor-rs's own Typer makes it worse, not better** — for the third time in
  this arc, and now on a new axis. Every OVER above is the port SEEING a call
  the reference did not type. A more complete traversal widens exactly the
  failing direction (§ 4).
- **Recommendation: do not implement slice 4 next.** Two things come first, in
  order: (1) put a real project in the differential's standing set — the corpus
  cannot fail this slice; (2) re-scope, with the numbers in § 5 on the table
  (§ 7).

---

## 1. The propagator, exactly

### 1a. The graph

| | |
|---|---|
| **nodes** | every method key in the run's merged `FileCollection#summaries` — `Class#m` / `Class.m` / `<toplevel>#m`, including `attr_*`- and `define_method`-synthesised units (`scanner.rb:222-228`, `:214-220`) |
| **edges** | `{caller_key => [callee_key]}`, built once by `Propagator.resolve_edges` (`propagator.rb:65-71`) from the per-site `FileCollection::Edge(receiver_class, kind, selector, self_call)` |
| **resolution** | `Index#targets_for` (`:195-207`): the ancestry walk `resolve_owner` finds the nearest definition (class → its `include`s → its superclass, breadth-first, cycle-guarded, `:222-236`), **plus** every project subclass override of the same selector — the closed world of ADR-103 WD4 (`:201-205`) |
| **dropped** | an edge that resolves to no project key is silently dropped, never tainted (`:70`, and the class comment at `:19-20`) |

`Edge#self_call` is recorded and **never read by the propagator** — it only
enters `freeze_edges`' sort key (`file_collection.rb:154`).

Two spellings the port must mirror or lose:

- `superclasses` / `includes` are **as-written candidate lists**, most-qualified
  first (`scanner.rb:246-250`), resolved when the whole project is in view.
- `build_descendants` picks **one** parent per child — the most-qualified
  candidate the project actually defines — because enqueuing every candidate
  would let `A::Base` and `B::Base` share the spelling `Base` and join an
  unrelated override into the proven lane (`propagator.rb:257-262`).

### 1b. The fixpoint

`iterate` (`:96-119`) is a worklist to a least fixpoint: each pass walks
`state.keys.sort`, so the answer never depends on Hash insertion order and a
pooled run agrees with a sequential one bit for bit; `reverse_edges` re-queues
only the callers of a key that moved. **No budget, no cap, no recursion guard** —
and the class comment says why in as many words: "the lattice is finite … and
every step is monotone, so iteration terminates on its own — a recursive or
mutually recursive cycle simply converges, and no recursion cap is needed or
wanted here" (`:21-24`). Measured on `02_propagation`'s `Recursive`: `walk`
(self-loop) and `mutual_a`/`mutual_b` (2-cycle) all converge, and `mutual_a`
acquires `nondet.time` from `mutual_b`.

### 1c. What propagates — all of it, in ONE pass

`absorb` (`:128-145`) moves **five** things along every edge, per visit:

| lane | operation |
|---|---|
| `proven` | `LabelSet#join` |
| `undischarged` | join (the `effects.tolerated:` lane, #385) |
| `declared` | join — **the `≤` lane travels the same edges** (`:38-41`) |
| `exhaustive` | **AND** |
| `causes` | Set union |

So slice 3's "the exhaustive bit is transitive" and slice 4's "the label lane is
transitive" are **the same traversal**, not two. There is no design in which the
port does one and not the other by simply not calling something: they are one
`absorb`. Splitting them is a port-side choice with its own soundness argument
(§ 5).

`declared` joining along the same edges also settles a slice-6 premise early: a
port that lands slice 4 without the declared lane will be a `DECLARED-MISMATCH`
away from correct on any project with an envelope — which is why the shipped
annotation self-defense (`mod.rs:227`) must survive slice 4 unchanged.

### 1d. Every place upstream declines to propagate — enumerated

| # | where | line | consequence |
|---|---|---|---|
| 1 | `push_edge` returns when `record.nil?` **or** `record.receiver_class.nil?` | `unit_scan.rb:515` | **no edge at all** — the big one, § 3 |
| 2 | a CLAIMED catalogue call keeps its edge only when `entry.posture? \|\| implicit` | `:409-411` | a ROW answer on a constant receiver contributes no edge |
| 3 | `visit_reflective_send` with a NON-literal selector | `:474` | `dynamic-send`, no edge |
| 4 | an `opaque_eval?` / `opaque_callable?` site returns before `record_edge` | `:435`, `:450` | no edge |
| 5 | `record_edge`'s edge that resolves to no project key | `propagator.rb:70` | dropped, not tainted |
| 6 | `targets_for` never leaves the project | `:195-207` | gem / core callees contribute nothing |
| 7 | `Propagator.propagate` rescues `StandardError` → `EffectTable.empty` | `propagator.rb:60-61` | whole-run fail-soft |
| 8 | a file whose scan raised | `scanner.rb:194-195` | the unit is `collector-error`-tainted, its edges lost |
| 9 | a `Dynamic` receiver **with** an imported envelope or discharging plugin row | `unit_scan.rb:457` | edge kept, taint suppressed |
| 10 | the whole walk skips a nested `def` / literal `define_method` | `:175-190` | those are units of their own |

Note the pairing of #1: at exactly the sites where upstream drops the edge, a
receiver-less call also **taints unconditionally**, because `record_edge`'s guard
is `self_call && (record.nil? || !record.resolved)` (`:503`). One missing
`record` therefore costs the port *both* lanes at once — it must not add the
edge, and it must add the taint.

---

## 2. The `resolved` bit — what it is, and the verdict

### 2a. What it is

`Collector::CallRecord#resolved` (`collector.rb:38`) is seeded **true** at
`record_call` (`:139`) and retracted only by `record_unresolved`
(`:144-149`), whose single caller is `ExpressionTyper#unresolved_call_result`
(`expression_typer.rb:1202`) — the choke point a call reaches after **every**
dispatch tier declined: `try_local_def_dispatch`, the per-element / inject /
hash-shape block folds, `MethodDispatcher.dispatch` (the RBS tier),
`try_user_method_inference`, `try_project_singleton_inference`. A `Dynamic`
receiver returns one line earlier (`:1184`), so `resolved: false` means
"non-Dynamic receiver, and nothing answered".

It gates exactly one thing: the `unresolved-self-call` taint at a receiver-less
uncatalogued call (`unit_scan.rb:503-511`). It does **not** gate the edge.

### 2b. What decides it for a PROJECT method — and this is the trap

`try_user_method_inference` → `infer_user_method_return` →
`build_user_method_body_scope` (`expression_typer.rb:2388`), which returns nil —
i.e. the tier declines — unless:

```ruby
return nil if def_node.body.nil?                                       # :1805
return nil unless params.nil? || user_method_param_shape_simple?(params)  # :2391
return nil unless required.size == arg_types.size                      # :2392
```

and `user_method_param_shape_simple?` (`:2427-2435`) is

```ruby
params.optionals.empty? && params.rest.nil? && params.keywords.empty? &&
  params.keyword_rest.nil? && params.block.nil?
```

**A keyword, optional, rest or block parameter on the CALLEE makes every
implicit-self call to it `unresolved-self-call`, while the edge still resolves
and the labels still propagate.** Measured, probe `p_arity`:

```
Arity#calls_kw      edge -> ['Arity#kw_callee']   TRANS proven=['io.output.stdout']  ex=FALSE
                    causes=[['unresolved-self-call','kw_callee']]
Arity#calls_splat   edge -> ['Arity#splat_callee'] TRANS proven=['io.fs.read']       ex=FALSE
Arity#calls_wrong_arity  (kw_callee with 0 args)                                     ex=FALSE
```

versus `02_propagation`'s `Pipeline#load_input` → `parse(text)` (one required
positional, one argument), which is `causes=[]`.

This is a **first-iteration binder restriction**, not a semantic fact — upstream
documents it as such (`:2424-2426`, "First iteration accepts only required
positional parameters … Optionals, rest, keyword params, and block params
disqualify the method"). The port would be reproducing a limitation, and the
limitation moves with the pin.

### 2c. Can a TYPER-FREE port produce it? — measured answer

The syntactic half **can** be reproduced: the callee's parameter shape and the
call site's positional-argument count are both pure syntax, and the port already
scans every unit. Reproducing it takes the arc from **6 OVER to 3** on the probe
set and from **3 to 0** on `p_arity` (§ 5a).

The rest **cannot**, and the residual is not a rounding error:

- `resolved` is true whenever ANY earlier tier answered — including the RBS
  tier, whose answer depends on the machine's installed gems and the project's
  `sig/`. The port taints there (UNDER, safe).
- `resolved` is **false** at every site the typer did not visit (§ 3), which no
  syntactic predicate covers.
- Synthesised units resolve **only on the receiver's own class**: `At#reads_slot`
  calling its own `attr_accessor :slot` is `causes=[]`, while
  `EChild#normal_attr` calling `EBase`'s `attr_accessor :text` through the
  superclass is `unresolved-self-call` (probes `p_attr`, `p_endless`) — and even
  the own-class rule leaks, because gitlab's `Gitlab::RepositoryCache#cache_key`
  reads its own `attr_reader :namespace` and upstream still declines (§ 5c).

**Verdict: slice 4 can stay typer-free, and it does not help.** Typer-freedom is
not the binding constraint — 0 OVER is, and the strictest typer-free rule
measured still scores **5 OVER on gitlab-foss/lib**. The minimal thing slice 4
would need is not a typer at all: it is *the reference's own traversal and
binder coverage map*, which is not a thing a port can hold (§ 4).

---

## 3. Slice 3's stand-in vs the real closure

### 3a. The stand-in NEVER under-taints

`Summary::exhaustive` is `causes.is_empty() && edge_selectors.is_disjoint(selectors)`
(`collect.rs:257`). Enumerating every site where upstream contributes taint the
port could miss:

| upstream site | port |
|---|---|
| claimed catalogue call, implicit self ⇒ edge | pushes the selector (`collect.rs:647`) — **same site** |
| claimed call, POSTURE ⇒ edge | posture tier is off (#106) ⇒ the call is uncatalogued ⇒ **explicit receiver ⇒ `dynamic-receiver` taint** |
| reflective `send` with a literal selector ⇒ edge | pushes the selector (`:708`) |
| uncatalogued, receiver-less ⇒ edge | pushes the selector **and** taints (`:697-698`) |
| uncatalogued, explicit receiver ⇒ edge | **`dynamic-receiver` taint** (`:695`) |
| uncatalogued, `self.` receiver ⇒ edge | `receiver().is_some()` is true for a `SelfNode` ⇒ **`dynamic-receiver` taint** |

Every row is a taint the port already has or a superset selector match. The
selector set ignores the receiver class, the kind and the ancestry scope, so it
matches wherever upstream's `targets_for` could and in many places it could not.
**So the stand-in is a pure over-taint: it has no under-taint at all**, and the
hazard slice 4 was chartered against ("a label propagated along an edge upstream
does not resolve") does not come from the stand-in. It comes from the edge set.

Two second-order notes: the stand-in is **one hop, not transitive** (it asks "does
my selector name a project unit", not "is what I reach tainted"), which is again
strictly more taint; and it fires on `Taint#literal_send`, where upstream is
exhaustive — visible today as one of `03_taint`'s two UNDERs.

### 3b. Where the REAL closure under-taints, and why that is the slice's hazard

Replacing the stand-in with the real class-scoped closure makes the port taint
**less** in two directions, and both are OVER risks:

1. **A selector-set hit the real closure would not resolve** (the selector names
   a unit on an unrelated class). Measured: `p_unrelated`'s `Gamma#calls_it`
   calls `only_on_beta`, which `Beta` defines. The stand-in taints; upstream
   taints too, for a different reason (`resolved: false`). Under a closure with
   no `resolved` reconstruction the port would go silent — the `S4_ARM=v0` arm
   scores **OVER** there (§ 5a).
2. **An edge upstream has that the port does not.** 534 on mastodon, 6,240-worth
   of missing labels on gitlab. Each is a callee whose false bit the port never
   ANDs in. Every one is covered today by the blanket `dynamic-receiver` taint,
   which is precisely why slice 3 shipped that taint unconditionally.

### 3c. The inversion, stated once

> The s3 stand-in is sound because **more edges ⇒ more taint**. The label lane is
> unsound for the same reason: **more edges ⇒ more labels**. One edge set cannot
> be conservative for both, and slice 4 needs both along the same `absorb`.

---

## 4. The typer-visit blind spots — the trap

### 4a. Mechanism

`Effects::Collector.record_call` fires from ONE place:
`ExpressionTyper#call_type_for` (`expression_typer.rb:1141`). A call node the
typer never types has no `CallRecord`, so:

- `push_edge` returns immediately (`unit_scan.rb:515`) → **no edge**, and
- `record_edge` taints, because `record.nil?` (`:503`) → **forced taint**.

### 4b. The positions, measured (probes `p_novisit`, `p_visit2`, `p_visit3`)

One class, one leaf method `leaky` proving `global.read`, one implicit-self call
to it per position. "edge" = upstream propagated the label.

| position | edge? | | position | edge? |
|---|---|---|---|---|
| string interpolation `"#{leaky}"` | **yes** | | `return leaky` | **NO** |
| argument position | yes | | `return leaky if c` | **NO** |
| modifier `if` body | yes | | modifier `unless` body | **NO** |
| ternary arm | yes | | block-form `unless` body | **NO** |
| `&&` / `\|\|` operand | yes | | **`elsif` arm** | **NO** |
| `rescue` / `ensure` body | yes | | regexp interpolation `/#{leaky}/` | **NO** |
| non-tail statement | yes | | symbol interpolation `:"#{leaky}"` | **NO** |
| `while` body | yes | | `next leaky` / `break leaky` | **NO** |
| hash value | yes | | receiver of `x[i] ||=` / `x[i] +=` / `x.a ||=` | **NO** |
| `case/in` body | yes | | a statically-false branch (`if false`) | **NO** |
| multiple assignment | yes | | parameter default (`def m(a = leaky)`) | **NO**† |
| simple block `xs.each { leaky }` | yes | | | |

† and no taint either — `UnitScan` only ever walks the **body**
(`unit_scan.rb:138`), so a parameter default is invisible to the effect scan on
both sides. That one is free.

`elsif` arms and `return` expressions are not exotic. This is the reference's
`StatementEvaluator` coverage map, and nothing more.

### 4c. It is not syntactically characterisable

Mirroring the whole table above (arm **D**, § 5b) removes 7 of mastodon's 12
OVERs. The 5 that survive are all `BackupService`, all from one call:

```ruby
account.statuses.…find_in_batches.with_index do |statuses, batch|
  file.write(statuses.map do |status|
    item = serialize_payload(status, serializer)      # <- no record, no edge
```

a self-call inside a block nested in a block whose receiver chain the typer gave
up on — while `xs.each { leaky }` in the same probe **is** typed. The blind set
is **semantic**, keyed on how far the typer got, not on the shape of the node.

Suppressing all block bodies (arm **E**) does reach 0 OVER on mastodon — and
then scores **111 OVER on gitlab-foss/lib** (§ 5c). Three corpora, three
different residuals. This is the "subset arguments need probing" failure mode,
fourth occurrence in this arc.

### 4d. Two smaller traps, both live, both control-armed

- **`<toplevel>`.** Upstream records `receiver_class: "Object"` for a toplevel
  self-call (`p_top`), and the summary key is `<toplevel>#helper`, so the edge
  **resolves to nothing** and the label does not propagate. A port mapping
  implicit self to the unit's own key prefix scores **1 OVER** on that probe
  (`S4_TOP=unit`).
- **`define_method` bodies.** The unit key is `Owner#name` (instance), but the
  typer sees the block's `self` as the CLASS OBJECT, so the edge is
  `Owner.<selector>` — singleton. `p_self`'s `Selfy#from_define_method` calls
  `helper`, `Selfy#helper` exists, and upstream reports
  `causes=[['unresolved-self-call','helper']]` with `effects: []`. A port using
  the unit's own instance/singleton flag scores **2 OVER** there — one label,
  one bit (`S4_DM=instance`).

---

## 5. The measurements

### 5a. Method

The discipline this arc has used four times: drive the **reference's own**
collector and propagator with the typer suppressed (`Accumulator#record` → nil,
so `record` is nil at every site) and a **syntax-only** edge set substituted at
`push_edge`, then grade the result with `effects_diff.compare()` against a live
oracle run of the same project. The simulation therefore uses upstream's real
`Propagator` — its ancestry resolution, closed-world override join and fixpoint
are exact, and only the edge set and the taint rule are the port's.

The port's syntactic edge target, in all arms:

```
receiver is nil or `self`   -> (this unit's owner class, singleton? from the unit)
receiver is a constant path -> (that constant, :singleton)
anything else               -> no edge
```

with `<toplevel>` emitting `Object` (§ 4d) and a `define_method` body emitting
`:singleton` (§ 4d).

Lane identity, so the numbers are the PORT's and not the simulation's: the
simulation's direct proven lane produces **exactly** the shipped binary's
missing-label counts — 26 on `05_posture`, 83 on `07_mutators`, **1,489 on
mastodon/app**, 6,240 on gitlab-foss/lib against the binary's 6,238 + 2
`absent-method`. The `MATCH` deltas below are the bit and label lanes moving,
nothing else.

Arms:

| arm | rule |
|---|---|
| **v0** | real closure, **no** `unresolved-self-call` taint at all |
| **v1** | taint unless the syntactic self-edge resolves to a project unit |
| **v2** | v1 **+** the binder admission test (§ 2b) and own-class-only synthesised units |
| **+BLIND** | v2 **+** mirror the § 4b table (no edge, forced taint) |
| **+block** | +BLIND **+** every block body treated as blind |
| **−labels** | the report prints the DIRECT proven lane; only the bit is transitive |
| **strict** | a synthesised (`attr_*` / `define_method`) unit never counts as resolved |

### 5b. The rule ladder, on the twelve scratch probes (40 methods)

| arm | MATCH | OVER | what fires |
|---|---|---|---|
| v0 | 32 | **6** | `p_arity` ×3, `p_unrelated`, `p_core_self`, `p_self` |
| v1 | 34 | **3** | `p_arity` ×3 — the binder admission test |
| **v2** | **37** | **0** | — |
| v2, `S4_DM=instance` | 36 | **2** | `p_self#from_define_method`, label + bit |
| v2, `S4_TOP=unit` | 37 | **1** | `p_top#caller_of_helper`, label |

### 5c. The three corpora — the decisive table

`harness/effects-corpus` totals exclude `04_declared` (the port's annotation
self-defense reports `methods: {}` there and the simulation does not implement
it; it contributes 4 `absent-method` UNDERs on both sides of every row).

| arm | corpus (564 methods) | mastodon/app (6,948) | gitlab-foss/lib (28,607) |
|---|---|---|---|
| **shipped slice 3** | 320 / **0 OVER** | 5,217 / **0 OVER** | 20,990 / **0 OVER** |
| v2 (labels + bit) | 327 / 0 | 6,122 / **12 OVER** | — |
| v2 + BLIND | 327 / 0 | 6,103 / **5 OVER** | — |
| v2 + BLIND + block | **327 / 0** | **6,030 / 0 OVER** | 23,019 / **111 OVER** |
| v2 + BLIND + block, −labels | 324 / 0 | 5,228 / 0 OVER | 21,103 / **29 OVER** |
| …+ strict | 324 / 0 | 5,227 / 0 OVER | 21,043 / **5 OVER** |

The residual gitlab OVERs are a long tail with no shared shape:
`Gitlab::RepositoryCache#cache_key` (`"#{type}:#{namespace}"`, own-class
`attr_reader`, nested two modules deep), `ClickHouse::Errors::DisabledError#initialize`
(`super(msg || default_message)`), `Gitlab::View::Presenter::Base#can?`
(`super(user, action, overridden_subject || __subject__)`),
`Gitlab::Database::AsyncIndexes::IndexBase#skip_log_message` (interpolation in a
`\`-continued string literal). Each would need its own probe, its own predicate,
and would leak again on the next corpus.

### 5d. What slice 4 would BUY, if it could ship

Against the shipped binary, arm **v2 + BLIND + block** (the best 0-OVER-on-two-corpora arm):

| | corpus | mastodon | gitlab |
|---|---|---|---|
| MATCH | 320 → **327** | 5,217 → **6,030** | 20,990 → 23,019 |
| missing-label | 111 → 110 | 1,489 → **672** | 6,238 → 4,181 |
| extra-taint | 127 → 127 | 242 → 246 | 1,377 → 1,314 |

Per corpus project: `01` 16→16 · `02` **10→14** · `03` **9→10** · `04` 0→0 ·
`05` 61→61 · `06` 5→5 · `07` **219→221**.
The 4 that close in `02` are `Pipeline#run` (labels), `Recursive#mutual_a`
(labels + bit), `Recursive#mutual_b` and `Recursive#walk` (bit) — exactly the
four the s3 probe § 5b classified as slice 4's. `03`'s is `Taint#literal_send`.
The residual `Pipeline#transform` and `Taint#through_a_ghost` need receiver
typing and are slice 6-or-later; `06_edge`'s `Reader#read_it` is the known #106
posture UNDER; `04_declared`'s four are slice 6's.

**The prize is real and it is mostly at scale** — 817 labels on mastodon, ~2,000
methods on gitlab — and it is bought at 111 over-claims.

### 5e. The predicted verdict table, as an acceptance

There is no arm that earns one. The nearest honest statement:

| project | arm v2+BLIND+block | verdict |
|---|---|---|
| `01_core_origins` | 16/16, 0 OVER | PASS |
| `02_propagation` | 14/15, 0 OVER | PASS |
| `03_taint` | 10/11, 0 OVER | PASS |
| `04_declared` | 0/4 (withheld), 0 OVER | PASS |
| `05_posture` | 61/133, 0 OVER | PASS |
| `06_edge` | 5/6, 0 OVER | PASS |
| `07_mutators` | 221/383, 0 OVER | PASS |
| **mastodon/app** | 6,030/6,948, 0 OVER | PASS |
| **gitlab-foss/lib** | 23,019/28,607, **111 OVER** | **FAIL** |

A slice-4 mini-spec that pins the first eight rows and omits the ninth would be
pinning a number the arc's own instrument refutes.

---

## 6. The corpus cannot gate this slice

The single most actionable finding. On all seven corpus projects:

```
S4_ARM=v0  (NO unresolved-self-call taint whatsoever)   MATCH=327 UNDER=237 OVER=0
S4_ARM=v2  (the tuned rule)                             MATCH=327 UNDER=237 OVER=0
```

Identical. `harness/effects-corpus` contains no method whose exhaustiveness
depends on the `resolved` bit at all, and none of the § 4b blind positions.
`06_edge` sees the s3 trap and nothing else. So:

- **Before any slice-4 implementation, `harness/effects_diff.py`'s standing set
  must gain a real project.** mastodon/app and gitlab-foss/lib are already
  members of `harness/sweep-corpora.yml` with the same rationale, and both run
  here in well under the sweep's budget (the mastodon differential is ~10 s).
  The effects differential is the only instrument in the repo that sees
  `.rigor.yml`, project `sig/` and plugins, and it is currently pointed only at
  fixtures the port's authors wrote.
- The twelve scratch probes of § 5b are the second half: they are small,
  deterministic, and each one catches a rule the corpus cannot. `p_arity`,
  `p_self`, `p_top`, `p_unrelated` and `p_endless` are worth promoting as
  `08_resolved` on whatever slice actually lands.

---

## 7. Recommendation

1. **Do not implement slice 4 as specified.** "Transitive propagation over the
   project call graph, overrides joined" at 0 OVER requires an edge set equal to
   upstream's, and upstream's is its typer's traversal, not the program's calls.
2. **Fix the instrument first** (§ 6). This is cheap, independently valuable, and
   it is the only way any later slice-4 attempt can be believed.
3. **If the arc wants the prize, it has to change the parity model, not the
   port.** The lanes ADR-0043 § 2 grades assume the oracle's answer is a
   *semantic* fact about the program. For the proven lane at slice 2/3 that held.
   For the transitive lane it does not: 817 of mastodon's missing labels are
   labels the reference itself declines to propagate for reasons its own docs
   call a first-iteration restriction. An amendment along the lines of "an OVER
   whose sole cause is an upstream traversal gap, demonstrated by a probe, is
   registered rather than fatal" is the same device
   `harness/run_snapshot.rb`'s two registered divergences already are — and it
   is a decision for the orchestrator and the ADR, not for an implementer.
4. **Do not reach for rigor-rs's Typer.** Third refutation in this arc, and the
   cleanest: every OVER measured above is the port seeing a call the reference
   did not type. A more complete traversal makes the failing direction worse.
   The s3 probe § 6b said this about receiver types; it is now also true about
   *which nodes get typed at all*.
5. **If a slice must ship now**, the only 0-OVER-on-three-corpora option
   measured is: keep slice 3's taint rule exactly, keep the selector-set
   stand-in, and land nothing of § 1c. That is a no-op, and saying so is the
   point.

---

## 8. Reproduction

```sh
# populate reference/rigor at b10bd5df from the parent checkout, then:
python3 harness/effects_diff.py --self-test    # all-MATCH on all seven projects
cargo build --offline --release -p rigor-cli
python3 harness/effects_diff.py                # MATCH=320 UNDER=248 OVER=0 DM=0
```

Everything else was produced by three session-scoped scratch drivers, all
removed after measurement:

- `edge_dump.rb` — runs the reference's `Analysis::Runner` with effects on and
  prints, per method, the direct summary, every `FileCollection::Edge` with the
  targets `Propagator::Index#targets_for` resolves it to, and the transitive
  entry. This is what § 1, § 2 and § 4b are read off.
- `s4_sim.rb` — the arms of § 5a. Suppresses the `Accumulator`, forces
  `posture_allowed?` false (#106), replaces `push_edge` with the syntactic
  target, replaces `record_edge`'s taint with the arm's rule, and runs a
  Prism-only pre-pass that builds the project's unit table, as-written ancestry
  and per-callee parameter shape — the same pre-pass a Rust port would need.
- `grade.py` — feeds the simulation's JSON and a live oracle run to
  `effects_diff.compare()`.

Sixteen scratch projects, all `paths: [lib]` (or a symlink to a real tree),
all removed: `p_top` `p_dm` `p_attr` `p_reopen` `p_incl` `p_desc` `p_const`
`p_self` `p_arity` `p_unrelated` `p_core_self` `p_ns` `p_endless` `p_novisit`
`p_visit2` `p_visit3`, plus `p_mast` / `p_gitlab` (a `.rigor.yml` beside a
symlink to `mastodon/app` and `gitlab-foss/lib`).
