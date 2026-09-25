# Dispatching a core/stdlib subclass's inherited calls through the ancestor's RBS

rigor-rs does not port upstream ADR-114 slice 1 (rigortype/rigor#1131). With
that slice, a Ruby-source subclass of a core or stdlib class resolves its
inherited calls against the ancestor's RBS, with `self` bound to the subclass.
The reference then types the results and reports on what follows. The port
leaves those results untyped and stays silent.

```ruby
class SubHash < Hash; end
SubHash.new.has_key?(:a).frob_zzz   # reference: undefined method `frob_zzz' for bool   port: silent
class MyErr < StandardError; end
MyErr.new("x").message.frob_yyy     # reference: … for String                          port: silent
```

## Why this is out of scope

This is a lenient gap, not a false positive. Only return-type precision moves,
and the negative gate on the subclass itself does not: `SubHash.new.frobnicate`
is silent on both engines. So porting it would only add coverage, and that
coverage measures at **zero** on the standing sweep.

The measurement was taken on 2026-09-25 at pin `e59b7b89`, over 9,337 files in
all 8 corpora. It compared the reference as pinned against the same checkout
with the slice's single dispatch hook short-circuited: the
`core_stdlib_ancestor_method` fall-through at the end of
`RbsDispatch.lookup_method` was replaced with `nil`. The two produced
byte-identical diagnostic output (174,548 rows each) in every corpus. That is
0 rows gained or lost and 0 message changes. The zero is not vacuous:

- **The disable is verified.** On the probe above, the pinned reference fires
  on both lines, and the short-circuited copy is silent on both.
- **The shape occurs in the corpus.** The sweep holds 91 files with
  `< StandardError`, 6 with `< Hash` (e.g. `Mail::IndifferentHash`), and 11 with
  `< StringIO`, `< StringScanner`, `< Array` or `< Set`.

No inherited-call result in those files feeds a call that a negative rule
catches.

AGENTS.md does not build a coverage slice that an `fp_audit --gaps` measurement
predicts will close nothing. Porting this would also not be free. It is a new
dispatch tier over the ancestor's RBS with `self` substitution. ADR-114 itself
flags its one risk as "wrong-precise propagation", so it is a new source of FPs,
bought for a row count of zero.

## What would change the decision

A corpus that exercises the shape: a sweep member whose reference gaps come from
a chain through an inherited core/stdlib method on a project subclass. Re-measure
the same way (short-circuit the hook, diff the full tuples) and re-open with the
count. Later ADR-114 slices that widen the negative gate on such subclasses would
also change the calculation, because they would move witnessing, not just
precision.

## Prior requests

- #144: "ADR-114 slice 1 is unported: a subclass of a core or stdlib class leaves
  inherited calls untyped (`SubHash#has_key?`, `MyErr#message`)"
