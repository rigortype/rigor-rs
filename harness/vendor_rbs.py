#!/usr/bin/env python3
"""Regenerate `crates/rigor-index/vendor/rbs` from an rbs gem checkout.

The vendored tree (ADR-0007) is the WHOLE `core/` directory PLUS the
`DEFAULT_LIBRARIES` stdlib set transitively closed over each lib's
`manifest.yaml` `dependencies:` — i.e. exactly the set the old runtime loader
ingested. That recipe used to live only in `PROVENANCE.md` prose and was
executed by hand; this script is the executable form, so a version bump is
reproducible and auditable instead of a manual copy.

`DEFAULT_LIBRARIES` is READ OUT OF `crates/rigor-index/src/rbs.rs` rather than
restated here — one source of truth, so the tree cannot silently drift from the
loader's own list.

`vendor/rbs/overlay/` is NOT touched: it mirrors the *reference's* own
`data/core_overlay/` + `data/vendored_gem_sigs/`, so it tracks the reference
pin, not the rbs version. Same for `PROVENANCE.md`. Both are carried across a
regeneration untouched.

Usage:
    python3 harness/vendor_rbs.py <rbs-gem-root> [--check]

    <rbs-gem-root>  a directory containing `core/` and `stdlib/`
                    (an unpacked/installed rbs gem, or an rbs checkout)
    --check         do not write: regenerate into a temp dir and report whether
                    it matches the committed tree byte-for-byte (the recipe's
                    self-test against the version already vendored)

The script does NOT update `PROVENANCE.md` — that file records the source path,
date and rationale, which are a human's to write.
"""
import filecmp
import os
import re
import shutil
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VENDOR = os.path.join(REPO, "crates/rigor-index/vendor/rbs")
RBS_RS = os.path.join(REPO, "crates/rigor-index/src/rbs.rs")

# `dependencies:\n  - name: foo` — the only manifest shape the rbs stdlib uses.
DEP_RE = re.compile(r"^\s*-\s*name:\s*(\S+)\s*$")


def default_libraries():
    """Parse `const DEFAULT_LIBRARIES: &[&str] = &[ … ];` out of rbs.rs."""
    src = open(RBS_RS, encoding="utf-8").read()
    m = re.search(r"const DEFAULT_LIBRARIES: &\[&str\] = &\[(.*?)\];", src, re.S)
    if not m:
        sys.exit("vendor_rbs: could not find DEFAULT_LIBRARIES in rbs.rs")
    return re.findall(r'"([^"]+)"', m.group(1))


def manifest_deps(lib_dir):
    path = os.path.join(lib_dir, "manifest.yaml")
    if not os.path.exists(path):
        return []
    deps = []
    for line in open(path, encoding="utf-8"):
        m = DEP_RE.match(line)
        if m:
            deps.append(m.group(1))
    return deps


def resolve_closure(gem_root, seeds):
    """DEFAULT_LIBRARIES closed transitively over manifest dependencies.

    A lib with no `stdlib/<lib>/0` directory is skipped SILENTLY — `prism` and
    `rbs` ship their RBS with their own gems, exactly as the runtime loader
    tolerates.
    """
    resolved, skipped, queue = {}, [], list(seeds)
    while queue:
        lib = queue.pop(0)
        if lib in resolved or lib in skipped:
            continue
        lib_dir = os.path.join(gem_root, "stdlib", lib, "0")
        if not os.path.isdir(lib_dir):
            skipped.append(lib)
            continue
        resolved[lib] = lib_dir
        queue.extend(manifest_deps(lib_dir))
    return resolved, skipped


def build(gem_root, dest):
    core_src = os.path.join(gem_root, "core")
    if not os.path.isdir(core_src):
        sys.exit(f"vendor_rbs: no core/ under {gem_root}")
    shutil.copytree(core_src, os.path.join(dest, "core"))

    resolved, skipped = resolve_closure(gem_root, default_libraries())
    for lib, lib_dir in sorted(resolved.items()):
        shutil.copytree(lib_dir, os.path.join(dest, "stdlib", lib, "0"))
    return resolved, skipped


def carry_into(staged):
    """Copy the non-gem-derived parts of the committed tree into `staged`.

    `PROVENANCE.md` is authored; `overlay/` tracks the reference pin. Neither is
    reconstructible from the rbs gem, so both survive a regeneration verbatim.
    """
    prov = os.path.join(VENDOR, "PROVENANCE.md")
    if os.path.exists(prov):
        shutil.copy(prov, os.path.join(staged, "PROVENANCE.md"))
    overlay = os.path.join(VENDOR, "overlay")
    if os.path.isdir(overlay):
        shutil.copytree(overlay, os.path.join(staged, "overlay"))


def diff_trees(a, b):
    """Recursive (left-only, right-only, differing) relative-path lists."""
    left, right, diff = [], [], []

    def walk(cmp_result, prefix):
        left.extend(os.path.join(prefix, n) for n in cmp_result.left_only)
        right.extend(os.path.join(prefix, n) for n in cmp_result.right_only)
        diff.extend(os.path.join(prefix, n) for n in cmp_result.diff_files)
        for name, sub in cmp_result.subdirs.items():
            walk(sub, os.path.join(prefix, name))

    walk(filecmp.dircmp(a, b), "")
    return left, right, diff


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    check = "--check" in sys.argv
    if len(args) != 1:
        print(__doc__)
        return 2
    gem_root = os.path.abspath(args[0])

    with tempfile.TemporaryDirectory(prefix="vendor-rbs") as tmp:
        staged = os.path.join(tmp, "rbs")
        resolved, skipped = build(gem_root, staged)
        rbs_count = sum(len([f for f in files if f.endswith(".rbs")])
                        for _, _, files in os.walk(staged))
        print(f"source:   {gem_root}")
        print(f"libs:     {len(resolved)} resolved, skipped {skipped or 'none'}")
        print(f"files:    {rbs_count} .rbs")

        if check:
            # PROVENANCE.md and overlay/ are carried, not generated — stage the
            # committed copies so the diff reports only gem-derived drift.
            carry_into(staged)
            missing, extra, differing = diff_trees(VENDOR, staged)
            if not (missing or extra or differing):
                print("CHECK: committed tree matches this source exactly.")
                return 0
            print("CHECK: MISMATCH vs the committed tree")
            for label, items in (("only in vendor/", missing),
                                 ("only in source", extra),
                                 ("differing", differing)):
                if items:
                    print(f"  {label} ({len(items)}):")
                    for p in sorted(items)[:20]:
                        print(f"    {p}")
                    if len(items) > 20:
                        print(f"    … {len(items) - 20} more")
            return 1

        carry_into(staged)
        shutil.rmtree(VENDOR, ignore_errors=True)
        shutil.copytree(staged, VENDOR)
        print(f"WROTE:    {VENDOR}  (PROVENANCE.md + overlay/ carried over; "
              "update PROVENANCE.md by hand)")
        return 0


if __name__ == "__main__":
    sys.exit(main())
