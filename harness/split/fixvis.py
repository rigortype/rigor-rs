#!/usr/bin/env python3
"""Fix imports and visibility after split_mod.py, driven by rustc's errors.

Usage: harness/split/fixvis.py --crate CRATE MODFILE [--prune]

Loops `cargo check -p CRATE --all-targets` and, until nothing changes:

  * a name MODFILE cannot resolve       -> added to MODFILE's
                                           `use crate::{…};` (or `super::`
                                           when the parent is not a crate root)
  * an item, method, field or type of MODFILE that rustc calls private
    anywhere, or that another file can no longer resolve  -> `pub(crate)`
    on its definition in MODFILE

It never picks a visibility by hand: every `pub(crate)` it adds answers an
error. With --prune it then removes the imports rustc reports unused:

  * in MODFILE — always;
  * in the parent — only an import unused in EVERY target. One unused only
    outside the test build is still reached by the tests through
    `use super::*`: it is reported, and `testimports.py` moves it into the
    test files that use it.

Anything it cannot fix is printed; the exit status is 1 while errors remain.
"""
import argparse
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import splitlib  # noqa: E402

UNRESOLVED = {"E0425", "E0412", "E0422", "E0433", "E0531", "E0532", "E0574", "E0423"}
KEYWORD = {"fn": "fn", "const": "const", "static": "static", "struct": "struct", "enum": "enum",
           "type": "type", "trait": "trait", "union": "union"}


def parent_of(modfile):
    d = os.path.dirname(modfile)
    for cand in (d + ".rs", os.path.join(d, "mod.rs"), os.path.join(d, "lib.rs"),
                 os.path.join(d, "main.rs")):
        if os.path.exists(cand) and os.path.abspath(cand) != modfile:
            return os.path.abspath(cand)
    splitlib.die(f"cannot find the file declaring {modfile}")


def defined(modfile):
    out = {}
    for r in splitlib.items(modfile):
        if r.kind in KEYWORD:
            out.setdefault(r.name, []).append(r)
    return out


def make_pub(modfile, name):
    L = open(modfile).read().split("\n")
    for r in defined(modfile).get(name, []):
        kw = KEYWORD[r.kind]
        for i in range(r.start - 1, r.end):
            m = re.match(r"^(\s*)((?:const |async |unsafe |extern \"C\" )*" + kw + r" " + re.escape(name) + r")\b", L[i])
            if m:
                if L[i].lstrip().startswith("pub"):
                    return False
                L[i] = m.group(1) + "pub(crate) " + L[i][len(m.group(1)):]
                open(modfile, "w").write("\n".join(L))
                print(f"  pub(crate) {r.kind} {name}  ({splitlib.rel(modfile)}:{i + 1})")
                return True
    return False


def make_field_pub(modfile, struct, field):
    L = open(modfile).read().split("\n")
    for r in defined(modfile).get(struct, []):
        if r.kind != "struct":
            continue
        for i in range(r.start - 1, r.end):
            m = re.match(r"^(\s+)" + re.escape(field) + r":", L[i])
            if m:
                L[i] = m.group(1) + "pub(crate) " + L[i][len(m.group(1)):]
                open(modfile, "w").write("\n".join(L))
                print(f"  pub(crate) field {struct}.{field}  ({splitlib.rel(modfile)}:{i + 1})")
                return True
    return False


USE_RE = re.compile(r"^use (crate|super)::(\{[^}]*\}|\w+);\n", re.M)


def add_imports(modfile, prefix, names):
    src = open(modfile).read()
    m = USE_RE.search(src)
    have = set()
    if m:
        have = {x.strip() for x in m.group(2).strip("{}").split(",") if x.strip()}
    line = splitlib.fmt_use(prefix, have | set(names)) + "\n"
    if m:
        src = src[:m.start()] + line + src[m.end():]
    else:
        L = src.split("\n")
        last = max(i for i, l in enumerate(L) if l.startswith("use "))
        while not L[last].rstrip().endswith(";"):
            last += 1
        L[last + 1:last + 1] = [""] + line.rstrip("\n").split("\n")
        src = "\n".join(L)
    open(modfile, "w").write(src)
    print(f"  import {prefix}::{{{', '.join(sorted(set(names) - have, key=splitlib.name_key))}}}")


def remove_import(path, name):
    """Drop `name` (a bare name or a full path) from path's top-level uses."""
    src = open(path).read()
    last = name.split("::")[-1]
    full = re.compile(r"^(use (?:[\w:]+));\n", re.M)
    for m in full.finditer(src):
        if m.group(1) == f"use {name}" or m.group(1).endswith(f"::{last}") and "::" not in name:
            src = src[:m.start()] + src[m.end():]
            open(path, "w").write(src)
            return True
    brace = re.compile(r"^use ([\w:]+)::\{([^}]*)\};\n", re.M | re.S)
    for m in brace.finditer(src):
        parts = [x.strip() for x in m.group(2).split(",") if x.strip()]
        if last in parts and (("::" not in name) or name.startswith(m.group(1) + "::")):
            parts.remove(last)
            new = (splitlib.fmt_use(m.group(1), parts) + "\n") if parts else ""
            src = src[:m.start()] + new + src[m.end():]
            open(path, "w").write(re.sub(r"\n\n\n+", "\n\n", src))
            return True
    return False


def errors(crate):
    return [m for m in splitlib.cargo_messages(crate) if m["level"] == "error"]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--crate", required=True)
    ap.add_argument("modfile")
    ap.add_argument("--prune", action="store_true")
    a = ap.parse_args()
    modfile = os.path.abspath(a.modfile)
    parent = parent_of(modfile)
    prefix = "crate" if os.path.basename(parent) in ("lib.rs", "main.rs") else "super"
    modname = os.path.basename(modfile)[:-3]

    for rnd in range(30):
        edits, want = 0, set()
        names = defined(modfile)
        for m in errors(a.crate):
            sp = splitlib.primary_span(m)
            if not sp:
                continue
            f = os.path.abspath(os.path.join(splitlib.REPO, sp["file_name"]))
            c, ns = splitlib.code(m), splitlib.backticked(m["message"])
            if not ns:
                continue
            first = ns[0].split("::")[-1]
            if c in UNRESOLVED:
                n = ns[0].split("::")[0]
                if f == modfile and n not in names:
                    want.add(n)
                elif f != modfile and n in names:
                    edits += make_pub(modfile, n)
            elif c in ("E0603", "E0624") or (c is None and " is private" in m["message"]):
                edits += make_pub(modfile, first)
            elif c in ("E0616", "E0451") and len(ns) >= 2:
                edits += make_field_pub(modfile, ns[1].split("::")[-1].split("<")[0], ns[0])
            elif c == "E0432" and first in names:
                edits += make_pub(modfile, first)
        if want:
            add_imports(modfile, prefix, want)
            edits += 1
        if not edits:
            break

    left = errors(a.crate)
    for m in left[:40]:
        sp = splitlib.primary_span(m)
        print("ERROR", splitlib.code(m), m["message"], sp and f"{sp['file_name']}:{sp['line_start']}")
    if left:
        sys.exit(1)
    if not a.prune:
        return

    for rnd in range(10):
        warns = {}   # (file, name) -> number of targets that report it unused
        for m in splitlib.cargo_messages(a.crate):
            sp = splitlib.primary_span(m)
            if m["level"] != "warning" or splitlib.code(m) != "unused_imports" or not sp:
                continue
            f = os.path.abspath(os.path.join(splitlib.REPO, sp["file_name"]))
            for n in splitlib.backticked(m["message"]):
                warns[(f, n)] = warns.get((f, n), 0) + 1
        changed = False
        for (f, n), times in sorted(warns.items()):
            if f == modfile and remove_import(f, n):
                print(f"  prune {n}")
                changed = True
            elif f == parent and n == f"{modname}::*":
                src = open(f).read().replace(f"pub(crate) use {modname}::*;\n", "", 1)
                open(f, "w").write(src)
                print(f"  drop unused re-export {modname}::* from {splitlib.rel(f)}")
                changed = True
            elif f == parent and times >= 2 and remove_import(f, n):
                print(f"  prune {n} from {splitlib.rel(f)} (unused in every target)")
                changed = True
        if not changed:
            break
    rest = [m for m in splitlib.cargo_messages(a.crate) if m["level"] == "warning"]
    for m in rest:
        sp = splitlib.primary_span(m)
        hint = ""
        if splitlib.code(m) == "unused_imports" and sp and os.path.abspath(
                os.path.join(splitlib.REPO, sp["file_name"])) == parent:
            hint = "  <- test-only: move into the tests with testimports.py"
        print("WARN", splitlib.code(m), m["message"], sp and f"{sp['file_name']}:{sp['line_start']}", hint)
    # keep the crate import rustfmt-shaped
    src = open(modfile).read()
    m = USE_RE.search(src)
    if m and "{" in m.group(2):
        names = [x.strip() for x in m.group(2).strip("{}").split(",") if x.strip()]
        open(modfile, "w").write(src[:m.start()] + splitlib.fmt_use(m.group(1), names) + "\n" + src[m.end():])


if __name__ == "__main__":
    main()
