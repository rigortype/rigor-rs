#!/usr/bin/env python3
"""Time the pathological nesting shapes of rigor-rs#424.

The issue's table is super-linear time on deep but uncapped chains. This
script regenerates it against a release `rigor` binary (default
`target/release/rigor`). Data stays out of the repo.

Default is the `**` row the first PR of #424 lands (800 and 1600 links).
`--all` adds the call / `+` / constant-path rows, the local-read residuals,
the modifier-`if` row, and the nested-`class` row.

Usage:
  python3 harness/time_nesting.py
  python3 harness/time_nesting.py --all
  python3 harness/time_nesting.py --bin target/release/rigor --timeout 120
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# (kind, sizes). Default is `**` only; `--all` uses the rest too.
POW = ("pow", [800, 1600])
# The nested-`class` row (#424's "4,999 nested class/module" shape). The
# lowering refuses nesting past 5,000 (a deterministic depth refusal, exit
# 1), so 2,000 is the largest size every build must complete.
NESTCLASS = ("nestclass", [1_000, 2_000])
OTHERS = [
    ("call", [10_000, 20_000]),
    ("plus", [10_000, 20_000]),
    ("const", [10_000, 20_000]),
    ("vpow", [800, 1_600]),
    ("vplus", [10_000, 20_000]),
    ("xcall", [10_000, 20_000]),
    ("modif", [5_000, 10_000]),
    NESTCLASS,
]


def generate(kind: str, n: int) -> str:
    if kind == "pow":
        return " ** ".join(["2"] * n) + "\n"
    if kind == "call":
        return "x" + ".a" * n + "\n"
    if kind == "plus":
        return "+".join(["1"] * n) + "\n"
    if kind == "const":
        return "::".join(["A"] * n) + "\n"
    if kind == "vpow":
        return "v = 2\n" + " ** ".join(["v"] * n) + "\n"
    if kind == "vplus":
        return "v = 1\n" + "+".join(["v"] * n) + "\n"
    if kind == "xcall":
        return "x = foo\n" + "x" + ".a" * n + "\n"
    if kind == "modif":
        return "1" + " if x" * n + "\n"
    if kind == "nestclass":
        # #424's "4,999 nested class/module" row: `class C0; class C1; …`
        # D deep. Qualified names grow with depth, so a build that rejoins
        # per-class prefixes shows up super-linearly here.
        return "".join(f"class C{i}\n" for i in range(n)) + "end\n" * n
    raise SystemExit(f"unknown kind {kind!r}")


def time_one(rigor: Path, src: str, timeout: float) -> tuple[float, int]:
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "t.rb"
        path.write_text(src)
        t0 = time.perf_counter()
        try:
            p = subprocess.run(
                [str(rigor), "check", str(path)],
                cwd=tmp,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=timeout,
            )
        except subprocess.TimeoutExpired:
            return timeout, -1
        return time.perf_counter() - t0, p.returncode


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--bin",
        default=str(ROOT / "target/release/rigor"),
        help="release rigor binary (default: target/release/rigor)",
    )
    ap.add_argument("--timeout", type=float, default=120.0)
    ap.add_argument(
        "--all",
        action="store_true",
        help="also time call / + / const / local-read / modifier-if / nested-class rows",
    )
    args = ap.parse_args(argv[1:])
    rigor = Path(args.bin)
    if not rigor.is_file():
        print(f"missing binary: {rigor} (cargo build --release)", file=sys.stderr)
        return 2
    rows = [POW] + (OTHERS if args.all else [])
    print("| shape | size → time | size → time | growth |")
    print("|---|---|---|---|")
    for kind, sizes in rows:
        times = []
        cells = []
        for n in sizes:
            dt, rc = time_one(rigor, generate(kind, n), args.timeout)
            times.append(dt)
            tag = "TIMEOUT" if rc == -1 else f"{dt:.2f} s"
            cells.append(f"{n} → {tag}")
        growth = "—"
        if times[0] > 0 and times[-1] >= 0 and times[0] < args.timeout:
            growth = f"×{times[-1] / times[0]:.2f}"
        print(f"| {kind} | {cells[0]} | {cells[1]} | {growth} |")
        sys.stdout.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
