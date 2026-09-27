#!/usr/bin/env python3
"""Line-multiset proof that a split only moved code.

Usage: harness/split/verify_move.py BASE_REV DIR [DIR ...]

Every line of every `*.rs` under each DIR (recursively) is tagged by its
syntactic place, using the syn lister, at BASE_REV and in the working tree:

  use         a line of a top-level `use` item
  mod         a line of a top-level body-less `mod NAME;` item (attributes
              such as `#[cfg(test)]` included)
  impl-frame  a top-level `impl` block's header lines or its closing `}`
  inner-doc   a `//!` line of the file header
  blank       an empty line outside any literal
  literal     a line inside a multi-line literal
  code        anything else

`pub(crate) ` is normalised away (the visibility a split adds), and the
multisets of (line, tag) are compared. A moved line cancels out wherever it
lands; what is left is what the split added or removed:

  scaffold    use / mod / impl-frame / inner-doc / blank rows
  doclink     a `code` row that is a `///` or a reference-style intra-doc
              target (`/// [`X`]: path`)
  UNEXPECTED  every other row: code, prose, or a literal's interior

Exit 0 when nothing is UNEXPECTED. The limits:

  * A scaffold row is accepted by its PLACE, not its meaning. A changed
    `use` line can point a name at a different item, so read every printed
    row; do not just trust the exit code.
  * Order is not compared: two moved lines swapping places leave no row.
    `git diff --color-moved` and the compiler (plus an identical
    `cargo test -- --list` and rustdoc warning set) cover what this cannot.
"""
import collections
import os
import re
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import splitlib  # noqa: E402

SCAFFOLD_TAGS = {"use", "mod", "impl-frame", "inner-doc", "blank"}
DOCLINK = re.compile(r"^\s*///( \[`[^`]+`\]: [\w:]+)?$")


def tag_lines(path):
    """(normalised line, tag) for every line of the file at `path`."""
    L = open(path).read().split("\n")
    tags = ["code"] * len(L)
    for i, line in enumerate(L):
        if line == "":
            tags[i] = "blank"
    for i, line in enumerate(L):
        if not line.startswith("//!"):
            break
        tags[i] = "inner-doc"
    for r in splitlib.items(path):
        if r.depth != 0:
            continue
        span = range(r.start - 1, r.end)
        if r.kind == "use":
            for i in span:
                tags[i] = "use"
        elif r.kind == "mod" and L[r.end - 1].rstrip().endswith(";"):
            for i in span:
                tags[i] = "mod"
        elif r.kind == "impl":
            hdr = next(i for i in span if L[i].rstrip().endswith("{")
                       and not L[i].lstrip().startswith(("//", "#")))
            j = hdr
            while j >= r.start - 1 and not L[j].lstrip().startswith(("//", "#")) and L[j] != "":
                tags[j] = "impl-frame"
                j -= 1
            if L[r.end - 1] == "}":
                tags[r.end - 1] = "impl-frame"
    for n in splitlib.literal_interior(path):
        tags[n - 1] = "literal"
    return [(l.replace("pub(crate) ", ""), t) for l, t in zip(L, tags)]


def main():
    if len(sys.argv) < 3:
        splitlib.die(__doc__)
    base, dirs = sys.argv[1], sys.argv[2:]
    old, new = collections.Counter(), collections.Counter()
    vis = 0
    with tempfile.TemporaryDirectory() as tmp:
        for d in dirs:
            files = subprocess.check_output(["git", "ls-tree", "-r", "--name-only", base, d + "/"],
                                            text=True, cwd=splitlib.REPO).split()
            for k, f in enumerate(f for f in files if f.endswith(".rs")):
                text = subprocess.check_output(["git", "show", f"{base}:{f}"], text=True, cwd=splitlib.REPO)
                p = os.path.join(tmp, f"{k}.rs")
                open(p, "w").write(text)
                vis -= text.count("pub(crate) ")
                old.update(tag_lines(p))
            for root, _, names in os.walk(os.path.join(splitlib.REPO, d)):
                for n in (n for n in names if n.endswith(".rs")):
                    p = os.path.join(root, n)
                    vis += open(p).read().count("pub(crate) ")
                    new.update(tag_lines(p))
    bad = 0
    for sign, diff in (("+", new - old), ("-", old - new)):
        for (line, tag), count in sorted(diff.items()):
            kind = ("scaffold" if tag in SCAFFOLD_TAGS
                    else "doclink" if tag == "code" and DOCLINK.match(line) else "UNEXPECTED")
            bad += kind == "UNEXPECTED"
            print(f"{sign}{count:4d} [{kind}:{tag}] {line!r}")
    print(f"pub(crate) added: {vis}")
    print("RESULT:", "move-only (scaffolding, visibility, doc-link targets) — read the scaffold rows"
          if not bad else f"{bad} UNEXPECTED line kinds")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
