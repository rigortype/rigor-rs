#!/usr/bin/env python3
"""The LSP survival matrix: lsp_probe.py over kinds x {buffer, on-disk alone,
on-disk + sibling}.

Usage: lsp_matrix.py CLI KIND:N ... [--out F]
"""
import argparse
import os
import subprocess
import sys

from common import DATA, HERE

ap = argparse.ArgumentParser()
ap.add_argument("cli")
ap.add_argument("specs", nargs="+")
ap.add_argument("--out", default=os.path.join(DATA, "lsp_matrix.tsv"))
a = ap.parse_args()
fh = open(a.out, "a")
for spec in a.specs:
    kind, n = spec.split(":")
    for extra in ([], ["--on-disk"], ["--on-disk", "--sibling"]):
        r = subprocess.run([sys.executable, os.path.join(HERE, "lsp_probe.py"), a.cli, kind, n] + extra,
                           capture_output=True, text=True, timeout=3600)
        line = r.stdout.strip() or ("ERR " + r.stderr.strip()[-300:])
        print(line, flush=True)
        fh.write(line + "\n")
        fh.flush()
