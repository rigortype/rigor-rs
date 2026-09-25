# `call.wrong-arity` on project methods defined in Ruby source

rigor-rs exempts every method a project defines in Ruby source from
`call.wrong-arity`, as the reference did through `v0.3.9`. Between `v0.3.9` and
`e59b7b89`, upstream removed that exemption in two steps:

- [rigortype/rigor#999](https://github.com/rigortype/rigor/pull/999) checks a
  source method that carries a trustworthy `sig/` declaration, or any authored
  inline `# @rbs` / `#:` type, against that declaration.
- [rigortype/rigor#1010](https://github.com/rigortype/rigor/pull/1010) checks a
  method declared nowhere against the `def`'s own parameter list. It stays silent
  wherever metaprogramming, reopened classes, mixins, subclasses or plugins could
  make a different definition run.

```ruby
class Greeter
  def hello(name) = "hi #{name}"
end
Greeter.new.hello   # e59b7b89: wrong number of arguments (given 0, expected 1)   port: silent
```

## Why this is out of scope

**The prize is zero rows.** The standing sweep at the `e59b7b89` pin has 14
`call.wrong-arity` gaps in total. Every one is on a core or gem class (`Redis`,
`Psych::DisallowedClass`, `Socket`, `Errno::ECONNRESET`, `Array` / `Enumerator`
`select`, `Concurrent::Channel`). None is on a method a project defines in
source. The survey corpora are real applications and libraries full of source
methods, and the reference found no mis-arity call among them to report.

**The FP surface is the whole design.** #1010 is a diagnostic about the code
that *runs*, gated by a list of reasons it might not be the `def` on screen. The
port would have to reproduce every silence condition exactly, and each one is a
place where "we do strictly less than the reference" can fail (it has failed
five times in this project; see the subset-argument lesson in AGENTS.md). The
closest precedent, singleton-receiver arity (`singleton-arity-checking.md`), was
ruled out for the same shape of reason: few rows, many ways to be wrong, and an
oracle still moving in that family. Here the oracle changed twice in one
release.

## What would change the decision

A measured gap count above zero on the standing sweep, or a project-shaped
corpus where the reference reports source-method arity errors the port misses.
With a count in hand, the port should be an allow-list: fire only where every
#1010 silence condition is *positively* established. It should not be a
deny-list of known exceptions. Probe each condition on both engines before
building.

## Prior requests

- #143: "`call.wrong-arity` exempts every source-defined method; upstream now
  checks declared ones (#999) and undeclared ones against the `def` (#1010)"
