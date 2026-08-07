#!/usr/bin/env python3
"""Classify the coverage gaps — the diagnostics the reference emits and rigor-rs
does not — by the MECHANISM behind them, not just by rule id.

`fp_audit.py --gaps` answers "which rules"; that is where the effort would go if
every gap in a rule were the same shape. They are not: the 400-odd
`call.undefined-method` gaps are a handful of distinct root causes with very
different closability, and the per-rule histogram hides which. This dumps every
gap with the reference's own message and buckets them, so a slice can be chosen
from measured frequency instead of from a guess.

The buckets come from the reference's message text, which names the receiver
type and the method — the two things that decide the mechanism. `--dump` writes
the raw rows so a bucket can be read case by case.

Usage:  python3 harness/gap_census.py [--sweep] [--dump FILE] [<dir> ...]
Env:    same as fp_audit.py (RIGOR_RS_BIN, REFERENCE_RIGOR_DIR).
"""
import importlib.util
import os
import re
import sys
from collections import Counter, defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("fp_audit", os.path.join(HERE, "fp_audit.py"))
fp = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fp)

# The reference's message shapes, in the order they are tried. Each yields the
# (receiver, method) pair the mechanism turns on; `None` where a shape does not
# carry one.
PATTERNS = [
    ("undefined-method", re.compile(r"undefined method [`'](?P<m>[^']+)' for (?P<r>.+)$")),
    ("possible-nil", re.compile(r"(?P<r>.+?) may be nil.*?[`'](?P<m>[^']+)'")),
    ("nil-receiver", re.compile(r"[`'](?P<m>[^']+)' called on .*?(?P<r>nil.*)$")),
    ("arg-type", re.compile(r"argument type mismatch at .*?[`'](?P<m>[^']+)' on (?P<r>\S+)")),
    ("arity", re.compile(r"wrong number of arguments to [`'](?P<m>[^']+)' on (?P<r>\S+)")),
]


def classify(rule, message):
    """(bucket, receiver, method) for one reference diagnostic."""
    msg = (message or "").strip()
    for _name, pat in PATTERNS:
        m = pat.search(msg)
        if m:
            recv = m.groupdict().get("r", "?").strip().rstrip(".")
            meth = m.groupdict().get("m", "?")
            return recv, meth
    return "?", "?"


def receiver_kind(recv):
    """Coarse shape of the receiver the reference resolved.

    This is the axis that decides closability: a CORE class means the port has
    the signature and simply did not resolve the receiver; a project class means
    the source index owes it; a literal means a fold owes it.
    """
    r = recv.strip()
    if r.startswith('"') or r.startswith("'"):
        return "string literal"
    if re.fullmatch(r"-?\d+(\.\d+)?", r):
        return "numeric literal"
    if r.startswith(":"):
        return "symbol literal"
    if r.startswith("[") or r.startswith("{"):
        return "collection literal"
    if r.startswith("singleton(") or r.startswith("Class<"):
        return "singleton"
    if "|" in r:
        return "union"
    if "::" in r:
        return "namespaced class"
    if re.fullmatch(r"[A-Z]\w*", r):
        return "bare class"
    return "other"


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    dump = None
    for i, a in enumerate(sys.argv):
        if a == "--dump" and i + 1 < len(sys.argv):
            dump = sys.argv[i + 1]
            if dump in args:
                args.remove(dump)
    if "--sweep" in sys.argv:
        present, absent = fp.sweep_targets()
        for e in absent:
            print(f"SKIPPED (not on this machine): {e['label']}")
        args = [e["path"] for e in present] + args
    if not args:
        print(__doc__)
        return 2

    fp.resolve_rs()  # same contract as fp_audit: the binary is reported, and a
                     # stale or unbuildable one refuses to run before measuring
    rows = []
    invalid = []
    for tgt in args:
        files = fp.rb_files(tgt)
        ref = fp.run_ref(files)
        if ref is None:
            print(f"{tgt}: INVALID (unparseable reference output) — no census "
                  "for this corpus")
            invalid.append(tgt)
            continue
        rs_diags = fp.run_rs(files)
        if rs_diags is None:
            # Without a port result every reference diagnostic looks like a gap,
            # which would read as a census of a catastrophically under-covering
            # port rather than as a broken run.
            print(f"{tgt}: INVALID (rigor-rs failed: {fp.LAST_RS_FAILURE}) — no "
                  "census for this corpus")
            invalid.append(tgt)
            continue
        rs_keys = fp.keys(rs_diags)
        for d in ref:
            if d.get("severity", "error") not in fp.PARITY:
                continue
            key = (os.path.abspath(d.get("path", "")), d.get("line"),
                   d.get("column"), d.get("rule"))
            if key in rs_keys:
                continue
            recv, meth = classify(d.get("rule"), d.get("message"))
            rows.append({
                "corpus": os.path.basename(tgt.rstrip("/")),
                "rule": d.get("rule"), "recv": recv, "meth": meth,
                "kind": receiver_kind(recv), "path": key[0], "line": key[1],
                "message": (d.get("message") or "").strip(),
            })
        print(f"{tgt}: {len([r for r in rows if r['corpus'] == os.path.basename(tgt.rstrip('/'))])} gaps")

    print(f"\n=== {len(rows)} coverage gaps ===")
    for rule, n in Counter(r["rule"] for r in rows).most_common():
        print(f"\n{n:5}  {rule}")
        sub = [r for r in rows if r["rule"] == rule]
        for kind, k in Counter(r["kind"] for r in sub).most_common(6):
            tops = Counter(f'{r["recv"]}#{r["meth"]}' for r in sub if r["kind"] == kind)
            sample = ", ".join(f"{s} ×{c}" for s, c in tops.most_common(4))
            print(f"       {k:5}  {kind:18} {sample}")

    print("\n=== the single most repeated (receiver, method) pairs overall ===")
    for pair, n in Counter(f'{r["rule"]} | {r["recv"]}#{r["meth"]}'
                           for r in rows).most_common(25):
        print(f"{n:5}  {pair}")

    if dump:
        import json
        with open(dump, "w", encoding="utf-8") as f:
            json.dump(rows, f, indent=1)
        print(f"\nwrote {len(rows)} rows to {dump}")
    if invalid:
        print(f"\nINVALID COMPARISONS: {len(invalid)} — the census above covers "
              "only the corpora that produced a valid comparison:")
        for t in invalid:
            print(f"  {t}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
