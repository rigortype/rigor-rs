#!/usr/bin/env python3
"""Give moved doc comments explicit targets for links that no longer resolve.

Usage: harness/split/doclinks.py FILE NAME=PATH [NAME=PATH ...]
  e.g. doclinks.py src/reach.rs 'CoreIndex::object_constant_class=rigor_index::CoreIndex::object_constant_class'

A shortcut intra-doc link (`[`NAME`]`) resolves against the names in scope
where the doc sits. After a move, a name only the old module imported leaves
the link broken — and importing it just for the doc trips `unused_imports`.
So for every `///` block in FILE that uses [`NAME`] as a shortcut link, this
appends a reference-style target at the end of the block:

    ///
    /// [`NAME`]: PATH

Find the NAMEs with `cargo doc -p CRATE --no-deps --document-private-items`
("unresolved link to `NAME`"), before and after the split.
"""
import re
import sys


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    path = sys.argv[1]
    targets = dict(a.split("=", 1) for a in sys.argv[2:])
    L = open(path).read().split("\n")
    out, i = [], 0
    while i < len(L):
        if not L[i].lstrip().startswith("///"):
            out.append(L[i])
            i += 1
            continue
        j = i
        while j < len(L) and L[j].lstrip().startswith("///"):
            j += 1
        block, indent = L[i:j], L[i][:len(L[i]) - len(L[i].lstrip())]
        text = "\n".join(block)
        add = [f"{indent}/// [`{n}`]: {p}" for n, p in targets.items()
               if re.search(r"\[`" + re.escape(n) + r"`\](?![(\[:])", text)
               and not re.search(r"^\s*/// \[`" + re.escape(n) + r"`\]:", text, re.M)]
        if add:
            if not re.match(r"^\s*/// \[`[^`]+`\]: ", block[-1]):
                block = block + [f"{indent}///"]
            block = block + add
            for a in add:
                print(f"{path}:{i + 1}: {a.strip()}")
        out += block
        i = j
    open(path, "w").write("\n".join(out))


if __name__ == "__main__":
    main()
