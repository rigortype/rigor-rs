#!/usr/bin/env python3
"""Which Prism node fields does ruby-prism's default `Visit` descend into
WITHOUT going through `Visit::visit` (typed fields, `visitor.visit_x_node(..)`)?
A cycle among those edges is a recursion a depth counter on
`visit_branch_node_enter` cannot see. Prints the typed-edge graph's cycles.

Usage: typed_cycles.py BUILD_DIR
"""
import glob
import os
import re
import sys

src = None
for f in glob.glob(os.path.join(sys.argv[1], "ruby-prism-*", "out", "bindings.rs")):
    s = open(f).read()
    if "pub fn visit_call_node" in s:
        src = s
        break
edges = {}
for m in re.finditer(r"pub fn (visit_\w+_node)<'pr, V>\(visitor: &mut V, node: &\w+<'pr>\)[^{]*\{(.*?)\n\}", src, re.S):
    fn, body = m.group(1), m.group(2)
    typed = set(re.findall(r"visitor\.(visit_\w+_node)\(", body))
    edges[fn] = typed
print("node types with typed-field edges:", sum(1 for v in edges.values() if v))
for k, v in sorted(edges.items()):
    if v:
        print("  %s -> %s" % (k, ", ".join(sorted(v))))
# cycles (DFS)
cycles = set()


def dfs(start, cur, path):
    for nxt in edges.get(cur, ()):
        if nxt == start:
            cycles.add(tuple(path + [nxt]))
        elif nxt not in path and len(path) < 6:
            dfs(start, nxt, path + [nxt])


for k in edges:
    dfs(k, k, [k])
print("typed-only cycles:")
for c in sorted(cycles):
    print("  " + " -> ".join(c))
