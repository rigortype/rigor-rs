#!/usr/bin/env python3
"""Where does `rigor check` spend its time on a deep file? Starts the run on a
big worker stack, waits DELAY s, runs macOS `sample` for DUR s, and prints the
heaviest frames of the busy thread (sample's "Sort by top of stack" list).

Usage: profile.py CLI KIND N [--delay S] [--dur S] [--out F]
"""
import argparse
import os
import re
import subprocess
import time

from common import cli, make_trial, rm

ap = argparse.ArgumentParser()
ap.add_argument("cli")
ap.add_argument("kind")
ap.add_argument("n", type=int)
ap.add_argument("--delay", type=float, default=2)
ap.add_argument("--dur", type=int, default=5)
ap.add_argument("--out")
a = ap.parse_args()
rigor = cli(a.cli) if not os.path.isabs(a.cli) else a.cli
d, files = make_trial(a.kind, a.n, True)
try:
    env = dict(os.environ, RUST_MIN_STACK=str(4096 << 20))
    p = subprocess.Popen([rigor, "check", "--format", "json"] + files, cwd=d, env=env,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(a.delay)
    outf = a.out or os.path.join(d, "sample.txt")
    subprocess.run(["sample", str(p.pid), str(a.dur), "-mayDie", "-file", outf], capture_output=True)
    p.kill()
    p.wait()
    txt = open(outf).read()
    m = re.search(r"Sort by top of stack, same collapsed.*?\n(.*?)\n\n", txt, re.S)
    print("\n".join((m.group(1) if m else txt[-3000:]).splitlines()[:25]))
finally:
    rm(d)
