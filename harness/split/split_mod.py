#!/usr/bin/env python3
"""Move selected items of a Rust file into a new child module, verbatim.

Usage: harness/split/split_mod.py FILE MOD SELECTION DOC

SELECTION is a file with one selector per line (`#` comments allowed):

  NAME        the top-level fn / const / static / struct / enum / type /
              trait / union named NAME
  impl:TYPE   a whole top-level inherent `impl` block, TYPE as rsitems prints
              it (`Typer<'i>`)
  TYPE::NAME  one method (or assoc const/type) of an inherent `impl TYPE`,
              TYPE without generics (`Typer::type_of`)

DOC is a file holding the new module's `//!` header.

Each moved item takes the gap above it (blank lines, `//` comments, a section
banner) with it, and items keep their original relative order. Moved methods
are wrapped in a copy of their source block's `impl` header, one wrapper per
source block. The new file gets FILE's private `use` lines (prune them with
`fixvis.py --prune`), and FILE gets `mod MOD;`, `pub(crate) use MOD::*;` and a
`pub use` for every moved `pub` item.

This only moves text. Run `fixvis.py` next (imports and visibility), then
`verify_move.py` (the line-multiset proof). Non-blank gap lines that travel
are printed: check that no banner meant for the items left behind moved.
"""
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import splitlib  # noqa: E402

ITEM_KINDS = ("fn", "const", "static", "struct", "enum", "type", "trait", "union")

def main():
    if len(sys.argv) != 5:
        splitlib.die(__doc__)
    path, mod, selfile, docfile = sys.argv[1:]
    target = os.path.join(splitlib.child_dir(path), f"{mod}.rs")
    if os.path.exists(target):
        splitlib.die(f"{target} exists; refusing to overwrite")
    src = open(path).read()
    L = src[:-1].split("\n") if src.endswith("\n") else src.split("\n")
    rows = splitlib.items(path)
    top = [r for r in rows if r.depth == 0]
    impls = {r.start: r for r in top if r.kind == "impl"}

    sel = [l.strip() for l in open(selfile) if l.strip() and not l.lstrip().startswith("#")]
    found = set()

    def extents(seq, first):
        prev, out = first - 1, []
        for r in seq:
            out.append((prev + 1, r))
            prev = r.end
        return out

    moved_top = []                      # (gap_start, item)
    for gs, r in extents(top, splitlib.header_end(L) + 1):
        key = f"impl:{r.name}" if r.kind == "impl" else r.name
        if key in sel and (r.kind == "impl" or r.kind in ITEM_KINDS):
            if key in found:
                splitlib.die(f"selector {key!r} matches more than one item")
            found.add(key)
            moved_top.append((gs, r))
    moved_meth = {}                     # impl start -> [(gap_start, item)]
    for istart, imp in impls.items():
        if "for" in imp.name.replace("<", " ").split() or f"impl:{imp.name}" in found:
            continue
        bare = re.sub(r"<.*", "", imp.name)
        kids = [r for r in rows if r.depth == 1 and r.parent == istart]
        for gs, r in extents(kids, imp.brace + 1):
            key = f"{bare}::{r.name}"
            if key in sel:
                found.add(key)
                moved_meth.setdefault(istart, []).append((gs, r))
    for istart, ms in moved_meth.items():
        kids = [r for r in rows if r.depth == 1 and r.parent == istart]
        if len(ms) == len(kids):
            name = impls[istart].name
            twins = sum(1 for i in impls.values() if i.name == name)
            hint = (f"select impl:{name}" if twins == 1 else
                    f"this file has {twins} `impl {name}` blocks, so impl:{name} is ambiguous: "
                    "merge them first (a prep commit), then select impl:" + name)
            splitlib.die(f"every item of impl {name} (line {istart}) is selected, "
                         f"which would leave an empty impl behind: {hint}")
    missing = [s for s in sel if s not in found]
    if missing:
        splitlib.die(f"no item for: {missing}")

    drop = set()
    for gs, r in moved_top + [m for ms in moved_meth.values() for m in ms]:
        drop.update(range(gs, r.end + 1))
        gap = [l for l in L[gs - 1:r.start - 1] if l.strip()]
        if gap:
            print(f"-- gap moving with {r.name} ({gs}-{r.start - 1}):")
            for l in gap:
                print("   " + l)

    def chunk(gs, end, strip_leading_blank):
        ls = L[gs - 1:end]
        while strip_leading_blank and ls and ls[0] == "":
            ls = ls[1:]
        return ls

    # The new module: doc, uses, then every moved piece in source order.
    uses, prev_end = [], None
    for r in top:
        if r.kind == "use" and not L[r.start - 1].lstrip().startswith("pub"):
            if uses and prev_end is not None and r.start > prev_end + 1:
                uses.append("")
            uses += L[r.start - 1:r.end]
            prev_end = r.end
    pieces = [(r.start, chunk(gs, r.end, True)) for gs, r in moved_top]
    for istart, ms in moved_meth.items():
        imp = impls[istart]
        if imp.brace == imp.end:
            splitlib.die(f"impl {imp.name} (line {imp.start}) opens and closes on one line; "
                         "move it whole with impl:TYPE")
        head = L[imp.kw - 1:imp.brace]
        if not head[-1].rstrip().endswith("{"):
            splitlib.die(f"impl {imp.name}: text after its opening brace on line {imp.brace}; "
                         "move it whole with impl:TYPE, or split that line first")
        # Outer attributes (`#[cfg(test)]`, `#[allow(…)]`) govern every method
        # in the block, so each wrapper carries a copy; docs stay behind.
        attrs = [L[i - 1] for i in splitlib.impl_attr_lines(L, imp)]
        body = []
        for n, (gs, r) in enumerate(ms):
            body += chunk(gs, r.end, n == 0)
        pieces.append((istart, attrs + head + body + ["}"]))
    out = open(docfile).read().rstrip("\n").split("\n") + [""] + (uses + [""] if uses else [])
    for _, lines in sorted(pieces, key=lambda p: p[0]):
        out += lines + [""]
    while out and out[-1] == "":
        out.pop()
    os.makedirs(os.path.dirname(target), exist_ok=True)
    with open(target, "w") as f:
        f.write("\n".join(out) + "\n")

    # The parent: drop the moved lines, collapse doubled blanks, wire the module.
    # Tidy only at the seams of the dropped ranges, never inside a literal:
    # a doubled blank line, a blank right after an `impl … {` whose first
    # method left, and one right before a `}` whose last method left.
    res, nums, at_seam = [], [], False
    inside = splitlib.literal_interior(path)
    for i, l in enumerate(L, 1):
        if i in drop:
            at_seam = True
            continue
        if at_seam and i not in inside and res:
            if l == "" and (res[-1] == "" or res[-1].rstrip().endswith("{")):
                continue
            if l.strip() == "}" and res[-1] == "" and nums[-1] not in inside:
                res.pop()
                nums.pop()
        at_seam = False
        res.append(l)
        nums.append(i)
    def first_body(ls):
        return next((i for i, l in enumerate(ls)
                     if re.match(r"^(pub(\(crate\))? )?(fn|struct|enum|const|static|type|trait|impl)\b", l)
                     or (l.startswith("#[") and not l.startswith("#[cfg(test)]"))
                     or l.startswith("/// ")), len(ls))

    def use_ends(ls):
        fb = first_body(ls)
        return [i for i in range(fb) if re.match(r"^(pub(\(\w+\))? )?use .*;$", ls[i]) or ls[i] == "};"]

    fb = first_body(res)
    mods = [i for i in range(fb) if re.match(r"^(pub(\(\w+\))? )?mod \w+;$", res[i])]
    if mods:
        res.insert(mods[-1] + 1, f"mod {mod};")
    else:
        first_use = next((i for i in range(fb) if re.match(r"^(pub(\(\w+\))? )?use ", res[i])), fb)
        res[first_use:first_use] = [f"mod {mod};", ""]
    ue = use_ends(res)
    at = (ue[-1] + 1) if ue else (res.index(f"mod {mod};") + 1)
    pubs = sorted(r.name for _, r in moved_top if r.kind != "impl" and r.vis == "pub")
    wiring = [f"pub(crate) use {mod}::*;"]
    if pubs:
        wiring.insert(0, splitlib.fmt_use(mod, pubs).replace("use ", "pub use ", 1))
    res[at:at] = wiring
    with open(path, "w") as f:
        f.write("\n".join(res) + "\n")
    for p in splitlib.ignored([target]):
        print(f"WARNING {p} is git-ignored: `git add` will skip it silently (use -f)")
    nm = sum(len(ms) for ms in moved_meth.values())
    print(f"moved {len(moved_top)} top-level items and {nm} impl items "
          f"({len(drop)} lines) -> {splitlib.rel(target)}")


if __name__ == "__main__":
    main()
