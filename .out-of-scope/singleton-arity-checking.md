# Singleton-receiver arity checking

rigor-rs does not check ARITY on a singleton receiver — `Time.at()`,
`Redis.new(x)`, `Psych::DisallowedClass.new(a, b)` draw no `call.wrong-arity`.
The singleton **argument-type** arm is a different matter and is already live
(`Base64.decode64(1)` and `Shellwords.shellsplit(1)` fire on both engines);
what is out of scope here is building the arity half to match the oracle.

## Why this is out of scope

**The prize is small and most of it sits in a family upstream is actively
retracting.** Measured on the standing sweep set at the `v0.3.9` pin
(`d0c370f7`, 9,337 files) there are **12** `call.wrong-arity` coverage gaps in
total:

```
[dependabot-core] wrong number of arguments to `new' on Errno::ECONNRESET (given 0, expected 2)
[dependabot-core] wrong number of arguments to `popen' on IO (given 4, expected 1..3)
[gitlab-foss/lib] wrong number of arguments to `new' on Redis (given 1, expected 0)   x2
[gitlab-foss/lib] wrong number of arguments to `select' on Array (given 1, expected 0)
[gitlab-foss/lib] wrong number of arguments to `select' on Enumerator (given 2, expected 0)
[mail]     wrong number of arguments to `new' on Psych::DisallowedClass (given 2, expected 0..1)  x4
[net-ssh]  wrong number of arguments to `getnameinfo' on Socket (given 2, expected 1)  x2
```

Two of the twelve are instance-side and not this concept at all. Of the ten
singleton rows, **seven are `.new`** — and `.new` arity is exactly where the
reference has been reporting diagnostics that are wrong about the program:
`.new`'s envelope comes from `BasicObject#initialize` (`0..0`) whenever the
class's own RBS declares no constructor, which is why `Redis.new(url)` reads as
"expected 0". That was filed upstream as
[rigortype/rigor#917](https://github.com/rigortype/rigor/issues/917) and upstream
has already begun retracting it: `v0.3.9`'s
[#946](https://github.com/rigortype/rigor/pull/946) stopped reporting
`Gem::Specification.new("mygem", "1.0.0")`, and `Gem::Specification` duly
disappeared from the census between the `v0.3.8` and `v0.3.9` pins.

So building this would mean porting a diagnostic family whose oracle is moving
underneath it, to win a handful of rows — and getting the FIRST version wrong is
cheap to do and expensive to notice. Two measured traps recorded during the
mini-spec, both of which a naive build walks into:

- `class_has_singleton_method`'s class arm early-returns `true` behind a
  recorded **36 false positives** on dependabot-core. Arity-checking a surface
  the port cannot enumerate is how false positives are made; the instance side
  carries a conservative completeness gate for exactly this reason.
- The port believes `Math.new` exists while the reference answers
  `call.undefined-method`. A naive arity arm therefore produces a **rule swap**
  at the same `path:line` — one diagnostic replaced by another — which no
  count-based check can see.

This is a deferral-shaped cost with a permanent cause: the rows are few, the
oracle is unstable in precisely the sub-family that holds most of them, and the
port's own leniency gates make the safe version of the check expensive.

## Salvage — two parity fixes that do NOT depend on this

The mini-spec's build order opened with two items that are worth making on their
own merits and are not arity work:

1. **Singleton alias resolution** in `singleton_method_overloads` —
   `Shellwords.split` misses where `Shellwords.shellsplit` hits, which is an
   alias the port does not follow. Two ATM census rows.
2. **Dropping `Class` from the base surface for modules** — the port believes
   `Math.new` exists. Fixing that is the trap above, removed at the source
   rather than worked around.

If either is picked up, file it as its own issue describing the parity defect;
do not revive it as "singleton arity, slice 1".

## Prior requests

- #124: "The port never arity- or argument-type-checks a singleton receiver:
  `Time.at()`, `File.new()`, `StringIO.new(1,2,3,4)` are all silent"
  (its own follow-up comment corrected three of the claims and measured the
  prize at floor 4 / ceiling 22 rows against the `v0.3.8` pin)

## Measurement notes

`docs/notes/20260909-singleton-arity-mini-spec.md` carries the `v0.3.8`
partition of all 128 arity/ATM census rows by blocker. The `v0.3.9` re-measure
above is the number that matters now; the same sweep reports **150**
`call.argument-type-mismatch` gaps, which is a different concept and is not
rejected here.
