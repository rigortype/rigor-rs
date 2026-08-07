# The narrowing subset argument breaks where OUR carrier is coarser (2026-08-08)

A live false positive on master, found by the stage-3b-1 build when its
value-descent reached the shape, and confirmed independently.

```ruby
def f(spec)
  h = spec || {}
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end
```

| | verdict |
|---|---|
| reference `v0.3.1` | **silent** |
| rigor-rs at master `a61992f` | `undefined method 'frobnicate_zzz' for Hash` |

(The ivar-write form `@spec = h.is_a?(Hash) ? … : h` is silent on BOTH at
master, which is why only 3b-1's descent into write values surfaced it. The
sweep had no other instance, so 0 FP / 9204 files was true and uninformative.)

## Why the FP-safety argument failed

Every narrowing spec in this arc rests on one claim: we narrow ONLY a local
whose type is `Dynamic`/`Top`, mirroring the reference's `narrow_class_other`
(`narrowing.rb:2425`), which narrows Dynamic/Top and leaves every other
carrier alone. That makes us a strict subset — **provided the two engines
agree on which locals are Dynamic.**

They do not. `rigor-rs` types `Node::Logical` as `Dynamic[top]`; the
reference's `analyse_or` builds a UNION of the two operand types. So for
`h = spec || {}`:

- reference: `h` is a union ⇒ `narrow_class_other` does not apply ⇒ no
  narrowing ⇒ silent.
- rigor-rs: `h` is `Dynamic[top]` ⇒ our gate fires ⇒ narrowed to `Nominal[Hash]`
  ⇒ we witness.

The gate inverts exactly where our typing is COARSER than the reference's.
"Narrow only Dynamic" is a subset rule only when Dynamic means the same thing
on both sides; a carrier we collapse and they refine turns the rule inside out.

Second instance, same cause: a local bound from a project method whose return
tail is a `Logical` (`def mk; unknown_zzz || {}; end`) — pinned as `fp2` in
`crates/rigor-infer/src/lib.rs`'s narrowing tests alongside `fp1` above.

Both are now asserted SILENT end to end
(`coarse_carrier_narrowing_is_silent_end_to_end`, `crates/rigor-rules`), with
the whole carrier matrix in `class_narrowing_carrier_fidelity_matrix` and an
oracle-verified fixture at `harness/corpus/85_narrowing_carrier_fidelity.rb`.

## Scope of the exposure — MEASURED

`Logical` was the one construct measured when this note was opened. It is 19
of them. 92 oracle probes (a value bound to a local, guarded with `is_a?`,
used) against the pinned reference from a fresh cwd with `--no-cache`:

### The gate FIRED where the reference declined — live FPs, all now closed

| carrier | what the reference types it | probes |
|---|---|---|
| `a \|\| b`, `a && b` | a UNION of the operand types | `logical_or`, `logical_and`, `logical_or_nested`, `logical_paren` |
| `h \|\|= …` | a union (the op-assign is an `or`) | `logical_orassign` |
| a project method whose return TAIL is a `Logical` | that union, through the call | `logical_via_method_toplevel`, `call_self_recv`, `recv_insource_logical` |
| `while`/`until`/`for` as a value | `nil` | `loop_while`, `loop_until`, `loop_for` |
| `begin`/`rescue` as a value, and the `rescue` modifier | a union of body + clauses | `beginrescue`, `rescue_modifier` |
| `case`/`in` WITH an `else` | a union of the branches | `case_when`, `case_in` |
| `if`/ternary as a value | a union — ours survives `type_of` but `Algebra::join` collapses `untyped \| Hash` back to `Dynamic` | `ternary_dyn`, `if_expr`, `return_expr` |
| `(a..b)` | `Range` | `range_lit`, `range_dyn` |
| `self` | the enclosing class instance | `self_read` |
| `->(x){ }`, `proc { }`, `lambda { }` | `Proc` | `lambda_lit`, `proc_lit`, `call_kernel_proc`, `call_kernel_lambda` |
| `__method__`, `binding`, `caller`, `block_given?` | their real RBS returns | `call_kernel_*` |
| `defined?(x)` | `String?` | `defined_p` |
| a receiver the reference resolves precisely (`Float::INFINITY.abs`) | that method's return | `recv_const_float` |

### The gate and the reference AGREE — the allow-list

| carrier | probes |
|---|---|
| a method / block / keyword / optional / rest / block parameter | `ctrl_param`, `ctrl_unbound`, `blockparam`, `allow_kwarg`, `allow_optarg`, `allow_restarg`, `allow_block_arg` |
| `@ivar` / `$gvar` / `@@cvar` read | `ivar_read`, `gvar_read`, `cvar_read`, `ctrl_ivar_bound` |
| a call through such a receiver — plain, chained, `[]`, safe-nav, block-bearing | `plain_call_dyn`, `call_chain`, `index_read`, `safenav`, `safenav_unknown`, `block_call_dyn`, `block_call_known`, `freeze_dyn`, `allow_param_index`, `allow_ivar_chain`, `allow_safenav_ivar`, `call_ivar_recv`, `call_gvar_recv` |
| a destructuring target — even off a `Logical` RHS | `multiwrite`, `mw_from_call`, `mw_from_logical` |

### The reference fires and we decline — accepted coverage cost

`yield` / `super` (`yield_val`, `super_val`, `super_args`), an implicit-self
call (`implicit_self_call`, `implicit_self_insource`), a `case` with NO `else`
(`case_noelse` — the reference collapses `untyped | nil` back to untyped), a
`begin`/`ensure` with no rescue clause (`begin_ensure`), a `ConstantRead`
binding or receiver (`const_read_unknown`, `call_const_recv_*`), a precisely
typed receiver (`call_str_recv`, `call_range_recv`, `call_self_unknown`), and
a call through an already-coarse local (`call_on_logical_local`).

## The fix built

An ALLOW-list, not a deny-list. A deny-list demonstrably leaks: `__method__`
and `proc { }` are receiverless `Call`s, exactly like the safe
`implicit_self_call`; `defined?` and a `*splat` lower into the same span-only
recovered carrier as the safe `yield`/`super`. There is no arena-visible
discriminator, so the safe direction is to enumerate what we KNOW agrees.

`narrowable_binding` (`crates/rigor-infer/src/lib.rs`) is that list — a
parameter, an `@ivar`/`$gvar`/`@@cvar` read, or a call through one of those.
`coarse_locals` runs it over each scope's binding sites to a fixpoint and
yields the names the `is_a?`/`case-when` gates refuse. Every `LocalVariable
OpWrite` and every `rescue => e` capture is unconditionally coarse; multi-write
targets deliberately are not. The set is per-scope, so a name made coarse in
one `def` still narrows in another (`class_narrowing_coarse_set_is_per_scope`).

Cost, measured with `gap_census.py --sweep` at the merge-base (98aae09, 1139
rows) and after (1142): **3 rows open, 0 close.** Two are gitlab-foss
`auth_hash.rb:44` (`location = get_info(:address)`, an implicit-self call);
one is `normalizer.rb:29`, where a block parameter is rebound from
`variables_expander.expand(…)` earlier in the same `def` and the scope-wide
coarse set therefore poisons the name. Both are the predicted implicit-self
decline. The row this note predicted would open, gitlab-foss
`concurrency_limit/middleware.rb:14`, was **already** a gap at the merge-base
(`safe_constantize` is a Rails method) — the prediction was wrong, not the
accounting.

## The DIFFERENT FP family the audit turned up — now closed
<!-- follow-up: docs/notes/20260808-disjoint-guard-suppression.md -->


Three probes here were reference-silent / rigor-rs-firing for the opposite
reason: rigor-rs had the PRECISE carrier and the reference the narrower one
(`Bot`). Closed in
[the disjoint-guard suppression slice](20260808-disjoint-guard-suppression.md),
with 180 oracle rows. Three corrections this note got wrong, worth carrying:

- **The mechanism is not (only) `narrow_nominal_to_class`.** The reference
  types `[1, 2]` as a `Tuple`, so the archetype routes through
  `narrow_shape_to_class`, whose collapse condition is
  `!subclass_of?("Array", guard)` — TRUE on an unresolvable guard class as
  well as a disjoint one. The nominal path collapses only on `:disjoint`. Same
  visible FP, two different rules, and the difference decides how far a fix can
  reach.
- **The branch is not silenced.** Only the guarded local's own calls are: a
  call on another local in the same branch, and a call nested in the suppressed
  call's ARGUMENTS, both still fire on the reference.
- **It is not one rule.** Every receiver-driven rule goes silent through
  `Bot` — measured for `undefined-method`, `wrong-arity`,
  `argument-type-mismatch`, `raise-non-exception`, `possible-nil-receiver`.
  rigor-rs happens to witness only the first through a local carrier today, so
  the suppression skips the whole call site rather than one rule.

`instance_of?` turned out to be the widest arm and was not probed here at all:
the reference's `exact:` path returns `Bot` on ANY name mismatch, so
`h = [1, 2]; h.instance_of?(Enumerable)` is silent even though Array IS
Enumerable.

## The substrate fix, recommended

Typing `Logical` as a real union is still the correct long-term answer and
would also unlock union receivers for stage 3a-4 — but the audit says it is
**not sufficient on its own**: it closes 5 of the 19 rows. The substrate slice
worth doing is broader and has a clean shape:

1. give `Type` a union that `Algebra::join` does not collapse against
   `Dynamic` (today `untyped | Hash` becomes `Dynamic`, which is what makes
   `if`/`case`/ternary — already unioned in `type_of` — read as Dynamic);
2. type `Logical`, `Loop` (`nil`), `BeginRescue`-with-clauses, `Range`,
   `Lambda`, `SelfExpr` and `defined?` for real;
3. THEN delete `coarse_locals` and let the plain Dynamic/Top gate carry the
   subset argument again — with the probe corpus above as the regression net.

Consumers span `type_of`, `sig-gen`, `type-of`, coverage and the LSP, so it
wants its own slice and its own gate run.

## The lesson worth keeping

Two earlier findings in this same arc were the same mistake in different
clothes: the safe-nav decline picked the wrong AXIS
([position rule](20260807-block-narrowing-position-rule.md)), and here the
Dynamic-only gate picked the wrong CARRIER. Both survived review because the
argument sounded like a subset argument. Before accepting "we do strictly
less than the reference", check that every term in the claim means the same
thing in both engines — and probe the shapes where our representation is
known to be coarser.

And when the audit says the exposure is 19 constructs rather than one, do not
patch the one: an enumeration of what is UNSAFE is only as good as the last
probe, while an enumeration of what is SAFE degrades into coverage loss. Every
row above cost 4 lines of probe and 40 seconds of oracle. That is the cheapest
evidence in this repo — it should have been collected before the first
"strict subset" claim, not after the third FP.
