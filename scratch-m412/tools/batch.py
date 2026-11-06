#!/usr/bin/env python3
"""Run a tool over a named kind set, logging to data/<name>.log and touching
data/<name>.done at the end (a marker to poll, never a pgrep waiter).

Usage: batch.py NAME TOOL [TOOL-ARGS...] -- KINDSET
KINDSET: capped | capped279 | uncapped | a comma list
"""
import os
import subprocess
import sys

from common import CAPS, DATA, HERE

UNCAPPED = ["chain", "plus", "minus", "andand", "oror", "kwand", "orkw", "index", "safenav",
            "argchain", "blkchain", "callblk", "cpath", "shovel", "lt", "ifmod", "unlessmod",
            "rescmod", "whilemod"]
CAPPED279 = ["array", "call", "block", "if", "paren", "begin", "defined", "super", "yield",
             "def", "class", "hash", "and", "ivopw", "interp"]

name, tool = sys.argv[1], sys.argv[2]
i = sys.argv.index("--")
tool_args, kindset = sys.argv[3:i], sys.argv[i + 1]
kinds = {"capped": list(CAPS), "capped279": CAPPED279, "uncapped": UNCAPPED}.get(kindset, kindset.split(","))
log = open(os.path.join(DATA, name + ".log"), "w")
r = subprocess.run([sys.executable, os.path.join(HERE, tool)] + tool_args + kinds,
                   stdout=log, stderr=subprocess.STDOUT, cwd=HERE)
open(os.path.join(DATA, name + ".done"), "w").write("exit %d\n" % r.returncode)
