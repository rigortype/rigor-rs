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

Usage:  python3 harness/fp_audit.py <dir-of-.rb> [<dir> ...]
Env:    RIGOR_RS_BIN (default target/release/rigor), REFERENCE_RIGOR_DIR
        (default reference/rigor).
"""
import glob
import json
import os
import subprocess
import sys
import time
from collections import Counter

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RS = os.environ.get("RIGOR_RS_BIN", os.path.join(REPO, "target/release/rigor"))
REF_DIR = os.environ.get("REFERENCE_RIGOR_DIR", os.path.join(REPO, "reference/rigor"))
REF_LIB = os.path.join(REF_DIR, "lib")
REF_EXE = os.path.join(REF_DIR, "exe", "rigor")
PARITY = {"error", "warning"}


def rb_files(d):
    return sorted(glob.glob(os.path.join(d, "**", "*.rb"), recursive=True))


def run_rs(files):
    r = subprocess.run([RS, "check", "--format", "json"] + files,
                       capture_output=True, text=True)
    try:
        return json.loads(r.stdout)
    except Exception:
        return []


def run_ref(files):
    # Clean cwd: no project .rigor.yml / Gemfile / sig auto-load, so the
    # reference analyses on core+stdlib only — comparable to rigor-rs's default.
    # Returns None (NOT []) when the reference produced no parseable JSON — a
    # batch failure (one poison file aborting the whole run) would otherwise look
    # like "reference found nothing", turning all of rigor-rs's output into false
    # FP candidates. A None result means the comparison is invalid, not FP-free.
    r = subprocess.run(["ruby", "-I", REF_LIB, REF_EXE, "check", "--format", "json"] + files,
                       capture_output=True, text=True, cwd="/tmp")
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


def audit(tgt, show=12):
    files = rb_files(tgt)
    if not files:
        print(f"{tgt}: no .rb files")
        return 0
    t = time.time()
    ref_diags = run_ref(files)
    if ref_diags is None:
        print(f"\n=== {tgt} ({len(files)} files) ===")
        print("  SKIPPED: reference produced no parseable output on this batch "
              "(likely one file aborts its run) — comparison invalid, not FP-free.")
        return 0
    rs, ref = keys(run_rs(files)), keys(ref_diags)
    fp, gap = rs - ref, ref - rs
    print(f"\n=== {tgt} ({len(files)} files, {time.time() - t:.1f}s) ===")
    print(f"  reference={len(ref)}  rigor-rs={len(rs)}  matched={len(rs & ref)}")
    print(f"  FP candidates (rigor-rs only): {len(fp)}")
    print(f"  coverage gaps (reference only): {len(gap)}")
    if fp:
        print("  FP by rule:", dict(Counter(k[3] for k in fp).most_common()))
        for k in sorted(fp)[:show]:
            print(f"    FP: {k[3]} @ {os.path.basename(k[0])}:{k[1]}:{k[2]}")
    return len(fp)


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(2)
    total = sum(audit(t) for t in sys.argv[1:])
    print(f"\nTOTAL FP candidates: {total}")
    sys.exit(1 if total else 0)
