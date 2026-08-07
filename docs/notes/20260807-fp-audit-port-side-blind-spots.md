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

## Fix shape (not built here)

1. `run_rs` returns `Option`-style `None` on non-zero exit or unparseable
   output; `audit()` reports `SKIPPED … comparison invalid` and exits
   non-zero, mirroring `run_ref`'s existing contract.
2. Make the binary unambiguous: auto-build like `run_corpus.rb`, or resolve
   debug/release by mtime and REFUSE to run when the chosen binary predates
   the working tree's last source change. Whichever is chosen, the two
   harness entry points must agree.
3. Print the resolved binary path + its mtime in the run header, so a stale
   measurement is visible in the transcript rather than inferred six days
   later.
