# Re-pin `v0.3.4 → v0.3.8` — the two INFERENCE families, ported (2026-09-09)

Implements § 1 (F-A) and § 2 (F-B) of
[`20260909-repin-v038-port-spec.md`](20260909-repin-v038-port-spec.md). Both
families are `crates/rigor-infer`; § 3 (F-C) and § 4 (F-D) are rules-layer work
and were done in parallel by another agent.

Everything below was measured against the PINNED reference at `ffb456b0`
(= `v0.3.8`), one fresh temp cwd per run, `--no-cache`, both `-I` libs
(`UPSTREAM.md` hazard 1). Row ids are the spec's; rows without one are new here.

**Result on the fixture harness**: the four unregistered false positives this
work owns — `60:59`, `67:36`, `67:59`, `86:134` — are closed, with **no matched
diagnostic lost** (402 matched before and after on the 98-fixture corpus; 431/470
with the two new fixtures). The three that remain, reported verbatim, belong to
the other agent's families:

```
91_qualified_witnessing.rb    call.undefined-method     @ line 42, col 5
96_anonymous_meta_class_body.rb  call.unresolved-toplevel @ line 70, col 3
96_anonymous_meta_class_body.rb  call.unresolved-toplevel @ line 74, col 3
```

## F-A — an untyped argument must not pin one overload (#521 / `3d5dddbb`)

### a15 and a17: the attribution the spec left open

Bisected with `git -C reference/rigor bisect start v0.3.8 v0.3.4` over a probe
script (exit 0 = fires, 1 = silent, 125 = the reference cannot run), with the
**rbs gem pinned** so the gem bump is not a confound — `gem list rbs` shows
4.2.0, 4.1.3, 4.1.1 … installed, and a plain `ruby -I` resolves the newest.

* **a17** (`Integer(u, 16).typo`) — fires at `v0.3.4` under BOTH rbs 4.1.1 and
  4.2.0, so it is a rigor-logic retraction. 9 bisect steps over 812 commits:
  **`3d5dddbb` is the first bad commit** — the same #521 commit as a1/a2/a4/a5.
  Ported with them; no separate root cause.
* **a15** (`"abc"[u].typo`) — **out of scope, and not a re-pin retraction at
  all.** It is already SILENT on the reference at `v0.3.4` (under rbs 4.1.1 and
  4.2.0 alike) and at `v0.3.0`, so it never entered the `v0.3.4..v0.3.8` window.
  It is a long-standing rigor-rs false positive in the GENERIC RBS dispatch
  (`String#[]`'s arms return `String?` and `String`, and the port pins the
  first). Closing it means porting #521's `join_candidate_returns` into the
  generic dispatch, which the spec explicitly forbids without a measured oracle
  row inside the window. **a17 does not supply one** — it is a Kernel fold, and
  the Kernel decline closes it. Left as a recorded residue.

### The gate the spec specified is wrong for this port — measured

The spec (and the task) said: decline when any argument's type is the literal
untyped carrier, `Type::Dynamic(top)`. Implemented exactly that way, it silenced
**four of the spec's own must-still-fire controls** plus five more:

| row | shape | oracle | naive gate |
|---|---|---|---|
| a7 | `s = "x" if s.nil?; Float(s).typo` | fires `for Float` | **silent** |
| a20 | `s = "x" if s.nil?; Array(s).typo` | fires | **silent** |
| a25 | `return unless s.is_a?(String); Integer(s, 16).typo` | fires | **silent** |
| a31 | `return unless s.is_a?(Integer); rand(s).typo` | fires | **silent** |
| q2/q3/q4 | a local rebound from a literal, `\|\|=`, or a plain `s = "x"` | fires | **silent** |
| q6/q8 | a parameter narrowed by an `is_a?` guard | fires | **silent** |

The cause is not upstream: it is that **`Type::Dynamic(top)` does not mean
"untyped" on this side of the fold**. A use site inside a method body reads an
EMPTY `TypeEnv` (`ScopedEnv::at`, `crates/rigor-rules/src/lib.rs` — a Ruby method
body is an independent local scope, and reading the flat top-level env there was
itself a measured 4-diagnostic false positive on rigor-survey). So EVERY def-body
local read answers `Dynamic[top]`, and `def f(u) = Float(u)` (reference-SILENT)
and `s = "x"; Float(s)` (reference-FIRING) are literally the same `TypeId` at the
fold. Upstream's `untyped_arg?` is not portable as written.

**This is the fourth time "we do strictly less than the reference" has failed in
this repo, and the first where the naive form was in the spec itself.**

### What was built instead

`Typer::arg_is_reference_untyped` — a syntactic ALLOW-LIST for "the reference
would call this untyped too". The argument's ROOT is a bare local (walking down
call receivers, so `kwargs[:upload_duration]` roots at `kwargs`), it sits inside
a `def`, and inside that def's span the root is

* never CLASS-GUARDED — no `is_a?` / `kind_of?` / `instance_of?` / `===` naming
  it, no `case` on it (a truthiness guard is deliberately NOT on the list:
  `return unless u`, `if u` and `return if u.nil?` are all measured silent on
  both engines, because `Dynamic` minus nil is still `Dynamic`); and
* either never REBOUND, or rebound only from values that are themselves untyped
  by the same test, recursively (depth 4, with a seen-set so `t = t.foo`
  terminates). `t = s.to_s` over an untyped `s` therefore declines (row q5, a
  PRE-EXISTING false positive this closes as a bonus), while `s = "x" if s.nil?`
  does not — the reference's carrier there is the union `"x" | Dynamic[top]`,
  which it still discriminates on. `x ||= v` declines outright.

Applies to `Float`, `Integer` (1- and 2-arg), `Array` and `rand`. `String` and
`format`/`sprintf` are NOT on the list — one matching overload each, so
upstream's join IS that return and both engines keep firing (a3/a9/a32/a33).
`Hash` is not either: `Hash(u)` is a REF row (a26), a coverage gap, not an FP.

The fold ANSWERS `Dynamic[top]` rather than returning "no answer", because the
explicit `Kernel.Float(u)` spelling routes to the same fold and a decline there
falls through to the singleton-RBS tier, which would re-pin the return (row a19,
reference-silent, newly measured).

Every withholding of this predicate is a coverage loss, never a false positive.
Known ones, recorded not chased: an IVAR argument (the arena's `VariableRead`
carries no name), a local shadowed by a block parameter (block params are not
arena writes), a top-level use site, and a def whose parameter list the arena
declines.

### F-A rows measured beyond the spec's table

| row | shape | oracle | port after |
|---|---|---|---|
| a19 | `Kernel.Float(u).typo` | silent | silent ✓ (was RS) |
| a20 | `s = "x" if s.nil?; Array(s).typo` | fires `for ["x" \| Dynamic[top]]` | fires ✓ |
| a21 | `Array([1, 2]).typo` | fires `for Array[1 \| 2]` | fires ✓ |
| a22 | `Array(5).typo` | fires `for Array[5]` | fires ✓ |
| a23 | `rand.typo` | fires `for Float` | fires ✓ |
| a24 | `Integer("1f", 16).typo` | fires `for 31` | fires ✓ |
| a25 | `is_a?(String)` then `Integer(s, 16).typo` | fires `for Integer` | fires ✓ |
| a26 | `Hash(u).typo` | fires `for Hash[…]` | silent — REF gap, leave |
| a31 | `is_a?(Integer)` then `rand(s).typo` | fires `for Integer` | fires ✓ |
| a32/a33 | `String(u)`, `sprintf("%d", u)` | fire `for String` | fire ✓ |
| a34/a36 | `return unless u` / `return if u.nil?` then `Float(u).typo` | silent | silent ✓ |
| q2–q4 | rebound-local carriers | fire `for Float` | fire ✓ |
| q5 | `t = s.to_s; Float(t).typo` | silent | silent ✓ (**pre-existing FP closed**) |
| q6/q8/q9/q10 | guard-narrowed parameters | fire | fire ✓ |
| a27–a30 | `@d = Float(u)` / `Array(u)` / `rand(u)` with a `rescue` write | silent on BOTH | silent ✓ |

## F-B — an unorderable `is_a?` guard widens to Dynamic (#533 item 4 / `70ca7e74`)

Upstream's change is one line: `narrow_nominal_to_class`'s `:unknown` arm answers
`Type::Combinator.untyped` instead of keeping the bound. `:subclass` keeps,
`:superclass` narrows, `:disjoint` is `Bot`, `instance_of?` is `Bot` before the
ordering is consulted, and `narrow_shape_to_class` is untouched.

### The widening fact, exactly as implemented

A THIRD `ClassFact` variant, `Widened` — not `ClassFact::Bot`, and the spec's
reason is measurable: `Bot` is the JOIN IDENTITY (`Bot ∪ Array = Array`) while
`untyped` ABSORBS (`Dynamic ∪ Array = Dynamic`). Row **b15b** is the control that
separates them — a `[1, 2]` carrier under an unorderable guard is `Bot` on the
truthy edge and the call AFTER the `if` fires on **both** engines, where a
widened one is silent (row b2b). Reusing `Bot` would silence b15b; widening the
shaped arm would too.

Semantics, each one measured:

| property | rule | rows |
|---|---|---|
| suppression | records into `ClassNarrowing::dead`, like `Bot` — every rule at that call, safe-nav included | b1, b3, b4, b8, b9, b12 |
| join | ABSORBS. `join_cenv` keeps an ENTRY `Widened` when every edge still carries it; the new `propagate_widened` carries an EDGE-established one back out | b2b, b26c, b27b |
| terminating edge | contributes nothing to the propagation — the code after the `if` runs on the other edge | b24, b29, b33 |
| reassignment | CLEARS it (`Facts::kill_local`), inside the branch and after the join | b16b, b18 |
| mutation | does NOT clear it — `kill_cenv_narrowed` now removes only `Narrowed` | b19, b32b |
| block | crosses INTO a block; does NOT escape one (the block join gets no `propagate_widened`) | b28 (in), b17b (not out) |
| stickiness | a later guard neither re-mints nor collapses | b21, b34b |
| `instance_of?` | stays `Bot` | b13 |
| `\|\|` union | ONE unorderable member widens the whole edge | b4, `seq_projsub_or` |
| chain twin | identical, and a widened address short-circuits later guards | `chain_projsub` et al. |
| Bot-derived verdicts | none — `dead` is a pure per-call-node suppression set (`rigor-rules` line 583) | — |

Port sites: `narrow_nominal_to_class`'s two `Unknown` arms; a new three-valued
`guard_meet_precise` (replacing the boolean `guard_collapses`) for the
PRECISE-CARRIER meet, which is where fixture 86's row actually lives — the local
has no fact yet and the carrier comes from `tenv`, so `narrow_nominal_to_class`
is never reached for it; `apply_guards`'s local arm; `class_flow_case`'s
`bot_subject` arm; both recording arms; the block-descent filter; `join_cenv`;
`kill_cenv_narrowed`.

### The `projsub` re-probe the spec demanded

Re-measured at `ffb456b0`, and the answer inverted the port's existing rows.
Until `v0.3.8` the reference's `:unknown` KEPT the bound, so the port split
`Unknown` by `source.knows_class` — a project class kept the carrier, an
unorderable RBS-space pair dropped. **All six project-class rows are now
reference-SILENT, and every one was a live false positive** firing `for String`:

```
seq_projclass  seq_projsub  seq_projsub_or
chain_projclass  chain_projsub  chain_projsub_or
```

`seq_ns_unknown_drop` / `chain_r7` (`File::Stat` then `URI::HTTP`) were already
silent through the DROP and stay silent through the widening — the fact is now
`Widened` (suppressing) rather than absent (merely factless). The whole
`knows_class` split is deleted: the reference no longer distinguishes a project
class from an RBS-less gem class here.

Controls re-verified unchanged: `seq_or_mixed`, `seq_superclass`,
`seq_subclass`, `seq_ctrl_write_between`, `chain_ctrl_use_between`,
`chain_or_mixed`.

### F-B rows measured beyond the spec's table

| row | shape (carrier `h = Array.new` unless noted) | oracle | port after |
|---|---|---|---|
| b14a/b14b | `s = "str"` carrier, guard then post-guard use | silent, silent | silent ✓ (both silent BEFORE too — see the Constant residue) |
| b15a/b15b | `h = [1, 2]` carrier, guard then post-guard use | silent, **fires** | silent, fires ✓ |
| b16a/b16b | guard, then `h = Array.new`, then use | silent, **fires** | silent, fires ✓ |
| b17a/b17b | guard INSIDE `[1].each do … end`, then use after | silent, **fires** | silent, fires ✓ |
| b18 | rebind inside the guarded branch | **fires** | fires ✓ |
| b19 | early-return guard, `h.push(1)`, then use | silent | silent ✓ |
| b20a/b20b | widened, then `is_a?(String)`, then use | **fires `for String`**, silent | silent, silent — b20a is a REF gap |
| b21 | `is_a?(U) && is_a?(Enumerable)` | silent | silent ✓ |
| b22 | project-class guard on a Nominal carrier | silent | silent ✓ |
| b24 / b29 / b33 | `return if guard` (and its block spelling), then use | **fires** | fires ✓ |
| b25a/b25b | explicit `if`/`else` | silent, **fires** | silent, silent — b25b a pre-existing REF gap |
| b26a/b26b/b26c | use before the guard, guarded use, post-join use | **fires**, silent, silent | ✓ |
| b27a/b27b | `case … when U`, then use after the `case` | silent, silent | silent ✓ |
| b28 | early-return guard, then use inside a block | silent | silent ✓ |
| b30a/b30b | `case` on a `[1, 2]` carrier, then use after | silent, **fires** | silent, fires ✓ |
| b31a/b31b | `while h.is_a?(U)` body, then use after | **fires**, **fires** | silent, fires — b31a a REF gap |
| b32a/b32b | guard, `h.push(1)`, then use | silent, silent | silent ✓ |
| b34a/b34b/b34c | guard, then `is_a?(Enumerable)`, then use | silent ×3 | silent ✓ |
| `seq_unknown_then_known` | `String` then `U` then `Hash` guards | **fires `for Hash`** | silent — REF gap |

## Residues left, and why

1. **a15** (`"abc"[u].typo`) — a pre-existing generic-RBS-dispatch FP, measured
   silent on the reference at `v0.3.4` and `v0.3.0`, so NOT a re-pin retraction.
   Closing it needs #521's `join_candidate_returns` in the generic dispatch; no
   oracle row inside the window requires it. See the attribution above.
2. **The `Constant`-carrier `:unknown` arm stays `Bot`.** Upstream's
   `narrow_constant_to_class` also answers `untyped` on `:unknown` (that is #657,
   a different issue), but rows b14a/b14b measure rigor-rs already silent on BOTH
   halves, so the difference is unobservable here and porting it would widen this
   change past the family it ports. Recorded so a future divergence is traceable.
3. **`seq_unknown_then_known` / b20a**: upstream RE-NARROWS a widened carrier
   through `narrow_class_other` (`Dynamic` → `Nominal[C]`) and fires; the port's
   `Widened` is sticky and stays silent. A coverage gap, deliberately: re-minting
   over a widened fact is FP-creating and unprobed.
4. **b25b, b31a, a14, a26** — pre-existing coverage gaps in the explicit-`else`
   edge, the `while`-predicate narrowing, `Array#first(untyped)` and `Hash(u)`.
   Untouched.
5. **Cost**: `arg_is_reference_untyped` does two arena scans per untyped-argument
   fold, so it is quadratic in the number of such folds per file. A synthetic
   worst case (9000 lines, 4500 conversions on bare parameters) measures 1.48s vs
   0.89s user against the same file with literal arguments. Real files carry a
   handful. Recorded on the function; revisit if a sweep file regresses.
6. **The standing sweep was NOT run here** (`fp_audit.py --gaps --sweep`). Both
   changes only ever REMOVE diagnostics, so they cannot add a sweep FP; what the
   sweep would show is coverage movement, and the release binary is shared with
   the parallel agent's work.

## Fixtures

* `harness/corpus/99_untyped_arg_overloads.rb` — 17 oracle-measured rows: every
  F-A RS row, every BOTH control including the six the naive gate silenced, and
  the `String`/`format` single-overload controls.
* `harness/corpus/100_unorderable_guard_widens.rb` — 13 oracle-measured firing
  rows plus the silent set: the join, the `case` and `===` spellings, the
  mutation and block rules, and the controls a naive fix would swallow (b5, b11,
  b15b, b30b, b13, b24, b33, b16b, b18, b17b, b31b, b26a).
* Fixtures 60, 67 and 86 keep their lines; their comments claimed the RETRACTED
  behaviour and now name the upstream commit and the retraction. Snapshots
  regenerated.

## F-A remainder — the roots that are not a `def` local (2026-09-09, later)

`arg_is_reference_untyped` only admitted a root that is a LOCAL of the enclosing
`def`, which left two standing-sweep false positives:

* gitlab-foss `lib/gitlab/ci/config/entry/pull_policy.rb:28:28` —
  `Array(@config).presence`, an ivar with no write in the file;
* gitlab-foss `lib/gitlab/filter_evaluator.rb:15:58` —
  `'not_in' => ->(actual, expected) { Array(expected).exclude?(actual) }`, a
  lambda parameter at class-body level with no enclosing `def` at all.

Both are closed. The predicate now dispatches on five ROOT KINDS
(`UntypedRoot`), each a port of what the reference actually types — every rule
below was measured at the pin (`ffb456b0`), fresh temp cwd, `--no-cache`.

### The root rules, as implemented

**ivar** — port of `build_class_ivar_index`. The region is the innermost
enclosing `ClassDef`/`ModuleDef` (the whole file with none), with a nested
class/module as a barrier. Only `@x = …` writes inside a `def` of that region
count; a CLASS-BODY write does not (row i1, silent), nor a write in a nested
class (i9), nor `@x ||= …` (i2 — the reference's collector recognises a plain
`InstanceVariableWriteNode` only, exactly like this arena). Then:

* **no write at all → untyped** (r2/r4/i7/t2, and the `pull_policy.rb` site);
* **writes present → untyped iff every write's value is itself reference-untyped
  AND `initialize` (or the class body) writes the name.** The `initialize`
  clause is not decoration: `contribute_read_before_write_nil!` folds
  `Constant[nil]` into an entry the class reads before writing unless the ctor
  writes it, so an untyped write in a NON-ctor method still fires (z7, i12 —
  both must-still-fire, both measured).

Any `MultiWrite` with a non-local target in the region refuses the whole test:
the reference DOES collect an ivar multi-target (`record_multi_write_ivars`, row
i5 fires) and `MultiTarget::Ignored` carries no name to match.

**No guard scan for an ivar.** The reference does not class-narrow one here at
all — `return unless @x.is_a?(String)` then a BARE `@x.typo` is silent (q1), so
is the same on an unwritten ivar (q2), the inline-`if` form (n5), `case`/`when`
(q4) and the `rand` fold (q5), where the identical guard on a def LOCAL fires
`for String` (q3). Seven measured forms, all silent; the local arm keeps its
guard scan unchanged (rows a25/a31 depend on it).

**cvar** — port of `build_class_cvar_index`, which collects `def`-body writes
ONLY. A class-body `@@n = nil` is walked past and never recorded, so it leaves
the read `Dynamic[Top]` (r14, and n7 where a class-body write sits beside an
untyped `def` one). No read-before-write machinery for cvars, so the rule is
just "every write's value is reference-untyped" (n3 silent, c1 fires).

**gvar** — port of `build_program_global_index`, which is program-wide: every
`$x = …` counts, at top level and in any `def`. Unwritten is untyped (g2);
`$g = nil` at top level (r15) and `$g = "s"` in a `def` (g3) keep firing; written
only from an untyped parameter is untyped (n4). Added beyond the four kinds the
task named, on the same measured footing — the mechanism is `scope.global(name)
|| dynamic_top`, the literal twin of the cvar one.

**proc-like parameter** — only reached when there is NO enclosing `def` (with
one, the existing def-region rule already covers a lambda inside it, row r12).
The innermost enclosing BINDER around the use site must be a `->`, `lambda {}`,
`proc {}` or `Proc.new {}` block; the region is then the whole file, so a
CAPTURED outer local the reference types still refuses (l5, which must keep
firing) and writes inside an unrelated `def` are excluded as another scope. An
ORDINARY block's parameter is deliberately NOT admitted — the reference types it
from the RBS yield (`[1, 2].each { |x| Float(x) }` fires `for 1.0`, row m11).

**constant** — a BARE name that resolves to nothing: not a class or module
(`constant_names_a_known_class` over the lexical prefix), not an RBS object
constant (`CoreIndex::object_constant_class` — `ENV`/`ARGV`/`STDOUT`), not
written by the project. A QUALIFIED path is refused outright: this port has no
table for class-scoped RBS constants and both `Float::INFINITY` (k4) and
`Errno::ENOENT` (k6) fire on both engines.

### The r11 finding: a `->` body's local writes never bind

Row r11 (`->(y) { y = 1; Float(y).typo }` inside a `def`) is reference-SILENT,
and the cause is NOT "a rebound parameter". Probed as instructed:

| probe | shape | oracle |
|---|---|---|
| p1 | `->(y) { y = 1; y.typo }` in a `def` | **silent** |
| p13 | `->(y) { z = 1; z.typo }` in a `def` — a FRESH lambda-local | **silent** |
| m5 | `->(a) { b = "s"; b.typo }` in a `def` | **silent** |
| p2 | `->(y) { "abc".typo }` in a `def` | fires — the body IS analysed |
| p8 | `x = 1; ->(y) { x.typo }` in a `def` — a captured local | fires `for 1` |
| p11/m6 | `lambda { \|y\| y = 1; y.typo }` in a `def` | **fires** `for 1` |
| m1/m2 | a fresh local in a `lambda {}` / an ordinary block | **fires** |
| m13 | `lambda { \|q\| q = 1; Float(q).typo }` in a `def` | **fires** `for 1.0` |

So a `->` body is analysed and DOES see the enclosing scope's bindings, but no
local write inside it binds — parameter rebind and fresh local alike — while
every other block spelling's writes do. That, not "rebound", is the mechanism,
and it is what was ported: the write scan skips a `Node::Lambda`-interior write
and counts every other one. r11 closes; m13 keeps firing.

### Rows measured beyond the task's list

Every row below is `oracle → port after`. RS = the port's FP, now closed.

| row | shape | oracle | port after |
|---|---|---|---|
| i1 | class-body `@x = "s"` only, read in a `def` | silent | silent ✓ (was RS) |
| i2 / z5 | `@x \|\|= "s"` in a SIBLING `def` | silent | silent ✓ (was RS) |
| i3 | `@x \|\|= "s"` then read in the SAME `def` | **fires** `[Dynamic[top]]` | silent — **new gap**, see residues |
| i5 | `@a, @b = "s", 1` then `Array(@a)` | fires `Array["s"]` | fires ✓ |
| i6 / t1 | top-level `def`'s own ivar write / top-level write + top-level read | fire | fire ✓ |
| i7 / t2 | top-level `def`, ivar unwritten / written only at top level | silent | silent ✓ (was RS) |
| i8 | ivar written in a `def` of a MODULE | fires | fires ✓ |
| i9 | ivar written only in a NESTED class | silent | silent ✓ (was RS) |
| i11 | ivar written inside a BLOCK of a `def` | fires | fires ✓ |
| i12 | `@c = @d` (`@d` unwritten) in a non-ctor `def` | fires `for []` | fires ✓ |
| i13 / z9 / n5 / q4 / q5 | a class-guarded ivar, five forms | silent | silent ✓ (all were RS) |
| q1 / q2 | a class-guarded ivar, BARE read (no fold) | silent | silent ✓ |
| q3 | the same guard on a def LOCAL — the control | **fires** `for String` | fires ✓ |
| n1 | untyped ctor write + untyped sibling write | silent | silent ✓ (was RS) |
| n2 | ctor writes `nil` | fires `Array[bot]` | fires ✓ |
| n6 | `Array(@c).presence` + a second use, untyped ctor write | silent | silent ✓ (the `pull_policy.rb` twin) |
| z6 | untyped ctor write + a typed sibling write | fires | fires ✓ |
| z7 | untyped write in a NON-ctor `def`, read elsewhere | fires `for []` | fires ✓ |
| z8 | `Array(@s.to_s)` — a chain over an untyped ivar | silent | silent ✓ (was RS) |
| z10 | untyped ctor write, defaulted in the reader | fires | fires ✓ |
| z1–z4 | the RHS of an ivar/cvar/gvar op-write | fire | fire ✓ (reachability unchanged) |
| c1 / c3 / n7 | cvar written in a `def` / never / class-body + untyped `def` write | fires, silent, silent | ✓ (c3, n7 were RS) |
| n3 | cvar written from an untyped parameter | silent | silent ✓ (was RS) |
| g2 / g3 / n4 | gvar unwritten / written in a `def` / written from a parameter | silent, fires, silent | ✓ (g2, n4 were RS) |
| k1 / k5 / k8 | a project constant, bare and shadowed-in-class | fire | fire ✓ |
| k3 | `Array(String)` — a class object | fires `[singleton(String)]` | fires ✓ |
| k4 / k6 | `Float::INFINITY`, `Errno::ENOENT` — qualified paths | fire | fire ✓ |
| l4 | `Proc.new { \|a\| Float(a) }` | silent | silent ✓ (was RS) |
| l5 | a captured top-level local read inside a `->` | **fires** | fires ✓ |
| l8 | `->(a) { [1].each { \|q\| Float(a) } }` | silent | fires — residue (ordinary block between) |
| m4 / m7 / m9 / m14 / m15 | a block on a CONSTANT-ASSIGNMENT RHS whose body writes a local | silent | fires — residue, see below |
| p5 | `->(y) { y = 1; Float(y).typo }` at top level | silent | silent ✓ (was RS) |
| p3 / p6 / p14 | a literal receiver inside a lambda / an ordinary block / a literal-arg fold | fire | fire ✓ |
| p7 / p11 / p12 | an ordinary-block or `lambda {}` parameter, in a `def` | fire | silent — pre-existing REF gaps, unchanged |

### Gates

* `cargo test --workspace --offline`: all green, including two new tests
  (`variable_reads_and_writes_carry_their_sigilled_name`,
  `untyped_argument_roots_beyond_def_locals`).
* `cargo clippy --workspace --all-targets -- -D warnings` in a fresh
  `CARGO_TARGET_DIR`: clean.
* `ruby harness/run.rb` and `ruby harness/run_snapshot.rb`: **PASS**, 103
  fixtures, 462 matched, 46 gaps, **0 unregistered extras**. On the 102 fixtures
  that predate this change the numbers are byte-identical to `42ecfaa`
  (439 matched, 440 rigor-rs diagnostics, 0 extras), so nothing was lost.
* A targeted sweep instead of `fp_audit --gaps --sweep`: the 265 files of the
  standing sweep corpora that contain any `Float`/`Integer`/`Array`/`rand(` call
  were run before and after. The ENTIRE diff is the two target sites going
  silent — no other diagnostic moved, added or lost. The release binary's output
  over the same 265 files is byte-identical to the debug binary's (the
  "sweep measures the RELEASE binary" hazard).

### Residues

1. **Row i3 — a new coverage gap, the one thing this change loses.**
   `@x ||= "s"` followed by a fold on `@x` in the SAME `def` fires on the
   reference (the or-write binds flow-sensitively, `Array[Dynamic[top]]`) but
   Prism's `InstanceVariableOrWriteNode` has no owned arena variant, so the write
   is invisible and the ivar test admits the name. Sibling-`def` or-writes are
   silent on BOTH engines (i2/z5), which is why only the same-body shape is lost.
   Closing it needs the ivar/cvar/gvar OP-writes lowered as an owned variant —
   deliberately not done here: `Node::VariableWrite`'s five consumers
   (`type_of`'s assignment arm, the class-narrowing walk's
   `widen_flow_writes`/`kill_cenv_writes`, the rules' child descent, `sig-gen`
   and `annotate`'s tail typing) would all start seeing a node kind they never
   have. Not present in the sweep corpora: of the ~20 ivar-argument fold sites
   there, none is i3-shaped. Pinned as a fixture-105 gap so it is visible.
2. **A block on a CONSTANT-ASSIGNMENT RHS.** `M = lambda { |a| b = "s";
   Float(b).typo }` is reference-silent while the same block in statement
   position fires (m3 vs m4) — the local write does not bind there, but a
   captured outer local still reads through (l5). Unported: the mechanism is not
   understood, and on this side the rows are unreachable anyway (at TOP level
   rigor-rs's env is real, so `b` types `String` and the decline's `type_of` gate
   never opens). Rows m4/m7/m9/m14/m15/l1/l2/l3/l9.
3. **An ordinary block between the use site and its proc-like binder** (row l8)
   keeps firing: the innermost binder must BE proc-like, because admitting
   through an intervening block would claim that block's own parameter is
   untyped, which it is not (m11). The safe direction; the same class as the
   already-recorded "a local shadowed by a block parameter" withholding.
4. **A guard anywhere in the region refuses a LOCAL root**, and for the no-`def`
   case the region is the whole file — so a same-named variable guarded in an
   unrelated top-level lambda refuses this one. Over-conservative, never an FP.
5. **Cost.** The local arm now makes one extra arena pass (collecting `def` and
   `->` spans) and the ivar/cvar arms two (the class region + its `def` list).
   Synthetic worst case, 1500 classes / 3000 ivar folds (13.5k lines): 0.41s vs
   0.31s user, release. A 1500-`def` / 4500-local-fold file (7.5k lines) is
   unchanged at 0.13s. Real files carry a handful.
6. **`fp_audit.py --gaps --sweep` was still not run.** The targeted 265-file
   before/after above is strictly more informative for this change (it shows the
   full diagnostic delta, not a gap count), and the change can only ever REMOVE
   diagnostics, so it cannot add a sweep FP.

### Fixture

`harness/corpus/105_untyped_arg_roots.rb` — 22 sections, oracle-measured at
`ffb456b0`: the 9 RS rows the task listed plus 14 more this work closed, every
BOTH control (r3/r5/r15, i5/i6/i8/i11/i12/t1, z6/z7/z10, n2, c1/g3, m13, l5, and
the six constant spellings), and the three recorded gaps (i3, r9, r10). Local
names are unique per row on purpose — the write and guard scans are region-wide,
not flow-ordered, so a reused name in a sibling row refuses the test for a reason
the row is not about.
