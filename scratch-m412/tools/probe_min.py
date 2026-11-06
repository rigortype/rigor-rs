#!/usr/bin/env python3
"""Smallest thread stack (MiB) on which the in-process probe survives MODE for
KIND at N (MODE: parse = parse + Prism destroy, visit, lower). Bisects over
MiB with `depthprobe child`.

Usage: probe_min.py MODE KIND N [N...] [--max-mib 4096]
"""
import argparse
import os
import subprocess
import time

from common import DATA, PROBE

ap = argparse.ArgumentParser()
ap.add_argument("mode")
ap.add_argument("kind")
ap.add_argument("ns", nargs="+", type=int)
ap.add_argument("--max-mib", type=int, default=4096)
ap.add_argument("--probe", default=PROBE)
ap.add_argument("--kib", action="store_true", help="bisect in KiB instead of MiB")
a = ap.parse_args()


def ok(n, mib):
    r = subprocess.run([a.probe, "child", a.mode, a.kind, str(n), str(mib if a.kib else mib * 1024)], capture_output=True, text=True)
    return r.returncode == 0 and r.stdout.startswith("OK")


fh = open(os.path.join(DATA, "probe_min.tsv"), "a")
for n in a.ns:
    t = time.time()
    lo, hi, m = 0, None, 1
    while m <= a.max_mib:
        if ok(n, m):
            hi = m
            break
        lo, m = m, m * 2
    if hi is None:
        line = "%s\t%s\t%d\t>%dMiB" % (a.mode, a.kind, n, a.max_mib)
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
