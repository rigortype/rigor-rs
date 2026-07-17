#!/usr/bin/env python3
"""Docs budget gate (issue #21; port of upstream rigor#119's WD2/WD4).

Hand compression of agent-facing docs does not hold — docs/CURRENT_WORK.md
re-inflated 15KB -> 184KB in three weeks under a correct-but-ungated contract,
exactly the ratchet upstream found in its ADR indexes. An economy rule with no
mechanical gate is a temporary state, not a decision; this script is the gate.

Checks:
  1. Byte budgets on the session-loaded / on-demand doc set.
  2. docs/CURRENT_WORK.md carries no status-essay marker — a landed arc folds
     to ONE ledger line; its detail belongs in docs/notes/ or docs/adr/.
  3. Local markdown links in the gated docs resolve (no dangling refs).

Run from the repo root: python3 harness/docs_check.py
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# (path, byte budget). CURRENT_WORK.md's budget is the mechanism that forces
# folding: raise it only with a recorded argument, not to fit a new essay.
BUDGETS = [
    ("docs/CURRENT_WORK.md", 24_576),
    ("docs/PORT_BACKLOG.md", 98_304),
    ("AGENTS.md", 16_384),
    ("CONTEXT.md", 16_384),
]

# The old status-essay convention opened every entry with this marker; its
# reappearance means an unfolded essay came back.
ESSAY_MARKER = "▶▶"  # "▶▶"

LINK_RE = re.compile(r"\[[^\]]*\]\(([^)#\s]+)(#[^)\s]*)?\)")

LINK_CHECKED = ["docs/CURRENT_WORK.md", "docs/PORT_BACKLOG.md"]


def main() -> int:
    failures = []

    for rel, budget in BUDGETS:
        path = ROOT / rel
        if not path.is_file():
            failures.append(f"{rel}: missing (budgeted file must exist)")
            continue
        size = path.stat().st_size
        if size > budget:
            failures.append(f"{rel}: {size} bytes exceeds budget {budget}")

    cw = ROOT / "docs/CURRENT_WORK.md"
    if cw.is_file() and ESSAY_MARKER in cw.read_text(encoding="utf-8"):
        failures.append(
            "docs/CURRENT_WORK.md: status-essay marker '▶▶' present — "
            "fold the entry to one ledger line and move detail to docs/notes/"
        )

    for rel in LINK_CHECKED:
        path = ROOT / rel
        if not path.is_file():
            continue
        for target, _frag in LINK_RE.findall(path.read_text(encoding="utf-8")):
            if "://" in target:
                continue
            if not (path.parent / target).exists():
                failures.append(f"{rel}: dangling link -> {target}")

    if failures:
        print("docs_check: FAIL")
        for f in failures:
            print(f"  - {f}")
        return 1

    print(f"docs_check: PASS ({len(BUDGETS)} budgets, links resolve)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
