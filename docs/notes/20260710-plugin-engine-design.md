# Plugin engine — design slice + value assessment (2026-07-10)

Design investigation of the Rails plugin engine (ADR-0013 architecture,
ADR-0027 contract), grounded in what rigor-rs already ships and a set of live
oracle probes. **Verdict: the plugin CODE engine is a large, interdependent
build whose rigor-rs-specific value is thin; the high-ROI plugin path is
expanding the PURE-RBS bundle mechanism that already ships.** The design ADRs
stay valid; this note records the sequencing + value reality that should govern
when/how the engine is built.

## Current state

- **ADR-0013 / ADR-0027 are accepted design** (two-kind model: sidecar-hosted
  Ruby default + native Rust port; the frozen hook surface `node_rule` /
  `dynamic_return` / `narrowing_facts` (née `type_specifier`, ADR-80) /
  `diagnostics_for_file`; manifest fields `open_receivers:` / `signature_paths:`
  / `additional_initializers:` / `protocol_contracts:`; FactStore + Kahn
  `prepare` ordering).
- **Live mechanism: pure-RBS bundles** — `rigor_index::plugins::BundledPlugin`
  (`id` + embedded RBS), config-gated via `.rigor.yml` `plugins:`, ingested
  through the same reopen-union merge as core RBS. Exactly one bundle ships:
  `activesupport-core-ext`.
- **Unbuilt:** every code-contribution hook, the manifest gates, the FactStore,
  and sidecar plugin-hosting.

## Value model (oracle-probed)

1. **Pure-RBS bundles = HIGH value, mechanism EXISTS.** Default (no plugins),
   rigor-rs fires `undefined-method` on `3.minutes` / `"x".squish` — Integer /
   String are core-known, the AS methods are not in core RBS, so they witness
   (an FP wall on any Rails project). Enabling `activesupport-core-ext`
   suppresses them (the RBS adds the methods → known → silent). So the
   highest-leverage plugin work is *more RBS bundles*, and it is bounded
   (vendor RBS + register), reusing the shipped mechanism.

2. **The code hooks / gates are thin in rigor-rs AND interdependent.**
   - rigor-rs's default leniency (witness only *fully-known core* classes)
     already avoids the FP classes the reference's gates exist to suppress:
     rigor-rs fires **0** `undefined-method` on `OpenStruct.new.anything`
     (OpenStruct is outside its known set ⇒ lenient), where the **reference
     fires** (OpenStruct is RBS-known, the dynamic field witnesses). So the
     `open_receivers:` / `method_missing`-exemption motivation is largely
     pre-satisfied by leniency.
   - **`open_receivers:` has no live consumer without `dynamic_return`.** It
     exempts a receiver *typed* `ActiveRecord::Relation`; but nothing types a
     receiver as `AR::Relation` unless the AR code plugin's model discovery +
     `dynamic_return` produce that type. A pure-RBS AR bundle declares the class
     but can't make any receiver *be* it, so `open_receivers` is inert. The gate
     and the code hook only deliver value **together**, as a package — there is
     no thin FP-safe slice with a paying consumer.
   - The reference itself measured the AR `dynamic_return` at **+0 gettable
     witnesses** over an ActiveSupport-aware baseline (recorded in
     `CURRENT_WORK` "STRATEGIC FINDING"): the code engine's coverage add is
     narrow (value-dependent returns / relation chaining), and rigor-rs already
     folds the general value-dependent cases natively (Tuple projections,
     if/case unions — this session).

## Recommendation / roadmap

- **Do NOT build the code engine as speculative thin slices.** It is
  interdependent (dynamic_return ⇄ open_receivers ⇄ model discovery ⇄ FactStore
  ⇄ sidecar) and only pays off as a full stack, on a Rails-scale use case whose
  gap is measured — the same discipline the flow-frontier note enforces ("never
  build a speculative slice without a valid-mode gap count predicting it pays").
- **The productization-relevant plugin work is PURE-RBS bundle expansion:**
  vendor additional core-ext-style RBS bundles (more ActiveSupport surface,
  other gems' core-ext) so a Rails/gem user gets the reference's plugin-enabled
  coverage through the shipped, FP-safe, bounded mechanism. This needs no engine.
- **When the code engine IS justified** (a measured Rails project where
  `dynamic_return`-typed receivers + gates would close real gaps), build it as a
  package per ADR-0013's strangler order — sidecar-hosted Ruby first (parity for
  free), native Rust ports hottest-first — not as isolated hooks.

## Session cross-reference

This is the third "big track, thin value" finding this session, all sharing one
root cause — **rigor-rs's leniency + pure-RBS design already captures the
FP-safe value, so the remaining large tracks are net-negative or gated/thin:**
(c) remaining CLI commands (substrate-blocked), (1) possible-nil / ivar
expansion (net-negative — ivar typing manufactures the FPs ADR-58 then
suppresses), and now the plugin CODE engine (interdependent, thin, no paying
thin-slice). The high-ROI work this session was the parity-faithful ports (a/b:
config-audit, diff, triage + hints, type-display, annotate, and the inference
precision folds) — that is where rigor-rs's marginal value lives.
