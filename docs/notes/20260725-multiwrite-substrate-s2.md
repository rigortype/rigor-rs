# MultiWrite substrate — Slice 2 results (2026-07-25)

Implements Slice 2 of [the spec](20260725-multiwrite-substrate-spec.md): RBS
**tuple returns** are typed per-position instead of collapsing to `Dynamic[top]`,
an RBS-only element class gets a registry identity, and the ADR-0042 qualified
surface witnesses on it. Fixture 68's last missing diagnostic
(`undefined method 'frobnicate' for Process::Status`, 40:8) closes — the harness
reaches **70 fixtures / 0 coverage gaps**.

Builds directly on [Slice 1](20260725-multiwrite-substrate-s1.md) (`Node::MultiWrite`
+ the `MultiTargetBinder` port): once `Process.wait2` types as a `Type::Tuple`,
the binder distributes it and `status` binds to the `Process::Status` nominal
with no further work.

## The return descriptor

`crates/rigor-index/src/rbs.rs`:

```rust
pub enum RbsReturnShape {
    Class(&'static str),        // ClassInstance, type args dropped, name AS WRITTEN
    Tuple(Vec<RbsReturnShape>), // recursive
    Unknown,                    // the reference's `untyped` degrade
}
```

Why this shape:

- **Additive, not a rewrite of the flat path.** `method_signature`'s
  `(Option<&'static str>, Arity, …)` tuple is untouched; a new `tuple_return(md)`
  runs its own overload loop (the style `block_overload_return` already uses) and
  stores into two NEW `ClassEntry` maps, `tuple_returns` / `singleton_tuple_returns`.
  A tuple return still yields `None` from `method_return` /
  `singleton_method_return`, so *every* existing consumer sees exactly what it saw
  before (see "callers" below). The alternative — widening the flat slot into an
  enum — would have touched every arm of the all-overloads-agree collapse and the
  four merge sites for zero extra capability.
- **Only the two shapes rigor-rs can carry losslessly.** The reference's
  `RbsTypeTranslator` (`rbs_type_translator.rb:63`) is a total function over the
  RBS type algebra; rigor-rs models `ClassInstance` (⇒ `Nominal`) and `Tuple`
  (⇒ `Type::Tuple`). Everything else — union, optional, literal, interface, type
  variable, `untyped`, `void` — is `Unknown`.
- **`Unknown` is a per-element degrade, not a decline.** The reference's fallback
  for an unhandled shape is `Type::Combinator.untyped`, so an element it types
  more precisely than we can (`String?` ⇒ `String | nil`) must not delete the
  SIBLING elements' precision. `Dynamic[top]` is silent in every rule, so the
  deviation only loses recall. A TOP-LEVEL non-tuple return is simply absent from
  the map (the flat path owns it).
- **Element names are the WRITTEN spelling** (`written_type_name`: namespace path
  + leaf ⇒ `Process::Status`), which is exactly the key shape ADR-0042's
  qualified registry uses — so an element name looks up in
  `knows_qualified_class` / `qualified_class_has_method` with no translation.
  Type arguments are dropped (`Array[Integer]` ⇒ `Class("Array")`), the same
  one-level discipline `RetainedParamType` and the flat return path already apply.
- **The all-overloads-agree discipline is preserved.** `tuple_return` returns
  `Some` only when EVERY overload's return is a tuple and all agree. `IO.pipe`
  (whose block overload returns the block's value) therefore declines and stays
  `Dynamic[top]` — a coverage gap, oracle-verified below, in the FP-safe direction.
  An `Optional` tuple (`-> [String, String]?`) is deliberately NOT unwrapped: the
  reference translates it to `Tuple | nil`, which the descriptor cannot carry.

Lookups mirror the existing walks exactly: `method_tuple_return` rides the
first-definer-wins ancestor walk of `method_return_is_void`;
`singleton_method_tuple_return` rides the superclass walk of
`singleton_method_return`. Neither chases aliases (an under-emit, never a wrong
answer).

## How the RBS-only class gets an id

`Process::Status` appears in NO source file of fixture 68 — it is reached only
THROUGH `Process.wait2` — so `SourceIndex`'s Pass 2 (which registers ids for
RBS-known names harvested from source `ConstantRead` nodes) never sees it, and
`CORE_CLASSES` is a fixed 9-name list.

New **Pass 2b** (`crates/rigor-infer/src/source_index.rs`) registers an id for
every class name reachable as an element of any tuple return in the LOADED RBS,
enumerated by `CoreIndex::tuple_return_class_names()`. It is declaration-driven,
not name-driven — no class name is special-cased anywhere in this slice — and the
set is small, closed and enumerable (17 names in the vendored rbs-4.0.3):

```
Addrinfo, Array, BigDecimal, Complex, Float, Gem::Version, Integer, Numeric,
Pathname, Proc, Process::Status, Rational, Resolv::DNS::Message,
Resolv::DNS::Name, Resolv::DNS::Resource, Socket, String
```

A name that is already a source class keeps the source registration (the
project's own class wins, as everywhere else), and an element the loaded RBS does
not model is skipped — an unregistered element simply leaves that slot
`Dynamic[top]` (silent).

The alternative considered — a THIRD `ClassId` range owned by `CoreIndex` — was
rejected: `class_name_for_id` recovery is plumbed through the source registry in
the rules, `render_receiver`, coverage and the LSP, so a new range would have
touched all of them for no semantic gain.

## The witness gate — and the FP that shaped it

`check_call`'s source-registry arm (`crates/rigor-rules/src/lib.rs`) gained one
disjunct: `index.knows_qualified_class(name) && source.is_declaration_only_class(name)`.
`knows_toplevel_class` refuses every namespaced name for the defect-2 reason (a
SHORT key like `Status` may be a project class), but a QUALIFIED key is an
isolated entry no project name can collide with — the same argument the typer's
`ConstantRead` arm already makes for `ERB::Util`, and the same surface
`qualified_class_has_method` (ADR-0042 slice 3, already built) checks. It adds
nothing for a bare name: a top-level class is already in `knows_toplevel_class`,
and a nested-only class's SHORT key has no qualified entry.

**The second conjunct is load-bearing, and it was measured.** The first draft of
this slice had the bare `knows_qualified_class` disjunct. A probe (not a corpus —
none of the corpora contain the shape) found a REAL false positive:

```ruby
g = Gem::Version.new("1.0")
g.segments        # oracle: SILENT.  first draft: undefined method `segments'
```

`segments` is a real `Gem::Version` method, and the vendored rbs-4.0.3 does not
declare it — but the reference does, in
`reference/rigor/data/vendored_gem_sigs/rubygems/rubygems_extras.rbs:129`. The
reference ships hand-written EXTRAS for twelve gems (ast, bcrypt, bundler, cgi,
did_you_mean, idn-ruby, mysql2, nokogiri, pg, prism, redis, rubygems) that
rigor-rs does not vendor, so for those classes **rigor-rs's method surface is a
strict SUBSET of the oracle's** and witnessing absence over it is unsound.

Two fixes were built and one was measured out:

1. **Decline the `.new` mint for a bundled qualified class** (in `type_dot_new`).
   Closes the FP, but REGRESSES generative output: `def version; Gem::Version.new("1.0"); end`
   went from `def version: () -> Gem::Version` (byte-identical to the oracle, on
   BOTH old and new) to `No candidates`. Rejected.
2. **Restrict the witness (not the type) to DECLARATION-ONLY classes**
   (`SourceIndex::is_declaration_only_class`): a class whose registry id came
   ONLY from the Pass 2b tuple-element sweep — the analyzed source neither
   declares the class nor names the constant anywhere, so the value can only have
   come from an RBS declaration. `Gem::Version.new(...)` names the constant ⇒ not
   declaration-only ⇒ silent (the pre-Slice-2 behaviour); `Process::Status` is
   never named in fixture 68 ⇒ witnesses. Types are untouched, so `sig-gen` and
   `annotate` keep their oracle-matching precision. **Shipped.**

Residual surface, audited: a tuple return only resolves for a TOP-LEVEL receiver
(both lookups ride the SHORT-key map, which holds no qualified keys), so of the
four QUALIFIED element classes — `Process::Status`, `Gem::Version`,
`Resolv::DNS::{Message,Name,Resource}` — only `Process::Status` (from `Process`)
is reachable at all; `Gem::Version` comes solely from `Gem::Requirement.parse`
and the `Resolv::DNS::*` ones solely from `Resolv::DNS::*` methods, all qualified
receivers. The restriction should be lifted when rigor-rs vendors the extras
bundle (its own slice — it changes `knows_class`, arity and ATM surfaces too).

The FP is pinned by `a_source_named_qualified_gem_class_stays_lenient`.

## Existing callers — none shift

**Index API.** No signature changed; three methods were added
(`method_tuple_return`, `singleton_method_tuple_return`,
`tuple_return_class_names`) and two `ClassEntry` maps. The flat return path's
DATA is unchanged too: a tuple return was `None` in `methods`/`singleton_methods`
before this slice and still is (`method_signature` is byte-for-byte untouched),
pinned by `singleton_tuple_return_is_retained`, which asserts
`singleton_method_return("Process", "wait2") == None`. The two merge sites
(`merge`, `merge_qualified`) gained first-write-wins arms for the new maps only.

**Registry.** The 17 names above are the only new `SourceIndex::class_id`
answers. The four call sites that consult it:

| site | shifts? | why |
| --- | --- | --- |
| `lib.rs` `ConstantRead` → toplevel `Singleton` | no | reaching it requires the name to be READ in source, which Pass 2 already registered |
| `lib.rs` `ConstantRead` → qualified `Singleton` (`ERB::Util`) | no | same |
| `type_dot_new` (`X.new`) | no | the receiver IS a `ConstantRead`, so same |
| singleton SCALAR return mint | **one case** | see below |

The scalar-return mint is the only place a name can be needed WITHOUT being read
in source. Enumerating the vendored RBS for a `def self.…` whose return is one of
the 17 gives 12 declarations, and 11 of them return their OWN receiver class
(`Pathname.pwd -> Pathname`, `Complex.rect -> Complex`, `Addrinfo.ip -> Addrinfo`,
`BigDecimal._load -> BigDecimal`, `Socket.unix_server_socket -> Socket`) — the
receiver constant must be read to make the call, so those were already registered
and already fired on the OLD binary (verified by probe). The twelfth is
`Delegator.delegating_block: (Symbol) -> Proc`. Probed:

| | old | new | oracle |
| --- | --- | --- | --- |
| `Delegator.delegating_block(:m).frobnicate` | *silent* | `5:32 undefined method 'frobnicate' for Proc` | **byte-identical** to new |

A coverage GAIN matching the oracle exactly. The instance-side scalar path
(`Tier 3`) mints only through `CoreIndex::class_id` (the 9 core names) and never
consults the registry, so it cannot shift at all.

**`analyze` composition / rule set:** untouched.

## Fixture 68 and the harness

```ruby
_pid, status = Process.wait2
status.exited?      # silent in both (the qualified surface resolves it)
status.frobnicate   # 40:8 — undefined method `frobnicate' for Process::Status
```

Byte-identical to `harness/snapshots/68_nested_stdlib_singleton.json`. No other
fixture's expectation moved.

| gate | result |
| --- | --- |
| `cargo build --offline && cargo test --offline` | PASS (all suites green; +10 new tests) |
| `ruby harness/run.rb` | **70 fixtures, 0 coverage gaps, 0 unregistered FP** |
| `ruby harness/run_snapshot.rb` | identical |
| `python3 harness/docs_check.py` | PASS (4 budgets, links resolve) |
| `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy --workspace --all-targets -- -D warnings` | clean |

(The summary prints `217/218` because the harness keys diagnostics by
`(rule, line, column)` and one fixture has two diagnostics sharing a key — the
same arithmetic Slice 1 recorded. The gate is `Coverage gaps: 0`.)

## `fp_audit --gaps` — 0 new FPs, and 0 delta on the mandated corpora

| corpus | files | ref | rs | FP candidates |
| --- | --- | --- | --- | --- |
| mastodon `app/models` | 248 | 115 | 112 | **0** |
| conference-app | 244 | 1998 | 1998 | **0** |
| gitlab-foss `lib` | 4676 | 1374 | 1044 | **0** |

Every number is identical to Slice 1's table: the change is output-inert on all
three mandated corpora. The old-vs-new differential (the pre-change and
post-change release binaries diffed per `(path, line, column, rule)`, each delta
classified against the oracle) confirms it — `0 added / 0 removed` on all three.

Coverage tiers are unchanged too (`rigor coverage --format json`, old vs new vs
oracle: mastodon `app/models` and haml `lib` — every tier bucket identical), so
the new `Type::Tuple`s displaced no node's classification.

### The wide sweep — 35,706 files, ZERO diagnostics moved

The same old-vs-new differential over fourteen corpora, every delta classified
against the oracle:

| corpus | files | old = new | added | removed | new FPs |
| --- | --- | --- | --- | --- | --- |
| mastodon `app/models` | 248 | 112 | 0 | 0 | **0** |
| conference-app | 244 | 1998 | 0 | 0 | **0** |
| gitlab-foss `lib` | 4676 | 1044 | 0 | 0 | **0** |
| mastodon `app` | 1236 | 410 | 0 | 0 | **0** |
| gitlab-foss `app` | 6513 | 1592 | 0 | 0 | **0** |
| rails | 3432 | 3093 | 0 | 0 | **0** |
| redmine | 4784 | 7622 | 0 | 0 | **0** |
| dependabot-core | 1650 | 138789 | 0 | 0 | **0** |
| haml (incl. `vendor/bundle`) | 3342 | 2511 | 0 | 0 | **0** |
| liquid (incl. `vendor/bundle`) | 3002 | 2356 | 0 | 0 | **0** |
| rubocop-ast | 2327 | 12380 | 0 | 0 | **0** |
| mail | 874 | 6666 | 0 | 0 | **0** |
| kramdown | 63 | 30 | 0 | 0 | **0** |
| faraday | 3315 | 5976 | 0 | 0 | **0** |

**The `check` output is bit-identical on every corpus swept** — this slice moves
no real-corpus diagnostic in either direction. The whole-repo corpora (haml /
liquid / faraday include their `vendor/bundle`, which is where Slice 1's reviewer
found its only real-corpus witness) were swept for exactly that reason.

Why nothing moved, mechanically: the instance tuple path needs a receiver whose
class the index resolves, and in real code `partition` / `divmod` receivers are
overwhelmingly untyped parameters (`line.to_s.partition(":")` — probed, both
engines silent, before and after); the singleton path needs a tuple-returning
class method, and `Process.wait2` / `Process.waitpid2` do not occur. The gain is
therefore fixture 68 plus the probe-verified shapes below — not a corpus number.

## Oracle-checked additions (synthetic probes)

Every shape the mechanism unlocks, checked against the hardened oracle
invocation (pinned `rigor-rbs-inline` plugin path, fresh cwd, `--no-cache`):

| source | old | new | oracle |
| --- | --- | --- | --- |
| `_pid, status = Process.wait2` ⇒ `status.frobnicate` | silent | `13:8 … for Process::Status` | **identical** |
| `pid, _s = Process.wait2` ⇒ `pid.frobnicate` | silent | `2:5 … for Integer` | **identical** |
| `Process.wait2.frobnicate` | silent | `13:15 … for [Integer, Process::Status]` | **identical** |
| `a, b, c = "x-y".partition("-")` ⇒ `a.frobnicate` | silent | `15:3 … for String` | same rule/line/column; the oracle renders the PINNED value `"x"` |
| `"x-y".partition("-").frobnicate` | silent | `… for [String, String, String]` | same position; oracle renders `["x", "-", "y"]` |
| `Delegator.delegating_block(:m).frobnicate` | silent | `5:32 … for Proc` | **identical** |
| `r, w = IO.pipe` ⇒ `r.frobnicate` | silent | silent | oracle FIRES (`for IO`) — a remaining gap, see below |
| `g = Gem::Version.new("1.0")` ⇒ `g.segments` | silent | silent | oracle silent — the FP above, closed |
| `Gem::Version.new("1.0").frobnicate` | silent | silent | oracle FIRES — the price of that guard (FP-safe) |

The two `partition` rows are the only message-text divergence: the reference's
ConstantFolding pins `"x-y".partition("-")` to the literal tuple `["x", "-", "y"]`
and renders the pinned value, while rigor-rs types the RBS tuple's classes. Same
rule, same line, same column, less precise RECEIVER RENDERING — a pre-existing
constant-folding gap (`folding::fold` declines `partition`), not something this
slice introduced. The harness keys on `(rule, line, column)`, and `fp_audit`
compares the same tuple, so neither gate is affected; it is recorded here so the
claim "byte-identical everywhere" is not made.

`IO.pipe` is the all-overloads-agree discipline declining: its block overload
returns the block's value, so the tuple is dropped and the slots stay
`Dynamic[top]`. rigor-rs under-emits where the oracle fires — FP-safe, and
pinned by `divergent_overloads_stay_dynamic` so a later block-aware overload
selection flips it visibly.

## `sig-gen` / `annotate` — user-visible output improved (oracle-exact)

| | old | new | oracle |
| --- | --- | --- | --- |
| `sig-gen --print`, `def reap; Process.wait2; end` | `No candidates` | `def reap: () -> [Integer, Process::Status]` | **byte-identical** |
| `annotate`, the same body | `#=> Dynamic[top]` | `#=> [Integer, Process::Status]` | **byte-identical** |
| `sig-gen --print`, `def version; Gem::Version.new("1.0"); end` | `-> Gem::Version` | `-> Gem::Version` | **byte-identical** (the rejected `type_dot_new` guard would have lost this) |

## What the spec got wrong / underspecified

1. **"The witness gate is already built" is the spec's biggest miss.**
   `qualified_class_has_method` was indeed built, but the GATE that reaches it
   (`knows_toplevel_class(name) || is_qualified_project_sig_class(name)`) refuses
   every bundled-RBS nested name, so `Process::Status` never reached the check —
   and, more importantly, opening that gate NAIVELY is a false positive: for a
   namespaced gem class rigor-rs's surface is a strict subset of the oracle's
   (the unvendored `vendored_gem_sigs` extras). ADR-0042 slices 3-4 stopping at
   `is_qualified_project_sig_class` was not an oversight; the bundled half needs
   the declaration-only provenance restriction built here. The spec's framing —
   "a value-typing wire-up, not new index machinery" — still holds for the index
   (nothing was added there for the witness), but the gate needed real design.
2. **The id-minting problem is broader than the spec's framing.** The spec named
   the missing registry id, but not that the fix must also be enumerable: the
   registry is shared, so registering a name changes `class_id` for every
   consumer. The enumeration table above is the audit that makes it safe, and it
   is the reason the slice's blast radius is one extra oracle-verified diagnostic
   shape (`Delegator.delegating_block`).
3. **The spec implied a single wiring point.** `method_signature` serves BOTH the
   instance and the singleton path, so the descriptor is inherently both. Fixture
   68 needs only the singleton half; the instance half (`String#partition` &c.)
   was wired too because suppressing it would encode a gap rather than a design
   boundary — and it measured at 0 new FPs across every corpus swept.
