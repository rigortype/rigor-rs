#!/usr/bin/env python3
"""Import, in each test file, the names it reached through the parent's uses.

Usage: harness/split/testimports.py --crate CRATE NAME=PATH [NAME=PATH ...]
  e.g. testimports.py --crate rigor-infer Node=rigor_parse Type=rigor_types

When a split moves the last production user of a parent import, the import
turns unused in the non-test build — but a test module still reaches the name
through `use super::*`. Dropping the parent import and importing the name in
the test files that use it keeps both builds warning-free. (`fixvis.py
--prune` reports exactly these as "test-only".)

Drop the import from the parent first, then run this: it compiles the tests,
and for each file that can no longer resolve a NAME it merges NAME into that
file's top-level `use PATH::{…};` (or adds one after its leading `use` block).
"""
import argparse
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import splitlib  # noqa: E402

UNRESOLVED = {"E0425", "E0433", "E0412", "E0422", "E0531", "E0532", "E0574", "E0423"}


def add(path, prefix, names):
    L = open(path).read().split("\n")
    pat = re.compile(r"^use " + re.escape(prefix) + r"::(\{([^}]*)\}|(\w+));$")
    for i, line in enumerate(L):
        m = pat.match(line)
        if m:
            have = {x.strip() for x in (m.group(2) or m.group(3)).split(",") if x.strip()}
            L[i] = splitlib.fmt_use(prefix, have | names)
            break
    else:
        last = max(i for i, line in enumerate(L[:60]) if line.startswith("use "))
        L.insert(last + 1, splitlib.fmt_use(prefix, names))
    open(path, "w").write("\n".join(L))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--crate", required=True)
    ap.add_argument("names", nargs="+", metavar="NAME=PATH")
    a = ap.parse_args()
    paths = dict(x.split("=", 1) for x in a.names)
    for _ in range(5):
        need = {}
        for m in splitlib.cargo_messages(a.crate):
            sp = splitlib.primary_span(m)
            ns = splitlib.backticked(m["message"])
            if m["level"] == "error" and splitlib.code(m) in UNRESOLVED and sp and ns and ns[0] in paths:
                f = os.path.join(splitlib.REPO, sp["file_name"])
                need.setdefault(f, {}).setdefault(paths[ns[0]], set()).add(ns[0])
        if not need:
            return
        for f, by_prefix in sorted(need.items()):
            if os.path.basename(f) in splitlib.ROOT_FILES:
                sys.exit(f"{splitlib.rel(f)} itself needs {by_prefix}: restore that import there")
            for prefix, names in by_prefix.items():
                print(f"  {splitlib.rel(f)}: use {prefix}::{sorted(names)}")
                add(f, prefix, names)
    sys.exit("names still unresolved after 5 rounds")


if __name__ == "__main__":
    main()
