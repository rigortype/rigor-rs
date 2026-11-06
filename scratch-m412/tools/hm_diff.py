#!/usr/bin/env python3
"""Byte-diff head vs master `rigor ARGS` on files, each in a fresh cwd.

usage: hm_diff.py [--multi] [--fmt json|text] FILE...
"""
import os, shutil, subprocess, sys, tempfile

SP = os.path.dirname(os.path.abspath(__file__))
HEAD = os.environ["HM_HEAD"]
MASTER = os.environ["HM_MASTER"]


def run(binp, files, fmt, extra_env=None):
    d = tempfile.mkdtemp(dir=os.environ.get("HM_RUN", "/private/tmp/claude-501/-Users-megurine-repo-rust-rigor-rs--claude-worktrees-nice-stonebraker-a2700c/4e518a3a-3b04-4185-916e-e2e962c876b7/scratchpad/m412/e2e"))
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
    return r.returncode, r.stdout, r.stderr


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
