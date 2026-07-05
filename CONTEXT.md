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
The cached helper process — the project's Ruby + bundle running a rigor-rs request loop — that executes the real Ruby calls rigor-rs does not reimplement natively (the long tail of constant folding and all plugin target-library invocations). Spawned lazily; its absence degrades to widening, preserving zero false positives. **Used by default** (the reversed policy): a run defaults to full fidelity and falls back to the sound subset only when Ruby is explicitly opted out or genuinely unavailable — see coverage posture.
_Avoid_: the Ruby process, the worker (name it); optional (it is the default, not opt-in)

**Sound subset**:
The diagnostic set rigor-rs emits WITHOUT the Ruby sidecar — a strict subset of full fidelity that is sound (never a wrong diagnostic, the zero-false-positive bar holds) but incomplete (omits findings that require executing real Ruby, which widen to `Dynamic` instead). What a `--no-ruby` run produces.
_Avoid_: degraded mode, reduced mode (name the guarantee: it is a *sound* subset, not merely lesser)

**Full fidelity**:
The diagnostic set rigor-rs emits WITH the Ruby sidecar available — equal to the reference's set (the diagnostic-set-parity target) and a strict superset of the sound subset. The default coverage posture.
_Avoid_: full parity (that names the correctness bar; this names the achieved diagnostic set of a sidecar-enabled run)

**Coverage posture**:
Which diagnostic set a given run is operating at — full fidelity (sidecar in use) or sound subset (no sidecar) — surfaced to the user so incompleteness is never silent (`rigor doctor`, a startup notice, and structured output metadata report it).
_Avoid_: mode, level (too generic; it names the completeness posture specifically)

**Divergence registry**:
The tracked ledger of intentional rigor-rs/reference differences excused from parity — each entry records the corrected behaviour and links an upstream report of a reference defect. The differential harness treats registered divergences as expected and every other divergence as a regression.
_Avoid_: ignore list, allowlist (those imply silent suppression; entries here are justified and linked upstream)

**Plugin**:
A host-agnostic fact producer / RBS synthesizer for a target library or DSL (e.g. Rails routes, ActiveRecord) — it returns facts, synthetic RBS, and diagnostics that the inference engine consumes; it is not itself part of the inference engine. A plugin is hosted either as a real Ruby plugin in the Ruby sidecar (default) or as a native Rust port.
_Avoid_: extension, addon (use "plugin")

**Certainty**:
The trinary result of a type relation — `yes`, `no`, or `maybe` — paired with evidence. `maybe` never refines as `yes`, never manufactures the complementary false-edge fact, and never promotes by repetition; it is also distinct from a budget / incomplete-inference cutoff, which names itself.
_Avoid_: confidence, probability (it is trinary, not numeric)

**Subtyping**:
The `<:` relation — value-set inclusion, reflexive and transitive, checked against a type's static facet. Drives method availability, member access, and refinement.
_Avoid_: assignability (reserve that for the gradual-consistency direction)

**Gradual consistency**:
The symmetric, non-transitive relation that is the only way a `Dynamic[T]` value may cross a typed boundary. Distinct from subtyping; `untyped` is not `top`.
_Avoid_: compatibility, assignable (name it; it is not `<:`)

**Normalization**:
The deterministic canonical form of a type. Equivalent inputs must produce identical output; because diagnostics render normalized types, it is a bit-for-bit parity surface (e.g. `1 | Integer` does not collapse; `true | false` reads as `bool` for display only).
_Avoid_: simplification, canonicalization (use "normalization")

**Shape type**:
A structurally-typed container carrier that records its element/entry types and (for a tuple) its exact length, so a read returns a precise member type instead of a bare `Array`/`Hash` nominal. Built STATICALLY from literals and constructors (no Ruby execution) — the reference's differentiated container precision. Two forms: **Tuple** and **HashShape**.
_Avoid_: literal type, refined type (name the structural nature)

**Tuple**:
The shape type for a fixed-length array: an ordered vector of element types (`[1, "a"]` → `Tuple[Integer, String]`). Minted from array literals and `Array.new(n)` with a small constant `n` (the reference caps this at `ARRAY_NEW_TUPLE_LIMIT = 16`; oversize or non-constant sizes stay `Nominal[Array]`). A `Tuple#[]` with a static index/range returns the member type / a sub-`Tuple` / `Constant[nil]` for a statically-out-of-range slice — so `tuple[Range]` is NON-nil, unlike `Nominal[Array]#[](Range) : Array?`.
_Avoid_: array literal type (a literal is one *source*; a `.map` result is also a Tuple)

**HashShape**:
The shape type for a hash with statically-known keys: a map from constant keys to value types, so `h[:k]` / `h.dig(:k)` / `h.fetch(:k)` return the precise value type. The hash counterpart of Tuple.
_Avoid_: hash literal type (same reason as Tuple)

**Shape-typing tier**:
The reference's dispatch tier that mints and consumes shape types — literal/constructor → shape, and shape-preserving methods (`map`/`select`/`flatten`/slice/`dig`/`fetch`) → shape. It sits between constant folding and RBS-nominal dispatch. rigor-rs ports it STATICALLY (no sidecar) when a slice pays; only the `Array.new`-provenance possible-nil fire (Slice 1a) is ported so far — the full `Type::Tuple` was measurement-deferred (thin EV) ([ADR-0039](docs/adr/0039-shape-typing-tier.md)).
_Avoid_: shape dispatch (that is one half — consuming; the tier also mints)

**Fact bucket**:
A named partition of a scope snapshot (local-binding, captured-local, object-content, global-storage, dynamic-origin, relational) with bucket-specific invalidation — e.g. an unknown call sweeps object-content but leaves local-binding intact.
_Avoid_: fact store (that is the cross-plugin channel — a different thing)

**Flow-effect bundle**:
The data contract a plugin or RBS annotation returns to the inference engine: a normal return plus truthy/falsey-edge facts, post-return assertions, exceptional/escape/mutation/invalidation effects, dynamic-reflection members, and provenance + certainty. Merged deterministically by the analyzer (core/RBS authoritative; plugins refine, never weaken).
_Avoid_: contribution, hook result (name it)
