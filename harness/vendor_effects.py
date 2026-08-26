#!/usr/bin/env python3
"""Regenerate `crates/rigor-effects/vendor/effects` from the PINNED reference.

The vendored tree (ADR-0043 slices 1-2) is the reference's whole `data/effects/`
directory — two hand-written YAML files, `registry.yml` (the label vocabulary)
and `core.yml` (the per-method catalogue) — copied VERBATIM, plus ONE derived
file, `mutators.yml`, EXTRACTED from the reference's Ruby source. It is the
repo's THIRD pin-tracking surface, alongside
`crates/rigor-index/vendor/rbs/overlay/` and `crates/rigor-index/vendor/plugins/`,
and it drifts exactly the way those two do: silently, invisibly to any corpus
sweep, until a re-pin.

`mutators.yml` is derived rather than copied because upstream has no data file
for it: `core.yml` names its three mutator sets BY REFERENCE (`mutators: array |
hash | string`) and its internal spec makes that normative — "The data file MUST
NOT re-spell a selector list". Upstream resolves the name against three Ruby
`%i[…]` literals it maintains for the widening rules; slice 2 needs their
contents (a `mutators:` selector is a receiver mutation on both the row and the
posture path), so this script lifts the literals out of the pinned Ruby and
writes them as data. The extraction is a PARSE of the pinned source, so `--check`
grades it exactly as it grades the two verbatim copies: regenerate in memory,
compare bytes.

This script is the executable form of that re-sync, modelled on
`harness/vendor_rbs.py`. `--check` is the drift GATE: it is independent of what
any corpus exercises (`harness/effects_diff.py` grades 6 of the catalogue's 420
rows), so it fails the instant the pin moves under an unchanged vendored copy.

The source is the pinned submodule `reference/rigor/`, NEVER a local rigor
checkout — that is `UPSTREAM.md` hazard 3, and the vendored plugin RBS is the
recorded case of that hazard applied to a file (two months of drift, 10 live
false positives). Populate the submodule first:

    git submodule update --init reference/rigor

Usage:
    python3 harness/vendor_effects.py [--check] [<data-effects-dir>]

    --check              do not write: compare the committed tree against the
                         source byte-for-byte and exit 1 on ANY difference,
                         printing both sha256s per file
    <data-effects-dir>   override the source directory (defaults to the pinned
                         submodule's `data/effects`); the Ruby sources the
                         mutator sets are extracted from are then read from
                         `<data-effects-dir>/../../lib`. For a bisect, not for
                         routine use

`PROVENANCE.md` is NOT generated — it records the pin, the date, the digests and
the carve-outs, which are a human's to write. It is carried across a
regeneration untouched, exactly as `vendor_rbs.py` carries its own.
"""
import hashlib
import os
import re
import shutil
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VENDOR = os.path.join(REPO, "crates/rigor-effects/vendor/effects")
SOURCE = os.path.join(REPO, "reference/rigor/data/effects")

# The whole of upstream's `data/effects/`. Stated rather than globbed: a file
# upstream ADDS must be a deliberate decision here, not a silent copy.
FILES = ("registry.yml", "core.yml")

# The DERIVED file, and the three `%i[…]` literals it is extracted from —
# `(set name, ruby file relative to lib/, constant)`, in the order the generated
# document lists them. Upstream's own reference table is `Catalog::MUTATOR_SETS`
# (`lib/rigor/effects/catalog.rb:43`); this is that table with each value
# resolved to the source that defines it.
DERIVED = "mutators.yml"
MUTATOR_SETS = (
    ("array", "rigor/inference/mutation_widening.rb", "ARRAY_MUTATORS"),
    ("hash", "rigor/inference/mutation_widening.rb", "HASH_MUTATORS"),
    ("string", "rigor/effects/mutation_classifier.rb", "STRING_MUTATORS"),
)

# The counts the slice-2 probe measured through the pinned Ruby loader
# (`docs/notes/20260826-effects-s2-probe.md` § 8). A set that changes SIZE under
# a re-pin is a semantic change to what counts as a receiver mutation, so it is
# refused here rather than written and noticed later.
EXPECTED_COUNTS = {"array": 31, "hash": 15, "string": 26}

# Authored, not generated — carried across a regeneration (the `vendor_rbs.py`
# precedent for `PROVENANCE.md` + `overlay/`).
CARRIED = ("PROVENANCE.md",)


def sha256(path):
    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


# --------------------------------------------------------------------------
# The derived file
# --------------------------------------------------------------------------
def extract_symbol_array(source, constant):
    """The selectors of `CONSTANT = %i[…]`, in source order.

    Bracket-DEPTH scanning, not a lazy `\\]` match: `[]=` is a member of all
    three sets and spells a balanced `[` `]` INSIDE the literal, which is
    exactly how Ruby's own `%i[…]` reads it. A regex that stopped at the first
    `]` would silently truncate `ARRAY_MUTATORS` at `fill` and drop 13
    selectors — an under-claim no test outside this file would notice.
    """
    match = re.search(rf"^\s*{re.escape(constant)}\s*=\s*%i\[", source, re.MULTILINE)
    if not match:
        raise ValueError(f"{constant}: no `%i[` literal found")
    depth, start = 1, match.end()
    index = start
    while index < len(source) and depth:
        if source[index] == "[":
            depth += 1
        elif source[index] == "]":
            depth -= 1
        index += 1
    if depth:
        raise ValueError(f"{constant}: unterminated `%i[` literal")
    return source[start:index - 1].split()


def render_mutators(lib_dir):
    """The `mutators.yml` document, as bytes. Deterministic: same pin, same file."""
    lines = [
        "# GENERATED — do not hand-edit. `python3 harness/vendor_effects.py`",
        "#",
        "# The three by-reference mutator sets `core.yml`'s `mutators:` key names,",
        "# EXTRACTED from the pinned reference's Ruby source (ADR-0043 slice 2).",
        "# Upstream keeps them as `%i[…]` literals rather than data, because the",
        "# widening rules and the effect model share one hand-audited list and its",
        "# internal spec forbids `core.yml` re-spelling a selector list. The port",
        "# needs the contents: a selector in its class's set is a receiver mutation",
        "# on the row path AND on the posture path (`catalog.rb:194`, `:253`).",
        "#",
        "# Every selector is quoted: `<<`, `[]=`, `!` and the bang family are not",
        "# plain YAML scalars.",
        "schema: 1",
        "sets:",
    ]
    for name, relative, constant in MUTATOR_SETS:
        path = os.path.join(lib_dir, relative)
        with open(path, encoding="utf-8") as handle:
            selectors = extract_symbol_array(handle.read(), constant)
        if len(selectors) != EXPECTED_COUNTS[name]:
            raise ValueError(
                f"{constant}: extracted {len(selectors)} selectors, expected "
                f"{EXPECTED_COUNTS[name]} — the pin changed what counts as a receiver "
                "mutation; re-read the diff as a SEMANTIC change and move "
                "EXPECTED_COUNTS + the crate's count test together"
            )
        lines.append(f"  {name}:")
        lines.append(f"    from: \"lib/{relative}: {constant}\"")
        lines.append("    selectors:")
        lines.extend(f"      - \"{selector}\"" for selector in selectors)
    return ("\n".join(lines) + "\n").encode("utf-8")


def lib_dir_for(source):
    """The reference's `lib/`, relative to the `data/effects` source directory."""
    return os.path.abspath(os.path.join(source, os.pardir, os.pardir, "lib"))


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
    lib = lib_dir_for(source)
    missing = sorted({
        relative for _, relative, _ in MUTATOR_SETS
        if not os.path.isfile(os.path.join(lib, relative))
    })
    if missing:
        sys.exit(
            f"vendor_effects: {lib} is missing {', '.join(missing)}\n"
            "  the mutator sets are EXTRACTED from the reference's Ruby source, so the\n"
            "  submodule must carry `lib/` and not only `data/`."
        )


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

    derived = render_mutators(lib_dir_for(source))

    if check:
        mismatched = []
        for name in FILES + (DERIVED,):
            dst = os.path.join(VENDOR, name)
            if name == DERIVED:
                src_digest = sha256_bytes(derived)
            else:
                src_digest = sha256(os.path.join(source, name))
            dst_digest = sha256(dst) if os.path.isfile(dst) else "ABSENT"
            status = "ok" if src_digest == dst_digest else "MISMATCH"
            print(f"  {name:<14} {status}{'  (derived)' if name == DERIVED else ''}")
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
    with open(os.path.join(VENDOR, DERIVED), "wb") as handle:
        handle.write(derived)
    carried = [n for n in CARRIED if os.path.isfile(os.path.join(VENDOR, n))]
    print(f"WROTE:    {len(FILES) + 1} file(s)  ({', '.join(carried) or 'nothing'} carried over; "
          "update PROVENANCE.md by hand)")
    for name in FILES + (DERIVED,):
        print(f"  {name:<14} {sha256(os.path.join(VENDOR, name))}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
