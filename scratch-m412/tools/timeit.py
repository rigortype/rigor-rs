#!/usr/bin/env python3
"""Wall time + peak RSS of one `rigor check` on a generated file, on a big
worker stack (two files) — for spotting super-linear passes on deep input.

Usage: timeit.py CLI KIND N [N...] [--mib M] [--one]
"""
import argparse
import os
import subprocess
import time

from common import cli, make_trial, rm

ap = argparse.ArgumentParser()
ap.add_argument("cli")
ap.add_argument("kind")
ap.add_argument("ns", nargs="+", type=int)
ap.add_argument("--mib", type=int, default=4096)
ap.add_argument("--one", action="store_true")
ap.add_argument("--timeout", type=int, default=1800)
a = ap.parse_args()
rigor = cli(a.cli) if not os.path.isabs(a.cli) else a.cli
for n in a.ns:
    d, files = make_trial(a.kind, n, not a.one)
    try:
        env = dict(os.environ, RUST_MIN_STACK=str(a.mib << 20))
        t = time.time()
        p = subprocess.Popen([rigor, "check", "--format", "json"] + files, cwd=d, env=env,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            out, err = p.communicate(timeout=a.timeout)
        except subprocess.TimeoutExpired:
            p.kill()
            out, err = p.communicate()
            print("%s\t%s\t%d\tTIMEOUT>%ds" % (a.cli, a.kind, n, a.timeout), flush=True)
            continue
        dt = time.time() - t
        ru = os.wait4(p.pid, 0)[2] if False else None
        print("%s\t%s\t%d\trc=%d\t%.2fs\tout=%dB\t%s" % (a.cli, a.kind, n, p.returncode, dt, len(out),
              err.decode("utf-8", "replace").strip().splitlines()[0][:100] if err.strip() else ""), flush=True)
    finally:
        rm(d)
