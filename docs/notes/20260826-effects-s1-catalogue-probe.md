# Effects slice 1 — the vendored effect catalogue: a probe

2026-08-26. Investigation only, no production code. Everything below is measured
against the PINNED submodule at `v0.3.4` (`b10bd5df`), invoked as
`ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib
reference/rigor/exe/rigor …` (UPSTREAM.md hazard 1) from the project directory
each measurement names.

Subject: [ADR-0043](../adr/0043-effect-system-port-parity-model.md) slice 1 —
"vocabulary + the vendored effect catalogue (`data/effects/registry.yml`,
`core.yml`), as a pin-tracking surface with a PROVENANCE and a re-sync step",
gated on "catalogue parses; label subsumption unit-tested" (ADR-0043 § 5, the
slice table).

Headline results:

- The catalogue is **two hand-written YAML files, 910 lines / 55 KB total**, and
  the vendorable part is genuinely all data — except **three references into
  Ruby code** the data deliberately does not re-spell.
- Slice 1 should follow the **`vendor/plugins/` precedent, not the `vendor/rbs/`
  one**: two files, direct `include_str!`, no `build.rs`, no new dependency
  (`serde_yaml` is already in `rigor-cli`).
- `effects_diff.py` grades **6 catalogue rows out of 420** today. It cannot be
  the drift gate; a byte-level re-sync check must ship with the vendored copy.
- Two slice-0 defects found, both in `harness/effects_diff.py` / the corpus (§ 8).
- The standing OPEN question — **when the oracle populates `declared:` — is
  SOLVED** (§ 7), by a mechanism none of the four refuted hypotheses named.

---

## 1. The catalogue at the pin — inventory

Upstream separates the **label taxonomy** from the **per-method catalogue**, and
they are two files with two loaders.

### 1a. `data/effects/registry.yml` — the vocabulary (67 lines, 2,217 bytes)

`sha256 bb0eb3f08568bc52c47ce3caa75d22d359b0455b3182825906884797289d7104`

Three keys, and that is the whole file: `vocabulary: 1`
(`data/effects/registry.yml:12`), `labels:` (`:14`) and `retired: {}` (`:67`).
Loaded by `Rigor::Effects::Registry.load_file`
(`lib/rigor/effects/registry.rb:71`, `DATA_PATH` at `:31`).

- **36 declared labels**, in four commented groups: Steins v1 verbatim (25),
  Ruby's `mutate` leaves (3), proposed shared `io.db` leaves (3), application
  roots (5).
- **`retired:` is empty at vocabulary 1** — the rename/removal compatibility
  table, mechanism-present and data-empty.
- **10 roots**: `cache email exit ffi global io job mutate nondet telemetry`.

The load-bearing subtlety, measured against the pinned loader:

```
known?("global")     = true   declared_row = false
known?("global.read")= true   declared_row = true
known?("io.smtp")    = false  declared_row = false
```

`Registry#known?` is **declared rows ∪ every ancestor of a declared row**
(`registry.rb:102` + `build_known` at `:161`). Four of the ten roots — `global`,
`email`, `job`, `cache` — exist ONLY as implied ancestors; no row spells them.
This is not academic: `core.yml`'s `global` posture (`defaults: global: [global]`,
`core.yml:48`+) emits the bare `global` label, so **a port that validates the
catalogue against the 36 declared rows alone rejects the shipped catalogue.**
Upstream's own data spec asserts exactly this pairing
(`spec/rigor/effects/catalog_data_spec.rb:28-36` requires `registry.known?`, not
membership).

The grammar and the relation are `lib/rigor/effects/label.rb`: `PATTERN`
(`:16`) is `[a-z][a-z0-9]*(\.[a-z][a-z0-9]*)*`, and `subsumes?` (`:39`) is
**segment-aware prefix subsumption** — `io` admits `io.net.http`, `io` does NOT
admit `iota`. That is the "label subsumption" slice 1's gate names.

### 1b. `data/effects/core.yml` — the per-method catalogue (843 lines, 52,785 bytes)

`sha256 85778dd3433fcb5561a933c9b2b22fb07048af980e35f93091f545655bda9c31`

Loaded by `Rigor::Effects::Catalog.load_file`
(`lib/rigor/effects/catalog.rb:122`, `DATA_PATH` at `:39`), memoised per process
(`:115`). Header: `schema: 1` (`core.yml:37`), `vocabulary: 1` (`:38`).

Measured shape (parsed with PyYAML; `!!str "<<"` survives `safe_load`, which is
why the file tags it — see the row-grammar comment at `core.yml:26-29`):

| thing | count |
|---|---|
| `defaults:` postures (`:48`) | **14** — `value world fs net ipc http process signal global nondet ffi stdout stderr stdin` |
| `universal:` selectors (`:69`) | **34** |
| `classes:` (`:105`) | **80** |
| instance rows (`methods:`) | **216** |
| singleton rows (`singleton_methods:`) | **204** |
| **total rows** | **420** |
| rows with an explicit `effects: []` (∅ ≠ "no row") | **77** |
| rows with `mutates: receiver` | **8** |
| rows with `narrow:` | **7** |
| classes with `kind: object` | **5** (`ENV ARGF STDIN STDOUT STDERR`) |
| classes naming a `mutators:` set | **3** (`Array`→array, `Hash`→hash, `String`→string) |
| classes carrying `singleton_posture:` | **1** (`Kernel`, → `value`) |
| distinct labels used by rows/postures | **19 of 36** |

Posture distribution over the 80 classes: `value` 46, `net` 12, `fs` 4,
`process` 3, `global` 3, `world` 2, `ipc` 2, `stdin` 2, `http`/`signal`/`nondet`/
`stdout`/`stderr`/`ffi` 1 each.

The 17 registry labels **no** `core.yml` row uses are not dead: `mutate.self` /
`mutate.instance` / `mutate.local` come from the construct + ownership path, the
`io.db.*` / `job.enqueue` / `email.send` / `cache.*` set comes from the plugin
layer, and `io` / `mutate` / `nondet` / `io.net` / `io.output` are interior
bounds an envelope may name.

### 1c. The three references into Ruby code — the reason this is not a pure data vendor

The data file deliberately does not re-spell three things. A vendored copy of the
two YAML files is **incomplete** without all three:

1. **Mutator sets, by reference.** `mutators: array | hash | string` resolves
   through `Catalog::MUTATOR_SETS` (`catalog.rb:43`) to
   `MutationWidening::ARRAY_MUTATORS` (**31** selectors, measured,
   `lib/rigor/inference/mutation_widening.rb:75`), `HASH_MUTATORS` (**15**,
   `:87`) and `MutationClassifier::STRING_MUTATORS` (**26**,
   `lib/rigor/effects/mutation_classifier.rb:31`). The internal spec makes this
   normative: "The data file MUST NOT re-spell a selector list"
   (`docs/internal-spec/effect-summaries.md:169`).
2. **Narrowing handlers.** `narrow:` names a handler in
   `Effects::Narrowing::HANDLERS` (`lib/rigor/effects/narrowing.rb:55`), which
   declares **7**: `kernel_open file_open pathname_open time_new random_new
   uri_open sql_verb`. `core.yml` uses **6** of them across 7 rows —
   `Kernel#open`, `File.open`, `Pathname#open`, `URI.open`, `OpenURI.open_uri`,
   `Time.new`, `Random.new`. **`sql_verb` has no `core.yml` row at all**: it
   serves PLUGIN rows for `connection.execute` / `exec_query` / `select_all`
   (`docs/internal-spec/effect-summaries.md:183`). Slice 1 need not implement it.
3. **The `universal:` / posture / row precedence.** `Catalog#lookup`
   (`catalog.rb:184`) is: class's own row → the 34-name universal list
   (`UNIVERSAL`, `:199`) → the class's posture; a class the catalogue does not
   list answers `nil` (contribute nothing, do NOT taint). Measured:

```
lookup("Kernel","puts")            = ["io.output.stdout"]   posture=false
lookup("IO","some_uncatalogued")   = ["io"]                 posture=true
lookup("Socket","class")           = []                     posture=false   # universal wins
lookup("Foo::Bar","baz")           = nil                                    # not listed
lookup("File","write",singleton:)  = ["io.fs.write"]
lookup("Array","push").mutates_receiver? = true                             # by reference
object_constant?("ENV") = true   object_constant?("File") = false
Catalog.default.identity = "1:85778dd3433fcb5561a933c9b2b22fb07048af980e35f93091f545655bda9c31"
```

That last line is worth keeping: upstream's own cache identity is
`schema:sha256(core.yml)` (`catalog.rb:158`), i.e. **upstream already treats the
file digest as the catalogue's identity.** The vendored copy should record the
same digest, so the PROVENANCE anchor and upstream's invalidation key are the
same number.

---

## 2. What ADR-0043 slice 1 requires, restated

The ADR's slice table gives slice 1 as "**vocabulary + the vendored effect
catalogue (`data/effects/registry.yml`, `core.yml`), as a pin-tracking surface
with a PROVENANCE and a re-sync step**", gated on "**catalogue parses; label
subsumption unit-tested**" (ADR-0043 § 5). Read with the rest of the ADR, the
deliverable is: bring both upstream data files into rigor-rs *verbatim*, behind a
loader that reproduces `Label`'s segment-aware prefix subsumption and
`Registry#known?`'s implied-ancestor rule and `Catalog#lookup`'s
row → universal → posture precedence, registered as a **third pin-tracking
surface** alongside `vendor/rbs/` and `vendor/plugins/` — with a `PROVENANCE.md`
recording source path, pin, date and digest, and a step in UPSTREAM.md's
pin-bump ritual that re-syncs it. It ships **no behaviour change**: ADR-0043 § 1
binds the whole arc to "the effects work may not change `crates/rigor-infer`'s
answers", and § 2's sound-subset contract does not engage until slice 2 emits a
summary. Slice 1's gate is therefore a *data* gate — the file loads, every label
it names is in the grammar and recognised by the vocabulary, every posture and
handler it names exists, and subsumption is unit-tested — plus the standing
`rigor check` gates staying at 0 movement, which is free while nothing reads the
catalogue.

---

## 3. Oracle runs

### 3a. The instrument still passes

```
$ python3 harness/effects_diff.py --self-test
=== SELF-TEST harness/effects-corpus/01_core_origins ===   oracle=16  MATCH=16  UNDER=0  OVER=0  DECLARED-MISMATCH=0
=== SELF-TEST harness/effects-corpus/02_propagation ===    oracle=15  MATCH=15  UNDER=0  OVER=0  DECLARED-MISMATCH=0
=== SELF-TEST harness/effects-corpus/03_taint ===          oracle=11  MATCH=11  UNDER=0  OVER=0  DECLARED-MISMATCH=0
=== SELF-TEST harness/effects-corpus/04_declared ===       oracle=4   MATCH=4   UNDER=0  OVER=0  DECLARED-MISMATCH=0
RESULT: PASS — the comparison is sound on every project.
```

46 methods, matching the recorded slice-0 baseline. Re-measured: the corpus's
proven lanes carry **28 summed labels / 10 distinct** (`global.read`,
`global.write`, `io.fs.read`, `io.fs.write`, `io.output.stdout`, `io.process`,
`mutate.self`, `mutate.static`, `nondet.random`, `nondet.time`).

### 3b. How catalogue entries manifest (`--format=json`, scratch project, `paths: [lib]`)

One class, one method per shape. Text form first:

```
Manifest#say:        [io.output.stdout]     puts "hello"
Manifest#warn_it:    [io.output.stderr]     warn "careful"
Manifest#write_file: [io.fs.write]          File.write(path, body)
Manifest#read_file:  [io.fs.read]           File.read(path)
Manifest#open_read:  [io.fs.read]           File.open(path, "r")     <- narrowed
Manifest#open_write: [io.fs.write]          File.open(path, "w")     <- narrowed
Manifest#open_blind: [io.fs]                File.open(path, mode)    <- unnarrowed fallback
Manifest#pipe_open:  [io.process]           open("|#{cmd}")          <- kernel_open
Manifest#fetch:      [io.net.http]          Net::HTTP.get(uri)
Manifest#connect:    [io.net]               TCPSocket.new(host,port) <- POSTURE, no row
Manifest#token:      [nondet.random]        SecureRandom.hex(16)
Manifest#clock:      [nondet.time]          Time.now
Manifest#fixed_clock:[]                     Time.new(2020,1,1)       <- time_new narrows to ∅
Manifest#home:       [global.read]          ENV["HOME"]              <- kind: object => ENV#[]
Manifest#set_home:   [global.write]         ENV["HOME"] = value
Manifest#dice:       [nondet.random]        rand(6)
Manifest#pure_add:   [] …?                  (a + b) * 2              <- NOT exhaustive
Manifest#socket_class: [] …?                sock.class
```

The JSON carries the five graded fields plus the origin attribution:

```json
"Manifest#open_read": {
  "effects": ["io.fs.read"], "declared": [], "exhaustive": true, "causes": [],
  "direct": { "catalogue:File.open": ["io.fs.read"] }
},
"Manifest#connect": {
  "effects": ["io.net"], "declared": [], "exhaustive": true, "causes": [],
  "direct": { "catalogue:TCPSocket.new": ["io.net"] }
},
"Manifest#pure_add": {
  "effects": [], "declared": [], "exhaustive": false,
  "causes": [["dynamic-receiver", null], ["dynamic-receiver","inferred_return_untyped"]],
  "direct": {}
}
```

Three things a port must copy exactly:

- The **origin key spelling** is `catalogue:<Owner>#<sel>` (instance) /
  `catalogue:<Owner>.<sel>` (singleton). Construct origins use
  `construct:<kind>` (measured over the corpus: `construct:ivar-write`,
  `gvar-read`, `gvar-write`, `cvar-write`, `xstring`).
- A **posture** answer produces the same `catalogue:` origin key as a row
  (`TCPSocket.new`, which has no row at all).
- A **narrowed row that narrows to ∅** (`Time.new(2020,1,1)`) produces an EMPTY
  `direct:` map — not a `catalogue:Time.new: []` entry.

### 3c. `rigor effects update` → `.rigor-effects.yml`

Scratch project with `effects.snapshot.{path,reach}` set and a three-hop call
graph (`render → header/body → File.read` and `render → emit → puts`):

```yaml
# .rigor-effects.yml — generated by `rigor effects update`. Commit it; review its diff.
schema: 1
rigor: "0.3.4"
vocabulary: 1
config_digest: "b7ffeff227be5f49897867ca3306047bf52c6584bea7a21f71af16c8f4d27999"
methods:
  "Report#body":   { effects: ["io.fs.read"] }
  "Report#emit":   { effects: ["io.output.stdout"] }
reach:
  "Report#body":   { effects: ["io.fs.read"] }
  "Report#emit":   { effects: ["io.output.stdout"] }
  "Report#render": { effects: ["io.fs.read", "io.output.stdout"] }
```

Confirms ADR-0043 § 3 as written: `methods:` is **direct** (only the two methods
with their own catalogue contribution appear; `render` and `header` do not), and
`reach:` is the transitive footprint at entry points. The four excluded header
keys are exactly the four present (`schema`, `rigor`, `vocabulary`,
`config_digest` — `effects_diff.py:61`).

---

## 4. Vendor mechanism recommendation

### 4a. Which precedent

The repo has two, and they are different tools for different problems:

| | `vendor/rbs/` (ADR-0007) | `vendor/plugins/` |
|---|---|---|
| size | thousands of `.rbs` | 1 file |
| embed | `build.rs` walks the tree, generates `EMBEDDED_RBS` (`crates/rigor-index/build.rs:44`) | direct `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), …))` (`crates/rigor-index/src/plugins.rs:40`) |
| regenerate | `harness/vendor_rbs.py` with a `--check` self-test | hand copy per `PROVENANCE.md` |
| tracks | the **rbs gem** version | the **reference pin** |

The catalogue is **2 files that track the reference pin**. That is the
`vendor/plugins/` shape, not the `vendor/rbs/` shape. A `build.rs` that walks a
directory to find two known files buys nothing and adds a codegen step to debug.

### 4b. Recommendation

**Where.** A new crate, `crates/rigor-effects`, owning
`crates/rigor-effects/vendor/effects/{core.yml,registry.yml,PROVENANCE.md}` and
the `Label` / `Registry` / `Catalog` modules. A *new* crate rather than a module
of `rigor-index` or `rigor-infer` because ADR-0043 § 1 forbids the effects work
from changing `crates/rigor-infer`'s answers, and **a dependency edge that does
not exist cannot be violated by review error**: `rigor-effects` depends on
nothing of ours, and only `rigor-cli` depends on it. If a later slice genuinely
needs the typer, that edge becomes a visible, arguable manifest change.

**Embed at build time, parse at runtime.** `include_str!` the two files (no
`build.rs`), and parse with **`serde_yaml`, which `rigor-cli` already depends on**
(`crates/rigor-cli/Cargo.toml:53`) for the `.rigor.yml` loader — so slice 1 adds
**zero new dependencies** and stays `cargo build --offline`-safe. Parse lazily
into a `OnceLock`/`LazyLock`, exactly as upstream memoises (`catalog.rb:115`),
so **`rigor check` never touches the bytes**: § 1's byte-identical-cost promise
then holds by construction rather than by measurement. 55 KB of `include_str!`
is noise against the existing `EMBEDDED_RBS` payload.

Rejected alternative: transform the YAML into a generated Rust table at build
time. It moves the same YAML dependency into `[build-dependencies]`, makes the
vendored bytes no longer byte-comparable to upstream's at runtime, and buys a
one-off parse of 52 KB.

**Regeneration script.** `harness/vendor_effects.py`, modelled on
`vendor_rbs.py`'s contract (which is the repo's stated reason that file exists:
"That recipe used to live only in `PROVENANCE.md` prose and was executed by hand;
this script is the executable form", `harness/vendor_rbs.py:6-9`):

```
python3 harness/vendor_effects.py            # copy reference/rigor/data/effects/*.yml -> vendor/effects/
python3 harness/vendor_effects.py --check    # do not write; exit 1 on ANY byte difference
```

Source is **the pinned submodule** `reference/rigor/data/effects/`, never a local
checkout (UPSTREAM.md hazard 3, and the `vendor/plugins/` copy is the recorded
case of that hazard applied to a file). `--check` prints both `sha256`s.
`PROVENANCE.md` stays hand-authored, and records source path, pin tag + commit,
date, and both digests — the `core.yml` one being the same number upstream uses
as its cache identity (§ 1c).

**Note a stale line while you are there.** `crates/rigor-index/vendor/plugins/PROVENANCE.md:6`
claims the plugin RBS is embedded "by `crates/rigor-index/build.rs` (the
`EMBEDDED_PLUGIN_RBS` table)". No such table exists — `build.rs` generates only
`EMBEDDED_RBS`, and the plugin payload is a direct `include_str!` in
`src/plugins.rs:40`. Harmless, but it is the file a slice-1 author reads to copy
the precedent.

---

## 5. Pin-surface hazard — the ritual step and the gate slice 1 must ship WITH the catalogue

The recorded lesson is that a vendored tree drifts silently and the cost is
measured in live false positives: the `activesupport-core-ext` copy sat unmoved
for two months and the drift was **10 FPs "neither sweep tool can see"**
(`crates/rigor-index/vendor/plugins/PROVENANCE.md:24-34`). The catalogue is the
same shape of surface and needs both halves of the answer that closed that one:
a **ritual step** and a **mechanical gate**.

### 5a. The ritual step

UPSTREAM.md **step 3** ("Check whether the release moved rbs") is where the
pin-tracking trees are re-synced; it currently names two — `vendor/rbs/` +
`overlay/`, and (since 2026-08-25) `vendor/plugins/`. Slice 1 adds the **third**
bullet, in the same commit as the vendored copy:

> **Re-sync `crates/rigor-effects/vendor/effects/` too** — `python3
> harness/vendor_effects.py --check`, and on a mismatch re-vendor and re-read
> the diff as a semantic change, not a copy. Upstream's `retired:` table and
> `vocabulary:` bump are the two entries that can invalidate a committed
> `.rigor-effects.yml`, and a `schema:` bump changes the row grammar.

Step 3's existing standing advice applies verbatim and should be quoted, not
paraphrased: *drive this off `diff`, both directions, not off a list a survey
named.* A catalogue re-audit that moves `IO#write` from `io` to `io.fs.write`
changes summaries with no source change on either side.

### 5b. What `effects_diff.py` already covers, and what it does not

Measured, by extracting every `direct:` origin key the oracle produces across all
four corpus projects:

```
distinct direct-origin keys exercised: 11
  catalogue:ENV#[]   catalogue:File.read   catalogue:File.write
  catalogue:Kernel#puts   catalogue:Kernel#rand   catalogue:Time.now
  construct:cvar-write  construct:gvar-read  construct:gvar-write
  construct:ivar-write  construct:xstring
=> catalogue origins: 6
```

**Six catalogue rows out of 420 — 1.4%.** Zero postures, zero `universal:`
selectors, zero `kind: object` classes other than `ENV`, zero of the 6 narrowing
handlers reached through a non-∅ answer, zero `mutators:`-by-reference rows.
`effects_diff.py` is a *behavioural* gate and a good one, but it is
coverage-limited in precisely the way harness fixture 98 was for the plugin RBS:
a drift in the other 414 rows is invisible to it.

### 5c. The gate slice 1 must ship

Three layers, cheapest first. Only the second is coverage-independent, and it is
the one that would have caught the plugin-RBS drift:

1. **A data unit test in `rigor-effects`** — upstream's own two data specs,
   ported. `spec/rigor/effects/registry_data_spec.rb` (the 36 labels in four
   named groups, vocabulary 1, `retired:` empty, the 10 roots, the interior
   nodes a bound may name, subsumption) and
   `spec/rigor/effects/catalog_data_spec.rb` (every label well-formed AND
   `registry.known?`; every row has a `why:`; every posture is in `defaults:`;
   every `narrow:` handler exists; a narrowed row's unnarrowed fallback is
   non-empty; the `universal:` list wins over a world posture; an unlisted class
   answers `nil`). This is exactly ADR-0043's "catalogue parses; label
   subsumption unit-tested", and it is upstream's own gate, so it is free.
2. **`harness/vendor_effects.py --check` in the standing gate set**, byte-for-byte
   against `reference/rigor/data/effects/`. This is the drift gate: it is
   independent of what any corpus exercises, and it fails the instant the pin
   moves under an unchanged vendored copy. `vendor_rbs.py --check` is the
   precedent and it is already written.
3. **A pinned-digest assertion** in the crate (the `sha256` from PROVENANCE), so
   an accidental hand-edit of the vendored bytes fails locally even without the
   submodule populated. Cheap, and it is the only one of the three that works in
   a checkout whose `reference/rigor` is empty.

`effects_diff.py` remains the semantic gate and grows coverage as slices 2-4
land; it is not a substitute for (2).

---

## 6. Shape check against slice 2

Slice 2 is "**direct** summaries: catalogue rows + the construct origins" gated
at 0 OVER (ADR-0043 § 5). What it consumes from slice 1, in the order
`Catalog#lookup` consults it (`catalog.rb:184`):

| slice 2 needs | carried by the recommended vendor? |
|---|---|
| `(owner, selector, singleton) → labels` from `methods:` / `singleton_methods:` | yes — 420 rows, verbatim |
| an explicit `effects: []` distinguished from "no row" | yes — 77 such rows; the loader must keep `Option<Entry>` vs `Entry(∅)` distinct (`catalog.rb:52-58`) |
| the 34-name `universal:` list, consulted after rows and before posture | yes |
| `posture:` / `singleton_posture:` resolved through the 14 `defaults:` | yes |
| `kind: object` (5 constants) so `ENV["k"]` keys as `ENV#[]` | yes |
| `mutates: receiver` → the ownership judgment | yes for the 8 explicit rows; **the 3 by-reference mutator sets are NOT in the YAML** (§ 1c) and must be ported from `mutation_widening.rb` / `mutation_classifier.rb` as slice-2 code |
| `narrow:` handler NAMES (6 used) | yes — the names; **the handler bodies are Ruby** (`narrowing.rb`) and are slice-2 code |
| the `from_posture` bit | yes — needed because a posture answer still keeps the project edge (`docs/internal-spec/effect-summaries.md:156-165`), which slice 4 propagates |
| origin key spelling `catalogue:Owner#sel` / `catalogue:Owner.sel` | measured in § 3b; a format decision, not data |

So the vendored shape carries **exactly** what slice 2 reads, with two named
carve-outs that are code and not data (mutator sets, handler bodies) — and slice
1 should say so in the PROVENANCE, because a future reader who sees
`mutators: array` in the YAML and no selector list will assume the copy is
truncated.

Upstream catalogue-adjacent surfaces that ADR-0043 puts **out of scope**:

- **The plugin effect layer.** `plugins/*/lib/rigor/plugin/*/effects.rb` is
  **1,107 lines across 9 Rails plugins**, and it contributes rows, attributions,
  edges, entry-point presets and its own `effect_labels:` root extension
  (`lib/rigor/effects/plugin_facts.rb`). It is the sole consumer of the 7th
  narrowing handler (`sql_verb`) and of the `io.db.*` / `job.enqueue` /
  `email.send` / `cache.*` labels. ADR-0043 names it in no slice.
- **`Registry#with(labels:, owner:)`** and its root-ownership rule
  (`registry.rb:138`, `:150`) — the project's `effects.labels:` extension and the
  plugin root-ownership check. Slice 1 needs `known?`, `roots` and the grammar;
  `with` can wait for the declared lane (slice 6).
- `retired:` is empty at the pin, but the **mechanism** is cheap and belongs in
  slice 1: a snapshot written by a newer Rigor is the thing it protects.

---

## 7. The standing OPEN question — SOLVED: `declared:` is the CALLER's lane

ADR-0043's one open question was "when does the oracle populate `declared:`?",
with four hypotheses refuted. All four shared an assumption, and the assumption
is what is wrong: **they assumed the declared lane belongs to the annotated
method.** It does not.

The mechanism is `UnitScan#import_envelope`
(`reference/rigor/lib/rigor/effects/unit_scan.rb:328-338`), whose own comment
states it plainly:

> "The **declared lane at a call site** … If the callee's own declaration carries
> an envelope, the bound joins **the caller's** `≤` lane under an `envelope:`
> origin"

`add_declared(Origin.envelope("Owner#sel"), envelope.bound)` at `:337` runs while
scanning **the call**, and the lane then travels call edges like the proven lane
(`propagator.rb:137`). A method's own annotation never colours its own row.

A second, independent rule stacks on top at render time:
`Entry#rendered_declared = declared.excluding_subsumed_by(proven)`
(`lib/rigor/effects/effect_table.rb:41-43` → `label_set.rb:100-107`) — a declared
label the proven lane already admits is dropped from the output.

**Decisive probe** (scratch project, `sig/gateway.rbs`, five predictions written
before the run, all five confirmed):

| method | body | annotated? | predicted `declared` | measured |
|---|---|---|---|---|
| `Gateway#fetch_row` | `id.to_s` | `%a{rigor:v1:effect io.db}` | `[]` | `[]` |
| `Gateway#caller_a` | `fetch_row(id)` | no | `["io.db"]` | `["io.db"]` |
| `Gateway#caller_b` | `fetch_row(id) + "!"` | no | `["io.db"]` | `["io.db"]` |
| `Gateway#caller_c` | `sleep 0; fetch_row(id)` | no | `[]` (proven `io` admits `io.db`) | `[]`, proven `["io"]` |
| `Gateway#lonely` | `1`, never called | `%a{rigor:v1:effect io.net}` | `[]` | `[]` |

This retro-explains `harness/effects-corpus/04_declared` completely:
`load_and_log` CALLS `load_row`, so it carries `load_row`'s `io.db`; `load_row`
itself calls nothing annotated, so its own lane is empty; `formats` is
`%a{pure}` (the empty bound) and uncalled; `unannotated` has nothing. Every
observation, no residue.

**Consequences for the port.** The ADR's § 2 phrasing — "the declared lane is
*copied from the author's annotation*" — is right about the source and wrong
about the destination, and slice 6 must implement two rules, not one:
(a) an envelope on a callee joins the **caller's** declared lane, keyed
`envelope:<Owner>#<sel>`, and propagates transitively; (b) the rendered value
subtracts anything the proven lane admits. Omitting (b) alone yields a
`DECLARED-MISMATCH` on correct code — the exact fatal verdict § 4 defines. The
ADR's "Open at accepted" section and the fixture's header comment should both be
updated when slice 6 is scoped.

---

## 8. Two slice-0 defects found while probing

Neither blocks slice 1; both are cheap and both live in the instrument slice 1
will be measured by.

### 8a. The result cache is NOT disabled, though ADR-0043 § 4 says it is

ADR-0043 § 4 states flatly: "**The result cache is disabled per UPSTREAM.md
hazard 2**"
([`docs/adr/0043-effect-system-port-parity-model.md:129-130`](../adr/0043-effect-system-port-parity-model.md)).
The implementation hedges — "both disable the result cache **where the CLI
accepts the flag**" (`harness/effects_diff.py:116`) — and neither arm passes one
(`run_ref` at `:143-145`, `run_rs` at `:153-155`). It cannot, because the flag
does not exist on this subcommand:

```
$ rigor effects --full --no-cache --format=json
invalid option: --no-cache
EXIT=64
```

`rigor effects`'s option surface is `--format`, `--full`,
`--no-tolerated-effects` only (`lib/rigor/cli/effects_command.rb:95-102`);
`--no-cache` exists on `check` (`lib/rigor/cli/check_command.rb:487`) and
`coverage`, not here. Since the tool runs **in the project directory** by design,
the cwd-keyed cache persists across runs by construction — which is exactly the
hazard the ADR sentence is invoking, and it is unmitigated.

Mitigating evidence, measured: the effects cache key is
`Effects::Identity.descriptor` (`lib/rigor/effects/identity.rb:71`) composed onto
the run's diagnostics descriptor, which carries an `engine` slot
(`Rigor::VERSION`) **and** an `engine-source` slot digesting the checkout's own
`lib/` (`lib/rigor/analysis/run_cache_key.rb:69-92`), plus the catalogue's
content digest. So a pin bump *does* invalidate. The claim is wrong; the risk it
names is covered by upstream's key, not by our flag.

Fix: `shutil.rmtree(project/".rigor", ignore_errors=True)` before each arm — the
only lever available, since `cache.path` is config and not a flag
(`lib/rigor/configuration.rb:99`) — which makes the ADR sentence true and also
fixes 8b. Alternatively amend the ADR to record what actually guards the
measurement (upstream's key) rather than a flag that is not passed.

### 8b. The effects corpus commits `.rigor/cache`, and every run dirties the tree

`git ls-files harness/effects-corpus` returns **69 files, 60 of them
`.rigor/cache/**/*.entry`**. `.gitignore` ignores `/.rigor/` at the repo root
only, so the per-project caches are tracked. And they are not even live here —
one `effects_diff.py --self-test` run left **28 new untracked cache
directories**, because the committed entries were written under a different
`engine-source` / gem set:

```
?? harness/effects-corpus/01_core_origins/.rigor/cache/analysis.run-effects/b1/
?? harness/effects-corpus/02_propagation/.rigor/cache/rbs.environment/37/
… 28 total
```

So the instrument leaves the working tree dirty after every run, and carries 60
dead files. Fix: `git rm -r --cached` the cache trees, widen `.gitignore` from
`/.rigor/` to `**/.rigor/`, and have `effects_diff.py` clear the directory per
arm (8a).

---

## Reproduction

```sh
# populate the submodule at the pin, then:
python3 harness/effects_diff.py --self-test
python3 harness/effects_diff.py --list

# catalogue inventory (PyYAML): counts in § 1b
python3 - <<'PY'
import yaml
c = yaml.safe_load(open("reference/rigor/data/effects/core.yml"))
print(len(c["classes"]), sum(len(b.get("methods") or {}) + len(b.get("singleton_methods") or {})
                             for b in c["classes"].values()))
PY

# the loader's own answers (§ 1c)
ruby -I reference/rigor/lib -e '
  require "rigor/effects/catalog"; require "rigor/effects/registry"
  c = Rigor::Effects::Catalog.default
  puts c.identity
  puts c.lookup("Socket","class").labels.to_a.inspect
  puts Rigor::Effects::Registry.default.known?("global").inspect'

# the declared-lane probe (§ 7) — a scratch project with the sig/ shown there
rigor effects --full --format=json
```
