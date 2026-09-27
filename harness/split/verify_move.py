#!/usr/bin/env python3
"""Line-multiset proof that a split only moved code.

Usage: harness/split/verify_move.py BASE_REV DIR [DIR ...]

Compares the multiset of lines of every `*.rs` under each DIR (recursively)
at BASE_REV with the working tree, after normalising `pub(crate) ` away (the
visibility a split adds). A moved line cancels out wherever it lands, so what
is left is exactly what the split ADDED or REMOVED. Each leftover line is
classified:

  scaffold    module plumbing: `//!` docs, `use` / `mod` / re-export lines and
              use-list continuations, an `impl … {` wrapper, a lone `}` / `};`,
              blank lines
  doclink     a reference-style intra-doc target (`/// [`X`]: path`) or `///`
  UNEXPECTED  anything else — a changed line of code or prose

Exit 0 when nothing is UNEXPECTED. The proof is about lines, not order or
placement: pair it with the compiler, an identical `cargo test -- --list`,
an identical rustdoc warning set, and a look at `git diff --color-moved`.
"""
import collections
import os
import re
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import splitlib  # noqa: E402

SCAFFOLD = re.compile(r"^(//!.*|(pub(\(\w+\))? )?use .*|    [\w:]+(, [\w:]+)*,?|\};|"
                      r"(pub(\(\w+\))? )?mod \w+;|(#\[cfg\(test\)\])|impl(<[^>]*>)? .*\{|\}|)$")
DOCLINK = re.compile(r"^\s*///( \[`[^`]+`\]: [\w:]+)?$")


def norm(line):
    return line.replace("pub(crate) ", "")


def main():
    if len(sys.argv) < 3:
        splitlib.die(__doc__)
    base, dirs = sys.argv[1], sys.argv[2:]
    old, new = collections.Counter(), collections.Counter()
    added_vis = 0
    for d in dirs:
        files = subprocess.check_output(["git", "ls-tree", "-r", "--name-only", base, d + "/"],
                                        text=True, cwd=splitlib.REPO).split()
        for f in (f for f in files if f.endswith(".rs")):
            text = subprocess.check_output(["git", "show", f"{base}:{f}"], text=True, cwd=splitlib.REPO)
            added_vis -= text.count("pub(crate) ")
            old.update(norm(l) for l in text.split("\n"))
        for root, _, names in os.walk(os.path.join(splitlib.REPO, d)):
            for n in (n for n in names if n.endswith(".rs")):
                text = open(os.path.join(root, n)).read()
                added_vis += text.count("pub(crate) ")
                new.update(norm(l) for l in text.split("\n"))
    bad = 0
    for sign, diff in (("+", new - old), ("-", old - new)):
        for line, count in sorted(diff.items()):
            tag = "scaffold" if SCAFFOLD.match(line) else "doclink" if DOCLINK.match(line) else "UNEXPECTED"
            bad += tag == "UNEXPECTED"
            print(f"{sign}{count:4d} [{tag}] {line!r}")
    print(f"pub(crate) added: {added_vis}")
    print("RESULT:", "move-only (scaffolding, visibility, doc-link targets)" if not bad
          else f"{bad} UNEXPECTED line kinds")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
