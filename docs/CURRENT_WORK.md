# rigor-rs — Current Work

The baton: **what is in flight and what to pull next**. This file is NOT a
running log — landed arcs, per-issue outcomes and follow-ups live in
GitHub issues/PRs, `docs/notes/` and `docs/adr/`; history: `git log`.
Keep only non-obvious in-flight matters that cross issue boundaries;
prune entries that are done or already captured elsewhere.
Port map: [PORT_BACKLOG.md](PORT_BACKLOG.md). Hard byte budget
(`harness/docs_check.py`).


## Now / Next

▶ NEXT: ADR-127 floor coverage cost; follow-ups #493/#600/#605,
paren/`&.` residuals #838–#847; mutation seams →#777–#785;
#877 residuals #894–#896; #898-review residuals #899–#903.
- **Pin `v0.4.8`**. Both exception tables EMPTY — new entries are real findings (`UPSTREAM.md`; overlay/`sig/shims` trap).

State (2026-11-22, `v0.4.8`): **174 fixtures / 0 unregistered**; sweep
**0 FP / 437**; effects gate 0 OVER. Clippy `-D warnings`, fresh
`CARGO_TARGET_DIR`.

## Standing conclusions (do not re-litigate without new evidence)
- **Possible-nil / Tier B/C CLOSED, not deferred** — the closing slice deletes
  the concrete-arm FP-safety mechanism; `fp_audit` 0 FP
  ([tier-bc](notes/20260717-tier-bc-track-closed.md)). SCOPE: 85 `Dynamic`-arm
  rows, not the 93 concrete-arm.
- **Reference-FP undefined-method clusters, CLOSED** — `pre_eval:` cross-file
  monkey-patch (inverts the ADR-0033 provenance gate), `nil` receiver,
  rdoc `Hash` ([141](notes/20260807-gap-adjudication-141.md)).
  `v0.3.8` re-adjudication: **395/799 decided** ([799](notes/20260909-gap-adjudication-799.md)).
- **5 FP-safe flow slices closed 0 gaps** — none without an `fp_audit
  --gaps` prediction ([flow-frontier](notes/20260706-flow-frontier-exhausted.md)).
- **Receiver-typing lever open**: gap census buckets by mechanism
  ([census](notes/20260807-gap-census.md)); 4 slices closed **47 rows**.
- **Effects transitive-label lane DECLINED — not parity-portable** (s4): best 5 OVER; parity outranks. [probe](notes/20260826-effects-s4-probe.md).
- **sig-gen closed** — byte surface 0, `--write` sound.
- **Plugin work:** pure-RBS closed [note](notes/20260710-pure-rbs-bundle-track-closed.md);
  sidecar+perf retired [ADR-0037](adr/0037-sidecar-perf-slices-retired-by-measurement.md).

## Build & gates

Tiers + commands: `AGENTS.md` → Gates.

Oracle: `reference/rigor` pin (`UPSTREAM.md`, fresh cwd); sweep
`sweep-corpora.yml`; RBS vendored (ADR-0007; `RIGOR_RBS_CORE_DIR`).
