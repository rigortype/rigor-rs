#!/usr/bin/env python3
"""End-to-end `rigor check` survival and minimum stack, per shape.

For each KIND at size N (default: Prism's cap for a capped kind):
  main    one file, the deep file on the main thread (default ulimit -s)
  worker  the deep file + a trivial file, so it lands on a rayon worker
          (default 2 MiB unless RUST_MIN_STACK is set)
  minw    smallest RUST_MIN_STACK in MiB (two files) that survives
  minm    smallest `ulimit -s` in MiB (one file) that survives (<= 63, the
          macOS hard limit)

A trial FAILS when the process dies by a signal or prints "overflowed its
stack"; the failing thread's name is recorded ('main' or '<unknown>' = pool).

Usage: e2e.py CLI KIND[:N] ... [--only main,worker,minw,minm] [--max-mib M] [--out F]
"""
import argparse
import os
import re
import subprocess
import time

from common import CAPS, DATA, cli, make_trial, rm

ap = argparse.ArgumentParser()
ap.add_argument("cli")
ap.add_argument("kinds", nargs="+")
ap.add_argument("--only", default="main,worker,minw,minm")
ap.add_argument("--max-mib", type=int, default=4096)
ap.add_argument("--out", default=os.path.join(DATA, "e2e.tsv"))
ap.add_argument("--sub", default="check")
a = ap.parse_args()
rigor = cli(a.cli) if not os.path.isabs(a.cli) else a.cli
only = a.only.split(",")
_trials = {}


def trial(kind, n, two, min_stack_mib=None, ulimit_mib=None):
    key = (kind, n, two, min_stack_mib, ulimit_mib)
    if key in _trials:
        return _trials[key]
    d, files = make_trial(kind, n, two)
    try:
        env = dict(os.environ)
        env.pop("RUST_MIN_STACK", None)
        if min_stack_mib:
            env["RUST_MIN_STACK"] = str(min_stack_mib << 20)
        args = [rigor, a.sub] + (["--format", "json"] if a.sub == "check" else []) + files
        if ulimit_mib:
            args = ["/bin/sh", "-c", 'ulimit -s %d && exec "$@"' % (ulimit_mib * 1024), "sh"] + args
        t = time.time()
        r = subprocess.run(args, cwd=d, capture_output=True, env=env, timeout=3600)
        dt = time.time() - t
        err = r.stderr.decode("utf-8", "replace")
        m = re.search(r"thread '([^']*)'.*has overflowed its stack", err)
        crashed = r.returncode < 0 or m is not None
        if crashed:
            det = "CRASH rc=%d thread=%s" % (r.returncode, m.group(1) if m else "?")
        else:
            det = "ok rc=%d out=%dB %.1fs" % (r.returncode, len(r.stdout), dt)
        _trials[key] = (not crashed, det)
        return _trials[key]
    finally:
        rm(d)


def min_mib(kind, n, two, lo_cap=None):
    """Smallest MiB that survives (worker: RUST_MIN_STACK; main: ulimit)."""
    hi_cap = a.max_mib if two else 63

    def ok(m):
        return trial(kind, n, two, min_stack_mib=m if two else None, ulimit_mib=None if two else m)[0]

    lo, hi = 0, None  # lo = known-bad MiB (0 = none), hi = known-good
    m = 1
    while m <= hi_cap:
        if ok(m):
            hi = m
            break
        lo = m
        m *= 2
    if hi is None:
        if hi_cap not in (lo,) and ok(hi_cap):
            hi = hi_cap
        else:
            return ">%d" % hi_cap, trial(kind, n, two, hi_cap if two else None, None if two else hi_cap)[1]
    while hi - lo > 1:
        mid = (lo + hi) // 2
        if ok(mid):
            hi = mid
        else:
            lo = mid
    return str(hi), trial(kind, n, two, hi if two else None, None if two else hi)[1]


out = open(a.out, "a")
for spec in a.kinds:
    kind, _, n = spec.partition(":")
    n = int(n) if n else CAPS[kind]
    row = [a.cli, a.sub, kind, str(n)]
    if "main" in only:
        row.append("main:" + trial(kind, n, False)[1])
    if "worker" in only:
        row.append("worker2MiB:" + trial(kind, n, True)[1])
    if "minw" in only:
        mw, det = min_mib(kind, n, True)
        row.append("minw=%sMiB(%s)" % (mw, det))
    if "minm" in only:
        mm, det = min_mib(kind, n, False)
        row.append("minm=%sMiB(%s)" % (mm, det))
    line = "\t".join(row)
    print(line, flush=True)
    out.write(line + "\n")
    out.flush()
