# The disjoint guard: where OUR carrier is precise and THEIRS is `Bot` (2026-08-08)

The FP family
[the carrier-fidelity audit](20260808-narrowing-carrier-fidelity-fp.md) left
open. It is the exact mirror of that note: there our carrier was COARSER than
the reference's and the Dynamic-only gate inverted; here our carrier is
PRECISE and the reference's is narrower still — `Bot`.

```ruby
def f
  h = [1, 2]
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end
```

| | verdict on master `1d6edae` |
|---|---|
| reference `v0.3.1` | **silent** |
| rigor-rs | `undefined method 'frobnicate_zzz' for Array` |

## Mechanism — and the correction to the earlier diagnosis

The carrier-fidelity note named `narrow_nominal_to_class`
(`narrowing.rb:2381`). That is one of THREE collapsing paths, and for the
reported archetype it is the wrong one: the reference types `[1, 2]` as a
`Tuple`, not a `Nominal`, so `narrow_class_dispatch` routes to
`narrow_shape_to_class` (`:2404`). The two have DIFFERENT collapse conditions,
and the difference is the whole reason this slice declines an arm:

| reference carrier | collapse rule (`is_a?`) | `ClassOrdering::Unknown` ⇒ |
|---|---|---|
| `Nominal[C]` | `class_ordering(C, guard) == :disjoint` | preserved — **fires** |
| `Tuple` / `HashShape` | `!subclass_of?("Array"\|"Hash", guard)` | `Bot` — **silent** |
| `Constant[v]` | `!subclass_of?(v.class.name, guard)` | `Bot` — **silent** |
| `Singleton[C]` | `!subclass_of?("Class", guard)` | `Bot` — **silent** |

Under `instance_of?` (`exact: true`) every one of them collapses on a bare
NAME MISMATCH, with no hierarchy consulted at all (`:2384`, `subclass_of?:2440`)
— `h = [1, 2]; h.instance_of?(Enumerable)` is `Bot` even though `Array` IS an
`Enumerable`.

`Bot` has no dispatch surface, so the reference emits nothing for a call whose
receiver is that local. It does **not** kill the branch: a call on a DIFFERENT
local in the same branch still fires (`scope_other_local`,
`scope_if_two_stmts`), and so does a call nested in the suppressed call's own
ARGUMENTS (`nest_arg_other`). The unit of silence is the call node whose
receiver is the guarded local.

## What was probed — 180 oracle rows

All against the pinned reference from a fresh temp cwd with `--no-cache` and
the bundled `rigor-rbs-inline` plugin path pinned (upstream #194).

### Which RULES go silent

Every rule the reference has for a receiver, because `Bot` answers none of
them. Each control below FIRES unguarded on the reference and is SILENT inside
the disjoint branch: `call.undefined-method` (`um_ctl_*`/`um_grd_*`),
`call.wrong-arity` (`wa_ctl_*`/`wa_grd_*` over String/Array/Hash/Integer
carriers), `call.argument-type-mismatch` (`atm_ref_ctl`/`atm_ref_grd`),
`call.raise-non-exception` (`rne_ctl`/`rne_grd`),
`call.possible-nil-receiver` (`pn_ctl_b`/`pn_grd_b`).

**rigor-rs can only produce ONE of them here**: measured over 29 carriers, no
local carrier witnesses `wrong-arity`, `argument-type-mismatch`,
`possible-nil-receiver`, `raise-non-exception` or `always-truthy-condition` on
our side at all — every such row is a pre-existing gap. The FP surface today is
exactly `call.undefined-method`. The fix still skips the WHOLE call site rather
than one rule: that is what the oracle shows, and a rule-scoped suppression
would become wrong the moment any of those rules gains a local carrier.

### Which GUARD FORMS (all rows: reference silent, master firing)

| form | probe | fixed |
|---|---|---|
| `is_a?` / `kind_of?` | `g_is_a`, `g_kind_of` | ✅ |
| `instance_of?`, disjoint | `g_instance_of_disjoint` | ✅ |
| `instance_of?`, SUPERCLASS (`exact:` ⇒ `Bot`) | `g_instance_of_super`, `iof_nominal_super` | ✅ |
| `instance_of?`, unresolvable class | `iof_nominal_unknown` | ✅ |
| `C === local` | `g_triple_eq`, `tripleeq_if` | ✅ |
| statement `if`, `if` modifier, ternary | `s_if`, `s_if_modifier`, `base_disj` | ✅ |
| ternary on an assignment RHS / as an argument | `ternary_rhs`, `ternary_arg` | ✅ |
| `elsif` | `elsif_disj` | ✅ |
| `case`/`when`, one condition | `s_case_when` | ✅ |
| `case`/`when`, ALL conditions disjoint | `case_multi_disj` | ✅ |
| `return`/`raise` guard, propagating past | `s_early_return`, `early_raise_after` | ✅ |
| top level, no `def` | `toplevel` | ✅ |

Forms where the reference does **not** collapse, and rigor-rs must keep firing:
the FALSEY edge (`s_unless_body`, `s_negated_if`, `s_case_when_else_branch` —
`narrow_nominal_not_class` preserves a disjoint nominal), a `when` clause whose
conditions do not ALL collapse (`case_multi_mixed`: `when Hash, Array` on an
Array unions back to the Array), a `case` consumed as a call RECEIVER
(`case_as_recv` — the position gate applies to the `Bot` path too), and a
`while` predicate (`s_while_guard`).

### Which CARRIERS — the 29-row census that decided the bound

Four columns per carrier: unguarded control, disjoint guard, non-disjoint
guard, and `is_a?(UnknownZzz)`. The last column is the discriminator, because
it separates the reference's `Nominal` carriers from its shape/constant ones.

| carrier | ref on `is_a?(UnknownZzz)` | our carrier collapses it? |
|---|---|---|
| `[1, 2]`, `[]`, `{a: 1}`, `{}`, `%w[a b]` | **silent** (shape) | no — declined |
| `[1,2].compact`, `.map{}`, `.freeze`, `.dup`, `.to_a`, `[[1],[2]]`, `{a:1}.dup` | **silent** (shape) | no — declined |
| `"abc"`, `42`, `1.5`, `:s`, `(1..3)`, `true`, `nil`, `"abc".upcase` | **silent** (constant) | no — declined, and we never witness there anyway |
| `*spec` (splat) | **FIRES** (nominal) | must not collapse |
| `h = []; h << 1` (mutator-widened) | **FIRES** (nominal) | must not collapse |
| `Array.new`, `Hash.new`, `String.new`, `->(x){}` | **FIRES** (nominal) | must not collapse |

That split is why the `ClassOrdering::Unknown` arm is **declined**: it is worth
17 live FP rows (12 in the `x_*_unk` column above, plus `n_unknown_class`,
`n_project_class`, `n_nonincluded_module`, `ns_guard`, `shadow_hash` — the last
of which declines one step earlier anyway, at
`SourceIndex::constant_shadowed`), but claiming it requires asserting which of OUR carriers the
reference holds as a shape rather than a nominal — and the census refutes every
cheap proxy. `[1, 2]` and `*spec` are both plain Arrays on our side and split in
theirs; so do `[1,2].compact` (shape) and `Array.new` (nominal). This is the
same class of claim that produced three FPs in this arc already
([the lesson](20260808-narrowing-carrier-fidelity-fp.md#the-lesson-worth-keeping)),
and here getting it wrong DESTROYS diagnostics rather than manufacturing them.
It wants its own slice with its own carrier evidence.

### Is the branch silenced entirely?

No — per local, per call node. `scope_other_local` / `scope_if_two_stmts`
(another local in the same branch) and `nest_arg_other` /
`nest_recv_of_other` / `nest_h_as_arg` (a call nested in the arguments) all
FIRE on the reference and still fire here.

### Lifetime of the `Bot` fact — probed, not assumed

| behaviour | probe | reference |
|---|---|---|
| reaches a nested conditional, a nested block body (`{}` and `do…end`) | `nest_deep`, `bot_into_block`, `bot_into_block_doend` | silent |
| survives a further guard on the same local, either edge | `bot_then_match`, `bot_then_neg`, `double_guard` | silent |
| survives past an inner `if`, a block call, a `while`, a `begin`/`rescue` | `bot_after_inner`, `bot_after_block_call`, `bot_after_while`, `bot_after_begin` | silent |
| survives a MUTATOR on the local (`h.push(3)`) | `bot_mutator_use` | silent |
| dies on a REBIND — in the branch, and inside a block | `bot_rebind_use`, `bot_block_rebind` | **fires** |
| dies at the conditional's JOIN | `after_branch` | **fires** |
| does not cross a nested `def` | `bot_nested_def` | **fires** |

Mutation-vs-rebind is the subtle one: a mutation widens a CARRIER and `Bot` has
none, so `kill_cenv_narrowed` spares it where `kill_cenv_writes` (still used
where nothing was descended) does not.

## The fix built

`Typer::class_narrowing_pass` (`crates/rigor-infer/src/lib.rs`) — the existing
narrowing walk, now carrying a second fact. `cenv`'s value type became
`ClassFact::{Narrowed(String), Bot}`; the two never coexist for one local
(`Narrowed` requires a `Dynamic`/`Top` carrier, `Bot` a precise one). The pass
returns `ClassNarrowing { calls, dead }`, and `rigor-rules`'s call loop
`continue`s on a `dead` node id — skipping the whole site, the independent
argument-type axis included.

`Typer::guard_collapses` is the gate, and it collapses on exactly three
conditions:

1. the local is ALREADY `Bot` (`narrow_class_other` returns `Bot` unchanged on
   both polarities, so no later guard can revive it);
2. `instance_of?` with a name the carrier's class does not equal — the
   reference's `exact:` path needs no hierarchy;
3. `CoreIndex::class_ordering(carrier, guard) == Disjoint`. That answer is
   already conservative: it requires both names to resolve AND both ancestor
   chains to be complete, and returns `Unknown` otherwise.

The carrier must additionally be one the reference's `narrow_class_dispatch`
(`:2311`) routes to a COLLAPSING helper — `Constant`, `Nominal`, `Tuple`,
`HashShape`. Everything else falls through to `narrow_other_class`, which
returns a non-Dynamic type UNCHANGED; `IntegerRange` is the one carrier
`CoreIndex::class_name_of` names that the reference never collapses, and the
variant gate is what keeps it out. Its class name then comes from
`class_name_of` — the SAME mapping `check_call` dispatches on — so the
suppression is co-extensive with the witness it removes: we can only suppress a
diagnostic we could have emitted.

`===` is recognised for the `Bot` path but deliberately NOT added to
`class_check_predicate`: minting a `Narrowed` fact from it would be a new,
unprobed COVERAGE claim, while collapsing to `Bot` only ever removes output.

The branch join is now edge-evidence based (`join_cenv`): a `Bot` present on
entry survives only while EVERY edge still carries it, which is exactly the
reference's `Bot | Bot = Bot` and drops the fact the moment an edge rebound the
name — no span heuristic required.

## What was DECLINED, and why

- **`ClassOrdering::Unknown` on a shape/constant carrier** (probes
  `n_unknown_class`, `n_project_class`, `n_nonincluded_module`, `ns_guard`,
  `shadow_hash`, the `x_*_unk` column). 17 live FP rows left open. Reason above:
  it is a carrier-fidelity claim, not a hierarchy fact.
- **A `Union` carrier.** The reference unions the per-member narrowings and
  collapses only when every member does. Unprobed on our side; rigor-rs
  witnesses very little through a union local anyway.
- **A guard on a `Singleton` carrier** (`Foo.is_a?(Hash)`). The reference maps
  it to `"Class"` and uses `subclass_of?`, so it is mostly the declined
  `Unknown` arm.
- **An `IntegerRange` carrier.** Excluded by the variant gate above, because
  the reference does not collapse it at all.
- **A stable single-hop CHAIN receiver** (`x.first.is_a?(Hash)`, probe
  `chain_guard`) — the reference narrows it through
  `method_chain_narrowings`; rigor-rs models no chain addresses, so the row is
  a pre-existing coverage gap on both the `Narrowed` and the `Bot` side.

## Cost — measured

`gap_census.py --sweep` at the merge-base `1d6edae` (**1142 rows**) and after
(**1142 rows**): **0 rows open, 0 close.** Rerun in full below; the standing
sweep stays at **0 FP / 9204 files** over 8 corpora.

That the coverage cost is exactly zero is not luck — it is what the bound
buys. `class_ordering` answers `Disjoint` only on two fully-resolved core
chains, and a call the reference would have witnessed inside such a branch does
not exist in the sweep corpora.

## What a reviewer should scrutinise

The anti-over-suppression half. `disjoint_guard_suppression_does_not_over_reach`
and `disjoint_guard_suppression_is_per_call_not_per_branch`
(`crates/rigor-rules`) plus items (8)-(15) of
`harness/corpus/86_disjoint_guard_suppression.rb` are the only thing standing
between this slice and a silent coverage hole — an FP gate cannot see one. In
particular: the `instance_of?` arm suppresses on a NAME MISMATCH with no
hierarchy check, so it is the widest thing here; the block carry-in
(`bot_into_block`) is the only place a fact crosses a scope boundary in this
pass; and `kill_cenv_narrowed` is the one place a `Bot` deliberately survives
an invalidation that kills a `Narrowed`.

## Probe corpus

`p1_rules` (rules × scope), `p2_forms` (predicates, statement forms, carriers,
non-disjoint controls), `p3_rules2` (per-rule carrier hunt, nesting,
invalidation), `p4_carriers` (the 29-carrier × 4-column census), `p5_edges`
(guard-class resolution, positions, `===`, `case` shapes), `p7_life` (the
lifetime table). One hazard worth recording: the probe runner batches every
file into ONE `rigor check`, and the SourceIndex is project-wide — a probe file
containing `class Hash; end` shadows the constant for every OTHER file in the
batch and silently disables the whole suppression. Keep constant-shadowing
probes in their own batch.
