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
error. With --prune it then removes the imports rustc reports unused, in
MODFILE always, and in the parent only when EVERY target reports them (the
glob re-export `pub(crate) use MOD::*;` included). An import unused only
outside the test build is still reached by the tests through `use super::*`:
it is reported as test-only, and `testimports.py` moves it into the test
files that use it.

Only top-level `use` items are edited, located by the syn lister (and, for
the glob, the exact `pub(crate) use MOD::*;` line split_mod wrote), so string
literals and code are never touched.
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


def read_lines(path):
    return open(path).read().split("\n")


def write_lines(path, L):
    open(path, "w").write("\n".join(L))


def defined(modfile):
    out = {}
    for r in splitlib.items(modfile):
        if r.kind in KEYWORD:
            out.setdefault(r.name, []).append(r)
    return out


def make_pub(modfile, name):
    L = read_lines(modfile)
    for r in defined(modfile).get(name, []):
        if r.vis != "-":
            continue
        kw = KEYWORD[r.kind]
        for i in range(r.start - 1, r.end):
            m = re.match(r"^(\s*)(?:(?:const|async|unsafe|extern \"C\") )*" + kw + r" " + re.escape(name) + r"\b",
                         L[i])
            if m:
                L[i] = m.group(1) + "pub(crate) " + L[i][len(m.group(1)):]
                write_lines(modfile, L)
                print(f"  pub(crate) {r.kind} {name}  ({splitlib.rel(modfile)}:{i + 1})")
                return True
    return False


def make_field_pub(modfile, struct, field):
    """`pub(crate)` on a named field (`name: T`) or, for a tuple struct, on
    the positional field `field` (`0`, `1`, …) of `struct S(A, B);`."""
    L = read_lines(modfile)
    for r in defined(modfile).get(struct, []):
        if r.kind != "struct":
            continue
        if field.isdigit():
            text = "\n".join(L[r.start - 1:r.end])
            m = re.search(r"\bstruct " + re.escape(struct) + r"\b[^(;{]*\(", text)
            if not m:
                continue
            depth, k, i = 0, 0, m.end()
            starts = [i]
            while i < len(text):
                ch = text[i]
                if ch in "(<[":
                    depth += 1
                elif ch in ")>]":
                    if depth == 0:
                        break
                    depth -= 1
                elif ch == "," and depth == 0:
                    starts.append(i + 1)
                i += 1
            if int(field) >= len(starts):
                continue
            at = starts[int(field)]
            while text[at] in " \n":
                at += 1
            if text[at:].startswith("pub"):
                return False
            text = text[:at] + "pub(crate) " + text[at:]
            L[r.start - 1:r.end] = text.split("\n")
            write_lines(modfile, L)
            print(f"  pub(crate) field {struct}.{field}  ({splitlib.rel(modfile)}:{r.start})")
            return True
        for i in range(r.start - 1, r.end):
            m = re.match(r"^(\s+)" + re.escape(field) + r":", L[i])
            if m:
                L[i] = m.group(1) + "pub(crate) " + L[i][len(m.group(1)):]
                write_lines(modfile, L)
                print(f"  pub(crate) field {struct}.{field}  ({splitlib.rel(modfile)}:{i + 1})")
                return True
    return False


def replace_use(path, start, end, new_text):
    """Replace a top-level use item's lines; when it is deleted and leaves a
    doubled blank line behind, drop one of the two blanks."""
    L = read_lines(path)
    new = new_text.split("\n") if new_text else []
    L[start - 1:end] = new
    if not new:
        i = start - 1
        if 0 < i < len(L) and L[i] == "" and L[i - 1] == "":
            del L[i]
    write_lines(path, L)


def find_use(path, prefix):
    """The first private top-level `use PREFIX::…;` item, parsed."""
    L = read_lines(path)
    for s, e, vis in splitlib.use_items(path):
        p = splitlib.parse_use("\n".join(L[s - 1:e]))
        if vis == "-" and p and p[0] == prefix:
            return s, e, p[1]
    return None


def add_imports(modfile, prefix, names):
    hit = find_use(modfile, prefix)
    if hit:
        s, e, have = hit
        replace_use(modfile, s, e, splitlib.fmt_use(prefix, set(have) | set(names)))
    else:
        L = read_lines(modfile)
        uses = splitlib.use_items(modfile)
        at = uses[-1][1] if uses else next((i for i, l in enumerate(L) if not l.startswith("//!")), 0)
        L[at:at] = [""] + splitlib.fmt_use(prefix, names).split("\n")
        write_lines(modfile, L)
        have = []
    added = sorted(set(names) - set(have), key=splitlib.name_key)
    print(f"  import {prefix}::" + (added[0] if len(added) == 1 else "{" + ", ".join(added) + "}"))


def remove_import(path, name, line=None):
    """Drop `name` (a bare name or a full path, as rustc words it) from the
    private top-level use item on `line` (the warning's line; any item holding
    the name when `line` is None). False when no such item holds it."""
    L = read_lines(path)
    last = name.split("::")[-1]
    for s, e, vis in splitlib.use_items(path):
        if vis != "-" or (line is not None and not s <= line <= e):
            continue
        p = splitlib.parse_use("\n".join(L[s - 1:e]))
        if not p or last not in p[1]:
            continue
        prefix, names = p
        if "::" in name and f"{prefix}::{last}" != name and not (prefix is None and name == last):
            continue
        rest = [n for n in names if n != last]
        replace_use(path, s, e, splitlib.fmt_use(prefix, rest) if rest else "")
        return True
    return False


def errors(crate):
    return [m for m in splitlib.cargo_messages(crate) if m["level"] == "error"]


def fix(crate, modfile, prefix):
    for _ in range(30):
        edits, want = 0, set()
        names = defined(modfile)
        for m in splitlib.cargo_messages(crate):
            if m["level"] == "warning" and splitlib.code(m) == "private_interfaces":
                # `pub(crate) fn f() -> T` with T private to MODFILE
                ns = splitlib.backticked(m["message"])
                if ns and ns[0] in names:
                    edits += make_pub(modfile, ns[0])
                continue
            if m["level"] != "error":
                continue
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
            return


def prune(crate, modfile, parent, modname):
    for _ in range(10):
        warns = {}   # (file, name, line) -> number of targets reporting it unused
        for m in splitlib.cargo_messages(crate):
            if m["level"] != "warning" or splitlib.code(m) != "unused_imports" or not m["spans"]:
                continue
            names = splitlib.backticked(m["message"])
            # rustc gives one span per unused name, in the message's order
            spans = m["spans"] if len(m["spans"]) == len(names) else [splitlib.primary_span(m)] * len(names)
            for n, sp in zip(names, spans):
                f = os.path.abspath(os.path.join(splitlib.REPO, sp["file_name"]))
                key = (f, n, sp["line_start"])
                warns[key] = warns.get(key, 0) + 1
        changed = False
        for (f, n, line), times in sorted(warns.items()):
            if f == modfile and times >= 2 and remove_import(f, n, line):
                print(f"  prune {n}")
                changed = True
            elif f == parent and times >= 2 and n == f"{modname}::*":
                L = read_lines(f)
                if f"pub(crate) use {modname}::*;" in L:
                    L.remove(f"pub(crate) use {modname}::*;")
                    write_lines(f, L)
                    print(f"  drop unused re-export {modname}::* from {splitlib.rel(f)}")
                    changed = True
            elif f == parent and times >= 2 and remove_import(f, n, line):
                print(f"  prune {n} from {splitlib.rel(f)} (unused in every target)")
                changed = True
        if not changed:
            return


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

    fix(a.crate, modfile, prefix)
    if a.prune and not errors(a.crate):
        prune(a.crate, modfile, parent, modname)
    msgs = splitlib.cargo_messages(a.crate)
    for m in msgs:
        if m["level"] not in ("error", "warning"):
            continue
        sp = splitlib.primary_span(m)
        hint = ""
        if splitlib.code(m) == "unused_imports" and sp and os.path.abspath(
                os.path.join(splitlib.REPO, sp["file_name"])) == parent:
            hint = "  <- unused in one build only: if the tests need it, move it with testimports.py"
        elif splitlib.code(m) == "unused_imports" and sp and os.path.abspath(
                os.path.join(splitlib.REPO, sp["file_name"])) == modfile:
            hint = "  <- unused in one build only (cfg-gated code?): gate the import or the module by hand"
        print(m["level"].upper(), splitlib.code(m), m["message"],
              sp and f"{sp['file_name']}:{sp['line_start']}", hint)
    if any(m["level"] == "error" for m in msgs):
        sys.exit(1)


if __name__ == "__main__":
    main()
