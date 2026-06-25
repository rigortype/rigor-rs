# rigor-rs

A Rust reimplementation of **Rigor**, the type-aware bug finder for Ruby. Driven by performance and single-binary distribution, it aims to reach full parity with the Ruby tool in phases and eventually replace it.

## Language

**Rigor**:
The type-aware bug finder for Ruby being reimplemented here — parses Ruby with Prism, infers types from the values expressions produce, reads RBS as authoritative, and reports diagnostics with a zero-false-positive bar.
_Avoid_: type checker (Rigor reasons about values, not only classes), linter

**rigor-rs**:
This project — the Rust reimplementation of Rigor.
_Avoid_: the port, the rewrite (use the project name)

**Reference implementation**:
The existing Ruby `rigor` codebase, the default oracle for behaviour during the migration — authoritative except where a discovered defect is triaged out via the divergence registry and reported upstream.
_Avoid_: the original, legacy

**Diagnostic**:
A single finding emitted by analysis, identified by a rule id (e.g. `call.undefined-method`) and a source location, carrying a severity and a human-readable message.
_Avoid_: error, warning, issue (those name severities or are too generic)

**Diagnostic-set parity**:
The correctness bar for the migration: for a given input, the set of `(rule id, location)` pairs rigor-rs emits matches the reference implementation. Message wording may improve; the set must match.
_Avoid_: full parity, byte parity (those name different, stronger or vaguer bars)

**Differential harness**:
The verification mechanism that runs rigor-rs and the reference implementation over the same corpus and compares their diagnostic sets to measure parity.
_Avoid_: diff test, golden test (those are general techniques; this is the specific cross-implementation comparison)

**Rubydex**:
Shopify's Rust static-analysis toolkit. A candidate *optional accelerator* for rigor-rs's index layer — adopted behind the index trait only if a spike proves it exposes populator-grade detail. Not the default backend: rigor-rs owns its index layer.
_Avoid_: the backend (it is not the default backend), the indexer

**Index layer**:
The "what exists" half of analysis — project/RBS file discovery, RBS *declaration* extraction, constant resolution, ancestor linearization (with visibility), method/constant resolution, refinement/generic resolution. Owned by rigor-rs behind its own trait, built on the `ruby-rbs` parser — not delegated to an external indexer by default.
_Avoid_: lower half, environment (reserve "environment" for the reference implementation's class registry)

**Inference engine**:
The "what is the type of this expression" half of analysis — the type lattice, RBS method-type translation, flow-sensitive inference, narrowing, typed method dispatch, and the RBS extended-annotation grammar. Owned entirely by rigor-rs; this is its differentiated value.
_Avoid_: upper half, type checker, analyzer

**Constant folding**:
Computing a literal/refined type for a constant expression by executing the real Ruby method on a value built from literals (e.g. `1 + 2` → the literal `3`), gated by a purity allowlist + catalogue + result check. Only pure, deterministic methods qualify; non-deterministic ones (`Array#sample`, `Object#hash`) are never folded.
_Avoid_: constant propagation (that is the static-dataflow notion; folding here executes real Ruby)

**Ruby sidecar**:
The optional, cached helper process — the project's Ruby + bundle running a rigor-rs request loop — that executes the real Ruby calls rigor-rs does not reimplement natively (the long tail of constant folding and all plugin target-library invocations). Spawned lazily; its absence degrades to widening, preserving zero false positives.
_Avoid_: the Ruby process, the worker (name it)

**Divergence registry**:
The tracked ledger of intentional rigor-rs/reference differences excused from parity — each entry records the corrected behaviour and links an upstream report of a reference defect. The differential harness treats registered divergences as expected and every other divergence as a regression.
_Avoid_: ignore list, allowlist (those imply silent suppression; entries here are justified and linked upstream)

**Plugin**:
A host-agnostic fact producer / RBS synthesizer for a target library or DSL (e.g. Rails routes, ActiveRecord) — it returns facts, synthetic RBS, and diagnostics that the inference engine consumes; it is not itself part of the inference engine. A plugin is hosted either as a real Ruby plugin in the Ruby sidecar (default) or as a native Rust port.
_Avoid_: extension, addon (use "plugin")
