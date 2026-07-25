# MultiWriteNode substrate — spec (2026-07-25)

rigor-rs has no arena lowering for Ruby's multiple assignment (`a, b = rhs`).
`MultiWriteNode` falls through `collect_recoverable_children`
(`crates/rigor-parse/src/ast.rs:2122-2186`, which recovers six local-read /
write / call kinds but **not** `LocalVariableTargetNode`) into a
`Node::Statements` carrier — or `Node::Other` when the RHS has no recoverable
child. The LHS names are dropped from the arena entirely.

## This is a LIVE FP in `check`, not just a coverage gap (measured 2026-07-25)

`collect_flow_writes` (`crates/rigor-infer/src/lib.rs:2255-2286`) records only
`LocalVariableWrite` / `LocalVariableOpWrite` / mutator calls, so a multi-write
rebind never widens an earlier straight-line binding:

```ruby
def probe(other)
  x = 5
  x, _y = other, 2
  if x            # rigor-rs: flow.always-truthy-condition   reference: SILENT
    puts "truthy"
  end
end
```

Measured with the hardened oracle: rigor-rs fires at 4:6, the reference emits
nothing. The gap propagates to every consumer of `widen_flow_writes` /
`widen_penv_writes` — `flow.always-truthy-condition`, `flow.unreachable-*`,
`call.possible-nil-receiver`. It has not shown up in `fp_audit` only because
the shape (straight-line binding → multi-write rebind → condition on it) is
absent from the surveyed corpora; it is ordinary Ruby.

The same missing lowering forced the coverage command to hand-roll a
Prism-side taint walk (`crates/rigor-cli/src/coverage.rs:666-736`, explicitly
"multi-writes have no arena lowering") and is one reason the upstream ADR-58
massign-ivar cluster is unportable.

## Slice 1 — lowering + flow-writes + destructuring (THE FP FIX)

1. **`Node::MultiWrite`** in the arena, carrying an ordered target list plus
   the RHS value id. Targets must model the reference's `lefts` / `rest`
   (splat) / `rights` triple, including a NESTED multi-target (`a, (b, c) = …`)
   and an anonymous/ignorable target. Follow the enum's existing conventions
   (`name_span` for name-anchored diagnostics; a sibling struct like
   `RescueClause` is the precedent for a composite child group). Two exhaustive
   matches must gain the variant: `Node::span()` (`ast.rs:578-616`) and
   `node_kind()` (`crates/rigor-cli/src/type_of.rs:283-320`).
2. **Flow writes**: every bound target name is recorded by
   `collect_flow_writes`, which closes the FP above.
3. **Destructuring propagation**: port the reference's
   `MultiTargetBinder` (`reference/rigor/lib/rigor/inference/multi_target_binder.rb`)
   faithfully — `Type::Tuple` decompose with front/rest/back split, non-tuple
   RHS ⇒ every slot `Dynamic[Top]`, a MISSING slot ⇒ `Constant[nil]`, nested
   targets recurse, splat binds the middle sub-tuple, and **`soften_optional_slot`**
   (a `Union` slot containing nil drops the nil; a bare-nil slot stays nil) —
   that last rule is an explicit FP-discipline decision upstream (haml
   `parse_tag`'s 9-tuple), not an optimization. `Type::Tuple` already exists
   (`crates/rigor-types/src/ty.rs:176`) with projection folds.
4. **Statement value** is the RHS type (Ruby semantics: `(a, b = [1,2])`
   evaluates to `[1,2]`), matching `expression_typer.rb:119`.
5. **Retire the coverage.rs multi-write taint workaround** in favour of the
   arena (leave the `for`-index and `rescue =>` taints alone — separate forms,
   separate slice).

**Parity guard (must not regress):** `flow.dead-assignment` deliberately
SKIPS multi-writes in BOTH engines
(`dead_assignment_collector.rb:20-22`; `crates/rigor-rules/src/lib.rs:2323`,
test `dead_assignment_multi_write_is_silent`). Adding the lowering must not
turn destructured targets into dead-assignment candidates. Same for
`void_value_use` (`void_value_use_collector.rb:30-37`).

**Gates:** the usual battery PLUS `fp_audit` on real corpora — this is an
FP-safety change, so a corpus sweep is mandatory, and the probe above must go
silent while the harness stays 0 FP.

## Slice 2 — fixture 68 (`Process::Status`), fixtures → 100%

Depends on Slice 1. The remaining chain, all cited:
- **RBS tuple returns are dropped**: `crates/rigor-index/src/rbs.rs:2745-2764`
  matches only `ClassInstanceType`/`InstanceType`/`VoidType`/`Optional(...)`;
  a tuple falls to `None`. The API itself
  (`crates/rigor-index/src/lib.rs:285`, `Option<&'static str>`) cannot carry a
  tuple — it needs a richer return descriptor. The reference does this by
  translating `RBS::Types::Tuple` → `Type::Tuple`
  (`rbs_type_translator.rb:63,162-167`), with **no** special-casing of
  `Process` anywhere.
- **No registry id for an RBS-only class**: `source_index.rs:395-417` mints
  ids for non-core RBS classes only from source `ConstantRead` harvesting, and
  fixture 68 never writes `Process::Status`. `CORE_CLASSES`
  (`crates/rigor-index/src/lib.rs:46-56`) is a fixed 9-name list.
- The witness gate itself is already built —
  `qualified_class_has_method` (`crates/rigor-index/src/lib.rs:204`), per
  [ADR-0042 slices 3–4](20260719-adr0042-slices-3-4.md): "a value-typing
  wire-up, not new index machinery".

Expected yield: fixture 68's single missing diagnostic
(`undefined method 'frobnicate' for Process::Status`, 40:8) → harness
**70 fixtures / 0 gaps**.
