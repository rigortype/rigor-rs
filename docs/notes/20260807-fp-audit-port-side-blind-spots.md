# `fp_audit` measures the RELEASE binary, and a broken port binary passes the gate (2026-08-07)

Found while re-censusing after the PR #63/#64 merges: the post-merge sweep
reproduced the **pre-merge numbers exactly** (1193 gaps, `String#first ×13`
still present) while the freshly built binary demonstrably fired on both
merged shapes. Two independent port-side defects in `harness/fp_audit.py`
(inherited by `gap_census.py`, which imports it), both confirmed by probe.

The reference side of this same file is hardened against exactly these
failures — `run_ref` returns `None` (not `[]`) and prints `SKIPPED`, with a
14-line comment explaining why a batch failure must not read as "found
nothing". The port side never got the same treatment. That asymmetry is the
finding.

## Defect 1 — the sweep measures `target/release/rigor`, `cargo build` writes debug

`fp_audit.py:38`:

```python
RS = os.environ.get("RIGOR_RS_BIN", os.path.join(REPO, "target/release/rigor"))
```

The documented build command (`docs/CURRENT_WORK.md` "Build & gates") is
`cargo build --offline`, which writes `target/debug/rigor`. Nothing rebuilds
or checks the release binary, and nothing warns when it is older than HEAD.
The binary this session measured was dated **Aug 1** — six days and several
arcs stale — so the two merged slices' closures were invisible and read as
"the slices closed nothing."

`harness/run_corpus.rb` does NOT share the defect: it defaults to
`target/debug/rigor` and auto-builds when absent. So the two harness entry
points disagree about which binary *is* rigor-rs — the same class of drift
`sweep-corpora.yml` was introduced to stop for corpora.

## Defect 2 — a failing port binary yields a VACUOUS 0-FP pass

`fp_audit.py:53-59`:

```python
def run_rs(files):
    r = subprocess.run([RS, "check", "--format", "json"] + files, ...)
    try:
        return json.loads(r.stdout)
    except Exception:
        return []
```

The exit code is never inspected and every failure collapses to `[]`. Since
FP candidates are `rs - ref`, an empty port result makes the count **0 by
construction**:

```console
$ RIGOR_RS_BIN=/usr/bin/false python3 harness/fp_audit.py <dir-with-1-diagnostic>
  reference=1  rigor-rs=0  matched=0
  FP candidates (rigor-rs only): 0
  coverage gaps (reference only): 1

TOTAL FP candidates: 0
```

A panicking, crashing or JSON-malformed rigor-rs therefore **passes the
project's central gate**. The only symptom is inflated coverage gaps — and
gaps are expected noise by design (ADR-0002 sound-subset), so no reader treats
them as a failure signal. A missing binary does crash loudly
(`FileNotFoundError`), which is why this has never been noticed.

## Blast radius on the record

The PR #63/#64 sweep numbers are NOT affected: both agents reported gap totals
near the baseline (1182 / 1179) rather than the reference's full diagnostic
count, which an empty port result would have produced (dependabot-core alone
emits 138,870), and their specific closures were re-verified independently —
the S5 fixture was run against the live oracle by the reviewer, and both
merged shapes fire in a freshly built binary. What IS void is the post-merge
census run in this session; it must be redone against a current release build.

## What was built (2026-08-07)

Both defects are closed in `harness/fp_audit.py`, `harness/gap_census.py` and
`harness/run_corpus.rb`. The contract is documented in
[harness/CORPUS.md](../../harness/CORPUS.md).

**1 — a failed run is INVALID, never 0.** `run_rs` now returns `None` on
anything that is not a trustworthy answer, exactly as `run_ref` already did: an
exit code outside `{0, 1}`, or stdout that is not a JSON array (that second one
is what catches a binary which exits 1 while printing nothing, i.e. the
`/usr/bin/false` probe). Note what is NOT checked, after a false-fire in review:
`rigor check` exits 1 iff at least one **error**-severity diagnostic was
emitted, so a warning-only batch exits 0 *with* diagnostics — comparing the exit
code against the list's emptiness marks healthy warning-only corpora INVALID,
which is the mirror image of the defect being fixed here. `audit()`
prints `INVALID: rigor-rs failed on this batch [reason] — comparison invalid,
not FP-free` and returns `None`, the total line is marked `INCOMPLETE (n of m
corpora compared)`, and the run exits 1. A reference-side failure now counts the
same way (it printed `SKIPPED` but still returned 0, so an all-skipped sweep
still exited 0). `gap_census.py` inherits all of it and reports the same
`INVALID` for either side. The same shape existed in `run_corpus.rb`'s
`run_rigorrs_batch` (`return {}` on every failure → 0 FPs → "STRONG RESULT");
it now aborts loudly.

**2 — the binary is one binary, and it dates itself.** All three corpus-scale
tools default to `target/release/rigor` (reasoning: release is what every
recorded sweep number was measured with, and the sweep is 9204 files through
both implementations; the fixture harness `run.rb`/`lib.rb` stays on debug,
which is right for its edit-run loop). The binary is auto-built when absent
(`cargo build --offline --release -p rigor-cli`), its path and build time are
printed in the run header, and a binary under `target/` older than the newest
file under `crates/` is REFUSED with exit 2 — nothing is measured. An explicit
`RIGOR_RS_BIN` outside the repo is honoured as deliberate but must exist, and
the header says its staleness was not checked.

## It caught a real invalid run within the hour

First use after merging: re-verifying PR #68's sweep in a scratch worktree, the
hardened `gap_census` reported `INVALID (unparseable reference output)` for all
eight corpora and exited 1 — the worktree's `reference/rigor` submodule had
never been initialised, so the ORACLE was missing. Under the old harness that
run would have printed a green `TOTAL FP candidates: 0`: no reference
diagnostics means no gaps and no FPs, which is indistinguishable from perfect
parity. The failure was invisible by construction and is now impossible to
miss.

Not covered: a binary built from *committed* sources that differ from HEAD in
some way mtimes cannot see (e.g. a `git checkout` that rewinds source files to
older mtimes than the binary). The mtime gate catches the observed failure —
edit, forget to rebuild, measure — not every possible divergence.
