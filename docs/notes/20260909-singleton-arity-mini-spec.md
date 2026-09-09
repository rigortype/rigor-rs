# Singleton-receiver arity / argument-type checking — measured mini-spec (#124)

**2026-09-09.** Pin `ffb456b0` (`v0.3.8`), port HEAD `77bcd3f`, ruby 4.0.5 /
rbs 4.2.0, release binary rebuilt from this worktree before every comparison.
Probe-and-design only — **no `crates/` file is touched**.

**Verdict: the mechanism is real and the issue's diagnosis of the substrate is
right, but the prize is much smaller than #124 claims and the naive
implementation is a measured false-positive engine.** Of the 128 arity/ATM
census rows, **39 are singleton-side** (not 23 — the issue's floor was low, and
its "unambiguously `.new`" count of 23 becomes 31 `.new` + 8 non-`.new`). But
only **4** of those 39 are closable by a slice that stops at the singleton rule;
**18** are blocked behind a qualified-singleton chain walk that does not exist,
**9** are blocked behind an RBS the port does not load at all, and the remaining
**8** are gated on an argument-typing axis this spec did not measure.

Against that, a naive implementation over-fires on **51 of the 164 top-level
classes** the reference declares (31%), every one oracle-confirmed.

Recommended shape: **land the substrate and the non-`.new` half; do NOT ship
`.new` arity until the `new`-derivation rule below is implemented exactly.**

---

## 1. Oracle-measured row table

Runner: `probe.py` (worktree root, scratch tool, not committed with the note's
claims resting on it — it prints `INVALID` and exits non-zero when the reference
emits no parseable JSON). Reference invoked from a fresh temp cwd as
`ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib
reference/rigor/exe/rigor check FILE --format json --no-cache`.

`RS-only` = a false positive that must not appear. `REF-only` = the prize.
`BOTH` = a must-still-fire control.

### 1a. Tiers of receiver — `.new`

| source line | reference | port | verdict |
|---|---|---|---|
| `File.new()` | `call.wrong-arity` L2 c6 `…(given 0, expected 1..3)` | silent | REF-only |
| `Array.new(1,2,3,4,5)` | `wrong-arity` c7 `(given 5, expected 0..2)` | silent | REF-only |
| `Hash.new(1,2,3)` | `wrong-arity` c6 `(given 3, expected 0..1)` | silent | REF-only |
| `String.new(1,2,3,4)` | `wrong-arity` c8 `(given 4, expected 0..1)` | silent | REF-only |
| `Struct.new()` | `wrong-arity` c8 `(given 0, expected 1..Infinity)` | silent | REF-only |
| `StringIO.new(1,2,3,4)` (stdlib) | `wrong-arity` c10 `(given 4, expected 0..2)` **+** `argument-type-mismatch` c14 ``parameter `string' … expected String, got 1`` | silent | REF-only ×2 |
| `Set.new(1,2,3,4)` | `wrong-arity` c5 `(given 4, expected 0..1)` | silent | REF-only |
| `Socket.new()` | `wrong-arity` c8 `(given 0, expected 2..3)` | silent | REF-only |
| `Tempfile.new(1,2,3,4,5)` | `wrong-arity` c10 `(given 5, expected 0..2)` | silent | REF-only |
| `UNIXSocket.new(1,2,3)` | `wrong-arity` c12 `(given 3, expected 1)` **+** ATM c16 `expected String, got 1` | silent | REF-only ×2 |
| `Gem::Specification.new(1,2,3,4)` (vendored gem, namespaced) | `wrong-arity` c20 `(given 4, expected 0)` | silent | REF-only |
| `Gem::Version.new()` | `wrong-arity` c14 `(given 0, expected 1)` | silent | REF-only |
| `Bundler::LockfileParser.new(1,2)` | `wrong-arity` c25 `(given 2, expected 1)` **+** ATM c29 `expected String, got 1` | silent | REF-only ×2 |
| `Psych::DisallowedClass.new(1,2)` | `wrong-arity` c24 `(given 2, expected 0..1)` | silent | REF-only |
| `RDoc::Comment.new(1,2,3,4,5)` | `wrong-arity` c15 `(given 5, expected 0..3)` **+** ATM c19 ``parameter `text' … expected String?, got 1`` | silent | REF-only ×2 |
| `Redis.new(1)` (vendored gem, top-level) | `wrong-arity` c7 `(given 1, expected 0)` | silent | REF-only |
| `DidYouMean::Formatter.new(1)` | `wrong-arity` c23 `(given 1, expected 0)` | silent | REF-only |
| `Widget.new` (project `sig/`, see §1e) | `wrong-arity` c8 `(given 0, expected 2)` | silent | REF-only |
| `Outer::Inner.new("a","b")` (project `sig/`, nested) | `wrong-arity` c14 `(given 2, expected 1)` | silent | REF-only |
| `Frobnicate::Widget.new()` (unknown class) | silent | silent | — |
| `Whatever.new(1,2,3)` (unknown class) | silent | silent | — |
| `Net::HTTP::Get.new()` (RBS-unknown to both) | silent | silent | — |
| `RDoc::Markup::Table.new(1)` (RBS-unknown to both) | silent | silent | — |
| `MyIO.new(1,2,3,4)` where `class MyIO < StringIO` | silent | silent | — (project subclass is never checked) |

### 1b. Singleton methods that are not `new`

| source line | reference | port | verdict |
|---|---|---|---|
| `Time.at()` | `wrong-arity` c6 `(given 0, expected 1..3)` | silent | REF-only |
| `File.read()` / `File.read(1,2,3,4,5)` | `wrong-arity` c6 `(expected 1..3)` | silent | REF-only ×2 |
| `Socket.getnameinfo()` / `…(1,2,3,4)` | `wrong-arity` c8 `(expected 1)` | silent | REF-only ×2 |
| `Shellwords.split()` / `…("a","b","c")` | `wrong-arity` c12 `(expected 1)` | silent | REF-only ×2 |
| `Dir.glob()` / `Dir.glob(1,2,3,4,5,6)` | `wrong-arity` c5 `(expected 1..2)` | silent | REF-only ×2 |
| `Base64.decode64()` | `wrong-arity` c8 `(expected 1)` | silent | REF-only |
| `JSON.parse()` | `wrong-arity` c6 `(expected 1..2)` | silent | REF-only |
| `Process.wait2(1,2,3,4,5)` | `wrong-arity` c9 `(expected 0..2)` | silent | REF-only |
| `Math.sqrt()` / `Math.sqrt(1,2,3)` | `wrong-arity` c6 `(expected 1)` | silent | REF-only ×2 |
| `Kernel.format()` | `wrong-arity` c8 `(expected 1..Infinity)` | silent | REF-only |
| `FileUtils.mkdir_p()` | `wrong-arity` c11 `(expected 1)` | silent | REF-only |
| `Digest::MD5.hexdigest(1,2,3,4)` | `wrong-arity` c13 `(expected 1)` | silent | REF-only |
| `ERB::Util.html_escape(1,2,3)` | `wrong-arity` c11 `(expected 1)` **+** ATM c23 | silent | REF-only ×2 |
| `CGI.parse()` | `wrong-arity` c5 `(expected 1)` | silent | REF-only |
| `Time.parse()` | `wrong-arity` c6 `(expected 1..2)` | silent | REF-only |
| `Dir.getwd(1)` / `Dir.pwd(1)` | `wrong-arity` c5 `(expected 0)` | silent | REF-only ×2 (`pwd` resolves through a singleton **alias**) |
| `Comparable.frobnicate()` | `call.undefined-method` c12 | `call.undefined-method` c12 | **BOTH** — control |
| `Math.frobnicate_zzz()` | `undefined-method` c6 | `undefined-method` c6 | **BOTH** — control |
| `Redis.frobnicate_zzz()` | `undefined-method` c7 | `undefined-method` c7 | **BOTH** — control |
| `"abc".upcase(1,2,3)` | `wrong-arity` c7 `(given 3, expected 0..2)` | same | **BOTH** — instance-side control |

### 1c. Argument shapes — what the reference's own gate declines

`wrong_arity_diagnostic` calls `plain_positional_call?`, which rejects a
`SplatNode` / `KeywordHashNode` / `BlockArgumentNode` / `ForwardingArgumentsNode`
argument. Measured, all from one file, both engines:

| source line | reference | port | verdict |
|---|---|---|---|
| `File.new(*args)` | **silent** | silent | ref declines on splat — confirmed |
| `Time.at(*args)`, `StringIO.new(*args)`, `Array.new(*args)` | **silent** | silent | same |
| `File.new(**opts)`, `Time.at(**opts)` | **silent** | silent | ref declines on double-splat |
| `File.new(mode: "r")` | **silent** | silent | ref declines on keyword args |
| `Dir.glob("x", base: "y", frobnicate: 1)` | **silent** | silent | same |
| `File.read("x", mode: "r", frobnicate: 2)` | **silent** | silent | same |
| `File.open() { \|f\| f }` | **`wrong-arity` c6 `(given 0, expected 1..3)`** | silent | REF-only — **a block LITERAL does NOT disable the check** |
| `Array.new(1,2,3,4) { \|i\| i }` | **`wrong-arity` c7 `(given 4, expected 0..2)`** | silent | REF-only — same |
| `File.open(&blk)` | **`wrong-arity` c6 `(given 0, …)`** | silent | REF-only — a block-PASS lands in `call_node.block`, not `arguments`, so `simple_positional?`'s `BlockArgumentNode` arm is dead in current Prism |
| `Array.new(1,2,3,4,&blk)` | **`wrong-arity` c7 `(given 4, …)`** | silent | REF-only — same |
| `Struct.new(:a,:b).new(1,2)` | **silent** | silent | `anonymous_struct_new_call?` declines explicitly |
| `Data.define(:x).new(1)` | **silent** | silent | same family |
| `S = Struct.new(:a,:b); S.new(1,2)` | **silent** | silent | — |

### 1d. The ATM half, separated from the arity half

| source line | reference | port | verdict |
|---|---|---|---|
| `StringIO.new(1)` (arity OK, type wrong) | ATM c14 ``parameter `string' of `new' on StringIO: expected String, got 1`` | silent | REF-only |
| `Math.sqrt("x")` | ATM c11 ``parameter `x' … expected Numeric, got "x"`` | silent | REF-only |
| `Shellwords.split(1)` | ATM c18 ``parameter `line' … expected String, got 1`` | silent | REF-only |
| `Shellwords.shellsplit(1)` | ATM c23 ``parameter `line' … expected String, got 1`` | **fires, same key** | **BOTH** |
| `Base64.decode64(1)` | ATM c17 ``parameter `str' … expected String, got 1`` | **fires, same key** | **BOTH** |
| `File.read(1)` | silent | silent | — |
| `Integer("x","y")` (no receiver) | silent | silent | — |
| `StringIO.new("x")`, `Math.sqrt(1.0)`, `Base64.decode64("x")`, `Shellwords.split("x")`, `File.read("x")` | silent | silent | — must-stay-silent controls |

**The port's singleton ATM arm is already live** (`check_argument_type_mismatch`,
`crates/rigor-rules/src/lib.rs:2274`, reads
`index.singleton_method_overloads`). `Base64.decode64` and
`Shellwords.shellsplit` reach it; `Shellwords.split` does not, and the
difference is that `split` is `alias self.split self.shellsplit`
(`stdlib/shellwords/0/shellwords.rbs:183`). **`singleton_method_overloads`
resolves no singleton alias** — unlike `class_has_singleton_method`, which
consults `singleton_aliases`. `Dir.pwd` / `Dir.getwd` and
`Regexp.compile` are the same shape. This is a real, isolated, cheap bug.

`StringIO.new(1)` fails for a different reason: **the port has no `new`
overloads for any class** (`singleton_method_overloads(_, "new")` is `false` for
every one of the 1368 names probed in §4) — no RBS declares `def self.new` for
these classes; the reference synthesizes it. See §3b.

### 1e. Project `sig/` tier

Fixture: `sig/widget.rbs` declaring `class Widget` with
`def initialize: (String name, Integer size) -> void` and
`def self.build: (String) -> Widget`, plus `module Outer; class Inner; def
initialize: (String a) -> void`. Analysed from the project root.

| source line | reference | port | verdict |
|---|---|---|---|
| `Widget.new` | `wrong-arity` c8 `(given 0, expected 2)` | silent | REF-only |
| `Widget.new("a", 1)` | silent | silent | — control |
| `Widget.new("a", 1, 2)` | `wrong-arity` c8 `(given 3, expected 2)` | silent | REF-only |
| `Widget.build()` | `wrong-arity` c8 `(given 0, expected 1)` | silent | REF-only |
| `Widget.build("a","b")` | `wrong-arity` c8 `(given 2, expected 1)` | silent | REF-only |
| `Outer::Inner.new` | `wrong-arity` c14 `(given 0, expected 1)` | silent | REF-only |
| `Outer::Inner.new("a")` | silent | silent | — control |
| `Outer::Inner.new("a","b")` | `wrong-arity` c14 `(given 2, expected 1)` | silent | REF-only |
| `Widget.frobnicate_zzz` | `undefined-method` c8 `for singleton(Widget)` | same | **BOTH** — the receiver typing substrate is already correct here |

The census cannot see any of this: `harness/fp_audit.py` runs both sides
core+stdlib only, so a green sweep says nothing about project-`sig/` behaviour.
It is real prize, unmeasured in size.

---

## 2. The prize, counted from the census

`census_v038.json` (799 rows) copied into the worktree. **128** rows carry
`call.wrong-arity` (18) or `call.argument-type-mismatch` (110). Every one was
opened at `path:line` and classified from the source, not from the census's
`recv` rendering.

**39 rows are singleton-side; 89 are instance-side.** The 89 are dominated by
one shape: `opt.separator nil` / `opt.accept X` on an `OptionParser` **instance**
(75 rows across `rdoc/ri/driver.rb` and `rubygems…/options.rb`), plus
`row['close']` on Array (4), `actor << nil` (3), `@count >= @expected` (3),
`ENV[…]` (1), `ims < last_modified` (1), and two chained `.select(…)` rows.
The census's `recv` field renders a singleton receiver two different ways
(`Gem::Specification` and `Bundler::LockfileParser:`), so it is not usable as
the discriminator — hence the by-hand pass.

**No row could not be classified.** No duplicate `(path, line, message)` exists;
the dependabot `v2` / `v4` helper trees are genuinely separate files.

### 2a. The 39 singleton rows, by blocker

| blocker | rows | receivers |
|---|---|---|
| **B1** — the port does not know the class at all | **9** | `RDoc::Comment` 3, `RDoc::Constant` 2, `RDoc::Markup::Document` 1, `RDoc::Markup::Table` 1, `RDoc::NormalModule` 1, `RDoc::Stats` 1 |
| **B2** — needs a qualified-singleton chain walk (port has `knows_qualified_class` only) | **18** | `Bundler::LockfileParser` 8, `Gem::Specification` 4, `Psych::DisallowedClass` 4, `DidYouMean::Formatter` 1, `Redis::Cluster` 1 |
| **B3** — short-key reachable; needs only the singleton arity / ATM path | **12** | `Redis` 2, `Socket` 2, `Shellwords` 2, `Base64` 1, `CGI` 1, `FileUtils` 1, `StringIO` 1, `Time` 1, `UNIXSocket` 1 |

By corpus: mail 20, dependabot-core 12, gitlab-foss/lib 4, net-ssh 3.
By rule: 16 `wrong-arity`, 23 `argument-type-mismatch`.
By method: 31 are `.new`, 8 are not (`Base64.urlsafe_decode64`, `CGI.parse`,
`FileUtils.options_of`, `Time.parse`, `Shellwords.split` ×2,
`Socket.getnameinfo` ×2).

**B1 is not a singleton problem at all.** `RDoc::*` reaches the reference through
`rbs-4.2.0/sig/rdoc/rbs.rbs` — the **`rbs` gem's own `sig/`**, which the
reference loads because `"rbs"` is in `DEFAULT_LIBRARIES` and
`RBS::EnvironmentLoader` resolves that name to the installed gem's `sig/`. The
port's vendored tree is the rbs **stdlib** closure and
`crates/rigor-index/vendor/rbs/PROVENANCE.md` records that `prism` and `rbs` are
silently skipped ("a lib whose dir is absent is skipped"). These 9 rows close
only by vendoring `rbs`'s own `sig/`, which is a different (and much larger)
decision. Confirmed: `RDoc::Stats.frobnicate_zzz()` is REF-only; the port's
`knows_qualified_class("RDoc::Stats")` is `false`.

### 2b. What is confidently closable

Of B3's 12, only **4** are arity rows whose envelope I could verify agrees
between the engines:

| row | site | reference | port `initialize` arity |
|---|---|---|---|
| gitlab `lib/gitlab/redis/queues.rb:77` | `::Redis.new(params)` | `wrong-arity (given 1, expected 0)` | `0..0` ✓ |
| gitlab `lib/gitlab/redis/wrapper.rb:116` | `::Redis.new(config)` | same | `0..0` ✓ |
| net-ssh `lib/net/ssh/transport/packet_stream.rb:44` | `Socket.getnameinfo(sockaddr, …)` | `wrong-arity (given 2, expected 1)` | (singleton `def self.getnameinfo`, envelope `1..1`) ✓ |
| net-ssh `…/packet_stream.rb:66` | `Socket.getnameinfo(addr, …)` | same | ✓ |

The other 8 B3 rows are ATM and each depends on the port producing the *argument*
type the reference produces — `String?`, `Dynamic[top]?`,
`"less" \| "more" \| … \| nil`. **That axis was not measured here**, so they are
not counted. Two of them (`Shellwords.split` ×2) additionally need the alias fix
of §1d.

The 18 B2 rows are reachable once the substrate exists: `Bundler::LockfileParser`
carries `def initialize: (String) -> void` in the port's own
`vendor/rbs/overlay/rbs_shims/bundler.rbs`, and `Gem::Specification`,
`Psych::DisallowedClass`, `DidYouMean::Formatter`, `Redis::Cluster` all resolve
their `.new` through an **inherited** `initialize` (`Exception#initialize` /
`BasicObject#initialize`) that only a qualified superclass walk can reach.

**So: floor 4, ceiling 22 (4 + 18) inside this census, plus an unmeasured
project-`sig/` prize.** #124's "23 unambiguously `.new`, and 23 is a floor" is
the wrong shape of claim: 31 rows are `.new`, but the number a singleton-rule
slice can actually close is 4.

---

## 3. The predicate design

### 3a. `qualified_class_has_singleton_method`

**It already exists** — `crates/rigor-index/src/rbs.rs:966`, private, and
`class_has_singleton_method` already routes to it (`rbs.rs:1388`) for any name
absent from the short-key `classes` map but present in `qualified`. What does
not exist is its class arm: the function early-returns `true` for anything that
is not a module —

```rust
// Stay silent on qualified classes until the chain walk lands.
if !entry.is_module { return true; }
```

— with a recorded measurement behind it: witnessing absence on a qualified class
over-fired **36 FPs on dependabot-core, all `singleton(Gem::Specification)`**,
because the singleton class inherits class methods down the superclass chain and
that chain is not walked over the qualified registry.

Measured today over the whole reference vocabulary (1368 declared class names,
164 top-level / 1204 namespaced): the port answers the conservative `true` for
`frobnicate_zzz` on **1142** of them — **1139 of the 1204 namespaced** (94.6%)
and only 3 of the 164 top-level. So the issue's "441 of 651" is not a quirk of
one vocabulary; the qualified singleton surface is vacuous almost everywhere.

**What the class arm must answer.** Mirror the *instance* twin
(`qualified_class_has_method`) exactly, which is the shape whose completeness
argument is already written down and already survives the sweep:

1. `qualified` has no entry for `qname` ⇒ **`true`** (unknown ⇒ silent). This is
   the "absent vs unknown" answer: the predicate never distinguishes them, and
   callers must not try — `knows_qualified_class` is the separate question, and
   an arity caller must gate on it first.
2. `entry.singleton_unbuildable` ⇒ **`true`** (the reference cannot build this
   singleton definition; its emptied tables must not read as proven-absent).
3. Leaf's own `singleton_methods` ∪ `singleton_attr_methods` ∪
   `qualified_singleton_alias_resolves` ⇒ **`true`**.
4. `extend`ed modules' instance methods ⇒ **`true`**; an `extend` target that
   resolves to nothing sets `complete = false`.
5. **New:** walk the **superclass chain over the qualified registry** — the
   singleton class inherits down `superclass` only, *not* through `include`s —
   collecting each ancestor's own `singleton_methods` / aliases / `extend`s.
   `Psych::DisallowedClass → Psych::Exception → RuntimeError → StandardError →
   Exception → Object` is the shape this must traverse. A link that cannot be
   resolved **as written** sets `complete = false`.
6. Base-object surface (`singleton_bases_lookup`) ⇒ **`true`**; its
   `bases_loaded` is a completeness precondition.
7. Witness absence (**`false`**) only when `complete && bases_loaded`.

**A class the index knows only under a short key.** Do not resolve it. The
routing in `class_has_singleton_method` already handles the two clean cases
(short map hit ⇒ short path; qualified-only hit ⇒ qualified path). A bare short
name that is ambiguous must go through `resolve_short_unambiguous`, which
returns `None` for 2+ candidates — and `None` must mean **`true`** (silent), not
a guess. This is the ADR-0042 defect-2 rule and it is what keeps a project
`Status` from inheriting `Process::Status`'s surface.

**`extend`ed modules.** Two distinct failure modes, both must stay silent rather
than witness: an `extend` target stored short that resolves to the wrong leaf
(the `Digest::Base` shape), and an `extend` target not in the set at all. Both
are already handled by the `complete = false` arm in the module branch; the
class arm must use the same discipline for every ancestor's `extend` list, not
just the leaf's.

**Base-object surface: one bug to fix while here.** `singleton_bases_lookup`
unions `Class`/`Module`/`Object`/`Kernel`/`BasicObject` unconditionally. A
**module**'s class object is a `Module`, not a `Class`, so it does not respond to
`new` — but the port answers `class_has_singleton_method("Math", "new") == true`
for all 45 module names measured in §4. Oracle: `Math.new` /
`Base64.new` / `FileUtils.new` / `Comparable.new` / `Kernel.new` /
`JSON.new` / `Gem.new` / `Bundler.new` / `Digest.new` / `Marshal.new` are each
`call.undefined-method` in the reference and **silent in the port today**. Under
a naive arity implementation they become `call.wrong-arity` — the same site, a
*different rule id*, i.e. a parity-key false positive that a count-based check
would not see. Fix: when `entry.is_module`, drop `Class` from `BASES`.

### 3b. `.new` is not a lookup, it is a derivation

The single most important design fact, and the one #124 does not mention. The
reference does not read a `def self.new` from RBS — it asks the rbs gem for
`singleton_definition(class).methods[:new]`, and `RBS::DefinitionBuilder#
build_singleton` produces that. Traced to its source file for three classes:

- `Gem::Specification.new` → `core/basic_object.rbs`
- `RDoc::Stats.new` → `core/basic_object.rbs`
- `Redis.new` → `data/vendored_gem_sigs/redis/redis.rbs`

The rule the measurements support:

> `.new`'s signature is an explicitly declared `def self.new` on the singleton
> superclass chain if any ancestor declares one; **otherwise** it is derived from
> the resolved instance `initialize` — and when no ancestor declares
> `initialize` either, that resolution lands on **`BasicObject#initialize: ()
> -> void`**, giving `.new` the envelope **exactly `0..0`**. Modules never get
> `new` at all.

Consequences the implementer must not get wrong:

- The `Class#new` signature (`(*untyped, **untyped) ?{ … } -> untyped`,
  `core/class.rbs:165`) is **never** what `.new` resolves to on a class — its
  `rest_positionals` would make the envelope `0..∞` and nothing would ever fire.
  Deriving from it is the safe-but-useless direction; deriving from
  `BasicObject#initialize` is the reference's actual behaviour and is where the
  FPs live.
- **188 of the 992 classes for which the reference computes a `.new` envelope
  get `[0,0]`, and 149 of those come from `basic_object.rbs`.** On those classes
  every `Klass.new(anything)` is a reference `wrong-arity` — including
  `Gem::Specification.new(name, version)`, `Redis.new(config)`,
  `DidYouMean::Formatter.new(suggestions)`, `RDoc::Stats.new(store, n, v)`,
  `RDoc::Markup::Document.new(report)`, all of which are **correct Ruby**. Seven
  of the 39 census rows are this. Porting them is a parity gain on a diagnostic
  family that is wrong about the program, and per the standing "upstream
  retractions are FP sources" finding it is exactly the family a later pin bump
  is likely to retract — at which point the port owes the retraction as port
  work. Budget for that before counting the 7.
- An explicitly declared `def self.new` **wins** over the derivation. `Struct`,
  `Ractor`, `TracePoint`, `Tempfile` and `PP` all declare one, and all five
  diverge from their own `initialize` (§4).

### 3c. Interaction with #123

#123 is fixing the **instance-side** qualified ancestor closure in
`crates/rigor-index/src/rbs.rs` — the defect where a late `overlay/` reopen of an
ancestor does not propagate down one more subclass level (26 measured
`(class, method)` holes, `gem` on `Bundler::Dependency` and the
`Nokogiri`/`Resolv`/`PP`/`Gem::LoadError` family). This spec **assumes**:

1. #123 leaves `qualified_ancestors` / `resolve_written_ref` / `collect_qualified`
   in place as the reference-resolution machinery, and only makes them see more.
2. #123 does not weaken the `qcomplete` flag's meaning: `qcomplete == false` must
   continue to mean "some link could not be resolved as written", and every
   consumer must continue to read it as **present ⇒ stay silent**.
3. #123 touches the **include/ancestor** walk. The singleton walk of §3a step 5
   is a *different* traversal (superclass only) and #123 does not provide it.

**What breaks if the assumption is wrong.** If #123's fix flips
`qcomplete` to `true` in cases where a link is resolved *optimistically* rather
than genuinely, then every singleton absence witness and every `.new` envelope
built on top of it inherits that optimism, and the failure mode is an FP, not a
miss. So: **after #123 lands and before step 3 of the build order below, re-run
the 185 625-probe instance diff of
`docs/notes/20260909-declared-unwitnessed-gem-classes.md` §1 and require it at
0 holes.** If #123 instead replaces that machinery outright, §3a step 5 must be
re-derived against whatever replaces it — do not port the steps mechanically.

### 3d. Which instance-side leniencies have singleton analogues

Measured, not assumed:

| instance-side gate | singleton analogue? | evidence |
|---|---|---|
| **project-defined method wins** (`discovered_method?`) | **YES, and it is the biggest one** | `class MyThing; def self.build(a,b); end` then `MyThing.build(1)` ⇒ **both silent**. `class Time; def self.zzz_helper(a); end` then `Time.zzz_helper(1,2,3)` ⇒ **both silent**, while `Time.at()` in the same file still fires. A project `def self.x` must suppress the check for **that name only**, not for the class. |
| **ADR-0033 provenance gate** (project-`sig/` fires, bundled stdlib/gem stays lenient) | **NO — does not apply** | The reference arity-checks `StringIO.new(1,2,3,4)` (bundled stdlib) and `Gem::Specification.new(1,2,3,4)` (bundled gem sig) exactly as it checks project-`sig/` `Widget.new`. Do **not** carry `is_project_sig_class` into the arity gate. |
| **declaration-only-class gate** (`is_declaration_only_class`) | **NO — does not apply** | same evidence; the arity rule's gate is `rbs_class_known?` + `definition_available?` + `trustworthy_signature`, none of which consults declaration-only-ness. |
| **open receiver / ADR-26** (`unauthoritative_inherited_signature?`) | **YES** — the reference shares this helper between `wrong-arity` and `argument-type-mismatch` by construction (`check_rules.rb:1228`) | **not probed** — it needs a plugin that vouches an unbounded surface. Flagged as the one gate in this table taken on source reading rather than measurement. |
| **synthesized stub receiver** (`synthesized_stub_receiver?`) | reference reads it in the undefined-method path; whether it reaches the arity path is via `lookup_method` | **not probed.** |
| **inferred-parameter receiver** (`inferred_param_receiver?`, ADR-67 WD6b) | **YES**, the reference declines first thing in `wrong_arity_diagnostic` | not probed; a receiver typed from a parameter's call-site lower bound must decline. |
| **project subclass of an RBS class** | **YES** | `class MyIO < StringIO; end; MyIO.new(1,2,3,4)` ⇒ **both silent**. |

---

## 4. The FP surface, probed

Instruments: a scratchpad-only crate linking `rigor-index` (not committed; no
`crates/` file touched) that pipes class names through the public `CoreIndex`
API, plus a scratchpad Ruby dump of the reference's `singleton_definition(n)
.methods[:new]` run through the reference's own `compute_arity_envelope`
verbatim. Vocabulary: all **1368** class names the reference's default
environment declares from an empty project root.

### 4a. The naive envelope is wrong on 51 of 164 top-level classes

The obvious implementation reads the port's short-key
`CoreIndex::method_arity(class, "initialize")` as the `.new` envelope. Diffed
against the reference's:

| outcome | count |
|---|---|
| identical envelope | 104 |
| **port envelope NARROWER (⇒ false positive)** | **6** |
| port envelope wider (missed witness only) | 0 |
| **reference DECLINES, port has an envelope (⇒ false positive)** | **45** |
| reference has an envelope, port declines (missed only) | 882 |
| both decline | 331 |

**The 45.** Every one is a **module** — `Math`, `JSON`, `Base64`, `FileUtils`,
`Gem`, `Bundler`, `Digest`, `Marshal`, `Abbrev`, `Benchmark`, `BigMath`,
`DidYouMean`, `Errno`, `Etc`, `FileTest`, `Find`, `Forwardable`, `GC`, `IDN`,
`Mysql2`, `Nokogiri`, `ObjectSpace`, `Observable`, `Open3`, `OpenURI`, `AST`,
`BCrypt`, `MonitorMixin`, `Mutex_m`, … The reference has no `.new` there at all;
the port's short-key ancestor walk defaults a module through `Object` →
`BasicObject` and finds `initialize`, giving `(0,0)`. Combined with the
`class_has_singleton_method` module bug of §3a, the arity path **is reached**
(`sing_new == true` for all 45), so `Math.new(1)` becomes an RS-only
`call.wrong-arity` where the reference emits `call.undefined-method`. Oracle,
verbatim: ``undefined method `new' for singleton(Math)``.

**The 6.** All oracle-confirmed:

| class | reference `.new` | port `initialize` | oracle on the FP shape |
|---|---|---|---|
| `Tempfile` | `0..2` (declared `def self.new`) | `1..3` | `Tempfile.new()` — reference **silent**, naive port would fire |
| `PP` | `0..4` | `0..0` | `PP.new(1,2,3)` — reference emits ATM **only**, no wrong-arity |
| `Struct` | `1..∞` | `0..0` | `Struct.new(:a,:b,:c)` — reference **silent** |
| `Ractor` | `0..∞` | `0..0` | `Ractor.new(1,2,3)` — reference **silent** |
| `TracePoint` | `0..∞` | `0..0` | `TracePoint.new(1,2,3)` — reference **silent** |
| `ConditionVariable` | `0..0` | `1..1` | `ConditionVariable.new` — reference **silent** |

Root cause of all six: the class declares an explicit `def self.new` whose
signature differs from `#initialize`, and the derivation rule of §3b must prefer
it. The `Tempfile` row is the sharpest — `Tempfile.new()` is idiomatic Ruby.

**The 882.** The reference has an envelope and the port has none — the entire
namespaced surface, plus the classes the port does not load. This is the miss
side, and it is why the census prize is 4 and not 22 until the substrate lands.

### 4b. Where else a naive implementation over-fires

Each probed, oracle answer recorded:

- **splat / double-splat / keyword arguments** — reference declines
  (`plain_positional_call?`). Any implementation that counts
  `args.len()` without the same filter fires on `File.new(*args)`,
  `Time.at(**opts)`, `File.new(mode: "r")`. All measured silent in the
  reference. The port's existing `args_all_plain` flag is the right carrier.
- **`Struct.new(…).new(…)` and `Data.define(…).new(…)`** — reference declines
  explicitly (`anonymous_struct_new_call?`). Measured silent.
- **modules** — §4a, 45 names.
- **the module `Class#new` leak** — §3a; also an under-emit today
  (`Math.new` gets no `undefined-method` from the port).
- **project subclasses of RBS classes** — measured silent both sides; the
  qualified/short registry must not let `MyIO < StringIO` inherit
  `StringIO`'s `.new` envelope.
- **the singleton-alias hole in `singleton_method_overloads`** is the *opposite*
  direction (a miss), but the fix is FP-adjacent: resolving an alias must resolve
  to the target's overloads, not merely mark the name present.

### 4c. What the sweep will and will not catch

`fp_audit.py --gaps --sweep` (0 FP / 9204 files, ~80 min at this pin) is the
landing gate, but it is **blind** to three of the surfaces above:

1. the module `.new` FP fires only where a corpus file writes `SomeModule.new`
   — rare;
2. the parity-key FP (`wrong-arity` where the reference says
   `undefined-method`) is invisible to any check that compares *counts* rather
   than `(rule_id, line, column)` keys;
3. the project-`sig/` tier of §1e is not exercised at all — both sides run
   core+stdlib only.

Each needs its own hand-built probe file, run against both engines with the
parity key compared.

---

## 5. Build order

Each step is independently landable and independently gated. Steps 1–2 are
output-inert or output-shrinking; the first output-growing step is 4.

**Step 0 — wait for #123, then re-verify.** Rebuild and re-run the 185 625-probe
instance-side diff of `20260909-declared-unwitnessed-gem-classes.md` §1.
**Gate:** 0 holes, and `qcomplete == false` still means "unresolved link".
If either fails, §3a step 5 must be re-derived — stop.

**Step 1 — `singleton_method_overloads` resolves singleton aliases.**
`crates/rigor-index/src/rbs.rs:2417`. Mirror `class_has_singleton_method`'s
`singleton_aliases` consultation, bounded-recursive, on each link of the
superclass chain. Output-growing but tiny and already-gated by the existing ATM
rule.
**Gate:** `Shellwords.split(1)` / `Dir.pwd(1)` / `Regexp.compile` go BOTH;
`Shellwords.shellsplit(1)` and `Base64.decode64(1)` stay BOTH; full sweep 0 FP.

**Step 2 — fix the module base-surface leak.** In `singleton_bases_lookup` (or
its callers), exclude `Class` when `entry.is_module`. Output-growing in the
`undefined-method` rule only.
**Gate:** `Math.new` / `Base64.new` / `Comparable.new` / `Kernel.new` /
`FileUtils.new` / `Gem.new` / `Bundler.new` / `Digest.new` / `Marshal.new` /
`JSON.new` go BOTH on `call.undefined-method`; `Math.sqrt(1.0)`,
`Base64.decode64("x")`, `FileUtils.mkdir_p("x")` stay silent; sweep 0 FP.
**This step is a hard prerequisite for step 4** — without it the 45-name FP is
live the moment arity fires.

**Step 3 — the qualified singleton chain walk (substrate, output-inert).**
Implement §3a step 5 inside `qualified_class_has_singleton_method` and delete
the `!entry.is_module` early-out. Expose nothing new publicly yet.
**Gate:** re-run the §4 witnessability probe — the 1139-of-1204 conservative-true
count must fall, and **every class that starts witnessing must be diffed against
the reference's own singleton surface at 0 holes** (the same instrument as §1 of
the parent note, singleton side). The recorded 36-FP
`singleton(Gem::Specification)` regression is the exact thing this gate exists to
catch. Ship a `pub fn qualified_class_has_singleton_method` only once that diff
is clean.

**Step 4 — `.new` derivation.** Synthesize the singleton `new` entry at
ingestion time (envelope **and** per-overload param shapes, so both rules are fed
from one place): explicit `def self.new` on the singleton chain wins; otherwise
derive from the resolved instance `initialize`; otherwise
`BasicObject#initialize` ⇒ `0..0`; **never** for a module.
**Gate:** the six §4a rows go silent-both (`Tempfile.new()`, `PP.new(1,2,3)`,
`Struct.new(:a,:b,:c)`, `Ractor.new(1,2,3)`, `TracePoint.new(1,2,3)`,
`ConditionVariable.new`); a fresh full-vocabulary envelope diff vs the reference
at **0 narrower rows and 0 reference-declines rows**; sweep 0 FP.

**Step 5 — the rule: `check_wrong_arity` grows a Singleton arm.**
`crates/rigor-rules/src/lib.rs:1838`. Today `index.class_name_of(interner,
recv_ty)` returns `None` for `Type::Singleton`, which is the whole defect. Add
the arm the ATM rule already has (`source.class_name_for_id(class)` at
`lib.rs:2270`), reading a new `singleton_method_arity` instead of
`method_arity`. Carry the reference's declines: `plain_positional_call?` (the
existing `args_all_plain` flag), `anonymous_struct_new_call?`, the project
`discovered_method?` suppression, ADR-26 `unauthoritative_inherited_signature?`.
Keep the existing `if has_block { return None }` — the reference does *not*
decline on a block (§1c), so this is a knowing under-emit, and widening it is a
separate slice with its own gate.
**Gate:** the four §2b rows go BOTH; the whole §1c decline table stays silent;
`"abc".upcase(1,2,3)` unchanged; sweep 0 FP.

**Step 6 — the project-`sig/` tier.** §1e, hand-built project fixture, not
sweep-visible.

### Must-still-fire controls a future fixture must carry

```ruby
# must FIRE, both engines, same (rule, line, column)
"abc".upcase(1, 2, 3)              # instance-side control, unchanged
Math.frobnicate_zzz()              # module singleton undefined-method
Redis.frobnicate_zzz()             # vendored-gem singleton undefined-method
Comparable.frobnicate()            # module undefined-method
Widget.frobnicate_zzz              # project-sig singleton undefined-method
Math.new                           # step 2: module has no `new`
Base64.decode64(1)                 # existing singleton ATM, must not regress
Shellwords.shellsplit(1)           # existing singleton ATM through no alias

# must stay SILENT, both engines
File.new(*args)                    # splat
Time.at(**opts)                    # double splat
File.new(mode: "r")                # keyword args
Struct.new(:a, :b).new(1, 2)       # anonymous struct chain
Data.define(:x).new(1)             # same family
Struct.new(:a, :b, :c)             # explicit `def self.new`, 1..INF
Tempfile.new()                     # explicit `def self.new`, 0..2
PP.new(1, 2, 3)                    # explicit `def self.new`, 0..4  (ATM only)
Ractor.new(1, 2, 3)                # explicit `def self.new`, 0..INF
TracePoint.new(1, 2, 3)            # explicit `def self.new`, 0..INF
ConditionVariable.new              # 0..0
Math.new(1)                        # module: undefined-method, NOT wrong-arity
FileUtils.new(1)                   # same
Gem.new(1)                         # same
MyIO.new(1, 2, 3, 4)               # class MyIO < StringIO
MyThing.build(1)                   # project `def self.build` wins
Time.zzz_helper(1, 2, 3)           # project reopen of a core class
Frobnicate::Widget.new()           # unknown class
StringIO.new("x")                  # correct call
Math.sqrt(1.0)                     # correct call
Widget.new("a", 1)                 # project sig, correct arity
Outer::Inner.new("a")              # nested project sig, correct arity
```

The parity key must be `(rule_id, line, column)`, not a count — the module
`.new` failure is a *rule swap* at an identical site.

---

## 6. Reproduction

```sh
export PATH=/Users/megurine/.local/share/mise/installs/ruby/4.0.5/bin:$PATH
ruby -e 'require "rbs"; puts "ruby #{RUBY_VERSION} rbs #{RBS::VERSION}"'   # 4.0.5 / 4.2.0
rm -rf reference/rigor
git clone -q --shared --no-checkout <main>/reference/rigor reference/rigor
git -C reference/rigor checkout -q ffb456b0
ruby -I reference/rigor/lib reference/rigor/exe/rigor --version            # rigor 0.3.8
cargo build --offline --release -p rigor-cli                               # REBUILD before comparing
```

Probes ran from a fresh temp cwd (or the fixture project root for §1e) via a
scratchpad runner that exits non-zero on unparseable reference output. The
reference envelope dump used `Rigor::Environment.for_project(root: <empty
tmpdir>)`, `rbs_loader.singleton_definition(n).methods[:new]`, and a verbatim
copy of `check_rules.rb`'s `compute_arity_envelope` / `arity_eligible?`. The
port side used a throwaway crate linking `rigor-index` by path and calling only
the public `CoreIndex` API. Neither script nor the fixture project is committed,
and **no `crates/` file was modified**.
