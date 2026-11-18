#!/usr/bin/env python3
"""Write the mixin-closure stress files of rigor-rs#464.

`object_mixin_may_define` once re-walked the whole top-level-include closure —
and allocated a `resolve_override_ancestor` candidate string per edge — for
EVERY `undefined-method` / `unresolved-toplevel` diagnostic. This generator
reproduces the two shapes the issue reports:

* `--include` (the verified case): MODS modules, each including the next SPAN,
  one top-level `include W0`, then CALLS `1.zorkN` receiver calls plus CALLS
  bare `zorkbareN` calls. Master took ~1 s on 90/30/5000; the un-memoised
  build took ~6.6 s.
* `--no-include` (the unverified 283 s case): MODS modules with no top-level
  `include`, scaled to CALLS diagnostics per side. The include closure is
  empty here, so whatever this shape measures is NOT the mixin walk — it is
  the per-diagnostic baseline.

Usage: gen_object_mixin_perf.py MODS SPAN CALLS [--include|--no-include] [OUT]
       (OUT defaults to stdout; --include is the default)

Measure with `/usr/bin/time -l` (macOS) or `/usr/bin/time -v` (Linux) around
`target/release/rigor check OUT`, and diff the output against the other build.
"""

import sys


def generate(mods: int, span: int, calls: int, include: bool) -> str:
    lines = []
    for i in range(mods):
        incs = ", ".join(f"W{j}" for j in range(i + 1, min(i + 1 + span, mods)))
        lines.append(f"module W{i}\n  include {incs}\nend" if incs else f"module W{i}\nend")
    if include:
        lines.append("include W0")
    for i in range(calls):
        lines.append(f"1.zork{i}")
    for i in range(calls):
        lines.append(f"zorkbare{i}")
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    args = [a for a in argv[1:] if not a.startswith("--")]
    include = "--no-include" not in argv
    if len(args) < 3:
        return print(__doc__) or 1
    mods, span, calls = (int(a) for a in args[:3])
    out = generate(mods, span, calls, include)
    if len(args) > 3:
        with open(args[3], "w") as f:
            f.write(out)
    else:
        sys.stdout.write(out)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
