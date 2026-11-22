# Issue #809 — param_accepts_ordering answers from the host hierarchy mirror

PR #815, merged head `0bd282da` (Approved `189e2e73` + test-only `0bd282da`).
Two-round Opus 5.5 review: round 1 `Needs fix` (namespace-relative param
heads → new root-class FPs), round 2 fix-confirmation `Approved`. Fable
design companion and Grok advisory unavailable (provider quota/balance —
recorded in the PR).

## What landed

`param_accepts_ordering` proved `Disjoint` from RBS ancestor chains; the
oracle's `class_subtype_result` (`acceptance.rb`) resolves both names in its
own host process (`resolve_class`: `const_get`, no autoload, no
`const_missing`) and answers `actual <= target` on the LIVE hierarchy. RBS
disagreements were port-only `call.argument-type-mismatch` FPs, all needing a
project `sig/` param naming the qualified class:

- host aliases the RBS models as distinct classes: `Errno::EWOULDBLOCK` IS
  `Errno::EAGAIN` on POSIX (~50 `Errno::E* ≡ Errno::NOERROR` on macOS);
  `Mutex`/`Queue`/`ConditionVariable`/`SizedQueue` ARE `Thread::*`, and the
  leaves are ambiguous in the leaf map (`MonitorMixin::ConditionVariable`);
- host ancestry RBS writes differently: `Gem::MissingSpecError < Gem::LoadError`,
  `Random` includes `Random::Base`/`Random::Formatter`,
  `IO::EAGAINWaitReadable < IO::WaitReadable`.

- `harness/host_classes.rb` now emits per resolvable name a `HostClass` —
  `canonical` (`Module#name`, aliases collapse) and `ancestors`
  (`ancestors.filter_map(&:name)`). Regenerated `host_resolvable.rs` (1,971
  names; the environment-bound set shifted ~200 entries — `Delegator`,
  `JSON::Ext::Generator::GeneratorMethods::*` in; `JSON::GeneratorMethods`,
  `JSON::ResumableParser` out — same pin `bbea1bac`, different generator-host
  gems, per the header contract).
- `param_accepts_ordering` consults the mirror whenever the arg name
  host-resolves — before the `toplevel_classes` deferral and again after leaf
  normalisation. Both-resolve ⇒ canonical identity, `ancestors.contains`
  (Subclass), reverse (Superclass), else Disjoint; actual-only ⇒
  `subtype_result_via_ancestors`' spelled-name ancestor scan (`no` when
  absent). The old `complete && host_resolvable_class(a)` Disjoint tail is
  unreachable now; the RBS fallback is accept-side only.
- `retained_param_type`'s `ClassInstance` arm resolves RELATIVE param heads
  lexically at build time (`resolve_lexical_name`, `resolve_namespace0`
  shape): head innermost-scope outward through the member `enclosing` chain
  then root, committed scope (tail must follow — no outer backtracking),
  `::`-rooted/`use`-anchored names skipped. Without it the mirror read
  `(Class c)` inside `module Digest` as `::Class` — the review's Finding-1
  FP — and the `expected:` label kept the written leaf. The resolved name
  fixes both: verdicts AND labels (`expected Digest::Class`, `expected
  Thread::Queue`, `expected MonitorMixin::ConditionVariable`).

## Measured

Fresh-dir `probe.py --dir`, ~50 rows: all 8 issue rows silent on both
engines (master fired on all 8). Controls fire with the full tuple:
`take_ea(1)`, `take_cv(Monitor.new)`, `take_tq(Mutex.new)`, `take_fm(Mutex.new)`
vs sig-class `Foo::Mutex`. Lexical-resolution rows match the oracle exactly
including `expected:` labels — Finding-1 silences, Finding-2 messages,
`Digest.take_c(1)` control, plus bonus closures (`W.take(1)` → `expected
W::Widget`, `Thread.take_l(1)` → `expected Thread::Backtrace::Location`,
`File.take_st(1)` → `expected File::Stat`, `Gem.take_le` → `expected
Gem::LoadError`, `Foo.take_i` → `expected Foo::Integer`, RBS commit-to-scope
tail-miss). Unit test `resolve_lexical_name_scopes` pins the walk.

Gates on `0bd282da`: `gate.sh` pass (docs_check, 140 rigor-index tests,
`run_snapshot.rb` 0 unregistered, 979/1084 matched); CI 5/5;
`fp_audit --gaps --sweep` **0 FP**, gap totals identical to a detached
`origin/master` run — the family lives behind project `sig/` params the
sweep cannot see; probes are the evidence instrument.

## Filed / disclosed (none blocking)

- **#817** — residual coverage losses, all present on master: class-alias
  param heads (`class Al = Real` — `normalize_namespace` not ported),
  nested-module singleton receiver lookup (`A2::B2.take_y`), flat-decl
  member context (`class Digest::Zed`), shadowed core param heads
  (`Bar::String`).
- **#810/#811** — top-level class-alias param heads keep the leaf label:
  `take_m(1)` now emits `expected Mutex` where the oracle says `expected
  Thread::Mutex` (master was silent — same-key drift at a formerly-lost
  row). The class-alias→target map closes both the label and the
  `take_alias` decline.
- `SizedQueue.new`/`SystemCallError.new` `wrong-arity` non-emits —
  pre-existing gap, master's too.
