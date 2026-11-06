#!/usr/bin/env python3
"""Prism's error-free nesting cap for every shape (`depthprobe cap`), plus the
error count / Prism tree depth at fixed sizes for the uncapped candidates.

Usage: caps.py [KIND...]   -> data/caps.tsv (appends)
"""
import os
import subprocess
import sys

from common import DATA, PROBE

CAPPED = ["array", "call", "block", "if", "paren", "begin", "defined", "super", "yield",
          "def", "class", "hash", "and", "ivopw", "interp"]
UNCAPPED = ["chain", "plus", "strcat", "andand", "oror", "kwand", "index", "safenav", "argchain",
            "blkchain", "cpath", "shovel", "lt", "ifmod", "rescmod", "whilemod", "ternary",
            "assign", "pow", "bang", "not", "uminus", "range"]
kinds = sys.argv[1:] or CAPPED + UNCAPPED
out = open(os.path.join(DATA, "caps.tsv"), "a")
for k in kinds:
    r = subprocess.run([PROBE, "cap", k, "1000000" if k in UNCAPPED else "40000"],
                       capture_output=True, text=True)
    line = r.stdout.strip() or ("ERR " + r.stderr.strip().splitlines()[-1] if r.stderr.strip() else "ERR")
    if k in UNCAPPED:
        for n in (10000, 100000, 1000000):
            e = subprocess.run([PROBE, "errors", k, str(n)], capture_output=True, text=True)
            line += "\t" + (e.stdout.strip().split("\t", 2)[2] if e.stdout.strip() else "ERR")
    print(line, flush=True)
    out.write(line + "\n")
