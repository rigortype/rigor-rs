#!/usr/bin/env python3
"""Drive `rigor lsp --transport=stdio` non-interactively on a deep file.

Steps (each reported): initialize -> initialized -> didOpen(deep buffer) ->
wait for publishDiagnostics (computed on a rayon worker) -> documentSymbol,
hover, completion (computed on the LOOP thread = main) -> shutdown/exit.
`--on-disk` also writes the deep file under lib/ (plus a trivial sibling with
`--sibling`), so the startup overlay build parses it too (rayon par_iter; on
the main thread when it is the only project file).

Usage: lsp_probe.py CLI KIND N [--on-disk] [--sibling] [--min-stack-mib M] [--ulimit-mib U]
"""
import argparse
import json
import os
import re
import select
import subprocess
import time

from common import cli, gen, TRIALS, rm
import tempfile

ap = argparse.ArgumentParser()
ap.add_argument("cli")
ap.add_argument("kind")
ap.add_argument("n", type=int)
ap.add_argument("--on-disk", action="store_true")
ap.add_argument("--sibling", action="store_true")
ap.add_argument("--min-stack-mib", type=int)
ap.add_argument("--ulimit-mib", type=int)
ap.add_argument("--timeout", type=float, default=120)
a = ap.parse_args()
rigor = cli(a.cli) if not os.path.isabs(a.cli) else a.cli

d = os.path.realpath(tempfile.mkdtemp(dir=TRIALS))
os.makedirs(os.path.join(d, "lib"))
src = gen(a.kind, a.n).decode()
path = os.path.join(d, "lib", "a.rb")
if a.on_disk:
    open(path, "w").write(src)
    if a.sibling:
        open(os.path.join(d, "lib", "b.rb"), "w").write("x = 1\n")
uri = "file://" + path
env = dict(os.environ, RIGOR_NO_RUBY="1")
env.pop("RUST_MIN_STACK", None)
if a.min_stack_mib:
    env["RUST_MIN_STACK"] = str(a.min_stack_mib << 20)
cmd = [rigor, "lsp", "--transport=stdio"]
if a.ulimit_mib:
    cmd = ["/bin/sh", "-c", 'ulimit -s %d && exec "$@"' % (a.ulimit_mib * 1024), "sh"] + cmd
p = subprocess.Popen(cmd, cwd=d, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                     stderr=subprocess.PIPE)
buf = b""
events = []


def send(obj):
    body = json.dumps(obj).encode()
    try:
        p.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        p.stdin.flush()
        return True
    except BrokenPipeError:
        return False


def recv(until, timeout):
    """Read messages until until(msg) is true; None on death/timeout."""
    global buf
    end = time.time() + timeout
    while time.time() < end:
        m = re.match(rb"Content-Length: (\d+)\r\n\r\n", buf)
        if m and len(buf) >= m.end() + int(m.group(1)):
            body = buf[m.end():m.end() + int(m.group(1))]
            buf = buf[m.end() + int(m.group(1)):]
            msg = json.loads(body)
            if until(msg):
                return msg
            continue
        r, _, _ = select.select([p.stdout], [], [], 0.2)
        if r:
            chunk = os.read(p.stdout.fileno(), 1 << 20)
            if not chunk:
                return None
            buf += chunk
        elif p.poll() is not None:
            return None
    return None


def step(name, ok):
    events.append("%s:%s" % (name, "ok" if ok else ("DIED" if p.poll() is not None else "timeout")))
    return ok


try:
    send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
          "params": {"processId": None, "rootUri": "file://" + d, "capabilities": {}}})
    if step("initialize", recv(lambda m: m.get("id") == 1, a.timeout)):
        send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
        send({"jsonrpc": "2.0", "method": "textDocument/didOpen",
              "params": {"textDocument": {"uri": uri, "languageId": "ruby", "version": 1, "text": src}}})
        diag = recv(lambda m: m.get("method") == "textDocument/publishDiagnostics"
                    and m["params"]["uri"] == uri, a.timeout)
        if step("diagnostics", diag):
            ds = diag["params"]["diagnostics"]
            events[-1] += "(%d%s)" % (len(ds), (" first=%s/sev%s %r" % (ds[0].get("code"), ds[0].get("severity"), ds[0].get("message", "")[:90])) if ds else "")
        for i, (meth, params) in enumerate([
            ("textDocument/documentSymbol", {"textDocument": {"uri": uri}}),
            ("textDocument/hover", {"textDocument": {"uri": uri}, "position": {"line": 0, "character": 1}}),
            ("textDocument/completion", {"textDocument": {"uri": uri}, "position": {"line": 0, "character": 1}}),
        ]):
            if p.poll() is not None:
                break
            send({"jsonrpc": "2.0", "id": 10 + i, "method": meth, "params": params})
            resp = recv(lambda m, i=i: m.get("id") == 10 + i, a.timeout)
            if step(meth.split("/")[1], resp):
                events[-1] += "(%s)" % ("null" if resp.get("result") is None else "value")
        if p.poll() is None:
            send({"jsonrpc": "2.0", "id": 99, "method": "shutdown", "params": None})
            step("shutdown", recv(lambda m: m.get("id") == 99, a.timeout))
            send({"jsonrpc": "2.0", "method": "exit", "params": None})
    try:
        p.wait(timeout=10)
    except subprocess.TimeoutExpired:
        p.kill()
        p.wait()
    err = p.stderr.read().decode("utf-8", "replace")
    m = re.search(r"thread '([^']*)'.*has overflowed its stack", err)
    print("%s\t%s\t%d\t%s%s%s\trc=%s\t%s\t%s" % (a.cli, a.kind, a.n, "on-disk" if a.on_disk else "buffer",
          "+sibling" if a.sibling else "", (" minstack=%d" % a.min_stack_mib) if a.min_stack_mib else "",
          p.returncode, " ".join(events), ("OVERFLOW thread=" + m.group(1)) if m else ""), flush=True)
finally:
    if p.poll() is None:
        p.kill()
    rm(d)
