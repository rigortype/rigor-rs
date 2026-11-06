#!/usr/bin/env python3
"""Pivot the measured data into the report tables (capped / uncapped).

Usage: tables.py   -> data/summary_capped.txt, data/summary_uncapped.txt
"""
import os
import re

from common import DATA


def p(name):
    return os.path.join(DATA, name)


SRC = {"chain": "x.a.a...", "plus": "1 + 1 + ...", "minus": "1 - 1 - ...", "andand": "a && a && ...",
       "oror": "a || a || ...", "kwand": "a and a ...", "orkw": "a or a ...", "index": "x[1][1]...",
       "safenav": "x&.a&.a...", "argchain": "x.a(1).a(1)...", "blkchain": "x.a { }.a { }...",
       "callblk": "x.a(1) { |y| y }...", "cpath": "A::A::A...", "shovel": "x << 1 << 1...",
       "lt": "1 < 1 < 1...", "ifmod": "1 if a if a...", "unlessmod": "1 unless a ...",
       "rescmod": "1 rescue 1 ...", "whilemod": "1 while a ..."}

# ---------- capped ----------
depth = {}
for line in open(p("capped_prism_depth.tsv")):
    f = line.split("\t")
    depth[f[0]] = f[3].strip().split("=")[1]
bind = {}
for line in open(p("binding_sites.log")):
    f = line.rstrip("\n").split("\t")
    if len(f) < 4:
        continue
    m = re.search(r"consumers\([\d.]+MiB\): (.*)", line)
    top = m.group(1).split(";")[0].strip() if m else ""
    bind.setdefault(f[0], {})[int(f[2][:-3])] = (f[3].split("|")[0].strip(), top)
first = {}
for line in open(p("ladder_capped.log")):
    f = line.rstrip("\n").split("\t")
    if f[2] == "2MiB":
        first[f[0]] = f[3].split("|")[0].strip()
out = ["| kind | N (cap) | visit depth | 1 file (main) | 2 files (worker 2 MiB) | min main MiB | min worker MiB | first site at 2 MiB | binding consumer at min-1 | Prism C binds |",
       "|---|---|---|---|---|---|---|---|---|---|"]
for line in open(p("e2e_capped.tsv")):
    f = line.rstrip("\n").split("\t")
    k, n = f[2], f[3]
    minw = re.search(r"minw=(\S+?)MiB", line).group(1)
    minm = re.search(r"minm=(\S+?)MiB", line).group(1)
    b = bind.get(k, {})
    top = b[max(b)][1] if b else ""
    prism = "yes" if "parse_" in top else "no"
    out.append("| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |" % (
        k, n, depth.get(k, ""), "ok" if "main:ok" in line else "CRASH", "ok" if "worker2MiB:ok" in line else "CRASH",
        minm, minw, first.get(k, "")[:60], top[:50], prism))
open(p("summary_capped.txt"), "w").write("\n".join(out) + "\n")
print("\n".join(out))

# ---------- uncapped ----------
bindu = {}
for line in open(p("binding_sites.log")):
    f = line.rstrip("\n").split("\t")
    m = re.search(r"consumers\([\d.]+MiB\): (.*)", line)
    if m and f[0] in SRC:
        prev = bindu.get(f[0])
        mib = int(f[2][:-3])
        if not prev or mib > prev[0]:
            bindu[f[0]] = (mib, m.group(1).split(";")[0].strip())
firstu = {}
for line in open(p("ladder_uncapped.log")):
    f = line.rstrip("\n").split("\t")
    if f[2] == "1MiB":
        firstu[f[0]] = f[3].split("|")[0].strip()
twenty = {}
for line in open(p("e2e_20k.log")):
    f = line.split("\t")
    twenty[f[2]] = re.search(r"minw=(\S+?)MiB", line).group(1)
out = ["| kind | source | min worker MiB @5k | @20k | check time @5k | binding consumer @5k (min-1) | first site at 1 MiB |",
       "|---|---|---|---|---|---|---|"]
for line in open(p("e2e_uncapped.tsv")):
    f = line.rstrip("\n").split("\t")
    if f[3] != "5000":
        continue
    k = f[2]
    m = re.search(r"minw=(\S+?)MiB\(ok rc=\d+ out=\d+B ([\d.]+)s", line)
    out.append("| %s | %s | %s | %s | %ss | %s | %s |" % (
        k, SRC.get(k, ""), m.group(1), twenty.get(k, ""), m.group(2), bindu.get(k, (0, ""))[1][:50],
        firstu.get(k, "")[:70]))
open(p("summary_uncapped.txt"), "w").write("\n".join(out) + "\n")
print("\n".join(out))
