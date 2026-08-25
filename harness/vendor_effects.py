#!/usr/bin/env python3
"""Regenerate `crates/rigor-effects/vendor/effects` from the PINNED reference.

The vendored tree (ADR-0043 slice 1) is the reference's whole `data/effects/`
directory — two hand-written YAML files, `registry.yml` (the label vocabulary)
and `core.yml` (the per-method catalogue) — copied VERBATIM. It is the repo's
THIRD pin-tracking surface, alongside `crates/rigor-index/vendor/rbs/overlay/`
and `crates/rigor-index/vendor/plugins/`, and it drifts exactly the way those
two do: silently, invisibly to any corpus sweep, until a re-pin.

This script is the executable form of that re-sync, modelled on
`harness/vendor_rbs.py`. `--check` is the drift GATE: it is independent of what
any corpus exercises (`harness/effects_diff.py` grades 6 of the catalogue's 420
rows), so it fails the instant the pin moves under an unchanged vendored copy.

The source is the pinned submodule `reference/rigor/data/effects/`, NEVER a
local rigor checkout — that is `UPSTREAM.md` hazard 3, and the vendored plugin
RBS is the recorded case of that hazard applied to a file (two months of drift,
10 live false positives). Populate the submodule first:

    git submodule update --init reference/rigor

Usage:
    python3 harness/vendor_effects.py [--check] [<data-effects-dir>]

    --check              do not write: compare the committed tree against the
                         source byte-for-byte and exit 1 on ANY difference,
                         printing both sha256s per file
    <data-effects-dir>   override the source directory (defaults to the pinned
                         submodule's `data/effects`); for a bisect, not for
                         routine use

`PROVENANCE.md` is NOT generated — it records the pin, the date, the digests and
the three carve-outs, which are a human's to write. It is carried across a
regeneration untouched, exactly as `vendor_rbs.py` carries its own.
"""
import hashlib
import os
import shutil
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VENDOR = os.path.join(REPO, "crates/rigor-effects/vendor/effects")
SOURCE = os.path.join(REPO, "reference/rigor/data/effects")

# The whole of upstream's `data/effects/`. Stated rather than globbed: a file
# upstream ADDS must be a deliberate decision here, not a silent copy.
FILES = ("registry.yml", "core.yml")

# Authored, not generated — carried across a regeneration (the `vendor_rbs.py`
# precedent for `PROVENANCE.md` + `overlay/`).
CARRIED = ("PROVENANCE.md",)


def sha256(path):
    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


def check_source(source):
    if not os.path.isdir(source):
        sys.exit(
            f"vendor_effects: no such directory {source}\n"
            "  the reference submodule is probably unpopulated — run\n"
            "    git submodule update --init reference/rigor\n"
            "  and never point this at a local rigor checkout (UPSTREAM.md hazard 3)."
        )
    missing = [name for name in FILES if not os.path.isfile(os.path.join(source, name))]
    if missing:
        sys.exit(f"vendor_effects: {source} is missing {', '.join(missing)}")


def report_extra(source):
    """Upstream files this recipe does not vendor — a new one is a decision."""
    present = {n for n in os.listdir(source) if not n.startswith(".")}
    return sorted(present - set(FILES))


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    check = "--check" in sys.argv
    if len(args) > 1:
        print(__doc__)
        return 2
    source = os.path.abspath(args[0]) if args else SOURCE
    check_source(source)

    print(f"source:   {source}")
    print(f"vendor:   {VENDOR}")
    extra = report_extra(source)
    if extra:
        print(f"NOTE:     upstream also ships {', '.join(extra)} — not vendored by this recipe")

    if check:
        mismatched = []
        for name in FILES:
            src = os.path.join(source, name)
            dst = os.path.join(VENDOR, name)
            src_digest = sha256(src)
            dst_digest = sha256(dst) if os.path.isfile(dst) else "ABSENT"
            status = "ok" if src_digest == dst_digest else "MISMATCH"
            print(f"  {name:<14} {status}")
            print(f"    source {src_digest}")
            print(f"    vendor {dst_digest}")
            if status != "ok":
                mismatched.append(name)
        if mismatched:
            print("CHECK: MISMATCH vs the pinned source — re-vendor, and read the")
            print("       diff as a SEMANTIC change, not a copy (UPSTREAM.md step 3).")
            return 1
        print("CHECK: committed tree matches the pinned source exactly.")
        return 0

    for name in FILES:
        shutil.copyfile(os.path.join(source, name), os.path.join(VENDOR, name))
    carried = [n for n in CARRIED if os.path.isfile(os.path.join(VENDOR, n))]
    print(f"WROTE:    {len(FILES)} file(s)  ({', '.join(carried) or 'nothing'} carried over; "
          "update PROVENANCE.md by hand)")
    for name in FILES:
        print(f"  {name:<14} {sha256(os.path.join(VENDOR, name))}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
