# Effects slice 2 — DIRECT summaries: a probe

2026-08-26. Investigation only, no production code. Everything measured against
the PINNED submodule at `v0.3.4` (`b10bd5df`), invoked as
`ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib
reference/rigor/exe/rigor effects --full --format=json` from the project
directory each measurement names, with `.rigor/cache` cleared either side of
every run (`rigor effects` accepts no `--no-cache` — the slice-1 probed fact,
`harness/effects_diff.py:146-158`).

Subject: [ADR-0043](../adr/0043-effect-system-port-parity-model.md) slice 2 —
"**direct** summaries: catalogue rows + the construct origins (backticks,
`$gvar`, `@@cvar`, `@ivar` writes, `alias`/`undef`, `define_method`)", gated on
"**0 OVER on the fixture set**" (ADR-0043 § 5).

Headline results:

- **The grader has FOUR traps, not two.** Beyond the exhaustiveness direction
  and the declared lane, (c) the proven lane is compared as a **raw string-set
  subset, not a lattice subset** — a *coarser* label where the oracle narrows is
  an `OVER`; and (d) the **posture tier turns typer precision directly into
  proven labels**, so rigor-rs being *more* precise than the reference is an
  over-claim path. Both verified by running `effects_diff.compare()` itself.
- **Slice 2 needs no typer at all.** Every one of the corpus's 11 direct origins
  is settled by SYNTAX (a constant-path receiver, or implicit self). Restricting
  the catalogue lookup to syntax-settled targets is a provable subset of
  upstream's, closes the whole graded label debt, and makes ADR-0043 § 1
  ("may not change `rigor-infer`'s answers") true by construction.
- **Slice 2 must ship a minimal `rigor effects --full --format=json`.** It is the
  only surface `effects_diff.py` can measure, and without it the gate is green by
  construction — the exact shape the tool's own docstring warns about.
- **The declared lane is the only fatal thing on the corpus.** An otherwise
  perfect slice 2 scores `MATCH=12 UNDER=34 OVER=0 DECLARED-MISMATCH=1` → FAIL,
  on one method (`Declared#load_and_log`). Two clean scope-outs exist (§ 2b).
- **Predicted post-slice-2 verdict: `MATCH=12 / UNDER=34 / OVER=0 / DM=0` → PASS**
  with `04_declared` scoped out. The remaining debt decomposes as **30 methods
  waiting on slice 3 (taint), 3 methods / 4 labels on slice 4 (transitive), 1
  method on slice 6 (declared)** — measured, not estimated (§ 6).
- **The lowered AST cannot carry the construct origins** — `@@x = v` and `$x = v`
  both lower to a nameless `Node::VariableWrite`, and backticks / `alias` /
  `undef` / `@x ||=` all fall to `Node::Other`. The collector must walk the
  **Prism** tree, as upstream's does (§ 5).

---

## 1. Slice-2 scope, precisely

### 1a. The ADR's boundaries, restated against what was measured

| slice | ADR-0043 § 5 text | what that IS, measured |
|---|---|---|
| **2** | direct summaries: catalogue rows + construct origins | the per-method `direct:` bundle map — `{Origin => labels}` — and its flat join. **11 construct origin spellings** and the catalogue's row/universal/posture answer, per method body, with no edge followed |
| 3 | the taint bit and its causes | `exhaustive:` + `causes:`. **5 non-plugin causes** have a producer at the pin: `dynamic-receiver`, `dynamic-send`, `opaque-callable`, `unresolved-self-call`, `unknown-ownership` (§ 4g) |
| 4 | transitive propagation, overrides joined | the `effects:` key of the JSON is the TRANSITIVE lane (`effects_report.rb` `row_for`: `entry.proven`); `direct` is slice 2's. On the corpus they differ for exactly **3 methods / 4 labels** |
| 5 | `rigor effects` + `--format=json` + `update`/`check`/`diff` | the **snapshot family** and the text renderer. The report-only JSON surface is pulled forward into slice 2 by the gate itself (§ 3) |
| 6+ | the declared lane, envelopes, `effect.*` diagnostics | `declared:` — populated by `UnitScan#import_envelope` at the CALLER (slice-1 probe § 7), then `excluding_subsumed_by(proven)` at render (`effect_table.rb:41-43`) |

Two things ADR-0043 § 6 puts out of scope for the whole port and that therefore
bound slice 2 too: the **plugin effect layer** (`PluginFacts`, 1,107 lines across
9 Rails plugins — it is the sole consumer of the 7th narrowing handler
`sql_verb` and of `io.db.*` / `job.enqueue` / `email.send` / `cache.*`), and
**views as effect units**. The project's own `effects.attribution:` table
(`unit_scan.rb:360-372`) is a declared-lane producer and so belongs to slice 6.

### 1b. What slice 2 must NOT do, stated as gate risks

- Not report a method the oracle does not (`compare` line 253-256: an extra
  method key is an unconditional `OVER`).
- Not claim exhaustiveness (§ 2a).
- Not emit a proven label the oracle does not spell **character for character**
  (§ 2c).
- Not populate the declared lane wrongly, and not report a method whose declared
  lane it cannot compute (§ 2b).

---

## 2. The grading traps, verified against `effects_diff.py`'s comparison code

All four verified by importing `harness/effects_diff.py` and running its own
`compare()` over synthesised port outputs (reproduction in § 8).

### 2a. Exhaustiveness — the port must emit `false`, and it grades UNDER

`compare` (`harness/effects_diff.py:265-267`) is exactly:

```python
if sex and not rex:
    verdicts["OVER"] += 1
    findings.append(("OVER", name, "claims exhaustiveness the oracle does not"))
```

`sex` comes from `lanes()` (`:239`): `exhaustive = bool(entry.get("exhaustive", False))`.

**Verdict: CONFIRMED.** A port that emits `"exhaustive": false` for every method
can never trip that branch — `False and …` is `False` regardless of `rex`. The
ADR's `exhaustive_rs ⇒ exhaustive_ref` is vacuously satisfied by an always-false
antecedent. Omitting the key entirely is equally safe (the default is `False`),
but the port should emit it explicitly so the JSON is honest.

**The bit is spelled `"exhaustive"`** — a JSON boolean, one per method, beside
`"effects"` / `"declared"` (`lib/rigor/cli/effects_renderer.rb`, `render_json`).
The oracle also emits `"causes"` as a list of `[cause, detail]` pairs; the grader
**never reads it**, so its content is a port-side choice. Upstream's invariant is
`causes.empty? == exhaustive` (`summary.rb:56-64`), and `TaintCause::ALL`
(`taint_cause.rb:16-27`) is a closed enum, so a slice-2 port emitting an
out-of-enum marker (`["port-incomplete", "taint is ADR-0043 slice 3"]`) is
honest, ungraded, and cannot be mistaken for a real cause.

Cost, measured: 30 of the corpus's 46 methods are `exhaustive: true` in the
oracle, so always-false converts them from `MATCH` to `UNDER:extra-taint`
(`compare` `:275-277`). That is the arc's odometer, exactly as the ADR says.

### 2b. The declared lane — a missing lane is FATAL, and slice 2 needs a scope-out

`lanes()`'s docstring (`:232-234`) claims "a shape this tool does not understand
can only ever produce UNDER, never a phantom OVER". True for `OVER` — and
**misleading for the declared lane**, which is a separate fatal verdict:

```python
declared = set(entry.get("declared") or [])   # :238  — missing key ⇒ EMPTY SET
...
if sd != rd:                                   # :268  — EXACT match, both ways
    verdicts["DECLARED-MISMATCH"] += 1
```

So the two cases the question asks about are **the same case**: a port whose
method entry lacks `"declared"` and a port whose entry has `"declared": []` both
read as ∅, and both are `DECLARED-MISMATCH` against any non-empty oracle lane.

**Measured on the corpus:** exactly ONE method in 46 carries a non-empty declared
lane — `Declared#load_and_log` → `["io.db"]` (from `04_declared`; the other three
annotated methods report `[]`, which the slice-1 probe § 7 explains: the lane
belongs to the CALLER, and the render rule subtracts what `proven` already
admits). Simulated: an otherwise-perfect slice 2 scores

```
04_declared  oracle=4 rs=4  MATCH=0 UNDER=4 OVER=0 DECL-MISMATCH=1
  DECLARED-MISMATCH: Declared#load_and_log — declared lane [] != oracle ['io.db']
TOTAL  MATCH=12 UNDER=34 OVER=0 DECLARED-MISMATCH=1  => FAIL
```

**Verdict: slice 2 does NOT need a declared implementation — it needs a
scope-out**, and there are two, both simulated clean:

| option | 04_declared result | totals | note |
|---|---|---|---|
| **D. scope the project out**: the gate command names `01/02/03` only; `04_declared` is a recorded slice-6 debt | `rs=0`, 4×`UNDER:absent-method` | `MATCH=12 UNDER=34 OVER=0 DM=0` → **PASS** | honest, mechanical, one line in the mini-spec |
| C. withhold the annotated method | `rs=3`, 3 extra-taint + 1 absent | same totals | needs the port to know WHICH method is annotated, i.e. to parse envelopes — half of slice 6 |

**Recommend D**, plus one cheap self-guard that makes it robust on projects the
gate does not name: **the port emits `methods: {}` for any project whose
signature or source surface carries an effect annotation** (`%a{pure}` /
`%a{rigor:v1:effect …}`) or whose `.rigor.yml` carries `effects.attribution:`.
That is a *lexical* precondition test — no envelope semantics — and an empty map
is always an under-claim (§ 2b's option D result). It converts "remember not to
run the gate on 04" into a property of the binary, which matters the moment
`effects_diff.py` is pointed at a real project-shaped corpus, as ADR-0043 § 4
says it should be.

Do NOT reach for the caller-lane join from the slice-1 probe § 7. It needs the
envelope index, the `envelope_target` carrier rule (`unit_scan.rb:345-350`:
implicit-self resolves against **this unit's own class**, not `Kernel`), *and*
`excluding_subsumed_by(proven)` at render — and omitting only the last of the
three produces a `DECLARED-MISMATCH` on correct code.

### 2c. NEW TRAP — the proven lane is a raw STRING-set subset, not a lattice subset

```python
over_labels = sp - rp     # :261  plain Python set difference over strings
```

There is no `subsumes` anywhere in the grader. So `io.fs` where the oracle proves
`io.fs.read` is `{"io.fs"} - {"io.fs.read"} = {"io.fs"}` → **OVER**. Measured:

```
F. coarse-label probe (io.fs vs io.fs.read)
  OVER=1  ('OVER', 'Origins#read_file', "proven labels not proven by the oracle: ['io.fs']")
```

This is not a grader artefact — it is ADR-0043 § 2 working as written. An
envelope check reads the proven lane, and `≤ io.fs.read` is exceeded by a bare
`io.fs`. The port's "safe direction" for an uncertain answer is therefore **∅,
never the parent label**, which is the exact opposite of upstream's handler
contract ("every handler is total and answers an **upper bound** … when the
literal does not settle the question it returns the row's parent label",
`narrowing.rb:19-21`).

Two consequences the mini-spec must state:

1. **Narrowing is not optional for slice 2.** `rigor-effects`'s slice-1
   `lookup_with` (`crates/rigor-effects/src/catalog.rs:433`) deliberately answers
   a narrowed row's **unnarrowed** entry. Consuming that directly gives
   `Time.new(2020,1,1)` → `nondet.time` where the oracle proves ∅ (**OVER**), and
   `File.open(p,"w")` → `io.fs` where the oracle proves `io.fs.write`
   (**OVER**). Either implement the handler or **drop the row entirely** — both
   are safe; using the fallback is not.
2. Wherever the port's AST cannot see what Prism sees, the fallback is ∅.

### 2d. NEW TRAP — the posture tier converts typer precision into proven labels

`Catalog#lookup`'s third tier answers the class's POSTURE for any selector the
class does not row (`catalog.rb:184`; ported at
`crates/rigor-effects/src/catalog.rs:450-452`), and `catalog_target`
(`unit_scan.rb:553-565`) falls back to `record.receiver_class` — the class the
**typer** projected the receiver to. Measured on a scratch project:

```
Posture#const_receiver  eff=[io.fs]   direct={catalogue:File.some_uncatalogued_thing: [io.fs]}
Posture#typed_local     eff=[io.net]  direct={catalogue:TCPSocket#some_uncatalogued_thing: [io.net],
                                              catalogue:TCPSocket.new: [io.net]}
Posture#literal_local   eff=[]        exhaustive=true      # String's `value` posture claims the call
Posture#universal_wins  eff=[io.net]  direct={catalogue:TCPSocket.new: [io.net]}   # `sock.class` ⇒ universal ∅
Posture#implicit_self_call eff=[]     exhaustive=true      # posture_allowed? is false for implicit self
```

`Posture#typed_local` is the hazard in one line: `sock.some_uncatalogued_thing`
proves `io.net` **because the typer said `TCPSocket`**. `sig_gen.rs`'s own header
records that rigor-rs is sometimes MORE precise than the reference ("rigor-rs's
inference is more ROBUST on shapes the reference degrades to `untyped`/nil"), and
here that excess lands straight in the proven lane as an over-claim. The
sound-superset licence sig-gen enjoys does not transfer; ADR-0043 § 2 inverts it.

**Mitigation, and it is free on the graded corpus:** slice 2 consults the
catalogue **only where `catalog_target` is settled by SYNTAX** —

- receiver `nil` or `self` → `["Kernel", false, implicit=true]` (`:555`)
- a constant-path receiver → `[constant, !object_constant?(constant), false]` (`:558`)

— and declines the third arm (`record.receiver_class`) entirely. That is a strict
subset of upstream's target set, so every answer it *does* give is upstream's own
answer, and the excluded arm can only cost `UNDER`. The posture tier stays ON for
constant receivers (`File.some_uncatalogued_thing` → `io.fs`, above), which is
safe because both engines spell the owner from the same syntax and read the same
vendored bytes.

Note `posture_allowed?` (`:429-431`) is `!implicit && !record&.dynamic &&
!DEFERRED_SELECTORS.include?(name)`. With the typer arm declined, `record` is
never consulted and the `&.` short-circuits to falsy — which is upstream's own
default when it has no record for the node, so the two agree. The port still owes
the other two conditions, and both are syntax-only.

---

## 3. The output surface — the minimal JSON, and whether slice 2 must ship a command

### 3a. What the grader actually consumes

`_parse_methods` (`effects_diff.py:160-169`) finds the **first `{` in stdout**,
`json.loads` from there, and reads exactly `obj["methods"]`, requiring a `dict`.
`lanes()` then reads three keys per method. The full consumed shape is:

```json
{ "methods": { "<Key>": { "effects": ["…"], "declared": ["…"], "exhaustive": true } } }
```

and nothing else. `causes` and `direct` are produced by the oracle
(`effects_renderer.rb` `render_json`) and **never graded**. The snapshot header
keys (`schema`, `rigor`, `vocabulary`, `config_digest`) are not in the report
JSON at all — they belong to `.rigor-effects.yml`
(`effects_diff.py:63` names them for the slice-5 comparison).

Method-key spellings, all measured: `Class#m`, `Class.m`, `Outer::Inner#m`,
`<toplevel>#m` (`scanner.rb:43`), `Class#attr=` for a synthesised writer.

Hard requirements on the port's process, from `run_rs` (`:194-221`):

| requirement | why |
|---|---|
| exit **0** | anything else is `INVALID` unless the message contains `unknown command` |
| accept `--full` **and** `--format=json` | an option error exits non-zero → `INVALID` |
| print a JSON object even when empty | `_parse_methods` returning `None` is `INVALID`, not "0 methods" |
| no preamble containing `{` before the payload | the parser starts at the first `{` |
| run correctly with **cwd = the project** and `stdin` closed | both arms run in-project by design (ADR-0043 § 4) |

### 3b. Verdict: yes, slice 2 carries a minimal report command

Slice 5 owns `rigor effects` in the ADR's table — but slice 2's gate is "0 OVER
on the fixture set", and the only instrument that computes it invokes
`rigor effects --full --format=json`. With no such command the run returns
`NOT-IMPLEMENTED` with `{}` (`:214-217`), every oracle method counts `UNDER`, and
`OVER` is **0 by construction** — the tool's own header calls that "the exact
shape that let a crashing binary pass the central gate once already". A slice
whose gate cannot fail has not been gated.

The alternatives were considered and rejected:

- *A hidden flag / env var / test-only binary.* Requires editing `run_rs`, i.e.
  bending the graded instrument to the port's convenience, and breaks
  ADR-0043 § 4's "the measured binary is `target/release/rigor`".
- *A Rust integration test that calls the collector directly.* Grades the
  collector, not the surface the oracle is compared through, and cannot see the
  project driver (config → file set → unit identity), which is where an
  extra-method `OVER` would come from.

So: **slice 2 ships `rigor effects [PATHS…]` with `--full` and `--format=json`
only.** Explicitly deferred to slice 5: `update` / `check` / `diff` / `explain`,
`--no-tolerated-effects`, and the polished text renderer (a one-line-per-method
text form costs four lines and should ship anyway so the default invocation is
not an error). `--full`'s semantics are worth implementing faithfully —
`trivial?` is "exhaustive AND proven ⊆ `{mutate.local}` AND nothing declared
survives" (`effect_table.rb:50-54`, `summary.rb:82-84`) — but on the graded path
`--full` is always passed, so a port that always lists everything is graded
identically.

---

## 4. Upstream's direct-summary mechanics (`unit_scan.rb`, 572 lines)

Two rules shape the walk, and both are quoted from its header (`:20-34`):
**containment** — a block literal's origins always join the enclosing method,
so the walk descends into `BlockNode`s and stops only at a nested `def` or a
literal-named `define_method`; and **observation** — "everything the walk knows
about a receiver comes from what the typer already decided at that call node …
the scan resolves nothing, walks no callee, and touches no `Scope`."

### 4a. Unit identity (`scanner.rb`) — what becomes a method key

Measured on a scratch project (47 methods):

| source | key(s) produced | direct summary |
|---|---|---|
| `def m` at top level | `<toplevel>#m` | its own |
| `def m` / `def self.m` / `class << self; def m` | `C#m` / `C.m` / `C.m` | its own |
| nested `def` inside a method | `C#nested_def`, **its own unit** | enclosing gets ∅ — a nested def is NOT `mutate.static` |
| `define_method(:lit){}` in a **class body** | `C#lit` | enclosing class body is not a unit at all |
| `define_method(:lit){}` **inside a method** | `C#lit` **and** the enclosing method gains `construct:define-method` → `mutate.static` | `unit_scan.rb:175-190` |
| `attr_reader :ro` | `C#ro` → ∅ | synthesised (`scanner.rb:222-229`) |
| `attr_writer :wo` | `C#wo=` → `construct:attr-writer` → `mutate.self` | |
| `attr_accessor :rw` | **both** `C#rw` (∅) and `C#rw=` (`mutate.self`) | |
| `module W; class I; def deep` | `W::I#deep` | lexical `::` join |
| `alias aliased original` in a **class body** | **no unit, no label** | class bodies are not units in v1 |

Reopenings join through `merge_unit` (`scanner.rb:252-255`); across files the
runner merges collections. A unit the scanner cannot finish is recorded as
`Summary.tainted("collector-error", name)` and its siblings are unaffected
(`scanner.rb:194-195`) — fail-soft per unit.

### 4b. A call site → origins, in the order `visit_call` runs them (`:220-233`)

1. `attribute` — the project's `effects.attribution:` table → **declared** lane +
   a `plugin-attribution` taint. (slice 6)
2. `attribute_plugin` — the plugin stratum. (out of ADR scope)
3. `import_envelope` — the callee's envelope → **declared** lane at the CALLER.
   (slice 6)
4. `claimed_by_catalogue?` — **this is slice 2**.
5. `visit_uncatalogued` — only when the catalogue did not claim; produces taints
   and project edges. (slices 3/4)
6. `visit_block_argument` — `&expr` taint. (slice 3)

`claimed_by_catalogue?` (`:381-395`) is four lines of real work:

```ruby
owner, singleton, implicit = catalog_target(node, record)
entry = Catalog.default.lookup(owner, node.name.to_s, singleton:, call_node: node,
                               posture: posture_allowed?(node, record, implicit))
return false if entry.nil?
add(Origin.catalogue(catalogue_key(owner, singleton, node)), entry.labels) unless entry.labels.empty?
classify_mutation(node.receiver) if mutating_catalogued?(node, entry, owner)
push_edge(record, node.name.to_s, implicit) if keeps_project_edge?(entry, implicit)
true
```

- **Origin key spelling** (`catalogue_key`, `:417-419`):
  `catalogue:<Owner>#<sel>` for instance, `catalogue:<Owner>.<sel>` for
  singleton. Measured: `catalogue:ENV#[]`, `catalogue:ENV#[]=`,
  `catalogue:File.read`, `catalogue:Kernel#puts`, `catalogue:Time.new`,
  `catalogue:TCPSocket#some_uncatalogued_thing`.
- **A row answering explicit ∅ still CLAIMS the call** — that is what the 77
  `effects: []` rows are for. `add` is skipped (`labels.empty?`), so the origin
  does not appear in `direct:`, but the uncatalogued path is suppressed and no
  taint is produced. Same for a narrowed row that narrows to ∅: measured,
  `Time.new(2020,1,1)` has `direct: {}` **and** `exhaustive: true`.
- **`kind: object` constants** (`ENV ARGF STDIN STDOUT STDERR`) flip the
  singleton bit: `catalog_target` (`:558`) returns
  `[constant, !object_constant?(constant), false]`, so `ENV["HOME"]` keys as the
  INSTANCE row `ENV#[]` while `File.read` keys as the singleton `File.read`.
  Available in slice 1 as `Catalog::object_constant`
  (`crates/rigor-effects/src/catalog.rs:403`).
- **`keeps_project_edge?`** (`:409-411`) is `entry.posture? || implicit` — a
  slice-4 concern, but the `from_posture` bit slice 1 already carries
  (`Entry::posture`, `catalog.rs:165`) is what it reads.

### 4c. The narrowing handlers — all six non-plugin ones, with measured semantics

`Narrowing::HANDLERS` declares seven (`narrowing.rb:55`); `core.yml` uses six
across 7 rows. `sql_verb` has **no `core.yml` row** — it serves plugin rows only
(`connection.execute` / `exec_query` / `select_all`), so slice 2 does not need it.

Every handler reads the call's **own argument literals and nothing else** — no
dataflow, by design (`narrowing.rb:11-15`).

| handler | rows | rule | measured |
|---|---|---|---|
| `file_open` | `File.open` (fallback `io.fs`) | `mode_labels(node, 1)` | `File.open(p)` → `io.fs.read` (absent mode is Ruby's `"r"`, **not** unknown); `"w"`→`io.fs.write`; `"a"`→`io.fs.write`; `"r+"`→`[io.fs.read, io.fs.write]`; `"wb"`→`io.fs.write` (the `b`/`t` flag and a `:ENC` suffix are stripped by `/\A[rwa]\+?/`); `"r:UTF-8"`→`io.fs.read`; `mode: "w"` keyword→`io.fs.write`; a computed mode→`io.fs`; `File::RDWR` (an integer flag, deliberately unresolved)→`io.fs` |
| `pathname_open` | `Pathname#open` (`io.fs`) | `mode_labels(node, 0)` — one argument left, the receiver is the path | measured `p.open("w")` → ∅ + `dynamic-receiver`, because an untyped `p` never reaches the row at all |
| `kernel_open` | `Kernel#open` (fallback `io`) | literal prefix starting `"\|"` → `io.process`; else `mode_labels(node, 1)`; non-literal → `io` | `open("\|ls")`→`io.process`; `open("\|#{cmd}")`→`io.process` (the leading literal RUN of an interpolated string counts); `open(t)`→`io` |
| `uri_open` | `URI.open`, `OpenURI.open_uri` (fallback `io`) | `http(s)://`→`io.net.http`; `file://`→`io.fs.read`; anything containing `://`→`io`; a bare path→`io.fs.read`; non-literal→`io` | all four measured |
| `time_new` | `Time.new` (fallback `nondet.time`) | **0 positional args** → `nondet.time`, any positional → ∅ | `Time.new`→`nondet.time`; `Time.new(2020,1,1)`→∅; **`Time.new(in: "+09:00")`→`nondet.time`** — keyword args are `grep_v`'d out of the positional count (`:170`) |
| `random_new` | `Random.new` (fallback `nondet.random`) | same shape | `Random.new`→`nondet.random`; `Random.new(42)`→∅ |

`WRITE_MODES` is `%w[w a w+ a+ r+]` (`:42`), and `mode_labels`'s three states are
load-bearing: **absent** = `"r"`, **present but unreadable** = the subsystem
parent `io.fs`, **a literal** = narrowed.

**Port note (§ 2c):** `time_new`/`random_new` are the only handlers where the
port's lowered arg list is lossy, and the safe direction is to **count every
lowered argument as positional** — `Time.new(in: …)` then answers ∅ (an UNDER)
instead of risking the reverse.

### 4d. Construct origins — all eleven spellings

Ten from `visit` (`unit_scan.rb:192-218`), one synthesised in `scanner.rb:55`.
Every one measured, and the value is a shared constant per kind (`:60-71`) — a
construct origin is line-free and carries no per-site state.

| origin | trigger | labels |
|---|---|---|
| `construct:define-method` | `define_method(:literal){}` **inside a method body** | `mutate.static` |
| `construct:xstring` | backticks / `%x{}`, interpolated or not | `io.process` |
| `construct:gvar-read` | `$LOAD_PATH` — **except** `FRAME_LOCAL_GLOBALS` = `$~ $_ $& $` $' $+ $!` (`:40`), measured: `$~` → ∅, exhaustive | `global.read` |
| `construct:gvar-write` | `$x =`, `$x ||=`, `$x &&=`, `$x op=` | `global.write` |
| `construct:cvar-read` | `@@x` | `global.read` |
| `construct:cvar-write` | `@@x =` and the three compound forms | `mutate.static` |
| `construct:ivar-write` | `@x =`, `@x ||=` (measured), `@x &&=`, `@x op=` | `mutate.self`, or **`mutate.static` in a singleton unit** (`:209`) |
| `construct:alias` | `alias` / `alias $a $b` **inside a method body** | `mutate.static` |
| `construct:undef` | `undef` inside a method body | `mutate.static` |
| `construct:receiver-mutation` | see § 4e | `mutate.{self,static,instance,local}` |
| `construct:attr-writer` | a synthesised `attr_writer`/`attr_accessor` setter | `mutate.self` |

An ivar **read** produces nothing; only writes do. Prism gives `$1` and `$&`
their own node types, so only the named specials need the exclusion list.

### 4e. `mutates: receiver` and the three by-reference mutator sets

Two independent questions (`mutation_classifier.rb:14-24`), both conservative.

**Is it a mutation?** (`mutating?`, `:56-66`)

- `[]=` and any `ATTRIBUTE_WRITER` (`/\A[a-z_][A-Za-z0-9_]*=\z/`, deliberately
  not `==` / `<=` / `!=` / `===`) — a write on **every** receiver, no type needed.
- otherwise the receiver's class must be known, and the selector must be in that
  class's set: **`ARRAY_MUTATORS` (31)** and **`HASH_MUTATORS` (15)**, cited from
  `lib/rigor/inference/mutation_widening.rb:75`/`:87`, and **`STRING_MUTATORS`
  (26)** at `mutation_classifier.rb:31`. Counts re-measured through the pinned
  loader. `n << 2` is a bit shift and `io << "x"` is output — hence the gate.

**Who owns the receiver?** (`label_for` / `ownership`, `:69-89`)

| receiver | label |
|---|---|
| `nil` (implicit self), `self`, an `@ivar` read | `mutate.self`, or `mutate.static` in a singleton unit |
| a `@@cvar` read | `mutate.static` |
| a local that is a **parameter** | `mutate.instance` |
| a local in `owned_locals` | `mutate.local` |
| anything else | **nil ⇒ `unknown-ownership` taint, never a bare `mutate`** |

`owned_locals` is `LocalOwnership.owned` (`local_ownership.rb:35-47`):
flow-insensitive and whole-body — every assignment to the name must be an
allocation (`[]`, `{}`, a string literal, a lambda, `.new`/`.dup`/`.clone`, `+""`)
and the name must never escape (a call argument, a `return`, an array element, an
`@ivar`/`$g`/`@@c`/`CONST` write's value, an assoc value, a block-pass, **or the
body's trailing expression**).

Two shapes enter through a catalogue row rather than the uncatalogued path
(`mutating_catalogued?`, `:413-415`): an explicit `mutates: receiver` row
(8 rows), or a **posture** answer plus `mutating?`. Note the posture arm passes
`owner` (the catalogue target) as the receiver class, not `record.receiver_class`.

**Slice-1 carry-over:** the slice-1 impl note § 6.2 records a pinned deviation —
on the posture path `Array#push` answers `mutates_receiver == false` where
upstream answers `true`, because the vendored YAML names the sets by reference
and slice 1 did not expand them. Slice 2 owns expanding all three sets and
changing that test deliberately.

### 4f. Self / implicit calls, blocks and yields at DIRECT scope

- **Implicit self** (`receiver.nil?`) and `self.` both spell `["Kernel", false,
  implicit=true]` (`:555`). The posture is **disallowed** for them (`:429-431`) —
  otherwise every unqualified call in a project body would be coloured `io`. A
  `Kernel` **row** still fires: `puts`, `rand`, `open`, `require` are the common
  hits. Measured control: `implicit_self_call` calling a project `helper` →
  ∅, exhaustive.
- **Blocks are containment.** `walk` descends into every block; a block literal's
  origins join the enclosing method whether the callee invokes it now, later or
  never. Measured: `Deferred#schedule`'s `proc { puts "ran" }` → the method proves
  `io.output.stdout`; `instance_eval { puts "block" }` likewise (the BLOCK form of
  an eval-family call does **not** taint, `:52-54`).
- **`yield`** originates nothing at direct scope — it is not a `CallNode`.
- **`&blk` forwarding** on the unit's own block parameter is ∅, not a taint
  (`:490`, `:528`).

### 4g. What makes a DIRECT summary tainted — the boundary slice 2 must know

`TaintCause::ALL` lists ten (`taint_cause.rb:16-27`). Grepping `lib/` for
producers at the pin: **`method-missing` and `budget` have none**;
`template-not-analysed` comes only from a plugin `row.taint`; `collector-error`
from `scanner.rb:194`; `plugin-attribution` from the attribution table and
non-discharging plugin rows. That leaves **five the core collector produces**,
all in `visit_uncatalogued` and its helpers:

| cause | trigger | measured |
|---|---|---|
| `dynamic-send` | `send`/`public_send`/`__send__` with a **non-literal** selector (`:472-477`). A literal selector is an ordinary edge and must NOT taint | `Taint#dynamic_send` |
| `opaque-callable` | (a) an eval-family call (`eval instance_eval class_eval module_eval`) with **≥1 positional argument**, or a bare argument-less receiver-less `binding` (`:543-548`); (b) a `.call` the analyzer cannot follow — receiver is not a lambda, not the unit's `&blk`, and the record is nil / has no receiver class / is `Proc`/`Method` (`:486-493`); (c) `&expr` where `expr` is neither a symbol nor the unit's `&blk` (`:522-531`) | `eval_string`, `bare_binding` (both ∅ + taint); `eval_block` → **no taint**, and the block's `puts` joins |
| `dynamic-receiver` | the typer said `Type::Dynamic`, and no envelope/plugin bound applies. `detail` is the `DynamicOrigin` name (`inferred_return_untyped`, `unsupported_syntax`, or `null`) | `Taint#dynamic_receiver`, `Origins#pure_arithmetic` (`(a + b) * 2` — untyped params!) |
| `unresolved-self-call` | a receiver-less call the dispatcher declined (`record.nil?` or `!record.resolved`) with no bound (`:500-512`) | |
| `unknown-ownership` | a mutation whose receiver's ownership is not provable (`:533-538`) | `Origins#owns_what_it_mutates` |

The line slice 2 must respect: **`unknown-ownership` is a slice-3 taint, but the
label suppression that produces it is slice-2 behaviour.** A port that emits a
bare `mutate` (or guesses `mutate.local`) where ownership is unprovable is an
`OVER` today, regardless of the taint bit.

Also load-bearing for slice 3: `classify_mutation` runs **first and
independently** of the taints (`:445`) — `params[:x] = 1` on an untyped `params`
is a proven `mutate.instance` *and* a `dynamic-receiver` taint.

---

## 5. The rigor-rs seam

### 5a. The observational precedent

`sig_gen.rs` is the standing pattern and it is three lines
(`crates/rigor-cli/src/sig_gen.rs:322-326`):

```rust
let index = CoreIndex::new();
let source_index = SourceIndex::build(&ast, &index);
let typer = Typer::with_source(&index, &source_index);
let mut interner = Interner::new();
let env = typer.build_toplevel_env(&ast, &mut interner);
```

then `typer.type_of(ast, node_id, &env, &mut interner)` per node. `Typer::type_of`
takes `&self` (`crates/rigor-infer/src/lib.rs:403`) — the only `&mut` is the
interner. Nothing in `rigor-infer` is asked to decide anything new. That is
exactly ADR-0043 § 1's "observational".

### 5b. The blocking finding — the lowered AST cannot carry the construct origins

Measured against `crates/rigor-parse/src/ast.rs`:

| construct slice 2 needs | lowered as | verdict |
|---|---|---|
| `@x = v` | `Node::InstanceVariableWrite { name, … }` (`:700`, lowered `:1813`) | **usable** |
| `@@x = v` | `Node::VariableWrite { value, span }` (`:688`, lowered `:1823`) | nameless |
| `$x = v` | the **same** `Node::VariableWrite` (`:1830`) | **indistinguishable from the cvar write** — and they carry OPPOSITE labels (`mutate.static` vs `global.write`) |
| `@x` / `@@x` / `$x` read | the **same** `Node::VariableRead { span }` (`:681`, `:1838-1852`) | all three collapse; `FRAME_LOCAL_GLOBALS` is impossible |
| `@x \|\|= v` | no owned variant — only the LOCAL op/and/or writes are lowered (`:1076-1100`) | lost; `Origins#ivar_memo` is unreachable |
| backticks, `alias`, `undef`, `a[i] op= v` | `Node::Other { span, jump }` (`:757`) | lost |

So the collector **cannot ride the lowered AST**, and growing the lowered node
set to fix it would change what every structural rule walk sees — a `rigor check`
movement risk on a slice whose whole promise is zero movement.

### 5c. The proposal — walk Prism, index the lowered arena by span

Upstream does exactly this: `Collector.record_root` pins the parsed root and
`Scanner` walks the **Prism** tree, consulting a node-identity table of
`CallRecord`s the typer filled in (`collector.rb:38`, `:114-117`).

rigor-rs already has the pieces:

- `rigor_parse::parse(source) -> ruby_prism::ParseResult` (`lib.rs:20`) and
  **`pub use ruby_prism`** (`lib.rs:9`) — a consumer can walk the real Prism tree
  with no new dependency, and the lowering demonstrably reaches every node type
  slice 2 needs (`as_class_variable_write_node`, `as_global_variable_read_node`, …).
- The typer's answer is keyed by lowered `NodeId`, so the bridge is a
  **`Span → NodeId` index over the lowered `Node::Call` nodes**, built once per
  file. `Node::Call` already carries `receiver: Option<NodeId>` and `span`
  (`ast.rs:385`), so a Prism `CallNode`'s `location()` offsets look up the lowered
  twin and hand back the receiver's `NodeId` to `type_of`. This is the exact
  analogue of upstream's `compare_by_identity` call table.

**And slice 2 does not need that bridge at all.** § 2d's syntax-only restriction
means the collector never asks the typer anything: all 11 corpus origins, and
every origin in the two scratch projects except `Posture#typed_local`, are
settled by a constant-path receiver or implicit self. Build the span index in
slice 3, when the `dynamic` bit and the `resolved` bit first matter.

### 5d. Receiver-type surface: what Typer exposes TODAY

For slices 3-4, upstream's `descriptor_for` (`collector.rb:155-164`) maps a
receiver type to `[class_name, kind]`. The rigor-rs analogue is already public,
and **no new `rigor-infer` surface is required**:

| upstream | rigor-rs, today | where |
|---|---|---|
| `Type::Nominal → [name, :instance]` | `CoreIndex::class_name_of(&interner, ty)` | `crates/rigor-index/src/lib.rs:466` |
| `Type::Tuple → ["Array"]`, `HashShape → ["Hash"]`, `Constant → value.class` | same function, same erasure | `:468`, `:477-479` |
| `Type::Singleton → [name, :singleton]` | **not covered** by `class_name_of` — write it in the consumer from `Interner::get` (`crates/rigor-types/src/interner.rs:62`) + `CoreIndex::class_name_for_id` (`rigor-index/src/lib.rs:450`), both `pub` | |
| `Type::Dynamic → descriptor_for(static_facet)` | `Type::Dynamic(TypeId)` is a public variant; recurse in the consumer | `crates/rigor-types/src/ty.rs:168` |
| a **project** class name | `SourceIndex::class_name_for_id` / `class_name_for_id_of` | `crates/rigor-infer/src/source_index.rs:1308`, `:1386` |
| the closed world slice 4 needs | `SourceIndex::build_project(asts, core)` | `:552` |

The one genuinely absent surface is upstream's `resolved` bit — "every dispatch
tier declined", which separates an unresolvable implicit-self call from an
ordinary one. That is a **slice-3** need, and the minimal read-only addition is a
predicate the `call.undefined-method` machinery already computes internally.
Slice 2 needs none of it.

### 5e. Where the code should live

**Recommendation: `crates/rigor-cli/src/effects/` (collector + command), with
`crates/rigor-effects` staying pure data and vocabulary.**

- It is the `sig_gen.rs` precedent: an observational consumer of `Typer` lives in
  `rigor-cli`, which already depends on `rigor-parse`, `rigor-infer`,
  `rigor-index`, `rigor-types`, `serde_json` and `serde_yaml`
  (`crates/rigor-cli/Cargo.toml:40-58`). The only manifest change is
  `rigor-effects = { path = "../rigor-effects" }`.
- It preserves the slice-1 argument verbatim: `rigor-effects` depends on nothing
  of ours, so ADR-0043 § 1 stays a dependency-graph fact. Pushing `rigor-parse` /
  `rigor-infer` into `rigor-effects` would discard that for nothing.
- Keeping the collector out of `rigor-infer` / `rigor-rules` makes "the effects
  work cannot change what `rigor check` decides" true by construction, which is
  the strongest form of § 1's promise.

If a later slice needs the collector from the check path (slice 6's `effect.*`
diagnostics), promoting it to its own crate is a visible, arguable manifest
change — which is the point.

**One harness chore this creates.** `crate_source_dirs`
(`harness/effects_diff.py:75-100`, and its three twins) derives the
stale-binary scan from the `rigor-cli` path-dependency closure. The moment
`rigor-cli` depends on `rigor-effects`, the crate re-enters the scan
automatically — by design, per the slice-1 impl note § 4. The **prose** at
`effects_diff.py:80-83` ("`rigor-effects` … is deliberately such a member")
becomes stale in the same commit and should be updated with it.

---

## 6. Debt prediction by TYPE

Predicted by running `harness/effects_diff.py`'s own `compare()` over a
synthesised port output — the *ideal* slice 2, whose proven lane is exactly the
union of the oracle's own `direct:` bundles, with `exhaustive: false` and
`declared: []` everywhere. Predicting by construction, not by grepping.

### 6a. The baseline, decomposed

| project | methods | transitive labels | of which DIRECT | oracle exhaustive | transitive-only methods |
|---|---|---|---|---|---|
| `01_core_origins` | 16 | 12 | **12** | 13 | — |
| `02_propagation` | 15 | 12 | 9 | 10 | `Pipeline#run` (2), `Recursive#mutual_a` (1) |
| `03_taint` | 11 | 3 | 2 | 6 | `Taint#literal_send` (1) |
| `04_declared` | 4 | 1 | 1 | 4 | — |
| **total** | **46** | **28** | **24** | **33** | **3 methods / 4 labels** |

So of the recorded 46-method / 28-label debt: **24 of 28 labels are DIRECT** and
in slice 2's reach; 4 need slice 4.

### 6b. Predicted post-slice-2 verdicts, per corpus

With `04_declared` scoped out (§ 2b option D):

| project | oracle | rigor-rs | MATCH | UNDER | OVER | DECLARED-MISMATCH | UNDER by kind |
|---|---|---|---|---|---|---|---|
| `01_core_origins` | 16 | 16 | **3** | 13 | 0 | 0 | extra-taint 13 |
| `02_propagation` | 15 | 15 | **4** | 11 | 0 | 0 | extra-taint 9, missing-label 2 |
| `03_taint` | 11 | 11 | **5** | 6 | 0 | 0 | extra-taint 5, missing-label 1 |
| `04_declared` | 4 | 0 | 0 | 4 | 0 | 0 | absent-method 4 |
| **TOTAL** | **46** | **42** | **12** | **34** | **0** | **0** → **PASS** |

Today's baseline for comparison: `MATCH=0 UNDER=46 OVER=0` (NOT-IMPLEMENTED).

The 12 `MATCH`es are precisely the methods the oracle **also** marks
non-exhaustive and whose transitive lane equals their direct lane — 01's
`pure_arithmetic` / `owns_what_it_mutates` / `mutates_its_argument`; 02's
`each_with_effect` / `dispatch` / `load_input` / `parse`; 03's `dynamic_receiver`
/ `dynamic_send` / `fully_resolved` / `opaque_callable` /
`unknown_constant_receiver`.

Note that 01_core_origins — the collector fixture — yields only **3** MATCH at
slice 2 despite closing all 12 of its labels. That is not a slice-2 failure; 13
of its 16 methods are exhaustive in the oracle and slice 2 declines the bit.

### 6c. What structurally cannot close before slices 3 / 4 / 6

Simulated by turning the taint bit on and leaving everything else identical:

| blocker | closes | measured effect |
|---|---|---|
| **slice 3** (taint) | **30 methods**, from `UNDER:extra-taint` to `MATCH` | the same simulation with exact `exhaustive` scores `MATCH=42 UNDER=3` |
| **slice 4** (transitive) | **3 methods / 4 labels** — `Pipeline#run` (`io.fs.read`, `io.output.stdout` two hops down), `Recursive#mutual_a` (`nondet.time` through mutual recursion), `Taint#literal_send` (`io.output.stdout` through a literal `send`) | these are the only `under:missing-label` rows left after slice 3 |
| **slice 6** (declared) | **1 method** — `Declared#load_and_log` | the only non-empty declared lane in the corpus |

So the corpus's theoretical ceiling before slice 6 is `MATCH=45/46`, and the
whole 28-label proven debt closes at slice 4.

### 6d. What the corpus does NOT measure, and the coverage this slice should add

`effects_diff.py` exercises **11 distinct origin keys** — 6 catalogue rows out of
420 (1.4%), 5 construct spellings out of 11. Absent entirely: every posture,
every `universal:` selector, all six narrowing handlers (except `Time.new`
narrowing to ∅), `kind: object` beyond `ENV#[]`, all three mutator sets,
`mutate.local` / `mutate.instance` (no corpus method produces either — see § 7b),
`<toplevel>#m`, `attr-writer`, `define-method`, `alias`, `undef`, `cvar-read`,
and singleton units. Slice 2 should grow the corpus with the shapes § 4c/§ 4d
enumerate; the two scratch projects in § 8 are a ready draft of one.

---

## 7. Small findings

### 7a. `Origins#owns_what_it_mutates`'s fixture comment does not match the oracle

`harness/effects-corpus/01_core_origins/origins.rb:77-83` says "Mutating an
object the frame OWNS (fresh, unescaped) is `mutate.local`". The oracle reports
`effects: []`, `exhaustive: false`, `causes: [["unknown-ownership", null]]` — the
method's tail expression is a bare `buffer` read, and `LocalOwnership`'s
`trailing_reads` (`local_ownership.rb:122-126`) counts that as an escape, so the
local is not frame-owned. The fixture is *correct as recorded* (the self-test
passes); the comment describes a case it does not contain. `mutate.local` is
reachable — measured, `s = +""; s.upcase!; nil` → `construct:receiver-mutation`
→ `mutate.local` — and a `; nil` tail is the whole difference. Worth fixing when
slice 2 grows the corpus, because an implementer reading that comment will build
the wrong ownership rule.

### 7b. `mutate.instance` has no corpus coverage either, for a subtler reason

`Origins#mutates_its_argument(list)` is written as the `mutate.instance` case,
and measured it is `effects: []` + `dynamic-receiver`: `list` is an untyped
parameter, so `receiver_class` is nil, so `mutating?` declines `<<` (it is only a
mutator on a *known* Array/Hash/String, `mutation_classifier.rb:60-65`) and the
ownership question is never asked. `list[0] = 1` would reach it —
`UNIVERSAL_MUTATORS` claims `[]=` on every receiver.

### 7c. `lookup_with`'s slice-1 carve-out is an OVER waiting to happen

`crates/rigor-effects/src/catalog.rs:430-431` documents "a narrowed row answers
its UNNARROWED entry". That is correct for slice 1 and **fatal if slice 2
consumes it unguarded** (§ 2c). The mini-spec should require the slice-2 caller
to branch on `Row::narrow()` (`catalog.rs:189`) before reading `Entry::labels`,
and a test should pin that `File.open` / `Time.new` / `Random.new` / `URI.open` /
`OpenURI.open_uri` / `Pathname#open` / `Kernel#open` never reach the fallback by
accident.

---

## 8. Reproduction

```sh
# populate reference/rigor at the pin b10bd5df from the main checkout, then:
python3 harness/effects_diff.py --self-test          # 46 methods, all MATCH

# the oracle's JSON per corpus project (cache cleared either side)
for p in harness/effects-corpus/*/; do
  rm -rf "$p/.rigor/cache"
  (cd "$p" && ruby -I ../../../reference/rigor/lib \
     -I ../../../reference/rigor/plugins/rigor-rbs-inline/lib \
     ../../../reference/rigor/exe/rigor effects --full --format=json)
  rm -rf "$p/.rigor/cache"
done

# the mutator-set counts (§ 4e)
ruby -I reference/rigor/lib -e '
  require "rigor/inference/mutation_widening"; require "rigor/effects/mutation_classifier"
  puts "ARRAY=#{Rigor::Inference::MutationWidening::ARRAY_MUTATORS.size} " \
       "HASH=#{Rigor::Inference::MutationWidening::HASH_MUTATORS.size} " \
       "STRING=#{Rigor::Effects::MutationClassifier::STRING_MUTATORS.size}"'
# => ARRAY=31 HASH=15 STRING=26
```

The § 6 predictions and the § 2 trap verifications were produced by importing
`harness/effects_diff.py` and calling its own `compare()` against a synthesised
port output built from the oracle's `direct:` bundles:

```python
import importlib.util, json
spec = importlib.util.spec_from_file_location("ed", "harness/effects_diff.py")
ed = importlib.util.module_from_spec(spec); spec.loader.exec_module(ed)

def slice2(ref):                      # the IDEAL direct-only port
    return {k: {"effects": sorted({l for ls in (e["direct"] or {}).values() for l in ls}),
                "declared": [], "exhaustive": False}
            for k, e in ref.items()}

ref = json.loads(oracle_stdout)["methods"]
print(ed.compare(ref, slice2(ref)))
```

The two scratch projects behind § 4a / § 4c / § 4d and § 2d are `paths: [lib]`
plus one file each (`units.rb` — 47 units covering unit identity, every narrowing
handler shape and every construct; `posture.rb` — the posture / universal /
implicit-self controls). Both were removed after measurement.
