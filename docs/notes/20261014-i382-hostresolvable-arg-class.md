# #382 — `String?` param × sig-only arg class was a false positive

PR #394, merged `59ced2d` (squash of `e3e0b62` + `112eda7`).

## Symptom

```ruby
# sig/widget.rbs: class Widget; def take_optstr: (String? x) -> void; …; end
w = Widget.new
w.take_optstr(w)      # master: FP argument-type-mismatch; oracle: silent
w.take_string(w)      # same
w.take_widget("s")    # both engines fire (control)
```

Not an `Optional` bug — `translated_param_accepts` already recursed into
`Optional`. The whole plain-param family fired: any argument whose class
existed only in project `sig/` (or in vendored-but-unloaded stdlib RBS,
e.g. `BigDecimal`, `CSV`) was proven `Disjoint` by the port's RBS index.

## Oracle mechanism

`class_subtype_result` (`reference/rigor/lib/rigor/inference/acceptance.rb`)
resolves BOTH class names via `resolve_class` → `const_get` on its own host
process — no autoload, no `const_missing`. A name the host cannot resolve
answers `maybe`, never a proven `no`, regardless of how much the RBS
environment knows about it. Project `sig/` classes are RBS data, not host
constants, so `Widget` can never carry a rejection.

## Fix

`param_accepts_arg_class`'s `ClassInstance` arm
(`crates/rigor-index/src/rbs.rs`) gates rejection on
`host_resolvable_class(arg_class)` — a generated 1769-name table
(`src/rbs/host_resolvable.rs`) of the constants the reference `check` host
resolves at analysis time. `Disjoint` and `Superclass` both reject under
the gate (`actual <= target` is false in `Superclass` too); `Equal` /
`Subclass` / `Unknown` accept as before.

Generator: `harness/host_classes.rb` — loads `rigor/cli`, drives one real
`check` so the analysis-time lazy `require`s have fired, then walks the
constant tree with `resolve_class`'s rules (`constants(false)`, pending
autoloads declined, non-Modules skipped). Environment-bound: regenerate
under the pinned runtime on each pin bump (~644 names at CLI load → 1769
mid-analysis; the table is the mid-analysis set).

## Measured outcome

- Headline rows silent on both; controls `take_widget("s")`,
  `take_string(Pathname arg)`, `take_string(42)`, `take_optstr(4.5)`
  still fire.
- Two coverage losses closed and verified against the oracle:
  `take_int(Numeric arg)` (the `Superclass` arm) and sig-class target ×
  core arg (`take_widget("s")` already fired via `Disjoint`).
- `fp_audit --gaps --sweep` on the release build: **0 FP**, 9,337 files;
  the port's key set is byte-identical to master (173,730 keys) — the
  gate suppresses nothing the corpora reach. Raw gap total drifts
  ±5 between runs (reference/corpus variance, not this change).
- Reviewers: Opus 5.5 Approved, Grok 4.6 Approved (advisory; re-probed the
  full matrix, zero new FPs).

## Accepted coverage losses (filed / disclosed)

- Qualified arg names (`JSON::ParserError`, `URI::HTTP`, `Zlib::Error`):
  host-resolvable but `class_ordering` is `Unknown` on the qualified key.
- `Set.new` / `URI.parse` / singleton args / `bool` params / sig-return
  `Object`/`Time`/`Comparable`/`Pathname.new` paths; `if`/`else` joins
  (`Widget | Integer`) never reach the gate; nested `Widget::String`
  leaf-keys. → #395, #396.
- `YAML::*` alias constants are absent from the generated table (the
  generator skips aliased modules; `Psych::*` canonical names are
  present) — safe side.

## Tests

`argument_mismatch_requires_host_resolvable_arg_class` in
`crates/rigor-cli/tests/check.rs` (project `sig/` `Widget` args silent +
core/stdlib controls firing); `atm_param_accepts_arg_class_instance` in
`rbs/embedded_tests.rs` updated — the old comment claimed a conservative
"runtime value might be Integer" theory for `Numeric` → `Integer`, but the
live oracle fires there (`Superclass` rejects).
