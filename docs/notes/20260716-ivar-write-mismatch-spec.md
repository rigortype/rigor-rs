# Binding spec — `def.ivar-write-mismatch` port (+ argument-type-mismatch deferral)

Oracle: reference `47ec8625`. Sonnet investigation 2026-07-16 (source read +
corpus-line confirmation + both-side probes).

## Verdicts

- **`def.ivar-write-mismatch`: BOUNDED — port now.** Reuses `class_name_of`,
  Typer/TypeEnv, ClassDef/ModuleDef method_bodies harvesting, the
  dead-assignment collector pattern. One precedented parser addition.
- **`call.argument-type-mismatch`: SUBSTRATE-BLOCKED — defer.** Needs
  per-overload/per-param RBS type retention (rbs.rs currently keeps only a
  merged (min,max) arity), an alias/interface degradation-recovery layer
  (`string`/`_ToStr` → the reference walks RAW RBS types for its nil channel),
  and a net-new acceptance/subtyping engine (`Inference::Acceptance` has no
  rigor-rs analog). 3 corpus gaps total; mastodon app fires ONCE in 1236
  files. Track as a shared substrate investment (would serve several rules).

## Rule semantics (`ivar_write_collector.rb` + check_rules.rb:286-391,1799-1804,1895-1904)

- Collector: walk each Class/Module node's DIRECT instance defs (barrier at
  nested Def/Class/Module; skip `def self.` bodies and class-body writes).
  Record every `@ivar = value` with `scope.type_of(value)`, grouped by
  (qualified class name, ivar name).
- ≥2 writes required. Class of a write = `concrete_class_name`
  (rigor-rs: `CoreIndex::class_name_of` is already the faithful analog) with
  TrueClass/FalseClass → `"bool"`.
- Canonical = FIRST write whose class != "NilClass" (leading `@x = nil`
  placeholders skipped). If that candidate's class resolves to None
  (Dynamic/Union/unmodeled) the WHOLE group is silent (later writes never
  inspected).
- Every write AFTER canonical fires iff its class is Some, != "NilClass"
  (clear-to-nil idiom always silent), and != canonical class.
- Message: `` instance variable `@ivar' on Class was previously assigned X; this write assigns Y ``
  anchored on the IVAR NAME token of the offending write. severity authored
  error; profiles {lenient: warning, balanced: warning, strict: error};
  evidence high; since 0.1.2.

## Confirmed corpus gaps

1. gitlab lib/system_check/incoming_email/imap_authentication_check.rb:39 —
   `@error = "<str>"` then `@error = error` in `rescue StandardError => error`
   → String vs StandardError. Needs increment (a): rescue-bound variable
   typing.
2. gitlab lib/uploaded_file.rb:42 — `@upload_duration = Float(kwargs[...])`
   then `= 0` in rescue → Float vs Integer. Needs increment (b): Kernel
   conversion NOMINAL fallback (reference types `Float(x)` as Float
   UNCONDITIONALLY even when unfoldable; rigor-rs kernel_fold currently
   declines non-constant args to Dynamic).

## rigor-rs attachment points

- **Parser**: `InstanceVariableWriteNode` currently lowers to the nameless
  shared `Node::VariableWrite` (ast.rs:1391-1397). Add
  `InstanceVariableWrite { name, value, name_span, span }` mirroring
  LocalVariableWrite (ast.rs:726-735). Keep VariableWrite for
  class-var/global writes; verify no consumer regresses (whatever matched
  VariableWrite for ivars must keep matching the new variant where behavior
  depended on it — check dead-assignment etc.).
- **Increment (a)**: `RescueClause` gains `bound_name: Option<String>`
  (`=> e`); bind it in the rescue-body TypeEnv to Nominal(exception class)
  (single-class clause; multi-class → probe the oracle; bare rescue →
  StandardError — PROBE the oracle for each before implementing).
- **Increment (b)**: kernel_fold — non-constant single-arg
  `Integer()/Float()/String()` (splat/shadow guards unchanged) falls back to
  Nominal of the conversion class instead of Dynamic. Verify vs oracle probes
  (incl. that downstream witnessing matches).
- **Collector**: new pass in rigor-rules following dead_assignments_in_def
  style over ClassDef/ModuleDef method_bodies. Rule already in
  ALL_CANONICAL_RULES; add const/catalog/IMPLEMENTED_RULES/explain (25→26).

## Must-stay-silent rows

nil-clear writes; leading-nil-then-typed (fires only on a THIRD conflicting);
first-non-nil candidate resolving to None kills the whole group; singleton-def
writes; same ivar name across different classes; class-body writes outside
defs; nested def/class barriers.
