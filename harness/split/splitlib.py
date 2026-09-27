"""Shared helpers for the harness/split scripts (see README.md).

The item lister is the Rust tool in `rsitems/`, built on first use into
`target/split-tools` (release, `--offline`) and rebuilt when its source is
newer than the binary.
"""
import json
import os
import re
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TOOL_DIR = os.path.join(REPO, "harness", "split", "rsitems")
TARGET = os.path.join(REPO, "target", "split-tools")
BIN = os.path.join(TARGET, "release", "rsitems")

# A file whose `mod x;` resolves to a sibling `x.rs` rather than `<stem>/x.rs`.
ROOT_FILES = ("lib.rs", "main.rs", "mod.rs")


def _tool():
    srcs = [os.path.join(TOOL_DIR, "Cargo.toml"), os.path.join(TOOL_DIR, "Cargo.lock"),
            os.path.join(TOOL_DIR, "src", "main.rs")]
    if not os.path.exists(BIN) or os.path.getmtime(BIN) < max(map(os.path.getmtime, srcs)):
        subprocess.run(["cargo", "build", "-q", "--offline", "--release", "--locked",
                        "--manifest-path", os.path.join(TOOL_DIR, "Cargo.toml"),
                        "--target-dir", TARGET], check=True)
    return BIN


class Item:
    """One rsitems row. `start` includes attributes and doc comments."""

    def __init__(self, depth, kind, name, start, end, parent):
        self.depth, self.kind, self.name = int(depth), kind, name
        self.start, self.end, self.parent = int(start), int(end), int(parent)

    def __repr__(self):
        return f"Item({self.depth} {self.kind} {self.name} {self.start}-{self.end})"


def items(path):
    out = subprocess.check_output([_tool(), path], text=True)
    return [Item(*l.split("\t")) for l in out.splitlines()]


def literals(path):
    """Multi-line literals as (start_line, start_col, end_line, end_col)."""
    out = subprocess.check_output([_tool(), path, "--literals"], text=True)
    return [tuple(int(x) for x in l.split("\t")) for l in out.splitlines()]


def _string_body_keeps_indent(text):
    """True when re-indenting the lines inside this literal changes its value.

    Only a plain (non-raw) string or byte string whose every newline is a
    `\\`-newline continuation is indent-free: the escape drops the newline and
    the next line's leading whitespace. Anything else — a raw string, a
    newline taken literally, a doc comment block — keeps its indentation.
    """
    m = re.match(r'^(?:b|c)?"', text)
    if not m:
        return True
    i = m.end()
    while i < len(text):
        ch = text[i]
        if ch == "\\":
            i += 2
            continue
        if ch == "\n":
            return True
        i += 1
    return False


def verbatim_lines(path):
    """Lines (1-based) that must not be re-indented: the interior lines of
    every multi-line literal whose value depends on their indentation."""
    src = open(path).read().split("\n")
    keep = set()
    for sl, sc, el, ec in literals(path):
        text = "\n".join([src[sl - 1][sc:]] + src[sl:el - 1] + [src[el - 1][:ec]])
        if _string_body_keeps_indent(text):
            keep.update(range(sl + 1, el + 1))
    return keep


def child_dir(path):
    """Where `mod x;` declared in `path` looks for `x.rs`."""
    d, base = os.path.split(path)
    return d if base in ROOT_FILES else os.path.join(d, base[:-3])


def cargo_messages(crate):
    """rustc's JSON diagnostics for `cargo check -p CRATE --all-targets`."""
    p = subprocess.run(["cargo", "check", "-q", "-p", crate, "--all-targets", "--locked",
                        "--message-format=json"], capture_output=True, text=True, cwd=REPO)
    msgs = []
    for line in p.stdout.splitlines():
        try:
            j = json.loads(line)
        except ValueError:
            continue
        if j.get("reason") == "compiler-message":
            msgs.append(j["message"])
    return msgs


def primary_span(msg):
    for s in msg["spans"]:
        if s["is_primary"]:
            return s
    return msg["spans"][0] if msg["spans"] else None


def code(msg):
    return (msg.get("code") or {}).get("code")


def backticked(text):
    return re.findall(r"`([^`]+)`", text)


def name_key(name):
    """rustfmt's (style edition 2021) `use` list order: snake_case, then
    CamelCase, then SCREAMING_CASE — the order the codebase is written in."""
    return (0 if name[0].islower() else 2 if name.upper() == name else 1, name)


def fmt_use(prefix, names, width=100):
    """A rustfmt-shaped `use PREFIX::{…};` wrapped at `width` columns."""
    names = sorted(set(names), key=name_key)
    if len(names) == 1:
        return f"use {prefix}::{names[0]};"
    one = f"use {prefix}::{{{', '.join(names)}}};"
    if len(one) <= width:
        return one
    lines, cur = [], "   "
    for n in names:
        if len(cur) + len(n) + 2 > width:
            lines.append(cur)
            cur = "   "
        cur += f" {n},"
    lines.append(cur)
    return f"use {prefix}::{{\n" + "\n".join(lines) + "\n};"


def rel(path):
    """`path` relative to the repo when inside it, else as given."""
    r = os.path.relpath(path, REPO)
    return path if r.startswith("..") else r


def die(msg):
    print(msg, file=sys.stderr)
    sys.exit(1)
