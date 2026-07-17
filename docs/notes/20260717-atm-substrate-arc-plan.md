# ATM shared-substrate arc — 3-slice plan (call.argument-type-mismatch)

Sonnet plan 2026-07-17. NOTE: the plan's "bundle install broken" risk is
SPURIOUS — invoke the reference as `ruby -I reference/rigor/lib
reference/rigor/exe/rigor` (as the harness does), NOT via bundler (verified:
prints 0.2.9). Implementers MUST re-read the cited reference lines and run
live probes before coding.

Reference: check_rules.rb:1884-2394 (rule, both channels, messages),
acceptance.rb (only the `no?` verdict is consumed by the rule),
rbs.rs:1305-1320 (ingest matches only Class/Module — TypeAlias/Interface are
dropped today), rbs.rs:1654-1721 (method_signature discards param types).

## Slice 1 — retention (branch `atm-substrate-1`)
- Retain per-overload OverloadSignature: required/optional positional
  `RetainedParamType` one-level tag enum {ClassInstance, Alias, Interface,
  Union(Vec), Optional(Box), Other(string leaf carrying to_s for labels)} +
  presence flags (rest positionals, req/opt/rest keywords, trailing).
  ADDITIVE alongside the merged arity envelope — existing consumers must stay
  byte-identical (gate = ZERO diagnostic diff on all corpora, not just 0 FP).
- NEW top-level ingestion: type_alias_defs (name → tag, cycle-capped) +
  interface_method_names (name → required method names). The vendored
  ruby-rbs parser already exposes TypeAlias/Interface/AliasType/InterfaceType.
- Measure memory delta (RSS on a gitlab-foss lib run); est. low-single-digit MB.

## Slice 2 — acceptance walk (branch `atm-substrate-2`)
- Build on CoreData::class_ordering (live, raise-non-exception mileage), NOT
  rigor-types/relations.rs (dead skeleton — cannot prove No).
- `admits_nil(tag)` / `accepts_arg_class(tag, class)`, conservative-true
  default; ClassInstance: NIL_COMPATIBLE {NilClass,Object,BasicObject,Kernel},
  ordering Disjoint=false else true; Alias: bounded expansion; Interface: all
  required methods present via class_has_method (verify embedded
  nil_class.rbs completeness FIRST — the one real FP risk); Union: OR;
  Optional/Other: true.

## Slice 3 — the rule (branch `atm-rule`)
- Envelope: receiver concrete + RBS-known; skip UNIVERSAL_EQUALITY {== != eql?
  equal? <=>}; plain-positional-only (AUDIT rigor-parse Call.args
  splat/kwarg distinguishability first); does NOT skip on discovered_method
  (RBS authoritative — divergence from the undefined-method precedent,
  comment it at the rule site).
- Single-overload: argument_check_eligible (no rest/kw/trailing); nil channel
  (param_admits_nil) + non-nil channel (skip if either side Dynamic; fire
  only on proven reject). Ivar-nil escapes trivially satisfied (ivar reads
  are Dynamic) — FP-safe.
- Multi-overload: every overload eligible + param at index; nil channel = ALL
  overloads reject nil; non-nil channel = COERCE_DISPATCH_METHODS {+ - * / %
  ** & | ^ << >> < > <= >=} excluded, arg must be a single concrete RBS-known
  class, all overloads reject. Labels: per-overload written types uniq'd
  (first-seen order) " | "-joined; single-overload message has the
  `parameter \`name' of` prefix, multi-overload does not. Severity error.
- Live probes A-J before implementing; explicitly test Integer#+ overload
  count (the coerce exclusion only guards the multi path) and rule precedence
  vs wrong-arity at one call site.

## Gates (every slice)
cargo test/clippy; slices 1-2: ZERO diagnostic diff on all corpora; slice 3:
oracle E2E byte-parity + fixture + run.rb/run_snapshot 0 FP + fp_audit 0 FP
on gitlab lib + app/models + mastodon app (expect ATM gaps 2→0 gitlab, 1→0
mastodon).
