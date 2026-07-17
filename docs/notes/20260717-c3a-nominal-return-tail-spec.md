# Binding spec — C3a: `self.class` nominal-return tail (`self.class.name` → String)

Follow-on to the 2026-07-17 gitlab-foss lib UM/PN gap classification
(`docs/notes/20260717-constant-shadow-gate-spec.md`). Cluster C3 (~90 gaps):
receivers typed via a plain-class RBS RETURN (no literal value). Sub-lever C3a.

## Oracle probe matrix (fresh cwd per probe; reference v0.3.0 RC)

Rule columns: **UM** = `call.undefined-method`, **PN** = `call.possible-nil-receiver`.
"REF" = reference oracle, "RS-before" = rigor-rs on master.

| # | snippet (inside `class Foo` unless noted) | REF | RS-before |
|---|---|---|---|
| 1 | `self.class.frobnicate` | silent | silent |
| 2 | `self.class.helpr` (Foo has `def self.helper`) | silent | silent |
| 3 | `self.class.name.frobnicate` | UM `frobnicate' for String` | silent |
| 4 | `self.class.name.demodulize` | UM `demodulize' for String` | silent |
| 5 | `x = self.class.name; x.frobnicate` | UM (line of x.frob) `for String` | silent |
| 6 | `k = self.class; k.name.frobnicate` | UM `for String` | silent |
| 7 | `self.class.to_s.frobnicate` | UM `for String` | silent |
| 8 | `self.class.name.upcase` | silent (upcase ∈ String) | silent |
| 9 | `self.class.name.length` | silent | silent |
| 10 | `self.class.name.split("::").frobnicate` | UM `for Array[String]` | silent |
| 11 | `Foo.name.frobnicate` (Foo = enclosing project const) | UM `for String` | silent |
| 12 | `self.class.name.frobnicate` at **toplevel** | silent | silent |
| 13 | `self.class.name.frobnicate` in `module M` instance method | UM `for String` | silent |
| 14 | `self.class.name.frobnicate` in `def self.bar` | UM `for String` | silent |
| 15 | `Foo.frobnicate` (project class-method typo) | silent | silent |
| 16 | `String.frobnicate` (core class-method typo) | UM `singleton(String)` | **silent** (RS gap, not FP) |
| 17 | `"hello".class.frobnicate` | UM `singleton(String)` | silent |
| 18 | `"hello".class.name.frobnicate` | UM `for String` | silent |
| 19 | `Foo.new; x.class.name.frobnicate` | UM `for String` | silent |

### Optional-return probes (the "tier-3 optional-unwrap generally" question)

| # | snippet | REF UM | REF PN |
|---|---|---|---|
| O1 | `[1,2].find { }.abs` | silent | **silent** |
| O2 | `[1,2].find { }.frobnicate` | UM `frobnicate' for 2` (tuple elem!) | silent |
| O3 | `"abc".slice(5).upcase` | UM `upcase' for nil` (overload→nil!) | silent |

## Decisions

### PORT (oracle-proven, zero-FP)

**Part A — `self.class.name` / `self.class.to_s` tail → `Nominal[String]`.** In
`Typer::type_call`, when `method ∈ {name, to_s}` (no args) and the RECEIVER node
is the specific shape `(self).class` (a `class` call, no args, on a `SelfExpr`)
inside a lexical class/module (`enclosing_prefix` non-empty) → `Nominal[String]`.
Toplevel (probe 12) has no enclosing scope → declines → silent, matching REF.

**REVISED after the FP audit (initial approach was UNSOUND).** The first cut
typed `self.class` itself → `Singleton(enclosing project class)`. That produced
**12 false positives** on gitlab-foss/lib: `self.class.<class_method>` — calling
one of the class's OWN class methods from an instance method (`valid_provider?`,
`with_redis`, `aspects`, `default_options`, …), a ubiquitous idiom — routed
through the rule's `Singleton` class-method witnessing path, which sees only the
core RBS surface and CANNOT verify a project-defined class method, so it fired
`undefined method X for singleton(Config)` etc. where REF (which resolves against
the project class) stays silent. `index.class_has_singleton_method` is NOT
conservative for a project-class name that collides with a modeled surface, so
the "lenient on unmodeled class" assumption was false.

Fix: NEVER type `self.class` to a witnessable `Singleton`. Match the SPECIFIC
`(self.class).name`/`.to_s` shape and type ONLY the always-`String` tail; leave
`self.class` itself Dynamic. Then `self.class.frobnicate` / `self.class.<class
method>` stay silent (Dynamic receiver, probes 1/2/15 and the 12 FP sites), and
`self.class.name.<x>` witnesses on the String tail. Verified 0 FP.

**Part B — `name`/`to_s` on a `Singleton` receiver → `Nominal[String]`.** In
`type_call`, when `recv_ty` is `Type::Singleton(_)` and `method ∈ {name, to_s}`
→ `Nominal[String]`. Both methods are always valid on a class object and always
return `String` (REF unwraps the `Module#name : String?` optional). This lights
probes 3–7, 11, 13, 14 and, as a bonus, the core-singleton `Foo.name`/`Time.name`
chains (a `Singleton` from the `ConstantRead` arm). Downstream witnessing then
runs against the real `String` RBS (`.demodulize`/`.underscore` → UM; `.upcase`/
`.length` → silent, probes 8/9).

### DECLINE (unproven / FP-risk)

- **General `x.class`** (probes 17–19). REF types `x.class` → `singleton(X)` for
  any typed receiver and witnesses core-singleton typos (probe 17 fires
  `singleton(String)`). rigor-rs's core-singleton witnessing is NARROWER than
  REF (probe 16: `String.frobnicate` silent on RS = a MISSING gap, not FP), so
  opening general `.class` would only add MISSING coverage, never FP — but it is
  NOT in the identified gap set (all 21 C3a `for String` gaps are `self.class`),
  and typing arbitrary literals/instances to core `Singleton` risks a
  singleton-surface disagreement FP on a hot path. Not proven necessary → decline.
- **General tier-3 optional (`T?`) unwrap** (probes O1–O3). The oracle does NOT
  implement a clean `T? → T` unwrap. REF's optional-return behavior is entangled
  with concrete value tracking: `find{}` → the tuple element `2` (O2), `slice(5)`
  → `nil` (O3), and the straight-line `find{}.abs` fires NEITHER rule (O1). There
  is no sound generic unwrap to port; the existing tuple/HashShape folds already
  cover the value-pinned subset. Decline — porting a generic `T?→T` unwrap would
  also risk making rigor-rs's PN channel (separate `method_return_nilable`
  channel) fire where REF's does not.

### PN (possible-nil) zero-FP argument

Part B returns a **non-nilable** `Nominal[String]`. `nilable_source_class`
resolves the receiver's core class via `class_name_of`, which returns `None` for
a `Singleton` carrier → `self.class.name` never mints a nilable fact → PN stays
silent on `self.class.name.<x>` (probe 8 confirmed REF PN silent). Verified as a
gate post-implementation.

## Expected deltas / gates

- gitlab-foss lib UM: closes the ~21 `self.class.name.<x>` `for String` gaps
  (+ bonus `Time.name` chains). Expect 200 → ~180±10; **0 FP REQUIRED**.
- mastodon app + gitlab app/models: 0 FP; matched ≥ prior.
- Fixture + snapshot with must-stay-silent rows (probes 1, 8, 9, 12, 15).

## MEASURED results (final)

- cargo test: 731 pass (6 new C3a typer tests); clippy clean on touched crate.
- Harness fixture `64_self_class_nominal_tail.rb` + snapshot: run.rb (live
  reference) Matched(3), run_snapshot PASS, 0 unregistered FP.
- fp_audit --gaps, **0 FP on all three**:
  - gitlab-foss/lib: UM gaps **200 → 179** (closed 21); matched 991 → **1003**.
  - mastodon/app: matched **404**, 0 FP.
  - gitlab-foss/app/models: matched **537**, 0 FP.
- DECLINED as planned: general `x.class`, general `T?`-unwrap, and the
  method-body-local case (`n = self.class.name; n.typo`) — the last a MISS via a
  pre-existing flat-env limitation, orthogonal to C3a, not an FP.
