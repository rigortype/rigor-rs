#!/usr/bin/env python3
"""Move a Rust file's inline `#[cfg(test)] mod NAME { … }` blocks to files.

Usage: harness/split/split_tests.py FILE [NAME ...]

Each top-level `mod NAME { … }` carrying `#[cfg(test)]` (only the NAMEs given,
when any are) becomes `mod NAME;`, and its body moves to where rustc looks for
it: `src/NAME.rs` beside a crate root or `mod.rs`, `src/<stem>/NAME.rs` beside
any other file. Module paths do not change, so test names and `use super::*`
resolve as before.

The body is de-indented by four spaces, except the interior lines of a
multi-line literal whose value depends on them (a raw string, a string with a
literal newline, a `/** */` doc): those stay byte-for-byte. A `\\`-continued
string is de-indented, since its escape drops the leading whitespace.

The move is PROVED before the script exits: re-inlining every new file into
the rewritten FILE must reproduce the original bytes. On a mismatch nothing is
left written. Comments and attributes on or above a `mod` stay in FILE.
"""
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import splitlib  # noqa: E402


def main():
    if len(sys.argv) < 2:
        splitlib.die(__doc__)
    path, only = sys.argv[1], set(sys.argv[2:])
    orig = open(path).read()
    L = orig.split("\n")
    keep = splitlib.verbatim_lines(path)
    outdir = splitlib.child_dir(path)

    out, cur, moved = [], 1, []   # moved: (name, open_line, close_line, target)
    for it in splitlib.items(path):
        if it.depth != 0 or it.kind != "mod" or (only and it.name not in only):
            continue
        head = L[it.start - 1:it.end]
        k = next((i for i, l in enumerate(head) if l.strip() == f"mod {it.name} {{"), None)
        if k is None or L[it.end - 1] != "}" or "#[cfg(test)]" not in (h.strip() for h in head[:k]):
            continue
        target = os.path.join(outdir, f"{it.name}.rs")
        if os.path.exists(target):
            splitlib.die(f"{target} exists; refusing to overwrite")
        open_ln = it.start + k
        body = []
        for n in range(open_ln + 1, it.end):
            line = L[n - 1]
            if n in keep or line == "":
                body.append(line)
            elif line.startswith("    "):
                body.append(line[4:])
            else:
                splitlib.die(f"{path}:{n}: body line not indented by 4: {line!r}")
        out.extend(L[cur - 1:open_ln - 1])
        out.append(L[open_ln - 1].replace(f"mod {it.name} {{", f"mod {it.name};"))
        cur = it.end + 1
        moved.append((it.name, open_ln, it.end, target, body))
    if not moved:
        splitlib.die(f"{path}: no inline #[cfg(test)] mod to move")
    # Position-dependent constructs mean something else once the body moves
    # (lines shift; include paths and `#[path]` resolve from the new file).
    for name, open_ln, close_ln, target, body in moved:
        for off, b in enumerate(body):
            hit = re.search(r"\b(line|column|file|module_path|include|include_str|include_bytes)!|#\[path\b", b)
            if hit:
                print(f"WARNING {splitlib.rel(path)}:{open_ln + 1 + off}: position-dependent "
                      f"`{hit.group(0)}` in moved mod {name}: check it by hand")
    out.extend(L[cur - 1:])
    new = "\n".join(out)

    # Proof: re-inline each body into the rewritten file.
    chk, by_decl = [], {f"mod {m[0]};": m for m in moved}
    for line in new.split("\n"):
        m = by_decl.get(line.strip())
        if not m:
            chk.append(line)
            continue
        name, open_ln, close_ln, _, body = m
        chk.append(line[:-1] + " {")
        for off, b in enumerate(body):
            n = open_ln + 1 + off
            chk.append(b if (n in keep or b == "") else "    " + b)
        chk.append("}")
    if "\n".join(chk) != orig:
        splitlib.die("re-inline check FAILED; nothing written")

    os.makedirs(outdir, exist_ok=True)
    for name, open_ln, close_ln, target, body in moved:
        with open(target, "w") as f:
            f.write("\n".join(body) + "\n")
    with open(path, "w") as f:
        f.write(new)
    for p in splitlib.ignored([m[3] for m in moved]):
        print(f"WARNING {p} is git-ignored: `git add` will skip it silently (use -f)")
    for name, open_ln, close_ln, target, body in moved:
        v = sum(1 for n in keep if open_ln < n < close_ln)
        print(f"{name}: {len(body)} lines -> {splitlib.rel(target)}"
              + (f" ({v} literal lines kept verbatim)" if v else ""))
    print(f"{splitlib.rel(path)}: {orig.count(chr(10))} -> {new.count(chr(10))} lines; "
          "re-inline check: byte-identical")


if __name__ == "__main__":
    main()
