"""lldb helper: on the crash stop, report the overflowing thread's innermost
frames, the first rigor function, the recursion cycle and its stack cost.

Loaded by crashsite.py via `command script import`; it calls
`script lldb_site.report("<out.json>")` from the crash hook.
"""
import json
import re

import lldb

RIGOR = re.compile(r"\b(rigor_parse|rigor_infer|rigor_rules|rigor_index|rigor_types|rigor_cli|rigor)::")


def clean(name):
    if not name:
        return "?"
    name = re.sub(r"::h[0-9a-f]{16}$", "", name)
    for k, v in (("_$LT$", "<"), ("$LT$", "<"), ("$GT$", ">"), ("$u20$", " "), ("$C$", ","),
                 ("$RF$", "&"), ("$u7b$", "{"), ("$u7d$", "}"), ("$BP$", "*"), ("..", "::")):
        name = name.replace(k, v)
    return name


def a_full_walk_ok(n):
    return n <= 400000


def report(out_path, top=600):
    proc = lldb.debugger.GetSelectedTarget().GetProcess()
    th = proc.GetSelectedThread()
    res = {
        "stop": th.GetStopDescription(200),
        "thread_name": th.GetName(),
        "thread_index": th.GetIndexID(),
    }
    n = th.GetNumFrames()
    res["num_frames"] = n
    frames = []
    for i in range(min(n, top)):
        f = th.GetFrameAtIndex(i)
        fn = f.GetFunctionName() or f.GetSymbol().GetName()
        mod = f.GetModule().GetFileSpec().GetFilename()
        frames.append((clean(fn), f.GetSP(), mod))
    bottom = []
    for i in range(max(min(n, top), n - 40), n):
        f = th.GetFrameAtIndex(i)
        fn = f.GetFunctionName() or f.GetSymbol().GetName()
        bottom.append(clean(fn))
    res["top"] = [fr[0] for fr in frames[:40]]
    res["bottom"] = bottom
    res["innermost"] = frames[0][0] if frames else None
    res["innermost_module"] = frames[0][2] if frames else None
    first_rigor = next((fr[0] for fr in frames if RIGOR.search(fr[0])), None)
    res["first_rigor"] = first_rigor
    # the recursion cycle: the smallest period p such that frames[i] == frames[i+p]
    # over a long window, skipping the innermost 20 frames (the partial level)
    names = [fr[0] for fr in frames]
    start = 20
    best = None
    for p in range(1, 120):
        w = names[start:start + 3 * p + 40]
        if len(w) < 2 * p + 1:
            break
        if all(w[i] == w[i + p] for i in range(len(w) - p)):
            best = p
            break
    if best:
        cyc = names[start:start + best]
        sp0 = frames[start][1]
        sp1 = frames[start + best][1]
        # how many nesting levels one cycle covers is shape-dependent; report bytes per cycle
        res["cycle_len"] = best
        res["cycle"] = cyc
        res["cycle_bytes"] = sp1 - sp0
    # the recursion's ENTRY: walk the whole stack (cheap enough up to ~1e5
    # frames) to the first frame below the innermost cycle whose function is
    # not part of that cycle, and the first rigor function anywhere.
    cyc_set = set(res.get("cycle") or [])
    entry = []
    rigor_anywhere = None
    if cyc_set:
        i = start
        while i < n:
            f = th.GetFrameAtIndex(i)
            nm = clean(f.GetFunctionName() or f.GetSymbol().GetName())
            if rigor_anywhere is None and RIGOR.search(nm):
                rigor_anywhere = (i, nm)
            if not entry and nm not in cyc_set:
                entry = [(i, nm)]
                for j in range(i + 1, min(n, i + 8)):
                    fj = th.GetFrameAtIndex(j)
                    entry.append((j, clean(fj.GetFunctionName() or fj.GetSymbol().GetName())))
            if entry and rigor_anywhere:
                break
            i += 1
    res["entry"] = entry
    res["first_rigor_anywhere"] = rigor_anywhere
    # stack CONSUMERS over the whole stack: bytes per function (frame i's size
    # = SP[i+1] - SP[i]); the innermost cycle is not always the big consumer
    # (a cheap scan called at the bottom of a deep lowering recursion).
    if a_full_walk_ok(n):
        per = {}
        prev_sp, prev_nm = None, None
        for i in range(n):
            f = th.GetFrameAtIndex(i)
            sp = f.GetSP()
            nm = clean(f.GetFunctionName() or f.GetSymbol().GetName())
            if prev_sp is not None:
                per[prev_nm] = per.get(prev_nm, 0) + (sp - prev_sp)
            prev_sp, prev_nm = sp, nm
        total = sum(per.values())
        res["stack_total"] = total
        res["consumers"] = sorted(per.items(), key=lambda kv: -kv[1])[:12]
    # every distinct function in the scanned window, with counts
    counts = {}
    for nm in names:
        counts[nm] = counts.get(nm, 0) + 1
    res["distinct"] = sorted(counts.items(), key=lambda kv: -kv[1])[:30]
    with open(out_path, "w") as fh:
        json.dump(res, fh, indent=1)
