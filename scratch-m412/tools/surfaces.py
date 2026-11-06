#!/usr/bin/env python3
"""How each surface reports a file past the depth bound: check, type-of,
annotate, coverage, mcp (check/outline tools), and lsp (via lsp_probe.py).

Usage: surfaces.py CLI [N]   (default N=30000, an `x.a` chain)
"""
import json
import os
import subprocess
import sys
import tempfile

from common import HERE, TRIALS, cli, gen, rm

rigor = cli(sys.argv[1]) if not os.path.isabs(sys.argv[1]) else sys.argv[1]
n = int(sys.argv[2]) if len(sys.argv) > 2 else 30000
d = tempfile.mkdtemp(dir=TRIALS)
try:
    src = gen("chain", n)
    open(os.path.join(d, "a.rb"), "wb").write(src)
    for args in (["check", "--format", "json", "a.rb"], ["check", "a.rb"], ["type-of", "a.rb:1:1"],
                 ["annotate", "a.rb"], ["coverage", "a.rb"]):
        r = subprocess.run([rigor] + args, cwd=d, capture_output=True, text=True, timeout=300)
        out = r.stdout.strip().splitlines()
        print("== %s  rc=%d" % (" ".join(args), r.returncode))
        print("   stdout: %s" % (" | ".join(l for l in out if "nesting" in l or "internal" in l)[:300] or "(%d lines)" % len(out)))
        print("   stderr: %s" % r.stderr.strip()[:300])
    msgs = [{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}}]
    for i, tool in enumerate(["check", "triage", "annotate", "type_of", "outline"]):
        a = {"source": src.decode()}
        if tool == "type_of":
            a.update(line=1, column=1)
        msgs.append({"jsonrpc": "2.0", "id": 10 + i, "method": "tools/call", "params": {"name": tool, "arguments": a}})
    r = subprocess.run([rigor, "mcp"], input="".join(json.dumps(m) + "\n" for m in msgs), capture_output=True,
                       text=True, timeout=300, cwd=d)
    for line in r.stdout.splitlines():
        m = json.loads(line)
        if m.get("id", 0) >= 10:
            res = m.get("result", {})
            print("== mcp %s  isError=%s  %s" % (["check", "triage", "annotate", "type_of", "outline"][m["id"] - 10],
                                                res.get("isError"), res.get("content", [{}])[0].get("text", "")[:160]))
finally:
    rm(d)
lsp = subprocess.run([sys.executable, os.path.join(HERE, "lsp_probe.py"), sys.argv[1], "chain", str(n)],
                     capture_output=True, text=True)
print("== lsp", lsp.stdout.strip())
