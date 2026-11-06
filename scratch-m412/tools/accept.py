#!/usr/bin/env python3
"""#412 acceptance: `rigor check --format json` on KIND:N, once alone (main
thread) and once with a trivial second file (rayon worker). Records the exit
code, whether the process crashed, the number of findings for a.rb, whether
one of them is `internal-error`, and the first stderr line.

Usage: accept.py CLI KIND[:N] ... [--sub check|coverage] [--workers N] [--out F]
"""
import argparse
import json
import os
import re
import subprocess
import time

from common import CAPS, DATA, cli, make_trial, rm

ap = argparse.ArgumentParser()
ap.add_argument("cli")
ap.add_argument("kinds", nargs="+")
ap.add_argument("--sub", default="check")
ap.add_argument("--workers", type=int)
ap.add_argument("--out", default=os.path.join(DATA, "accept.tsv"))
ap.add_argument("--timeout", type=int, default=1800)
a = ap.parse_args()
rigor = cli(a.cli) if not os.path.isabs(a.cli) else a.cli
fh = open(a.out, "a")


def run(kind, n, two):
    d, files = make_trial(kind, n, two)
    try:
        env = dict(os.environ)
        env.pop("RUST_MIN_STACK", None)
        args = [rigor, a.sub] + (["--format", "json"] if a.sub in ("check", "coverage") else [])
        if a.workers:
            args.append("--workers=%d" % a.workers)
        if a.sub == "type-of":
            files = [f + ":1:1" for f in files]
        t = time.time()
        r = subprocess.run(args + files, cwd=d, capture_output=True, env=env, timeout=a.timeout)
        dt = time.time() - t
        err = r.stderr.decode("utf-8", "replace")
        crashed = r.returncode < 0 or "overflowed its stack" in err
        info = ""
        if a.sub == "check" and not crashed:
            try:
                rows = [x for x in json.loads(r.stdout) if x.get("path") == "a.rb"]
                ie = [x for x in rows if x.get("rule") == "internal-error"]
                info = "a.rb findings=%d internal-error=%d%s" % (
                    len(rows), len(ie), (" [%s] %s" % (ie[0].get("severity"), ie[0].get("message"))) if ie else "")
            except Exception as e:  # noqa: BLE001
                info = "unparseable stdout: %s" % e
        elif not crashed:
            info = "out=%dB" % len(r.stdout)
        m = re.search(r"thread '([^']*)'.*overflowed", err)
        return "%s rc=%d %.1fs %s%s %s" % ("CRASH" if crashed else "ok", r.returncode, dt, info,
                                           (" thread=" + m.group(1)) if m else "",
                                           ("stderr: " + err.strip().splitlines()[0][:160]) if err.strip() else "")
    finally:
        rm(d)


for spec in a.kinds:
    kind, _, n = spec.partition(":")
    n = int(n) if n else CAPS[kind]
    for two in (False, True):
        line = "%s\t%s%s\t%s\t%d\t%s\t%s" % (a.cli, a.sub, (" --workers=%d" % a.workers) if a.workers else "",
                                            kind, n, "2 files" if two else "1 file", run(kind, n, two))
        print(line, flush=True)
        fh.write(line + "\n")
        fh.flush()
