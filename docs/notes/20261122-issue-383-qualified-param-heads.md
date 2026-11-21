# Issue #383 — qualified param heads reach the ATM stub/acceptance gates

PR #808, merged head `f3f80461` (Approved `36d66555`, identical diff +
master merge). One-round Opus 5.5 adversarial review; design companion
skipped after one probe per provider (Fable quota, OpenCode/Grok balance).

## What landed

`stub_typed_param`'s `ClassInstance` arm asked only the leaf-keyed
`knows_class`, so a param head naming a QUALIFIED declared class
(`Outer::Inner`, `Thread::Backtrace::Location`, `M::Deep::Nest`) read as
"undefined" and `call.argument-type-mismatch` declined — a regression the
#378 port introduced with the #661 stub gate.

- `stub_typed_param` also consults `knows_qualified_class` — the oracle's
  `rbs_class_known?` resolves the written name absolutely against
  `env.class_decls ∪ env.class_alias_decls`. No leaf fallback: `Nope::Mutex`
  still declines although `Mutex` exists.
- `param_accepts_arg_class` routes non-toplevel heads through
  `param_accepts_ordering`, an approximation of `class_subtype_result`
  (`acceptance.rb`): `HOST_RESOLVABLE_CLASSES` answers the host
  `resolve_class`; RBS ancestor chains (qualified-keyed when the arg's
  normalised name is in the qualified registry) answer `actual <= target` /
  `subtype_result_via_ancestors`, complete-chain gated. Leaf spellings
  normalise through `resolve_short_unambiguous` (`Mutex`/`Thread::Mutex` are
  one host class). A head unknown to `qualified` stays `Unknown` — the stub
  gate owns it.
- `single_concrete_arg_class` admits a qualified arg name.
- Unit test `atm_param_accepts_arg_qualified_class_instance` pins the
  ordering on vendored names.

## Measured

~30-row project-`sig/` probe matrix (`harness/probe.py --dir`): every nil-
and non-nil-channel row matches the oracle's full tuple including message —
`deep(nil)`, rooted/union/optional/generic/multi-overload params,
`take_loc`, `take_nested`, `take_alias_q`, `take_my_t`, non-nil literals
(`1`, `"s"`, `:sym`, `2**70`, `"é"`), `&.` receiver, union args, `use`-map
aliases. All must-still-decline controls silent.

CI green 5/5. `fp_audit --gaps --sweep`: **0 FP**, gap totals identical to a
master binary — no sweep row exercises qualified param heads (the sweep
loads no project `sig/`), so the measured outcome lives only in probes.

## Filed from review (none blocking — edge shapes adjacent to the fix)

- **#809** — FP: `param_accepts_ordering` proves Disjoint from RBS chains
  where the host hierarchy says `<=` (`Errno::EWOULDBLOCK ≡ Errno::EAGAIN`
  and ~50 macOS `Errno::E* ≡ Errno::NOERROR`; `Random`/`Gem::MissingSpecError`
  superclass divergence). Fix: extend `harness/host_classes.rb` to emit
  canonical `Module#name` + host ancestors.
- **#810** — FP/drift: alias-leaf ambiguity + no class-alias→target map
  (`ConditionVariable` ambiguous via `MonitorMixin`; `Queue`/`Mutex`). The
  map would also close the `take_alias` decline and label drift.
- **#811** — drift: `expected:` labels render leaf-retained names
  (`Mutex`/`SourceList?`/`Widget` vs their qualified forms).
- **#812** — the issue's "also" list: alias generics, `Range[T]` covariance,
  `Marshal::_Source`, tuple/record/proc/`singleton`/`bool` heads.

## Deliberate declines

Class-alias param heads (`class Aliased = Thread::Mutex`) stay declined:
the oracle renders the resolved TARGET label (`expected Thread::Mutex`),
which needs #810's map — firing `expected Aliased` would be a drift-FP.
Block-param typing (`blk { |b| b.wut }`) is a separate family.
