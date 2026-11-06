"""Shared paths for the #412 measuring tools.

M412 is the out-of-tree scratch dir (build targets, trial dirs). Override with
the M412 env var. Every build lives in its own CARGO_TARGET_DIR under
M412/targets/<name>.
"""
import os
import shutil
import subprocess
import tempfile

M412 = os.environ.get(
    "M412",
    "/private/tmp/claude-501/-Users-megurine-repo-rust-rigor-rs--claude-worktrees-nice-stonebraker-a2700c/"
    "4e518a3a-3b04-4185-916e-e2e962c876b7/scratchpad/m412",
)
HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(os.path.dirname(HERE), "data")
PROBE = os.path.join(M412, "targets", "probe", "release", "depthprobe")
TRIALS = os.path.join(M412, "e2e")
os.makedirs(TRIALS, exist_ok=True)

# Prism's error-free ceiling per capped kind (measured by `depthprobe cap`).
CAPS = {
    "array": 9999, "call": 3332, "block": 2500, "if": 3332, "paren": 9998,
    "begin": 4999, "defined": 9998, "super": 3332, "yield": 3332, "def": 5000,
    "class": 4999, "hash": 4999, "and": 3332, "ivopw": 2499, "interp": 2499,
    # beyond the #279 table
    "while": 4999, "unless": 3332, "case": 4999, "lambda": 4999, "module": 4999,
    "sclass": 4999, "doblock": 2499, "rescue": 4999, "rcall": 2499, "idx": 3332,
    "splat": 9998, "kwsplat": 4999, "regex": 3332, "ternary": 4999, "assign": 3332,
    "opassign": 3332, "pow": 4999, "bang": 9998, "not": 9998, "uminus": 9998,
    "blockparam": 2500,
}


def cli(name):
    return os.path.join(M412, "targets", name, "release", "rigor")


def gen(kind, n):
    return subprocess.run([PROBE, "gen", kind, str(n)], capture_output=True, check=True).stdout


def make_trial(kind, n, two):
    """A FRESH dir holding a.rb (the deep file) and, with `two`, a trivial b.rb
    so rayon splits the worklist and the deep file lands on a pool worker."""
    d = tempfile.mkdtemp(dir=TRIALS)
    with open(os.path.join(d, "a.rb"), "wb") as f:
        f.write(gen(kind, n))
    files = ["a.rb"]
    if two:
        with open(os.path.join(d, "b.rb"), "w") as f:
            f.write("x = 1\n")
        files.append("b.rb")
    return d, files


def rm(d):
    shutil.rmtree(d, ignore_errors=True)
