#!/usr/bin/env python3
"""Fresh-dir parity probes: run the reference and the port on the same input.

Usage:
  harness/probe.py -e 'SRC' [-e 'SRC' ...]      # snippet mode: `check` diagnostics
  harness/probe.py -f FILE.rb [-f ...]          # same, one probe per file
  harness/probe.py --dir PROJ -e 'SRC'          # same, inside a copy of PROJ
  harness/probe.py --dir PROJ -- ARGS...        # raw mode: any subcommand

Snippet mode writes each source to `a.rb` in a fresh directory per engine, runs
`check --format json a.rb`, and compares the full diagnostic tuples
(rule, line, column, severity, message) plus the exit code. With `--dir`, each probe
directory starts as a copy of PROJ, which is how to probe project `sig/` and
`.rigor.yml` behaviour that the fixture harness and the sweep cannot see.

Raw mode copies PROJ into a fresh directory per engine, runs `rigor ARGS` there
on both engines, and compares stdout, stderr and the exit code byte for byte.
Use it for CLI, config, baseline and output-format probes. The reference's
`check` prints a run summary (wall time, memory) on stderr that never matches;
`--ignore-stderr` leaves stderr out of the comparison.

What it gets right so a hand-rolled loop does not have to (AGENTS.md → Probing):
  * a fresh cwd per engine per probe (the reference's `.rigor/cache` is keyed
    by cwd and serves stale results across probes);
  * the checkout's rigor-rbs-inline pinned onto the reference's load path
    (UPSTREAM.md hazard 1);
  * `--no-cache` on the reference's `check` only — the port rejects the flag
    and would take it as a path;
  * diagnostics compared as tuples, so the two engines' different JSON shapes
    (the reference prints an object, the port a bare array) do not matter —
    use snippet mode, not raw mode, for `check` diagnostics;
  * JSON read from stdout only, so stderr progress lines cannot corrupt it;
  * the port binary resolved exactly as fp_audit.py does (target/release,
    auto-built when absent, refused when older than the crate sources);
  * `POSIXLY_CORRECT` removed from both environments.

Exit status: 0 when every probe matches, 1 when any differs, 2 on a usage
error or when an engine produced no parseable JSON (snippet mode) or died on a
signal (raw mode). `--keep` leaves the probe directories in place and prints them.

Env: RIGOR_RS_BIN, REFERENCE_RIGOR_DIR (as fp_audit.py).
"""
import argparse
import contextlib
import json
import os
import shutil
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True  # importing fp_audit must not litter harness/__pycache__
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fp_audit  # noqa: E402  (shares binary resolution and the reference paths)

REF_PLUGIN = os.path.join(fp_audit.REF_DIR, "plugins", "rigor-rbs-inline", "lib")


def env():
    e = dict(os.environ)
    e.pop("POSIXLY_CORRECT", None)
    return e


def ref_cmd(args):
    args = list(args)
    if args and args[0] == "check" and "--no-cache" not in args:
        args.insert(1, "--no-cache")
    # -E UTF-8 as harness/lib.rb: messages inspect strings, and an unset LANG
    # would escape their non-ASCII.
    return ["ruby", "-E", "UTF-8", "-I", fp_audit.REF_LIB, "-I", REF_PLUGIN,
            fp_audit.REF_EXE] + args


def run(cmd, cwd):
    r = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, env=env())
    return r.returncode, r.stdout, r.stderr


def diagnostics(stdout):
    """Full tuples from `--format json` stdout (the reference prints an object,
    the port a bare array). None when stdout holds no parseable JSON."""
    starts = [i for i in (stdout.find("{"), stdout.find("[")) if i >= 0]
    if not starts:
        return None
    try:
        obj = json.loads(stdout[min(starts):])
    except json.JSONDecodeError:
        return None
    rows = obj.get("diagnostics", []) if isinstance(obj, dict) else obj
    return [(d.get("rule"), d.get("line"), d.get("column"), d.get("severity"), d.get("message"))
            for d in rows]


def fmt(rows):
    if rows is None:
        return "(no JSON)"
    if not rows:
        return "silent"
    return "; ".join(f"{r}@{l}:{c} [{s}] {m}" for r, l, c, s, m in rows)


def probe_snippet(rs, label, src, root, base):
    results = {}
    for eng in ("ref", "port"):
        d = os.path.join(root, eng)
        if base:
            shutil.copytree(base, d, symlinks=True)
        else:
            os.makedirs(d)
        with open(os.path.join(d, "a.rb"), "w") as f:
            f.write(src if src.endswith("\n") else src + "\n")
        args = ["check", "--format", "json", "a.rb"]
        cmd = ref_cmd(args) if eng == "ref" else [rs] + args
        code, out, err = run(cmd, d)
        results[eng] = (code, diagnostics(out), err)
    (rc, rrows, rerr), (pc, prows, perr) = results["ref"], results["port"]
    failed = rrows is None or prows is None
    same = not failed and rrows == prows and rc == pc
    print(f"{'=' if same else '≠'} {label}: {src.strip()}")
    if same:
        print(f"    both  exit={rc}  {fmt(rrows)}")
    else:
        print(f"    ref   exit={rc}  {fmt(rrows)}")
        print(f"    port  exit={pc}  {fmt(prows)}")
        for eng, rows, err in (("ref", rrows, rerr), ("port", prows, perr)):
            if rows is None and err.strip():
                print(f"    {eng} stderr: {err.strip().splitlines()[-1]}")
    return "error" if failed else ("same" if same else "diff")


def probe_raw(rs, proj, args, root, ignore_stderr):
    results = {}
    for eng in ("ref", "port"):
        d = os.path.join(root, eng)
        shutil.copytree(proj, d, symlinks=True)
        cmd = ref_cmd(args) if eng == "ref" else [rs] + list(args)
        results[eng] = run(cmd, d)
    channels = (("exit", 0), ("stdout", 1)) + (() if ignore_stderr else (("stderr", 2),))
    same = all(results["ref"][i] == results["port"][i] for _, i in channels)
    if not same and any(results[e][0] < 0 for e in ("ref", "port")):
        same = "error"  # killed by a signal
    print(f"{'=' if same is True else '≠'} rigor {' '.join(args)}  (from {proj})")
    for name, i in channels:
        rv, pv = results["ref"][i], results["port"][i]
        if rv == pv:
            print(f"    {name}: identical" + (f" ({rv})" if name == "exit" else ""))
            continue
        print(f"    {name}: differs")
        if name == "exit":
            print(f"      ref {rv}  port {pv}")
            continue
        for eng, v in (("ref", rv), ("port", pv)):
            body = v if v else "(empty)\n"
            print(f"      --- {eng}")
            for line in body.splitlines()[:40]:
                print(f"      {line}")
    return same


def main():
    ap = argparse.ArgumentParser(
        description=__doc__.split("\n\n")[0],
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("-e", dest="snippets", action="append", default=[],
                    help="Ruby source to probe (repeatable)")
    ap.add_argument("-f", dest="files", action="append", default=[],
                    help="Ruby file to probe (repeatable)")
    ap.add_argument("--dir", help="project directory copied into each probe directory "
                    "(with -e/-f: the snippet runs inside it; with -- ARGS: raw mode)")
    ap.add_argument("--ignore-stderr", action="store_true",
                    help="raw mode: compare exit code and stdout only")
    ap.add_argument("--keep", action="store_true",
                    help="keep the probe directories and print their root")
    ap.add_argument("args", nargs=argparse.REMAINDER,
                    help="raw mode: rigor arguments after --")
    ns = ap.parse_args()
    raw_args = ns.args[1:] if ns.args[:1] == ["--"] else ns.args

    snippet_mode = bool(ns.snippets or ns.files)
    if snippet_mode and raw_args:
        ap.error("rigor arguments after -- are raw mode; drop -e/-f to use them")
    if not snippet_mode and not (ns.dir and raw_args):
        ap.error("nothing to probe: give -e/-f, or --dir PROJ -- ARGS")

    with contextlib.redirect_stdout(sys.stderr):
        rs = fp_audit.resolve_rs()

    root = tempfile.mkdtemp(prefix="rigor-probe-")
    outcomes = []
    try:
        if not snippet_mode:
            r = probe_raw(rs, os.path.abspath(ns.dir), raw_args, os.path.join(root, "raw"),
                          ns.ignore_stderr)
            outcomes.append("error" if r == "error" else ("same" if r else "diff"))
        else:
            probes = [(f"e{i + 1}", s) for i, s in enumerate(ns.snippets)]
            for path in ns.files:
                with open(path) as f:
                    probes.append((os.path.basename(path), f.read()))
            for i, (label, src) in enumerate(probes):
                outcomes.append(probe_snippet(rs, label, src, os.path.join(root, f"{i + 1:03d}"),
                                              os.path.abspath(ns.dir) if ns.dir else None))
    finally:
        if ns.keep:
            print(f"probe dirs: {root}", file=sys.stderr)
        else:
            shutil.rmtree(root, ignore_errors=True)
    if "error" in outcomes:
        return 2
    return 1 if "diff" in outcomes else 0


if __name__ == "__main__":
    sys.exit(main())
