#!/usr/bin/env python3
"""Run a tool over a named kind set, logging to $RIGOR_STACK_DATA/<name>.log and
touching <name>.done at the end (a marker to poll, never a pgrep waiter).

Usage: batch.py NAME TOOL [TOOL-ARGS...] -- KINDSET
KINDSET: capped | uncapped | inscope | typed | mixed | bound | screen | a comma list
"""
import os
import subprocess
import sys

from common import BOUND, CAPS, DATA, HERE, MIXED, SCREEN_KINDS, TYPED_FIELD, UNCAPPED, UNCAPPED_IN_SCOPE

name, tool = sys.argv[1], sys.argv[2]
i = sys.argv.index("--")
tool_args, kindset = sys.argv[3:i], sys.argv[i + 1]
SETS = {"capped": list(CAPS), "uncapped": UNCAPPED, "inscope": UNCAPPED_IN_SCOPE,
        "typed": TYPED_FIELD, "mixed": MIXED, "bound": BOUND, "screen": SCREEN_KINDS}
kinds = SETS.get(kindset, kindset.split(","))
log = open(os.path.join(DATA, name + ".log"), "w")
r = subprocess.run([sys.executable, os.path.join(HERE, tool)] + tool_args + kinds,
                   stdout=log, stderr=subprocess.STDOUT, cwd=HERE)
open(os.path.join(DATA, name + ".done"), "w").write("exit %d\n" % r.returncode)
