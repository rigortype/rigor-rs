# ADR-0042 gate deliverables (2026-07-19)

Both pre-implementation gate steps of
[ADR-0042](../adr/0042-qualified-key-index-registration.md) executed
(subagent-parallel: an oracle behavior matrix + a full consumer inventory).
Verdict: **the migration is safety-POSITIVE, feasible, and has one real scope
gap the ADR must absorb before implementation.**

## Gate step 1 — nested-class oracle matrix (fixtures 68–70)

Twelve scenarios probed on both engines (hardened invocation, pin `7a69f142`).
Landed as three permanent fixtures pinning **9 documented coverage gaps** (the
migration's target surface) + the already-matching non-nested contrast:

- **68_nested_stdlib_singleton** — `ERB::Util` / `CGI::Util` (the ADR's
  short-key MERGE collision: method-disjoint in the vendored rbs, fully
  isolated by the oracle) + `Process::Status` + the non-nested `CGI` contrast
  (already byte-matching; the migration must not regress it).
- **69_nested_project_sig** — the decisive mirror-image case: the oracle
  witnesses `Outer::Inner.new.spni` through the QUALIFIED path (merging the
  two-file sig reopen) and keeps bare `Inner` silent. rigor-rs did the exact
  opposite — silent on qualified, FIRING through the bare short-key door
  (`spni' for Inner` where the oracle is silent = an oracle-FP shape). The
  bare door is CLOSED in this arc (below); the qualified gap is pinned.
- **70_nested_shadow_sig** — residual defect-2 unsoundness: a toplevel
  project-sig `Status`/`Instance` still has the nested-stdlib surface
  (`exited?`/`digest`) short-key-MERGED into it and silently accepted; the
  oracle isolates the namespaces (4 diags). Pinned as gaps the migration
  must close.

## FP fix landed with the gate (not deferred)

The s5 bare-door firing violated the registry discipline (rigor-rs, not the
reference, was the defect), so it is fixed here: the UM source-range witness
gate drops its `|| is_project_sig_class` arm — a TOPLEVEL sig class is in the
`knows_toplevel_class` set via its authoritative registration, so that arm
only ever ADDED nested-only sig classes reached by a bare name the reference
never resolves. Fixtures 37/38 (toplevel Widget) unaffected; all gates green.

## Gate step 2 — consumer inventory (condensed; full report in the PR)

~46 SAFE / 6 GUARD / 5 NEEDS-ALIAS / 1 TEST-PIN across all four crates.
Decision-relevant findings:

1. **No consumer becomes UNSOUND under alias-collapse-to-nothing** for an
   ambiguous short name — every NEEDS-ALIAS site degrades to a missed
   witness, never a new FP. The ADR's preferred option (a) stands.
2. **Two NEW latent-FP sites found** (unguarded short-key exposure, absent
   from fp_audit only by corpus coincidence): the ATM instance branch
   (`rigor-rules/lib.rs` ~1740) and the void-use instance branch (~3019)
   recover a `SourceIndex` name and consult core surfaces with NO
   defect-2 guard. The migration fixes both automatically — it is
   safety-positive, not merely a coverage win.
3. **Real scope gap**: qualifying the DECLARATION key alone does not fix
   superclass / include / extend / return-type / param-type REFERENCES,
   which are extracted short-only (`type_name_str`). A nested class whose
   relative reference stops resolving degrades to chain-incomplete ⇒ silent
   (FP-safe by the existing conservative-completeness contract) but is a
   coverage-regression risk the ADR must gate on explicitly (resolution
   pass vs alias fallback = a design sub-decision).
4. `shadowed_rescue`'s lexical candidate ladder already BUILDS the correct
   qualified names — dead code today (nothing can match), activated by the
   migration for free. `SourceIndex`'s own short-keyed registry is a
   parallel surface the ADR deliberately does not cover.
5. One TEST-PIN (`knows_toplevel_class_distinguishes_namespaced`) and three
   doc blocks narrate short-key semantics and must move with the migration.

## Status

ADR-0042 gate: **SATISFIED** (regression surface pinned in fixtures 68–70;
inventory recorded). Implementation remains a separate, explicitly-approved
arc; the ADR gains the reference-resolution scope item before any code.
