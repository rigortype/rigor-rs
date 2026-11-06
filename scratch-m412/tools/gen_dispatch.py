#!/usr/bin/env python3
"""Prototype: generate `rigor_parse::stack::visit_dispatch`, a free-function
copy of ruby-prism's provided `Visit::visit` (from the build's generated
bindings.rs), so a Visit impl can override `visit` as
`maybe_grow(|| visit_dispatch(self, node))`.

Usage: gen_dispatch.py BUILD_DIR OUT_RS
"""
import glob
import os
import re
import sys

build, out = sys.argv[1], sys.argv[2]
src = None
for f in glob.glob(os.path.join(build, "ruby-prism-*", "out", "bindings.rs")):
    s = open(f).read()
    if "   fn visit(&mut self, node: &Node<'pr>) {" in s:
        src = s
        break
start = src.index("   fn visit(&mut self, node: &Node<'pr>) {")
body = src[start:src.index("\n   }\n", start)]
arms = re.findall(r"Node::(\w+) \{ parser, pointer, marker \} => \{\n\s+let concrete = \w+ \{[^}]*\};\n"
                  r"\s+self\.(visit_\w+_node_enter)\(concrete\.as_node\(\)\);\n"
                  r"\s+self\.(visit_\w+)\(&concrete\);\n\s+self\.(visit_\w+_node_leave)\(\);", body)
assert len(arms) > 100, len(arms)
lines = ["/// GENERATED (scratch-m412/tools/gen_dispatch.py): the body of ruby-prism's",
         "/// provided `Visit::visit`, as a free fn so an override can wrap it.",
         "pub fn visit_dispatch<'pr, V: ruby_prism::Visit<'pr> + ?Sized>(v: &mut V, node: &ruby_prism::Node<'pr>) {",
         "    use ruby_prism::Node;",
         "    match node {"]
for name, enter, visit, leave in arms:
    as_fn = "as" + visit[len("visit"):]
    lines.append("        Node::%s { .. } => {" % name)
    lines.append("            let concrete = node.%s().unwrap();" % as_fn)
    lines.append("            v.%s(concrete.as_node());" % enter)
    lines.append("            v.%s(&concrete);" % visit)
    lines.append("            v.%s();" % leave)
    lines.append("        }")
lines += ["    }", "}", ""]
open(out, "w").write("\n".join(lines))
print(len(arms), "arms")
