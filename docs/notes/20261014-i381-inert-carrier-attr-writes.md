# #381 — inert-carrier attr writes inside blocks kept dropping indexed records

PR #400, merged `f15be3c`. Found in review of #377 (#366), extended in review
of #387 (#379): after the owner re-key, inert-position attr writes replayed
at NESTED depth too, widening the loss.

## Symptom

```ruby
h = {a: 1}
h[:a] ||= "s"
[1].each { END { h.default ||= 0 }; h[:a].frobnicate }
h[:a].frobnicate
# ref:    fires @3:43 + @4:7  (`frobnicate for "s" | 1`)
# master: silent @3:43 — coverage loss
```

`closure_mutations` (collect-time side table replayed inside each deferred
body's own scope, #366) collected EVERY `h.attr op= v` under a block/lambda —
including writes inside `StatementsKind::Inert` carriers (`END { }` /
`BEGIN { }` bodies, `defined?` operands, `super`/`yield` arguments), which
the reference never evaluates. The replay ran `widen_attribute_write`,
dropping `h[:a]`'s rooted records → untyped → silent.

`toplevel_mutations` has had the `in_inert_carrier` retain since #357;
`closure_mutations` was built without it. Fix: the same retain at collect
time. One caveat probed during review — `in_scanned_inert_carrier` is NOT
exempted: attr writes are not `[]=`/mutator content marks, so #312's
iterated-body text scan never lands them (`while c; each { super(h.attr ||= 0) }`
stays silent on the oracle too).

## Measured outcome

- Issue row + nested-depth row: byte-identical to the oracle (2 diagnostics
  each, `"s" | 1` messages).
- Reviewer grid (40+ shapes): every branch diagnostic ⊆ ref; all remaining
  divergences identical on master (pre-existing). Bonus recoveries: `->`,
  `do…end`, `map`, `for`, `compare_by_identity` receivers, reads inside the
  END body itself.
- Must-still-fire: evaluated `h.default ||= 0` in a block still widens.
- Sweep 0 FP / 9,337; `check.rs` 18/18, `rigor-infer` 346/346.

## Deferred findings (filed)

- #401 — `when`-condition attr writes inside blocks replay into the block
  scope (the `Recovered::blocked` sibling of this envelope; toplevel keeps
  them deliberately per #343/#357).
- #402 — deferred `END`-body INDEX writes (`h[:a] ||= v`) are dropped; the
  reference lands them at exit (different write family — needs the
  deferred-exit model, not the inert drop).
