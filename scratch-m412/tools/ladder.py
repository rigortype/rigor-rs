#!/usr/bin/env python3
"""Crash-site ladder: run crashsite.py (two files, the deep file on a rayon
worker) at every stack size from 2 MiB up to (min surviving stack - 1), so the
walkers show up in the order a growing stack exposes them.

Usage: ladder.py CLI KIND:N:MINW ... [--jobs J] [--step S] [--out-dir D]
Writes one JSON per (kind, MiB) under --out-dir and one summary line per run.
"""
import argparse
import concurrent.futures as cf
import json
import os
import subprocess
import sys

from common import DATA, HERE

ap = argparse.ArgumentParser()
ap.add_argument("cli")
ap.add_argument("specs", nargs="+")
ap.add_argument("--jobs", type=int, default=4)
ap.add_argument("--step", type=int, default=1)
ap.add_argument("--start", type=int, default=2)
ap.add_argument("--out-dir", default=os.path.join(DATA, "sites"))
ap.add_argument("--last-only", action="store_true", help="only minw-1 (the binding point), plus 2 MiB")
a = ap.parse_args()
os.makedirs(a.out_dir, exist_ok=True)
jobs = []
for spec in a.specs:
    kind, n, minw = spec.split(":")
    mibs = sorted({a.start, int(minw) - 1}) if a.last_only else range(a.start, int(minw), a.step)
    for mib in mibs:
        if mib >= 1 and mib < int(minw):
            jobs.append((kind, int(n), mib))


def short(name):
    """`<a::b::T as x::Visit>::m` -> `T::m`; `a::b::<impl a::B>::m` -> `B::m`; else last 2 segments."""
    import re
    m = re.match(r"<(?:[\w:]+::)?(\w+)(?:<.*?>)? as [\w:]+(?:<.*?>)?>::(\w+)$", name)
    if m:
        return "%s::%s" % m.groups()
    m = re.match(r".*<impl (?:[\w:]+::)?(\w+)(?:<.*?>)?>::(\w+)$", name)
    if m:
        return "%s::%s" % m.groups()
    return "::".join(name.split("::")[-2:])[:80]


def run(job):
    kind, n, mib = job
    out = os.path.join(a.out_dir, "%s_%s_%d_%dMiB.json" % (a.cli, kind, n, mib))
    if os.path.exists(out):
        os.remove(out)  # a stale JSON would read as a crash
    r = subprocess.run([sys.executable, os.path.join(HERE, "crashsite.py"), a.cli, kind, str(n), "--two",
                        "--min-stack-mib", str(mib), "--out", out], capture_output=True, text=True)
    return job, out, r.stdout.strip()


with cf.ThreadPoolExecutor(a.jobs) as ex:
    for job, out, line in ex.map(run, jobs):
        kind, n, mib = job
        site = "?"
        if os.path.exists(out):
            res = json.load(open(out))
            cyc = res.get("cycle") or []
            rig = [c for c in cyc if "rigor" in c]
            site = "%s | %s B/cycle | innermost=%s" % (" > ".join(short(c) for c in cyc),
                                                        res.get("cycle_bytes"), res.get("innermost", "")[:60])
        cons = ""
        if os.path.exists(out) and res.get("stack_total"):
            tot = res["stack_total"]
            cons = " || consumers(%.1fMiB): " % (tot / 2**20) + "; ".join("%s %.0f%%" % (short(nm), 100.0 * b / tot) for nm, b in res["consumers"][:5])
        print("%s\t%d\t%dMiB\t%s%s" % (kind, n, mib, site if os.path.exists(out) else line[:200], cons), flush=True)
