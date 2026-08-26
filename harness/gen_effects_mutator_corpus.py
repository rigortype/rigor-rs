#!/usr/bin/env python3
"""Generate `harness/effects-corpus/07_mutators` from the VENDORED mutator sets.

The blind spot this project closes (issue #110, the shape of #106): the port
vendors **72 selectors** in three by-reference sets
(`crates/rigor-effects/vendor/effects/mutators.yml`), the collector's mutation
judgment is the only consumer of them, and the whole rest of
`harness/effects-corpus/` touches **two** — `clear` and `upcase!`. A vendored
data table read by the collector with no corpus generated FROM the table is
exactly what shipped the posture over-claim, and it took a generated project to
see it.

**Generated, not hand-written**, for the reason
`harness/gen_effects_posture_corpus.py` is: the sets move with the pin (they are
extracted from upstream's `%i[…]` literals by `harness/vendor_effects.py`), and a
hand-list would freeze today's 72 and go quietly stale. Nothing here encodes what
either engine ANSWERS — every selector in the vendored file gets the same five
probes and the differential reports the verdict.

Four receiver shapes per (set, selector), because the slice-2 / slice-3 rules
(`docs/notes/20260826-effects-s2-probe.md` § 4e) distinguish exactly these:

  `Owned#*`     a local seeded by the set's own literal and never let out — the
                one shape where upstream's `LocalOwnership` PROVES `mutate.local`.
  `Unowned#*`   the same seed with a trailing bare read, so the type is known and
                the ownership is not: upstream answers ∅ + `unknown-ownership`,
                never a bare `mutate`. A port that ever guessed an owner here
                turns all 72 into OVER rows.
  `Ivar#*`      an `@ivar` receiver — `mutate.self`, and `mutate.static` in a
                singleton unit. The proven label must be the SELF one, so a
                mis-owned mutation shows up as an OVER rather than as silence.
  `Konstant#*`  a constant receiver. `MutationClassifier#label_for` recognises
                nil/self, ivar and cvar reads, parameters and frame-owned locals
                — a constant is none of them, so ownership is NEVER provable and
                the answer is ∅ on both sides.

and two sections that are type-free by construction, i.e. that need no receiver
class on either side and so must MATCH today:

  `TypeFree#*` / `TypeFreeSingleton#*`
                `[]=` (a member of all three sets) and its attribute-writer twin,
                across seven receiver shapes and all eight write spellings — the
                two plain calls plus the six compound-write Prism node types the
                collector branches on individually. This is where the port
                actually proves `mutate.*` today, so it is where an ownership
                divergence would surface as an OVER.
  `Rowed#*`     the must-still-fire ROW control: every catalogued row whose
                selector is in a mutator set AND whose owner is spellable from
                syntax (`ENV#store`, `File.delete`, …). A row is authoritative
                and is NOT a receiver mutation, so these must keep proving the
                row's own labels and nothing else.
  `Suppressed#*` the must-still-SUPPRESS control, and the reason
                `NON_MUTATING_ROWED_SELECTORS` (`collect.rs`) exists: a
                universally-mutating selector that some catalogue row answers as
                a NON-mutation. `Thread#[]=` on a provably-owned `Thread.new` is
                the fatal member — the oracle proves `global.write` through the
                row, and a port that dropped the suppression would prove
                `mutate.local`, which is an OVER.

Usage:
    python3 harness/gen_effects_mutator_corpus.py [--check]

    --check   do not write: regenerate in memory and compare bytes against the
              committed project, exiting 1 on any difference. The drift gate — a
              re-vendored `mutators.yml` with an unregenerated corpus fails here.
"""
import os
import re
import sys

import yaml

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MUTATORS = os.path.join(REPO, "crates/rigor-effects/vendor/effects/mutators.yml")
CATALOGUE = os.path.join(REPO, "crates/rigor-effects/vendor/effects/core.yml")
PROJECT = os.path.join(REPO, "harness/effects-corpus/07_mutators")

# A selector this generator spells as a plain `recv.name` call. Everything else
# needs its own syntax and must be listed in SPELLINGS, which is asserted total.
IDENTIFIER = re.compile(r"\A[a-z_][A-Za-z0-9_]*[?!]?\Z")

# Upstream's `ATTRIBUTE_WRITER` (`mutation_classifier.rb:37`).
ATTRIBUTE_WRITER = re.compile(r"\A[a-z_][A-Za-z0-9_]*=\Z")

# How to CALL a selector the identifier rule cannot spell. Asserted total over
# the vendored sets, so a re-vendor that introduces a third operator selector
# fails loudly here instead of silently dropping its probes.
SPELLINGS = {
    "<<": "{recv} << 1",
    "[]=": "{recv}[0] = 1",
}

# The literal that seeds a receiver of each SET, so the reference's typer names
# the class its set belongs to. Asserted total over the vendored set names, so a
# re-vendored file that adds a fourth set fails here rather than skipping it.
SEEDS = {
    "array": "[]",
    "hash": "{}",
    "string": '+""',
}

# The type-free write spellings — the two plain calls (`[]=` and an attribute
# writer) plus the six compound-write Prism node types `UnitScan::visit_construct`
# branches on individually (`collect.rs:574-588`, upstream `unit_scan.rb:215`).
# `[]=` is a member of every vendored set and is asserted so below; `slot=` is
# its type-free twin, spelled from `ATTRIBUTE_WRITER` rather than from the sets
# because upstream's rule is the REGEX, not a list.
WRITE_FORMS = [
    ("index_set", "{recv}[0] = 1", "the plain `[]=` call"),
    ("index_op", "{recv}[0] += 1", "IndexOperatorWriteNode"),
    ("index_or", "{recv}[0] ||= 1", "IndexOrWriteNode"),
    ("index_and", "{recv}[0] &&= 1", "IndexAndWriteNode"),
    ("writer", "{recv}.slot = 1", "the plain attribute-writer call"),
    ("writer_op", "{recv}.slot += 1", "CallOperatorWriteNode"),
    ("writer_or", "{recv}.slot ||= 1", "CallOrWriteNode"),
    ("writer_and", "{recv}.slot &&= 1", "CallAndWriteNode"),
]

# The receiver shapes the ownership judgment separates (`mutation_classifier.rb`
# `ownership`). `(name, parameters, prelude, receiver, tail, why)`.
TYPE_FREE_SHAPES = [
    ("owned", "", ["recv = []"], "recv", "nil", "a frame-owned local: mutate.local"),
    ("escaping", "", ["recv = []"], "recv", "recv",
     "the same local, handed to the caller: ownership unprovable"),
    ("param", "(recv)", [], "recv", "nil", "a parameter: mutate.instance"),
    ("ivar", "", ["@recv = []"], "@recv", "nil", "an ivar read: mutate.self"),
    ("cvar", "", ["@@recv = []"], "@@recv", "nil", "a cvar read: mutate.static"),
    ("konstant", "", [], "K_TYPE_FREE", "nil", "a constant: never provable"),
    ("selfrecv", "", [], "self", "nil", "self: mutate.self"),
]

# The same, inside a singleton unit, where `self` and its ivars flip to
# `mutate.static` and a local does not.
SINGLETON_SHAPES = ["owned", "ivar", "selfrecv"]

RIGOR_YML = """\
# GENERATED by harness/gen_effects_mutator_corpus.py — see mutators.rb.
paths:
  - .
"""

HEADER = """\
# GENERATED by `python3 harness/gen_effects_mutator_corpus.py` — DO NOT EDIT.
#
# Source: crates/rigor-effects/vendor/effects/mutators.yml — the VENDORED
# by-reference mutator sets, i.e. the bytes the measured binary compiles in
# ({sets}), plus crates/rigor-effects/vendor/effects/core.yml
# for the two catalogue-row control sections.
#
# The MUTATOR-SET coverage gate (issue #110). `mutators.yml` carries {pairs}
# (set, selector) pairs and the collector's mutation judgment is their only
# consumer, yet the rest of harness/effects-corpus touches two of them. That is
# the shape that shipped the posture over-claim (#106): a vendored data table
# read by the collector with no corpus generated FROM the table.
#
# Nothing here encodes what either engine ANSWERS. Every selector in the vendored
# file gets the same four receiver shapes — the ones upstream's
# `MutationClassifier` separates — and `harness/effects_diff.py` reports the
# verdict. A re-vendor that adds a selector adds its probes for free.
#
# `TypeFree` / `TypeFreeSingleton` are the type-free half: `[]=` and its
# attribute-writer twin need no receiver class on either side, so they are where
# the port proves `mutate.*` today and where an ownership divergence would
# surface as an OVER. `Rowed` and `Suppressed` are the must-still-fire and
# must-still-suppress controls, both derived from the catalogue.
"""


def load(path):
    with open(path, encoding="utf-8") as handle:
        return yaml.safe_load(handle)


def slug(selector):
    """A method-name fragment for a selector, injective over the vendored sets."""
    if selector in SPELLINGS:
        return {"<<": "shovel", "[]=": "index_set"}[selector]
    return selector.replace("!", "_bang").replace("?", "_p").replace("=", "_set")


def call(selector, receiver):
    """`selector` invoked on `receiver`, as Ruby spells it.

    An attribute writer needs its argument or the parser eats the next line —
    `recv.foo=` followed by `nil` is `recv.foo = nil`, which would silently
    swallow the statement the shape depends on.
    """
    if selector in SPELLINGS:
        return SPELLINGS[selector].format(recv=receiver)
    if ATTRIBUTE_WRITER.match(selector):
        return f"{receiver}.{selector[:-1]} = 1"
    return f"{receiver}.{selector}"


def pairs(mutators):
    """`[(set, selector, method-name suffix)]`, in the vendored file's order."""
    out = []
    for name, body in mutators["sets"].items():
        for selector in body["selectors"]:
            out.append((name, selector, f"{name[0]}_{slug(selector)}"))
    return out


def assert_total(mutators):
    """Every vendored set and every vendored selector must be spellable."""
    unknown = sorted(set(mutators["sets"]) - set(SEEDS))
    if unknown:
        sys.exit(f"gen_effects_mutator_corpus: no seed literal for set(s) {unknown} — "
                 "add one to SEEDS and re-read the differential")
    unspellable = sorted({s for _, s, _ in pairs(mutators)
                          if not IDENTIFIER.match(s) and s not in SPELLINGS})
    if unspellable:
        sys.exit(f"gen_effects_mutator_corpus: no call spelling for {unspellable} — "
                 "add one to SPELLINGS")
    names = [name for _, _, name in pairs(mutators)]
    if len(names) != len(set(names)):
        sys.exit("gen_effects_mutator_corpus: slug collision among the vendored selectors")
    missing = sorted(n for n, b in mutators["sets"].items() if "[]=" not in b["selectors"])
    if missing:
        sys.exit(f"gen_effects_mutator_corpus: set(s) {missing} no longer carry `[]=` — "
                 "the TypeFree section's premise has moved; re-derive it")


def universally_mutating(selector):
    """Upstream's `UNIVERSAL_MUTATORS ∪ ATTRIBUTE_WRITER` (`mutation_classifier.rb:57`)."""
    return selector == "[]=" or bool(ATTRIBUTE_WRITER.match(selector))


def catalogue_rows(catalogue, mutators):
    """`(owner, bucket, selector, row)` for every row of every catalogued class.

    `bucket` is the one a call on the CONSTANT looks up: a `kind: object`
    constant names an object, so the call is an instance dispatch; every other
    constant names a class and keys the singleton bucket (`collect.rs`'s
    `catalog_target`).
    """
    sets = {n: set(b["selectors"]) for n, b in mutators["sets"].items()}
    for owner, body in (catalogue["classes"] or {}).items():
        body = body or {}
        own = sets.get(body.get("mutators"), set())
        for bucket in ("methods", "singleton_methods"):
            for selector, row in (body.get(bucket) or {}).items():
                row = dict(row or {})
                # `catalog.rb:259` / `catalog.rs:657`: an instance row whose
                # selector is in its class's own set mutates the receiver even
                # without an explicit `mutates: receiver`.
                row["_mutates"] = row.get("mutates") == "receiver" or selector in own
                row["_object"] = body.get("kind") == "object"
                yield owner, bucket, selector, row


def row_controls(catalogue, mutators):
    """Rows in a mutator set whose owner a constant receiver can spell."""
    union = {s for b in mutators["sets"].values() for s in b["selectors"]}
    out = []
    for owner, bucket, selector, row in catalogue_rows(catalogue, mutators):
        if selector not in union:
            continue
        # Only the bucket a CONSTANT receiver actually reaches.
        if row["_object"] != (bucket == "methods"):
            continue
        if not (row.get("effects") or []) or row.get("narrow"):
            continue
        out.append((owner, selector, list(row["effects"]), row["_object"]))
    return out


def suppression_controls(catalogue, mutators):
    """The rows that put a selector in `NON_MUTATING_ROWED_SELECTORS`.

    A universally-mutating selector some row answers as a NON-mutation: upstream
    reaches that row through the TYPER for a receiver this port cannot name, so
    the port suppresses the label rather than mirroring it. Each control gives
    the selector a receiver whose ownership IS provable wherever the class
    allows one, which is what makes the suppression falsifiable.
    """
    out = []
    for owner, bucket, selector, row in catalogue_rows(catalogue, mutators):
        if not universally_mutating(selector) or row["_mutates"]:
            continue
        if row["_object"]:
            seed, why = owner, "an object constant has no allocator"
        elif bucket == "methods":
            seed, why = f"{owner}.new", "a provably-owned instance — the FATAL member"
        else:
            seed, why = owner, "a singleton row: the receiver is the class object"
        out.append((owner, selector, list(row.get("effects") or []), seed, why))
    return out


def section(title, prose, class_name, methods, singleton=False):
    """One `class … end` block with its explanatory banner."""
    rule = "-" * max(3, 72 - len(title))
    lines = [f"# --- {title} {rule}", "#"]
    lines.extend(f"# {line}".rstrip() for line in prose)
    lines.extend(["", f"class {class_name}"])
    indent = "    " if singleton else "  "
    if singleton:
        lines.append("  class << self")
    bodies = []
    for name, parameters, body, why in methods:
        block = [f"{indent}# {why}"] if why else []
        block.append(f"{indent}def {name}{parameters}")
        block.extend(f"{indent}  {line}" for line in body)
        block.append(f"{indent}end")
        bodies.append("\n".join(block))
    lines.append("\n\n".join(bodies))
    if singleton:
        lines.append("  end")
    lines.append("end")
    return "\n".join(lines)


def receiver_sections(mutators):
    """The four per-(set, selector) receiver shapes."""
    out = []
    probes = pairs(mutators)

    out.append(section(
        "the PROVABLY-OWNED receiver",
        [f"One method per vendored (set, selector) pair ({len(probes)}). The local is seeded by",
         "the set's own literal, so the reference's typer names the class the set",
         "belongs to, and it never escapes, so `LocalOwnership#owned` proves it —",
         "the one shape in which upstream proves `mutate.local` for a set member.",
         "A `nil` tail is load-bearing: a trailing bare read is an escape",
         "(`local_ownership.rb:122`), which is what the `Unowned` section below is."],
        "Owned",
        [(name, "", [f"recv = {SEEDS[st]}", call(sel, "recv"), "nil"], f"{st}: {sel}")
         for st, sel, name in probes]))

    out.append(section(
        "the TYPED-but-UNOWNED receiver",
        ["The same seed and the same call, handed to the caller by the trailing bare",
         "read. The type is known and the ownership is not, so upstream answers ∅",
         "plus an `unknown-ownership` taint and NEVER a bare `mutate`",
         "(`mutation_classifier.rb:20`). A port that guessed an owner here — or that",
         "answered the parent label — turns every one of these into an OVER."],
        "Unowned",
        [(name, "", [f"recv = {SEEDS[st]}", call(sel, "recv"), "recv"], f"{st}: {sel}")
         for st, sel, name in probes]))

    out.append(section(
        "the IVAR receiver",
        ["`@recv` is seeded in the body so the reference can type it. Both the write",
         "and the mutation earn `mutate.self`, so the PROVEN lane is the same one",
         "label either way and a mis-owned mutation (`mutate.local` on an ivar, say)",
         "shows up as an OVER rather than as silence."],
        "Ivar",
        [(name, "", [f"@recv = {SEEDS[st]}", call(sel, "@recv"), "nil"], f"{st}: {sel}")
         for st, sel, name in probes]))

    out.append(section(
        "the CONSTANT receiver",
        ["`label_for` recognises nil/self, ivar and cvar reads, parameters and",
         "frame-owned locals — a constant is none of them, so ownership is never",
         "provable and upstream answers ∅ + `unknown-ownership` however precisely it",
         "typed the receiver. The constants are seeded at the top of this file."],
        "Konstant",
        [(name, "", [call(sel, konstant(name)), "nil"], f"{st}: {sel}")
         for st, sel, name in probes]))
    return out


def konstant(name):
    return f"K_{name.upper()}"


def type_free_sections(mutators):
    shapes = {s[0]: s for s in TYPE_FREE_SHAPES}
    out = []

    methods = []
    for shape, parameters, prelude, receiver, tail, why in TYPE_FREE_SHAPES:
        for form, template, form_why in WRITE_FORMS:
            methods.append((f"{shape}_{form}", parameters,
                            prelude + [template.format(recv=receiver), tail],
                            f"{why} — {form_why}"))
    out.append(section(
        "TYPE-FREE: `[]=` and the attribute writer, on every receiver shape",
        ["`[]=` is a member of all three vendored sets AND of upstream's",
         "`UNIVERSAL_MUTATORS`, so it is claimed on every receiver with no class",
         "needed; `slot=` is its twin through `ATTRIBUTE_WRITER`. Eight spellings",
         "each: the two plain calls plus the six compound-write node types the",
         "collector branches on individually, which bypass the catalogue entirely",
         "and so are pure ownership readings on both sides.",
         "",
         "This is the section where the port PROVES `mutate.*` today, so it is the",
         "one an ownership divergence would surface in — as an OVER, not as silence."],
        "TypeFree",
        methods))

    methods = []
    for shape in SINGLETON_SHAPES:
        _, parameters, prelude, receiver, tail, why = shapes[shape]
        for form, template, form_why in WRITE_FORMS:
            methods.append((f"{shape}_{form}", parameters,
                            prelude + [template.format(recv=receiver), tail],
                            f"{why} — {form_why}"))
    out.append(section(
        "TYPE-FREE, in a SINGLETON unit",
        ["`class << self` flips the axis that separates `mutate.self` from",
         "`mutate.static` for self and its ivars, and leaves a local alone",
         "(`mutation_classifier.rb:80`). Same eight spellings."],
        "TypeFreeSingleton",
        methods, singleton=True))
    return out


def control_sections(catalogue, mutators):
    out = []
    rows = row_controls(catalogue, mutators)
    out.append(section(
        "control: a ROWED mutator selector is the ROW's answer, not a mutation",
        [f"Every catalogued row ({len(rows)}) whose selector is in a vendored set and whose",
         "owner a CONSTANT receiver can spell. `Catalog#lookup` reads the row first",
         "and a row is not a receiver mutation unless it says so, so `ENV.clear` is",
         "`global.write` and nothing else — never `mutate.*`. These must stay MATCH:",
         "a port that let a set membership override a row would show up here as an",
         "OVER, and one that stopped reading rows would lose every label below."],
        "Rowed",
        [(f"r_{owner.replace('::', '__').lower()}_{slug(selector)}", "",
          [call(selector, owner)],
          f"{owner}{'#' if is_object else '.'}{selector}: {', '.join(labels)}")
         for owner, selector, labels, is_object in rows]))

    controls = suppression_controls(catalogue, mutators)
    out.append(section(
        "control: the selectors a row answers as a NON-mutation stay SUPPRESSED",
        [f"The {len(controls)} rows behind `NON_MUTATING_ROWED_SELECTORS` (`collect.rs`): a",
         "universally-mutating selector that some catalogue row answers as a plain",
         "effect. Upstream reaches those rows through the TYPER, for a receiver this",
         "port cannot name from syntax — so the port suppresses the label rather",
         "than mirroring a typing it does not have.",
         "",
         "The receiver is made provably-owned wherever the class has an allocator,",
         "which is what makes the suppression falsifiable: the oracle proves the",
         "ROW's label, and a port that dropped the suppression would prove",
         "`mutate.local` beside it — a label the oracle does not prove, i.e. an OVER."],
        "Suppressed",
        [(f"s_{owner.replace('::', '__').lower()}_{slug(selector)}", "",
          [f"recv = {seed}", call(selector, "recv"), "nil"],
          f"{owner}#{selector} -> {', '.join(labels) or 'nothing'} ({why})")
         for owner, selector, labels, seed, why in controls]))
    return out


def render(catalogue, mutators):
    assert_total(mutators)
    probes = pairs(mutators)
    described = ", ".join(f"{n} {len(b['selectors'])}" for n, b in mutators["sets"].items())

    out = [HEADER.format(sets=described, pairs=len(probes)).rstrip("\n")]
    out.append("\n".join(
        ["# --- the CONSTANT receivers, one per probe " + "-" * 30, "#",
         "# Seeded by the set's own literal, exactly as the `Owned` locals are, so the",
         "# reference types them and the only thing the differential can be reading is",
         "# the ownership judgment.", ""]
        + [f"{konstant(name)} = {SEEDS[st]}" for st, _, name in probes]
        + ["", "# The `TypeFree` constant receiver — one shared read; a constant carries no",
           "# per-site state.", "K_TYPE_FREE = []"]))

    out.extend(receiver_sections(mutators))
    out.extend(type_free_sections(mutators))
    out.extend(control_sections(catalogue, mutators))

    counts = {
        "receiver probes": 4 * len(probes),
        "type-free probes": len(TYPE_FREE_SHAPES) * len(WRITE_FORMS)
        + len(SINGLETON_SHAPES) * len(WRITE_FORMS),
        "row controls": len(row_controls(catalogue, mutators)),
        "suppression controls": len(suppression_controls(catalogue, mutators)),
    }
    return "\n\n".join(out).rstrip("\n") + "\n", counts


def main():
    check = "--check" in sys.argv[1:]
    if [a for a in sys.argv[1:] if a != "--check"]:
        print(__doc__)
        return 2

    body, counts = render(load(CATALOGUE), load(MUTATORS))
    files = {"mutators.rb": body, ".rigor.yml": RIGOR_YML}

    print(f"mutator sets: {os.path.relpath(MUTATORS, REPO)}")
    print(f"catalogue:    {os.path.relpath(CATALOGUE, REPO)}")
    print(f"project:      {os.path.relpath(PROJECT, REPO)}")
    for label, n in counts.items():
        print(f"  {label:<22} {n}")
    print(f"  {'TOTAL methods':<22} {sum(counts.values())}")

    if check:
        stale = []
        for name, text in sorted(files.items()):
            path = os.path.join(PROJECT, name)
            current = None
            if os.path.isfile(path):
                with open(path, encoding="utf-8") as handle:
                    current = handle.read()
            status = "ok" if current == text else ("ABSENT" if current is None else "STALE")
            print(f"  {name:<14} {status}")
            if status != "ok":
                stale.append(name)
        if stale:
            print("CHECK: the committed corpus does not match the vendored sets —")
            print("       regenerate with `python3 harness/gen_effects_mutator_corpus.py`")
            print("       and read the diff as a MUTATOR-SET change.")
            return 1
        print("CHECK: the committed corpus matches the vendored sets exactly.")
        return 0

    os.makedirs(PROJECT, exist_ok=True)
    for name, text in sorted(files.items()):
        with open(os.path.join(PROJECT, name), "w", encoding="utf-8") as handle:
            handle.write(text)
    print("written.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
