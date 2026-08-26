# Effects slice 3 — the taint bit and its causes: a probe

2026-08-26. Investigation only, no production code. Everything measured against
the PINNED submodule at `v0.3.4` (`b10bd5df`), populated into this worktree from
the main checkout's tree, invoked as
`ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib …`
from the project directory each measurement names, with `.rigor/cache` cleared
either side of every run.

Subject: [ADR-0043](../adr/0043-effect-system-port-parity-model.md) slice 3 —
"the taint bit and its causes (unresolved / dynamic receivers)", gated on
"0 OVER; taint at least as strict as the oracle's". Builds on
[the slice-2 probe](20260826-effects-s2-probe.md) and
[its impl note](20260826-effects-s2-impl.md).

Headline results:

- **Yes, a typer-free collector can prove exhaustiveness — but only under THREE
  rules, and the naive one is a disaster.** Emitting slice 2's direct taint bit
  as `exhaustive` scores **10 OVER on the 46-method corpus and 986 OVER on
  mastodon/app** (14% of its methods). The rule that works ("T2", § 4) scores
  **0 OVER on both, plus on two new adversarial probes**.
- **The prize is 27 methods, not 30.** The measured baseline is
  `MATCH=12 UNDER=34 (extra-taint 27, missing-label 3, absent-method 4)`; the
  four `04_declared` UNDERs are `absent-method`, withheld by slice 2's
  annotation self-defense, and belong to slice 6. Of the 27, **23 close
  typer-free**; 4 do not (§ 5).
- **`exhaustive` in the graded JSON is the TRANSITIVE bit, not the direct one**
  (`effects_report.rb:41` → `effect_table.rb:32-46` → `propagator.rb:138-141`).
  This is slice 3's central trap and it is not theoretical: a catalogue-CLAIMED
  call still keeps a project edge (`unit_scan.rb:409`), so a body whose every
  call the catalogue answers can still be transitively tainted (§ 3b, probe
  `p_edge`). The cheap sound stand-in is a **selector-set edge taint**, which
  costs **nothing** on the graded corpus.
- **NEW TRAP, and it is a LIVE OVER on the SHIPPED slice-2 binary.**
  `posture_allowed?` reads the typer's `dynamic` bit, and **8 of the
  catalogue's 80 classes name constants the reference's typer cannot resolve** —
  `Net::HTTP`, `Net::SMTP`, `Net::FTP`, `OpenSSL::SSL::SSLSocket`,
  `Fiddle::Handle`, `Fiddle::Function`, `PTY`, `SOCKSSocket`. Measured today:
  **7 proven-lane OVERs** on `harness/effects_diff.py` (§ 3c). Slice 2's
  "postures stay ON for constant-path receivers, probe-verified safe" is false;
  the s2 probe verified it on `File` and `TCPSocket` only.
- **Consuming rigor-rs's Typer does NOT make the bit safe.** The port's known
  precision advantage is exactly the unsafe direction — `sig_gen.rs:20-23`
  records that rigor-rs "is more ROBUST on shapes the reference degrades to
  `untyped`", and `untyped == Dynamic`. A receiver rigor-rs types nominally and
  the reference types `Dynamic` is an over-claim, not a win (§ 6).
- **Predicted post-slice-3 verdict:
  `MATCH=35 / UNDER=11 / OVER=0 / DM=0` → PASS**, per corpus
  01: **16**/0/0/0 · 02: **10**/5/0/0 · 03: **9**/2/0/0 · 04: 0/4/0/0 —
  derived with `effects_diff.compare()` itself, and pinned to the REAL port
  because the shipped binary's proven lane is byte-identical to the simulation's
  on all three graded corpora (§ 5c).

---

## 1. Upstream's exhaustiveness rule, exactly

### 1a. Three levels, and the JSON grades the third

| level | rule | where |
|---|---|---|
| **direct**, one unit | `exhaustive: @causes.empty?` — the bit is *defined* as "the walk of this body produced no taint" | `unit_scan.rb:138-145` |
| **joined**, reopenings + the several files contributing to one key | `exhaustive: @exhaustive && other.exhaustive?`, causes union | `summary.rb:89-98` |
| **transitive**, what `rigor effects --format=json` prints | `absorb` ANDs the bit along every RESOLVED project edge and unions the causes, to a fixpoint | `propagator.rb:128-145`, seeded at `:77-86` |

`EffectsReport.row_for` reads `entry.exhaustive?` — the `EffectTable::Entry`
field the propagator wrote (`effects_report.rb:35-44`), and
`effect_table.rb:18-20` says so in as many words: "`proven` / `exhaustive` /
`causes` are the transitive readings". `direct` is the separate key the snapshot
records, and it carries **only bundles** — the direct taint bit is not in the
JSON at all.

**So `exhaustive_ref(m)` = (m's own walk produced no taint) AND (no project
method m reaches produced one).** A slice-3 port that computes the first
conjunct and emits it is over-claiming by construction. Measured cost of doing
so: § 3a.

The invariant `causes.empty? == exhaustive` holds at every level, with one
escape: `Summary#normalize_causes` **drops** a cause outside `TaintCause::ALL`
(`summary.rb:141-151`), so a producer emitting an out-of-enum marker yields
`exhaustive: false` with `causes: []`. Slice 2 does exactly that on purpose
(`port-incomplete`); slice 3 should stop (§ 7).

### 1b. The taint producers — verified and refined at the pin

Grepping `taint(` across `reference/rigor/lib` returns eleven call sites in one
file, plus `Summary.tainted` at `scanner.rb:195`. **`method-missing` and
`budget` have no producer at the pin** — confirmed, the s2 probe's reading
stands. `template-not-analysed` and the plugin `opaque-callable` come only from
a plugin row's `taint:` (`unit_scan.rb:261`; `rigor-actionpack`'s `render`,
`rigor-activesupport-core-ext`'s `instrument`).

| # | cause | trigger, precisely | line | typer-free? |
|---|---|---|---|---|
| 1 | `dynamic-send` | `send` / `public_send` / `__send__` whose first argument is **not** a `SymbolNode` or `StringNode`. A literal one is an ordinary edge and must NOT taint | `:472-477` | **EXACT** — pure syntax |
| 2 | `opaque-callable` (a) | an `eval` / `instance_eval` / `class_eval` / `module_eval` call with **≥1 POSITIONAL** argument (`KeywordHashNode`s are `grep_v`'d, `:567-569`), or a bare receiver-less argument-less `binding` | `:435`, `:543-548` | **EXACT** — pure syntax |
| 3 | `opaque-callable` (b) | `&expr` where `expr` is neither a `SymbolNode` nor a read of the unit's own `&blk` parameter | `:522-531` | **EXACT** — needs the `&blk` NAME, which slice 2 deliberately dropped |
| 4 | `opaque-callable` (c) | `.call` on a receiver that is not a `LambdaNode` and not the unit's `&blk`, **and** `record.nil? \|\| record.receiver_class.nil? \|\| receiver_class ∈ {Proc, Method}` | `:450`, `:486-493` | **SOUND, over-taints** — `record.nil?` is always true typer-free |
| 5 | `unknown-ownership` | a mutation whose `MutationClassifier#label_for(receiver)` is nil. `label_for` is **pure syntax** (`mutation_classifier.rb:69-89`): nil ⟺ the receiver is not `nil`/`self`/an ivar read/a cvar read/a parameter local/a frame-owned local | `:533-538` | **SPLIT** — EXACT for the six compound-write node types (`visit` calls `classify_mutation` unconditionally, `:214-216`) and for the catalogued path (`mutating_catalogued?` passes the SYNTAX-derived `owner`, `:413-415`); typer-dependent on the uncatalogued path, where `mutating?(node, record&.receiver_class)` gates it (`:445`) |
| 6 | `dynamic-receiver` | `record&.dynamic` at an uncatalogued call with no bound. `detail` is the `Inference::DynamicOrigin` name | `:458` | **NOT DECIDABLE** — the typer's verdict, and the port has no analogue (§ 6) |
| 7 | `unresolved-self-call` | a receiver-less call the dispatcher declined (`record.nil? \|\| !record.resolved`) with no bound; `detail` is the selector | `:500-512` | **NOT DECIDABLE** — the `resolved` bit is the typer's own "nothing here resolved" |
| 8 | `plugin-attribution` | the project's `effects.attribution:` table (`:371`), and any NON-discharging plugin row (`:262`) | `:262`, `:371` | out of ADR scope; see § 3d |
| 9 | `template-not-analysed` | a plugin row's `taint:` | `:261` | out of scope |
| 10 | `collector-error` | a unit whose scan RAISED — recorded as a tainted summary, and the method is still REPORTED | `scanner.rb:194-195` | port is per-FILE fail-soft, so it omits instead: UNDER, safe |

Two suppressors that make upstream MORE exhaustive, both measured in § 3e:
a call the catalogue **claims** never reaches producers 1-4/6/7 at all
(`:231`), and an imported **envelope** (or a discharging plugin row) short-circuits
6 and 7 (`:457`, `:509`).

---

## 2. The typer-free question, answered

For producers 6 and 7 — and for the typer-gated half of 5 — a typer-free port
cannot decide. Soundness (`exhaustive_rs ⇒ exhaustive_ref`) requires it to taint
**at least as often**, so the only available answer is to taint whenever it
cannot rule the producer out:

> **T0.** Every call that reaches the uncatalogued path taints: with an EXPLICIT
> receiver as `dynamic-receiver` (the port cannot prove the typer did not say
> `Dynamic`), receiver-less as `unresolved-self-call` (it cannot prove the
> dispatcher resolved). Producers 1-3 stay exact; 4 over-taints for free; 5 is
> exact where it is syntax-only and subsumed by the blanket rule elsewhere.

Under T0, **a method is exhaustive iff every call in its body is claimed by the
catalogue through a SYNTAX-settled target** — a constant-path receiver, or
implicit self matching a `Kernel` row — and no compound write has unprovable
ownership. That is a real, non-empty class: it is 23 of the corpus's 27
extra-taint methods and 702 of mastodon/app's 945.

T0 is necessary and **not sufficient**. Two further rules are (§ 3b, § 3c).

---

## 3. The traps, each measured

Method: the reference's OWN `UnitScan` was driven with the per-file `CallRecord`
table suppressed (`Collector::Accumulator#record` → no-op), so `record` is nil at
every site — precisely what a typer-free collector sees. That makes the arms
apples-to-apples: `catalog_target` keeps only its constant-path and
implicit-self arms, `posture_allowed?` reads `!record&.dynamic` as true,
`mutating?` sees no receiver class, `record_edge` sees `record.nil?`, and no
edge is ever pushed. Each rule below is then a further patch. Reproduction: § 8.

### 3a. NAIVE — emitting the direct taint bit is a 10 / 986 OVER failure

```
B. NAIVE slice 3 — typer-free collector, DIRECT taint bit emitted as `exhaustive`
  01_core_origins  MATCH=13 UNDER=0 OVER=3     mutates_its_argument, owns_what_it_mutates, pure_arithmetic
  02_propagation   MATCH= 8 UNDER=4 OVER=3     each_with_effect, dispatch, parse
  03_taint         MATCH= 6 UNDER=1 OVER=4     dynamic_receiver, fully_resolved, opaque_callable,
                                               unknown_constant_receiver
  TOTAL  MATCH=27 UNDER=9 OVER=10  => FAIL

  mastodon/app     MATCH=4501 UNDER=1500 OVER=986  (all 986 "claims exhaustiveness the oracle does not")
```

Every one of the ten is a producer the port cannot decide: `dynamic-receiver`
(7), `unknown-ownership` (1 — `Origins#owns_what_it_mutates`, where
`buffer << 1` is a mutation only because the typer named `Array`), and the two
`Deferred#each_with_effect` / `Dispatcher#dispatch` untyped receivers.

### 3b. TRAP — `exhaustive` is TRANSITIVE, and a CLAIMED call still keeps an edge

`keeps_project_edge?(entry, implicit)` is `entry.posture? || implicit`
(`unit_scan.rb:409-411`), and its own comment names the measured Redmine case:
"`Kernel#format` is a real row and `CustomField#format` is a real method, and
only the union reads both correctly". So a call the catalogue CLAIMS — no taint
at the site — still contributes an edge whose callee's taint ANDs in.

Probe `p_edge` (three shapes, oracle measured):

| method | the call | direct bit | TRANSITIVE bit |
|---|---|---|---|
| `Shadow#calls_shadowed` | `format("a")` → the `Kernel#format` row, implicit ⇒ edge to the project's own `Shadow#format` | **true** | **false** |
| `Shadow#calls_shadowed_caller` | `caller` → the `Kernel#caller` row ⇒ edge to `Shadow#caller` | **true** | **false** |
| `Reader#read_it` | `File.slurp("x")` → File's **posture** ⇒ edge to the project's `File.slurp` | **true** | **false** |

T0 alone scores **3 OVER** here. The fix is cheap and needs no propagator:

> **T1 = T0 +** taint whenever a call could contribute a project edge and the
> selector names a project unit. `push_edge` (`:514-520`) is the single funnel
> all three edge sources go through — the claimed path, a literal-selector
> reflective `send` (`:476`), and the uncatalogued path — so the taint goes
> there, conditioned on the selector being in the project's own selector set.

Soundness: `Propagator::Index#targets_for` (`propagator.rb:195-207`) can only
resolve to keys `Class#selector` / `Class.selector` that the collection holds, so
{real targets} ⊆ {units whose selector matches}; ignoring `kind` and the ancestry
scope makes the port's set a superset. **Cost on the graded corpus: zero** — no
catalogue-claimed selector in 01/02/03 (`puts`, `rand`, `proc`, `raise`,
`Time.now`, `File.read`, `File.write`, `ENV#[]`, `Time.new`) is also a project
method name.

### 3c. NEW TRAP — the posture tier is a LIVE OVER on the shipped binary

`posture_allowed?` is `!implicit && !record&.dynamic && !DEFERRED_SELECTORS…`
(`:429-431`). A typer-free port has no `record`, so `!record&.dynamic` is `true`
and the class default answers. Upstream refuses it whenever the typer said
`Dynamic` — **including for a constant-path receiver whose CONSTANT the
reference cannot resolve**.

Probe `p_posture` — one method per catalogued class, calling an uncatalogued
selector so only the posture can answer. Oracle result: **8 of the 80 catalogued
classes taint**, every one `["dynamic-receiver", "unsupported_syntax"]` with
`effects: []`:

```
Fiddle::Function  Fiddle::Handle  Net::FTP  Net::HTTP  Net::SMTP
OpenSSL::SSL::SSLSocket  PTY  SOCKSSocket
```

Against the **shipped `target/release/rigor`**, `harness/effects_diff.py` reports:

```
=== …/p_posture ===
  oracle=80 methods / 26 proven labels   rigor-rs=80 / 33
  MATCH=1  UNDER=72  OVER=7  DECLARED-MISMATCH=0
    OVER: Posture#c_net__http — proven labels not proven by the oracle: ['io.net.http']
    OVER: Posture#c_net__ftp / c_net__smtp / c_openssl__ssl__sslsocket — ['io.net']
    OVER: Posture#c_fiddle__handle — ['ffi']
    OVER: Posture#c_pty — ['io.process']
    OVER: Posture#c_sockssocket — ['io.net']
  RESULT: FAIL
```

`Fiddle::Function` does not appear only because its posture is `value` (∅) —
it is the eighth, and it becomes an exhaustiveness OVER the moment slice 3 turns
the bit on. **T1 alone leaves 15 OVER on this probe** (7 label + 8 bit).

The fix is one line and provably safe:

> **T2 = T1 +** drop the POSTURE tier for constant-path receivers. `Catalog#lookup`
> answers a **ROW** first and a **UNIVERSAL** selector second, both *before* it
> consults `posture:` (`catalog.rb:186-190`), so `posture: false` loses only the
> class default and never a row. That is exactly what upstream itself does when
> the receiver is `Dynamic`.

Cost, measured: **zero on the graded corpus** (no posture answers occur there —
§ 5c), and **4 methods of 6,948 on mastodon/app** (5238 → 5234 MATCH). An
independent origin census over mastodon/app's 152 catalogue origins puts the
posture tier at 9 of them (`Resolv::DNS` 3, `FileUtils` 2, `Socket` 2, `Resolv`
1, `File` 1).

Two rejected alternatives:

- *An allow-list of resolvable classes.* The set is a function of the
  reference's RBS environment, which moves with the pin AND with the machine's
  installed gems — the same hazard `UNBUILDABLE_DEFINITIONS` exists for (an
  installed gem's `sig/` can silence a whole class in the reference, and the
  entry is gem-keyed, not pin-keyed). If the label loss ever matters, derive the
  set in `harness/vendor_effects.py` with a `--check` drift gate; do not
  hand-list it.
- *Ask rigor-rs's Typer whether the constant resolves.* § 6 — the wrong
  direction.

### 3d. What upstream's suppressors mean for the port

**Envelopes make upstream MORE exhaustive** — the safe direction. Probe
`p_env`, oracle measured:

```
Bounded#calls_ghost_bare          ex=false  causes=[["unresolved-self-call","ghost_bare"]]
Bounded#calls_ghost_with_envelope ex=true   declared=["io.db"]   causes=[]
```

`record_edge` returns before its taint when a bound is present (`:509`), and
`visit_uncatalogued` skips `dynamic-receiver` for a bounded Dynamic receiver
(`:457`). A port that ignores envelopes therefore taints more. Safe.

*(Side finding for slice 6 / ADR-0043's "Open at accepted": `calls_ghost_with_envelope`
is **not itself annotated** and still reports `declared: ["io.db"]`. That settles
the fixture's open question — the declared lane is the CALLER's import of the
callee's envelope, never the method's own annotation. `04_declared`'s `formats`
and `load_row` report `[]` because nothing calls them; `load_and_log` reports
`io.db` because it calls `load_row`.)*

**The project's `effects.attribution:` table and PLUGIN rows make upstream LESS
exhaustive** — the unsafe direction (`:262`, `:371`). Slice 2's self-defense
already withholds a project carrying `effects.attribution:`
(`crates/rigor-cli/src/effects/mod.rs:203-236`); it does **not** withhold a
project with plugins, and plugins additionally synthesise framework UNITS
(`scanner.rb:163-173`) that widen the selector set T1 depends on. Under slice 2
this cost nothing, because the bit was always false. **Slice 3 must extend the
self-defense to any project that loads an effect-contributing plugin.**

### 3e. Smaller findings

- **`Taint#fully_resolved`'s fixture comment is wrong.** It says "the control:
  fully resolved, no taint"; the oracle reports `exhaustive: false`,
  `causes: [["dynamic-receiver","inferred_return_untyped"]]` —
  `a.to_s` is claimed by the `universal:` list, and `.length` on its
  inferred-untyped result is not. Same class of error as the two comments slice 2
  fixed; worth the same treatment.
- **`trivial?` becomes reachable.** With `exhaustive` always false the port's
  `Row::trivial()` is dead (`mod.rs:275-283`). Turning the bit on makes the
  DEFAULT report start omitting rows. The grader always passes `--full`, so this
  is not a slice-3 gate risk — but `rendered_declared` is part of upstream's
  `trivial?` (`effect_table.rb:52-54`) and the port's declared lane is always ∅,
  so the two will disagree on any row upstream keeps for a surviving declared
  label. A slice-5 snapshot landmine, recorded here.
- **The `&blk` parameter name must come back.** Slice 2 dropped it because only
  producers 3 and 4 read it (s2 impl note § 7). Without it, `foo(&blk)` and
  `blk.call` taint where upstream does not — UNDER, not fatal, but it is
  coverage this slice is spending.

---

## 4. The rule slice 3 should implement, in one place

**T2**, on top of the shipped slice-2 collector:

1. **Drop the posture tier** for constant-path receivers; keep ROW and
   UNIVERSAL answers. (§ 3c — also fixes a live slice-2 OVER.)
2. **Blanket conservative taint** on the uncatalogued path: explicit receiver ⇒
   `dynamic-receiver`, receiver-less ⇒ `unresolved-self-call`. (§ 2)
3. **Edge taint**: when a claimed call keeps a project edge (implicit self, or —
   after rule 1 — nothing else), or a reflective `send` carries a LITERAL
   selector, taint if that selector names any unit the run collected. (§ 3b)
4. **Exact producers**, implemented as upstream spells them: `dynamic-send`
   (non-literal selector), `opaque-callable` (eval family with ≥1 positional
   arg / bare `binding` / `&expr` not a symbol and not the unit's `&blk` / `.call`
   on a non-lambda non-`&blk` receiver), `unknown-ownership` (`label_for` nil at
   the six compound-write node types and on the claimed `mutates: receiver`
   path).
5. **Restore the `&blk` parameter name** to the unit scan.
6. **Extend the annotation self-defense** to plugin-bearing projects.

Rule 3 is the one that reads oddly and it is the load-bearing one: it is the
**stand-in for the transitive AND**, priced at a syntactic selector-set test
instead of the class graph, edge resolution and fixpoint that slice 4 owns.
When slice 4 lands, rule 3 is replaced by the real closure and the taint it
manufactures goes away.

---

## 5. The 27, classified — and the predicted verdict table

### 5a. Baseline, measured today

```
harness/effects_diff.py
  01_core_origins  MATCH= 3 UNDER=13 {extra-taint 13}
  02_propagation   MATCH= 4 UNDER=11 {extra-taint 9, missing-label 2}
  03_taint         MATCH= 5 UNDER= 6 {extra-taint 5, missing-label 1}
  04_declared      MATCH= 0 UNDER= 4 {absent-method 4}
  TOTAL  MATCH=12 UNDER=34 OVER=0 DM=0  => PASS
```

**27 extra-taint**, not 30: the slice-2 probe's § 6c figure counted the whole
46-method corpus, and `04_declared`'s four are withheld by the self-defense as
`absent-method`. They close at slice 6, not slice 3.

### 5b. The classification

| # | class | methods | what they are |
|---|---|---|---|
| **23** | **(a) exhaustive provable typer-free** | 01: all 13 (`write_stdout` `read_clock` `read_file` `write_file` `read_env` `random_number` `fixed_time` `ivar_write` `ivar_memo` `gvar_read` `gvar_write` `cvar_write` `subprocess`) · 02: 6 (`Deferred#schedule` `Dispatcher#initialize` `FileSink#deliver` `Pipeline#emit` `Sink#deliver` `StdoutSink#deliver`) · 03: 4 (`Taint#initialize` `Taint#known_target` `Taint::Ghost#method_missing` `Taint::Ghost#respond_to_missing?`) | every call is claimed by a syntax-settled catalogue target, or there are no calls at all |
| **2** | **(b) needs receiver typing** | `Pipeline#transform` (`[1,2,3].map { \|n\| n * 2 }` — the typer types the literal AND the block parameter), `Taint#through_a_ghost` (`Ghost.new.anything_at_all` — a chained call on a project instance) | the port taints `dynamic-receiver/port-cannot-decide` |
| **2** | **(b)+(c) needs the `resolved` bit AND the transitive lane** | `Recursive#mutual_b`, `Recursive#walk` | the implicit-self call resolves upstream (needs `resolved`), AND the resulting edge must be closed over for the bit to stay true (slice 4) |

The 3 `missing-label` UNDERs (`Pipeline#run`, `Recursive#mutual_a`,
`Taint#literal_send`) are unchanged: they are slice 4's, as the s2 probe said.

### 5c. Predicted post-slice-3 verdict table — the acceptance to pin

Derived by running `effects_diff.compare()` over the T2 arm. It is pinned to the
REAL port, not to the simulation: the shipped binary's proven lane is
**byte-identical to the T2 simulation's on every method of all three graded
corpora** (0 lane diffs, § 8), which also proves rule 1 costs nothing there.

| project | oracle | rigor-rs | MATCH | UNDER | OVER | DM | UNDER by kind |
|---|---|---|---|---|---|---|---|
| `01_core_origins` | 16 | 16 | **16** | 0 | 0 | 0 | — |
| `02_propagation` | 15 | 15 | **10** | 5 | 0 | 0 | extra-taint 3, missing-label 2 |
| `03_taint` | 11 | 11 | **9** | 2 | 0 | 0 | extra-taint 1, missing-label 1 |
| `04_declared` | 4 | 0 | 0 | 4 | 0 | 0 | absent-method 4 |
| **TOTAL** | **46** | **42** | **35** | **11** | **0** | **0** → **PASS** |

`01_core_origins` reaching 16/16 is the collector fixture closing completely.
A HIGHER total than 35 is over-reach, not a bonus.

Two further acceptance rows this slice should add, both 0-OVER gates on probes
that today FAIL:

| probe | today | required |
|---|---|---|
| `p_posture` (80 methods, one per catalogued class) | **OVER=7** on the shipped binary | OVER=0 |
| `p_edge` (6 methods: Kernel-row shadowing ×2, posture edge ×1) | OVER=0 today only because the bit is off; **OVER=3** under T0, i.e. with rule 3 absent | OVER=0 |

### 5d. Real-project scale — mastodon/app, 6,948 methods

The first effects differential this repo has run on a real project (9.7 s):

| arm | MATCH | UNDER | OVER | verdict |
|---|---|---|---|---|
| shipped slice 2 | 4,517 | 2,431 (missing-label 1,486 · extra-taint **945**) | **0** | PASS |
| NAIVE slice 3 (direct bit) | 4,501 | 1,500 | **986** | **FAIL** |
| **T2** | **5,234** | 1,714 (missing-label 1,471 · extra-taint **243**) | **0** | **PASS** |

T2 closes **702 of the 945** (74%). The 243 residual, by the port's own cause:
**210 are `dynamic-receiver` alone** — pure receiver-typing debt — and ~33
involve `unresolved-self-call`, i.e. the `resolved` bit and/or the transitive
lane. Slice 2 is also validated at scale here: the method KEY sets agree exactly
(6,948 both sides), so unit identity, `attr_*` synthesis and `define_method`
units are right on 1,236 real files.

Read the T2 MATCH column as an upper bound at this scale: the lane-identity
check of § 5c was run on the three graded corpora, not on mastodon, and the
simulation's proven lane is better than the shipped port's on 15 of these
methods (missing-label 1,486 → 1,471). The **extra-taint** column — 945 → 243,
702 closed — is the honest slice-3 figure, and the OVER column is exact.

---

## 6. Typer consumption — available, cheap, and it does NOT close (b)

### 6a. The inventory holds post-#103/#105

Re-verified against the current tree; every surface the s2 probe § 5d listed is
still `pub` and unchanged in signature:

| need | rigor-rs, today |
|---|---|
| receiver class | `CoreIndex::class_name_of(&Interner, TypeId)` — `crates/rigor-index/src/lib.rs:466` |
| a class id's name | `CoreIndex::class_name_for_id` — `:450`; `SourceIndex::class_name_for_id` — `crates/rigor-infer/src/source_index.rs:1308`, `class_name_for_id_of` — `:1386` |
| the raw type | `Interner::get(TypeId) -> &Type` — `crates/rigor-types/src/interner.rs:62`; `Type::Dynamic(TypeId)` — `crates/rigor-types/src/ty.rs:168` |
| the closed world | `SourceIndex::build_project(&[&LoweredAst], &CoreIndex)` — `source_index.rs:552` |
| typing | `Typer::with_source` (`crates/rigor-infer/src/lib.rs:299`), `build_toplevel_env` (`:1982`), `type_of(&self, …)` (`:403`) — `&self`, the sig-gen precedent (`sig_gen.rs:322-326`) |

Still missing, and still the only genuinely absent surface: upstream's
**`resolved`** bit ("every dispatch tier declined"). rigor-rs computes the
question internally for `call.undefined-method` but exposes no predicate.

Also missing and NOT recorded before: **rigor-rs has no `DynamicOrigin`
analogue.** `coverage --protection` is explicitly unimplemented
(`crates/rigor-cli/src/coverage.rs:349-353`), so the port cannot produce the
`detail` upstream attaches to `dynamic-receiver` at all. Upstream's six causes
are `external_gem_without_rbs`, `framework_dsl_boundary`,
`analyzer_budget_cutoff`, `explicit_untyped`, `inferred_return_untyped`,
`unsupported_syntax` (`inference/dynamic_origin.rb:14-36`).

### 6b. Why reading it does not make the bit safe

To close class (b) the port must prove the receiver is **not `Dynamic` in the
REFERENCE's typer**. rigor-rs's own answer is the wrong evidence, and the repo
has already written down why:

> "conversely, rigor-rs's inference is more ROBUST on shapes the reference
> degrades to `untyped`/nil (a string-interpolation return, a `%i[]` word array,
> a top-level project-class `.new` → its instance)."
> — `crates/rigor-cli/src/sig_gen.rs:20-23`

`untyped == Dynamic[top]`. Every shape in that list is a case where rigor-rs
says "nominal" and the reference says `Dynamic`, i.e. where upstream taints and
the port would not — an OVER. sig-gen's sound-**superset** licence makes that
excess a win; ADR-0043 § 2 inverts the direction and makes the identical excess
a defect. § 3c is the same asymmetry already realised on the CONSTANT surface:
the reference cannot resolve `Net::HTTP`, and there is no reason to expect
rigor-rs to share that hole.

So the Typer's answer is usable in exactly one direction — **"rigor-rs says
`Dynamic`" is extra evidence to taint**, which adds UNDERs and closes nothing.
The direction slice 3 needs is not available.

**One narrow island is worth a probe of its own** (do not spec it blind): a
receiver that is a LITERAL — array, hash, string, integer, symbol, regexp,
lambda. `Collector#descriptor_for` maps `Tuple → Array`, `HashShape → Hash`,
`Constant → value.class` (`collector.rb:155-164`), and a literal's type is never
`Dynamic` in either engine. That would let the port claim `Array#map` for
`[1,2,3].map` — half of `Pipeline#transform`; the block parameter `n` still
needs real typing, so it closes 0 of the corpus's 4 on its own. Cheap, but it is
a subset argument, and this repo's record on those is 3 failures in one arc.

### 6c. What Typer consumption costs at runtime — measured

Today's `effects` command parses with Prism and walks; it never lowers, never
builds a `SourceIndex`, never types. Consuming the Typer means the whole
pipeline plus the `Span → NodeId` bridge the s2 probe § 5c designs.

On gitlab-foss/lib (**4,676 files**), same binary, 3 interleaved runs, wall:

| command | median (min–max) | note |
|---|---|---|
| `rigor effects --full --format=json` | **592 ms** (548–1081) | Prism parse + walk, SERIAL |
| `rigor check lib` | **469 ms** (462–553) | full pipeline, rayon-parallel |
| `RAYON_NUM_THREADS=1 rigor check lib` | **1,110 ms** (1108–1218) | full pipeline, serial |

So a serial Typer-consuming effects command lands near **1.1 s + the Prism walk
≈ 1.7 s, roughly 3× today's**, and the closed-world `SourceIndex::build_project`
is the stage-2 merge the harvest-cache note measures at **70.5 ms** at this scale
(`docs/notes/20260826-harvest-cache-remeasure.md` § 1). Parallelising the
per-file collection after a shared index would bring it back toward `check`'s
465 ms. None of that is prohibitive — the cost is not the reason to decline it;
§ 6b is.

---

## 7. `causes` — ungraded, and what shape keeps it honest

`effects_diff.py` reads exactly three keys per method — `lanes()` at `:233-246`
takes `effects`, `declared`, `exhaustive`, and `compare()` at `:249-286` reads
nothing else. **`causes` and `direct` are never graded.** Confirmed by reading
the grader, and by the fact that slice 2 ships an out-of-enum
`["port-incomplete", …]` marker today and passes.

Upstream's spelling, from a live oracle run:

```json
"causes": [["dynamic-receiver", "unsupported_syntax"]]
"causes": [["dynamic-send", null]]
"causes": []
```

A JSON array of two-element arrays: `[cause, detail]`. `cause` is one of
`TaintCause::ALL`'s ten strings (`taint_cause.rb:16-27`), in this order by
arising — `dynamic-receiver`, `dynamic-send`, `method-missing`,
`unresolved-self-call`, `opaque-callable`, `unknown-ownership`,
`plugin-attribution`, `template-not-analysed`, `collector-error`, `budget`.
`detail` is a string or `null`, and the pairs are **de-duplicated and sorted by
`[cause, detail.to_s]`** (`summary.rb:143-151`). Observed details:

| cause | detail |
|---|---|
| `dynamic-receiver` | the `DynamicOrigin` name (`inferred_return_untyped`, `unsupported_syntax`, …) or `null` |
| `dynamic-send`, `opaque-callable`, `unknown-ownership` | `null` |
| `unresolved-self-call` | the **selector** |
| `collector-error` | the method name |
| `plugin-attribution`, `template-not-analysed` | the plugin row key |

**Recommendation for slice 3**, so that slice 5's renderer and snapshot do not
inherit a lie: stop emitting out-of-enum markers and use the enum spelling the
SITE SHAPE earns — `dynamic-receiver` for an explicit receiver,
`unresolved-self-call` for a receiver-less call, and the exact producers'
own spellings. For `detail`, emit **the selector** for `unresolved-self-call`
(matching upstream character for character wherever upstream also taints) and
**`null`** for `dynamic-receiver` (which upstream also emits when it has no
`DynamicOrigin`, and the port has none at all — § 6a). That keeps
`causes.empty? == exhaustive` true on the port side, which it is not today.

---

## 8. Reproduction

```sh
# populate reference/rigor at b10bd5df from the main checkout (never the network,
# never REFERENCE_RIGOR_DIR), then:
python3 harness/effects_diff.py --self-test      # 46/46 MATCH on all four projects
cargo build --offline --release -p rigor-cli
python3 harness/effects_diff.py                  # MATCH=12 UNDER=34 OVER=0 DM=0
```

The direct-vs-transitive split, the T0/T1/T2 arms and the predicted table were
produced by driving the reference's OWN collector with the typer suppressed and
grading the result with `effects_diff.compare()`. The driver
(`typerfree_dump.rb`, session-scoped scratch, removed after measurement) is three
monkey-patches:

```ruby
class Rigor::Effects::Collector::Accumulator          # no CallRecords at all
  def record(_node, _rec) = nil
  def recorded?(_node)    = false
  def mark_unresolved(_n) = nil
end

class Rigor::Effects::UnitScan
  def posture_allowed?(_n, _r, _i) = false            # T2 rule 1
  def push_edge(_record, selector, _self_call)        # T2 rule 3
    taint("unresolved-self-call", "…") if SELECTORS.include?(selector)
  end
  alias_method :record_edge_orig, :record_edge        # T2 rule 2
  def record_edge(node, record, bound = nil)
    return taint("dynamic-receiver", "…") unless node.receiver.nil?
    record_edge_orig(node, record, bound)
  end
end
```

then `Runner#effect_table` is dumped per method as
`{effects: direct.proven, declared: [], exhaustive: direct.exhaustive?}` and fed
to `effects_diff.compare()` against the oracle's real output. `SELECTORS` is the
project's own selector set, taken from the oracle's method keys — the same
syntactic superset the port would build from its own units.

Four scratch projects, all `paths: [lib]`, all removed after measurement:
`p_edge` (§ 3b), `p_posture` (§ 3c — generated from `core.yml`'s `classes:`
keys, one method per class calling `zz_uncatalogued_zz`), `p_env` (§ 3d), and
`p_mast` / `p_cost` (a `.rigor.yml` beside a symlink to mastodon/app and
gitlab-foss/lib, § 5d and § 6c). `p_posture` and `p_edge` are worth **promoting
into `harness/effects-corpus/`** as slice-3 fixtures: `p_posture` catches a live
over-claim no existing gate sees, and it is generated from the vendored
catalogue, so it grows with the pin.
