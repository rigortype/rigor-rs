# #380 — closure mutations under non-descending containers never replayed

PR #389, merged `c75a8c9` (squash of `cfe765d`, `a0f7134`, `783d0c0`).

## Symptom

`entry_descend` flat-applied `Loop`/`Case`/`When` subtrees and
`BeginRescue`'s else/ensure arms via `apply_subtree_effects` — which
deliberately does not cross `FlowEdge::Barrier` — and never descended into
the site-holding child. A literal block/lambda nested under one therefore
never replayed its own `closure_mutations`:

```ruby
h = {a: 1}
h[:a] ||= "s"
while c
  [1].each { h.default ||= 0; h[:a].frobnicate }  # master: FP @in-body
  break
end
h[:a].frobnicate                                 # fires on both
```

## Fix (two parts, two review rounds)

1. `entry_descend_site_child` descends into the one `flow_children` child
   holding the site after the flat envelope. `Loop`/`Case` use it
   directly; `When` iterates `body` only — the oracle does not evaluate a
   condition-position block into the read's scope (`when [1].each { w; r }`
   still fires in-body, round-1 fix `a0f7134`). `BeginRescue` uses it for
   else/ensure fallthrough; `main_body`/`rescue` clauses already descended.
   Non-Sequence `Statements` carriers (`Recovered`/`Jump`/`Inert`) stay
   flat — the oracle does not evaluate them as call sites either.

2. `preserve_env` threads through `entry_descend`/`entry_children`
   (round-2 fix `783d0c0`). The container-originated descent skips the
   barrier/lambda/def captures of `flow.env` — the end-of-FILE env, whose
   install resurrected post-statement rebinds into earlier blocks
   (`x = "s"; while x; each { x.upcase }; break; end; x = 1` fired
   `for 1`). The top-level/`begin` main-body/`rescue`-clause capture paths
   are unchanged — same FP family, pre-existing on master, filed as #390.

## Measured outcome

- Review-regression rows (post-container local rebind): all silent on both
  engines — `while`/`until`/`for`/`case`/`ensure`/`else`/lambda-in-loop.
- Type drift resolved: `x = 1; while x; each { x.upcase }; x = "s"` reports
  `for 1` on both (was `for "s"` on the first head).
- Coverage gain: `rescue => e` under a loop now fires
  `e.baz for StandardError` matching the reference (master silent).
- `fp_audit --gaps --sweep` on the final head: **0 FP**, 9,337 files,
  815 gaps (unchanged).
- Diagnostics-ordering `≠` rows (port puts flow errors before
  `unresolved-toplevel`) are identical on master — pre-existing.
- Reviewers: Opus r1 Needs fix → r2 Approved; Grok 4.6 r2 Approved.
- Filed: #390 (`flow.env` capture family), comment on #386 (`if q` with
  nil-bound local under a container stays live — dead-fold doesn't reach).

## Tests

`container_nested_block_attr_write_drops_indexed_narrowing` (12 suppression
+ 4 carrier/read-before-write controls + `when`-condition control) and
`container_nested_block_keeps_flat_env` (7 post-rebind silent rows +
nested-composition + `for 1` drift control) in `crates/rigor-cli/tests/check.rs`.
