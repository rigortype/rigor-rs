#!/usr/bin/env python3
"""Write the sub-cap `local_reach` W^4 band of rigor-rs#441.

One local self-rewritten W times inside a loop (`p = p + 1 if p > K`,
K = 0..W-1), with a literal-index call reading it in the loop body. The
`"abc"[p].zz` read INSIDE the `while` is what makes the writes reach a
diagnostic-driving `local_reach`; #440's `onevar` variant ended in a
bare `p` after the loop and never exercised the band. The port declines
the `.zz` itself on every measured commit (reference fires
`undefined method 'zz'`), so the row is timing-only.

Measured on release `check` (the #441 PR head vs master `5a6eb0da`):

    W = 25   50.9 s -> 0.25 s   (write-loop absorption)
    W = 49   46 m   -> 0.26 s
    regexp_parser 2.12.0 scanner.rb (W ~ 79)   19.0 s -> 0.56 s

Usage: gen_onevar_band.py W [OUT]  (OUT defaults to stdout)

Measure with `/usr/bin/time -l` (macOS) or `/usr/bin/time -v` (Linux)
around `target/release/rigor check OUT`. With the #441 absorption a
joined reach saturates to `(UNKNOWN, Some(None))` a few writes in, so
any W under the 256 fan-out cap should check in well under a second.
"""

import sys


def generate(w: int) -> str:
    if w < 1:
        raise ValueError("W must be >= 1")
    lines = [
        "def f(c, n)",
        "  p = 0",
        "  while c",
        *(f"    p = p + 1 if p > {k}" for k in range(w)),
        '    "abc"[p].zz',
        "  end",
        "end",
    ]
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    if len(argv) not in (2, 3):
        print(next(l for l in __doc__.splitlines() if l.startswith("Usage:")), file=sys.stderr)
        return 2
    src = generate(int(argv[1]))
    if len(argv) == 3:
        with open(argv[2], "w", encoding="utf-8") as f:
            f.write(src)
    else:
        sys.stdout.write(src)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
