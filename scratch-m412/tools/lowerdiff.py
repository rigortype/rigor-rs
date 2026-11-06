#!/usr/bin/env python3
"""Lowering differential + parse/lower timing over the 19k-file corpus for
two probe279 builds (default master vs grow).

  dump:  `depthprobe dump LIST OUT` per variant, then per-file digest compare
  bench: `depthprobe bench LIST REPS`, alternated ROUNDS times

Usage: lowerdiff.py LIST [--variants master,grow] [--rounds 3] [--reps 3] [--no-dump]
"""
import argparse
import os
import subprocess

from common import DATA, M412

ap = argparse.ArgumentParser()
ap.add_argument("list")
ap.add_argument("--variants", default="master,grow")
ap.add_argument("--rounds", type=int, default=3)
ap.add_argument("--reps", type=int, default=3)
ap.add_argument("--no-dump", action="store_true")
a = ap.parse_args()
vs = a.variants.split(",")
exe = {v: os.path.join(M412, "targets", "probe279-" + v, "release", "depthprobe") for v in vs}
out = open(os.path.join(DATA, "lowerdiff.txt"), "a")


def say(s):
    print(s, flush=True)
    out.write(s + "\n")
    out.flush()


if not a.no_dump:
    dumps = {}
    for v in vs:
        f = os.path.join(M412, "dump_%s.tsv" % v)
        r = subprocess.run([exe[v], "dump", a.list, f], capture_output=True, text=True)
        say("dump %s exit %d %s" % (v, r.returncode, r.stderr.strip()))
        dumps[v] = dict(line.rstrip("\n").split("\t", 1) for line in open(f))
    base = dumps[vs[0]]
    for v in vs[1:]:
        diff = [k for k in base if base[k] != dumps[v].get(k)]
        say("%s vs %s: files=%d identical=%d DIFFER=%d %s" % (v, vs[0], len(base), len(base) - len(diff), len(diff), diff[:5]))
for rnd in range(a.rounds):
    for v in (vs if rnd % 2 == 0 else vs[::-1]):
        r = subprocess.run([exe[v], "bench", a.list, str(a.reps)], capture_output=True, text=True)
        say("bench r%d %-8s %s" % (rnd, v, r.stdout.strip()))
