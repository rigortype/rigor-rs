# Re-pin `v0.3.4 → v0.3.8` — port spec for the retracted diagnostics (2026-09-09)

**Status: implementation spec.** The mechanical half of the bump is commit
`53ea933` on branch `upstream-pin-v0.3.8` (submodule `ffb456b0` = tag `v0.3.8`,
rbs 4.2.0, plugin sig, effects catalogue, #437 divergences retired, snapshots
regenerated). At that commit `ruby harness/run.rb` reports **7 unregistered false
positives**, every one an upstream RETRACTION (a diagnostic the reference stopped
emitting), bisected to four upstream commits. This note is the mini-spec for
porting those four retractions; the bump note proper follows once the gates are
green.

Every table below was measured 2026-09-09 against the PINNED reference at
`ffb456b0` (one fresh temp cwd per run, `--no-cache`, both `-I` libs — `UPSTREAM.md`
hazard 1) and against the port at `53ea933`. `RS` = the port fires and the oracle
does not (a false positive to close); `BOTH` = both fire (a must-still-fire
control); `REF` = the oracle fires and the port does not (a coverage gap — leave it).
The probe files are reproduced in full under § 6 so the rows can be re-run.

## 0. Operating rules for the implementer

- Work in a git worktree branched from `upstream-pin-v0.3.8` at `53ea933` or later.
  Populate the reference OFFLINE, never from the network and never via
  `REFERENCE_RIGOR_DIR` (`UPSTREAM.md` hazard 3):
  ```sh
  rm -rf reference/rigor
  git clone -q --shared --no-checkout /Users/megurine/repo/rust/rigor-rs/reference/rigor reference/rigor
  git -C reference/rigor checkout -q ffb456b0
  ruby -I reference/rigor/lib reference/rigor/exe/rigor --version   # -> rigor 0.3.8
  ```
- Every oracle probe: `ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib reference/rigor/exe/rigor check <file> --format json --no-cache`
  from a fresh temp cwd. Every port probe: `target/debug/rigor check <file> --format json`.
- The harness REFUSES a binary older than any file under `crates/` — rebuild
  (`cargo build --offline -p rigor-cli`) before `ruby harness/run.rb`.
- Port the SCOPE of each retraction, not its identity half; prefer allow-lists;
  the must-still-fire controls are load-bearing (a fix that silences a `BOTH` row
  is wrong even if the harness goes green). Try to make the gate lie before
  trusting it: for each family, write at least one control that the naive
  over-broad fix WOULD silence, and show both engines still fire there.
- Gates before reporting: `cargo test --workspace --offline`;
  `CARGO_TARGET_DIR=/tmp/rigor-clippy-target-<family> cargo clippy --workspace --offline -- -D warnings`
  (a FRESH target dir — the incremental cache hides warnings CI rejects);
  `ruby harness/snapshot.rb` for any NEW fixture, then `ruby harness/run.rb` and
  `ruby harness/run_snapshot.rb`. Your family's rows must show 0 unregistered
  extras; report any remaining extras from the OTHER families verbatim.
- New fixtures follow `harness/corpus/97_defined_operand_not_evaluated.rb`'s header
  convention: every firing line and every silent control is oracle-measured at the
  pin, and the header says so. Number them from 99.
- Commit on the worktree branch. Do not push, do not open a PR. Report: branch,
  commit, the `run.rb` SUMMARY block, and the residues you left.

## 1. F-A — an untyped argument must not pin one overload (upstream #521 / PR #537, `3d5dddbb`)

**Upstream semantics** (`lib/rigor/inference/method_dispatcher/overload_selector.rb`,
`rbs_dispatch.rb#join_candidate_returns`): when any argument is UNTYPED
(`Dynamic[Top]` — the literal untyped carrier, not a `Dynamic[T]` with a facet) the
strict pass and the alias pass decline, the gradual pass returns EVERY overload that
matches arity + block shape, and the dispatch joins their returns: one candidate → its
return; all returns identical → that return; otherwise `Dynamic[union]`, on which no
negative rule fires. Fixture rows retracted: `60:59` (`@x = Float(kwargs[:k])` no
longer types `Float`, so the `rescue` write of `0` is no mismatch), `67:36`
(`Array(config).presence`), `67:59` (`rand(n).frobnicate`).

**Port site**: `crates/rigor-infer/src/lib.rs`, the Kernel fold path (`is_kernel_fold`,
≈ lines 1330–1470). `Float`, `Integer` (1- and 2-arg), `Array` and `rand` fall to a
NOMINAL fallback for a non-pinned argument; that fallback must now decline (answer
`Dynamic[top]`) when any argument's type is untyped. `String(u)` and `format(fmt, u)`
keep their answer (single-overload / agreeing returns — see rows a3, a9). Then audit the
generic RBS dispatch for the same pinning: rows a15 and a17 were still being bisected
when this was written — attribute them yourself (`git -C reference/rigor bisect` between
`v0.3.4` and `v0.3.8` with the probe below takes ~10 runs) and port whatever they
attribute to, or record them as a separate root cause.

| row | shape | oracle `ffb456b0` | port `53ea933` | verdict |
|---|---|---|---|---|
| a1 | `Float(u).typo` (u untyped) | silent | fires `for Float` | **RS — close** |
| a2 | `Integer(u).typo` | silent | fires `for Integer` | **RS — close** |
| a3 | `String(u).typo` | fires `for String` | fires | BOTH — control |
| a4 | `Array(u).typo` | silent | fires `for Array` | **RS — close** |
| a5 | `rand(u).typo` | silent | fires `for Integer` | **RS — close** |
| a6 | `Float("1.5").typo` | fires `for 1.5` | fires | BOTH — control |
| a7 | `s = "x" if s.nil?; Float(s).typo` | fires `for Float` | fires | BOTH — control (typed arg keeps the pick) |
| a8 | `rand(5).typo` | fires `for Integer` | fires | BOTH — control |
| a9 | `format("%d", u).typo` | fires `for String` | fires | BOTH — control |
| a13 | `"abc".center(u).typo` | fires `for String` | fires | BOTH — control (overloads agree) |
| a14 | `[1, 2].first(u).typo` | fires `for Array[1 \| 2]` | silent | REF — gap, leave |
| a15 | `"abc"[u].typo` | silent | fires `for String` | **RS — attribute, then close** |
| a16 | `[1, 2][u].typo` | silent | silent | both silent |
| a17 | `Integer(u, 16).typo` | silent | fires `for Integer` | **RS — attribute, then close** |
| a18 | `@dur = Float(u)` / `rescue; @dur = 0` | silent | `def.ivar-write-mismatch` | **RS — close** (fixture 60:59) |

Fixture work: fixtures 60 and 67 keep their lines (snapshots already regenerated) but
their comments claim the old behaviour — rewrite them. Add a fixture for the a-rows.

## 2. F-B — `is_a?` against a class the environment cannot order widens to Dynamic (upstream #533 item 4, `70ca7e74`)

**Upstream semantics** (`lib/rigor/inference/narrowing.rb#narrow_nominal_to_class`):
the `:unknown` ordering arm — the guard class cannot be ordered against the Nominal
carrier — now answers `untyped` ("the guard proved membership in a class the engine
cannot name, which destroys the old knowledge"). `:subclass` keeps the bound,
`:superclass` narrows, `:disjoint` is `Bot` — unchanged. Consequences the rows show:
the truthy edge is Dynamic, the early-return fall-through is Dynamic, and **the
post-guard join is Dynamic** (`Dynamic ∪ Array`), so a call AFTER the `if` is silent
too. The falsey edge is untouched. Fixture row retracted: `86:134`.

**Port site**: `crates/rigor-infer/src/lib.rs` `narrow_nominal_to_class` (≈ 3781) — the
`ClassOrdering::Unknown` arms (single-class and `||`-union), and the chain twin
(`c.chains`, ≈ 3734). Today `Unknown` either keeps the carrier (a project-known name) or
drops the fact (`None`); both leave the carrier TYPE in force, so the call witnesses.
Design constraint, not a suggestion: `ClassFact::Bot` is the identity at a join and
feeds the Bot-on-entry logic (≈ 5141–5174) — reusing it would be sound for the call but
wrong at the join and risks an unreachable verdict. Add a distinct widening fact (name
it for what it is) that (a) suppresses every receiver-typed witness on the local on
that edge, like `Bot`; (b) SURVIVES the join — widened ∪ anything = widened; (c) is
cleared by a write to the local; (d) feeds no `Bot`-derived verdict. A union guard with
one unorderable member widens the whole edge. `instance_of?` (`g.exact`) stays `Bot`.

Re-probe the port's existing `projsub` / `chain_projsub` / `projsub_or` test rows
against `ffb456b0` before touching them: the reference's `:unknown` arm no longer
distinguishes a project class, but the reference may ORDER a project class through its
own discovered hierarchy (row b6 is silent on both sides — a `ProjKlass < Hash` guard on
a `String` carrier is provably disjoint). Measure, then update the rows to what the
oracle says.

| row | shape (carrier `h = Array.new`) | oracle | port | verdict |
|---|---|---|---|---|
| b1 | `h.typo if h.is_a?(UnknownZzz)` | silent | fires | **RS — close** (86:134) |
| b2a/b2b | `h.t1 if h.is_a?(UnknownZzz); h.t2` | silent, silent | fires, fires | **RS — close BOTH** (the join) |
| b3 | `return unless h.is_a?(UnknownZzz); h.typo` | silent | fires | **RS — close** |
| b4 | `h.typo if h.is_a?(UnknownZzz) \|\| h.is_a?(Hash)` | silent | fires | **RS — close** |
| b5 | `h.typo unless h.is_a?(UnknownZzz)` | fires `for Array` | fires | BOTH — control (falsey edge) |
| b6 | `s = String.new; s.typo if s.is_a?(ProjKlass)` (`ProjKlass < Hash`) | silent | silent | both silent |
| b7 | `h = [1, 2]; h.typo if h.is_a?(UnknownZzz)` (Tuple carrier) | silent | silent | both silent |
| b8 | `case h when UnknownZzz then h.typo end` | silent | fires | **RS — close** |
| b9 | `h.typo if UnknownZzz === h` | silent | fires | **RS — close** |
| b10 | `h.typo if h.is_a?(Comparable)` | silent | silent | both silent (disjoint ⇒ Bot) |
| b11 | `h.typo if h.is_a?(Enumerable)` | fires `for Array` | fires | BOTH — control (subclass keeps the bound) |
| b12 | `h.typo if h.kind_of?(UnknownZzz)` | silent | fires | **RS — close** |
| b13 | `h.typo if h.instance_of?(UnknownZzz)` | silent | silent | both silent (exact ⇒ Bot) |

Also probe, and pin in the fixture: reassignment after the guard
(`h.t if h.is_a?(U); h = Array.new; h.typo` — expect fires on both), and the guard
inside a block / loop.

## 3. F-C — a mixin-module receiver, and a `Class`/`Module` value, are not enumerable (upstream #739 / PR #741 `3636649f`; #742 / PR #743 `23341a87`)

**Upstream semantics** (`lib/rigor/analysis/check_rules.rb`): an INSTANCE-side receiver
whose Nominal class is an RBS MODULE declines `call.undefined-method` outright
(`module_mixin_receiver?` → `environment.rbs_module?`; it used to retry the lookup
against `Object`). A receiver typed `Class` or `Module` (Nominal — `METACLASS_ARMS`)
declines too. `x.class.typo` where `x` is mixin-typed declines (keyed on the `.class`
SYNTAX, not the type). The SINGLETON side is untouched: `Comparable.typo`,
`Digest::Instance.typo`, `String.typo` keep firing. Only `call.undefined-method` moved;
probe `call.wrong-arity` on a module receiver (`v.hexdigest(1, 2, 3)` after a
`Digest::Instance` guard) on both engines and pin whatever the oracle says. Fixture row
retracted: `91:42`.

**Port site**: `crates/rigor-rules/src/lib.rs` `check_call` (≈ 1307+): after the
instance receiver's class NAME is resolved (both the core-id path and the
source-registry path), decline when `index.is_module(name)` (`rigor-index/src/rbs.rs:1180`
— verify it answers for a qualified name such as `Digest::Instance` and for `Kernel`,
`Enumerable`, `Comparable`) or when the name is exactly `Class` / `Module`. Keep the
`Type::Singleton` branch untouched.

| row | shape (`def f(v)` + `return unless v.is_a?(…)`) | oracle | port | verdict |
|---|---|---|---|---|
| c1 | `Digest::Instance` → `v.typo` | silent | fires | **RS — close** (91:42) |
| c2 | `Enumerable` → `v.typo` | silent | fires | **RS — close** |
| c3 | `Comparable` → `v.typo` | silent | fires | **RS — close** |
| c4 | `Digest::Instance.typo` (singleton) | fires | fires | BOTH — control |
| c5 | `Comparable.typo` (singleton) | fires | fires | BOTH — control |
| c6 | `Digest::Instance` → `v.class.typo` | silent | silent | both silent — pin as control |
| c7 | `Class` → `v.typo` | silent | fires `for Class` | **RS — close** |
| c8 | `Module` → `v.typo` | silent | fires `for Module` | **RS — close** |
| c9 | `String` → `v.typo` | fires | fires | BOTH — control |
| c10 | `Kernel` → `v.typo` | silent | fires `for Kernel` | **RS — close** |
| c11 | `String.typo` (singleton) | fires | fires | BOTH — control |
| c12 | `Digest::Instance` → `v.hexdigest.typo` | fires `for String` | silent | REF — gap, leave |
| c13 | project `module ProjMix` → `v.typo` | silent | silent | both silent |

## 4. F-D — a constant-write meta-new block is the class body it is (upstream #590 / PR #619, `b3d688f7`)

**Upstream semantics**: `Const = Class.new do … end` (and `Module.new` /
`Struct.new(…) do` / `Data.define(…) do`, and the `ConstantPathWrite` spelling) now
enters its block as the constant's class body, so `call.unresolved-toplevel` cannot fire
inside it — the position the `v0.3.4` port deliberately carved OUT because the reference
still fired there. Fixture rows retracted: `96:70`, `96:74`.

**Port site**: `crates/rigor-rules/src/lib.rs` `meta_new_block_body_spans` (≈ 946):
delete the `constant_write_values` carve-out and the doc paragraph that justifies it;
flip the test `unresolved_toplevel_fires_in_constant_assigned_meta_class_body` to the
silent expectation (rename it). Residues where the ORACLE still fires and the port is
silent — `X = Class.new do … end.freeze` and `X ||= Class.new do … end` (the rvalue is not
the meta-new call) — are coverage gaps: record them, do not chase them.

| row | shape | oracle | port | verdict |
|---|---|---|---|---|
| d1 | `Registry = Class.new do attr_reader :e end` | silent | fires | **RS — close** (96:70) |
| d2 | `Coercible = Module.new do attr_reader :r end` | silent | fires | **RS — close** (96:74) |
| d3 | `Line = Struct.new(:text) do def shout; text.upcase; end end` (`text`) | silent | fires | **RS — close** |
| d4 | `Point = Data.define(:x) do def dbl; x * 2; end end` (`x`) | silent | fires | **RS — close** |
| d5 | `Outer::Inner = Class.new do attr_reader :z end` | silent | silent | both silent |
| d6 | `Frozen = Class.new do attr_reader :f end.freeze` | fires | silent | REF — gap, leave |
| d7 | `Lazy \|\|= Class.new do attr_reader :l end` | fires | silent | REF — gap, leave |
| d8 | `Base = Class.new(StandardError) do attr_reader :b end` | silent | fires | **RS — close** |
| d9 | `module Wrap; Inner2 = Class.new do attr_reader :w end; end` | silent | silent | both silent |
| d10 | `Plain = Class.new(parent_of(1)) do …` — `parent_of` (argument position) | fires | fires | BOTH — control |
| d10' | … its body `attr_reader :p` | silent | fires | **RS — close** |
| d11 | `some_dsl_call do attr_reader :d end` (#316 DSL block) | fires ×2 | fires ×2 | BOTH — control |

## 5. What the sweep may add

The 9204-file standing sweep (`python3 harness/fp_audit.py --gaps --sweep`, release
binary) was still running when this spec was written. Its FP list is classified in the
bump note; rows that fall into families 1–4 are closed by this work, anything else gets
its own section there. Candidates from the `v0.3.7` `Fixed` list that the fixture corpus
cannot see: #627 dead version-guard arms (mail's `yaml.rb`), #546 block-taking calls on a
folded `Set` constant, #545/#507/#504 `||=`-filled collections no longer folding
`empty?`/`size`, #559 index writes on an unshapeable receiver, #554 `extend` /
`module_function` surfaces, #652/#685 compact-namespace constant resolution.

## 6. The probe files

`fa_overload.rb`, `fb_guard.rb`, `fc_module.rb`, `fd_constbody.rb` — verbatim, so the
tables above can be regenerated with the side-by-side runner
(`probe_diff.py <ref-checkout> <rs-bin> <file>…`, which prints `REF`/`RS`/`BOTH` per
`(rule, line, column)` for parity severities). Row ids in the tables are the `frobnicate_<id>`
selectors.

```ruby
# fa_overload.rb
def a1(u) = Float(u).frobnicate_a1
def a2(u) = Integer(u).frobnicate_a2
def a3(u) = String(u).frobnicate_a3
def a4(u) = Array(u).frobnicate_a4
def a5(u) = rand(u).frobnicate_a5
def a6 = Float("1.5").frobnicate_a6
def a7(s)
  s = "x" if s.nil?
  Float(s).frobnicate_a7
end
def a8 = rand(5).frobnicate_a8
def a9(u) = format("%d", u).frobnicate_a9
def a11(u) = ([true] * u).frobnicate_a11
def a13(u) = "abc".center(u).frobnicate_a13
def a14(u) = [1, 2].first(u).frobnicate_a14
def a15(u) = "abc"[u].frobnicate_a15
def a16(u) = [1, 2][u].frobnicate_a16
def a17(u) = Integer(u, 16).frobnicate_a17
def a18(u)
  @dur = Float(u)
rescue ArgumentError
  @dur = 0
end
```

```ruby
# fb_guard.rb  (each in its own `def`; `h = Array.new` unless stated)
h.frobnicate_b1 if h.is_a?(UnknownZzzClass)
h.frobnicate_b2a if h.is_a?(UnknownZzzClass); h.frobnicate_b2b
return unless h.is_a?(UnknownZzzClass); h.frobnicate_b3
h.frobnicate_b4 if h.is_a?(UnknownZzzClass) || h.is_a?(Hash)
h.frobnicate_b5 unless h.is_a?(UnknownZzzClass)
class ProjKlass < Hash; end; s = String.new; s.frobnicate_b6 if s.is_a?(ProjKlass)
h = [1, 2]; h.frobnicate_b7 if h.is_a?(UnknownZzzClass)
case h; when UnknownZzzClass then h.frobnicate_b8; end
h.frobnicate_b9 if UnknownZzzClass === h
h.frobnicate_b10 if h.is_a?(Comparable)
h.frobnicate_b11 if h.is_a?(Enumerable)
h.frobnicate_b12 if h.kind_of?(UnknownZzzClass)
h.frobnicate_b13 if h.instance_of?(UnknownZzzClass)
```

```ruby
# fc_module.rb  (each `def cN(v); return unless v.is_a?(<guard>); <call>; end`)
c1  Digest::Instance  v.frobnicate_c1
c2  Enumerable        v.frobnicate_c2
c3  Comparable        v.frobnicate_c3
c4  (none)            Digest::Instance.frobnicate_c4
c5  (none)            Comparable.frobnicate_c5
c6  Digest::Instance  v.class.frobnicate_c6
c7  Class             v.frobnicate_c7
c8  Module            v.frobnicate_c8
c9  String            v.frobnicate_c9
c10 Kernel            v.frobnicate_c10
c11 (none)            String.frobnicate_c11
c12 Digest::Instance  v.hexdigest.frobnicate_c12
c13 module ProjMix; def pm; end; end  → v.is_a?(ProjMix); v.frobnicate_c13
```

```ruby
# fd_constbody.rb
Registry = Class.new do
  attr_reader :entries
end
Coercible = Module.new do
  attr_reader :raw
end
Line = Struct.new(:text) do
  def shout
    text.upcase
  end
end
Point = Data.define(:x) do
  def dbl
    x * 2
  end
end
Outer::Inner = Class.new do
  attr_reader :z
end
Frozen = Class.new do
  attr_reader :f
end.freeze
Lazy ||= Class.new do
  attr_reader :l
end
Base = Class.new(StandardError) do
  attr_reader :b
end
module Wrap
  Inner2 = Class.new do
    attr_reader :w
  end
end
Plain = Class.new(parent_of(1)) do
  attr_reader :p
end
some_dsl_call do
  attr_reader :d
end
```
