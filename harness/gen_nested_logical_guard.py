#!/usr/bin/env python3
"""Write the nested alternating &&/|| guard of rigor-rs#453.

`subject_edge` once evaluated a Logical's left operand twice on the
non-sequential edge, with no memo. Deep alternating nesting therefore grew
about 1.6× per term. Measured on master `1c2594a` (release `check`):

    26 terms  0.47 s
    30 terms  2.18 s
    34 terms 14.05 s

The generator is `if ((((ENV.key?("k0") && x1) || x2) && x3) …)\\n  1\\nend`.

Usage: gen_nested_logical_guard.py TERMS [OUT]  (OUT defaults to stdout)

Measure with `/usr/bin/time -l` (macOS) or `/usr/bin/time -v` (Linux) around
`target/release/rigor check OUT`. Acceptance (release): 34 terms under 0.5 s,
68 terms under 1 s.
"""

import sys


def generate(terms: int) -> str:
    if terms < 1:
        raise ValueError("TERMS must be >= 1")
    expr = 'ENV.key?("k0")'
    for i in range(1, terms):
        op = "&&" if i % 2 == 1 else "||"
        expr = f"({expr} {op} x{i})"
    return f"if {expr}\n  1\nend\n"


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
