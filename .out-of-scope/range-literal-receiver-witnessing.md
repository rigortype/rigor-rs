# Witnessing `call.undefined-method` on a `Range` literal receiver

rigor-rs does not report an undefined method called on a `Range` **literal**
receiver. `(1..2).nonsense` is silent on the port, while the reference reports
it on both the `v0.3.9` and `e59b7b89` pins. The same receiver built with
`Range.new(1, 2)` already fires on both engines, so this is about the literal's
path into the witnessing gate, not about `Range` itself.

```ruby
(1..2).nonsense_zzz          # reference: undefined method `nonsense_zzz' for 1..2   port: silent
Range.new(1, 2).nonsense_zzz # reference: … for 1..2                                 port: … for Range
```

## Why this is out of scope

This is a lenient gap, not a false positive: the port reports less than the
oracle. The project only builds coverage it can measure, and this lever does not
register in that measurement. The standing sweep at the `e59b7b89` pin (9,337
files, 3,829 coverage gaps) has **zero** `call.undefined-method` gaps on a
`Range` receiver, literal or not. AGENTS.md is explicit that a coverage slice is
not built without an `fp_audit --gaps` count predicting it closes gaps. This
one predicts none.

Building it is also not free. The literal would need a `Range` carrier (or the
witnessing gate would need to stop declining it), and every carrier the port
adds has been a source of FPs through the shapes that consume it: the
collection-carrier arc (#128, #132) is the most recent example. Taking that risk
for a row count of zero is the trade AGENTS.md rules out.

## What would change the decision

A corpus that exercises the shape. If a future sweep member reports gaps of the
form `undefined method … for 1..2` (or a range with other endpoints), re-open
this with the count. ADR-109's bounded ranges (#130) touch the same node, and
porting them may give the literal a type as a side effect. If that happens,
re-probe this row before building anything separate.

## Prior requests

- #142: "A method call on a `Range` literal receiver never witnesses
  `call.undefined-method`: `(1..2).typo` is silent, `Range.new(1, 2).typo` fires"
