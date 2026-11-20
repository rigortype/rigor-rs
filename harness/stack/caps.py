#!/usr/bin/env python3
"""Prism's error-free nesting cap for every shape (`depthprobe cap`), plus the
error count / Prism tree depth at fixed sizes for the uncapped candidates.

Usage: caps.py [KIND...]   -> $RIGOR_STACK_DATA/caps.tsv (appends)
"""
import os
import subprocess
import sys

from common import CAPS, DATA, MIXED, SCREEN_KINDS, ensure_probe

# Kinds Prism may not cap: probe them to a million links; capped kinds stop
# at 40,000 (their cap sits below 10,000).
CAPPED_ALL = set(CAPS) | set(MIXED)

kinds = sys.argv[1:] or SCREEN_KINDS
probe = ensure_probe()
out = open(os.path.join(DATA, "caps.tsv"), "a")
for k in kinds:
    maxn = "1000000" if k not in CAPPED_ALL else "40000"
    r = subprocess.run([probe, "cap", k, maxn], capture_output=True, text=True)
    line = r.stdout.strip() or ("ERR " + r.stderr.strip().splitlines()[-1] if r.stderr.strip() else "ERR")
    if k not in CAPPED_ALL:
        for n in (10000, 100000, 1000000):
            e = subprocess.run([probe, "errors", k, str(n)], capture_output=True, text=True)
            line += "\t" + (e.stdout.strip().split("\t", 2)[2] if e.stdout.strip() else "ERR")
    print(line, flush=True)
    out.write(line + "\n")
