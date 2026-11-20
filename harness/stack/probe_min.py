#!/usr/bin/env python3
"""Smallest thread stack (MiB, or KiB with --kib) on which the in-process
probe survives MODE for KIND at N (MODE: parse = parse + Prism destroy,
visit, lower). Bisects with `depthprobe child`.

--max-mib is always a MiB cap (default 4096), including under --kib, so
`probe_min.py --kib parse array 9999` can report 8257 KiB without bumping
the cap to 65536.

Usage: probe_min.py MODE KIND N [N...] [--max-mib 4096] [--kib]
"""
import argparse
import os
import subprocess
import time

from common import DATA, ensure_probe

ap = argparse.ArgumentParser()
ap.add_argument("mode")
ap.add_argument("kind")
ap.add_argument("ns", nargs="+", type=int)
ap.add_argument("--max-mib", type=int, default=4096)
ap.add_argument("--probe", default=None, help="depthprobe path (default: built under RIGOR_STACK_DATA)")
ap.add_argument("--kib", action="store_true", help="bisect in KiB instead of MiB")
a = ap.parse_args()
a.probe = a.probe or ensure_probe()


def ok(n, size):
    # depthprobe child takes KiB; size is already in the chosen unit.
    kib = size if a.kib else size * 1024
    r = subprocess.run([a.probe, "child", a.mode, a.kind, str(n), str(kib)], capture_output=True, text=True)
    return r.returncode == 0 and r.stdout.startswith("OK")


fh = open(os.path.join(DATA, "probe_min.tsv"), "a")
unit = "KiB" if a.kib else "MiB"
cap = a.max_mib * 1024 if a.kib else a.max_mib
for n in a.ns:
    t = time.time()
    lo, hi, m = 0, None, 1
    while m <= cap:
        if ok(n, m):
            hi = m
            break
        lo, m = m, m * 2
    if hi is None:
        line = "%s\t%s\t%d\t>%d%s" % (a.mode, a.kind, n, cap, unit)
    else:
        while hi - lo > 1:
            mid = (lo + hi) // 2
            if ok(n, mid):
                hi = mid
            else:
                lo = mid
        line = "%s\t%s\t%s\t%d\tmin=%d%s\t%.0fs" % (os.path.basename(os.path.dirname(a.probe)), a.mode, a.kind, n, hi, "KiB" if a.kib else "MiB", time.time() - t)
    print(line, flush=True)
    fh.write(line + "\n")
