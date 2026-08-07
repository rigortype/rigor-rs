# ADR-0042 Slice 5 — qualified-key routing for the return-lookup family (2026-08-07)

Census "mechanism 3" ([gap census](20260807-gap-census.md), see the dated
correction there), re-scoped by measurement. The census's label ("inherited
singleton returns") and provenance suspicion (openssl) were both wrong.

## Measured mechanism (pin `v0.3.1`, probes re-runnable)

`Digest::SHA256.hexdigest("x")` — reference types `String`, rigor-rs
`Dynamic[top]`. The probe ladder isolates the cause:

| probe | rigor-rs |
|---|---|
| top-level project-sig hierarchy, inherited singleton (`Sub.make` with `Base.make: () -> String`) | **works** — fires on the chained typo |
| `Digest::Class.hexdigest` (directly on the DECLARING class) | Dynamic |
| `Digest::SHA2.hexdigest` (1 hop), `Digest::SHA256.digest`, instance `Digest::SHA256.new.hexdigest` | all Dynamic |
| namespaced project-sig replica: `Foo::Klass` existence witnessing (`k.frobnicate`) | **works** |
| same replica, returns (`k.imake.frobnicate`, `Foo::Klass.make.frobnicate`) | silent — the method is found, its `-> String` return is LOST |

So: **not** ingestion (`stdlib/digest/0/digest.rbs` is byte-identical between
the reference's rbs-4.1.0 and `crates/rigor-index/vendor/rbs`; `digest` is in
both `DEFAULT_LIBRARIES` — `default_libraries.rb:24` /
`crates/rigor-index/src/rbs.rs:42`), **not** ancestor-chain walking (works for
top-level names). The return-type lookups read only the SHORT-key `self.classes`
map: `singleton_method_return_inner` (`crates/rigor-index/src/rbs.rs:1531`)
opens with `self.classes.get_key_value(class_name)?` and the caller passes the
QUALIFIED name (`crates/rigor-infer/src/lib.rs:1494`), so every namespaced
receiver misses to `None` → Dynamic. ADR-0042 Slice 2 routed the EXISTENCE
gates through the qualified registry (`class_has_singleton_method`,
`rbs.rs:1227-1235`); the return family never got the same routing.

## Scope

Route the whole return-lookup family through the qualified registry when the
class name is namespaced, following the existing Slice-2 pattern:

- `method_return` (`rbs.rs:1387`) and `method_tuple_return` (`:1414`)
- `singleton_method_return` (`:1489` / inner `:1531`) and
  `singleton_method_tuple_return` (`:1431`)
- the twins: `method_return_nilable` (`:1571`), `method_return_with_block`
  (`:1596`), and any `*_is_void` sibling — enumerate by grepping the callers in
  `crates/rigor-infer/src/lib.rs:1483-1510` and `rbs.rs` 1300–1700 so none is
  missed (a half-routed family would type SOME chains and not others).

Requirements:

1. **Namespace-aware superclass resolution.** Qualified entries store their
   superclass reference LEAF-ONLY (`ingest_class`, `rbs.rs:2609-2643`), and
   both links of the digest chain are AMBIGUOUS leaves in the vendored set
   (leaf `Class` ∈ {`::Class`, `Digest::Class`}; leaf `Base` ∈
   {`Random::Base`, `Digest::Base`}). Resolution must try the enclosing
   namespace first (RBS resolves an unqualified superclass name relative to
   the declaring namespace outward), and **DECLINE on residual ambiguity** —
   declining loses coverage; guessing manufactures FPs. This is the slice's
   entire FP risk; concentrate review and tests here.
2. **`SELF_RETURN` interplay.** `resolve_call_site_return` (`rbs.rs:1398`)
   resolves the sentinel against the queried class name; on the qualified path
   it must resolve against the QUALIFIED receiver (so `-> self` / `-> instance`
   on a namespaced class yields the qualified nominal, mirroring what
   fixture 77 pins for short names).
3. Existence gates (`class_has_singleton_method`, `class_has_method`) are
   ALREADY routed — do not touch them; byte-identical behavior outside the
   return family is the review bar.
4. The ADR-0042 deliberate decline at `qualified_class_has_singleton_method`
   (`rbs.rs:895`, the measured 36-FP guard) is about EXISTENCE witnessing on
   qualified classes; this slice must not weaken it.

## Prediction (gap-set diff, per the chain-gap-prediction rule)

**~14 solid**: the gitlab-foss/lib `Digest::SHA256.hexdigest(...).first(N)` /
`.last(6)` family (bucketed under receiver `String` in the census dump —
archetype validated end-to-end: reference fires `undefined method 'first' for
String`, rigor-rs silent). Instance-side spillover on other namespaced
receivers may add a few. The census's "singleton receiver" bucket (12) is a
DIFFERENT mechanism (existence witnessing) and this slice closes none of it —
do not count it. Verify by diffing `gap_census.py --sweep --dump` output
against the 2026-08-07 baseline: rows may only MOVE from gap to matched; zero
new FP rows.

## Verification (binding)

- Unit tests in `crates/rigor-index` reproducing the probe ladder (each rung),
  the ambiguous-leaf DECLINE (leaf `Base` must not resolve to `Random::Base`),
  and the namespaced project-sig replica (instance + singleton returns).
- New fixture `harness/corpus/82_qualified_return_lookup.rb`:
  `Digest::SHA256.hexdigest("x").frobnicate_zzz` positive + a control that must
  keep firing + a negative control on an ambiguous-superclass shape. Verify
  expected output against the oracle; regenerate snapshots
  (`ruby harness/snapshot.rb`).
- Gates, all green: `cargo build --offline && cargo test --offline`,
  `ruby harness/run.rb`, `ruby harness/run_snapshot.rb`,
  `python3 harness/fp_audit.py --gaps --sweep` (**0 FP / 9204 files**),
  `python3 harness/docs_check.py`, fresh-target clippy
  (`CARGO_TARGET_DIR=<fresh> cargo clippy --workspace -- -D warnings`).
