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

# The DERIVED file, and the three constants it is extracted from —
# `(set name, ruby file relative to lib/, constant)`, in the order the generated
# document lists them. Upstream's own reference table is `Catalog::MUTATOR_SETS`
# (`lib/rigor/effects/catalog.rb`); this is that table with each value resolved
# to the source that defines it, and `CATALOG_REFS` below is the table as
# upstream spells it, checked so a re-pointed set cannot go unnoticed.
#
# A constant is either a `%i[…]` literal or — since the `e59b7b89` pin, where
# `hash` became `MutationClassifier::HASH_MUTATORS` — a `(A | B | Set[:x])`
# UNION of other constants and literal sets, which `resolve_constant` follows
# into each term's own file.
DERIVED = "mutators.yml"
MUTATOR_SETS = (
    ("array", "rigor/inference/mutation_widening.rb", "ARRAY_MUTATORS"),
    ("hash", "rigor/effects/mutation_classifier.rb", "HASH_MUTATORS"),
    ("string", "rigor/inference/string_mutation.rb", "MUTATORS"),
)

# `Catalog::MUTATOR_SETS`' right-hand sides verbatim, as written inside
# `Rigor::Effects`. A mismatch means upstream re-pointed a set: update
# `MUTATOR_SETS` above to the new definition site and re-read the diff.
CATALOG_FILE = "rigor/effects/catalog.rb"
CATALOG_REFS = {
    "array": "Inference::MutationWidening::ARRAY_MUTATORS",
    "hash": "MutationClassifier::HASH_MUTATORS",
    "string": "Inference::StringMutation::MUTATORS",
}

# The counts the slice-2 probe measured through the pinned Ruby loader
# (`docs/notes/20260826-effects-s2-probe.md` § 8), moved at the `e59b7b89` pin:
# hash 15 -> 20 (`495a7458` lists `shift`; `c6aba2c9` makes the set the
# classifier's union with `HashLookupMutation::MUTATORS` + `rehash`), string
# 26 -> 35 (`4a6b43f6` makes `StringMutation::MUTATORS` the one String table).
# A set that changes SIZE under a re-pin is a semantic change to what counts as
# a receiver mutation, so it is refused here rather than written and noticed
# later.
EXPECTED_COUNTS = {"array": 31, "hash": 20, "string": 35}

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


def constant_rhs(source, constant):
    """The right-hand side of `CONSTANT = …`, up to the first newline outside
    any bracket — so a parenthesised multi-line union reads whole."""
    match = re.search(rf"^\s*{re.escape(constant)}\s*=\s*", source, re.MULTILINE)
    if not match:
        raise ValueError(f"{constant}: no definition found")
    depth, index = 0, match.end()
    while index < len(source):
        char = source[index]
        if char in "([{":
            depth += 1
        elif char in ")]}":
            depth -= 1
        elif char == "\n" and depth == 0:
            break
        index += 1
    return source[match.end():index].strip()


def module_file(path):
    """`["Inference", "MutationWidening"]` -> `rigor/inference/mutation_widening.rb`."""
    snake = [re.sub(r"(?<!^)(?=[A-Z])", "_", part).lower() for part in path]
    return os.path.join("rigor", *snake) + ".rb"


def resolve_constant(lib_dir, relative, constant, seen=()):
    """The selectors `CONSTANT` (defined in `lib/<relative>`) holds, in Ruby
    `Set#|` order (the left operand's members, then each NEW member of the next).

    A `%i[…]` literal is read directly. A union `(A::B::C | D::E | Set[:x])` is
    followed term by term: a constant path resolves to `lib/rigor/<module
    path>.rb`, first against the defining file's own namespace (`rigor/effects/`
    names a sibling `MutationClassifier`), then from `Rigor` (`Inference::…`);
    a `Set[:a, :b]` literal contributes its symbols. Any other shape is refused,
    never guessed.
    """
    key = (relative, constant)
    if key in seen:
        raise ValueError(f"{constant}: cyclic definition")
    with open(os.path.join(lib_dir, relative), encoding="utf-8") as handle:
        source = handle.read()
    if re.search(rf"^\s*{re.escape(constant)}\s*=\s*%i\[", source, re.MULTILINE):
        return extract_symbol_array(source, constant)
    body = re.sub(r"\.freeze\s*$", "", constant_rhs(source, constant)).strip()
    if body.startswith("(") and body.endswith(")"):
        body = body[1:-1]
    selectors = []
    for term in (t.strip() for t in body.split("|")):
        literal = re.fullmatch(r"Set\[(.*)\]", term, re.DOTALL)
        if literal:
            members = [m.strip().lstrip(":") for m in literal.group(1).split(",") if m.strip()]
        elif re.fullmatch(r"(?:[A-Z]\w*::)+[A-Z_][A-Z0-9_]*", term):
            *path, name = term.split("::")
            namespace = os.path.dirname(relative).split(os.sep)[1:]
            candidates = [module_file(namespace + path), module_file(path)]
            target = next(
                (c for c in candidates if os.path.isfile(os.path.join(lib_dir, c))), None
            )
            if target is None:
                raise ValueError(f"{constant}: cannot locate `{term}` (tried {candidates})")
            members = resolve_constant(lib_dir, target, name, seen + (key,))
        else:
            raise ValueError(f"{constant}: unsupported union term `{term}`")
        selectors.extend(m for m in members if m not in selectors)
    return selectors


def check_catalog_refs(lib_dir):
    """Refuse when `Catalog::MUTATOR_SETS` names a different constant than the
    one `MUTATOR_SETS` resolves — a re-pointed set is a semantic change."""
    with open(os.path.join(lib_dir, CATALOG_FILE), encoding="utf-8") as handle:
        source = handle.read()
    found = dict(re.findall(r'"(\w+)"\s*=>\s*([\w:]+)', constant_rhs(source, "MUTATOR_SETS")))
    if found != CATALOG_REFS:
        raise ValueError(
            f"Catalog::MUTATOR_SETS moved: {found} (expected {CATALOG_REFS}) — "
            "re-point MUTATOR_SETS / CATALOG_REFS and re-read the diff"
        )


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
    check_catalog_refs(lib_dir)
    for name, relative, constant in MUTATOR_SETS:
        selectors = resolve_constant(lib_dir, relative, constant)
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
