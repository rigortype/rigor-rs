# Binding spec — `call.raise-non-exception`, `flow.shadowed-rescue-clause` (v0.3.0 RC)

Oracle: reference `47ec8625`. Sonnet investigation (source read + live probes,
2026-07-16). Zero-FP bar: every "MUST stay silent" row is load-bearing.

---

## 1. `call.raise-non-exception`

**Reference:** `lib/rigor/analysis/check_rules.rb:1478-1645`
(`raise_non_exception_diagnostic` + verdict fns), `rule_ids.rb:23`,
`rule_catalog.rb:217-237`. Severity **error** (all profiles), evidence_tier
high, since 0.3.0. Doc slug `#rule-call-raise-non-exception`.

### Fires iff ALL of

1. implicit self (`receiver.nil?`) — `obj.raise(x)` is always silent;
2. name ∈ {`raise`, `fail`};
3. no block;
4. first positional arg exists and is not Splat/KeywordHash/BlockArg/Forwarding
   (bare `raise`, `raise *a`, `raise(...)` silent);
5. scope resolved;
6. `raise`/`fail` NOT redefined reachably — four sites checked: same-file
   toplevel def, in-source Object/Kernel monkey-patch, enclosing class instance
   def, enclosing class singleton def;
7. `raise_operand_verdict(type) == :illegal` (trinary; only `:illegal` fires).

### Verdict function

- **Union**: recurse per member; all illegal → illegal; all legal → legal;
  ANY mixed/unknown → unknown (silent). `c ? 42 : "msg"` silent;
  `c ? 42 : :sym` fires, operand renders `42 | :sym` (branch order, unsorted).
- **Singleton(class)** (`raise Array`):
  - unknown if: class nil / no env / open-receiver stub / **project-discovered
    class (unconditional bail — the most important gate)** / not RBS-known.
  - else order vs `Exception`: equal/subclass → legal; **superclass OR
    disjoint → illegal** unless the SINGLETON side defines `#exception` (duck).
  - **No module exclusion**: `raise Comparable`, `raise Class`, `raise Object`
    all FIRE (probe-confirmed) — operand renders `singleton(X)`.
- **Instance-typed** (everything else):
  - resolve concrete class name; unknown if nil/no env.
  - unknown if class ∈ `{Class, Module, Object, BasicObject}`
    (`RAISE_UNEXACT_INSTANCE_CLASSES` — instance path ONLY, do not apply to
    the singleton path).
  - unknown if project-discovered (even `CustomError < StandardError` —
    probe-confirmed silent).
  - unknown if not RBS-known or RBS says module.
  - order vs `String` first: equal/subclass → legal.
  - else vs `Exception`: equal/subclass → legal; disjoint → illegal unless
    duck `#exception`; **superclass → unknown** (asymmetric with the
    singleton path — preserve exactly).
- **Duck check**: in-source `def exception` on the relevant side, else RBS
  lookup; an UNBUILDABLE RBS definition returns true (assume duck — a
  structural gap never manufactures a firing).

### Probe matrix (fires: `raise 42`, `raise :sym`, `raise nil`, `raise Array`,
`raise({a: 1})` → `{ a: 1 }`, `raise 1..2`, `fail 3.14` (message says `fail`),
`raise Comparable`/`Class`/`Object` → `singleton(X)`, `raise Time.new` → `Time`,
all-illegal union. Silent: Exception class ± msg arg, `"str"`,
`ArgumentError.new`, ANY project class (instance or class), bare raise,
Dynamic operand, mixed union, duck class, explicit receiver, any reachable
redefinition, unresolved constant (`raise NotAThing`).)

### Anchor + message

Anchor = the `raise`/`fail` method-name token (rigor-rs `Call.message_span`).
Message (em-dash U+2014):

```
`<name>' operand types as <type>, which is not an Exception class, an Exception instance, a String, or an object defining `#exception' — this raises TypeError at runtime
```

`<name>` = `raise`|`fail` verbatim; `<type>` = describe(:short) via the
existing `describe_named` renderer (byte-parity path — `singleton(X)`, bare
nominals, scalar inspect, `{ k: v }`, `a..b`, `A | B` branch order). JSON
carries `method_name`, no `receiver_type`.

### rigor-rs attachment points

- `raise 42` lowers as ordinary implicit-self `Node::Call` — but the existing
  per-call rule walk iterates `receiver: Some(..)` only ⇒ needs its OWN pass
  (template: `check_always_raises` for typer-verdict shape,
  `unresolved_toplevel_diagnostics` for the receiver-None walk, WITHOUT its
  toplevel-only filter — raise fires inside method bodies).
- Typer already covers all needed operand typing (Constant/Singleton/Union in
  branch order/HashShape/ranges).
- **New substrate: public class-ordering API** on rigor-index
  (`Equal/Subclass/Superclass/Disjoint/Unknown` — `ancestors()` in rbs.rs:882
  is currently private) + a class-vs-module bit (ClassEntry has none). A
  narrower `ordering vs {Exception, String}` helper suffices for this rule.
- Project gate: `SourceIndex::knows_class` — bail unconditionally.
- Duck: `SourceIndex::class_has_method(_, "exception")` +
  `CoreIndex::class_has_singleton_method`; add a source-level singleton
  counterpart if absent.
- Redefinition gate: `SourceIndex::is_toplevel_def` + a new innermost-enclosing
  ClassDef lookup (span-containment technique as in
  `unresolved_toplevel_diagnostics` scope_spans).
- Wiring: `CALL_RAISE_NON_EXCEPTION` const, catalog (severity error), rules
  lists, explain.rs.

---

## 2. `flow.shadowed-rescue-clause`

**Reference:** `check_rules/shadowed_rescue_collector.rb` (~230 lines),
builder `check_rules.rb:1798-1814`, `rule_ids.rb:39`, `rule_catalog.rb:408-437`.
Severity warning (lenient info / strict error), evidence_tier high, since 0.3.0.

### Fires iff (per later clause)

- ≥2 rescue clauses in the SAME BeginNode chain (never across nested begins);
- later clause fully "certified" (every name resolves lexically to a CLASS
  with known ancestry — module / unresolved / dynamic / splat ⇒ opaque, out);
- EVERY later name is covered by SOME certified earlier clause
  (`covered_by?`: string-equal, or class_ordering equal/subclass, or —
  on `:unknown` — the project discovered-superclass chain walk,
  namespace-resolved longest-prefix-first, cycle-guarded, depth cap 32).
- Opaque earlier clauses contribute NOTHING (not "cover everything").

Certification details: bare `rescue`/`rescue => e` = implicit `StandardError`.
Only ConstantRead/ConstantPath (incl. `::Foo`) qualify; lexical resolution
tries innermost-namespace-qualified candidates outward. Project classes
certify ONLY with a discovered superclass (`class Foo < Bar`); a bare
`class Foo`/`module Foo` is indistinguishable in discovery ⇒ uncertified.
(NOTE: the OPPOSITE polarity of raise-non-exception's project gate and module
handling — do not unify.)

### Probes

Fires: StandardError→ArgumentError; bare→ArgumentError; exact dup;
StandardError→(ArgumentError, TypeError) [multi-class arm, all covered];
(ArgumentError, TypeError)→ArgumentError; project `CustomError <
StandardError` after StandardError; Exception→StandardError; def-level rescue.
Silent: narrow→wide; partial coverage (one name uncovered);
unresolved earlier OR later; `rescue Kernel` (module); splat `*ERRORS`;
dynamic `rescue klass`; disjoint siblings; nested begin vs outer;
line-suppressed.

### Anchor + message

Anchor = later (dead) clause's `rescue` keyword (RescueNode start).

```
shadowed `<clause_source>': every exception class it names is already caught by the earlier `<earlier_source>' (line <N>) clause, so this clause can never run
```

- `<clause_source>` = `rescue` + the RAW source slices of the exception
  expressions, comma-joined (as written, un-canonicalized).
- multiple covering clauses: `` `<src>' (line N) `` joined with `" and "`,
  trailing word pluralizes to `clauses`.

### rigor-rs attachment points — hard blocker first

- **Lowering**: `Node::BeginRescue { body, span }` flattens all clause
  structure away. Needs:
  ```rust
  BeginRescue { body, clauses: Vec<RescueClause>, span }
  struct RescueClause { exceptions: Vec<NodeId>, body: Vec<NodeId>, span: Span }
  ```
  from the existing Prism walk (`rescue_clause()`/`subsequent()`/
  `exceptions()`/`statements()`). Plan as its own slice; design it so the
  return-in-ensure slice's `ensure_body` and this coexist.
- Constant resolution: check for an existing qualified-name resolver on
  ConstantRead/ConstantPath in rigor-infer before building one.
- Same shared class-ordering primitive as rule 1 + is_module bit; module
  polarity DIFFERS (here modules are excluded).
- `SourceIndex` per-class `superclass: Option<String>` is the
  discovered-superclass analog for the project chain walk (depth 32 + seen-set).
- Pure syntax + ancestry — no Typer needed. Self-contained module
  (`crates/rigor-rules/src/shadowed_rescue.rs`), own pass over BeginRescue.
