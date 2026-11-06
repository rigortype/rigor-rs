#!/usr/bin/env python3
"""Prototype edit: wrap a recursive fn in `maybe_grow` by renaming it to
`<name>__g` and adding a same-signature shim that calls it inside
`rigor_parse::stack::maybe_grow` (or `crate::stack::` inside rigor-parse).

Handles methods (`&self` / `&mut self`) and free fns with simple `name: Type`
params. Prints the shim it inserted.

Usage: wrapfn.py FILE FN_NAME [GROW_PATH]
"""
import re
import sys

path, name = sys.argv[1], sys.argv[2]
grow = sys.argv[3] if len(sys.argv) > 3 else "rigor_parse::stack::maybe_grow"
src = open(path).read()
m = re.search(r"\n(?P<indent>[ \t]*)(?P<attrs>(?:#\[[^\n]*\]\n[ \t]*)*)(?P<vis>pub(?:\([^)]*\))? )?fn %s(?P<gen><[^(]*>)?\(" % re.escape(name), src)
assert m, "fn not found"
# find the matching ')' of the param list
i = m.end()
depth = 1
while depth:
    c = src[i]
    depth += {"(": 1, ")": -1}.get(c, 0)
    i += 1
params = src[m.end():i - 1]
# signature tail up to the body's '{'
j = src.index("{", i)
tail = src[i:j]
# param names (skip self)
names = []
depth = 0
cur = ""
for c in params + ",":
    if c in "<([" :
        depth += 1
    elif c in ">)]":
        depth -= 1
    if c == "," and depth == 0:
        p = cur.strip()
        cur = ""
        if not p or p.split(":")[0].strip() in ("self", "&self", "&mut self", "mut self", "&'_ self"):
            continue
        names.append(re.sub(r"^mut\s+", "", p.split(":")[0].strip()))
    else:
        cur += c
is_method = params.split(",")[0].strip() in ("self", "&self", "&mut self", "mut self")
# refuse to wrap a shim twice
assert "%s__g" % name not in src, "already wrapped"
call = ("self." if is_method else "") + name + "__g(" + ", ".join(names) + ")"
indent = m.group("indent")
shim = "\n%s%sfn %s%s(%s)%s{\n%s    %s(|| %s)\n%s}\n" % (
    indent, m.group("vis") or "", name, m.group("gen") or "", params, tail, indent, grow, call, indent)
renamed = src[:m.start()] + shim + src[m.start():m.end()].replace("fn %s" % name, "fn %s__g" % name, 1) + src[m.end():]
open(path, "w").write(renamed)
print(shim)
