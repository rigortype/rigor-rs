# The reference is the oracle except for triaged defects

Status: accepted

[ADR-0002](0002-diagnostic-set-parity.md) makes the reference implementation the oracle for diagnostic-set parity. This ADR qualifies that: rigor-rs targets parity with the reference's **intended** behaviour, not bug-for-bug fidelity. Unreasonable implementations, bugs, and unintended quality regressions discovered in the reference during the port are **not reproduced** — they are reported upstream as improvement opportunities, and the two implementations converge on the corrected behaviour. The final spec should match as closely as possible, and fixing the Ruby side is an expected outcome, not an exception.

## Discipline (to keep this from becoming a parity loophole)

The reference stays the oracle **by default**: any divergence is assumed to be a rigor-rs bug and fixed in Rust to match — unless it is explicitly legitimized.

- **Divergence registry.** A divergence is excused from parity only by an entry in a tracked registry recording: the location + rule, the reference's output, rigor-rs's corrected output, why it is a reference defect, and a link to the upstream issue/PR on the **Ruby Rigor** repo (`rigortype/rigor` — a *different* tracker from rigor-rs's own, [ADR-0009](0009-config-baseline-compatibility.md)).
- **Upstream report required + review.** Classifying a divergence as a reference defect requires the upstream report to exist and a reviewer to sign off. rigor-rs MAY implement the corrected behaviour ahead of the upstream fix, but only with the registry entry in place.
- **Harness semantics.** The differential harness ([ADR-0002](0002-diagnostic-set-parity.md)) treats registered divergences as expected (green) and **every unregistered divergence as a real rigor-rs regression (red)**. An unexplained red is always a regression to fix, never silently waved through — this is what keeps the oracle trustworthy.
- **Convergence closes the entry.** When the upstream fix lands, the pinned reference is bumped (ADR-0002's snapshot refresh) and the entry is removed — the implementations agree again. The registry is a ledger of *in-flight* upstream fixes, expected to trend toward empty.

## Considered options

- **Loose marking** (implementer notes "reference defect", reports later) — rejected: the loophole risk (any inconvenient divergence declared a defect) erodes the oracle.
- **Upstream-first** (fix Ruby before rigor-rs ever diverges; rigor-rs always matches the current pinned reference) — rejected as the *default* mode: cleanest harness, but couples rigor-rs's progress to landing upstream fixes. The registry lets the port proceed while the fix is in flight; upstream-first remains the natural path when a defect is trivial to fix on the Ruby side.
- **Bug-for-bug fidelity** (reproduce reference defects faithfully) — rejected: contradicts the project's intent and ships known-wrong behaviour.
