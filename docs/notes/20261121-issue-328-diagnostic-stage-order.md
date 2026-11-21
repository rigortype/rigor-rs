# Per-file diagnostics in the reference's stage order — #328 (2026-11-21)

Issue #328: the port appended rule passes in its own order, so rows like
`call.unresolved-toplevel@1:1` landed after `call.undefined-method@2:3`
and every `probe.py` comparison reported an order-only `≠`. PR #790 ports
the reference's emission contract (`CheckRules.diagnose`): the converged
walk's per-node rows first (queued keyed by OWNING-node span — pre-order
visit, parents first at equal starts via the larger span end — plus the
rule's slot in the reference's per-node emission array), then
`self_undefined_method`, then each collector in
`COLLECTOR_DIAGNOSTIC_BUILDERS` order, then suppression markers. The
caller-side stages (`static.value-use.void`,
`flow.shadowed-rescue-clause`) slot in via `emission_stage(rule_id)` in
`check`, LSP and MCP.

## Measured outcome

- The brief's rows all `=`: the two-line row; an 8-diagnostic interleaved
  file (stage order beats position twice); the same-(line,col) tie
  (`unreachable-branch` + `dead-assignment` at one spot, the main-pass row
  leading a positionally-earlier collector row); #435 m3 / #437 o7; text
  format byte-identical (single- and multi-file); `baseline generate`
  bins in the reference's order.
- `run_snapshot.rb`/`snapshot.rb --check` are order-insensitive (sets /
  sort before comparing), so the fixture gate cannot see ordering — CI
  stayed green for the wrong reason before this; `probe.py` snippet mode
  (tuples in stream order) is the ordering gate.
- `fp_audit --gaps --sweep`: **0 FP / 437 gaps, identical to master**.
- Adversarial review on head `a7e23df9`: ~25 counterexample shapes
  (nested-def visit order, stage inversions, equal-start chains, value
  edges, project `sig/`, suppression tails, parse errors, multi-file
  text and baseline) all `=`; Approved. Fable companion quota-blocked
  until Nov 25, no Codex on this host.
- Found while probing (pre-existing, master-verified, filed): **#791**
  shadowed-rescue FP — a bare rescue name certifies via a collapsed RBS
  short key (`A` ← `Resolv::DNS::Resource::IN::A`); **#792**
  project-`sig/` arity invisible to `method_arity`; a nilable-String
  `possible-nil-receiver` row on #783; `def.method-visibility-mismatch`
  unported.

## Lesson

Ordering is a parity axis the set-based gates cannot see: snapshot and
sweep pass vacuously on order, and `≠` noise in probes hid real rows
behind order diffs. With stream order now faithful, every future
`probe.py` row checks it for free.
