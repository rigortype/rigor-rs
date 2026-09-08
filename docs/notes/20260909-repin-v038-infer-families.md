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
