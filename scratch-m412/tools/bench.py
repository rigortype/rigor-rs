#!/usr/bin/env python3
"""Wall time + peak RSS of `rigor check --format json <every .rb>` over a corpus,
per variant, interleaved (ABAB...) so machine noise hits every variant alike.

A variant is NAME=CLI[,ENV=VAL...], e.g.
  base=cli-188  s64=cli-188,RUST_MIN_STACK=67108864  grow=cli-grow
Each run gets a FRESH empty cwd and the explicit file list (fp_audit's shape).
Also records the output digest so a variant that changes output is visible.

Usage: bench.py CORPUS_DIR REPS VARIANT... [--out F]
"""
import argparse
import hashlib
import os
import statistics
import subprocess
import tempfile
import time

from common import DATA, TRIALS, cli, rm

ap = argparse.ArgumentParser()
ap.add_argument("corpus")
ap.add_argument("reps", type=int)
ap.add_argument("variants", nargs="+")
ap.add_argument("--out", default=os.path.join(DATA, "bench.tsv"))
a = ap.parse_args()


def rb_files(d):
    out = []
    for root, dirs, files in os.walk(d):
        dirs[:] = sorted(x for x in dirs if not x.startswith("."))
        for f in sorted(files):
            p = os.path.join(root, f)
            if f.endswith(".rb") and os.path.isfile(p) and not os.path.islink(p):
                out.append(p)
    return out


files = ([l.strip() for l in open(a.corpus) if l.strip()] if os.path.isfile(a.corpus) else rb_files(a.corpus))
if len(files) > 8000:
    # ARG_MAX: pass the directory itself (rigor expands it, ADR-0040)
    files = [os.path.abspath(a.corpus)]
variants = []
for v in a.variants:
    name, spec = v.split("=", 1)
    parts = spec.split(",")
    env = dict(os.environ)
    env.pop("RUST_MIN_STACK", None)
    for kv in parts[1:]:
        k, val = kv.split("=", 1)
        env[k] = val
    variants.append((name, cli(parts[0]) if not os.path.isabs(parts[0]) else parts[0], env))
res = {name: [] for name, _, _ in variants}
digests = {name: set() for name, _, _ in variants}
for rep in range(a.reps):
    for name, rigor, env in (variants if rep % 2 == 0 else variants[::-1]):
        d = tempfile.mkdtemp(dir=TRIALS)
        try:
            t = time.time()
            p = subprocess.Popen([rigor, "check", "--format", "json"] + files, cwd=d, env=env,
                                 stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            out = p.stdout.read()
            _, status, ru = os.wait4(p.pid, 0)
            dt = time.time() - t
            res[name].append((dt, ru.ru_maxrss, ru.ru_utime + ru.ru_stime))
            digests[name].add(hashlib.sha256(out).hexdigest()[:12])
        finally:
            rm(d)
with open(a.out, "a") as fh:
    for name, rows in res.items():
        walls = [r[0] for r in rows]
        rss = [r[1] for r in rows]
        cpu = [r[2] for r in rows]
        line = "%s\t%s\tfiles=%d\treps=%d\twall_med=%.2fs\twall_min=%.2fs\tcpu_med=%.1fs\trss_max_med=%.0fMiB\trss_max_max=%.0fMiB\tout=%s" % (
            os.path.basename(a.corpus.rstrip("/")), name, len(files), len(rows), statistics.median(walls), min(walls),
            statistics.median(cpu), statistics.median(rss) / 2**20, max(rss) / 2**20, ",".join(sorted(digests[name])))
        print(line, flush=True)
        fh.write(line + "\n")
