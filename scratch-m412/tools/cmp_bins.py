#!/usr/bin/env python3
"""Byte-compare two rigor binaries' `check --format json` (stdout, stderr,
exit code) on generated deep files (KIND:N, one file) and on given .rb files.

Usage: cmp_bins.py CLI_A CLI_B [KIND:N ...] [--files F.rb ...] [--out F]
"""
import argparse
import os
import shutil
import subprocess
import tempfile

from common import DATA, TRIALS, cli, make_trial, rm

ap = argparse.ArgumentParser()
ap.add_argument("a")
ap.add_argument("b")
ap.add_argument("specs", nargs="*")
ap.add_argument("--files", nargs="*", default=[])
ap.add_argument("--out", default=os.path.join(DATA, "cmp_bins.tsv"))
a = ap.parse_args()
bins = [cli(x) if not os.path.isabs(x) else x for x in (a.a, a.b)]
fh = open(a.out, "a")


def run(binary, d, files):
    r = subprocess.run([binary, "check", "--format", "json"] + files, cwd=d, capture_output=True, timeout=3600)
    return r.returncode, r.stdout, r.stderr


def compare(label, d, files):
    ra, rb = run(bins[0], d, files), run(bins[1], d, files)
    same = ra == rb
    line = "%s\t%s\trc=%d/%d\tstdout=%dB\t%s" % (label, "IDENTICAL" if same else "DIFFER", ra[0], rb[0], len(ra[1]),
                                               "" if same else "stderr_a=%r stderr_b=%r" % (ra[2][:120], rb[2][:120]))
    print(line, flush=True)
    fh.write(line + "\n")


for spec in a.specs:
    kind, _, n = spec.partition(":")
    d, files = make_trial(kind, int(n), False)
    try:
        compare(spec, d, files)
    finally:
        rm(d)
for f in a.files:
    d = tempfile.mkdtemp(dir=TRIALS)
    try:
        shutil.copy(f, os.path.join(d, "a.rb"))
        compare(os.path.basename(f), d, ["a.rb"])
    finally:
        rm(d)
