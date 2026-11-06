#!/usr/bin/env python3
"""Prototype loop: find the next overflowing recursive walker for a deep
shape and wrap it in `maybe_grow`, until the shape survives on a small stack.

Each round: crashsite.py (two files, RUST_MIN_STACK=--mib) on the grow build;
take the rigor functions of the innermost recursion cycle; wrap the first one
that is a plain named fn (wrapfn.py), rebuild, repeat. Every wrapped site is
appended to data/grow_iter/sites.tsv.

Usage: growloop.py KIND N [--mib 1] [--rounds 20]
"""
import argparse
import json
import os
import re
import subprocess
import sys

from common import DATA, HERE, M412

TREE = os.path.join(M412, "trees", "grow")
TARGET = os.path.join(M412, "targets", "cli-grow")
ap = argparse.ArgumentParser()
ap.add_argument("kind")
ap.add_argument("n", type=int)
ap.add_argument("--mib", type=int, default=1)
ap.add_argument("--rounds", type=int, default=20)
a = ap.parse_args()
os.makedirs(os.path.join(DATA, "grow_iter"), exist_ok=True)
log = open(os.path.join(DATA, "grow_iter", "sites.tsv"), "a")


def build():
    r = subprocess.run(["cargo", "+1.88.0", "build", "--release", "--offline", "-p", "rigor-cli"], cwd=TREE,
                       env=dict(os.environ, CARGO_TARGET_DIR=TARGET), capture_output=True, text=True)
    if r.returncode:
        print(r.stderr[-3000:])
        sys.exit(1)


def locate(sym):
    """`rigor_infer::a::b::<impl ...>::f` / `rigor_infer::a::b::f` -> (file, f)."""
    m = re.match(r"(rigor_\w+)::(.*?)(?:::<impl [^>]*>)?::(\w+)$", sym)
    if not m:
        return None
    crate, mods, fn = m.groups()
    base = os.path.join(TREE, "crates", crate.replace("_", "-"), "src")
    parts = [p for p in mods.split("::") if p and not p.startswith("<")]
    # nested fns: drop trailing path parts until a file exists
    while parts:
        for cand in (os.path.join(base, *parts) + ".rs", os.path.join(base, *parts, "mod.rs")):
            if os.path.exists(cand):
                return cand, fn
        parts = parts[:-1]
    return (os.path.join(base, "lib.rs"), fn)


for rnd in range(a.rounds):
    out = os.path.join(DATA, "grow_iter", "%s_%d_%dMiB_r%d.json" % (a.kind, a.n, a.mib, rnd))
    if os.path.exists(out):
        os.remove(out)  # a stale JSON would read as a crash
    r = subprocess.run([sys.executable, os.path.join(HERE, "crashsite.py"), "cli-grow", a.kind, str(a.n), "--two",
                        "--min-stack-mib", str(a.mib), "--out", out], capture_output=True, text=True)
    print(r.stdout.strip()[:600], flush=True)
    if not os.path.exists(out):
        print("SURVIVED (no crash stop)")
        break
    res = json.load(open(out))
    cyc = res.get("cycle") or []
    cands = [c for c in cyc if re.match(r"rigor_\w+::", c) and "{{closure}}" not in c and " as " not in c]
    if not cands:
        print("no plain rigor fn in cycle; stop:", cyc)
        break
    sym = cands[0]
    loc = locate(sym)
    print("wrap", sym, "->", loc, flush=True)
    w = subprocess.run([sys.executable, os.path.join(HERE, "wrapfn.py"), loc[0], loc[1],
                        "crate::stack::maybe_grow" if "rigor-parse" in loc[0] else "rigor_parse::stack::maybe_grow"],
                       capture_output=True, text=True)
    if w.returncode:
        print("wrapfn failed:", w.stderr[-800:])
        break
    log.write("%s\t%d\t%s\t%s\t%s\n" % (a.kind, a.n, sym, os.path.relpath(loc[0], TREE), res.get("cycle_bytes")))
    log.flush()
    build()
