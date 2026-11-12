#!/usr/bin/env python3
"""Write the D defs x K ivar-guard-pairs stress class of rigor-rs#438.

One class holds an `initialize` that writes `@x`/`@y`, then D defs. Each def
holds K pairs of an ivar truthiness guard (`if @x then a = a + k end`) and an
ivar-guarded call (`Float(@y).w if @y`), so the file has about 3*D*K ivar
reads inside one class region with D+1 defs. The writes matter: without them
the ivar reach is trivial and the shape stops being pathological. Every
`Float(@y).w` is a diagnostic, so a before/after diff compares real output.
`ReachIndex::class_ivar_scope` once kept a copy of the region's def list per
read span; this shape makes that cost visible in max RSS.

Usage: gen_ivar_scope_stress.py D K [OUT]  (OUT defaults to stdout)

Measure with `/usr/bin/time -l` (macOS) or `/usr/bin/time -v` (Linux) around
`target/release/rigor check OUT`, and diff the output against the other build.
"""

import sys


def generate(defs: int, pairs: int) -> str:
    lines = [
        "class IvarScopeStress",
        "  def initialize",
        "    @x = nil",
        '    @y = "1.5"',
        "  end",
    ]
    for d in range(defs):
        lines.append(f"  def m{d}")
        lines.append("    a = 0")
        for k in range(pairs):
            lines.append(f"    if @x then a = a + {k} end")
            lines.append("    Float(@y).w if @y")
        lines.append("    a")
        lines.append("  end")
    lines.append("end")
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    if len(argv) not in (3, 4):
        print(next(l for l in __doc__.splitlines() if l.startswith("Usage:")), file=sys.stderr)
        return 2
    src = generate(int(argv[1]), int(argv[2]))
    if len(argv) == 4:
        with open(argv[3], "w", encoding="utf-8") as f:
            f.write(src)
    else:
        sys.stdout.write(src)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
