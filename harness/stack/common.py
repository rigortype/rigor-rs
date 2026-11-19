"""Shared paths and helpers for the #412 stack-measurement tools (rigor-rs#428).

Nothing here writes into the repo: generated sources, trial dirs, TSV logs
and the depthprobe build all live under RIGOR_STACK_DATA (default
`${TMPDIR}/rigor-stack-data`). Regenerate the data; do not commit it.

The rigor binary is always an explicit path argument (`release` / `debug`
expand to `target/<name>/rigor`). depthprobe is built on first use into
RIGOR_STACK_DATA/target with the pinned toolchain (`RIGOR_STACK_CARGO`,
default `cargo +1.88.0`; override to plain `cargo` where rustup is absent).
"""
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
DATA = os.environ.get("RIGOR_STACK_DATA") or os.path.join(
    tempfile.gettempdir(), "rigor-stack-data")
TRIALS = os.path.join(DATA, "trials")
PROBE_SRC = os.path.join(HERE, "depthprobe")
PROBE = os.environ.get("RIGOR_STACK_PROBE") or os.path.join(
    DATA, "target", "release", "depthprobe")
CARGO = os.environ.get("RIGOR_STACK_CARGO", "cargo +1.88.0").split()
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

# The screen's uncapped shapes (chains Prism parses iteratively, no cap), the
# same chains nested inside a scope, chains that walk Prism typed fields, and
# capped shapes whose cap is not in CAPS yet (caps.py measures it first).
UNCAPPED = ["chain", "plus", "minus", "andand", "oror", "kwand", "orkw", "index",
            "safenav", "argchain", "blkchain", "callblk", "cpath", "shovel", "lt",
            "ifmod", "unlessmod", "rescmod", "whilemod", "strcat"]
UNCAPPED_IN_SCOPE = ["defchain", "defretchain", "blkinchain", "classchain",
                     "asgnchain", "ivchain", "ifchain", "defplus", "deforor",
                     "defifmod"]
TYPED_FIELD = ["rescuechain", "elsifchain", "whenchain"]
# Capped shapes outside the #279 table CAPS carries: caps.py measures them.
MIXED = ["ret", "masgn", "arrparen", "bangparen", "splatparen", "parenbegin",
         "defparen", "lambdaparen", "interpparen", "andparen", "ifparen",
         "notparen"]

# The bound's stack budget (ADR-0045): one level per link, so KIND:24990 sits
# just under MAX_NESTING_DEPTH except ifmod (two edges per level -> 12,490).
BOUND = ["elsifchain:24990", "ifmod:12490", "unlessmod:12490", "rescuechain:24990",
         "andand:24990", "chain:24990", "whenchain:24990"]

# ~80 shapes: the 37 capped + 20 uncapped + 10 in-scope + 3 typed-field + 12
# mixed. e2e.py screens the first four sets (capped kinds default to CAPS);
# MIXED kinds need an explicit KIND:N from a measured cap.
SCREEN_KINDS = (list(CAPS) + UNCAPPED + UNCAPPED_IN_SCOPE + TYPED_FIELD
                + MIXED)


def ensure_probe():
    """The depthprobe binary, building it on first use."""
    if os.environ.get("RIGOR_STACK_PROBE"):
        return PROBE
    if not os.path.exists(PROBE):
        env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(DATA, "target"))
        subprocess.run(CARGO + ["build", "--release", "--locked"], cwd=PROBE_SRC,
                       env=env, check=True)
    return PROBE


def cli(name):
    """Resolve a rigor binary argument: `release`/`debug` -> target/<name>/rigor,
    any other value is a path (relative to the caller's cwd)."""
    if name in ("release", "debug"):
        return os.path.join(ROOT, "target", name, "rigor")
    return name if os.path.isabs(name) else os.path.join(os.getcwd(), name)


def gen(kind, n):
    return subprocess.run([ensure_probe(), "gen", kind, str(n)],
                          capture_output=True, check=True).stdout


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
