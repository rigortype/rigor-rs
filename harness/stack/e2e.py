#!/usr/bin/env python3
"""End-to-end `rigor check` survival per shape, post-#431 semantics.

rigor's analysis threads are a fixed ladder now: the command thread tries
256 MiB then 64 MiB then runs INLINE on the process main thread (Unix);
pool workers try 256, 64, then an explicit 2 MiB, and a pool with no worker
at all exits 70 (ADR-0045 amendments; rigor-rs#431/#678). `RUST_MIN_STACK`
moves nothing — every spawn names an explicit size — so the #412-era
"min worker stack" bisect is dead. What a screen can still measure:

  main    one file (command thread, 256 MiB on a healthy host)
  worker  the deep file + a trivial file, so it lands on a pool worker
  at64    two files under a `ulimit -v` that forces 64 MiB fallback threads
  at2     two files under a `ulimit -v` that forces 2 MiB pool workers
  inline  one file under a `ulimit -v` that forces the command thread onto
          the process main thread (pool workers at 2 MiB)
  minm    smallest `ulimit -s` (MiB) that survives the inline run — the
          check pipeline's real floor, the #412 table's `minm` equivalent

A trial FAILS when the process dies by a signal or prints "overflowed its
stack". The rung a run landed on is read off the stderr fallback warning
("using 64 MiB" / "using 2 MiB" / "using the process main thread").

`ulimit -v` is honoured on Linux; on macOS it is not, so the forced-rung
columns report n/a there (their numbers come from the CI job).

Usage: e2e.py CLI KIND[:N] ... [--only ...] [--minm-budget V] [--out F]
  CLI is `release`/`debug` (target/<name>/rigor) or a binary path.
"""
import argparse
import os
import re
import subprocess
import sys
import time

from common import CAPS, DATA, cli, make_trial, rm

ap = argparse.ArgumentParser()
ap.add_argument("cli")
ap.add_argument("kinds", nargs="+")
ap.add_argument("--only", default="main,worker,at64,at2,inline,minm")
ap.add_argument("--out", default=os.path.join(DATA, "e2e.tsv"))
ap.add_argument("--sub", default="check")
ap.add_argument("--timeout", type=int, default=1800)
ap.add_argument("--minm-headroom", type=int, default=64,
                help="ulimit -v headroom (MiB) above each ulimit -s under test;")
                # must cover the load image + 2 MiB workers, stay < 64 MiB free
a = ap.parse_args()
rigor = cli(a.cli)
only = a.only.split(",")
_trials = {}
_budgets = None  # {"at64": MiB, "at2": MiB, "inline": MiB}

# The fallback warning (rigor-cli threads.rs): post-#703, one line per
# strictly-worse rung: "some analysis threads are using 64 MiB" / "2 MiB",
# or "some analysis runs on the process main thread (N MiB)". The pre-#703
# "could not reserve ... using X" form is kept for older binaries.
OLD_WARN = re.compile(
    r"could not reserve \d+ MiB thread stacks, using (?:(\d+) MiB|the process main thread)")
POOL_ERR = "could not start the analysis thread pool"


def rungs(err):
    """The fallback rungs a run landed on, weakest first. Empty = all 256."""
    out = set()
    if POOL_ERR in err:
        return ["dead"]
    if "some analysis runs on the process main thread" in err:
        out.add("main")
    if "some analysis threads are using 2 MiB" in err:
        out.add("2")
    if "some analysis threads are using 64 MiB" in err:
        out.add("64")
    m = OLD_WARN.search(err)
    if m:
        out.add(m.group(1) if m.group(1) else "main")
    return [r for r in ("main", "2", "64") if r in out]


def sh_ulimit(args, v_mib=None, s_mib=None):
    # bash, not /bin/sh: dash's ulimit has no working -v on Ubuntu.
    pre = []
    if v_mib:
        pre.append("ulimit -v %d" % (v_mib * 1024))
    if s_mib:
        pre.append("ulimit -s %d" % (s_mib * 1024))
    if not pre:
        return args
    return ["/bin/bash", "-c", "%s && exec \"$@\"" % "; ".join(pre), "sh"] + args


def run(files, d, v_mib=None, s_mib=None, timeout=None, no_ruby=False):
    args = sh_ulimit([rigor, a.sub] + (["--format", "json"] if a.sub == "check" else []) + files,
                     v_mib, s_mib)
    env = dict(os.environ)
    if no_ruby:
        # The Ruby sidecar is a separate process, not part of rigor's thread
        # stacks; under a rung-forcing budget its spawn is the binding
        # failure, not the analysis. `off` runs the Ruby-free sound subset.
        env["RIGOR_NO_RUBY"] = "1"
    t = time.time()
    r = subprocess.run(args, cwd=d, capture_output=True, env=env,
                       timeout=timeout or a.timeout)
    dt = time.time() - t
    err = r.stderr.decode("utf-8", "replace")
    rg = rungs(err)
    if ("cannot modify limit" in err or "invalid option" in err
            or "Illegal option" in err):
        rg = ["noforce"]  # ulimit itself failed (macOS has no working -v)
    elif (r.returncode in (126, 127)
            or (r.returncode != 0 and not err.strip())
            or "error while loading shared libraries" in err
            or "Cannot allocate memory" in err):
        rg = ["dead"]  # the budget stopped even the exec
    mo = re.search(r"thread '([^']*)'.*has overflowed its stack", err)
    crashed = r.returncode < 0 or mo is not None
    return {"rc": r.returncode, "rung": rg[0] if rg else "256",
            "rungs": "+".join(rg) if rg else "256", "crashed": crashed,
            "dt": dt, "thread": mo.group(1) if mo else None,
            "err": err.strip().splitlines()[0][:120] if err.strip() else ""}


def trial(kind, n, two, v_mib=None, s_mib=None, no_ruby=False):
    key = (kind, n, two, v_mib, s_mib, no_ruby)
    if key in _trials:
        return _trials[key]
    d, files = make_trial(kind, n, two)
    try:
        r = run(files, d, v_mib, s_mib, no_ruby=no_ruby)
        # Survival = check ran to a verdict (rc 0 clean, 1 findings). Anything
        # else — a crash, a dead pool/exec, a could-not-start exit like the
        # sidecar's 69 or the pool's 70 — never analysed the file.
        survived = (not r["crashed"] and r["rc"] in (0, 1)
                    and r["rung"] not in ("dead", "noforce"))
        det = (("CRASH rc=%d thread=%s" % (r["rc"], r["thread"])) if r["crashed"]
               else ("dead rc=%d" % r["rc"]) if not survived
               else "ok rc=%d rung=%s %.1fs" % (r["rc"], r["rungs"], r["dt"]))
        _trials[key] = (survived, det)
        return _trials[key]
    finally:
        rm(d)


def probe_budgets():
    """The machine's rung-forcing `ulimit -v` budgets (MiB), probed on a
    trivial file: the first budget whose stderr names each rung. None when
    `ulimit -v` is not honoured (macOS) or probing failed."""
    global _budgets
    if _budgets is not None:
        return _budgets
    d, files = make_trial("plus", 3, True)
    try:
        def rung_at(v):
            return run(files, d, v_mib=v, timeout=60)["rung"]

        # Descend a coarse ladder to bracket each rung's boundary, then bisect.
        ladder = [8192, 4096, 2048, 1536, 1024, 768, 640, 512, 448, 384, 320,
                  256, 224, 192, 160, 128, 96, 80, 64, 48, 32, 16]
        seen = {}
        for v in ladder:
            seen[v] = rung_at(v)
        print("e2e: rung probe " + " ".join("%dM:%s" % (v, seen[v]) for v in ladder),
              file=sys.stderr)
        if seen[ladder[-1]] in ("256", "noforce"):
            _budgets = {}  # ulimit -v not honoured (macOS): every budget is 256
            return _budgets
        out = {}
        for key, rung in {"at64": "64", "at2": "2", "inline": "main"}.items():
            vs = [v for v in ladder if seen[v] == rung]
            if vs:
                out[key] = (min(vs) + max(vs)) // 2
            # else: the rung is skipped on this host; the column reports n/a
        _budgets = out
        return _budgets
    finally:
        rm(d)


def budget_for(key):
    b = probe_budgets()
    return b.get(key)


def minm(kind, n):
    """Smallest `ulimit -s` MiB on which the INLINE run survives. The
    rung-forcing `ulimit -v` scales with the stack under test — the
    process-main stack is itself charged to RLIMIT_AS, so the budget is
    s + headroom: enough for the load image and 2 MiB pool workers, short
    of the command thread's 64 MiB rung."""
    hr = a.minm_headroom
    # ulimit -v unhonoured (macOS) or unstartable: nothing to bisect.
    if trial(kind, n, False, v_mib=65, s_mib=1, no_ruby=True)[1].startswith(
            "dead"):
        return "n/a", "ulimit -v/-s not honoured on this host"

    def ok(m):
        return trial(kind, n, False, v_mib=m + hr, s_mib=m, no_ruby=True)[0]

    lo, hi = 1, None
    while lo <= 512:
        if ok(lo):
            hi = lo
            break
        lo *= 2
    if hi is None:
        return ">512", trial(kind, n, False, v_mib=512 + hr, s_mib=512,
                             no_ruby=True)[1]
    lo = hi // 2
    while hi - lo > 1:
        mid = (lo + hi) // 2
        if ok(mid):
            hi = mid
        else:
            lo = mid
    return str(hi), trial(kind, n, False, v_mib=hi + hr, s_mib=hi,
                          no_ruby=True)[1]


out = open(a.out, "a")
for spec in a.kinds:
    kind, _, n = spec.partition(":")
    n = int(n) if n else CAPS[kind]
    row = [rigor.split("/target/")[-1], a.sub, kind, str(n)]
    if "main" in only:
        row.append("main:" + trial(kind, n, False)[1])
    if "worker" in only:
        row.append("worker:" + trial(kind, n, True)[1])
    for key in ("at64", "at2"):
        if key in only:
            v = budget_for(key)
            row.append(key + "=" + (trial(kind, n, True, v_mib=v)[1] if v is not None else "n/a"))
    if "inline" in only:
        v = budget_for("inline")
        row.append("inline=" + (trial(kind, n, False, v_mib=v, no_ruby=True)[1]
                               if v is not None else "n/a"))
    if "minm" in only:
        mm, det = minm(kind, n)
        row.append("minm=%sMiB(%s)" % (mm, det))
    line = "\t".join(row)
    print(line, flush=True)
    out.write(line + "\n")
    out.flush()
