# #439: the reach-memo audit is now a CI gate — and the gate can now fail

PR #753, merged 2026-11-21 (`a12cf602`). The `RIGOR_REACH_MEMO_AUDIT`
recompute-and-compare is the only gate for the reach memo's
`seen`-membership and re-entry validity rules — output diffs cannot see
them, and nothing in CI ran it.

## What landed

- **CI**: a second `run_snapshot.rb` pass under `RIGOR_REACH_MEMO_AUDIT=1`
  in the parity job. ~52s on CI over all 174 fixtures (per-fixture process
  spawn dominates; the audit itself is ~2s over the base run).
- **Item 4 fix**: the audit's recompute stored a duplicate entry at the
  front of the per-key LRU — `local_reach_framed` gained a `store` flag,
  audit passes `false`. `frames.record` still runs identically.
- **Harness fix (the review's catch)**: `capture3` discarded the child's
  exit status, and the CLI reports a panicked analysis as an
  info-severity `internal-error` diagnostic that `PARITY_SEVERITIES`
  filters — so the audit step could **never fail**: a panicking fixture
  just lost diagnostics, which is coverage, which never fails.
  `run_rigor_rs` now raises `HarnessError` on `panicked at` on stderr, an
  `internal-error` diagnostic at any severity, or a non-zero exit with no
  output (a plain exit-1 is normal — `check` exits 1 on any error-severity
  finding). Both `run.rb` and `run_snapshot.rb` count them and exit 1.
  Verified: `deps_match→true` + the membership shape appended to a
  fixture → `RESULT: FAIL`.
- **Tests** (`crates/rigor-infer/src/reach_memo_tests.rs`, 4): membership
  shape and g01 shape run under a forced audit (tri-state `AtomicU8` +
  **thread-local** `Cell<Option<bool>>` test override — the process-global
  first cut leaked and doubled `block_chain_types_once_per_driver_visit`'s
  type-event count, 3059 events for 10 links); a deterministic re-entry
  pin (below); an LRU no-duplicate pin.

## The re-entry rule's bite surface shrank since #433

The #433 review proved the rule load-bearing with g01
(`local_reach(x,(58,59),seen=["q@73","x@58"]) memo multi:false !=
recompute multi:true`). On master that panic does not reproduce —
post-#437 lifecycle (provisional eviction of scan-time truthys, `landed`
clearing, dead-set reset on dead change) removed the divergent replay
surface: post-scan reaches reset the memo, and mid-scan replays keep the
dep pred in-flight so stored and recomputed agree. Verified on a master
binary and ~20k fuzzed g01-family shapes under a four-site folding
break — zero divergent hits. (One trap while fuzzing: replay stacks must
be *distinct* keys — a duplicated `seen` entry panicked under intact
rules, a driver artifact no real traversal produces.)

So the regression test asserts the rule's own semantics instead of a
divergent panic: drive a shape that stores entries with `folding` deps,
land the dep's pred on `Some`, then query under a stack rebuilt from the
entry's recorded membership answers (`present` verbatim + filler keys no
interner saw). The hit must drop the entry; with the check removed it
survives and `folding_entry_counts` never drops. Deterministic — it
doesn't depend on the stale answer diverging.

Caveat recorded in the PR: the fixture corpus exercises NEITHER rule —
0 of 174 fixtures panic even with `deps_match` disabled. The CI step is
a broad shallow net; the unit tests are the real pins.

## Numbers

`gate.sh` pass; `fp_audit --gaps --sweep` 0 FP; clippy 1.88 clean;
audit run coverage 980/1084 = master. Opus review: one `Needs fix`
round (the swallowed-panic finding), fix-confirmation Approved.
→ #754 (`internal-error` also covers depth refusals — a deep-nesting
fixture would fail as "invalid run").
