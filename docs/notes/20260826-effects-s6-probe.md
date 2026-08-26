# Effects slice 6 — the DIRECT half of the declared lane: a probe

2026-08-26. Investigation only, no production code. Everything measured against
the PINNED submodule at `v0.3.4` (`b10bd5df`), populated into this worktree from
the parent checkout's tree (never the network, never `REFERENCE_RIGOR_DIR`),
invoked as `ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib …`
from the project directory each measurement names, with `.rigor/cache` cleared
either side of every oracle run.

Subject: [ADR-0043](../adr/0043-effect-system-port-parity-model.md) slice 6's
DIRECT half — retiring the `"methods": {}` annotation self-defense
([`crates/rigor-cli/src/effects/mod.rs:227`](../../crates/rigor-cli/src/effects/mod.rs))
and reporting the `declared` lane. Builds on
[the slice-1 catalogue probe](20260826-effects-s1-catalogue-probe.md) § 7, whose
mechanism (`declared` is the CALLER's lane) is confirmed here and extended.

**Headline: the suppression cannot be retired, and it is currently too NARROW,
not too wide.**

- **A live defect, on the shipped binary, no `crates/` edit.** The self-defense
  covers two of the **four** declared-lane producers. A project with
  `effects.envelopes:` scores **1 DECLARED-MISMATCH**; a project with `plugins:`
  and no annotation of any kind scores **3** (§ 6). Both are FATAL verdicts on
  correct code, both are invisible to the standing gate, and both are the
  documented adoption path. This is the trap, and it points the opposite way
  from the arc's expectation.
- **The direct half needs the wall.** `envelope_target` is `catalog_target` with
  one substitution, so its third arm is `record.receiver_class` — the typer's.
  `t.fetch` on a receiver typed by the project's own `sig/` is the ORDINARY way
  to call an annotated method, and it imports a bound the port cannot see
  (§ 2b, probe `p_typed`). Only implicit-self and constant-path are in reach.
- **The render rule breaks on our UNDER proven lane, measured.**
  `rendered_declared = declared.excluding_subsumed_by(proven)` over the
  **transitive** lanes of both. `Trap#subsumed_by_transitive_proven` renders `[]`
  upstream and would render `['io.fs.read']` from a direct-only port (§ 3c). The
  declared lane travels the same edges, so `Trap#outer` / `Trap#outer_outer`
  break in the other direction. **Both directions are DECLARED-MISMATCH.**
- **The slice is not measurable.** Across the whole standing set at `--scale` —
  **36,167 oracle methods** — exactly **ONE** carries a non-empty declared lane:
  `04_declared`'s `Declared#load_and_log`. mastodon/app: **0**. gitlab-foss/lib:
  **0** (§ 5a). The direct half's entire prize is **+2 MATCH** (§ 5c).
- **There is no safe intermediate state.** Lane-absent == lane-empty and the
  lane is compared for exact equality, so "emit methods with an empty declared
  lane" is FATAL wherever the oracle has one — which is precisely the projects
  the feature exists for.
- **Recommendation: fix the self-defense (§ 6d), do not retire it (§ 7).**

---

## 1. Where envelopes come from — the parse surface, exactly

### 1a. The grammar, in two regexes

Both live in `RbsExtended` and nothing else decides meaning:

| | pattern | file:line |
|---|---|---|
| purity | `/\A\s*pure\s*\z/` | `rbs_extended.rb:637` |
| labelled | `/\Arigor:v1:effect(?:\s+(?<labels>.*?))?\s*\z/` | `rbs_extended.rb:644` |

`parse_effect_annotation` (`:681`) then splits the payload on `,` **with a −1
limit** (so a trailing comma yields an empty token), strips each, and:

- `tokens.empty? || any token fails Label::PATTERN` → **malformed** → `LabelSet::TOP`;
- any token the **registry** does not `known?` → **the WHOLE tag** degrades to
  `LabelSet::TOP`, not the recognised subset (`:694-697`) — the fail-open rule,
  stated in the method's own doc: narrowing to the recognised subset "would turn
  a typo into findings on correct code";
- otherwise `LabelSet.new(tokens)` — de-duplicated and **sorted**.

`read_effect_envelope` (`:714`) reads an annotation LIST: it finds `pure`
anywhere in the list, and separately takes the **first** annotation that matches
the labelled directive (`each … break`). `pure` wins over a labelled tag in
either source order, and the contradiction is recorded on the reporter.

**`EnvelopeIndex#[]` returns `nil` for a ⊤ envelope** (`envelope_index.rb:136`):
"a bound that bounds nothing is not a bound". So malformed, unknown-label and
absent all collapse to the same observable — no import, no discharge.

Measured, probe `p_parse` (25 callers, one per shape, `Ann` declared in `sig/`
only so no project edge can confound the reading — every `declared` below is an
import at the call site):

| annotation on the callee | caller's `declared` |
|---|---|
| `%a{rigor:v1:effect io.db}` | `["io.db"]` |
| `%a{rigor:v1:effect io.db, nondet.time}` | `["io.db","nondet.time"]` |
| `%a{rigor:v1:effect   io.db  ,   nondet.time  }` | `["io.db","nondet.time"]` |
| `%a{rigor:v1:effect nondet.time, io.db}` | `["io.db","nondet.time"]` — **sorted** |
| `%a{rigor:v1:effect io.db, io.db}` | `["io.db"]` — de-duplicated |
| `%a{rigor:v1:effect io}` | `["io"]` — an interior node is a legal bound |
| `%a{rigor:v1:effect global}` | `["global"]` — a **phantom root** (§ 1a of the slice-1 probe) |
| `%a{rigor:v1:effect mutate.local}` | `["mutate.local"]` — **not** dropped by the render rule |
| `%a{pure}` / `%a{ pure }` | `[]` — EMPTY bound; `add_declared` early-returns on an empty set (`unit_scan.rb:156`) |
| `%a{purely}` | `[]` — matched whole, so not `pure` |
| `%a{pure io}` | `[]` — likewise |
| `%a{rigor:v1:effect}` | `[]` — malformed → ⊤ |
| `%a{rigor:v1:effect io.db,}` | `[]` — the −1 split leaves an empty token → malformed |
| `%a{rigor:v1:effect IO}` | `[]` — outside the label grammar → malformed |
| `%a{rigor:v1:effect io.zzz}` | `[]` — unknown → ⊤ |
| **`%a{rigor:v1:effect io.db, io.zzz}`** | **`[]`** — one unknown label discards the KNOWN one too |
| `%a{rigor:v1:effects io.db}` | `[]` — not the directive |
| `%a{pure}` + `%a{rigor:v1:effect io.db}` | `[]` — pure wins |
| `%a{rigor:v1:effect io.db}` + `%a{pure}` | `[]` — pure wins in **either** order |
| `%a{rigor:v1:effect io.db}` + `%a{rigor:v1:effect nondet.time}` | `["io.db"]` — **first** labelled tag only |
| `%a{deprecated}` + `%a{rigor:v1:effect io.db}` | `["io.db"]` — an unrelated tag does not block |
| no annotation | `[]` |

Two callers in one body join (`["io.db","nondet.time"]`); the same callee twice
joins with itself (`["io.db"]`).

### 1b. The five strata an envelope can be WRITTEN in

`EnvelopeIndex.build` (`envelope_index.rb:68`) reads them all; `#[]` (`:130`)
consults them nearest-first:

    per-method annotation > class-level annotation > `effects.envelopes:` > accepted signature

1. **The project's own `.rbs`.** `SignatureSources.collect` globs
   `signature_paths:` (default `["sig"]`) and `EnvelopeScanner.scan` parses each
   with `RBS::Parser` — **not** the built environment, deliberately, so the
   diagnostic can name `sig/foo.rbs:12` (`envelope_scanner.rb:22-31`). Keys are
   `Class#m` / `Class.m`; **`def self?.x` declares BOTH** (`:149-155`); a nested
   `class Bar` inside `module Foo` keys `Foo::Bar` (`:118-123`). An unparseable
   `.rbs` contributes nothing (`:111-116`), and the whole scan is
   `rescue StandardError → EMPTY` (`:66`).
2. **rbs-inline `# @rbs %a{…}` in the RUBY source**, arriving as the loader's
   `virtual:<plugin-id>:<path>` buffers. Measured live at the pin, probe
   `p_inline`: `# @rbs %a{rigor:v1:effect io.db}` above a `def` gives the caller
   `["io.db"]`. **The plugin is AUTO-WIRED** — `configuration.rb:308`, ADR-93
   WD2, no `plugins:` entry needed, gated only on `rbs-inline` being resolvable.
3. **A CLASS-level annotation**, on the `class` / `module` declaration. It
   distributes to every method of the class, and in `EnvelopeIndex` that is
   literally `@class_envelopes[owner]` — **consulted for ANY selector**, whether
   the class declares it or not. Measured (`p_class`, class-level
   `%a{rigor:v1:effect io.db}` on `Wide`):

   ```
   Wide#a               decl=['io.db']   # `b`, a real method
   Wide#core_self_call  decl=['io.db']   # `puts` — an implicit-self CORE call
   Wide#ghost_self_call decl=['io.db']   # a selector NOTHING defines
   Wide#const_receiver  decl=[]          # `File.read` — the carrier is `File`
   Wide#calls_nearest   decl=['nondet.time']  # per-method annotation wins
   ```

   The fan-out is total: one class-level tag colours every implicit-self call
   site in the class.
4. **`effects.envelopes:` in `.rigor.yml`.** `namespace:` entries participate;
   **`match:` entries deliberately do NOT** (`envelope_index.rb:111` rejects
   every entry with a nil `namespace`) — a path glob needs the whole-project
   class-source table a per-file window cannot see. `namespace_match?` is
   segment-aware, hand-rolled, not `File.fnmatch` over a `/`-substituted name
   (`config_envelopes.rb:143`): `Ns::*` is exactly one segment, `Ns::**` is one
   or more, neither matches bare `Ns`. First matching entry in file order wins;
   no merging. An unknown label in `effect:` degrades that entry to ⊤. Measured
   (`p_config`):

   ```
   Ns::Sel#a             decl=['io.db']              # namespace: "Ns::*"
   Ns::Deeper::Far#a     decl=[]                     # `Ns::*` is ONE segment
   Deep::Nested::Empty#a decl=[]                     # `effect: []` — empty bound
   UserPresenter#a       decl=[]                     # match: — NOT in the index
   ```
5. **The ACCEPTED stratum** — every annotated method member of the **built** RBS
   environment: gem-shipped signatures, Rigor's bundled overlays, core RBS
   (`envelope_scanner.rb:92`, `from_loader`). Read-only, never checkable, and
   **class-level annotations are deliberately not read here**. At the pin the
   only `%a{…}` in the installed `rbs-4.1.1` core/stdlib is one `%a{pure}` in
   `core/regexp.rbs` (an EMPTY bound → no declared label, but it DOES discharge
   the site). The non-empty ones live in plugin RBS —
   `plugins/rigor-activerecord/sig/active_record/relation.rbs` carries
   `%a{rigor:v1:effect io.db.read}` on 18 selectors. **This stratum is
   environment-dependent**, the same hazard as `UNBUILDABLE_DEFINITIONS`.

Lookup is by the **exact** owner, never an ancestor walk: an envelope on
`Base#m` is not imported at a call keyed `Sub#m` (`envelope_index.rb:38-39`).

### 1c. And two producers that are NOT envelopes at all

`add_declared` has three call sites, plus one synthesised-unit path:

| # | producer | file:line | keyed by |
|---|---|---|---|
| 1 | `import_envelope` — the five strata above | `unit_scan.rb:337` | **`envelope_target`** |
| 2 | `attribute` — the project's `effects.attribution:` table | `unit_scan.rb:370` | **`catalog_target`** |
| 3 | `attribute_plugin` — a loaded plugin's effect rows | `unit_scan.rb:258` | receiver path / class / result |
| 4 | `FrameworkUnits` — plugin-synthesised units born with a declared lane (ActiveRecord's uniqueness validator, `io.db.read`) | `framework_units.rb:155` | — |

(2) is keyed by `catalog_target`, **not** `envelope_target`, so an implicit-self
call spells `Kernel#<selector>` there. Measured: an `attribution:` row for
`"Kernel#puts"` colours every bare `puts` in the project
(`Conf#attributed_kernel decl=['vendor.thing']`). Attribution labels are
grammar-checked at load (a malformed one raises) but **not** registry-checked,
and an attributed site additionally keeps a `plugin-attribution` taint — the
table never discharges.

`effects.labels:` extends the vocabulary an annotation and an attribution are
both read against (`registry.rb:58`).

### 1d. The declared lane's ORIGINS are invisible in the report

`EffectsReport::Row#direct` is `entry.direct.**bundles**` — the PROVEN bundles
only (`effects_report.rb:43`). `declared_bundles` never surfaces. Confirmed on
`p_parse`: every caller shows `direct: {}` beside a non-empty `declared`. So a
port owes the labels and not the `envelope:` / `attribution:` / `plugin:` origin
spellings — a real simplification.

---

## 2. Direct vs transitive — the verdict

### 2a. `envelope_target` is `catalog_target` with one substitution

```ruby
def envelope_target(node, record)
  owner, singleton, implicit = catalog_target(node, record)
  return [@owner_class, @singleton] if implicit
  [owner, singleton]
end
```

(`unit_scan.rb:345-350`), over `catalog_target` (`:553-562`):

| receiver | `catalog_target` | `envelope_target` | port has it? |
|---|---|---|---|
| nil or `self` | `["Kernel", false, implicit]` | **the unit's own owner class + its singleton flag** | **yes** |
| a constant path | `[constant, !object_constant?(constant), false]` | same | **yes** |
| anything else | `[record.receiver_class, record.kind == :singleton, false]` | same | **NO — the typer** |

The first two arms are exactly the pair slice 2 already implements
(`20260826-effects-s2-impl.md` § 2.4, "implicit self, constant path"). The third
is the wall.

### 2b. The wall is not an edge-set wall — it is worse, and it bites the COMMON case

Slice 4's wall was "upstream's edge set is its typer's traversal". This one is
narrower and more damaging: the declared IMPORT needs the receiver's *static
class*, and that is the ordinary way an annotated method is called. Probe
`p_typed` — `T#fetch` annotated `%a{rigor:v1:effect io.db}` in `sig/`, `T`
declared in `sig/` only so no project edge exists:

```
Callers2#via_param    (t)   t.fetch          decl=['io.db']   <- sig-typed parameter
Callers2#via_new            t = T.new; t.fetch  decl=['io.db'] <- typed local
Callers2#via_builder        T.build.fetch    decl=['io.db']   <- typed call result
Callers2#via_untyped  (t)   t.fetch          decl=[]          <- Dynamic, no facet
Callers2#via_const          T.fetch          decl=[]          <- constant path keys `T.fetch` (SINGLETON), and the sig declares `T#fetch`
```

Three of the five rows are the typer arm. The one row the port's syntax reaches
(`via_const`) is the one that **misses**, because a constant-path receiver keys
as a singleton — which is right, and is the shape a port would most easily get
wrong.

### 2c. …and the lane is transitive anyway

`Propagator#absorb` moves `declared` along every edge with a plain `join`
(`propagator.rb:137`; the slice-4 probe § 1c enumerates all five lanes it moves
in ONE pass). Measured, `p_render`:

```
Trap#calls_annotated  decl=['io.db']   # the import
Trap#outer            decl=['io.db']   # 1 hop above it
Trap#outer_outer      decl=['io.db']   # 2 hops
```

and independently in `p_class`, `Narrow#s` (a subclass calling an inherited
`Wide#a`) shows `['io.db']` although **`Narrow` has no envelope in the index at
all** — verified by dumping `EnvelopeScanner.scan`, which lists only
`Wide#nearest`, `Both#dual`, `Both.dual`, `Reopened#in_sig` and the two class
envelopes. It arrives purely by propagation through the ancestry-resolved edge.

**Verdict on Q2.** The port's constant-path + implicit-self scope is enough for
`04_declared`'s four methods — and for nothing else the corpus can be shown to
contain, because the corpus contains nothing else (§ 5a). The cases out of reach
are named exactly: (i) every call on a typed non-constant receiver; (ii) every
method one hop or more above an importing call site; (iii) every method whose
proven lane acquires, transitively, a label that subsumes one of its own
directly-declared ones (§ 3c).

---

## 3. The render rule

### 3a. Exact semantics

`EffectTable::Entry#rendered_declared = declared.excluding_subsumed_by(proven)`
(`effect_table.rb:41-43`), and this is the ONLY thing `EffectsReport` prints
under the `declared` key (`effects_report.rb:40`). Both operands are the
**transitive** fields of the entry — the lanes are kept raw in the table on
purpose, "because a further join has to see what was actually declared"
(`effect_table.rb:30-31`).

`LabelSet#excluding_subsumed_by(other)` (`label_set.rb:100-107`):

```ruby
return self if @top || other.empty?
kept = @labels.reject { |label| other.admits?(label) }
return self if kept.length == @labels.length
kept.empty? ? EMPTY : self.class.new(kept)
```

- `other.admits?(l)` is `@labels.any? { |m| Label.subsumes?(m, l) }` — **segment-aware
  prefix subsumption**, already ported as `crates/rigor-effects`'s `Label`.
- **`other.empty?` short-circuits**: a proven lane of ∅ drops nothing. `TOP` on
  the left returns self; `TOP` on the right admits every well-formed label and
  so drops everything (unreachable here — a ⊤ envelope is never imported).
- **The relation is asymmetric and the direction matters.** A proven ANCESTOR
  drops a declared descendant; a proven DESCENDANT does **not** drop a declared
  ancestor.
- `mutate.local` gets no special treatment. `Envelope#tolerates?` adds
  `TRIVIAL_BOUND`, but that is the CHECK, not the render.

Measured, `p_render` (direct proven in every row):

| method | proven | declared raw | rendered |
|---|---|---|---|
| `exact_direct` | `io.fs.read` | `io.fs.read` | `[]` |
| `proven_ancestor_direct` | `io.fs` | `io.fs.read` | `[]` — ancestor admits descendant |
| `declared_ancestor_direct` | `io.fs.read` | `io.fs` | `["io.fs"]` — **kept** |
| `unrelated_direct` | `io.output.stdout` | `io.db` | `["io.db"]` |
| `partial_direct` | `io.fs.read` | `io.fs.read`,`io.db` | `["io.db"]` |

### 3b. It also decides whether the row is printed at all

`Entry#trivial?` is `exhaustive && proven ⊆ TRIVIAL_BOUND && rendered_declared.empty?`
(`effect_table.rb:52-54`). The differential always runs `--full`, so this cannot
bite the gate — but a method whose only content is a declared label is NOT
trivial and appears in the default report and in the snapshot. Slice 5 owes it.

### 3c. **Yes — our UNDER proven lane breaks it. Measured.**

The port's `effects` key is the DIRECT proven lane (slice 4 never shipped:
`crates/rigor-cli/src/effects/mod.rs:17-19`). So the port would compute
`declared_direct \ subsumed_by(proven_direct)` where upstream computes
`declared_trans \ subsumed_by(proven_trans)`. Since `proven_direct ⊆ proven_trans`
the port **subsumes less and renders more**, and the extra label is a
DECLARED-MISMATCH. `p_render`'s `Trap`:

```ruby
class Trap
  def leaf                             = File.read("x")     # proves io.fs.read
  def subsumed_by_transitive_proven
    leaf                               # transitive io.fs.read
    Ann.fs_read                        # declared io.fs.read
  end
end
```

```
oracle:  Trap#subsumed_by_transitive_proven  eff=['io.fs.read']  decl=[]   direct={}
port:    would render                        eff=[]              decl=['io.fs.read']
```

Two independent fatal errors in one row: the port renders a declared label
upstream dropped, **and** the port's own proven lane is empty because the
subsuming label is exactly the one propagation supplies.

And the mirror, from § 2c: `Trap#outer` has `decl=['io.db']` upstream and `[]`
from a direct-only port. **The declared lane has no safe direction, and a
direct-only implementation is wrong in BOTH of them.**

---

## 4. Can the suppression be retired, and in what order?

### 4a. What retiring it costs today, measured

`04_declared` with its annotations stripped from `sig/` (so the suppression does
not fire, and the port's answer is byte-identical to what it would print for the
real fixture with the self-defense lifted), graded against the oracle's run on
the REAL annotated fixture:

| method | oracle | port today | verdict |
|---|---|---|---|
| `Declared#formats` | `[] / [] / ex` | `[] / [] / ex` | MATCH |
| `Declared#load_row` | `[] / [] / ex` | `[] / [] / ¬ex` | UNDER extra-taint |
| `Declared#load_and_log` | `[io.output.stdout] / **[io.db]** / ex` | `[io.output.stdout] / **[]** / ¬ex` | **DECLARED-MISMATCH** |
| `Declared#unannotated` | `[] / [] / ex` | `[] / [] / ex` | MATCH |

`2 MATCH / 1 UNDER / 1 DECLARED-MISMATCH → FAIL`. The self-defense is load-bearing
exactly as slice 2 designed it.

### 4b. The intermediate state is NOT safe

"Emit methods but an empty declared lane" is what the table above IS. `lanes()`
reads a missing key as ∅ (`effects_diff.py:290`) and `compare()` tests
`sd != rd` (`:320`), so **lane-absent == lane-empty and both are fatal against a
non-empty oracle lane**. There is no direction to be conservative in: the only
safe answer for a method whose oracle lane the port cannot compute is **not to
report the method at all**.

### 4c. The safe sequence

**Step 0 — WIDEN the self-defense (a defect fix, not slice 6).** § 6 measures
two live DECLARED-MISMATCH sources the current predicate misses. Until they are
covered the port is not merely under-claiming, it is emitting the fatal verdict.
Free on the standing gate, which is itself the finding.

**Step 1 — replace the LEXICAL test with a PARSED one.** Build the envelope
index for real (§ 1b strata 1, 3 and 4 — `sig/` `.rbs`, class-level, and
`effects.envelopes: namespace:`), and suppress on "the index holds at least one
**non-empty, non-⊤** bound, or the config holds an `attribution:` table, or the
config holds `plugins:`". This is strictly more precise than the line scan in
both directions:

- a project whose annotations are all `%a{pure}`, malformed or unknown-label has
  an **all-empty declared lane** — the empty bound never reaches `add_declared`
  (`unit_scan.rb:156`) and a ⊤ bound is never imported
  (`envelope_index.rb:136`) — and is reportable today. Measured, probe
  `p_pureonly` (`%a{pure}` ×2, `%a{rigor:v1:effect}`, `%a{rigor:v1:effect io.zzz}`):
  the oracle reports **6 methods, every one `declared: []`**, and the shipped
  port scores `MATCH=0 UNDER=6 OVER=0 DM=0`, all six `absent-method`. Six
  methods withheld for nothing, and `%a{pure}` is the ecosystem's existing
  purity spelling that rbs core and Steep both carry — and that rigor-rs's own
  vendored plugin RBS uses;
- conversely a `.rb` file whose PROSE contains `%a{pure}` suppresses the whole
  project today for nothing.

Step 1 needs an RBS annotation reader the port does not have:
`crates/rigor-index/src/rbs.rs` (5,934 lines) contains **no** `%a{…}` handling
at all. It does not need the rbs-inline stratum, because a project carrying
`# @rbs %a{…}` still trips the line scan over `.rb` sources — which must
therefore be KEPT as a second, coarser gate, since **rigor-rs has no rbs-inline
reader of any kind** (the only occurrence of the string in `crates/` is the
self-defense's own comment).

**Step 2 — the direct import, behind a per-method exactness gate.** Report a
method `M` only when the port can PROVE its two lanes are already transitive.
The sufficient condition is cheap and does not need the edge set:

    reach⁺(M) = transitive closure of { project unit U : U's selector appears as a
                call selector in M's body }

`reach⁺ ⊇ reach` because every upstream edge `M → N` comes from a call node in
`M` whose selector is `N`'s (`push_edge`, `unit_scan.rb:514`, and
`Index#targets_for` only ever resolves the same selector). The port already
computes this selector set — it is slice 3's stand-in (`collect.rs:257`) — and
its unit table already includes the `attr_*` / `define_method` synthesised units
`targets_for` can land on (`collect.rs:414-437`). Then `M` is reportable when

1. `⋃_{N ∈ reach⁺(M)} declared_direct(N) ⊆ declared_direct(M)`, and
2. `⋃_{N ∈ reach⁺(M)} proven_direct(N) ⊆ proven_direct(M)`, and
3. no call site in `M` is in the typer arm with a selector the index could
   answer — i.e. if the index holds any class-level or `namespace:` bound then
   NO unhandled-receiver call is safe; otherwise only calls whose selector
   appears in the index's per-method key set are unsafe.

All four of `04_declared`'s methods pass this gate (`load_and_log`'s reach⁺ is
`{Declared#load_row}`, whose direct lanes are both ∅).

**Step 3 — blocked.** The transitive declared lane rides `absorb`'s edge set,
which is the slice-4 wall. It is *strictly harder* than slice 4: there, an
edge-set mismatch cost OVER only, and 0 OVER was already unreachable
(111 / 29 / 5 on gitlab-foss/lib, `20260826-effects-s4-probe.md` § 5c). Here
BOTH directions are fatal, so the declared lane needs **exact** edge-set parity,
and mastodon/app alone has 48 extra and 534 missing edges.

**Do NOT implement the envelope's taint DISCHARGE.** An imported bound
suppresses `unresolved-self-call` (`unit_scan.rb:509`) and `dynamic-receiver`
(`:457`). Measured: `Wide#ghost_self_call` calls a selector nothing defines and
is `exhaustive: true`. Implementing the lane without the discharge leaves those
rows UNDER (safe); implementing the discharge is an exhaustiveness claim, which
is the fatal direction.

---

## 5. The predicted verdict table

### 5a. How many methods carry a declared lane at all — the decisive number

Oracle run over the standing set, counting methods with a non-empty `declared`:

| project | oracle methods | with a declared lane |
|---|---|---|
| `01_core_origins` | 16 | 0 |
| `02_propagation` | 15 | 0 |
| `03_taint` | 11 | 0 |
| **`04_declared`** | 4 | **1** (`Declared#load_and_log`, `["io.db"]`) |
| `05_posture` | 133 | 0 |
| `06_edge` | 6 | 0 |
| `07_mutators` | 383 | 0 |
| `08_resolved` | 44 | 0 |
| **`mastodon/app`** | 6,948 | **0** |
| **`gitlab-foss/lib`** (`--scale`) | 28,607 | **0** |
| **total** | **36,167** | **1** |

Neither real corpus contains the substring `%a{` anywhere, and the synthesised
config the instrument writes carries `paths:` and nothing else — no `plugins:`,
no `effects:` — so no producer can fire there. **The slice is not measurable
outside `04_declared`'s four methods.** The instrument that closed slice 4's
vacuity (`20260826-s112-effects-instrument.md`) does not close this one, and no
corpus on this machine would: adoption of the feature is what creates the lane,
and nothing has adopted it.

### 5b. Baseline at this tree (unchanged; re-measured)

```
TOTAL  MATCH=5565  UNDER=1995  OVER=0  DECLARED-MISMATCH=0     RESULT: PASS
```
per project `01` 16/0 · `02` 10/5 · `03` 9/2 · `04` **0/4** · `05` 61/72 ·
`06` 5/1 · `07` 219/164 · `08` 28/16 · `mastodon/app` 5,217/1,731.

### 5c. Predicted, with the direct half of § 4c step 2 shipped

Every project except `04_declared` is bit-identical — no other project has an
annotation, a config producer or a plugin, so nothing enters the lane and
nothing is withheld.

| project | today | with the direct half | verdict |
|---|---|---|---|
| `01_core_origins` | 16/0/0/0 | 16/0/0/0 | PASS |
| `02_propagation` | 10/5/0/0 | 10/5/0/0 | PASS |
| `03_taint` | 9/2/0/0 | 9/2/0/0 | PASS |
| **`04_declared`** | **0/4**/0/0 | **2/2**/0/0 | PASS |
| `05_posture` | 61/72/0/0 | 61/72/0/0 | PASS |
| `06_edge` | 5/1/0/0 | 5/1/0/0 | PASS |
| `07_mutators` | 219/164/0/0 | 219/164/0/0 | PASS |
| `08_resolved` | 28/16/0/0 | 28/16/0/0 | PASS |
| **`mastodon/app`** | 5,217/1,731/0/0 | 5,217/1,731/0/0 | PASS |
| **TOTAL** | 5,565/1,995/0/0 | **5,567/1,993**/0/0 | PASS |
| `gitlab-foss/lib` (`--scale`) | 20,990/7,617/0/0 | unchanged | PASS |

Cells are `MATCH / UNDER / OVER / DECLARED-MISMATCH`. **The whole slice is worth
+2 MATCH out of 36,167 methods**, and the two rows that move are
`Declared#formats` and `Declared#unannotated` — the ones with NO annotation,
which move only because the project stops being withheld. The one method that
actually exercises the lane, `load_and_log`, stays UNDER on the taint bit.

`04_declared` cannot reach 4/0: `load_row`'s port-side `dynamic-receiver` taint
on `ROWS.fetch` is a slice-2 constant-receiver gap unrelated to envelopes, and
`load_and_log`'s is the discharge § 4c declines.

---

## 6. The trap — the self-defense is too NARROW, and it is a live defect

Every slice in this arc had one. This one points the opposite way from the
expectation in the brief: § 3's render-rule hazard is real (§ 3c) but it is
*prospective*. The trap is **already shipped**.

`carries_effect_annotations` (`mod.rs:227`) tests exactly two things — the
`ANNOTATION_HINT` line scan over `sig/**/*.rbs` + the project's `.rb` sources,
and `effects.attribution:` in the config. That covers producer 1's *annotation*
surfaces and producer 2. It covers **neither producer 3/4 nor producer 1's
config stratum**.

### 6a. `effects.envelopes:` — 1 DECLARED-MISMATCH

`.rigor.yml` with `effects: {envelopes: [{namespace: "Svc::*", effect: [io.db]}]}`,
no annotation anywhere, two methods (probe `p_envcfg`), graded with the
harness's own `compare()` against the shipped `target/release/rigor`:

```
oracle=2  rigor-rs=2
MATCH=1 UNDER=1 OVER=0 DECLARED-MISMATCH=1
  DECLARED-MISMATCH: Svc::Repo#find — declared lane [] != oracle ['io.db']
```

`config_declares_attribution` reads `effects.attribution` and nothing else
(`mod.rs:255-262`), so the suppression never fires.

### 6b. `plugins:` — 3 DECLARED-MISMATCH

`.rigor.yml` with `plugins: [rigor-activesupport-core-ext]`, no annotation, no
`effects:` block at all, four methods calling `Time.current` / `Date.current` /
`Time.zone` (probe `p_plugin`, the reference arm additionally given the plugin's
`lib` on `-I` so `plugins:` resolves):

```
oracle=4  rigor-rs=4
MATCH=0 UNDER=4 OVER=0 DECLARED-MISMATCH=3
  DECLARED-MISMATCH: Clock#now   — declared lane [] != oracle ['global.read', 'nondet.time']
  DECLARED-MISMATCH: Clock#today — declared lane [] != oracle ['global.read', 'nondet.time']
  DECLARED-MISMATCH: Clock#zone  — declared lane [] != oracle ['global.read']
```

A plugin row's labels go to the DECLARED lane, always
(`unit_scan.rb:255-258` — `add_declared` runs whether or not the row discharges).
Nine bundled Rails plugins ship `effect_attributions:`, and
`FrameworkUnits.uniqueness_summary` additionally synthesises whole units born
with `io.db.read` in the lane. The port's plugin self-defense
(`configures_plugins`, `mod.rs:292`) withholds only the **exhaustiveness bit**
and says so in its own doc comment — "deliberately NARROWER than the annotation
self-defense" — which is correct for the bit and wrong for this lane.

### 6c. Why the standing gate cannot see either

No fixture under `harness/effects-corpus/` carries `plugins:`, `effects:` or
`envelopes:` in its `.rigor.yml` — grepped, zero hits across all eight. Both
real corpora run against a synthesised minimal config by construction
(`effects_diff.py:364-374`, and the comment says why: "so the two arms differ in
nothing but the engine under test"). The very normalisation that removed a
confound removed the only shape that exercises producers 3 and 4. **This is the
fixture-corpus blind spot on a new axis: not a shape the authors did not think
of, but a shape the instrument deliberately erases.**

### 6d. The fix, and its cost

Widen the predicate to the union of the producers:

```
suppress ⟺  ANNOTATION_HINT over sig/ + .rb        (producer 1, annotations)
         ∨  effects.attribution: present            (producer 2)
         ∨  effects.envelopes:   present            (producer 1, config)
         ∨  plugins:             non-empty          (producers 3 + 4)
```

Cost on the standing set: **zero** — no member trips any of the three new arms.
That is precisely why it needs fixtures shipped WITH it: a `09_declared_config`
project per new arm, each an oracle-graded reproducer of § 6a / § 6b, or the
widening is another gate that cannot fail. `configures_plugins` already exists
and is already computed; the `envelopes:` arm is three lines beside
`config_declares_attribution`.

Note the direction this forces, because it contradicts the standing decision at
face value: for the declared lane, matching the reference means **more**
suppression, not less. The two are only reconciled by ADR-0043 § 2's exact-match
rule — silence is the sole sound answer, and the port is currently NOT silent
where it must be.

---

## 7. Recommendation

1. **Ship § 6d now, as a defect fix with fixtures.** It is small, it is
   measured, and it is the only part of this slice that is unambiguously right
   today. Everything else in slice 6 is a coverage question; this is a
   correctness one.
2. **Do not retire the suppression.** The direct half is worth **+2 MATCH on
   36,167 methods**, needs an RBS annotation reader the port does not have, and
   is wrong in both fatal directions on any project that calls an annotated
   method the ordinary way (§ 2b) or from one hop up (§ 2c). The `04_declared`
   fixture is the only thing on this machine that would grade it, and it is four
   methods the port's own authors wrote.
3. **If the arc wants the lane anyway, take § 4c step 1 alone.** Replacing the
   line scan with a parsed index is a real coverage win with **no** declared-lane
   risk — it un-suppresses `%a{pure}`-only and malformed-only projects, which
   import nothing (measured: 6 methods on `p_pureonly`, all `declared: []` on
   the oracle side, all withheld today) — and it is the prerequisite for step 2
   whenever step 3 unblocks. It also removes the false-positive suppression a
   `%a{` in prose causes today.
4. **Correct the `04_declared` fixture header.** `sig/declared.rbs:9-20` still
   carries the OPEN QUESTION and its four refuted hypotheses; the slice-1 probe
   § 7 answered it and this note confirms every clause. A future implementer
   reading that comment will build the wrong lane. Same for ADR-0043's "Open at
   accepted" section.
5. **Do not reach for rigor-rs's Typer.** Fourth refutation in this arc. Here it
   would not even help in the safe direction: the third `envelope_target` arm
   needs upstream's *own* projection of the receiver class, and a different
   projection that happens to name a class with an envelope is a
   DECLARED-MISMATCH, not an under-claim.

---

## 8. Gates at this tree

No `crates/` change was made — the § 4a and § 6 measurements all use the SHIPPED
`target/release/rigor`, which is why the two live DECLARED-MISMATCH sources are
facts about master and not about a variant build.

| gate | verdict |
|---|---|
| `cargo test --workspace` | **PASS** — 1,271 passed, 0 failed |
| `harness/effects_diff.py` (default set) | **PASS** — 5,565 / 1,995 / 0 / 0, matching the recorded baseline to the digit |
| `harness/docs_check.py` | **PASS** |
| residue under `mastodon/app` and `gitlab-foss/lib` | none; no `rigor-effects-*` temp project survives |

---

## 9. Reproduction

```sh
# populate reference/rigor at b10bd5df from the parent checkout, then:
python3 harness/effects_diff.py --self-test
cargo build --offline --release -p rigor-cli
python3 harness/effects_diff.py            # MATCH=5565 UNDER=1995 OVER=0 DM=0
```

Everything else came from nine session-scoped scratch projects, all `paths:
[lib]`, all removed after measurement: `p_parse` (the § 1a grammar table, 25
callers), `p_render` (§ 3a + the § 3c trap), `p_class` (class-level
distribution, `def self?.x`, subclass, nested module), `p_config`
(`attribution:` / `envelopes:` `namespace:` vs `match:` / `labels:`), `p_inline`
(the rbs-inline stratum), `p_typed` (the § 2b typer arm), `p_pureonly` (§ 4c
step 1's coverage claim), `p_envcfg` (§ 6a) and `p_plugin` (§ 6b), plus
`p_04_stripped` (a copy of `04_declared` with the
annotations removed from `sig/`, which is how § 4a reads the port's answer
without editing `crates/`). Two scratch drivers, also removed: an oracle wrapper
that clears `.rigor/cache` either side of the run, and a grader that feeds a
live oracle run and the shipped binary's JSON to `effects_diff.compare()`.

The declared-lane census of § 5a is `effects_diff.resolve_targets` +
`run_ref`, counting `len([m for m in ref.values() if m["declared"]])` per
project.
