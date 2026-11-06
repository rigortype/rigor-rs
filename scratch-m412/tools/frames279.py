#!/usr/bin/env python3
"""Static stack frame sizes (arm64 prologue) of the lowering functions in a
probe binary: the pre-indexed `stp …, [sp, #-N]!` plus every `sub sp, sp,
#M[, lsl #12]` before the first non-prologue instruction.

Usage: frames.py BINARY...
"""
import re
import subprocess
import sys

WANT = re.compile(r"rigor_parse.*?(\d+)(lower_node|lower_calls|lower_expressions|lower_control|lower_definitions|lower_jumps|lower_body|lower_typed_operand|lower_optional_body|lower_recovered|lower_blocked|lower_discarded)17h")


def frames(binary):
    syms = subprocess.run(["nm", binary], capture_output=True, text=True).stdout.splitlines()
    out = {}
    for s in syms:
        parts = s.split()
        if len(parts) != 3:
            continue
        addr, _, name = parts
        m = WANT.search(name)
        if not m or "closure" in name:
            continue
        a = int(addr, 16)
        dis = subprocess.run(["objdump", "-d", "--no-show-raw-insn", "--start-address=%#x" % a,
                              "--stop-address=%#x" % (a + 96), binary], capture_output=True, text=True).stdout
        total = 0
        in_probe = False
        for line in dis.splitlines():
            ins = line.split(":", 1)[-1].strip()
            mp = re.match(r"sub\s+x9, sp, #(0x[0-9a-f]+|\d+)(, lsl #12)?", ins)
            if mp:
                v = int(mp.group(1), 0)
                total += v << 12 if mp.group(2) else v
                in_probe = True
                continue
            if in_probe:
                if ins.startswith("b.ne"):
                    in_probe = False
                continue
            m1 = re.match(r"stp\s+\w+, \w+, \[sp, #-(0x[0-9a-f]+|\d+)\]!", ins)
            m2 = re.match(r"sub\s+sp, sp, #(0x[0-9a-f]+|\d+)(?:, lsl #12)?", ins)
            if m1:
                total += int(m1.group(1), 0)
            elif m2:
                v = int(m2.group(1), 0)
                if "lsl #12" in ins:
                    v <<= 12
                total += v
        out[m.group(2)] = max(total, out.get(m.group(2), 0))
    return out


for b in sys.argv[1:]:
    f = frames(b)
    print(b.split("/targets/")[-1], " ".join("%s=%d" % (k, f[k]) for k in sorted(f)))
