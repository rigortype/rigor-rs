# #128: store values named by their TYPED answer (2026-09-21)

Closes [rigor-rs#128](https://github.com/rigortype/rigor-rs/issues/128), the
residue the `v0.3.9` re-pin filed against its own `MutationRejoin` port:
`cb2fde8`'s commit message already named the shape (`h['a'] = 1.to_s` names
`String` for the reference and nothing here).

## The defect was symmetric, off one root

The collection-carrier pass grew a carrier's value side with the erased class
of each store — and named that class SYNTACTICALLY: literals, an array/hash
literal, a local looked up in the pass's sparse env through `coll_carrier`
(Array/Hash only), and the join of a ternary's arms. Everything else
contributed nothing. The reference types the argument instead, so the port
named strictly less — and "less" cut BOTH ways:

```ruby
h['a'] = 1.to_s; h['b'] = 'x' if flag   # a1: ref's edges agree {String} -> FIRES;
                                        #     port's {}/{String} differed -> LOST ROW
h['a'] = 'x';    h['b'] = 'y'.to_i if flag # b1: ref's {String}/{String,Integer}
                                        #     differ -> SILENT; port agreed -> FP
```

Under-naming is the FP direction here, not the safe one: an unnamed store
leaves both edges' member sets untouched, so they agree, the join keeps the
carrier, and the receiver keeps witnessing where the oracle went quiet. The
standing sweep happened to contain no live instance — the b1 shape was a
latent FP the gate could not reach, which is why it was filed, not found.

## The fix

`coll_store_value_classes` now answers with `stmt_value_type` — the `type_of`
entry plus its statement-wrapper unwrapping — against the pass's OWN sparse
`tenv`, and erases the answer to member classes. The env choice is the
established precedent (the local-assignment arm already calls `type_of` with
it); the rules layer's `ScopedEnv` stays out — it is top-level-only and empty
inside a `def` body, which is where mutation code lives.

Erasure keeps members as bare class nominals (`TypeId` equality is the only
thing read), and follows `erase_to_rbs_named` where the type model has a
spelling rule:

| typed answer | member contributed |
|---|---|
| `Union[...]` | each member's erased class (`flag ? 'a' : 1` -> `{String, Integer}`) |
| `Refined` / `Difference` | the base's, recursively |
| `Intersection` | the first member's |
| `Nominal{C}` / `DataInstance{C}` | `Nominal[C]` — re-interned off the `ClassId`, so a PROJECT class names too (`CoreIndex::class_name_of` spells only the nine CORE_CLASSES) |
| `Singleton(C)` | the singleton carrier itself — already erased-level, and injective |
| `Constant` / `Tuple` / `HashShape` / `IntegerRange` | `class_name_of` -> `Nominal` (`"a"` -> `String`, `[1]` -> `Array`, `1..3` -> `Integer`) |
| `Dynamic[...]` | nothing — `Dynamic[top]` is the reference's own seed, in every instantiation |

Gains over the syntactic version beyond method calls: a non-carrier local
(`s = 'x'`) now names its class, a ternary with no `else` contributes the
`nil` the reference's `type_of_if` adds, and `case`/`when` values join the
same way. `stmt_value_type` keeps the `(a; b)` / `begin x end` / write-as-value
wrappers the classifier unwrapped by hand.

## Measured

- `a1` fires `call.undefined-method` at the same `(rule, line, column)` as the
  reference; `b1` is silent. Both probed in fresh temp cwds at the `v0.3.9`
  pin (`d0c370f7`), `--no-cache`.
- Fixture `108_mutation_rejoin_value_side` gains r19–r22: a method-call-typed
  store in each direction (`1.to_s` FIRES, `'y'.to_i` SILENT) and the
  local-carried pair (`s = 'x'` agreeing FIRES, disagreeing SILENT — r22 was
  an FP shape under the classifier, which could not name a `String` local).
- `run.rb` + `run_snapshot.rb`: **PASS**, 531/580 matched (+2 over the
  pre-change 529), 0 unregistered extras.
- `fp_audit.py --sweep`: **0 FP candidates / 9,337 files**, and every
  per-corpus matched count identical to a same-day master-side sweep
  (mastodon 420, gitlab-foss 1,087, mail 6,656, Ruby 14, dependabot-core
  156,703, concurrent-ruby 5,715, net-ssh 125, haml 5).
- `cargo +1.88.0 clippy --workspace --all-targets --locked -- -D warnings`:
  clean; `cargo test --workspace`: green.

## Residue (the approximation, now written on the member-set machinery)

The member set is still an APPROXIMATION: the reference carries real element
types and joins them (`Tuple[1]` vs `Tuple[2]` differ; `String` vs
`Dynamic[String]` differ), this pass compares erased class sets. Two
consequences, both FP-direction but pre-existing and unreachable by today's
sweep: two same-class-but-different-type stores agree here where the oracle
separates (measured: `h['a'] = [1, 'x']` + `h['b'] = [2] if flag` — both
erase to `Array`, port fires, oracle silent; identical under the old
classifier); and a store whose value types `Dynamic` contributes nothing
where the oracle adds a member (`h['b'] = 1..3 if flag` — `Range` is not a
registered core class so `nominal_or_untyped` degrades to `untyped`; again
identical under the old classifier — both are latent pre-existing FPs, not
regressions). Unbound locals still type `Dynamic[top]` —
the residue the issue itself leaves by design, and the measurement that
decides whether the faithful content-join port is worth its own arc.

## Review round (adversarial subagent; every claim oracle-probed)

A read-only adversarial review traced the diff against `mutation_widening.rb`,
`mutation_rejoin.rb`, `content_join.rb`, and `combinator.rb` and reported five
candidate FP seams plus four lost-row/latent ones. Probing each against the
v0.3.9 oracle in fresh dirs:

- **FIXED — store on a union-of-carriers collapsed the union** (pre-existing):
  `h['a']=1; h['b']='x' if flag; h['c']=1.5` — the port merged every arm's
  members into one carrier and fired; the reference's `widen_union` widens
  each arm and re-unions, keeping `Hash[…,I|S|F] | Hash[…,I|F]`, so it is
  silent. The store path now grows each union arm separately and re-interns
  the union, deduping only structurally identical arms (fixture r23; the
  converging r12/`h[:c]='y'` shape still re-joins and fires).
- **DEFERRED — Hash store KEYS are not members** (pre-existing):
  `h['a']=1; h[:b]=1 if flag` — the reference joins the key into `Hash[K,V]`'s
  K side and separates; the flat member set is value-side only and fires. A
  flat key∪value mix was tried and REVERTED: it hides a value-side class
  behind an identical key-side class, turning `h['a']=1; h['b']='x' if flag`
  (today correctly silent) into an FP. The faithful fix is a two-sided member
  encoding — a carrier-model change filed as a follow-up.
- **NOT REPRODUCED — seed literal elements**: `a=[1]…` vs `a=['s']…` is silent
  in BOTH engines; the claimed FP does not exist on this shape.
- **OUT OF SCOPE per the issue brief** — block-body store args
  (`coll_block_mutations`) and the `concat`/`insert`/`fill`/`replace`
  content-adders. Probes confirm the port fires where the reference is
  silent on `insert` (pre-existing); the issue explicitly excludes both.
- **Lost rows confirmed, deferred** (coverage gaps, not FPs): the
  `pinned_evidence` admissibility floor (`a=[1]` then `a<<'x'` vs `a<<1.5` —
  the reference floors both to `Array[1|u]` and fires; the port separates)
  and the `regrowable_carrier?`/`keep_precise_parameters` gate
  (`Hash.new(0)` keeps a proven `Integer` value side — the reference fires;
  the port separates).
- Stale comments corrected: the r15 fixture note (union arms now contribute
  both classes), `coll_widen_for_mutator`'s "re-joins into one carrier",
  `coll_store_value_args`'s doc (key side + remaining adders noted), and the
  `coll_erased_store_member` wrapper-erasure citation (the store path's
  normalizer is `widen_value_pinned`, which keeps the wrappers whole — a
  merge-direction residual, unreachable via `type_of` today).
