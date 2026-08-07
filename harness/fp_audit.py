#!/usr/bin/env python3
"""Real-corpus false-positive audit: rigor-rs vs the reference oracle.

The differential harness (`run.rb`) gates parity on a small hand-built corpus.
This complements it by running BOTH implementations over a REAL project's files
and reporting, per (rule, path, line, column):

  * FP candidates  — emitted by rigor-rs but NOT the reference (a violation of
                     rigor-rs's zero-false-positive bar; the actionable output)
  * coverage gaps  — emitted by the reference but NOT rigor-rs (expected; the
                     sound-subset-of-reference contract, ADR-0002)

Both run core+stdlib only for a fair comparison: the reference from a clean cwd
(so it auto-loads no project config / bundle), rigor-rs from the repo (which
ships no `sig/` or `rbs_collection`). Parity severities only (error/warning).

Usage:  python3 harness/fp_audit.py [--gaps] [--sweep] [<dir-of-.rb> ...]
        --gaps  also aggregates coverage gaps (reference-only) by rule — the map
                of where to spend coverage effort.
        --sweep runs the STANDING sweep set (`harness/sweep-corpora.yml`) instead
                of a hand-typed directory list. Extra directories may still be
                passed; they run after the standing set. A listed corpus that is
                not present on this machine is reported as SKIPPED, never
                silently dropped.
Env:    RIGOR_RS_BIN (default target/release/rigor — auto-built when absent, and
        REFUSED when older than the newest file under crates/),
        REFERENCE_RIGOR_DIR (default reference/rigor).

Exit:   0 only when every requested corpus produced a VALID comparison with zero
        FP candidates. Any FP, any batch where either side failed, and any
        refusal to run exit non-zero — a partial run must never read as a pass.
"""
import glob
import json
import os
import subprocess
import sys
import tempfile
import time
from collections import Counter

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# RELEASE, deliberately — and the SAME default as `harness/run_corpus.rb`, which
# used to say debug. The sweep is 9204 files across both implementations; the
# release binary is what every recorded sweep number was measured with, and a
# debug binary is several times slower over that set. The corpus-scale tools
# (this file, `gap_census.py`, `run_corpus.rb`) therefore all mean
# `target/release/rigor` by "rigor-rs". The fixture harness (`harness/run.rb` /
# `lib.rb`, 76 tiny files in an edit-run loop) deliberately stays on debug.
# Divergence between the two corpus entry points is what made a six-day-old
# binary measurable in the first place — see
# docs/notes/20260807-fp-audit-port-side-blind-spots.md.
RS_DEFAULT = os.path.join(REPO, "target/release/rigor")
RS = os.environ.get("RIGOR_RS_BIN", RS_DEFAULT)
RS_OVERRIDDEN = "RIGOR_RS_BIN" in os.environ
CARGO_BUILD = ["cargo", "build", "--offline", "--release", "-p", "rigor-cli"]
REF_DIR = os.environ.get("REFERENCE_RIGOR_DIR", os.path.join(REPO, "reference/rigor"))
REF_LIB = os.path.join(REF_DIR, "lib")
REF_EXE = os.path.join(REF_DIR, "exe", "rigor")
PARITY = {"error", "warning"}

# Set by run_rs when it returns None, so the caller can say WHY the comparison
# is invalid instead of just that it is.
LAST_RS_FAILURE = None
_RESOLVED_RS = None


def die(msg):
    """Refuse to measure. Non-zero exit, loud, on stderr."""
    sys.stdout.flush()  # keep the header above the error when stdout is piped
    print(f"\nERROR: {msg}", file=sys.stderr)
    sys.exit(2)


def _stamp(mtime):
    return time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(mtime))


def newest_source():
    """(mtime, path) of the newest file under crates/ — the port's source truth.

    Everything under crates/ counts, not just *.rs: the vendored RBS the index
    loads is an input to the diagnostics exactly as the Rust is.
    """
    newest = (0.0, None)
    for root, dirs, names in os.walk(os.path.join(REPO, "crates")):
        dirs[:] = [d for d in dirs if d not in ("target", ".git")]
        for n in names:
            p = os.path.join(root, n)
            try:
                m = os.path.getmtime(p)
            except OSError:
                continue
            if m > newest[0]:
                newest = (m, p)
    return newest


def resolve_rs():
    """Resolve, report and VALIDATE the rigor-rs binary. Memoized.

    Three things this must never do silently, all of them observed:
      * measure a binary that predates the source (a six-day-old release build
        made two merged slices read as "closed nothing"),
      * disagree with the other corpus entry point about which file is rigor-rs,
      * leave the measured path out of the transcript, so the staleness can only
        be reconstructed days later.
    So: auto-build when absent (as run_corpus.rb does), print path + mtime in the
    run header, and REFUSE when older than the newest file under crates/.

    An explicit RIGOR_RS_BIN pointing outside the repo's target/ is honoured as
    given — it is a deliberate choice (bisecting, an installed build) and its
    mtime carries no relation to this tree — but it is still printed, and it
    still has to survive run_rs's exit-code contract.
    """
    global _RESOLVED_RS
    if _RESOLVED_RS:
        return _RESOLVED_RS
    path = RS
    in_target = os.path.abspath(path).startswith(
        os.path.join(REPO, "target") + os.sep)
    if not os.path.exists(path):
        if RS_OVERRIDDEN:
            die(f"RIGOR_RS_BIN={path} does not exist. Nothing was measured.")
        print(f"rigor-rs binary absent at {path}; building: "
              f"{' '.join(CARGO_BUILD)}", flush=True)
        if subprocess.run(CARGO_BUILD, cwd=REPO).returncode != 0:
            die(f"`{' '.join(CARGO_BUILD)}` failed. Nothing was measured.")
        if not os.path.exists(path):
            die(f"binary still missing after build: {path}")
    if not os.access(path, os.X_OK):
        die(f"rigor-rs binary is not executable: {path}")
    mtime = os.path.getmtime(path)
    print(f"rigor-rs binary: {path}")
    print(f"  built:                 {_stamp(mtime)}")
    if in_target:
        src_mtime, src_path = newest_source()
        rel = os.path.relpath(src_path, REPO) if src_path else "(none)"
        print(f"  newest crates/ source: {_stamp(src_mtime)}  ({rel})")
        if src_mtime > mtime:
            die(f"STALE BINARY — {path} was built {_stamp(mtime)} but "
                f"{rel} changed {_stamp(src_mtime)}.\n"
                f"       The measurement would describe a binary that is not "
                f"this working tree. Rebuild first:\n"
                f"         {' '.join(CARGO_BUILD)}")
    else:
        print("  staleness:             NOT CHECKED (RIGOR_RS_BIN points "
              "outside this repo's target/)")
    _RESOLVED_RS = path
    return path


def rb_files(d):
    # Absolute paths: run_ref invokes the reference from a temp cwd, so
    # relative paths would resolve against the wrong directory.
    return sorted(os.path.abspath(f)
                  for f in glob.glob(os.path.join(d, "**", "*.rb"),
                                     recursive=True))


def run_rs(files):
    # Returns None (NOT []) whenever the port did not produce a trustworthy
    # answer — the same contract run_ref has carried all along, and for the same
    # reason, inverted: FP candidates are `rs - ref`, so an empty port result
    # makes the FP count 0 BY CONSTRUCTION. A panicking, crashing or
    # JSON-malformed rigor-rs used to PASS this project's central gate, with
    # inflated coverage gaps as the only symptom — and gaps are expected noise
    # by design (ADR-0002), so nobody reads them as failure.
    #
    # `rigor check` exit codes: 0 = no diagnostics, 1 = diagnostics emitted.
    # Anything else (64 usage, 101 panic, 127 not-found …) is a failure, and so
    # is stdout that is not a JSON array, or an exit code that contradicts the
    # array's emptiness (that last one is what catches a binary which exits 1
    # while printing nothing at all).
    global LAST_RS_FAILURE
    rs = resolve_rs()
    r = subprocess.run([rs, "check", "--format", "json"] + files,
                       capture_output=True, text=True)
    if r.returncode not in (0, 1):
        LAST_RS_FAILURE = (f"exit {r.returncode}: "
                           f"{(r.stderr or r.stdout).strip()[:200]!r}")
        return None
    try:
        out = json.loads(r.stdout)
    except Exception as e:
        LAST_RS_FAILURE = (f"exit {r.returncode}, unparseable stdout "
                           f"({e}): {r.stdout.strip()[:200]!r}")
        return None
    if not isinstance(out, list):
        LAST_RS_FAILURE = f"stdout parsed to {type(out).__name__}, expected a JSON array"
        return None
    if (r.returncode == 0) != (not out):
        LAST_RS_FAILURE = (f"exit {r.returncode} contradicts {len(out)} "
                           f"diagnostics on stdout")
        return None
    LAST_RS_FAILURE = None
    return out


def run_ref(files):
    # Clean cwd: no project .rigor.yml / Gemfile / sig auto-load, so the
    # reference analyses on core+stdlib only — comparable to rigor-rs's default.
    # Returns None (NOT []) when the reference produced no parseable JSON — a
    # batch failure (one poison file aborting the whole run) would otherwise look
    # like "reference found nothing", turning all of rigor-rs's output into false
    # FP candidates. A None result means the comparison is invalid, not FP-free.
    # The bundled rigor-rbs-inline lib is pinned onto the load path (upstream
    # issue #194): the ADR-93 auto-wire otherwise resolves a stale installed
    # rigortype gem's pre-gate plugin copy and poisons the comparison.
    # Fresh per-invocation temp cwd + --no-cache (UPSTREAM.md hazard 2): the
    # reference's persistent result cache is keyed by cwd and NOT scoped to the
    # reference version, so a shared cwd (the old cwd="/tmp") could serve stale
    # results across invocations — surviving even a submodule pin bump.
    ref_plugin = os.path.join(REF_DIR, "plugins", "rigor-rbs-inline", "lib")
    with tempfile.TemporaryDirectory(prefix="rigor-fp-audit-ref") as tmpcwd:
        r = subprocess.run(["ruby", "-I", REF_LIB, "-I", ref_plugin,
                            REF_EXE, "check", "--format", "json", "--no-cache"]
                           + files,
                           capture_output=True, text=True, cwd=tmpcwd)
    i = r.stdout.find("{")
    if i < 0:
        return None
    try:
        obj = json.loads(r.stdout[i:])
    except Exception:
        return None
    return obj.get("diagnostics", []) if "diagnostics" in obj else None


def keys(diags):
    return {
        (os.path.abspath(d.get("path", "")), d.get("line"), d.get("column"), d.get("rule"))
        for d in diags
        if d.get("severity", "error") in PARITY
    }


def audit(tgt, show=12, show_gaps=False, gap_rules=None):
    """FP-candidate count for one corpus, or None when the comparison is INVALID.

    None is not zero. Every invalid corpus makes the whole run exit non-zero:
    a batch either side failed on has no FP count, and reporting it as 0 is how
    a broken run reads as a green gate.
    """
    files = rb_files(tgt)
    if not files:
        print(f"{tgt}: no .rb files")
        return 0
    t = time.time()
    ref_diags = run_ref(files)
    if ref_diags is None:
        print(f"\n=== {tgt} ({len(files)} files) ===")
        print("  INVALID: reference produced no parseable output on this batch "
              "(likely one file aborts its run) — comparison invalid, not FP-free.")
        return None
    rs_diags = run_rs(files)
    if rs_diags is None:
        print(f"\n=== {tgt} ({len(files)} files) ===")
        print(f"  INVALID: rigor-rs failed on this batch [{LAST_RS_FAILURE}] "
              "— comparison invalid, not FP-free.")
        return None
    rs, ref = keys(rs_diags), keys(ref_diags)
    fp, gap = rs - ref, ref - rs
    print(f"\n=== {tgt} ({len(files)} files, {time.time() - t:.1f}s) ===")
    print(f"  reference={len(ref)}  rigor-rs={len(rs)}  matched={len(rs & ref)}")
    print(f"  FP candidates (rigor-rs only): {len(fp)}")
    print(f"  coverage gaps (reference only): {len(gap)}")
    if fp:
        print("  FP by rule:", dict(Counter(k[3] for k in fp).most_common()))
        for k in sorted(fp)[:show]:
            print(f"    FP: {k[3]} @ {os.path.basename(k[0])}:{k[1]}:{k[2]}")
    if gap_rules is not None:
        for k in gap:
            gap_rules[k[3]] += 1
    if show_gaps and gap:
        print("  gaps by rule:", dict(Counter(k[3] for k in gap).most_common()))
    return len(fp)


SWEEP_MANIFEST = os.environ.get(
    "SWEEP_CORPORA", os.path.join(REPO, "harness", "sweep-corpora.yml")
)


def sweep_targets():
    """The standing sweep set's present directories, plus the absent ones.

    Membership lives in `harness/sweep-corpora.yml` so the sweep is a
    reproducible set rather than whatever directories the last session happened
    to type. Absent corpora are RETURNED, not dropped — the caller reports them,
    because a sweep that quietly measured half its set reads as a green gate.
    """
    import yaml  # local: only --sweep needs it, so a plain run has no dependency

    with open(SWEEP_MANIFEST, encoding="utf-8") as f:
        entries = yaml.safe_load(f).get("corpora", [])
    present, absent = [], []
    for e in entries:
        (present if os.path.isdir(e["path"]) else absent).append(e)
    return present, absent


if __name__ == "__main__":
    flags = {"--gaps", "--sweep"}
    args = [a for a in sys.argv[1:] if a not in flags]
    show_gaps = "--gaps" in sys.argv  # also report coverage-gap breakdown by rule
    absent = []
    if "--sweep" in sys.argv:
        present, absent = sweep_targets()
        print(f"Standing sweep set ({SWEEP_MANIFEST}): "
              f"{len(present)} present, {len(absent)} absent")
        args = [e["path"] for e in present] + args
    if not args:
        print(__doc__)
        sys.exit(2)
    resolve_rs()  # header first: the measured binary belongs in the transcript
    gap_rules = Counter() if show_gaps else None
    results = [(t, audit(t, show_gaps=show_gaps, gap_rules=gap_rules))
               for t in args]
    for e in absent:
        print(f"\n=== {e['label']} — SKIPPED: {e['path']} is not on this machine "
              f"(the sweep set is INCOMPLETE for this run) ===")
    invalid = [t for t, n in results if n is None]
    total = sum(n for _, n in results if n is not None)
    # When anything was invalid the total is NOT a result — say so on the same
    # line, because "TOTAL FP candidates: 0" is what gets grepped and quoted.
    suffix = ("" if not invalid else
              f" — INCOMPLETE ({len(results) - len(invalid)} of {len(results)} "
              f"corpora compared)")
    print(f"\nTOTAL FP candidates: {total}{suffix}")
    if show_gaps:
        print("TOTAL coverage gaps by rule (where to spend coverage effort):")
        for rule, n in gap_rules.most_common():
            print(f"  {n:6}  {rule}")
    if invalid:
        # The FP total above is not a result: it was measured over a subset. A
        # run that could not compare everything asked of it is a FAILED run.
        print(f"\nINVALID COMPARISONS: {len(invalid)} — this run did NOT measure "
              "an FP count for:")
        for t in invalid:
            print(f"  {t}")
        print("RESULT: FAIL (invalid comparison — NOT an FP-free result)")
        sys.exit(1)
    sys.exit(1 if total else 0)
