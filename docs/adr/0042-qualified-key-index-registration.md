# Qualified-key index registration (the defect-2 root fix)

Status: proposed — filed 2026-07-18 while the evidence is fresh; NOT scheduled.
Implementation gated on a dedicated nested-class regression surface being built
first (see "Gate" below).

## Context — the short-key wall, measured twice

The index (`rigor-index`) registers every RBS declaration by its LAST path
segment (short key). `module ERB; module Util` registers `"Util"`;
`class Process::Status` registers `"Status"`. Two measured consequences:

1. **defect-2 (patched around, not fixed)**: a project model named `Status`
   resolved to the namespaced stdlib class and inherited its (lacking)
   class-method surface ⇒ FP. The mitigation is `knows_toplevel_class` — a
   *bypass gate* consulted at each new consumer, not a fix. The M2-GO 4b arc
   added another such consultation (the UM source-range witness gate), after
   `fp_audit` caught a `knows_class`-wide gate FP'ing on gitlab's
   `Clusters::Instance` model (bare name vs an RBS short key).
2. **Short-key MERGE collision (new, 2026-07-18)**: `module Util` is declared
   in BOTH `erb.rbs` and `cgi.rbs`; both fold into ONE `"Util"` `ClassEntry`
   by the reopen-union merge. The entry is neither `ERB::Util` nor
   `CGI::Util` — it is an unsound composite. GO-slice 5 (witnessing
   `ERB::Util.html_escape_once`, ~5 gitlab sites) cannot be built soundly on
   it, even though the PARSE side already lowers `A::B` receivers to a
   full-path `ConstantRead("ERB::Util")`.

The reference has neither problem: RBS::Environment resolves fully-qualified
names natively. Under the standing direction — reproduce the reference's
declaration-driven inference — qualified keys are the structural end-state.

## Decision (proposed)

Register nested declarations under their QUALIFIED name (`"ERB::Util"`),
threading the enclosing prefix through `ingest_class`/`ingest_module`/
`collect_members`. Keep genuine top-level declarations exactly as today (same
keys, same reopen-union merge — `class Time` core + plugin reopens are
unaffected). For bare-name (short-key) lookups, decide ONE of:

- (a) an explicit alias table `short -> [qualified…]` consulted only by the
  consumers that today rely on short-key hits (conservative: an ambiguous
  short name — 2+ qualified entries — resolves to NOTHING, which is exactly
  the FP-safe direction the `Util` collision demands); or
- (b) dual registration with a tombstone on collision.

(a) is preferred: collisions become explicit and self-silencing.

## Why not now (the ROI framing)

Direct payoff is ~5 gitlab sites (the `singleton(<class>)` cluster). The real
justification is **stopping the bypass-gate accretion**: every future consumer
of `knows_class` must today remember the defect-2 trap (two have been caught
by measurement already; the ones not yet caught are latent FPs in possible-nil
/ ATM / ancestor walks). That is a correctness-debt argument, not a coverage
argument — it does not outrank the measured Phase-3 work, hence "proposed, not
scheduled".

## Gate (before any implementation)

1. Build a nested-class regression surface FIRST: fixtures exercising
   `A::B.new`, `A::B.singleton`, bare-`B` project shadowing, cross-file
   reopens of nested classes — checked against the live reference.
2. Inventory every `knows_class` / `knows_toplevel_class` /
   `class_has_method` / `ancestors` call site and classify short-key
   assumptions BEFORE flipping registration.
3. The usual gates: 0 FP on the fixture set + fp_audit corpora; the sig-gen
   byte-parity surface must not move.
