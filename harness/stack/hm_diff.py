#!/usr/bin/env python3
"""Byte-diff head vs master `rigor ARGS` on files, each in a fresh cwd.

usage: HM_MASTER=<master rigor> [HM_HEAD=<head rigor>] hm_diff.py [--multi] [--fmt json|text] FILE...

HEAD defaults to target/release/rigor. Each run gets a fresh cwd under
RIGOR_STACK_DATA (HM_RUN overrides).
"""
import os, shutil, subprocess, sys, tempfile

from common import ROOT, TRIALS

HEAD = os.environ.get("HM_HEAD", os.path.join(ROOT, "target", "release", "rigor"))
MASTER = os.environ.get("HM_MASTER") or sys.exit("set HM_MASTER=<path to the master rigor binary>")


def run(binp, files, fmt, extra_env=None):
    d = tempfile.mkdtemp(dir=os.environ.get("HM_RUN", TRIALS))
    names = []
    for i, f in enumerate(files):
        base = os.path.basename(f)
        shutil.copy(f, os.path.join(d, base))
        names.append(base)
    env = dict(os.environ)
    env.pop("RUST_MIN_STACK", None)
    if extra_env:
        env.update(extra_env)
    r = subprocess.run([binp, "check", "--format", fmt, *names], cwd=d, capture_output=True, env=env, timeout=600)
    shutil.rmtree(d, ignore_errors=True)
    # timing/memory stderr lines differ run to run; the rest is contract
    err = b"\n".join(l for l in r.stderr.splitlines() if not l.strip().startswith((b"Wall time:", b"Memory peak:")))
    return r.returncode, r.stdout, err


def main():
    args = sys.argv[1:]
    multi = "--multi" in args
    fmt = "json"
    if "--fmt" in args:
        fmt = args[args.index("--fmt") + 1]
        del args[args.index("--fmt"):args.index("--fmt") + 2]
    files = [a for a in args if not a.startswith("--")]
    groups = [files] if multi else [[f] for f in files]
    same = diff = 0
    for g in groups:
        h = run(HEAD, g, fmt)
        m = run(MASTER, g, fmt)
        if h == m:
            same += 1
        else:
            diff += 1
            print("DIFF", g[:3], "exit", h[0], m[0], "stdout_eq", h[1] == m[1], "stderr_eq", h[2] == m[2])
            if h[2] != m[2]:
                print("  head stderr:", h[2][:300])
                print("  master stderr:", m[2][:300])
    print(f"same={same} diff={diff}")


if __name__ == "__main__":
    main()
