#!/usr/bin/env python3
"""Time `depthprobe errors KIND N` over a list of N (spot quadratic parses).

Usage: scale.py KIND N1 N2 ...
"""
import subprocess
import sys
import time

from common import PROBE

kind = sys.argv[1]
for n in sys.argv[2:]:
    t = time.time()
    r = subprocess.run([PROBE, "errors", kind, n], capture_output=True, text=True, timeout=600)
    print("%s\t%s\t%.2fs\t%s" % (kind, n, time.time() - t, (r.stdout.strip() or r.stderr.strip()[-200:])[:300]), flush=True)
