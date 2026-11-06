#!/usr/bin/env python3
"""Where does `rigor check` overflow? Runs the release CLI under lldb on a
depth-N file and records the crashing thread, its innermost frames, the first
rigor function, the recursion cycle and its stack cost per cycle.

Usage: crashsite.py CLI KIND N [--two] [--min-stack-mib M] [--ulimit-kib K] [--out F]
  --two            add a trivial b.rb so the deep file lands on a rayon worker
  --min-stack-mib  RUST_MIN_STACK for spawned threads
  --ulimit-kib     soft RLIMIT_STACK for the main thread
Prints one summary line; the full JSON goes to --out (default: stdout only).
"""
import argparse
import json
import os
import subprocess
import tempfile

from common import HERE, M412, cli, make_trial, rm

ap = argparse.ArgumentParser()
ap.add_argument("cli")
ap.add_argument("kind")
ap.add_argument("n", type=int)
ap.add_argument("--two", action="store_true")
ap.add_argument("--min-stack-mib", type=int)
ap.add_argument("--ulimit-kib", type=int)
ap.add_argument("--out")
ap.add_argument("--sub", default="check", help="subcommand (check, coverage, sig-gen, type-of ...)")
ap.add_argument("--extra", default="", help="extra args after the subcommand, space-separated")
a = ap.parse_args()

rigor = cli(a.cli) if not os.path.isabs(a.cli) else a.cli
d, files = make_trial(a.kind, a.n, a.two)
a.out = os.path.abspath(a.out) if a.out else None
out_json = a.out or os.path.join(d, "site.json")
env = dict(os.environ)
if a.min_stack_mib:
    env["RUST_MIN_STACK"] = str(a.min_stack_mib << 20)
args = [a.sub] + (a.extra.split() if a.extra else []) + (["--format", "json"] if a.sub == "check" else []) + files
lldb_cmd = ["lldb", "--batch",
            "-o", "command script import " + os.path.join(HERE, "lldb_site.py"),
            "-o", "settings set target.env-vars " + (("RUST_MIN_STACK=" + env["RUST_MIN_STACK"]) if a.min_stack_mib else "X=1"),
            "-o", "run",
            "-k", "script lldb_site.report(%r)" % out_json,
            "-k", "kill", "-k", "quit",
            "--", rigor] + args
if a.ulimit_kib:
    cmd = ["/bin/sh", "-c", 'ulimit -s %d; exec "$@"' % a.ulimit_kib, "sh"] + lldb_cmd
else:
    cmd = lldb_cmd
try:
    p = subprocess.run(cmd, cwd=d, capture_output=True, text=True, timeout=1800, env=env)
    if os.path.exists(out_json):
        res = json.load(open(out_json))
        cyc = res.get("cycle") or []
        ent = res.get("entry") or []
        fra = res.get("first_rigor_anywhere")
        print("%s %s N=%d%s%s%s thread=%r frames=%d innermost=%s cycle=[%s] bytes/cycle=%s entry=%s first_rigor=%s" % (
            a.cli, a.kind, a.n, " two" if a.two else "",
            " minstack=%dMiB" % a.min_stack_mib if a.min_stack_mib else "",
            " ulimit=%dKiB" % a.ulimit_kib if a.ulimit_kib else "",
            res.get("thread_name"), res.get("num_frames", 0), res.get("innermost"),
            " > ".join(c[:100] for c in cyc), res.get("cycle_bytes"),
            " < ".join(e[1][:80] for e in ent[:3]), fra[1][:120] if fra else None))
        tot = res.get("stack_total") or 0
        if tot:
            print("    consumers of %.2f MiB: %s" % (tot / 2**20, "; ".join(
                "%s %.0f%%" % (nm[-90:], 100.0 * b / tot) for nm, b in res["consumers"][:6])))
        if a.out:
            res["args"] = vars(a)
            json.dump(res, open(a.out, "w"), indent=1)
    else:
        tail = (p.stdout + p.stderr).strip().splitlines()[-6:]
        print("%s %s N=%d%s NO CRASH STOP: %s" % (a.cli, a.kind, a.n, " two" if a.two else "", " / ".join(tail)))
finally:
    rm(d)
